// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! SIO0: the controller and memory card port.
//!
//! One synchronous serial port shared by four devices, not four ports. The
//! select line in `JOY_CTRL` bit 13 picks a pair, and the **first byte of the
//! transfer** picks which of that pair answers: `0x01` for the controller,
//! `0x81` for the memory card. So there is one transfer state machine here with
//! a target, rather than four devices poked independently.
//!
//! Every byte is an exchange. The byte going out and the byte coming in cross
//! on the wire, so a device's answer is always one step behind the command that
//! asked for it. See `docs/notes/SIO.md`.

use crate::irq::{self, Irq};

/// Cycles from the end of a byte to the device pulling `/ACK` low.
///
/// Approximate, and it matters that it is not zero. The BIOS reads the pad with
/// the controller interrupt **masked off in `I_MASK`**, and polls `I_STAT` bit 7
/// in a tight loop instead: it writes a byte, spins until the bit appears,
/// clears it, reads the reply, and writes the next byte. So this delay is the
/// thing that paces the whole transfer, and an acknowledge that arrives in zero
/// cycles would let the loop run a byte ahead of the port.
const PAD_ACK_DELAY: u64 = 338;

/// How long `/ACK` stays low once asserted.
const ACK_WIDTH: u64 = 10;

/// `RSTA_SIO_TRACE=1` logs every byte exchanged on the port.
///
/// The same tool as `RSTA_GTE_TRACE`, and for the same reason: the counters
/// tell you *how much* software talked to the port, and only the bytes tell you
/// what it asked for.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RSTA_SIO_TRACE").is_ok_and(|v| v != "0"))
}

/// Buttons in the order the pad puts them on the wire.
///
/// The values are bit positions in [`Pad::buttons`], where **1 means pressed**.
/// The wire wants the opposite, and the inversion happens once, in
/// [`Pad::reply`], rather than being carried through the host-facing API.
pub mod button {
    pub const SELECT: u16 = 0;
    pub const L3: u16 = 1;
    pub const R3: u16 = 2;
    pub const START: u16 = 3;
    pub const UP: u16 = 4;
    pub const RIGHT: u16 = 5;
    pub const DOWN: u16 = 6;
    pub const LEFT: u16 = 7;
    pub const L2: u16 = 8;
    pub const R2: u16 = 9;
    pub const L1: u16 = 10;
    pub const R1: u16 = 11;
    pub const TRIANGLE: u16 = 12;
    pub const CIRCLE: u16 = 13;
    pub const CROSS: u16 = 14;
    pub const SQUARE: u16 = 15;
}

/// A digital controller.
#[derive(Clone, Copy, Default)]
pub struct Pad {
    pub connected: bool,
    /// One bit per [`button`], set while held.
    pub buttons: u16,
}

impl Pad {
    /// The reply to byte `step` of a transfer, and whether the pad acknowledges.
    ///
    /// The final byte is deliberately not acknowledged: the missing pulse is
    /// the only signal software gets that the transfer is over.
    fn reply(&self, step: u32) -> (u8, bool) {
        // L3 and R3 are in the layout but a digital pad never asserts them.
        let held = self.buttons & !((1 << button::L3) | (1 << button::R3));
        let wire = !held;
        match step {
            0 => (0xFF, true),
            1 => (0x41, true), // digital pad
            2 => (0x5A, true),
            3 => (wire as u8, true),
            4 => ((wire >> 8) as u8, false),
            _ => (0xFF, false),
        }
    }
}

/// Which device claimed the transaction in progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// Nothing addressed yet, or an address byte nothing recognised.
    None,
    Pad,
}

impl Target {
    fn code(self) -> u8 {
        match self {
            Target::None => 0,
            Target::Pad => 1,
        }
    }

    fn from_code(v: u8) -> Target {
        match v {
            1 => Target::Pad,
            _ => Target::None,
        }
    }
}

#[derive(Clone)]
pub struct Sio {
    mode: u16,
    ctrl: u16,
    baud: u16,

    /// The byte last shifted in, if software has not read it yet.
    rx: Option<u8>,
    /// Byte index within the current transaction.
    step: u32,
    target: Target,

