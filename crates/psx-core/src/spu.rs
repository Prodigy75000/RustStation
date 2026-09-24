// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The SPU: 24 ADPCM voices, their envelopes, and the mixer.
//!
//! Written from psx-spx, "Sound Processing Unit (SPU)", and nothing else. The
//! distilled version, with the numbers and the guesses, is `docs/notes/SPU.md`.
//!
//! The chip runs one sample every 768 CPU cycles, which is exactly 44 100 Hz
//! off the 33.8688 MHz clock. Everything here advances in whole samples: a
//! remainder of cycles is carried between calls, so syncing every cycle and
//! syncing once a frame reach the same state, which is the same promise the
//! timers make.
//!
//! What one sample does, in order:
//!
//! 1. Key-off, then key-on, for whatever software asked for since the last
//!    sample. The hardware takes register writes at its own rate.
//! 2. Every voice: interpolate a sample (or take the noise level), scale it by
//!    the envelope, scale that by the two voice volumes, then advance the pitch
//!    counter, then step the envelope and the volume sweeps. All 24 voices run
//!    whether keyed on or not, because on hardware they do, and a silent voice
//!    reading past the IRQ address still raises the interrupt.
//! 3. The capture buffers at the bottom of sound RAM.
//! 4. The mix, the main volume, and the output.
//!
//! CD audio arrives one frame per sample from the drive, through the closure
//! [`Spu::run_with_cd`] is given, already through the drive's own volume
//! matrix. It is captured, then mixed at the CD volume if SPUCNT bit 0 allows.
//!
//! **Not here yet:** reverb.

/// Registers, `0x1F801C00` to `0x1F801E80`. 640 bytes, addressed as 16-bit.
pub const REG_BYTES: usize = 640;
const REGS: usize = REG_BYTES / 2;

/// Sound RAM. Half a megabyte, and every address wraps into it.
pub const RAM_BYTES: usize = 512 * 1024;
const RAM_MASK: u32 = RAM_BYTES as u32 - 1;

pub const VOICES: usize = 24;

/// CPU cycles per output sample. 33 868 800 / 44 100, exactly.
pub const CYCLES_PER_SAMPLE: u64 = 768;
pub const SAMPLE_RATE: u32 = 44_100;

/// Samples per ADPCM block: 16 bytes, two of header, fourteen of nibbles.
const BLOCK_SAMPLES: u32 = 28;

/// Host-side output is capped at a second of stereo audio. A harness that
/// never drains it must not grow without bound.
const OUT_CAP: usize = SAMPLE_RATE as usize * 2;

// Register offsets from 0x1F801C00, in bytes.
const MVOLL: usize = 0x180;
const MVOLR: usize = 0x182;
const KON: usize = 0x188;
const KOFF: usize = 0x18C;
const PMON: usize = 0x190;
const NON: usize = 0x194;
const ENDX: usize = 0x19C;
const IRQA: usize = 0x1A4;
const TRANSFER_ADDR: usize = 0x1A6;
const TRANSFER_FIFO: usize = 0x1A8;
const SPUCNT: usize = 0x1AA;
const RAM_CTRL: usize = 0x1AC;
const SPUSTAT: usize = 0x1AE;
const AVOLL: usize = 0x1B0;
const AVOLR: usize = 0x1B2;
const MVOLXL: usize = 0x1B8;
const MVOLXR: usize = 0x1BA;
const VOLX: usize = 0x200;
const VOLX_END: usize = VOLX + VOICES * 4;

// Per-voice registers, as a halfword index within the voice's 16 bytes.
const V_VOLL: usize = 0;
const V_PITCH: usize = 2;
const V_START: usize = 3;
const V_ADSR1: usize = 4;
const V_ADSR2: usize = 5;
const V_ENVX: usize = 6;
const V_REPEAT: usize = 7;

const CNT_ENABLE: u16 = 1 << 15;
const CNT_UNMUTE: u16 = 1 << 14;
const CNT_IRQ: u16 = 1 << 6;
const CNT_CD: u16 = 1 << 0;

const STAT_IRQ: u16 = 1 << 6;
const STAT_CAPTURE_HALF: u16 = 1 << 11;

/// ADPCM prediction filters. SPU-ADPCM has five; XA has the first four.
const POS: [i32; 5] = [0, 60, 115, 98, 122];
const NEG: [i32; 5] = [0, 0, -52, -55, -60];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Off = 0,
    Attack = 1,
    Decay = 2,
    Sustain = 3,
    Release = 4,
}

impl Phase {
    pub(crate) fn from_u8(v: u8) -> Option<Phase> {
        Some(match v {
            0 => Phase::Off,
            1 => Phase::Attack,
            2 => Phase::Decay,
            3 => Phase::Sustain,
            4 => Phase::Release,
            _ => return None,
        })
    }
}

/// One voice's moving parts. The registers software writes live in the
/// register file; this is what the hardware keeps behind them.
#[derive(Clone)]
pub struct Voice {
    /// Pitch counter. Bits 12 and up index the sample within the block, bits
    /// 4..11 are the interpolation phase.
    pub(crate) counter: u32,
    /// Byte address of the block being played.
    pub(crate) addr: u32,
    /// That block's flag byte, acted on when the block runs out.
    pub(crate) flags: u8,
    /// The block, decoded.
    pub(crate) samples: [i16; 28],
    /// The last three samples of the previous block, oldest first. The
    /// interpolator reaches back three samples, across a block boundary.
    pub(crate) history: [i16; 3],
    /// ADPCM predictor state, carried from block to block.
    pub(crate) old: i16,
    pub(crate) older: i16,
    pub(crate) phase: Phase,
    /// ENVX: the envelope level.
    pub(crate) env: i16,
    pub(crate) env_counter: u32,
    /// The current left and right volumes, after any sweep.
    pub(crate) vol: [i16; 2],
    pub(crate) vol_counter: [u32; 2],
    /// This sample after the envelope. Pitch modulation of the next voice and
    /// the voice 1 and 3 capture buffers both read it.
    pub(crate) outx: i16,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            counter: 0,
            addr: 0,
            flags: 0,
            samples: [0; 28],
            history: [0; 3],
            old: 0,
            older: 0,
            phase: Phase::Off,
            env: 0,
            env_counter: 0,
            vol: [0; 2],
            vol_counter: [0; 2],
            outx: 0,
        }
    }

    /// Sample `k` of the current block, reaching into the previous one for
    /// `k` down to -3.
    fn sample(&self, k: i32) -> i32 {
        if k >= 0 {
            self.samples[k as usize] as i32
        } else {
            self.history[(3 + k) as usize] as i32
        }
    }

    /// Four-point interpolation between the samples either side of the pitch
    /// counter. The table sums to just under unity, so this cannot overflow.
    fn interpolate(&self) -> i16 {
        let i = ((self.counter >> 4) & 0xFF) as usize;
        let n = (self.counter >> 12) as i32;
        let out = ((GAUSS[0xFF - i] as i32 * self.sample(n - 3)) >> 15)
            + ((GAUSS[0x1FF - i] as i32 * self.sample(n - 2)) >> 15)
            + ((GAUSS[0x100 + i] as i32 * self.sample(n - 1)) >> 15)
            + ((GAUSS[i] as i32 * self.sample(n)) >> 15);
        out.clamp(-0x8000, 0x7FFF) as i16
    }
}

