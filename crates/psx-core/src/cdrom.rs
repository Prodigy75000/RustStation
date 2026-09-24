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
//! The drive reads from a [`crate::disc::Disc`] when one is inserted, and
//! answers with the empty-tray error when one is not. See `docs/notes/CDROM.md`.

use crate::disc::{self, Disc, RAW_SECTOR};
use crate::irq::{self, Irq};
use crate::xa;

/// Cycles from a command being written to its acknowledgement.
///
/// Approximate. `cdrom/timing` measures this precisely, but every measurement in
/// it needs a disc in the drive, so none of it can be checked yet.
const ACK_DELAY: u64 = 50_401;
/// Cycles from the acknowledgement to a completion, for the commands that have
/// a second response.
const COMPLETE_DELAY: u64 = 120_000;

/// Cycles per sector at single speed.
///
/// This one is not a guess: the drive turns at exactly 75 sectors per second,
/// so it is the CPU clock divided by 75. `cdrom/timing` measures 446 040 ticks
/// against this 451 584, and 222 222 against the double-speed 225 792, which is
/// within about 1% and is the closest thing to a confirmation available without
/// a disc of its own.
const SECTOR_CYCLES: u64 = 33_868_800 / 75;

/// Response interrupt codes, in the low three bits of the flags register.
const INT1_DATA: u8 = 1;
const INT2_COMPLETE: u8 = 2;
const INT3_ACK: u8 = 3;
const INT4_END: u8 = 4;
const INT5_ERROR: u8 = 5;

/// Drive status bits.
const STAT_ERROR: u8 = 1 << 0;
const STAT_MOTOR: u8 = 1 << 1;
const STAT_SHELL_OPEN: u8 = 1 << 4;
const STAT_READING: u8 = 1 << 5;
const STAT_SEEKING: u8 = 1 << 6;
const STAT_PLAYING: u8 = 1 << 7;

/// Mode bits, from `Setmode`.
const MODE_AUTOPAUSE: u8 = 1 << 1;
const MODE_REPORT: u8 = 1 << 2;
const MODE_XA_FILTER: u8 = 1 << 3;
const MODE_WHOLE_SECTOR: u8 = 1 << 5;
const MODE_XA_ADPCM: u8 = 1 << 6;
const MODE_DOUBLE_SPEED: u8 = 1 << 7;

/// Submode bits, from a Mode 2 sector's subheader. Bit 1 is video, bit 3 data,
/// bit 0 end-of-record and bit 7 end-of-file; none of those change what the
/// drive does with the sector, so only the three that do are named.
const SUBMODE_AUDIO: u8 = 1 << 2;
#[cfg_attr(not(test), allow(dead_code))] // routing ignores it; the tests build sectors with it
const SUBMODE_FORM2: u8 = 1 << 5;
const SUBMODE_REALTIME: u8 = 1 << 6;

/// A Mode 2 sector, which is the only kind that carries a subheader.
const SECTOR_MODE2: u8 = 2;

/// Bytes a sector yields: the user data alone, or everything from the header on.
const DATA_2048: usize = 2048;
const DATA_2340: usize = 2340;

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

/// CD audio waiting for the SPU, in 44 100 Hz stereo frames. A CD-DA sector is
/// 588 frames and an XA sector at most 9 408, so this holds a couple of
/// sectors of either with room to spare.
pub const AUDIO_FIFO: usize = 16_384;

/// Stereo frames in one CD-DA sector: 2352 bytes of 16-bit left/right pairs.
const CDDA_FRAMES: usize = 588;

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

/// Where the drive sends a sector it has read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Software,
    Decoder,
    Dropped,
}

pub struct Cdrom {
    index: u8,
    irq_enable: u8,
    irq_flags: u8,

    /// The drive status byte, which is not the same thing as the status
    /// register at `0x1F801800`.
    stat: u8,
    mode: u8,
    /// The XA file and channel from `Setfilter`. Nothing filters on them,
    /// because the filter only chooses which audio stream reaches a decoder
    /// this core does not have, but `Getparam` reports them back and software
    /// is entitled to check that what it set is what it gets.
    filter: [u8; 2],
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

    /// Where the head is, and where `Setloc` last pointed it.
    read_lba: u32,
    seek_target: u32,
    /// A read is running: sectors keep arriving until something stops it.
    reading: bool,
    /// Cycles until the next sector, while `reading`.
    sector_countdown: u64,

    /// The last sector read, from its header onwards, which is what `GetlocL`
    /// reports and what the data FIFO serves.
    sector: [u8; DATA_2340],
    /// How much of `sector` software has asked for, and how far it has read.
    /// Zero until the request register's buffer-read bit is set: the sector
    /// exists in the drive before software asks for it.
    data_len: u16,
    data_pos: u16,

    /// The disc in the drive. Host-provided and **not serialized**, the same
    /// way the BIOS image is not: a save state records that a disc was present,
    /// not the disc itself.
    pub disc: Option<Disc>,

    /// Commands seen, and commands this core does not recognise. Host-side
    /// observation only, never serialized.
    pub commands: u64,
    pub unknown_commands: u64,
    /// Sectors delivered. The figure that says whether a game is actually
    /// loading or just asking politely.
    pub sectors_read: u64,
    /// Sectors the drive took off the disc and routed to the audio decoder
    /// rather than to software. Counted separately because they are read and
    /// then deliberately not reported, which from software's side is
    /// indistinguishable from a sector that was never read at all.
    pub xa_sectors: u64,

    /// CD audio. The volume matrix is left-to-left, left-to-right,
    /// right-to-right, right-to-left, 0x80 being unity. What software writes
    /// goes to `atv_next`, and only takes effect when ADPCTL bit 5 applies it.
    pub(crate) atv: [u8; 4],
    pub(crate) atv_next: [u8; 4],
    /// ADPCTL bit 0: XA-ADPCM muted. CD-DA is not affected.
    pub(crate) xa_mute: bool,
    /// The `Mute` command: all CD audio silenced at the output, though the
    /// drive carries on decoding.
    pub(crate) muted: bool,
    /// CD-DA playback is running, a sector at a time, like a read that hands
    /// its sectors to the SPU instead of to software.
    pub(crate) playing: bool,
    pub(crate) play_countdown: u64,
    /// A `Setloc` not yet acted on, which is where `Play` starts from.
    pub(crate) setloc_pending: bool,
    pub(crate) xa: xa::Decoder,
    /// Frames waiting for the SPU, oldest at `audio_head`.
    pub(crate) audio: Vec<[i16; 2]>,
    pub(crate) audio_head: u16,
    pub(crate) audio_len: u16,