    /// Cycles until `/ACK` asserts, then until it releases. `None` between
    /// transfers. Held as a countdown rather than an absolute cycle so that a
    /// save state carries no dependence on when the machine happened to start.
    ack_countdown: Option<u64>,
    /// `/ACK` is low right now: `JOY_STAT` bit 7.
    ack_level: bool,
    /// `JOY_STAT` bit 9, cleared by writing `JOY_CTRL` bit 4.
    irq: bool,

    /// Port 1 and port 2. Host-facing: the frontend writes these directly.
    pub pads: [Pad; 2],

    /// Bytes exchanged, and how many of those a device answered. Host-side
    /// observation only, never serialized: they exist so a harness can tell
    /// "software never talked to the port" from "software talked to it and did
    /// not like the answer", which look identical from the outside.
    pub transfers: u64,
    pub acknowledged: u64,
}

impl Default for Sio {
    fn default() -> Sio {
        Sio::new()
    }
}

impl Sio {
    pub fn new() -> Sio {
        Sio {
            mode: 0,
            ctrl: 0,
            baud: 0,
            rx: None,
            step: 0,
            target: Target::None,
            ack_countdown: None,
            ack_level: false,
            irq: false,
            pads: [
                Pad {
                    connected: true,
                    buttons: 0,
                },
                Pad::default(),
            ],
            transfers: 0,
            acknowledged: 0,
        }
    }

    fn selected(&self) -> bool {
        self.ctrl & 0x0002 != 0
    }

    fn slot(&self) -> usize {
        ((self.ctrl >> 13) & 1) as usize
    }

    fn ack_irq_enabled(&self) -> bool {
        self.ctrl & 0x1000 != 0
    }

    // ---- register interface ----------------------------------------------

    /// `offset` is relative to `0x1F801040`.
    pub fn read(&mut self, offset: u32, width: u32) -> u32 {
        match offset & !3 {
            0 => self.read_rx() as u32,
            4 => self.status(),
            8 => {
                // MODE at +8, CTRL at +A: one word holds both.
                if width == 4 {
                    self.mode as u32 | ((self.control() as u32) << 16)
                } else if offset & 2 == 0 {
                    self.mode as u32
                } else {
                    self.control() as u32
                }
            }
            12 => {
                if width == 4 {
                    (self.baud as u32) << 16
                } else if offset & 2 == 0 {
                    0
                } else {
                    self.baud as u32
                }
            }
            _ => 0,
        }
    }

    /// `offset` is relative to `0x1F801040`.
    pub fn write(&mut self, offset: u32, width: u32, val: u32) {
        match offset & !3 {
            0 => self.transfer(val as u8),
            4 => {} // STAT is read only
            8 => {
                if width == 4 {
                    self.mode = val as u16;
                    self.set_control((val >> 16) as u16);
                } else if offset & 2 == 0 {
                    self.mode = val as u16;
                } else {
                    self.set_control(val as u16);
                }
            }
            12 => {
                if width == 4 {
                    self.baud = (val >> 16) as u16;
                } else if offset & 2 != 0 {
                    self.baud = val as u16;
                }
            }
            _ => {}
        }
    }

    fn read_rx(&mut self) -> u8 {
        // An empty FIFO reads 0xFF here. That is a choice, not a measurement:
        // see the open questions in docs/notes/SIO.md.
        self.rx.take().unwrap_or(0xFF)
    }

    fn status(&self) -> u32 {
        let mut s = 0u32;
        s |= 1 << 0; // TX is always ready: transfers complete within the write
        if self.rx.is_some() {
            s |= 1 << 1;
        }
        s |= 1 << 2; // and always finished, for the same reason
        if self.ack_level {
            s |= 1 << 7;
        }
        if self.irq {
            s |= 1 << 9;
        }
        s
    }

    /// `JOY_CTRL` as software reads it back. Bits 4 and 6 are strobes and read
    /// as zero; storing them would re-fire on any read-modify-write.
    fn control(&self) -> u16 {
        self.ctrl & !0x0050
    }