/// How an envelope moves: the ADSR phases and the volume sweeps all share
/// this one generator, differing only in which register bits feed it.
#[derive(Clone, Copy, Debug)]
struct Rate {
    exponential: bool,
    decrease: bool,
    /// Phase-inverted: the sweep runs towards -8000h instead of +7FFFh.
    negative: bool,
    shift: u32,
    step: u32,
    /// All rate bits set. The level then never moves, and the minimum of one
    /// counter tick per sample is not applied.
    frozen: bool,
}

/// One sample of an envelope. The counter accumulates until bit 15 sets, and
/// only then does the level move.
fn envelope(level: i16, counter: &mut u32, r: Rate) -> i16 {
    let level = level as i32;
    let mut step = 7 - r.step as i32;
    if r.decrease != r.negative {
        // +7,+6,+5,+4 become -8,-7,-6,-5.
        step = !step;
    }
    step <<= 11u32.saturating_sub(r.shift);
    let mut inc = 0x8000u32 >> r.shift.saturating_sub(11);

    if r.exponential && !r.decrease && level > 0x6000 {
        // "Exponential" increase is a slower linear rate near the top.
        if r.shift < 10 {
            step >>= 2;
        } else if r.shift >= 11 {
            inc >>= 2;
        } else {
            step >>= 1;
            inc >>= 1;
        }
    } else if r.exponential && r.decrease {
        step = (step * level) >> 15;
    }
    if !r.frozen {
        inc = inc.max(1);
    }

    *counter += inc;
    if *counter & 0x8000 == 0 {
        return level as i16;
    }
    *counter = 0;

    let next = level + step;
    let next = if !r.decrease {
        next.clamp(-0x8000, 0x7FFF)
    } else if r.negative {
        next.clamp(-0x8000, 0)
    } else {
        next.clamp(0, 0x7FFF)
    };
    next as i16
}

/// A volume register: fixed when bit 15 is clear, a sweep when it is set.
fn volume(reg: u16, current: &mut i16, counter: &mut u32) {
    if reg & 0x8000 == 0 {
        // Fifteen bits, signed, standing for twice their value.
        *current = (reg << 1) as i16;
        return;
    }
    let r = Rate {
        exponential: reg & 0x4000 != 0,
        decrease: reg & 0x2000 != 0,
        negative: reg & 0x1000 != 0,
        shift: ((reg >> 2) & 0x1F) as u32,
        step: (reg & 3) as u32,
        frozen: reg & 0x7F == 0x7F,
    };
    *current = envelope(*current, counter, r);
}