    /// CD-DA sectors played, and CD audio frames dropped because the FIFO was
    /// full. Host-side observation only.
    pub cdda_sectors: u64,
    pub audio_overflows: u64,
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
            filter: [0; 2],
            seek_loc: [0; 3],
            params: [0; PARAM_MAX],
            params_len: 0,
            response: [0; RESPONSE_MAX],
            response_len: 0,
            response_pos: 0,
            pending: [Response::default(); PENDING_MAX],
            pending_len: 0,
            countdown: 0,
            read_lba: 0,
            seek_target: 0,
            reading: false,
            sector_countdown: 0,
            sector: [0; DATA_2340],
            data_len: 0,
            data_pos: 0,
            disc: None,
            commands: 0,
            unknown_commands: 0,
            sectors_read: 0,
            xa_sectors: 0,
            atv: [0x80, 0, 0x80, 0],
            atv_next: [0x80, 0, 0x80, 0],
            xa_mute: false,
            muted: false,
            playing: false,
            play_countdown: 0,
            setloc_pending: false,
            xa: xa::Decoder::new(),
            audio: vec![[0; 2]; AUDIO_FIFO],
            audio_head: 0,
            audio_len: 0,
            cdda_sectors: 0,
            audio_overflows: 0,
        }
    }

    // ---- CD audio --------------------------------------------------------

    fn push_audio(&mut self, frame: [i16; 2]) {
        if self.audio_len as usize >= AUDIO_FIFO {
            self.audio_overflows += 1;
            return;
        }
        let at = (self.audio_head as usize + self.audio_len as usize) % AUDIO_FIFO;
        self.audio[at] = frame;
        self.audio_len += 1;
    }

    /// The next frame of CD audio for the SPU, after the drive's volume matrix
    /// and mute. Silence when there is nothing waiting.
    pub fn pop_audio(&mut self) -> [i16; 2] {
        if self.audio_len == 0 {
            return [0, 0];
        }
        let [l, r] = self.audio[self.audio_head as usize];
        self.audio_head = ((self.audio_head as usize + 1) % AUDIO_FIFO) as u16;
        self.audio_len -= 1;
        if self.muted {
            return [0, 0];
        }
        let (l, r) = (l as i32, r as i32);
        let a = self.atv.map(|v| v as i32);
        let out_l = (l * a[0] + r * a[3]) >> 7;
        let out_r = (r * a[2] + l * a[1]) >> 7;
        [
            out_l.clamp(-0x8000, 0x7FFF) as i16,
            out_r.clamp(-0x8000, 0x7FFF) as i16,
        ]
    }

    /// Frames of CD audio waiting. Host-side observation.
    pub fn audio_queued(&self) -> usize {
        self.audio_len as usize
    }

    // ---- register interface ----------------------------------------------

    /// `offset` is relative to `0x1F801800`.
    pub fn read(&mut self, offset: u32) -> u32 {
        let v = match offset & 3 {
            0 => self.status_register(),
            1 => self.pop_response(),
            2 => self.pop_data(),
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
            1 => match self.index {
                0 => self.command(val),
                3 => self.atv_next[2] = val, // right to right
                _ => {}
            },
            2 => match self.index {
                0 => self.push_param(val),
                1 => self.irq_enable = val & 0x1F,
                2 => self.atv_next[0] = val, // left to left
                _ => self.atv_next[3] = val, // right to left
            },
            3 if self.index == 2 => self.atv_next[1] = val, // left to right
            3 if self.index == 3 => {
                // ADPCTL: bit 0 mutes XA, bit 5 applies the volume matrix.
                if trace_enabled() {
                    eprintln!("cdrom adpctl {val:02x} atv_next {:02x?}", self.atv_next);
                }
                self.xa_mute = val & 0x01 != 0;
                if val & 0x20 != 0 {
                    self.atv = self.atv_next;
                }
            }
            // Index 0 of the last address is the request register: bit 7 hands
            // the sector the drive is holding to the data FIFO. Until software
            // asks, the sector exists but is not readable, which is why the
            // FIFO is loaded here and not when the sector arrives.
            3 if self.index == 0 => {
                if val & 0x80 != 0 {
                    // Only when the FIFO has been drained. Software is allowed
                    // to set this bit again while it is still working through a
                    // sector, and rewinding to the start there hands it the
                    // beginning twice. That is how Grand Theft Auto 2 failed:
                    // it reads the twelve-byte header and subheader of a
                    // whole-sector read, re-arms, and then expects the 2048
                    // bytes of user data. Rewinding gave it the header again,
                    // so every file it opened was twelve bytes out of step, and
                    // it retried the volume descriptor forever.
                    if self.data_pos >= self.data_len {
                        if trace_enabled() {
                            let skip = usize::from(self.mode & MODE_WHOLE_SECTOR == 0) * 12;
                            eprintln!(
                                "cdrom fifo mode={:02x} len={} first={:02x?}",
                                self.mode,
                                self.sector_bytes(),
                                &self.sector[skip..skip + 16]
                            );
                        }
                        self.data_len = self.sector_bytes() as u16;
                        self.data_pos = 0;
                    }
                } else {
                    self.data_len = 0;
                    self.data_pos = 0;
                }
            }
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
        if self.data_pos < self.data_len {
            s |= 1 << 6; // data FIFO not empty
        }
        s
    }

    /// How many bytes of a sector this mode serves: the 2048-byte user data, or
    /// everything from the sector header on.
    fn sector_bytes(&self) -> usize {
        if self.mode & MODE_WHOLE_SECTOR != 0 {
            DATA_2340
        } else {
            DATA_2048
        }
    }

    /// Take one byte from the data FIFO.
    ///
    /// With the whole sector selected the FIFO starts at the header; otherwise
    /// it starts at the user data, 12 bytes further in.
    fn pop_data(&mut self) -> u8 {
        if self.data_pos >= self.data_len {
            return 0;
        }
        let skip = if self.mode & MODE_WHOLE_SECTOR != 0 {
            0
        } else {
            12
        };
        let v = self.sector[skip + self.data_pos as usize];
        self.data_pos += 1;
        v
    }

    /// Drain the data FIFO a word at a time, which is what DMA channel 3 does.
    pub fn read_word(&mut self) -> u32 {
        let mut w = 0u32;
        for i in 0..4 {
            w |= (self.pop_data() as u32) << (8 * i);
        }
        w
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
                // Setloc, in BCD minute/second/frame. It only records where to
                // go; nothing moves until a seek or a read.
                self.seek_loc = [self.param(0), self.param(1), self.param(2)];
                self.seek_target = disc::msf_bcd_to_lba(self.seek_loc);
                self.setloc_pending = true;
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0E => {
                // Setmode
                self.mode = self.param(0);
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0D => {
                // Setfilter: which XA audio stream reaches the ADPCM decoder,
                // when Setmode bit 3 turns filtering on. See `route_sector`.
                self.filter = [self.param(0), self.param(1)];
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0B | 0x0C => {
                // Mute, Demute. The drive keeps decoding either way; muting
                // only zeroes what it sends the SPU.
                self.muted = cmd == 0x0B;
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            0x0F => {
                // Getparam: the mode and filter back, with a pad byte between.
                let s = self.take_stat();
                let out = [s, self.mode, 0, self.filter[0], self.filter[1]];
                self.queue(INT3_ACK, &out, ACK_DELAY);
            }
            0x0A => {
                // Init: acknowledge, then complete. The motor spins up whether
                // or not there is anything on it.
                self.mode = 0;
                self.filter = [0; 2];
                self.playing = false;
                self.stat = (self.stat & !STAT_SHELL_OPEN) | STAT_MOTOR;
                let s = self.stat;
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT2_COMPLETE, &[s], COMPLETE_DELAY);
            }
            0x08 | 0x09 => {
                // Stop, Pause. The acknowledgement still reports the drive as
                // reading; only the completion says it has stopped.
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.reading = false;
                self.playing = false;
                self.stat &= !(STAT_READING | STAT_SEEKING | STAT_PLAYING);
                if cmd == 0x08 {
                    self.stat &= !STAT_MOTOR;
                }
                let done = self.stat;
                self.queue(INT2_COMPLETE, &[done], COMPLETE_DELAY);
            }
            0x19 => self.test(),
            0x1A => {
                // GetID: acknowledge, then say what is in the drive.
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                match self.region() {
                    Some(region) => {
                        let mut out = [0x02, 0x00, 0x20, 0x00, 0, 0, 0, 0];
                        out[4..].copy_from_slice(&region);
                        self.queue(INT2_COMPLETE, &out, COMPLETE_DELAY);
                    }
                    None => self.queue(INT5_ERROR, &NO_DISC_ID, COMPLETE_DELAY),
                }
            }
            0x03 => self.play(),
            0x1E => {
                // ReadTOC: re-read the table of contents from the disc's lead-in.
                // Ours is parsed from the cue sheet once and cannot go stale, so
                // there is nothing to do but take the time and answer.
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
                self.queue(INT2_COMPLETE, &[s], COMPLETE_DELAY);
            }
            0x15 | 0x16 => self.seek(),
            0x06 | 0x1B => self.start_read(),
            0x10 => self.getloc_l(),
            0x11 => self.getloc_p(),
            0x13 => self.get_tn(),
            0x14 => self.get_td(),
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

    /// Everything that needs a disc answers the same way without one: an
    /// acknowledgement, then an error. Not silence. Software left waiting for a
    /// response that never comes is the failure this avoids.
    fn no_disc(&mut self) {
        let s = self.take_stat();
        self.queue(INT3_ACK, &[s], ACK_DELAY);
        self.queue(INT5_ERROR, &[s | STAT_ERROR, 0x80], COMPLETE_DELAY);
    }

    fn seek(&mut self) {
        if self.disc.is_none() {
            return self.no_disc();
        }
        let s = self.take_stat() | STAT_SEEKING;
        self.queue(INT3_ACK, &[s], ACK_DELAY);
        self.read_lba = self.seek_target;
        self.reading = false;
        self.playing = false;
        self.setloc_pending = false;
        self.stat = (self.stat & !(STAT_READING | STAT_SEEKING | STAT_PLAYING)) | STAT_MOTOR;
        // The head has to land before GetlocL can report where it is, so the
        // sector under it is fetched now rather than at the next read.
        let lba = self.read_lba;
        self.fetch_sector(lba);
        let done = self.stat;
        self.queue(INT2_COMPLETE, &[done], COMPLETE_DELAY);
    }

    fn start_read(&mut self) {
        if self.disc.is_none() {
            return self.no_disc();
        }
        self.read_lba = self.seek_target;
        self.reading = true;
        self.playing = false;
        self.setloc_pending = false;
        self.stat = (self.stat & !(STAT_SEEKING | STAT_PLAYING)) | STAT_MOTOR | STAT_READING;
        let s = self.take_stat();
        self.queue(INT3_ACK, &[s], ACK_DELAY);
        self.sector_countdown = self.sector_cycles();
    }

    /// `Play`: CD-DA from a track, from a pending `Setloc`, or from where the
    /// head is. The first parameter, when there is one and it is not zero, is a
    /// track number in BCD; one past the last restarts the current track.
    fn play(&mut self) {
        let Some(d) = self.disc.as_ref() else {
            return self.no_disc();
        };
        let track = if self.params_len > 0 {
            disc::from_bcd(self.param(0))
        } else {
            0
        };
        if track != 0 {
            let start = match d.tracks.iter().find(|t| t.number == track) {
                Some(t) => t.start_lba,
                None => d.track_at(self.read_lba).map(|t| t.start_lba).unwrap_or(0),
            };
            self.read_lba = start;
        } else if self.setloc_pending {
            self.read_lba = self.seek_target;
        }
        self.setloc_pending = false;
        self.reading = false;
        self.playing = true;
        self.play_countdown = self.sector_cycles();
        self.stat = (self.stat & !(STAT_READING | STAT_SEEKING)) | STAT_MOTOR | STAT_PLAYING;
        let s = self.stat;
        self.queue(INT3_ACK, &[s], ACK_DELAY);
    }

    /// Advance CD-DA playback by `elapsed` cycles, a sector at a time.
    fn run_play(&mut self, elapsed: u64, irq: &mut Irq) {
        let mut left = elapsed;
        while self.playing && left >= self.play_countdown {
            left -= self.play_countdown;
            self.play_countdown = self.sector_cycles();
            self.play_sector(irq);
        }
        if self.playing {
            self.play_countdown -= left;
        }
    }

    /// One sector of CD-DA: its samples to the SPU, a report if one is due,
    /// and the end of the track or the disc.
    fn play_sector(&mut self, irq: &mut Irq) {
        let lba = self.read_lba;
        let Some(d) = self.disc.as_mut() else {
            self.playing = false;
            return;
        };
        let here = d
            .track_at(lba)
            .map(|t| (t.number, t.mode == disc::TrackMode::Audio, t.start_lba));
        let mut raw = [0u8; RAW_SECTOR];
        let Some((track, audio, start)) = here.filter(|_| d.read_sector(lba, &mut raw)) else {
            // Off the end of the disc: the motor stops and INT4 says so.
            self.playing = false;
            self.stat &= !(STAT_PLAYING | STAT_MOTOR);
            let s = self.stat;
            self.deliver(INT4_END, &[s], irq);
            return;
        };
        if self.mode & MODE_AUTOPAUSE != 0 {
            let next = d.track_at(lba + 1).map(|t| t.number);
            if next != Some(track) {
                // End of the track: pause there, at the end of it.
                self.playing = false;
                self.stat &= !STAT_PLAYING;
                let s = self.stat;
                self.deliver(INT4_END, &[s], irq);
            }
        }

        // Data sectors play as silence.
        let mut peak = 0u16;
        for i in 0..CDDA_FRAMES {
            let frame = if audio {
                let l = i16::from_le_bytes([raw[4 * i], raw[4 * i + 1]]);
                let r = i16::from_le_bytes([raw[4 * i + 2], raw[4 * i + 3]]);
                peak = peak.max(l.unsigned_abs());
                [l, r]
            } else {
                [0, 0]
            };
            self.push_audio(frame);
        }
        self.read_lba = lba + 1;
        self.cdda_sectors += 1;

        // Reports come every ten frames, alternating absolute time (frames
        // 00, 20, 40, 60) and time within the track, with 80h added to the
        // seconds (frames 10, 30, 50, 70).
        let frame = lba % 75;
        if self.playing && self.mode & MODE_REPORT != 0 && frame.is_multiple_of(10) {
            let (msf, second_flag) = if frame.is_multiple_of(20) {
                (disc::lba_to_msf_bcd(lba), 0)
            } else {
                let within = lba.saturating_sub(start);
                (disc::frames_to_msf_bcd(within), 0x80)
            };
            let peak = peak.min(0x7FFF);
            let out = [
                self.stat,
                disc::to_bcd(track),
                0x01,
                msf[0],
                msf[1] | second_flag,
                msf[2],
                peak as u8,
                (peak >> 8) as u8,
            ];
            self.deliver(INT1_DATA, &out, irq);
        }
    }

    /// Deliver an unsolicited response now, if the last one has been
    /// acknowledged. A report that finds an interrupt outstanding is dropped,
    /// as the next one is only a few frames away.
    fn deliver(&mut self, code: u8, bytes: &[u8], irq: &mut Irq) {
        if self.irq_flags != 0 {
            return;
        }
        self.response[..bytes.len()].copy_from_slice(bytes);
        self.response_len = bytes.len() as u8;
        self.response_pos = 0;
        self.irq_flags = code;
        if self.irq_enable & code & 0x07 != 0 {
            irq.raise(irq::CDROM);
        }
    }

    fn sector_cycles(&self) -> u64 {
        if self.mode & MODE_DOUBLE_SPEED != 0 {
            SECTOR_CYCLES / 2
        } else {
            SECTOR_CYCLES
        }
    }

    /// Pull the sector at `lba` into the drive's buffer.
    ///
    /// Returns false past the end of the disc, which is how a read runs off the
    /// lead-out rather than looping forever.
    fn fetch_sector(&mut self, lba: u32) -> bool {
        let Some(disc) = self.disc.as_mut() else {
            return false;
        };
        let mut raw = [0u8; RAW_SECTOR];
        if !disc.read_sector(lba, &mut raw) {
            return false;
        }
        // From the header on: the 12-byte sync pattern is not part of what the
        // drive hands over.
        self.sector.copy_from_slice(&raw[12..12 + DATA_2340]);
        true
    }

    /// Does the sector in the buffer belong to the audio decoder rather than to
    /// software?
    ///
    /// This is the demultiplexer, and it lives in the drive because software
    /// cannot do it. Full-motion video on this console is one CD-XA stream
    /// carrying video and audio sectors interleaved, and it is read with the
    /// 2048-byte sector size, which hands software the user data and **not the
    /// subheader that says which kind it is**. So a drive that reports every
    /// sector gives the video player audio it has no way to recognise.
    ///
    /// The rule: with XA-ADPCM enabled, a real-time Form 2 audio sector is
    /// consumed by the drive. Everything else is reported. With XA-ADPCM
    /// disabled the same sector is ordinary data, which is how a program that
    /// wants to look at the audio itself gets to.
    ///
    /// `Setfilter` and its mode bit deliberately do not appear here. The filter
    /// chooses which of several interleaved audio streams reaches the decoder,
    /// and a sector it rejects is dropped rather than handed to software. So
    /// filtered and unfiltered audio are both withheld, and the filter cannot
    /// change what this answers. It will matter the day there is a decoder to
    /// route the accepted ones to.
    ///
    /// psx-spx's delivery rules, which this follows: a sector goes to the
    /// decoder if it is a Mode 2 real-time audio sector, XA-ADPCM is on, and
    /// either filtering is off or its file and channel match `Setfilter`.
    /// With filtering on, a real-time audio sector that was not decoded is
    /// dropped rather than reported. Form 2 is not part of the test.
    fn route_sector(&self) -> Route {
        const AUDIO: u8 = SUBMODE_REALTIME | SUBMODE_AUDIO;
        let audio = self.sector[3] == SECTOR_MODE2 && self.sector[6] & AUDIO == AUDIO;
        let filtering = self.mode & MODE_XA_FILTER != 0;
        let chosen = self.sector[4] == self.filter[0] && self.sector[5] == self.filter[1];
        if audio && self.mode & MODE_XA_ADPCM != 0 && (!filtering || chosen) {
            Route::Decoder
        } else if audio && filtering {
            Route::Dropped
        } else {
            Route::Software
        }
    }

    /// `GetlocL`: the header and subheader of the sector under the head.
    ///
    /// Taken from the sector's own header rather than from where we believe the
    /// head is, which is the entire point of the command.
    fn getloc_l(&mut self) {
        if self.disc.is_none() {
            return self.no_disc();
        }
        let mut out = [0u8; 8];
        out.copy_from_slice(&self.sector[..8]);
        self.queue(INT3_ACK, &out, ACK_DELAY);
    }

    /// `GetlocP`: track, index, and the position both within the track and on
    /// the disc as a whole.
    fn getloc_p(&mut self) {
        let Some(d) = self.disc.as_ref() else {
            return self.no_disc();
        };
        let lba = self.read_lba;
        let (track, index, start) = match d.track_at(lba) {
            Some(t) => (t.number, u8::from(lba >= t.start_lba), t.start_lba),
            None => (1, 1, 0),
        };
        // The relative position counts from the track, so it carries no
        // lead-in. It used to go through the lead-in conversion with 150
        // subtracted first, which clamped the first two seconds of every track
        // to 00:02:00.
        let within = lba.saturating_sub(start);
        let rel = disc::frames_to_msf_bcd(within);
        let abs = disc::lba_to_msf_bcd(lba);
        let out = [
            disc::to_bcd(track),
            disc::to_bcd(index),
            rel[0],
            rel[1],
            rel[2],
            abs[0],
            abs[1],
            abs[2],
        ];
        self.queue(INT3_ACK, &out, ACK_DELAY);
    }

    /// `GetTN`: the first and last track numbers, in BCD.
    fn get_tn(&mut self) {
        let Some(d) = self.disc.as_ref() else {
            return self.no_disc();
        };
        let first = d.tracks.first().map(|t| t.number).unwrap_or(1);
        let last = d.tracks.last().map(|t| t.number).unwrap_or(1);
        let s = self.take_stat();
        let out = [s, disc::to_bcd(first), disc::to_bcd(last)];
        self.queue(INT3_ACK, &out, ACK_DELAY);
    }

    /// `GetTD`: where a track starts, as minute and second. Track 0 means the
    /// lead-out, which is where the disc ends.
    fn get_td(&mut self) {
        let n = disc::from_bcd(self.param(0));
        let Some(d) = self.disc.as_ref() else {
            return self.no_disc();
        };
        let lba = if n == 0 {
            d.length
        } else {
            match d.tracks.iter().find(|t| t.number == n) {
                Some(t) => t.start_lba,
                None => {
                    let s = self.take_stat();
                    self.queue(INT5_ERROR, &[s | STAT_ERROR, 0x10], ACK_DELAY);
                    return;
                }
            }
        };
        let msf = disc::lba_to_msf_bcd(lba);
        let s = self.take_stat();
        let out = [s, msf[0], msf[1]];
        self.queue(INT3_ACK, &out, ACK_DELAY);
    }

    /// The four `SCEx` bytes `GetID` reports, or `None` for an empty tray.
    ///
    /// Read from the licence text the disc carries in its system area rather
    /// than assumed from the BIOS, because the two can disagree and it is the
    /// disagreement that produces the "wrong region" screen. **Untested**:
    /// there is no disc here to try it against, so the fallback matters, and
    /// the fallback is to report the disc as licensed rather than to reject it.
    fn region(&mut self) -> Option<[u8; 4]> {
        let disc = self.disc.as_mut()?;
        // A disc with no readable system area still reports as licensed, the
        // fallback this has always had.
        Some(
            disc.licence_region()
                .unwrap_or(disc::Region::America)
                .scex(),
        )
    }

    fn test(&mut self) {
        match self.param(0) {
            // The controller firmware's own date and version.
            0x20 => self.queue(INT3_ACK, &FIRMWARE_ID, ACK_DELAY),
            // Reset the SCEx counters and listen for region strings where the
            // head is. Those strings are only in the lead-in, and there is no
            // modchip here to fake them elsewhere, so nothing is ever counted.
            0x04 => {
                let s = self.take_stat();
                self.queue(INT3_ACK, &[s], ACK_DELAY);
            }
            // Read the SCEx counters: total and successful, both zero for the
            // reason above. This pair is an anti-modchip check. Crash Bash
            // plays a stretch of its disc at 08:28:00, muted, sends 04h, then
            // reads 05h and wants 00h,00h: a modchip would answer non-zero.
            // Refused, it restarted the whole check from Init, forever, on the
            // "Sony Computer Entertainment America presents" screen.
            0x05 => self.queue(INT3_ACK, &[0, 0], ACK_DELAY),
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
        // CD-DA runs on its own clock, whatever the command side is doing.
        if self.playing {
            self.run_play(elapsed, irq);
        }
        if self.pending_len == 0 {
            // A running read produces a sector at a time for as long as nothing
            // stops it, which is not a queued response: the queue is two deep
            // and a read is unbounded.
            if self.reading {
                self.run_read(elapsed, irq);
            }
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
            eprintln!("cdrom int{} {:02x?}", r.irq, &r.data[..r.len as usize]);
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

    /// Deliver the next sector of a running read.
    ///
    /// Gated on the interrupt being acknowledged like everything else, so a
    /// host that stops servicing the drive stalls the read instead of silently
    /// dropping sectors on the floor.
    fn run_read(&mut self, elapsed: u64, irq: &mut Irq) {
        if self.sector_countdown > elapsed {
            self.sector_countdown -= elapsed;
            return;
        }
        self.sector_countdown = 0;
        if self.irq_flags != 0 {
            return;
        }

        let lba = self.read_lba;
        if !self.fetch_sector(lba) {
            // Off the end of the disc. Stop, and say so, rather than looping.
            self.reading = false;
            self.stat &= !STAT_READING;
            let s = self.stat | STAT_ERROR;
            self.response[0] = s;
            self.response_len = 1;
            self.response_pos = 0;
            self.irq_flags = INT5_ERROR;
            if self.irq_enable & INT5_ERROR & 0x07 != 0 {
                irq.raise(irq::CDROM);
            }
            return;
        }
        self.read_lba = lba.wrapping_add(1);
        self.sectors_read += 1;
        self.sector_countdown = self.sector_cycles();

        match self.route_sector() {
            Route::Software => {}
            route => {
                // The drive keeps it. No interrupt, no data FIFO, and the head
                // carries on: from software's side this sector never existed.
                self.xa_sectors += 1;
                if trace_enabled() {
                    eprintln!("cdrom xa sector {lba} {route:?}");
                }
                if route == Route::Decoder {
                    let coding = xa::Coding::from_byte(self.sector[7]);
                    let mute = self.xa_mute;
                    let mut frames = Vec::with_capacity(9408);
                    self.xa.decode_sector(&self.sector, coding, &mut |f| {
                        frames.push(if mute { [0, 0] } else { f })
                    });
                    if trace_enabled() {
                        let peak = frames
                            .iter()
                            .map(|f| f[0].unsigned_abs().max(f[1].unsigned_abs()))
                            .max()
                            .unwrap_or(0);
                        eprintln!(
                            "cdrom xa decode {:?} ci={:02x} sub={:02x?} frames={} peak={peak}",
                            coding,
                            self.sector[7],
                            &self.sector[4..8],
                            frames.len()
                        );
                    }
                    for f in frames {
                        self.push_audio(f);
                    }
                }
                return;
            }
        }

        self.response[0] = self.stat;
        self.response_len = 1;
        self.response_pos = 0;
        self.irq_flags = INT1_DATA;

        if trace_enabled() {
            eprintln!("cdrom int1 sector {lba}");
        }
        if self.irq_enable & INT1_DATA & 0x07 != 0 {
            irq.raise(irq::CDROM);
        }
    }

    /// Cycles until the next response is due, for the scheduler.
    ///
    /// `None` while an unacknowledged interrupt is blocking delivery: there is
    /// nothing to wake up for until software writes the flags back.
    pub fn cycles_to_event(&self) -> Option<u64> {
        // A playing drive hands the SPU a sector on schedule, acknowledged or
        // not, and the SPU's samples depend on when it arrives.
        let play = self.playing.then_some(self.play_countdown.max(1));
        if self.irq_flags != 0 {
            return play;
        }
        let min = |t: u64| Some(play.map_or(t, |p| p.min(t)));
        if self.pending_len > 0 {
            return min(self.countdown.max(1));
        }
        if self.reading {
            return min(self.sector_countdown.max(1));
        }
        play
    }

    // ---- save state ------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn parts(&self) -> ([u8; 10], [u8; 3], [u8; PARAM_MAX], [u8; RESPONSE_MAX], u64) {
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
                self.filter[0],
                self.filter[1],
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
    /// The drive's own position and buffer, as distinct from the register
    /// block. Serialized because a state restored mid-read has to resume the
    /// read, not silently drop it.
    pub(crate) fn drive_parts(&self) -> (u32, u32, u8, u64, u16, u16, &[u8; DATA_2340]) {
        (
            self.read_lba,
            self.seek_target,
            u8::from(self.reading),
            self.sector_countdown,
            self.data_len,
            self.data_pos,
            &self.sector,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore_drive(
        &mut self,
        read_lba: u32,
        seek_target: u32,
        reading: u8,
        sector_countdown: u64,
        data_len: u16,
        data_pos: u16,
        sector: [u8; DATA_2340],
    ) {
        self.read_lba = read_lba;
        self.seek_target = seek_target;
        self.reading = reading != 0;
        self.sector_countdown = sector_countdown;
        // Clamped, not trusted: a length past the buffer would index out of it.
        self.data_len = data_len.min(DATA_2340 as u16);
        self.data_pos = data_pos.min(DATA_2340 as u16);
        self.sector = sector;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        regs: [u8; 10],
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
        self.filter = [regs[8], regs[9]];
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
    use crate::disc::Disc;

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

    // ---- with a disc in the drive ---------------------------------------

    /// A synthetic disc whose every sector carries its own number, so a read
    /// can be checked against the sector it should have come from.
    fn with_disc() -> Cdrom {
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n";
        let mut image = vec![0u8; RAW_SECTOR * 32];
        for lba in 0..32 {
            let base = lba * RAW_SECTOR;
            // A plausible Mode 2 Form 1 header, then a marker in the user data.
            let msf = disc::lba_to_msf_bcd(lba as u32);
            image[base + 12..base + 15].copy_from_slice(&msf);
            image[base + 15] = 2;
            image[base + 16] = 0x11; // subheader: file
            image[base + 17] = 0x22; // channel
            image[base + 24] = lba as u8;
            image[base + 25] = 0xA5;
        }
        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![image]).ok();
        assert!(c.disc.is_some());
        c
    }

    /// Seek to `lba` and start reading, leaving the drive running.
    fn seek_and_read(c: &mut Cdrom, irq: &mut Irq, lba: u32) {
        let msf = disc::lba_to_msf_bcd(lba);
        issue(c, 0x02, &msf); // Setloc
        let _ = take(c, irq);
        issue(c, 0x15, &[]); // SeekL
        let _ = take(c, irq);
        let _ = take(c, irq);
        issue(c, 0x06, &[]); // ReadN
        let _ = take(c, irq);
    }

    #[test]
    fn a_read_delivers_sectors_one_at_a_time() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        seek_and_read(&mut c, &mut irq, 10);

        for expected in 10..14u32 {
            let (code, _) = take(&mut c, &mut irq);
            assert_eq!(code, INT1_DATA, "sector {expected}");

            // The sector is in the drive but not readable until asked for.
            assert_eq!(c.status_register() & (1 << 6), 0, "FIFO empty until asked");
            c.write(0, 0);
            c.write(3, 0x80);
            assert_ne!(c.status_register() & (1 << 6), 0, "and full once asked");

            assert_eq!(c.pop_data(), expected as u8, "user data, first byte");
            assert_eq!(c.pop_data(), 0xA5);

            // Clear it again, or the next pass sees this sector's leftovers:
            // the FIFO holds what it was given until told otherwise.
            c.write(3, 0x00);
        }
        assert_eq!(c.sectors_read, 4);
    }

    /// Re-arming the request register partway through a sector must not rewind
    /// it.
    ///
    /// This is worth a test of its own because the failure is invisible at the
    /// register level and catastrophic above it. Grand Theft Auto 2 reads the
    /// twelve-byte header and subheader of a whole-sector read, sets the bit
    /// again, and expects the user data to follow. Rewinding handed it the
    /// header a second time, so every file it read was twelve bytes out of
    /// step; it rejected the volume descriptor and retried it 256 times rather
    /// than opening anything.
    /// `Play` has to report the drive as playing, and `Pause` has to stop it.
    ///
    /// Written when there was no CD audio at all, and kept because the lesson
    /// stands: software polls for the playing bit, and a drive that says it is
    /// idle immediately after being told to play is a spin.
    #[test]
    fn play_reports_the_drive_as_playing_until_it_is_paused() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x0A, &[]); // Init, to spin up
        let _ = take(&mut c, &mut irq);
        let _ = take(&mut c, &mut irq);

        issue(&mut c, 0x03, &[]);
        let (code, reply) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK, "Play is acknowledged, not refused");
        assert_ne!(
            reply[0] & STAT_PLAYING,
            0,
            "and the drive says it is playing"
        );
        assert_eq!(c.unknown_commands, 0);

        issue(&mut c, 0x09, &[]); // Pause
        let _ = take(&mut c, &mut irq);
        let (_, done) = take(&mut c, &mut irq);
        assert_eq!(done[0] & STAT_PLAYING, 0, "and stops when paused");
    }

    /// `Getparam` hands back what `Setfilter` was given: software is entitled
    /// to check that what it set is what it gets, and a drive that answers zero
    /// to that is a spin.
    /// The anti-modchip pair Crash Bash uses. A genuine drive reading away from
    /// the lead-in counts no SCEx strings, and both commands are answered, not
    /// refused: a refusal made the game restart its check forever.
    #[test]
    fn scex_counters_read_zero_away_from_the_lead_in() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x19, &[0x04]);
        let (code, _) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK, "04h is acknowledged, not an error");
        issue(&mut c, 0x19, &[0x05]);
        let (code, reply) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        assert_eq!(
            reply,
            [0, 0],
            "no strings counted, as on an unmodified console"
        );
    }

    #[test]
    fn getparam_reports_the_mode_and_the_filter_it_was_given() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_DOUBLE_SPEED | MODE_WHOLE_SECTOR]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x0D, &[3, 5]); // Setfilter: file 3, channel 5
        let _ = take(&mut c, &mut irq);

        issue(&mut c, 0x0F, &[]);
        let (code, reply) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        assert_eq!(reply[1], MODE_DOUBLE_SPEED | MODE_WHOLE_SECTOR, "the mode");
        assert_eq!(reply[3], 3, "the filter's file");
        assert_eq!(reply[4], 5, "the filter's channel");
        assert_eq!(c.unknown_commands, 0);

        // Init resets both, so a game that reinitialises the drive and then
        // asks is not told about the settings it just threw away.
        issue(&mut c, 0x0A, &[]);
        let _ = take(&mut c, &mut irq);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x0F, &[]);
        let (_, after) = take(&mut c, &mut irq);
        assert_eq!(after[1], 0, "Init clears the mode");
        assert_eq!((after[3], after[4]), (0, 0), "and the filter with it");
    }

    /// `ReadTOC` answers twice, like every other two-stage command.
    #[test]
    fn read_toc_acknowledges_then_completes() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x1E, &[]);
        assert_eq!(take(&mut c, &mut irq).0, INT3_ACK);
        assert_eq!(take(&mut c, &mut irq).0, INT2_COMPLETE);
        assert_eq!(c.unknown_commands, 0, "not refused as unknown");
    }

    #[test]
    fn re_arming_the_request_register_does_not_rewind() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_WHOLE_SECTOR]);
        let _ = take(&mut c, &mut irq);
        seek_and_read(&mut c, &mut irq, 5);
        let _ = take(&mut c, &mut irq);

        c.write(0, 0);
        c.write(3, 0x80);
        let header: Vec<u8> = (0..12).map(|_| c.pop_data()).collect();
        assert_eq!(
            header[0],
            disc::lba_to_msf_bcd(5)[0],
            "started at the header"
        );

        // Software may set the bit again while it is still working through the
        // sector. Hardware ignores that; only an empty FIFO reloads.
        c.write(3, 0x80);
        let next = c.pop_data();
        assert_ne!(
            next, header[0],
            "re-arming rewound the FIFO to the start of the sector"
        );
        assert_eq!(next, 5, "the thirteenth byte is the user data's first");

        // Draining it fully and then re-arming *does* reload, because that is
        // how software reads the same sector twice.
        while c.data_pos < c.data_len {
            c.pop_data();
        }
        c.write(3, 0x80);
        assert_eq!(
            c.pop_data(),
            disc::lba_to_msf_bcd(5)[0],
            "back at the header"
        );
    }

    #[test]
    fn the_whole_sector_mode_serves_the_header_too() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_WHOLE_SECTOR]); // Setmode
        let _ = take(&mut c, &mut irq);
        seek_and_read(&mut c, &mut irq, 5);
        let _ = take(&mut c, &mut irq);

        c.write(0, 0);
        c.write(3, 0x80);
        assert_eq!(c.data_len, DATA_2340 as u16, "2340 bytes, not 2048");
        // The first byte is now the header's minute, not the user data.
        assert_eq!(c.pop_data(), disc::lba_to_msf_bcd(5)[0]);
    }

    #[test]
    fn double_speed_halves_the_time_between_sectors() {
        let mut single = with_disc();
        let mut double = with_disc();
        let mut irq = Irq::new();

        issue(&mut double, 0x0E, &[MODE_DOUBLE_SPEED]);
        let _ = take(&mut double, &mut irq);

        seek_and_read(&mut single, &mut irq, 0);
        seek_and_read(&mut double, &mut irq, 0);
        assert_eq!(single.sector_cycles(), SECTOR_CYCLES);
        assert_eq!(double.sector_cycles(), SECTOR_CYCLES / 2);
    }

    #[test]
    fn getloc_l_reports_the_sector_under_the_head() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        let msf = disc::lba_to_msf_bcd(9);
        issue(&mut c, 0x02, &msf);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x15, &[]);
        let _ = take(&mut c, &mut irq);
        let _ = take(&mut c, &mut irq);

        issue(&mut c, 0x10, &[]); // GetlocL
        let (code, data) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        // Header, then subheader: the position comes from the sector itself.
        assert_eq!(&data[..3], &msf, "minute, second, frame");
        assert_eq!(data[3], 2, "mode");
        assert_eq!(data[4], 0x11, "subheader file");
        assert_eq!(data[5], 0x22, "subheader channel");
    }

    #[test]
    fn getloc_p_reports_the_track_and_both_positions() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        seek_and_read(&mut c, &mut irq, 20);
        let _ = take(&mut c, &mut irq); // one sector, so the head has moved

        issue(&mut c, 0x11, &[]); // GetlocP
        let (code, data) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        assert_eq!(data[0], 0x01, "track 1, in BCD");
        assert_eq!(data[1], 0x01, "index 1");
        assert_eq!(&data[5..8], &disc::lba_to_msf_bcd(21), "absolute position");
        assert_eq!(&data[2..5], &[0x00, 0x00, 0x21], "21 frames into track 1");
    }

    #[test]
    fn get_tn_and_get_td_describe_the_table_of_contents() {
        let cue = "FILE \"x.bin\" BINARY\n\
                   TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\n INDEX 01 00:01:00\n";
        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![vec![0u8; RAW_SECTOR * 200]]).ok();
        let mut irq = Irq::new();

        issue(&mut c, 0x13, &[]); // GetTN
        let (_, tn) = take(&mut c, &mut irq);
        assert_eq!(&tn[1..3], &[0x01, 0x02], "first and last track, in BCD");

        issue(&mut c, 0x14, &[0x02]); // GetTD for track 2
        let (_, td) = take(&mut c, &mut irq);
        let expected = disc::lba_to_msf_bcd(75);
        assert_eq!(&td[1..3], &expected[..2], "minute and second of track 2");
    }

    #[test]
    fn get_td_for_a_track_that_is_not_there_is_an_error() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x14, &[0x09]);
        let (code, _) = take(&mut c, &mut irq);
        assert_eq!(code, INT5_ERROR);
    }

    #[test]
    fn getid_reports_a_licensed_disc_when_one_is_present() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x1A, &[]);
        let (first, _) = take(&mut c, &mut irq);
        assert_eq!(first, INT3_ACK);
        let (second, data) = take(&mut c, &mut irq);
        assert_eq!(second, INT2_COMPLETE, "not the empty-tray error");
        assert_eq!(
            &data[4..8],
            b"SCEA",
            "the default when no region is stamped"
        );
    }

    #[test]
    fn the_region_comes_from_the_disc_not_from_a_guess() {
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n";
        let mut image = vec![0u8; RAW_SECTOR * 32];
        let text = b"Licensed  by          Sony Computer Entertainment Europe";
        image[4 * RAW_SECTOR + 24..4 * RAW_SECTOR + 24 + text.len()].copy_from_slice(text);

        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![image]).ok();
        let mut irq = Irq::new();
        issue(&mut c, 0x1A, &[]);
        let _ = take(&mut c, &mut irq);
        let (_, data) = take(&mut c, &mut irq);
        assert_eq!(&data[4..8], b"SCEE");
    }

    #[test]
    fn pause_stops_the_sectors_arriving() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        seek_and_read(&mut c, &mut irq, 0);
        let _ = take(&mut c, &mut irq);
        let before = c.sectors_read;

        issue(&mut c, 0x09, &[]); // Pause
        let _ = take(&mut c, &mut irq);
        let _ = take(&mut c, &mut irq);

        c.run(SECTOR_CYCLES * 8, &mut irq);
        assert_eq!(c.sectors_read, before, "nothing more arrived");
        assert_eq!(c.cycles_to_event(), None, "and nothing is scheduled");
    }

    #[test]
    fn reading_off_the_end_of_the_disc_stops_with_an_error() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        // The synthetic disc is 32 sectors long.
        seek_and_read(&mut c, &mut irq, 30);
        assert_eq!(take(&mut c, &mut irq).0, INT1_DATA, "sector 30");
        assert_eq!(take(&mut c, &mut irq).0, INT1_DATA, "sector 31");

        let (code, _) = take(&mut c, &mut irq);
        assert_eq!(code, INT5_ERROR, "rather than looping forever");
        assert!(!c.reading);
    }

    #[test]
    fn a_stalled_host_delays_the_read_rather_than_losing_sectors() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        seek_and_read(&mut c, &mut irq, 0);

        // Deliver one sector and never acknowledge it.
        c.run(SECTOR_CYCLES, &mut irq);
        assert_eq!(c.irq_flags, INT1_DATA);
        c.run(SECTOR_CYCLES * 10, &mut irq);
        assert_eq!(c.sectors_read, 1, "the read stalls, it does not skip ahead");
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
    /// A disc whose sectors alternate: every `audio_every`th one is a real-time
    /// Form 2 XA audio sector, the rest are ordinary real-time video.
    fn with_xa_disc(audio_every: usize) -> Cdrom {
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n";
        let mut image = vec![0u8; RAW_SECTOR * 32];
        for lba in 0..32 {
            let base = lba * RAW_SECTOR;
            let msf = disc::lba_to_msf_bcd(lba as u32);
            image[base + 12..base + 15].copy_from_slice(&msf);
            image[base + 15] = SECTOR_MODE2;
            image[base + 16] = 0x11; // file
            image[base + 17] = 0x22; // channel
            image[base + 18] = if lba % audio_every == audio_every - 1 {
                SUBMODE_REALTIME | SUBMODE_AUDIO | SUBMODE_FORM2
            } else {
                SUBMODE_REALTIME | 0x02 // real-time video
            };
            image[base + 24] = lba as u8;
        }
        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![image]).ok();
        assert!(c.disc.is_some());
        c
    }

    /// With XA-ADPCM armed, an audio sector is kept by the drive: no interrupt,
    /// nothing in the FIFO, and the next `INT1` is the sector after it.
    ///
    /// This cannot be left to software. The stream is read with the 2048-byte
    /// sector size, which hands over the user data and not the subheader that
    /// says which kind of sector it is, so a drive that reports every sector
    /// gives a video player audio it has no way to recognise.
    #[test]
    fn an_xa_audio_sector_never_reaches_software() {
        let mut c = with_xa_disc(4);
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_XA_ADPCM]); // Setmode: XA-ADPCM, 2048 bytes
        let _ = take(&mut c, &mut irq);
        seek_and_read(&mut c, &mut irq, 0);

        // Sectors 3, 7 and 11 are audio and must not appear.
        for expected in [0u8, 1, 2, 4, 5, 6, 8] {
            let (code, _) = take(&mut c, &mut irq);
            assert_eq!(code, INT1_DATA);
            c.write(0, 0);
            c.write(3, 0x80);
            assert_eq!(c.pop_data(), expected, "the audio sectors were skipped");
            c.write(3, 0x00);
        }
        assert_eq!(c.xa_sectors, 2, "sectors 3 and 7 went to the audio decoder");
    }

    /// The same sector with XA-ADPCM disabled is ordinary data. The mode bit
    /// decides, not the sector.
    #[test]
    fn without_xa_adpcm_an_audio_sector_is_just_data() {
        let mut c = with_xa_disc(4);
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[0]); // Setmode: no XA-ADPCM
        let _ = take(&mut c, &mut irq);
        seek_and_read(&mut c, &mut irq, 0);

        for expected in 0u8..5 {
            let (code, _) = take(&mut c, &mut irq);
            assert_eq!(code, INT1_DATA);
            c.write(0, 0);
            c.write(3, 0x80);
            assert_eq!(c.pop_data(), expected, "every sector is delivered");
            c.write(3, 0x00);
        }
        assert_eq!(c.xa_sectors, 0);
    }

    // ---- CD audio ----------------------------------------------------------

    /// A disc whose second track is audio: 20 sectors of data, then 20 of
    /// CD-DA where every sample's left channel is its frame index within the
    /// sector and its right channel the sector's LBA, so a misplaced byte or a
    /// swapped channel shows up as a wrong number.
    fn with_audio_track() -> Cdrom {
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n \
                   TRACK 02 AUDIO\n INDEX 01 00:00:20\n";
        let mut image = vec![0u8; RAW_SECTOR * 40];
        for lba in 20..40usize {
            for i in 0..CDDA_FRAMES {
                let base = lba * RAW_SECTOR + 4 * i;
                image[base..base + 2].copy_from_slice(&(i as i16).to_le_bytes());
                image[base + 2..base + 4].copy_from_slice(&(-(lba as i16)).to_le_bytes());
            }
        }
        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![image]).ok();
        assert!(c.disc.is_some());
        c
    }

    #[test]
    fn play_streams_the_audio_track_to_the_spu() {
        let mut c = with_audio_track();
        let mut irq = Irq::new();
        issue(&mut c, 0x03, &[0x02]); // Play track 2
        let (code, _) = take(&mut c, &mut irq);
        assert_eq!(code, INT3_ACK);
        let queued = c.audio_queued();
        c.run(SECTOR_CYCLES, &mut irq);
        assert_eq!(
            c.audio_queued() - queued,
            CDDA_FRAMES,
            "one sector, 588 frames"
        );
        assert_eq!(c.cdda_sectors as usize, 1 + queued / CDDA_FRAMES);

        // Default volume is unity, straight through.
        let first = c.pop_audio();
        assert_eq!(
            first,
            [0, -20],
            "sample 0 of track 2's first sector, LBA 20"
        );
        assert_eq!(c.pop_audio(), [1, -20]);
    }

    #[test]
    fn play_without_a_track_starts_at_a_pending_setloc() {
        let mut c = with_audio_track();
        let mut irq = Irq::new();
        issue(&mut c, 0x02, &disc::lba_to_msf_bcd(33)); // Setloc
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x03, &[]);
        let _ = take(&mut c, &mut irq);
        while c.audio_queued() == 0 {
            c.run(SECTOR_CYCLES, &mut irq);
        }
        assert_eq!(c.pop_audio(), [0, -33]);
    }

    #[test]
    fn a_data_track_plays_as_silence() {
        let mut c = with_disc();
        let mut irq = Irq::new();
        issue(&mut c, 0x03, &[]);
        let _ = take(&mut c, &mut irq);
        c.run(SECTOR_CYCLES * 2, &mut irq);
        assert!(c.audio_queued() >= CDDA_FRAMES);
        while c.audio_queued() > 0 {
            assert_eq!(c.pop_audio(), [0, 0]);
        }
    }

    #[test]
    fn play_reports_every_ten_frames_when_asked() {
        let mut c = with_audio_track();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_REPORT]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x02, &disc::lba_to_msf_bcd(20)); // frame 20: absolute
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x03, &[]);
        let _ = take(&mut c, &mut irq);
        let (code, report) = take(&mut c, &mut irq);
        assert_eq!(code, INT1_DATA);
        assert_eq!(report.len(), 8);
        assert_eq!(report[1], 0x02, "track 2");
        assert_eq!(
            &report[3..6],
            &disc::lba_to_msf_bcd(20),
            "absolute time on frame 20"
        );
        // Frame 30 is the next report, and is relative: 80h on the seconds.
        let (code, report) = take(&mut c, &mut irq);
        assert_eq!(code, INT1_DATA);
        assert_ne!(report[4] & 0x80, 0, "time within the track");
        assert_eq!(report[5], 0x10, "ten frames into the track");
    }

    #[test]
    fn play_off_the_end_of_the_disc_stops_with_int4() {
        let mut c = with_audio_track();
        let mut irq = Irq::new();
        issue(&mut c, 0x02, &disc::lba_to_msf_bcd(39));
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x03, &[]);
        let _ = take(&mut c, &mut irq);
        let (code, reply) = take(&mut c, &mut irq);
        assert_eq!(code, INT4_END);
        assert_eq!(reply[0] & STAT_PLAYING, 0);
        assert!(!c.playing);
    }

    #[test]
    fn the_volume_matrix_applies_only_when_told_to() {
        let mut c = Cdrom::new();
        c.push_audio([1000, -2000]);
        c.push_audio([1000, -2000]);
        c.push_audio([1000, -2000]);
        // Four different levels, so a crossed path gives a different answer.
        c.write(0, 2);
        c.write(2, 0x80); // L to L, unity
        c.write(3, 0x40); // L to R, half
        c.write(0, 3);
        c.write(1, 0x20); // R to R, a quarter
        c.write(2, 0x10); // R to L, an eighth
        assert_eq!(c.pop_audio(), [1000, -2000], "not applied yet");
        c.write(3, 0x20); // ADPCTL: apply
                          // Left: 1000 + -2000 / 8. Right: -2000 / 4 + 1000 / 2.
        assert_eq!(c.pop_audio(), [750, 0]);
        // Mute silences the output without draining any less.
        c.muted = true;
        assert_eq!(c.pop_audio(), [0, 0]);
        assert_eq!(c.audio_queued(), 0);
        assert_eq!(c.pop_audio(), [0, 0], "empty is silence");
    }

    /// A disc of XA audio sectors alternating between two channels, each
    /// filled with a constant non-zero nibble so decoded audio is audible.
    fn with_two_xa_channels() -> Cdrom {
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n";
        let mut image = vec![0u8; RAW_SECTOR * 16];
        for lba in 0..16 {
            let base = lba * RAW_SECTOR;
            image[base + 12..base + 15].copy_from_slice(&disc::lba_to_msf_bcd(lba as u32));
            image[base + 15] = SECTOR_MODE2;
            image[base + 16] = 1; // file
            image[base + 17] = (lba % 2) as u8; // channel
            image[base + 18] = SUBMODE_REALTIME | SUBMODE_AUDIO | SUBMODE_FORM2;
            image[base + 19] = 0x01; // stereo, 37 800 Hz, 4-bit
            let fill = if lba % 2 == 0 { 0x33 } else { 0x55 };
            for p in 0..18 {
                let portion = base + 24 + p * 128;
                // Headers shift 0, filter 0; data all the same nibble.
                for h in 0..16 {
                    image[portion + h] = 0x00;
                }
                for d in 16..128 {
                    image[portion + d] = fill;
                }
            }
        }
        let mut c = enabled();
        c.disc = Disc::from_memory(cue, vec![image]).ok();
        assert!(c.disc.is_some());
        c
    }

    #[test]
    fn xa_audio_is_decoded_into_the_fifo() {
        let mut c = with_two_xa_channels();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_XA_ADPCM]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x06, &[]); // ReadN from 0
        let _ = take(&mut c, &mut irq);
        c.run(c.sector_cycles(), &mut irq);
        // 2016 stereo frames at 37 800 Hz make 2352 at 44 100.
        assert_eq!(c.audio_queued(), 2352);
        assert_eq!(c.xa_sectors, 1);
        // Past the resampler's 29-tap warm-up, a constant 3 << 4 comes out
        // scaled by the table's gain.
        for _ in 0..40 {
            c.pop_audio();
        }
        let [l, r] = c.pop_audio();
        assert_eq!(l, r, "the same nibble on both channels");
        // 3 << 12 = 12288, times the gain of about 0.906, less up to half an
        // LSB for each of the 29 taps, which the document divides separately.
        assert!((11_090..11_160).contains(&l), "about 11 140: {l}");
    }

    #[test]
    fn the_filter_chooses_one_channel_and_drops_the_other() {
        let mut c = with_two_xa_channels();
        let mut irq = Irq::new();
        issue(&mut c, 0x0D, &[1, 1]); // Setfilter: file 1, channel 1
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x0E, &[MODE_XA_ADPCM | MODE_XA_FILTER]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x06, &[]);
        let _ = take(&mut c, &mut irq);
        for _ in 0..4 {
            c.run(c.sector_cycles(), &mut irq);
        }
        assert_eq!(c.xa_sectors, 4, "all four kept by the drive");
        assert_eq!(
            c.audio_queued(),
            2 * 2352,
            "but only channel 1's two decoded"
        );
        assert_eq!(c.irq_flags, 0, "and none reported to software");
    }

    #[test]
    fn with_the_filter_on_and_xa_off_audio_is_still_withheld() {
        let mut c = with_two_xa_channels();
        let mut irq = Irq::new();
        issue(&mut c, 0x0E, &[MODE_XA_FILTER]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x06, &[]);
        let _ = take(&mut c, &mut irq);
        c.run(c.sector_cycles(), &mut irq);
        assert_eq!(c.irq_flags, 0, "a real-time audio sector is not data");
        assert_eq!(
            c.audio_queued(),
            0,
            "and with XA off it is not audio either"
        );
    }

    #[test]
    fn the_xa_mute_bit_silences_xa_but_keeps_its_timing() {
        let mut c = with_two_xa_channels();
        let mut irq = Irq::new();
        c.write(0, 3);
        c.write(3, 0x01); // ADPCTL: mute XA
        issue(&mut c, 0x0E, &[MODE_XA_ADPCM]);
        let _ = take(&mut c, &mut irq);
        issue(&mut c, 0x06, &[]);
        let _ = take(&mut c, &mut irq);
        c.run(c.sector_cycles(), &mut irq);
        assert_eq!(c.audio_queued(), 2352);
        while c.audio_queued() > 0 {
            assert_eq!(c.pop_audio(), [0, 0]);
        }
    }
}