    fn set_control(&mut self, val: u16) {
        if val & 0x0040 != 0 {
            // Reset. Everything except the pads, which are the host's, and the
            // observation counters, which are the harness's.
            let (pads, transfers, acknowledged) = (self.pads, self.transfers, self.acknowledged);
            *self = Sio::new();
            self.pads = pads;
            self.transfers = transfers;
            self.acknowledged = acknowledged;
            return;
        }

        let was_selected = self.selected();
        self.ctrl = val & !0x0050;

        if val & 0x0010 != 0 {
            self.irq = false;
        }

        // Dropping the select line ends the transaction. Without this, software
        // that aborts a transfer part way leaves the state machine mid-protocol
        // and the next transfer's address byte is read as payload.
        if was_selected && !self.selected() {
            self.step = 0;
            self.target = Target::None;
            self.ack_countdown = None;
            self.ack_level = false;
        }
    }

    // ---- the transfer ----------------------------------------------------

    fn transfer(&mut self, tx: u8) {
        // With TX disabled or nothing selected the byte goes nowhere, and no
        // device is listening to answer it.
        if self.ctrl & 0x0001 == 0 || !self.selected() {
            self.rx = Some(0xFF);
            return;
        }

        if self.step == 0 {
            self.target = match tx {
                0x01 => Target::Pad,
                _ => Target::None,
            };
        }

        let (rx, ack) = match self.target {
            Target::Pad => {
                let pad = self.pads[self.slot()];
                if pad.connected {
                    pad.reply(self.step)
                } else {
                    // Nothing drives the line and nothing pulls /ACK. There is
                    // no "absent" status bit; software finds out by timing out.
                    (0xFF, false)
                }
            }
            Target::None => (0xFF, false),
        };

        if trace_enabled() {
            eprintln!(
                "sio slot={} step={} tx={tx:02x} rx={rx:02x} ack={}",
                self.slot(),
                self.step,
                u8::from(ack)
            );
        }

        self.rx = Some(rx);
        self.step += 1;
        self.transfers += 1;
        self.acknowledged += u64::from(ack);

        if ack {
            // Back to the "waiting to assert" phase, always. Software can start
            // the next byte while the previous pulse is still low, and leaving
            // the level set makes the countdown's expiry read as the *release*
            // of the old pulse instead of the assertion of the new one, so the
            // interrupt for that byte never fires at all. See docs/notes/SIO.md.
            self.ack_level = false;
            self.ack_countdown = Some(PAD_ACK_DELAY);
        } else {
            self.ack_countdown = None;
            self.step = 0;
            self.target = Target::None;
        }
    }

    // ---- timing ----------------------------------------------------------

    /// Advance `/ACK` by `elapsed` cycles, raising the controller interrupt on
    /// the falling edge.
    pub fn run(&mut self, elapsed: u64, irq: &mut Irq) {
        let Some(left) = self.ack_countdown else {
            return;
        };
        if elapsed < left {
            self.ack_countdown = Some(left - elapsed);
            return;
        }

        // The countdown covers two phases: assert, then release ACK_WIDTH
        // cycles later. Both can land inside one `elapsed` at coarse
        // granularity, which is why the release is computed rather than
        // scheduled as a second event.
        let overshoot = elapsed - left;
        if !self.ack_level {
            self.ack_level = true;
            self.irq = true;
            if self.ack_irq_enabled() {
                irq.raise(irq::CONTROLLER);
            }
            if overshoot >= ACK_WIDTH {
                self.ack_level = false;
                self.ack_countdown = None;
            } else {
                self.ack_countdown = Some(ACK_WIDTH - overshoot);
            }
        } else {
            self.ack_level = false;
            self.ack_countdown = None;
        }
    }

    /// Cycles until `/ACK` next changes, for the scheduler.
    pub fn cycles_to_event(&self) -> Option<u64> {
        self.ack_countdown
    }