fn clamp16(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

#[derive(Clone)]
pub struct Spu {
    regs: [u16; REGS],
    pub ram: Vec<u8>,
    /// Where the next transfer writes, in bytes. The register holds this in
    /// 8-byte units, which is the sort of factor that silently works when
    /// everything you test writes to address zero.
    transfer_addr: u32,

    pub(crate) voices: [Voice; VOICES],
    /// Key-on and key-off bits written since the last sample, applied at the
    /// start of the next one.
    pub(crate) pending_on: u32,
    pub(crate) pending_off: u32,
    /// ENDX: which voices have hit a loop-end flag since they were keyed on.
    pub(crate) endx: u32,
    pub(crate) noise_level: u16,
    pub(crate) noise_timer: i32,
    /// The current main volumes, after any sweep.
    pub(crate) main_vol: [i16; 2],
    pub(crate) main_counter: [u32; 2],
    /// Position in the capture buffers, in samples, 0..512.
    pub(crate) capture: u16,
    /// SPUSTAT bit 6, the interrupt flag. Set on a hit, cleared by software
    /// clearing SPUCNT bit 6.
    pub(crate) irq_flag: bool,
    /// Cycles run since the last sample, always under 768.
    pub(crate) cycle_frac: u32,

    /// The interrupt line went up and the bus has not seen it yet. Drained by
    /// the bus after every call that can set it, so never live across a save.
    irq_edge: bool,

    /// Interleaved stereo output, for the host. Never serialized.
    pub out: Vec<i16>,
    /// Bytes pushed into sound RAM, by either route. Host-side observation
    /// only, never serialized.
    pub bytes_written: u64,
    /// Key-ons applied. Host-side observation only.
    pub key_ons: u64,
}

impl Default for Spu {
    fn default() -> Spu {
        Spu::new()
    }
}

impl Spu {
    pub fn new() -> Spu {
        Spu {
            regs: [0; REGS],
            ram: vec![0; RAM_BYTES],
            transfer_addr: 0,
            voices: std::array::from_fn(|_| Voice::new()),
            pending_on: 0,
            pending_off: 0,
            endx: 0,
            noise_level: 0,
            noise_timer: 0,
            main_vol: [0; 2],
            main_counter: [0; 2],
            capture: 0,
            irq_flag: false,
            cycle_frac: 0,
            irq_edge: false,
            out: Vec::new(),
            bytes_written: 0,
            key_ons: 0,
        }
    }

    // ---- registers -------------------------------------------------------

    /// `offset` is relative to `0x1F801C00`.
    pub fn read(&self, offset: u32, width: u32) -> u32 {
        let o = (offset & !1) as usize;
        let lo = self.reg(o) as u32;
        if width == 4 {
            lo | ((self.reg(o + 2) as u32) << 16)
        } else if width == 1 {
            (lo >> (8 * (offset & 1))) & 0xFF
        } else {
            lo
        }
    }

    fn reg(&self, o: usize) -> u16 {
        match o {
            _ if o < MVOLL && o % 16 == V_ENVX * 2 => self.voices[o / 16].env as u16,
            ENDX => self.endx as u16,
            x if x == ENDX + 2 => (self.endx >> 16) as u16,
            SPUSTAT => self.status(),
            MVOLXL => self.main_vol[0] as u16,
            MVOLXR => self.main_vol[1] as u16,
            VOLX..VOLX_END => {
                let n = (o - VOLX) / 4;
                self.voices[n].vol[(o / 2) & 1] as u16
            }
            _ => self.regs.get(o / 2).copied().unwrap_or(0),
        }
    }

    /// SPUSTAT is derived, never stored. The low six bits follow SPUCNT, and
    /// software waits on them; the transfer-busy bits stay clear because
    /// transfers here complete inside the write that starts them.
    fn status(&self) -> u16 {
        let cnt = self.regs[SPUCNT / 2];
        let mut s = cnt & 0x3F;
        // "Seems to be same as SPUCNT bit 5."
        s |= (cnt & 0x20) << 2;
        if self.irq_flag {
            s |= STAT_IRQ;
        }
        if self.capture_irq_enabled() && self.capture >= 0x100 {
            s |= STAT_CAPTURE_HALF;
        }
        s
    }

    /// `offset` is relative to `0x1F801C00`.
    pub fn write(&mut self, offset: u32, width: u32, val: u32) {
        let o = (offset & !1) as usize;
        match width {
            4 => {
                self.write16(o, val as u16);
                self.write16(o + 2, (val >> 16) as u16);
            }
            // There is no byte lane on this bus. A byte write to an odd address
            // is dropped; to an even one it is a halfword write of the low half
            // of the register the CPU stored, not of the byte.
            1 if offset & 1 == 1 => {}
            _ => self.write16(o, val as u16),
        }
    }

    fn write16(&mut self, o: usize, val: u16) {
        if o / 2 >= REGS {
            return;
        }
        match o {
            _ if o < MVOLL && o % 16 == V_ENVX * 2 => {
                self.voices[o / 16].env = val as i16;
            }
            KON => {
                self.regs[o / 2] = val;
                self.pending_on |= val as u32;
            }
            x if x == KON + 2 => {
                self.regs[o / 2] = val;
                self.pending_on |= ((val & 0xFF) as u32) << 16;
            }
            KOFF => {
                self.regs[o / 2] = val;
                self.pending_off |= val as u32;
            }
            x if x == KOFF + 2 => {
                self.regs[o / 2] = val;
                self.pending_off |= ((val & 0xFF) as u32) << 16;
            }
            TRANSFER_ADDR => {
                self.regs[o / 2] = val;
                // Held in 8-byte units.
                self.transfer_addr = (val as u32) * 8;
            }
            TRANSFER_FIFO => self.push(val),
            SPUCNT => {
                self.regs[o / 2] = val;
                // Clearing the enable is also the acknowledge.
                if val & CNT_IRQ == 0 {
                    self.irq_flag = false;
                }
            }
            // Read only, all of them derived. Letting a write land in the
            // register file would make them stop following the hardware.
            SPUSTAT | ENDX | MVOLXL | MVOLXR => {}
            x if x == ENDX + 2 => {}
            VOLX..VOLX_END => {}
            _ => self.regs[o / 2] = val,
        }
    }

    fn vreg(&self, v: usize, r: usize) -> u16 {
        self.regs[v * 8 + r]
    }

    fn reg32(&self, o: usize) -> u32 {
        self.regs[o / 2] as u32 | (self.regs[o / 2 + 1] as u32) << 16
    }

    // ---- interrupt -------------------------------------------------------

    fn irq_address(&self) -> u32 {
        (self.regs[IRQA / 2] as u32 * 8) & RAM_MASK
    }

    fn irq_armed(&self) -> bool {
        let cnt = self.regs[SPUCNT / 2];
        cnt & CNT_ENABLE != 0 && cnt & CNT_IRQ != 0
    }

    /// The capture buffers only raise the interrupt, and only report which half
    /// they are in, when RAM_CTRL bit 2 or 3 is set.
    fn capture_irq_enabled(&self) -> bool {
        self.regs[RAM_CTRL / 2] & 0x0C != 0
    }

    /// Sound RAM at `addr` has just been touched. `len` bytes from it.
    fn touch(&mut self, addr: u32, len: u32) {
        let irq = self.irq_address();
        if addr <= irq && irq < addr + len {
            self.hit();
        }
    }

    fn hit(&mut self) {
        if self.irq_armed() && !self.irq_flag {
            self.irq_flag = true;
            self.irq_edge = true;
        }
    }

    /// Whether the interrupt line has gone up since the last call.
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq_edge)
    }

    // ---- transfers -------------------------------------------------------

    /// Push one halfword into sound RAM at the transfer pointer.
    fn push(&mut self, val: u16) {
        let a = self.transfer_addr & RAM_MASK;
        self.ram[a as usize] = val as u8;
        self.ram[((a + 1) & RAM_MASK) as usize] = (val >> 8) as u8;
        self.touch(a, 2);
        self.transfer_addr = self.transfer_addr.wrapping_add(2);
        self.bytes_written += 2;
    }

    /// One word from DMA channel 4, which is how a game moves samples in bulk.
    pub fn write_word(&mut self, word: u32) {
        self.push(word as u16);
        self.push((word >> 16) as u16);
    }

    /// One word back out, for a transfer the other way.
    pub fn read_word(&mut self) -> u32 {
        let mut w = 0u32;
        let a = self.transfer_addr & RAM_MASK;
        for i in 0..4 {
            w |= (self.ram[((a + i) & RAM_MASK) as usize] as u32) << (8 * i);
        }
        self.touch(a, 4);
        self.transfer_addr = self.transfer_addr.wrapping_add(4);
        w
    }

    fn write_ram16(&mut self, addr: u32, val: i16) {
        let a = addr & RAM_MASK;
        self.ram[a as usize] = val as u8;
        self.ram[((a + 1) & RAM_MASK) as usize] = (val as u16 >> 8) as u8;
    }

    // ---- time ------------------------------------------------------------

    /// Advance by `cycles` CPU cycles with no CD audio coming in.
    pub fn run(&mut self, cycles: u64) {
        self.run_with_cd(cycles, &mut || [0, 0]);
    }

    /// Advance by `cycles` CPU cycles, producing a sample for every 768 and
    /// taking one frame of CD audio from `cd` for each.
    pub fn run_with_cd(&mut self, cycles: u64, cd: &mut impl FnMut() -> [i16; 2]) {
        let total = self.cycle_frac as u64 + cycles;
        self.cycle_frac = (total % CYCLES_PER_SAMPLE) as u32;
        for _ in 0..total / CYCLES_PER_SAMPLE {
            let frame = cd();
            self.tick(frame);
        }
    }

    /// Cycles until the SPU could next raise its interrupt, which is the next
    /// sample, and only while the interrupt is armed. Otherwise nothing it does
    /// is visible between syncs and it can be left to catch up.
    pub fn cycles_to_event(&self) -> Option<u64> {
        if self.irq_armed() && !self.irq_flag {
            Some(CYCLES_PER_SAMPLE - self.cycle_frac as u64)
        } else {
            None
        }
    }

    fn tick(&mut self, cd: [i16; 2]) {
        self.apply_keys();

        let cnt = self.regs[SPUCNT / 2];
        let pmon = self.reg32(PMON);
        let non = self.reg32(NON);

        let mut left = 0i32;
        let mut right = 0i32;
        for v in 0..VOICES {
            let raw = if non & (1 << v) != 0 {
                self.noise_level as i16
            } else {
                self.voices[v].interpolate()
            };
            let voice = &mut self.voices[v];
            let s = (raw as i32 * voice.env as i32) >> 15;
            voice.outx = s as i16;
            left += (s * voice.vol[0] as i32) >> 15;
            right += (s * voice.vol[1] as i32) >> 15;

            let step = self.pitch_step(v, pmon);
            self.advance(v, step);
            self.step_envelope(v);
            let (l, r) = (self.vreg(v, V_VOLL), self.vreg(v, V_VOLL + 1));
            let voice = &mut self.voices[v];
            volume(l, &mut voice.vol[0], &mut voice.vol_counter[0]);
            volume(r, &mut voice.vol[1], &mut voice.vol_counter[1]);
        }
        self.step_noise(cnt);

        self.capture_sample(cd);

        if cnt & CNT_ENABLE == 0 || cnt & CNT_UNMUTE == 0 {
            // The enable and the mute gate the voices, and "don't care" for CD
            // audio, which passes regardless.
            left = 0;
            right = 0;
        }
        if cnt & CNT_CD != 0 {
            left += (cd[0] as i32 * self.regs[AVOLL / 2] as i16 as i32) >> 15;
            right += (cd[1] as i32 * self.regs[AVOLR / 2] as i16 as i32) >> 15;
        }

        let (ml, mr) = (self.regs[MVOLL / 2], self.regs[MVOLR / 2]);
        volume(ml, &mut self.main_vol[0], &mut self.main_counter[0]);
        volume(mr, &mut self.main_vol[1], &mut self.main_counter[1]);
        let out_l = clamp16((clamp16(left) * self.main_vol[0] as i32) >> 15);
        let out_r = clamp16((clamp16(right) * self.main_vol[1] as i32) >> 15);

        if self.out.len() >= OUT_CAP {
            self.out.drain(..OUT_CAP / 2);
        }
        self.out.push(out_l as i16);
        self.out.push(out_r as i16);
    }

    fn apply_keys(&mut self) {
        let off = std::mem::take(&mut self.pending_off);
        let on = std::mem::take(&mut self.pending_on);
        for v in 0..VOICES {
            if off & (1 << v) != 0 && self.voices[v].phase != Phase::Off {
                self.voices[v].phase = Phase::Release;
                self.voices[v].env_counter = 0;
            }
            if on & (1 << v) != 0 {
                self.key_on(v);
            }
        }
    }

    fn key_on(&mut self, v: usize) {
        let start = (self.vreg(v, V_START) as u32 * 8) & RAM_MASK;
        let voice = &mut self.voices[v];
        voice.addr = start;
        voice.counter = 0;
        voice.phase = Phase::Attack;
        voice.env = 0;
        voice.env_counter = 0;
        voice.old = 0;
        voice.older = 0;
        voice.history = [0; 3];
        self.endx &= !(1 << v);
        self.key_ons += 1;
        self.fetch_block(v);
    }

    fn pitch_step(&self, v: usize, pmon: u32) -> u32 {
        let mut step = self.vreg(v, V_PITCH) as u32;
        if v > 0 && pmon & (1 << v) != 0 {
            let factor = self.voices[v - 1].outx as i32 + 0x8000;
            // The register is taken as signed here, which is a hardware glitch
            // above 7FFFh, and the sign is then thrown away.
            let signed = step as u16 as i16 as i32;
            step = ((signed * factor) >> 15) as u32 & 0xFFFF;
        }
        step.min(0x4000)
    }

    fn advance(&mut self, v: usize, step: u32) {
        let voice = &mut self.voices[v];
        voice.counter += step;
        if voice.counter >> 12 < BLOCK_SAMPLES {
            return;
        }
        voice.counter -= BLOCK_SAMPLES << 12;
        voice.history = [voice.samples[25], voice.samples[26], voice.samples[27]];

        if voice.flags & 1 != 0 {
            // Loop end: jump to the repeat address. Without the repeat bit the
            // voice is also released and silenced on the spot.
            self.endx |= 1 << v;
            voice.addr = (self.regs[v * 8 + V_REPEAT] as u32 * 8) & RAM_MASK;
            if voice.flags & 2 == 0 {
                voice.phase = Phase::Release;
                voice.env = 0;
            }
        } else {
            voice.addr = (voice.addr + 16) & RAM_MASK;
        }
        self.fetch_block(v);
    }

    /// Read and decode the block at the voice's address.
    fn fetch_block(&mut self, v: usize) {
        let a = self.voices[v].addr;
        let byte = |i: u32| self.ram[((a + i) & RAM_MASK) as usize];
        let header = byte(0);
        let flags = byte(1);
        let mut data = [0u8; 14];
        for (i, d) in data.iter_mut().enumerate() {
            *d = byte(2 + i as u32);
        }

        let mut shift = (header & 0x0F) as i32;
        if shift > 12 {
            shift = 9;
        }
        let filter = ((header >> 4) & 7).min(4) as usize;

        let voice = &mut self.voices[v];
        let (mut old, mut older) = (voice.old as i32, voice.older as i32);
        for i in 0..28 {
            let b = data[i / 2];
            let nibble = if i & 1 == 0 { b & 0x0F } else { b >> 4 };
            let t = ((nibble as i32) << 28) >> 28;
            let s = ((t << 12) >> shift) + ((old * POS[filter] + older * NEG[filter] + 32) >> 6);
            let s = clamp16(s);
            voice.samples[i] = s as i16;
            older = old;
            old = s;
        }
        voice.old = old as i16;
        voice.older = older as i16;
        voice.flags = flags;

        if flags & 4 != 0 {
            // Loop start: remember this block as the place to come back to.
            self.regs[v * 8 + V_REPEAT] = (a / 8) as u16;
        }
        self.touch(a, 16);
    }

    fn step_envelope(&mut self, v: usize) {
        let a1 = self.vreg(v, V_ADSR1);
        let a2 = self.vreg(v, V_ADSR2);
        let voice = &mut self.voices[v];
        let rate = match voice.phase {
            Phase::Off => return,
            Phase::Attack => Rate {
                exponential: a1 & 0x8000 != 0,
                decrease: false,
                negative: false,
                shift: ((a1 >> 10) & 0x1F) as u32,
                step: ((a1 >> 8) & 3) as u32,
                frozen: (a1 >> 8) & 0x7F == 0x7F,
            },
            Phase::Decay => Rate {
                exponential: true,
                decrease: true,
                negative: false,
                shift: ((a1 >> 4) & 0x0F) as u32,
                step: 0,
                frozen: false,
            },
            Phase::Sustain => Rate {
                exponential: a2 & 0x8000 != 0,
                decrease: a2 & 0x4000 != 0,
                negative: false,
                shift: ((a2 >> 8) & 0x1F) as u32,
                step: ((a2 >> 6) & 3) as u32,
                frozen: (a2 >> 6) & 0x7F == 0x7F,
            },
            Phase::Release => Rate {
                exponential: a2 & 0x20 != 0,
                decrease: true,
                negative: false,
                shift: (a2 & 0x1F) as u32,
                step: 0,
                frozen: a2 & 0x1F == 0x1F,
            },
        };
        voice.env = envelope(voice.env, &mut voice.env_counter, rate);

        let sustain_level = (((a1 & 0x0F) as i32) + 1) * 0x800;
        match voice.phase {
            Phase::Attack if voice.env == 0x7FFF => {
                voice.phase = Phase::Decay;
                voice.env_counter = 0;
            }
            Phase::Decay if (voice.env as i32) <= sustain_level => {
                voice.phase = Phase::Sustain;
                voice.env_counter = 0;
            }
            Phase::Release if voice.env == 0 => voice.phase = Phase::Off,
            _ => {}
        }
    }

    fn step_noise(&mut self, cnt: u16) {
        let shift = ((cnt >> 10) & 0x0F) as u32;
        let step = ((cnt >> 8) & 3) as i32 + 4;
        let l = self.noise_level;
        let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
        self.noise_timer -= step;
        if self.noise_timer < 0 {
            self.noise_level = (l << 1) | parity;
            self.noise_timer += 0x20000 >> shift;
            if self.noise_timer < 0 {
                self.noise_timer += 0x20000 >> shift;
            }
        }
    }

    /// The four 1 KB capture buffers: CD left and right before volume, then
    /// voices 1 and 3 after their envelopes.
    fn capture_sample(&mut self, cd: [i16; 2]) {
        let at = self.capture as u32 * 2;
        let values = [cd[0], cd[1], self.voices[1].outx, self.voices[3].outx];
        for (i, value) in values.into_iter().enumerate() {
            let addr = 0x400 * i as u32 + at;
            self.write_ram16(addr, value);
            if self.capture_irq_enabled() {
                self.touch(addr, 2);
            }
        }
        self.capture = (self.capture + 1) & 0x1FF;
    }

    // ---- save state ------------------------------------------------------

    pub(crate) fn parts(&self) -> (&[u16; REGS], &[u8], u32) {
        (&self.regs, &self.ram, self.transfer_addr)
    }

    pub(crate) fn restore(&mut self, regs: [u16; REGS], ram: &[u8], transfer_addr: u32) {
        self.regs = regs;
        if ram.len() == RAM_BYTES {
            self.ram.copy_from_slice(ram);
        }
        self.transfer_addr = transfer_addr;
        self.irq_edge = false;
    }
}

