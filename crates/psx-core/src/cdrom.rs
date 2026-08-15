// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000
//
//! The CD-ROM controller.
//!
//! A separate microcontroller with its own firmware, not a memory-mapped
//! register block. The CPU hands it a command plus parameters and gets
//! **interrupts** back carrying response bytes, so a command produces one, two,
//! or a stream of responses rather than returning a value. What is modelled here
//! is therefore a queue of scheduled responses.
//!
//! Only the controller is implemented, with an empty drive. There is no disc
//! image support and no sector data, so anything that would need to read
//! something answers with the error the hardware gives for an empty tray. See
//! `docs/notes/CDROM.md`.

use crate::irq::{self, Irq};

/// Cycles from a command being written to its acknowledgement.
///
/// Approximate. `cdrom/timing` measures this precisely, but every measurement in
/// it needs a disc in the drive, so none of it can be checked yet.
const ACK_DELAY: u64 = 50_401;
/// Cycles from the acknowledgement to a completion, for the commands that have
/// a second response.
const COMPLETE_DELAY: u64 = 120_000;

/// Response interrupt codes, in the low three bits of the flags register.
const INT2_COMPLETE: u8 = 2;
const INT3_ACK: u8 = 3;
const INT5_ERROR: u8 = 5;

/// Drive status bits.
const STAT_ERROR: u8 = 1 << 0;
const STAT_MOTOR: u8 = 1 << 1;
const STAT_SHELL_OPEN: u8 = 1 << 4;

/// The controller firmware's own date and version, reported by `Test(0x20)`.
/// Measured hardware data, used as data: this is the SCPH-1001 drive's.
const FIRMWARE_ID: [u8; 4] = [0x94, 0x09, 0x19, 0xC0];

/// The `GetID` reply for an empty tray: "no disc".
const NO_DISC_ID: [u8; 8] = [0x08, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// The most bytes any response carries.
const RESPONSE_MAX: usize = 16;
/// The parameter FIFO's depth.
const PARAM_MAX: usize = 16;
/// How many responses can be outstanding at once. Two covers every command:
/// an acknowledgement plus a completion.
const PENDING_MAX: usize = 2;

/// `RSTA_CDROM_TRACE=1` logs every command and every response delivered.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RSTA_CDROM_TRACE").is_ok_and(|v| v != "0"))
}

#[derive(Clone, Copy, Default)]
struct Response {
    irq: u8,
    len: u8,
    data: [u8; RESPONSE_MAX],
}

impl Response {
    fn new(irq: u8, bytes: &[u8]) -> Response {
        let mut data = [0u8; RESPONSE_MAX];
        data[..bytes.len()].copy_from_slice(bytes);
        Response {
            irq,
            len: bytes.len() as u8,
            data,
        }
    }
}

#[derive(Clone)]
pub struct Cdrom {
    index: u8,
    irq_enable: u8,
    irq_flags: u8,

    /// The drive status byte, which is not the same thing as the status
    /// register at `0x1F801800`.
    stat: u8,
    mode: u8,
    /// The seek target from `Setloc`, as minute, second and frame in BCD.
    seek_loc: [u8; 3],

    params: [u8; PARAM_MAX],
    params_len: u8,

    response: [u8; RESPONSE_MAX],
    response_len: u8,
    response_pos: u8,

    pending: [Response; PENDING_MAX],
    pending_len: u8,
    /// Cycles until the head of `pending` is delivered.
    countdown: u64,

    /// Is there a disc in the drive? No image support yet, so this is false and
    /// the commands that would read from it report the empty-tray error.
    pub disc: bool,

    /// Commands seen, and commands this core does not recognise. Host-side
    /// observation only, never serialized.
    pub commands: u64,
    pub unknown_commands: u64,
}

impl Default for Cdrom {
    fn default() -> Cdrom {
        Cdrom::new()
    }
}