    // ---- save state ------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn parts(&self) -> (u16, u16, u16, u16, u32, u8, u64, u8, u8) {
        (
            self.mode,
            self.ctrl,
            self.baud,
            // The RX latch is one byte plus "is there one", packed so the
            // serialized form has no Option and no padding.
            match self.rx {
                Some(b) => 0x100 | b as u16,
                None => 0,
            },
            self.step,
            self.target.code(),
            self.ack_countdown.unwrap_or(0),
            u8::from(self.ack_countdown.is_some()),
            u8::from(self.ack_level) | (u8::from(self.irq) << 1),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        mode: u16,
        ctrl: u16,
        baud: u16,
        rx: u16,
        step: u32,
        target: u8,
        ack_countdown: u64,
        ack_pending: u8,
        flags: u8,
    ) {
        self.mode = mode;
        self.ctrl = ctrl;
        self.baud = baud;
        self.rx = if rx & 0x100 != 0 {
            Some(rx as u8)
        } else {
            None
        };
        self.step = step;
        self.target = Target::from_code(target);
        self.ack_countdown = (ack_pending != 0).then_some(ack_countdown);
        self.ack_level = flags & 1 != 0;
        self.irq = flags & 2 != 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one full five-byte read the way the BIOS does, and return the
    /// bytes that came back.
    fn read_pad(sio: &mut Sio, irq: &mut Irq) -> Vec<u8> {
        sio.write(0x0A, 2, 0x1003); // TXEN | select | ACK interrupt enable
        let mut out = Vec::new();
        for tx in [0x01u8, 0x42, 0x00, 0x00, 0x00] {
            sio.write(0, 1, tx as u32);
            sio.run(PAD_ACK_DELAY + ACK_WIDTH, irq);
            out.push(sio.read(0, 1) as u8);
            sio.write(0x0A, 2, 0x1013); // acknowledge
        }
        sio.write(0x0A, 2, 0x1000); // drop select
        out
    }

    #[test]
    fn a_digital_pad_answers_the_five_byte_sequence() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        assert_eq!(read_pad(&mut sio, &mut irq), [0xFF, 0x41, 0x5A, 0xFF, 0xFF]);
    }

    #[test]
    fn buttons_are_reported_active_low() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.pads[0].buttons = (1 << button::CROSS) | (1 << button::START);
        let r = read_pad(&mut sio, &mut irq);