/// The interpolation table, from psx-spx, 512 entries. Each set of four taps
/// a quarter-table apart sums to 7F7Fh..7F81h, which is what keeps the
/// interpolator from overflowing and what the test below checks.
#[rustfmt::skip]
const GAUSS: [i16; 512] = [
    -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1,
    0, 0, 0, 0, 0, 0, 0, 1,
    1, 1, 1, 2, 2, 2, 3, 3,
    3, 4, 4, 5, 5, 6, 7, 7,
    8, 9, 9, 10, 11, 12, 13, 14,
    15, 16, 17, 18, 19, 21, 22, 24,
    25, 27, 28, 30, 32, 33, 35, 37,
    39, 41, 44, 46, 48, 51, 53, 56,
    58, 61, 64, 67, 70, 73, 77, 80,
    84, 87, 91, 95, 99, 103, 107, 111,
    116, 120, 125, 130, 135, 140, 145, 150,
    156, 161, 167, 173, 179, 186, 192, 199,
    205, 212, 219, 227, 234, 242, 250, 257,
    266, 274, 283, 291, 300, 309, 319, 328,
    338, 348, 358, 369, 379, 390, 401, 412,
    424, 436, 448, 460, 473, 485, 498, 512,
    525, 539, 553, 567, 582, 597, 612, 627,
    643, 659, 675, 692, 708, 726, 743, 761,
    779, 797, 816, 835, 854, 874, 894, 914,
    935, 956, 977, 999, 1020, 1043, 1066, 1089,
    1112, 1136, 1160, 1184, 1209, 1234, 1260, 1286,
    1312, 1339, 1366, 1394, 1422, 1450, 1479, 1508,
    1537, 1567, 1598, 1628, 1660, 1691, 1723, 1756,
    1789, 1822, 1856, 1890, 1924, 1959, 1995, 2031,
    2067, 2104, 2141, 2179, 2217, 2256, 2295, 2334,
    2374, 2415, 2456, 2497, 2539, 2582, 2624, 2668,
    2712, 2756, 2801, 2846, 2892, 2938, 2985, 3032,
    3079, 3128, 3176, 3225, 3275, 3325, 3376, 3427,
    3479, 3531, 3584, 3637, 3691, 3745, 3799, 3855,
    3910, 3967, 4023, 4081, 4138, 4197, 4255, 4315,
    4374, 4435, 4495, 4557, 4619, 4681, 4744, 4807,
    4871, 4935, 5000, 5065, 5131, 5197, 5264, 5332,
    5399, 5468, 5536, 5606, 5676, 5746, 5817, 5888,
    5959, 6032, 6104, 6177, 6251, 6325, 6400, 6475,
    6550, 6626, 6702, 6779, 6856, 6934, 7012, 7091,
    7170, 7249, 7329, 7409, 7490, 7571, 7653, 7735,
    7817, 7900, 7983, 8066, 8150, 8234, 8319, 8404,
    8489, 8575, 8661, 8748, 8834, 8922, 9009, 9097,
    9185, 9273, 9362, 9451, 9541, 9630, 9720, 9811,
    9901, 9992, 10083, 10174, 10266, 10358, 10450, 10542,
    10635, 10727, 10820, 10913, 11007, 11100, 11194, 11288,
    11382, 11476, 11571, 11665, 11760, 11855, 11950, 12045,
    12140, 12236, 12331, 12427, 12522, 12618, 12714, 12809,
    12905, 13001, 13097, 13193, 13289, 13385, 13481, 13577,
    13673, 13769, 13865, 13961, 14056, 14152, 14248, 14343,
    14439, 14534, 14630, 14725, 14820, 14915, 15010, 15104,
    15199, 15293, 15387, 15481, 15575, 15669, 15762, 15855,
    15948, 16041, 16133, 16226, 16317, 16409, 16500, 16592,
    16682, 16773, 16863, 16953, 17042, 17131, 17220, 17308,
    17396, 17484, 17571, 17658, 17744, 17830, 17916, 18001,
    18086, 18170, 18254, 18337, 18420, 18502, 18584, 18665,
    18746, 18826, 18905, 18985, 19063, 19141, 19219, 19295,
    19372, 19447, 19522, 19597, 19671, 19744, 19816, 19888,
    19959, 20030, 20100, 20169, 20238, 20306, 20373, 20439,
    20505, 20570, 20634, 20698, 20760, 20822, 20884, 20944,
    21004, 21063, 21121, 21178, 21235, 21290, 21345, 21399,
    21452, 21505, 21556, 21607, 21657, 21706, 21754, 21801,
    21848, 21893, 21938, 21982, 22025, 22066, 22107, 22148,
    22187, 22225, 22262, 22299, 22334, 22369, 22402, 22435,
    22467, 22498, 22527, 22556, 22584, 22611, 22637, 22662,
    22686, 22709, 22731, 22752, 22772, 22791, 22809, 22826,
    22842, 22857, 22872, 22885, 22897, 22908, 22918, 22927,
    22935, 22942, 22948, 22953, 22957, 22960, 22962, 22963,
];