impl Cdrom {
    pub fn new() -> Cdrom {
        Cdrom {
            index: 0,
            irq_enable: 0,
            irq_flags: 0,
            // The shell-open bit latches: it reads set until the first command
            // clears it, which is how software tells "the lid is open" from
            // "the disc may have changed since you last looked".
            stat: STAT_SHELL_OPEN,
            mode: 0,
            seek_loc: [0; 3],
            params: [0; PARAM_MAX],
            params_len: 0,
            response: [0; RESPONSE_MAX],
            response_len: 0,
            response_pos: 0,
            pending: [Response::default(); PENDING_MAX],
            pending_len: 0,
            countdown: 0,
            disc: false,
            commands: 0,
            unknown_commands: 0,
        }
    }

    // ---- register interface ----------------------------------------------

    /// `offset` is relative to `0x1F801800`.
    pub fn read(&mut self, offset: u32) -> u32 {
        let v = match offset & 3 {
            0 => self.status_register(),
            1 => self.pop_response(),
            2 => 0, // the data FIFO, which stays empty without a disc
            _ => match self.index {
                1 | 3 => self.irq_flags | 0xE0,
                _ => self.irq_enable | 0xE0,
            },
        };
        v as u32
    }

    /// `offset` is relative to `0x1F801800`.
    pub fn write(&mut self, offset: u32, val: u8) {
        match offset & 3 {
            0 => self.index = val & 3,
            // Index 0 is the command register; the others are SPU volume and
            // sound map paths, which do not exist.
            1 => {
                if self.index == 0 {
                    self.command(val);
                }
            }
            2 => match self.index {
                0 => self.push_param(val),
                1 => self.irq_enable = val & 0x1F,
                _ => {}
            },
            // A write to the flags register acknowledges the bits set in it.
            // It does not assign: treating it as an assignment leaves the line
            // stuck. Bit 6 is a separate strobe that empties the parameters.
            _ => {
                if self.index == 1 {
                    self.irq_flags &= !(val & 0x1F);
                    if val & 0x40 != 0 {
                        self.params_len = 0;
                    }
                }
            }
        }
    }

    fn status_register(&self) -> u8 {
        let mut s = self.index;
        s |= 1 << 2; // the ADPCM FIFO, always empty here
        if self.params_len == 0 {
            s |= 1 << 3; // parameter FIFO empty
        }
        if (self.params_len as usize) < PARAM_MAX {
            s |= 1 << 4; // parameter FIFO not full
        }
        if self.response_pos < self.response_len {
            s |= 1 << 5; // response FIFO not empty
        }
        s
    }

    fn pop_response(&mut self) -> u8 {
        if self.response_pos < self.response_len {
            let v = self.response[self.response_pos as usize];
            self.response_pos += 1;
            v
        } else {
            // Reading past the end wraps within the 16-byte window on hardware
            // rather than returning a fixed value. Nothing here depends on it.
            0
        }
    }

    fn push_param(&mut self, val: u8) {
        if (self.params_len as usize) < PARAM_MAX {
            self.params[self.params_len as usize] = val;
            self.params_len += 1;
        }
    }

    fn param(&self, i: usize) -> u8 {
        if i < self.params_len as usize {
            self.params[i]
        } else {
            0
        }
    }

    // ---- commands --------------------------------------------------------

    /// Queue a response `delay` cycles from now. Only the first entry carries a
    /// delay; the rest follow once the previous one has been acknowledged.
    fn queue(&mut self, irq: u8, bytes: &[u8], delay: u64) {
        if (self.pending_len as usize) >= PENDING_MAX {
            return;
        }
        if self.pending_len == 0 {
            self.countdown = delay;
        }
        self.pending[self.pending_len as usize] = Response::new(irq, bytes);
        self.pending_len += 1;
    }

    /// The status byte as this command reports it, clearing the latched
    /// shell-open bit on the way out.
    fn take_stat(&mut self) -> u8 {
        let s = self.stat;
        self.stat &= !STAT_SHELL_OPEN;
        s
    }