        // Start is bit 3 of the low byte, Cross bit 6 of the high byte, and a
        // held button reads 0.
        assert_eq!(r[3], !(1u8 << 3), "low byte, Start held");
        assert_eq!(r[4], !(1u8 << 6), "high byte, Cross held");
    }

    #[test]
    fn l3_and_r3_are_never_asserted_by_a_digital_pad() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.pads[0].buttons = 0xFFFF;
        let r = read_pad(&mut sio, &mut irq);
        assert_eq!(r[3], (1 << 1) | (1 << 2), "only L3 and R3 stay released");
    }

    #[test]
    fn an_absent_pad_reads_all_ones_and_never_acknowledges() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.pads[0].connected = false;

        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);
        assert_eq!(sio.read(0, 1), 0xFF);
        assert_eq!(sio.cycles_to_event(), None, "nothing pulled /ACK");
        sio.run(10_000, &mut irq);
        assert!(!irq.pending() || irq.stat() & (1 << irq::CONTROLLER) == 0);
    }

    #[test]
    fn the_last_byte_is_not_acknowledged() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write(0x0A, 2, 0x1003);
        for tx in [0x01u8, 0x42, 0x00, 0x00] {
            sio.write(0, 1, tx as u32);
            assert!(sio.cycles_to_event().is_some(), "byte {tx:#04x} acked");
            sio.run(PAD_ACK_DELAY + ACK_WIDTH, &mut irq);
        }
        sio.write(0, 1, 0x00); // the fifth and last
        assert_eq!(
            sio.cycles_to_event(),
            None,
            "the missing pulse is how software learns the transfer ended"
        );
    }

    #[test]
    fn a_new_byte_restarts_the_acknowledge_pulse() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);

        // Let the pulse assert, but not release.
        sio.run(PAD_ACK_DELAY, &mut irq);
        assert_ne!(sio.status() & (1 << 7), 0, "still low");
        irq.ack(!(1 << irq::CONTROLLER));
        sio.write(0x0A, 2, 0x1013);

        // Software is entitled to start the next byte here, and that byte must
        // interrupt in its own right. Leaving the level set makes the next
        // expiry read as the release of the *old* pulse, and the interrupt for
        // this byte never fires: the BIOS then reads three bytes of a five-byte
        // pad report and gives up.
        sio.write(0, 1, 0x42);
        sio.run(PAD_ACK_DELAY, &mut irq);
        assert_ne!(
            irq.stat() & (1 << irq::CONTROLLER),
            0,
            "the second byte acknowledged too"
        );
    }

    #[test]
    fn the_acknowledge_is_delayed() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);

        sio.run(PAD_ACK_DELAY - 1, &mut irq);
        assert_eq!(sio.status() & (1 << 9), 0, "not yet");
        assert_eq!(irq.stat() & (1 << irq::CONTROLLER), 0);

        sio.run(1, &mut irq);
        assert_ne!(sio.status() & (1 << 9), 0, "STAT bit 9");
        assert_ne!(sio.status() & (1 << 7), 0, "/ACK reads 1 while the line is low");
        assert_ne!(irq.stat() & (1 << irq::CONTROLLER), 0);
    }

    #[test]
    fn the_acknowledge_interrupt_can_be_masked_off() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write(0x0A, 2, 0x0003); // TXEN | select, but no ACK interrupt
        sio.write(0, 1, 0x01);
        sio.run(PAD_ACK_DELAY, &mut irq);

        assert_ne!(sio.status() & (1 << 9), 0, "the port still latches it");
        assert_eq!(irq.stat() & (1 << irq::CONTROLLER), 0, "the CPU does not see it");
    }

    #[test]
    fn dropping_the_select_line_restarts_the_transaction() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);
        sio.run(PAD_ACK_DELAY + ACK_WIDTH, &mut irq);
        sio.write(0, 1, 0x42);
        let _ = sio.read(0, 1);

        sio.write(0x0A, 2, 0x1001); // abort part way through
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);
        assert_eq!(sio.read(0, 1), 0xFF, "back at the address byte");
        sio.run(PAD_ACK_DELAY + ACK_WIDTH, &mut irq);
        sio.write(0, 1, 0x42);
        assert_eq!(sio.read(0, 1), 0x41, "and step 1 answers as step 1");
    }

    #[test]
    fn an_unrecognised_address_byte_selects_nothing() {
        let mut sio = Sio::new();
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x81); // a memory card, which is not implemented
        assert_eq!(sio.read(0, 1), 0xFF);
        assert_eq!(sio.cycles_to_event(), None);
    }

    #[test]
    fn a_transfer_without_tx_enabled_goes_nowhere() {
        let mut sio = Sio::new();
        sio.write(0x0A, 2, 0x1002); // select, but TXEN clear
        sio.write(0, 1, 0x01);
        assert_eq!(sio.read(0, 1), 0xFF);
        assert_eq!(sio.cycles_to_event(), None);
    }

    #[test]
    fn the_strobe_bits_read_back_as_zero() {
        let mut sio = Sio::new();
        sio.write(0x0A, 2, 0x1013);
        assert_eq!(
            sio.read(0x0A, 2) & 0x0050,
            0,
            "acknowledge and reset are strobes, not state"
        );
    }

    #[test]
    fn reset_clears_the_port_but_not_the_pads() {
        let mut sio = Sio::new();
        let mut irq = Irq::new();
        sio.pads[0].buttons = 1 << button::CROSS;
        sio.write(0x0A, 2, 0x1003);
        sio.write(0, 1, 0x01);
        sio.run(PAD_ACK_DELAY, &mut irq);

        sio.write(0x0A, 2, 0x0040);
        assert_eq!(sio.status() & (1 << 9), 0, "interrupt cleared");
        assert_eq!(sio.cycles_to_event(), None);
        assert_eq!(
            sio.pads[0].buttons,
            1 << button::CROSS,
            "the pad belongs to the host, not the port"
        );
    }
}