#[cfg(test)]
mod tests {
    use super::*;

    const V0: u32 = 0x000; // voice 0 registers
    const V1: u32 = 0x010;

    /// An SPU with the chip on and unmuted, full main volume, and voice 0
    /// pointed at a block of `nibbles` with the given header and flags at
    /// 0x1000.
    fn with_block(header: u8, flags: u8, nibbles: [u8; 28]) -> Spu {
        let mut s = Spu::new();
        s.write(0x1AA, 2, 0xC000);
        s.write(0x180, 2, 0x3FFF);
        s.write(0x182, 2, 0x3FFF);
        s.ram[0x1000] = header;
        s.ram[0x1001] = flags;
        for i in 0..14 {
            s.ram[0x1002 + i] = (nibbles[2 * i] & 0xF) | (nibbles[2 * i + 1] << 4);
        }
        s.write(V0 + 6, 2, 0x1000 / 8);
        s
    }

    fn key_on(s: &mut Spu, voice: u32) {
        s.write(0x188, 2, 1 << voice);
        s.run(CYCLES_PER_SAMPLE);
    }

    #[test]
    fn registers_read_back_what_was_written() {
        let mut s = Spu::new();
        s.write(0x1AA, 2, 0xC000); // SPUCNT: enable, unmute
        assert_eq!(s.read(0x1AA, 2), 0xC000);
        // The whole thing that unblocked Crash Bandicoot. A register that reads
        // zero forever is a hang, not a missing feature.
        s.write(0x000, 2, 0x1234); // voice 0 volume left
        assert_eq!(s.read(0x000, 2), 0x1234);
    }