    fn command(&mut self, cmd: u8) {
        self.commands += 1;
        if trace_enabled() {
            eprintln!(
                "cdrom cmd {cmd:02x} params {:02x?}",
                &self.params[..self.params_len as usize]
            );
        }

        match cmd {
            0x01 => {
                // Getstat
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x02 => {
                // Setloc, in BCD minute/second/frame
                self.seek_loc = [self.param(0), self.param(1), self.param(2)];
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0E => {
                // Setmode
                self.mode = self.param(0);
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0B..=0x0D => {
                // Mute, Demute, Setfilter: accepted, nothing to do without audio
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0A => {
                // Init: acknowledge, then complete. The motor spins up whether
                // or not there is anything on it.
                self.mode = 0;
                self.stat = (self.stat & !STAT_SHELL_OPEN) | STAT_MOTOR;
                let s = self.stat;
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT2_COMPLETE, &[s], COMPLETE_DELAY);
            }
            0x08 | 0x09 => {
                // Stop, Pause
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT2_COMPLETE, &[s], COMPLETE_DELAY);
            }
            0x19 => self.test(),
            0x1A => {
                // GetID: acknowledge, then say what is in the drive.
                // There is only one answer to give, because there is no disc
                // image support to give another one from. When there is, this
                // is where the licensed-game reply goes, and `disc` is the flag
                // that will choose between them.
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT5_ERROR, &NO_DISC_ID, COMPLETE_DELAY);
            }
            // Everything that needs to read something. With an empty tray the
            // hardware answers with the error, not with silence, and software
            // that waits for a response it will never get is the failure mode
            // this avoids.
            0x06 | 0x10 | 0x11 | 0x13 | 0x14 | 0x15 | 0x16 | 0x1B => {
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT5_ERROR, &[s | STAT_ERROR, 0x80], COMPLETE_DELAY);
            }
            _ => {
                self.unknown_commands += 1;
                let s = self.take_stat();
                self.queue(INT5_ERROR, &[s | STAT_ERROR, 0x40], ACK_DELAY);
            }
        }

        // Per command, not per write: a command with no parameters would
        // otherwise inherit the previous one's.
        self.params_len = 0;
    }

    fn test(&mut self) {
        match self.param(0) {
            // The controller firmware's own date and version.
            0x20 => self.queue(INT3_ACK, &FIRMWARE_ID, ACK_DELAY),
            _ => {
                let s = self.take_stat();
                self.queue(INT5_ERROR, &[s | STAT_ERROR, 0x10], ACK_DELAY);
            }
        }
    }

    // ---- timing ----------------------------------------------------------

    /// Deliver whatever has come due.
    ///
    /// The gate on `irq_flags` is the important part: the controller holds the
    /// next response until software has acknowledged the current interrupt. A
    /// second response never overwrites the first and never arrives early.
    pub fn run(&mut self, elapsed: u64, irq: &mut Irq) {
        if self.pending_len == 0 {
            return;
        }
        if self.countdown > elapsed {
            self.countdown -= elapsed;
            return;
        }
        self.countdown = 0;
        if self.irq_flags != 0 {
            return;
        }

        let r = self.pending[0];
        self.response[..].copy_from_slice(&r.data);
        self.response_len = r.len;
        self.response_pos = 0;
        self.irq_flags = r.irq;

        if trace_enabled() {
            eprintln!(
                "cdrom int{} {:02x?}",
                r.irq,
                &r.data[..r.len as usize]
            );
        }

        for i in 1..PENDING_MAX {
            self.pending[i - 1] = self.pending[i];
        }
        self.pending_len -= 1;
        self.countdown = if self.pending_len > 0 {
            COMPLETE_DELAY
        } else {
            0
        };

        if self.irq_enable & self.irq_flags & 0x07 != 0 {
            irq.raise(irq::CDROM);
        }
    }

    /// Cycles until the next response is due, for the scheduler.
    ///
    /// `None` while an unacknowledged interrupt is blocking delivery: there is
    /// nothing to wake up for until software writes the flags back.
    pub fn cycles_to_event(&self) -> Option<u64> {
        if self.pending_len == 0 || self.irq_flags != 0 {
            None
        } else {
            Some(self.countdown.max(1))
        }
    }