    #[test]
    fn status_follows_control() {
        let mut s = Spu::new();
        s.write(0x1AA, 2, 0x801F);
        assert_eq!(s.read(0x1AE, 2), 0x1F, "the low six bits");
        // Bit 7 mirrors the DMA request bit 5.
        s.write(0x1AA, 2, 0x8020);
        assert_eq!(s.read(0x1AE, 2), 0xA0);

        // And it stays derived: a write to it must not stick.
        s.write(0x1AE, 2, 0xFFFF);
        assert_eq!(s.read(0x1AE, 2), 0xA0);
    }

    #[test]
    fn the_transfer_address_is_in_eight_byte_units() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0x0100); // 0x100 * 8 = 0x800
        s.write(0x1A8, 2, 0xBEEF);
        assert_eq!(s.ram[0x800], 0xEF);
        assert_eq!(s.ram[0x801], 0xBE);
        // Reading the register back gives the unscaled value software wrote.
        assert_eq!(s.read(0x1A6, 2), 0x0100);
    }

    #[test]
    fn the_transfer_pointer_advances() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0);
        for v in [0x1111u32, 0x2222, 0x3333] {
            s.write(0x1A8, 2, v);
        }
        assert_eq!(&s.ram[0..6], &[0x11, 0x11, 0x22, 0x22, 0x33, 0x33]);
        assert_eq!(s.bytes_written, 6);
    }

    #[test]
    fn a_dma_word_becomes_two_halfwords_in_order() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0);
        s.write_word(0xBBBB_AAAA);
        assert_eq!(&s.ram[0..4], &[0xAA, 0xAA, 0xBB, 0xBB], "low half first");
    }

    #[test]
    fn sound_ram_wraps_rather_than_panicking() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0xFFFF);
        s.write(0x1A8, 2, 0x1234);
        s.write(0x1A8, 2, 0x5678);
        assert_eq!(s.ram[RAM_BYTES - 8], 0x34);
        assert_eq!(s.bytes_written, 4);
    }

    /// psx-spx: 8-bit writes to odd addresses are ignored, and to even ones
    /// are executed as 16-bit writes of the low half of the stored register.
    #[test]
    fn a_byte_write_is_a_halfword_write_or_nothing() {
        let mut s = Spu::new();
        s.write(0x1C0, 2, 0xAABB);
        s.write(0x1C1, 1, 0x1234_56CC);
        assert_eq!(s.read(0x1C0, 2), 0xAABB, "odd byte dropped");
        s.write(0x1C0, 1, 0x1234_5678);
        assert_eq!(s.read(0x1C0, 2), 0x5678, "even byte stores the halfword");
    }

    #[test]
    fn a_word_access_covers_two_registers() {
        let mut s = Spu::new();
        s.write(0x1C0, 4, 0xDDDD_CCCC);
        assert_eq!(s.read(0x1C0, 2), 0xCCCC);
        assert_eq!(s.read(0x1C2, 2), 0xDDDD);
        assert_eq!(s.read(0x1C0, 4), 0xDDDD_CCCC);
    }

    #[test]
    fn the_interpolation_table_sums_to_just_under_unity() {
        for i in 0..256 {
            let sum = GAUSS[i] as i32
                + GAUSS[0xFF - i] as i32
                + GAUSS[0x100 + i] as i32
                + GAUSS[0x1FF - i] as i32;
            assert!((0x7F7F..=0x7F81).contains(&sum), "entry {i}: {sum:#x}");
        }
        // And literal values, so a table that merely has the right sums
        // cannot pass.
        assert_eq!(GAUSS[0], -1);
        assert_eq!(GAUSS[0x80], 0x01A8);
        assert_eq!(GAUSS[0x1FF], 0x59B3);
    }

    #[test]
    fn interpolation_weights_each_of_the_four_samples_by_its_own_tap() {
        // Four distinct samples, two in the previous block and two in this one,
        // at phase 0x40: every tap lands on a different sample, so a swapped
        // or mirrored index changes the answer.
        let mut v = Voice::new();
        v.history = [0, 1000, -3000];
        v.samples[0] = 7000;
        v.samples[1] = -11000;
        v.counter = (1 << 12) | (0x40 << 4);
        let i = 0x40;
        let expect = ((GAUSS[0xFF - i] as i32 * 1000) >> 15)
            + ((GAUSS[0x1FF - i] as i32 * -3000) >> 15)
            + ((GAUSS[0x100 + i] as i32 * 7000) >> 15)
            + ((GAUSS[i] as i32 * -11000) >> 15);
        assert_eq!(v.interpolate() as i32, expect);
        // Pinned as a literal too, worked outside Rust from taps 1756, 20944, 9901
        // and 39, so the test is not only the formula compared with itself.
        assert_eq!(expect, 236);
    }

    #[test]
    fn adpcm_filter_zero_is_the_nibble_shifted() {
        // Shift 0: a nibble lands in the top four bits.
        let mut n = [0u8; 28];
        n[0] = 0x7;
        n[1] = 0x8; // -8
        n[2] = 0xF; // -1
        let mut s = with_block(0x00, 0, n);
        key_on(&mut s, 0);
        assert_eq!(&s.voices[0].samples[..4], &[0x7000, -0x8000, -0x1000, 0]);

        // Shift 4 is sixteen times quieter, and shifts 13 to 15 act as 9.
        let mut s = with_block(0x04, 0, n);
        key_on(&mut s, 0);
        assert_eq!(s.voices[0].samples[0], 0x0700);
        let mut s = with_block(0x0D, 0, n);
        key_on(&mut s, 0);
        assert_eq!(s.voices[0].samples[0], 0x7000 >> 9);
    }

    #[test]
    fn adpcm_filter_one_predicts_from_the_last_sample() {
        // Filter 1: s = nibble + old * 60 / 64. One impulse, then zeros,
        // decays by 60/64 per sample.
        let mut n = [0u8; 28];
        n[0] = 0x4;
        let mut s = with_block(0x10, 0, n);
        key_on(&mut s, 0);
        let v = &s.voices[0];
        assert_eq!(v.samples[0], 0x4000);
        assert_eq!(v.samples[1], ((0x4000i32 * 60 + 32) >> 6) as i16);
        assert_eq!(v.samples[2], ((v.samples[1] as i32 * 60 + 32) >> 6) as i16);
    }

    #[test]
    fn adpcm_filter_two_uses_both_past_samples() {
        // Filter 2: old * 115 / 64 - older * 52 / 64.
        let mut n = [0u8; 28];
        n[0] = 0x2;
        n[1] = 0x1;
        let mut s = with_block(0x20, 0, n);
        key_on(&mut s, 0);
        let v = &s.voices[0];
        assert_eq!(v.samples[0], 0x2000);
        let s1 = 0x1000 + ((0x2000 * 115 + 32) >> 6);
        assert_eq!(v.samples[1] as i32, s1.min(0x7FFF));
        let s2 = (s1.min(0x7FFF) * 115 - 0x2000 * 52 + 32) >> 6;
        assert_eq!(v.samples[2] as i32, s2.clamp(-0x8000, 0x7FFF));
    }

    #[test]
    fn adpcm_predictor_state_carries_across_blocks() {
        let mut n = [0u8; 28];
        n[27] = 0x4;
        let mut s = with_block(0x10, 0, n);
        // Second block: filter 1, all zero nibbles. Its first sample is
        // predicted from the first block's last.
        s.ram[0x1010] = 0x10;
        s.write(V0 + 4, 2, 0x4000); // four samples a tick
        key_on(&mut s, 0);
        assert_eq!(s.voices[0].samples[27], 0x4000);
        for _ in 0..7 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_eq!(s.voices[0].addr, 0x1010, "moved on to the second block");
        assert_eq!(s.voices[0].samples[0], ((0x4000 * 60 + 32) >> 6) as i16);
        assert_eq!(s.voices[0].history, [0, 0, 0x4000], "and kept its tail");
    }

    #[test]
    fn attack_rises_to_full_then_decays_to_sustain() {
        let mut s = with_block(0, 0, [0; 28]);
        // Attack shift 0, step 0: +7 << 11 each sample, linear.
        // Decay shift 0, sustain level 7 (0x4000), and a sustain that holds.
        s.write(V0 + 8, 2, 0x0007);
        s.write(V0 + 10, 2, 0x1FC0); // sustain frozen: every rate bit set
        key_on(&mut s, 0);
        // The key-on sample itself stepped the envelope once.
        assert_eq!(s.read(V0 + 0xC, 2), 7 << 11, "ENVX reads the live level");
        assert_eq!(s.voices[0].phase, Phase::Attack);
        s.run(CYCLES_PER_SAMPLE);
        assert_eq!(s.read(V0 + 0xC, 2), 2 * (7 << 11));
        // 0x7FFF / 0x3800 is 2.3, so the third step saturates.
        s.run(CYCLES_PER_SAMPLE);
        assert_eq!(s.read(V0 + 0xC, 2), 0x7FFF);
        assert_eq!(s.voices[0].phase, Phase::Decay);
        for _ in 0..200 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_eq!(s.voices[0].phase, Phase::Sustain);
        let env = s.read(V0 + 0xC, 2) as i32;
        assert!(
            env <= 0x4000 && env > 0x3000,
            "stopped at the sustain level, got {env:#x}"
        );
    }

    #[test]
    fn exponential_decrease_is_proportional_to_the_level() {
        // Shift 0: step -8 << 11 = -0x4000, scaled by level / 0x8000.
        let r = Rate {
            exponential: true,
            decrease: true,
            negative: false,
            shift: 0,
            step: 0,
            frozen: false,
        };
        let mut c = 0;
        let a = envelope(0x7FFF, &mut c, r);
        assert_eq!(a as i32, 0x7FFF + ((-0x4000 * 0x7FFF) >> 15));
        let b = envelope(a, &mut c, r);
        assert_eq!(b as i32, a as i32 + ((-0x4000 * a as i32) >> 15));
    }

    #[test]
    fn a_slow_rate_steps_only_every_few_samples() {
        // Shift 13: the counter gains 0x8000 >> 2 per sample, so one step in
        // four, each of +7.
        let r = Rate {
            exponential: false,
            decrease: false,
            negative: false,
            shift: 13,
            step: 0,
            frozen: false,
        };
        let (mut level, mut c) = (0i16, 0u32);
        let mut seen = Vec::new();
        for _ in 0..8 {
            level = envelope(level, &mut c, r);
            seen.push(level);
        }
        assert_eq!(seen, [0, 0, 0, 7, 7, 7, 7, 14]);
    }

    #[test]
    fn exponential_increase_slows_near_the_top() {
        let r = Rate {
            exponential: true,
            decrease: false,
            negative: false,
            shift: 0,
            step: 0,
            frozen: false,
        };
        let mut c = 0;
        assert_eq!(
            envelope(0x4000, &mut c, r),
            0x4000 + (7 << 11),
            "below 0x6000, full rate"
        );
        assert_eq!(
            envelope(0x6001, &mut c, r),
            0x6001 + (7 << 9),
            "above it, a quarter"
        );
    }

    #[test]
    fn all_rate_bits_set_freezes_the_level() {
        let r = Rate {
            exponential: false,
            decrease: false,
            negative: false,
            shift: 0x1F,
            step: 3,
            frozen: true,
        };
        let mut c = 0;
        let mut level = 0x1234;
        for _ in 0..100_000 {
            level = envelope(level, &mut c, r);
        }
        assert_eq!(level, 0x1234);
    }

    #[test]
    fn a_one_shot_block_ends_the_voice_and_sets_endx() {
        // Loop end without repeat: jump, set ENDX, release at level zero.
        let mut s = with_block(0, 1, [0x7; 28]);
        s.write(V0 + 8, 2, 0x0000); // fastest attack
        s.write(V0 + 4, 2, 0x4000);
        key_on(&mut s, 0);
        assert_eq!(s.read(0x19C, 2), 0, "ENDX cleared by key-on");
        for _ in 0..7 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_eq!(s.read(0x19C, 2), 1);
        assert_eq!(s.read(V0 + 0xC, 2), 0);
        assert_eq!(s.voices[0].phase, Phase::Off);

        // And a fresh key-on is what clears it, not merely the power-on value.
        key_on(&mut s, 0);
        assert_eq!(s.read(0x19C, 2), 0, "ENDX cleared by the second key-on");
    }

    #[test]
    fn a_looping_block_keeps_playing() {
        // Loop start and loop end with repeat: the block plays forever, ENDX
        // sets on each pass, and the envelope is left alone.
        let mut s = with_block(0, 0b111, [0x7; 28]);
        s.write(V0 + 8, 2, 0x0000);
        s.write(V0 + 4, 2, 0x4000);
        key_on(&mut s, 0);
        assert_eq!(
            s.read(V0 + 0xE, 2),
            0x1000 / 8,
            "loop start sets the repeat address"
        );
        for _ in 0..70 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_eq!(s.read(0x19C, 2), 1);
        assert_eq!(s.voices[0].addr, 0x1000);
        assert_ne!(s.voices[0].phase, Phase::Off);
        assert_ne!(s.read(V0 + 0xC, 2), 0);
    }

    #[test]
    fn key_off_releases() {
        let mut s = with_block(0, 0b111, [0x7; 28]);
        s.write(V0 + 8, 2, 0x0000);
        s.write(V0 + 10, 2, 0x0000); // linear release, shift 0: -0x4000 a sample
        key_on(&mut s, 0);
        for _ in 0..10 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_ne!(s.voices[0].phase, Phase::Release);
        s.write(0x18C, 2, 1);
        for _ in 0..3 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert_eq!(s.voices[0].phase, Phase::Off);
        assert_eq!(s.read(V0 + 0xC, 2), 0);
    }

    #[test]
    fn a_keyed_voice_is_heard_and_its_volume_scales_it() {
        let mut s = with_block(0, 0b111, [0x4; 28]); // a constant 0x4000
        s.write(V0 + 8, 2, 0x0000);
        s.write(V0, 2, 0x3FFF); // left at full
        s.write(V0 + 2, 2, 0x1000); // right at a quarter
        s.write(V0 + 4, 2, 0x1000);
        key_on(&mut s, 0);
        for _ in 0..40 {
            s.run(CYCLES_PER_SAMPLE);
        }
        let n = s.out.len();
        let (l, r) = (s.out[n - 2] as i32, s.out[n - 1] as i32);
        // 0x4000 through the interpolator (255/256), full envelope, voice
        // volume 0x7FFE and main 0x7FFE: just under 0x4000.
        assert!((0x3F00..0x4000).contains(&l), "left {l:#x}");
        assert!(
            (r * 4 - l).abs() < 16,
            "right is a quarter: {r:#x} vs {l:#x}"
        );
    }

    #[test]
    fn muted_or_disabled_output_is_exactly_zero() {
        // A DC level while quiet is inaudible until the stream stops. Silence
        // must be zero, not merely small.
        let mut s = with_block(0, 0b111, [0x4; 28]);
        s.write(V0 + 8, 2, 0x0000);
        s.write(V0, 2, 0x3FFF);
        s.write(V0 + 2, 2, 0x3FFF);
        s.write(V0 + 4, 2, 0x1000);
        key_on(&mut s, 0);
        s.run(CYCLES_PER_SAMPLE * 10);
        assert!(s.out.iter().any(|&x| x != 0), "audible before muting");
        s.write(0x1AA, 2, 0x8000); // enabled, muted
        s.out.clear();
        s.run(CYCLES_PER_SAMPLE * 100);
        assert!(s.out.iter().all(|&x| x == 0));

        let mut idle = Spu::new();
        idle.write(0x1AA, 2, 0xC000);
        idle.write(0x180, 2, 0x3FFF);
        idle.write(0x182, 2, 0x3FFF);
        idle.run(CYCLES_PER_SAMPLE * 100);
        assert_eq!(idle.out.len(), 200);
        assert!(idle.out.iter().all(|&x| x == 0));
    }

    #[test]
    fn pitch_modulation_takes_its_factor_from_the_voice_before() {
        let mut s = Spu::new();
        // Voice 0 silent, so its OUTX is 0 and the factor is exactly 1.0.
        s.write(V1 + 4, 2, 0x1000);
        assert_eq!(s.pitch_step(1, 0b10), 0x1000);
        // A loud voice 0 nearly doubles it.
        s.voices[0].outx = 0x7FFF;
        assert_eq!(s.pitch_step(1, 0b10), (0x1000 * 0xFFFF) >> 15);
        // And negative full scale stops it.
        s.voices[0].outx = -0x8000;
        assert_eq!(s.pitch_step(1, 0b10), 0);
        // Without the PMON bit it is ignored.
        assert_eq!(s.pitch_step(1, 0), 0x1000);
        // Voice 0 cannot be modulated, having nothing before it.
        s.write(V0 + 4, 2, 0x1000);
        assert_eq!(s.pitch_step(0, 0b1), 0x1000);
    }

    #[test]
    fn pitch_is_clamped_to_four_times() {
        let mut s = Spu::new();
        s.write(V0 + 4, 2, 0xFFFF);
        assert_eq!(s.pitch_step(0, 0), 0x4000);
    }

    #[test]
    fn noise_shifts_in_the_parity() {
        let mut s = Spu::new();
        // Shift 15, step 4: the timer reloads by 4 and loses 4 a sample, so it
        // clocks every sample. From zero the parity is 1.
        s.step_noise(0x3C00);
        s.step_noise(0x3C00);
        s.step_noise(0x3C00);
        assert_eq!(s.noise_level, 0b111);
        // Bit 10 set flips the parity to 0.
        s.noise_level = 1 << 10;
        s.noise_timer = 0;
        s.step_noise(0x3C00);
        assert_eq!(s.noise_level, 1 << 11);
    }

    #[test]
    fn a_voice_reading_the_irq_address_raises_the_interrupt() {
        let mut s = with_block(0, 0, [0; 28]);
        s.write(0x1A4, 2, 0x1010 / 8); // the second block
        s.write(0x1AA, 2, 0xC040); // IRQ enabled
        s.write(V0 + 4, 2, 0x4000);
        key_on(&mut s, 0);
        assert!(!s.take_irq(), "the first block is not the address");
        for _ in 0..7 {
            s.run(CYCLES_PER_SAMPLE);
        }
        assert!(s.take_irq());
        assert_eq!(s.read(0x1AE, 2) & 0x40, 0x40, "SPUSTAT shows it");
        assert!(!s.take_irq(), "one edge per hit");

        // Clearing the enable is the acknowledge.
        s.write(0x1AA, 2, 0xC000);
        assert_eq!(s.read(0x1AE, 2) & 0x40, 0);
    }

    #[test]
    fn a_transfer_into_the_irq_address_raises_the_interrupt() {
        let mut s = Spu::new();
        s.write(0x1A4, 2, 0x2000 / 8);
        s.write(0x1AA, 2, 0x8040);
        s.write(0x1A6, 2, 0x1FF8 / 8);
        for _ in 0..4 {
            s.write(0x1A8, 2, 0);
        }
        assert!(!s.take_irq());
        s.write(0x1A8, 2, 0);
        assert!(s.take_irq());
    }

    #[test]
    fn capture_buffers_record_voices_one_and_three() {
        let mut s = with_block(0, 0b111, [0x4; 28]);
        s.write(V1 + 6, 2, 0x1000 / 8);
        s.write(V1 + 8, 2, 0x0000);
        s.write(V1 + 4, 2, 0x1000);
        key_on(&mut s, 1);
        for _ in 0..20 {
            s.run(CYCLES_PER_SAMPLE);
        }
        let at = (s.capture as usize - 1) * 2;
        let got = i16::from_le_bytes([s.ram[0x800 + at], s.ram[0x801 + at]]);
        assert_eq!(got, s.voices[1].outx);
        assert_ne!(got, 0);
    }

    #[test]
    fn the_capture_buffer_raises_the_interrupt_only_when_enabled() {
        let mut s = Spu::new();
        s.write(0x1A4, 2, 0x010 / 8); // CD left, sample 8
        s.write(0x1AA, 2, 0x8040);
        s.run(CYCLES_PER_SAMPLE * 20);
        assert!(!s.take_irq(), "RAM_CTRL bits 2-3 clear");
        s.write(0x1AC, 2, 0x0004);
        s.run(CYCLES_PER_SAMPLE * 512);
        assert!(s.take_irq());
    }

    #[test]
    fn sync_granularity_does_not_change_the_outcome() {
        let build = || {
            let mut s = with_block(
                0x12,
                0b111,
                [
                    3, 9, 1, 14, 7, 0, 5, 12, 2, 8, 11, 4, 6, 13, 1, 3, 15, 0, 7, 9, 2, 10, 5, 8,
                    12, 6, 4, 11,
                ],
            );
            s.write(V0 + 8, 2, 0x8F43);
            s.write(V0 + 10, 2, 0x4A85);
            s.write(V0, 2, 0x3000);
            s.write(V0 + 2, 2, 0x9045); // a sweep
            s.write(V0 + 4, 2, 0x0D31);
            s.write(0x188, 2, 1);
            s
        };
        let total = CYCLES_PER_SAMPLE * 3000 + 17;
        let mut a = build();
        a.run(total);
        let mut b = build();
        let mut left = total;
        let mut k = 1;
        while left > 0 {
            let n = k.min(left);
            b.run(n);
            left -= n;
            k = k * 7 % 1013 + 1;
        }
        assert_eq!(a.out, b.out);
        assert_eq!(a.cycle_frac, b.cycle_frac);
        assert_eq!(a.voices[0].env, b.voices[0].env);
        assert!(a.out.iter().any(|&x| x != 0), "and it made a sound");
    }
}