    // ---- save state ------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn parts(&self) -> ([u8; 8], [u8; 3], [u8; PARAM_MAX], [u8; RESPONSE_MAX], u64) {
        (
            [
                self.index,
                self.irq_enable,
                self.irq_flags,
                self.stat,
                self.mode,
                self.params_len,
                self.response_len,
                self.response_pos,
            ],
            self.seek_loc,
            self.params,
            self.response,
            self.countdown,
        )
    }

    /// The response queue, flattened: `len`, then each entry's irq, byte count
    /// and payload. Fixed width so the state stays a constant length.
    pub(crate) fn pending_parts(&self) -> (u8, [u8; PENDING_MAX * (2 + RESPONSE_MAX)]) {
        let mut out = [0u8; PENDING_MAX * (2 + RESPONSE_MAX)];
        for (i, r) in self.pending.iter().enumerate() {
            let base = i * (2 + RESPONSE_MAX);
            out[base] = r.irq;
            out[base + 1] = r.len;
            out[base + 2..base + 2 + RESPONSE_MAX].copy_from_slice(&r.data);
        }
        (self.pending_len, out)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        regs: [u8; 8],
        seek_loc: [u8; 3],
        params: [u8; PARAM_MAX],
        response: [u8; RESPONSE_MAX],
        countdown: u64,
        pending_len: u8,
        pending: [u8; PENDING_MAX * (2 + RESPONSE_MAX)],
    ) {
        self.index = regs[0] & 3;
        self.irq_enable = regs[1];
        self.irq_flags = regs[2];
        self.stat = regs[3];
        self.mode = regs[4];
        // Clamped, not trusted: a truncated length would index past the array.
        self.params_len = regs[5].min(PARAM_MAX as u8);
        self.response_len = regs[6].min(RESPONSE_MAX as u8);
        self.response_pos = regs[7].min(RESPONSE_MAX as u8);
        self.seek_loc = seek_loc;
        self.params = params;
        self.response = response;
        self.countdown = countdown;
        self.pending_len = pending_len.min(PENDING_MAX as u8);
        for (i, r) in self.pending.iter_mut().enumerate() {
            let base = i * (2 + RESPONSE_MAX);
            r.irq = pending[base];
            r.len = pending[base + 1].min(RESPONSE_MAX as u8);
            r.data
                .copy_from_slice(&pending[base + 2..base + 2 + RESPONSE_MAX]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a command with parameters, the way software does.
    fn issue(c: &mut Cdrom, cmd: u8, params: &[u8]) {
        c.write(0, 0); // index 0
        for p in params {
            c.write(2, *p);
        }
        c.write(1, cmd);
    }

    /// Run until the next interrupt arrives, then read its code and payload.
    fn take(c: &mut Cdrom, irq: &mut Irq) -> (u8, Vec<u8>) {
        for _ in 0..64 {
            c.run(ACK_DELAY.max(COMPLETE_DELAY), irq);
            if c.irq_flags != 0 {
                break;
            }
        }
        let code = c.irq_flags;
        let mut out = Vec::new();
        while c.status_register() & (1 << 5) != 0 {
            out.push(c.pop_response());
        }
        c.write(0, 1); // index 1
        c.write(3, 0x07); // acknowledge
        c.write(0, 0);
        (code, out)
    }

    fn enabled() -> Cdrom {
        let mut c = Cdrom::new();
        c.write(0, 1);
        c.write(2, 0x1F); // interrupt enable
        c.write(0, 0);
        c
    }

    #[test]
    fn getstat_acknowledges_with_the_drive_status() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x01, &[]);
        let (code, data) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        assert_eq!(data, vec![STAT_SHELL_OPEN]);
        assert_ne!(irq.stat() & (1 << irq::CDROM), 0);
    }

    #[test]
    fn the_shell_open_bit_latches_until_it_is_read() {
        let mut c = enabled();
        let mut irq = Irq::new();

        issue(&mut c, 0x01, &[]);
        let (_, first) = take(&mut c, &mut irq);
        assert_eq!(first, vec![STAT_SHELL_OPEN], "set on the first look");

        issue(&mut c, 0x01, &[]);
        let (_, second) = take(&mut c, &mut irq);
        assert_eq!(second, vec![0], "and clear on the second");
    }

    #[test]
    fn init_acknowledges_then_completes() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x0A, &[]);

        let (first, _) = take(&mut c, &mut irq);
        assert_eq!(first, INT3_ACK);
        let (second, data) = take(&mut c, &mut irq);
        assert_eq!(second, INT2_COMPLETE);
        assert_eq!(data, vec![STAT_MOTOR], "the motor is spinning");
    }

    #[test]
    fn a_second_response_waits_for_the_first_to_be_acknowledged() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x0A, &[]);

        // Long enough for both, if nothing were gating them.
        c.run(10 * (ACK_DELAY + COMPLETE_DELAY), &mut irq);
        assert_eq!(c.irq_flags, INT3_ACK, "still the acknowledgement");
        assert_eq!(c.pending_len, 1, "the completion is still queued");
        assert_eq!(
            c.cycles_to_event(),
            None,
            "and nothing to wake up for until software acknowledges"
        );

        c.write(0, 1);
        c.write(3, 0x07);
        c.write(0, 0);
        c.run(COMPLETE_DELAY, &mut irq);
        assert_eq!(c.irq_flags, INT2_COMPLETE);
    }

    #[test]
    fn getid_reports_an_empty_tray() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x1A, &[]);

        let (first, _) = take(&mut c, &mut irq);
        assert_eq!(first, INT3_ACK);
        let (second, data) = take(&mut c, &mut irq);
        assert_eq!(second, INT5_ERROR);
        assert_eq!(data, NO_DISC_ID.to_vec());
    }

    #[test]
    fn test_20_reports_the_firmware_version() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x19, &[0x20]);
        let (code, data) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        assert_eq!(data, FIRMWARE_ID.to_vec());
    }

    #[test]
    fn reading_without_a_disc_is_an_error_not_a_silence() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x15, &[]); // SeekL

        let (first, _) = take(&mut c, &mut irq);
        assert_eq!(first, INT3_ACK);
        let (second, data) = take(&mut c, &mut irq);
        assert_eq!(second, INT5_ERROR, "software must not be left waiting");
        assert_ne!(data[0] & STAT_ERROR, 0);
    }

    #[test]
    fn the_parameter_fifo_is_emptied_per_command() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x02, &[0x00, 0x02, 0x16]); // Setloc
        let _ = take(&mut c, &mut irq);
        assert_eq!(c.seek_loc, [0x00, 0x02, 0x16]);
        assert_eq!(c.params_len, 0, "or the next command inherits them");

        issue(&mut c, 0x0E, &[]); // Setmode, no parameters
        let _ = take(&mut c, &mut irq);
        assert_eq!(c.mode, 0, "and not 0x16");
    }

    #[test]
    fn the_index_selects_which_register_an_address_is() {
        let mut c = Cdrom::new();
        c.write(0, 1);
        c.write(2, 0x1F); // interrupt enable, at index 1
        assert_eq!(c.read(3) & 0x1F, 0, "index 1 reads the flags");
        c.write(0, 0);
        assert_eq!(c.read(3) & 0x1F, 0x1F, "index 0 reads the enable");
    }

    #[test]
    fn writing_the_flags_acknowledges_rather_than_assigns() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x01, &[]);
        c.run(ACK_DELAY, &mut irq);
        assert_eq!(c.irq_flags, INT3_ACK);

        c.write(0, 1);
        c.write(3, 0x01); // clear bit 0 only
        assert_eq!(c.irq_flags, INT3_ACK & !0x01, "only the written bits clear");
    }

    #[test]
    fn a_masked_interrupt_still_latches_in_the_controller() {
        let mut c = Cdrom::new(); // interrupt enable left at zero
        let mut irq = Irq::new();
        issue(&mut c, 0x01, &[]);
        c.run(ACK_DELAY, &mut irq);

        assert_eq!(c.irq_flags, INT3_ACK, "the controller has its response");
        assert_eq!(
            irq.stat() & (1 << irq::CDROM),
            0,
            "but the CPU never sees it"
        );
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_ignored() {
        let mut c = enabled();
        let mut irq = Irq::new();
        issue(&mut c, 0x7F, &[]);
        let (code, _) = take(&mut c, &mut irq);
        assert_eq!(code, INT5_ERROR);
        assert_eq!(c.unknown_commands, 1);
    }
}
