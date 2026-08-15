// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Save states: byte-identical across every target, by construction.
//!
//! This file implements the in-house core save-state contract
//! (`TrophyHubResources/specs/play/IN_HOUSE_CORE_SAVESTATE_SPEC.md`). The rules
//! it exists to satisfy, restated so they cannot be lost:
//!
//! 1. Nothing native-layout in the stream. No `memcpy` of a struct, no
//!    `transmute`, no derived serializer. Every field is written explicitly
//!    through the byte cursor below.
//! 2. Little-endian, explicit widths. `usize` is never serialized; `bool`
//!    becomes a `u8` 0/1; no floats appear anywhere in emulator state.
//! 3. Fixed field order, no map iteration.
//! 4. A magic + `format_version` header. **Bump [`FORMAT_VERSION`] on any
//!    layout change**, because the netplay handshake trusts it.
//! 5. Loading validates: truncated, oversized, wrong-magic and newer-version
//!    inputs all return `false`, never panic and never read out of bounds.
//!
//! Two things are deliberately **excluded**:
//!
//! * The BIOS image, this console's ROM. A state is only meaningful next to
//!   the same BIOS, exactly as a cartridge state is next to the same cartridge.
//! * Host-side observation: the TTY buffer, the queued PSX-EXE, and the bus /
//!   GTE diagnostic counters. They do not influence emulated behaviour, and
//!   keeping them out means a debug session cannot change a state's bytes.

use crate::bus::{self, Bus};
use crate::cpu::Cpu;
use crate::video::Standard;
use crate::Psx;

/// Core magic. The trailing digit is a generation marker: it only changes if
/// the stream stops being a RustStation state at all.
pub const MAGIC: &[u8; 8] = b"RSTAPSX1";

/// Bump on **any** layout change.
///
/// History:
/// * 1: CPU, COP0, GTE register file, RAM, scratchpad, memory control.
/// * 2: adds the master clock and the timed devices (interrupt controller,
///   video timing, root counters).
pub const FORMAT_VERSION: u16 = 2;

const HEADER_BYTES: usize = 8 + 2;
const CPU_BYTES: usize = 32 * 4     // regs
    + 32 * 4                        // out_regs
    + 4 * 5                         // hi, lo, pc, next_pc, current_pc
    + 1 + 4                         // pending load: register, value
    + 1 + 1                         // branch, delay_slot
    + 8; // cycles
const COP0_BYTES: usize = 11 * 4;
const GTE_BYTES: usize = 64 * 4;
const BUS_BYTES: usize = 4 + bus::RAM_SIZE   // length-prefixed RAM
    + 4 + bus::SCRATCHPAD_SIZE               // length-prefixed scratchpad
    + 9 * 4                                  // mem_ctrl
    + 4 * 2                                  // ram_size, cache_ctrl
    + 8 * 3                                  // cycle, next_event, synced_to
    + TIMED_BYTES;

/// The timed devices, added in format version 2.
const TIMED_BYTES: usize = 2 + 2                  // irq: stat, mask
    + 1 + 8 + 4 + 8 + 8 + 8 + 1 + 8               // video
    + 3 * (2 + 2 + 2 + 4 + 1 + 1)                 // three root counters
    + 8; // timers.sync_uses

/// Exact serialized length for [`FORMAT_VERSION`]. Derived from the field
/// widths above rather than from `save_state().len()`, so a test that pins it
/// is comparing the serializer against an independent statement of the layout,
/// not against itself.
pub const STATE_SIZE: usize = HEADER_BYTES + CPU_BYTES + COP0_BYTES + GTE_BYTES + BUS_BYTES;

/// Offsets of the two length prefixes, derived from the same field widths.
/// `load_state` checks them *before* it writes anything, which is what lets the
/// restore itself be a single pass that cannot fail half-way and leave a torn
/// machine behind.
const RAM_LEN_OFFSET: usize = HEADER_BYTES + CPU_BYTES + COP0_BYTES + GTE_BYTES;
const SCRATCHPAD_LEN_OFFSET: usize = RAM_LEN_OFFSET + 4 + bus::RAM_SIZE;

// ---------------------------------------------------------------------------
// Byte cursor
// ---------------------------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn with_capacity(n: usize) -> Writer {
        Writer {
            buf: Vec::with_capacity(n),
        }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32s(&mut self, vs: &[u32]) {
        for v in vs {
            self.u32(*v);
        }
    }
    fn bytes(&mut self, vs: &[u8]) {
        self.buf.extend_from_slice(vs);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn bool(&mut self) -> Option<bool> {
        // Canonicalized rather than rejected: the serializer only ever emits
        // 0/1, and coercing keeps the restore pass free of any failure the
        // up-front length check has not already ruled out.
        Some(self.u8()? != 0)
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        Some(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn u32s(&mut self, out: &mut [u32]) -> Option<()> {
        for slot in out.iter_mut() {
            *slot = self.u32()?;
        }
        Some(())
    }
    fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }
}

// ---------------------------------------------------------------------------
// Serialize
// ---------------------------------------------------------------------------

fn write_cpu(w: &mut Writer, cpu: &Cpu) {
    w.u32s(&cpu.regs);
    w.u32s(&cpu.out_regs);
    w.u32(cpu.hi);
    w.u32(cpu.lo);
    w.u32(cpu.pc);
    w.u32(cpu.next_pc);
    w.u32(cpu.current_pc);
    w.u8(cpu.load.0);
    w.u32(cpu.load.1);
    w.bool(cpu.branch);
    w.bool(cpu.delay_slot);
    w.u64(cpu.cycles);

    // COP0
    w.u32(cpu.cop0.bpc);
    w.u32(cpu.cop0.bda);
    w.u32(cpu.cop0.jump_dest);
    w.u32(cpu.cop0.dcic);
    w.u32(cpu.cop0.bad_vaddr);
    w.u32(cpu.cop0.bdam);
    w.u32(cpu.cop0.bpcm);
    w.u32(cpu.cop0.sr);
    w.u32(cpu.cop0.cause);
    w.u32(cpu.cop0.epc);
    w.u32(cpu.cop0.prid);

    // COP2 (GTE) register file. Serialized even though the commands are not
    // implemented, so landing them later does not move the layout.
    w.u32s(&cpu.gte.data);
    w.u32s(&cpu.gte.control);
}

fn write_bus(w: &mut Writer, b: &Bus) {
    w.u32(b.ram.len() as u32);
    w.bytes(&b.ram);
    w.u32(b.scratchpad.len() as u32);
    w.bytes(&b.scratchpad);
    w.u32s(&b.mem_ctrl);
    w.u32(b.ram_size);
    w.u32(b.cache_ctrl);

    // The master clock and the scheduler's bookkeeping. `synced_to` has to be
    // in the stream: without it a restored machine would replay, or skip, the
    // cycles between the last device sync and the moment the state was taken.
    w.u64(b.cycle);
    w.u64(b.next_event_raw());
    w.u64(b.synced_to_raw());

    w.u16(b.irq.stat());
    w.u16(b.irq.mask());

    w.u8(match b.video.standard() {
        Standard::Ntsc => 0,
        Standard::Pal => 1,
    });
    let (dot_in_line, line, clock_frac, dot_frac, dot_divider, in_vblank, frames) =
        b.video.parts();
    w.u64(dot_in_line);
    w.u32(line);
    w.u64(clock_frac);
    w.u64(dot_frac);
    w.u64(dot_divider);
    w.bool(in_vblank);
    w.u64(frames);

    for index in 0..3 {
        let (counter, mode, target, div8, irq_fired, sync_released) = b.timers.parts(index);
        w.u16(counter);
        w.u16(mode);
        w.u16(target);
        w.u32(div8);
        w.bool(irq_fired);
        w.bool(sync_released);
    }
    w.u64(b.timers.sync_uses);
}

fn read_cpu(r: &mut Reader, cpu: &mut Cpu) -> Option<()> {
    r.u32s(&mut cpu.regs)?;
    r.u32s(&mut cpu.out_regs)?;
    cpu.hi = r.u32()?;
    cpu.lo = r.u32()?;
    cpu.pc = r.u32()?;
    cpu.next_pc = r.u32()?;
    cpu.current_pc = r.u32()?;
    // Masked, not validated: register indices are five bits by construction,
    // and a coercion keeps this pass infallible.
    let load_reg = r.u8()? & 31;
    cpu.load = (load_reg, r.u32()?);
    cpu.branch = r.bool()?;
    cpu.delay_slot = r.bool()?;
    cpu.cycles = r.u64()?;

    cpu.cop0.bpc = r.u32()?;
    cpu.cop0.bda = r.u32()?;
    cpu.cop0.jump_dest = r.u32()?;
    cpu.cop0.dcic = r.u32()?;
    cpu.cop0.bad_vaddr = r.u32()?;
    cpu.cop0.bdam = r.u32()?;
    cpu.cop0.bpcm = r.u32()?;
    cpu.cop0.sr = r.u32()?;
    cpu.cop0.cause = r.u32()?;
    cpu.cop0.epc = r.u32()?;
    cpu.cop0.prid = r.u32()?;

    r.u32s(&mut cpu.gte.data)?;
    r.u32s(&mut cpu.gte.control)?;
    Some(())
}

fn read_bus(r: &mut Reader, b: &mut Bus) -> Option<()> {
    // Both regions are fixed-size for this console, so a length that is not the
    // expected one is a malformed state, not a resize request. `load_state` has
    // already checked both prefixes before the first byte was written, so these
    // reads only consume them.
    let _ram_len = r.u32()?;
    b.ram.copy_from_slice(r.take(bus::RAM_SIZE)?);
    let _scratchpad_len = r.u32()?;
    b.scratchpad.copy_from_slice(r.take(bus::SCRATCHPAD_SIZE)?);
    r.u32s(&mut b.mem_ctrl)?;
    b.ram_size = r.u32()?;
    b.cache_ctrl = r.u32()?;

    let cycle = r.u64()?;
    let next_event = r.u64()?;
    let synced_to = r.u64()?;
    b.restore_clock(cycle, next_event, synced_to);

    let stat = r.u16()?;
    let mask = r.u16()?;
    b.irq.restore(stat, mask);

    let standard = if r.u8()? == 0 {
        Standard::Ntsc
    } else {
        Standard::Pal
    };
    let dot_in_line = r.u64()?;
    let line = r.u32()?;
    let clock_frac = r.u64()?;
    let dot_frac = r.u64()?;
    let dot_divider = r.u64()?;
    let in_vblank = r.bool()?;
    let frames = r.u64()?;
    b.video.restore(
        standard,
        dot_in_line,
        line,
        clock_frac,
        dot_frac,
        dot_divider,
        in_vblank,
        frames,
    );

    for index in 0..3 {
        let counter = r.u16()?;
        let mode = r.u16()?;
        let target = r.u16()?;
        let div8 = r.u32()?;
        let irq_fired = r.bool()?;
        let sync_released = r.bool()?;
        b.timers
            .restore(index, counter, mode, target, div8, irq_fired, sync_released);
    }
    b.timers.sync_uses = r.u64()?;

    Some(())
}

impl Psx {
    /// Serialized length. Constant for a given [`FORMAT_VERSION`], and equal on
    /// every platform.
    pub fn state_size() -> usize {
        STATE_SIZE
    }

    pub fn save_state(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(STATE_SIZE);
        w.bytes(MAGIC);
        w.u16(FORMAT_VERSION);
        write_cpu(&mut w, &self.cpu);
        write_bus(&mut w, &self.bus);
        debug_assert_eq!(w.buf.len(), STATE_SIZE, "STATE_SIZE is out of date");
        w.buf
    }

    /// Restore. Returns `false` (leaving the machine untouched) for anything
    /// that is not a state this build can read.
    ///
    /// Every rejection happens *before* the first byte is written, which is why
    /// a bad state cannot leave a half-restored machine behind. That is only
    /// sound because the format is fixed-size: once the length, the magic, the
    /// version and the two length prefixes agree, no read in the restore pass
    /// can run off the end.
    pub fn load_state(&mut self, data: &[u8]) -> bool {
        // Truncated *and* oversized in one check.
        if data.len() != STATE_SIZE {
            return false;
        }
        let mut header = Reader::new(data);
        if header.take(8) != Some(&MAGIC[..]) {
            return false;
        }
        if header.u16() != Some(FORMAT_VERSION) {
            return false;
        }
        if le32_at(data, RAM_LEN_OFFSET) as usize != bus::RAM_SIZE {
            return false;
        }
        if le32_at(data, SCRATCHPAD_LEN_OFFSET) as usize != bus::SCRATCHPAD_SIZE {
            return false;
        }

        let mut r = Reader::new(data);
        let restored = (|| -> Option<()> {
            r.take(HEADER_BYTES)?;
            read_cpu(&mut r, &mut self.cpu)?;
            read_bus(&mut r, &mut self.bus)?;
            Some(())
        })()
        .is_some()
            && r.at_end();

        debug_assert!(
            restored,
            "the up-front checks should have made a failed restore unreachable"
        );
        restored
    }
}

fn le32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a-64 over the whole buffer. Integer-only and dependency-free, so
    /// the checksum itself has no target-dependent behaviour to worry about.
    fn fnv1a64(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in data {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }

    /// A BIOS-shaped image that is not a BIOS: a deterministic byte pattern,
    /// so tests never need a copyrighted dump.
    fn fake_bios() -> Vec<u8> {
        (0..bus::BIOS_SIZE).map(|i| (i * 7 + 3) as u8).collect()
    }

    /// A machine driven into a state that touches every serialized field:
    /// registers, HI/LO, both delay slots, COP0, the GTE file, RAM, the
    /// scratchpad and the memory-control ports.
    fn stirred() -> Psx {
        let mut psx = Psx::new(fake_bios()).unwrap();

        for i in 0..32u32 {
            psx.cpu.force_reg(i, 0xA5A5_0000 | i);
        }
        psx.cpu.hi = 0x1234_5678;
        psx.cpu.lo = 0x9ABC_DEF0;
        psx.cpu.pc = 0x8000_1000;
        psx.cpu.next_pc = 0x8000_1004;
        psx.cpu.current_pc = 0x8000_0FFC;
        psx.cpu.load = (17, 0xDEAD_BEEF);
        psx.cpu.branch = true;
        psx.cpu.delay_slot = true;
        psx.cpu.cycles = 0x0102_0304_0506_0708;

        psx.cpu.cop0.sr = 0x0040_0002;
        psx.cpu.cop0.cause = 0x0000_0020;
        psx.cpu.cop0.epc = 0x8000_0F00;
        psx.cpu.cop0.bad_vaddr = 0x0000_0003;
        psx.cpu.cop0.bpc = 0x1111_1111;
        psx.cpu.cop0.bda = 0x2222_2222;
        psx.cpu.cop0.jump_dest = 0x3333_3333;
        psx.cpu.cop0.dcic = 0x4444_4444;
        psx.cpu.cop0.bdam = 0x5555_5555;
        psx.cpu.cop0.bpcm = 0x6666_6666;

        for i in 0..32 {
            psx.cpu.gte.data[i] = 0x0BAD_0000 | i as u32;
            psx.cpu.gte.control[i] = 0x0C0C_0000 | i as u32;
        }

        for i in 0..4096usize {
            psx.bus.ram[i * 373 % bus::RAM_SIZE] = (i * 11 + 5) as u8;
        }
        for i in 0..bus::SCRATCHPAD_SIZE {
            psx.bus.scratchpad[i] = (i * 3 + 1) as u8;
        }
        psx.bus.store32(0x1F80_1000, 0x1F00_0000);
        psx.bus.store32(0x1F80_1060, 0x0000_0B88);
        psx.bus.store32(0xFFFE_0130, 0x0001_E988);

        // The timed devices, put somewhere non-trivial: a part-way scanline, a
        // non-zero clock remainder, and three root counters in different modes.
        psx.bus.store32(0x1F80_1074, 0x0000_0FFF); // I_MASK
        psx.bus.store32(0x1F80_1108, 977); // timer 0 target
        psx.bus.store32(0x1F80_1104, 0x0058); // timer 0 mode: target IRQ, repeat
        psx.bus.store32(0x1F80_1114, 1 << 8); // timer 1 from HBlank
        psx.bus.store32(0x1F80_1124, 2 << 8); // timer 2 from sysclock/8
        psx.bus.tick(123_457);
        psx.bus.raise_irq(crate::irq::VBLANK);

        psx
    }

    /// The golden-bytes test. The format is target-independent by construction,
    /// so these three numbers are the same on Windows, Android and iOS. This
    /// test passing here is what makes cross-platform state transfer safe,
    /// rather than something to re-verify per platform.
    ///
    /// Sensitivity was proven, not assumed: swapping the order of two
    /// same-width fields in `write_cpu` turns the checksum red while the length
    /// stays put, which is exactly the failure a length check alone would miss.
    #[test]
    fn golden_bytes() {
        let snap = stirred().save_state();

        // Header, byte for byte.
        assert_eq!(&snap[0..8], MAGIC);
        assert_eq!(&snap[8..10], &[0x02, 0x00]);

        // Total length, pinned to a literal, deliberately NOT compared against
        // `Psx::state_size()`, which would only compare the layout to itself.
        assert_eq!(snap.len(), 2_098_947);

        // Whole-buffer checksum: any added, removed, reordered or re-widened
        // field moves it.
        assert_eq!(fnv1a64(&snap), 0xCD17_E780_A751_77E6);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let src = stirred();
        let first = src.save_state();

        let mut dst = Psx::new(fake_bios()).unwrap();
        assert!(dst.load_state(&first));
        let second = dst.save_state();

        assert_eq!(first, second);
    }

    /// Byte equality of two serializations would still hold if a whole region
    /// were dropped from both sides, so check the restored *machine* too.
    #[test]
    fn restored_machine_matches_the_original() {
        let src = stirred();
        let mut dst = Psx::new(fake_bios()).unwrap();
        assert!(dst.load_state(&src.save_state()));

        assert_eq!(src.bus.ram, dst.bus.ram);
        assert_eq!(src.bus.scratchpad, dst.bus.scratchpad);
        assert_eq!(src.bus.mem_ctrl, dst.bus.mem_ctrl);
        assert_eq!(src.cpu.regs(), dst.cpu.regs());
        assert_eq!(src.cpu.out_regs(), dst.cpu.out_regs());
        assert_eq!(src.cpu.pc, dst.cpu.pc);
        assert_eq!(src.cpu.next_pc, dst.cpu.next_pc);
        assert_eq!(src.cpu.pending_load(), dst.cpu.pending_load());
        assert_eq!(src.cpu.cop0.sr, dst.cpu.cop0.sr);
        assert_eq!(src.cpu.gte.data, dst.cpu.gte.data);
        assert_eq!(src.cpu.gte.control, dst.cpu.gte.control);
    }

    /// Two independently constructed machines, driven identically, must
    /// serialize identically, the property netplay lockstep rests on.
    #[test]
    fn cross_instance_determinism() {
        let a = stirred().save_state();
        let b = stirred().save_state();
        assert_eq!(a, b);
    }

    /// Running the BIOS-shaped image for a while from two fresh machines must
    /// also agree: catches nondeterminism that only appears once the CPU has
    /// executed something.
    #[test]
    fn execution_is_deterministic() {
        let mut a = Psx::new(fake_bios()).unwrap();
        let mut b = Psx::new(fake_bios()).unwrap();
        a.run(10_000);
        b.run(10_000);
        assert_eq!(a.save_state(), b.save_state());
    }

    #[test]
    fn rejects_truncated() {
        let snap = stirred().save_state();
        let mut psx = Psx::new(fake_bios()).unwrap();
        assert!(!psx.load_state(&snap[..snap.len() - 1]));
        assert!(!psx.load_state(&snap[..9]));
        assert!(!psx.load_state(&[]));
    }

    #[test]
    fn rejects_oversized() {
        let mut snap = stirred().save_state();
        snap.push(0);
        let mut psx = Psx::new(fake_bios()).unwrap();
        assert!(!psx.load_state(&snap));
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut snap = stirred().save_state();
        snap[0] = b'X';
        let mut psx = Psx::new(fake_bios()).unwrap();
        assert!(!psx.load_state(&snap));
    }

    #[test]
    fn rejects_newer_version() {
        let mut snap = stirred().save_state();
        snap[8] = (FORMAT_VERSION + 1) as u8;
        snap[9] = ((FORMAT_VERSION + 1) >> 8) as u8;
        let mut psx = Psx::new(fake_bios()).unwrap();
        assert!(!psx.load_state(&snap));
    }

    /// A rejected state must leave the machine exactly as it was.
    #[test]
    fn rejection_does_not_disturb_the_machine() {
        let mut psx = stirred();
        let before = psx.save_state();
        let mut bad = before.clone();
        bad[0] = b'X';
        assert!(!psx.load_state(&bad));
        assert_eq!(psx.save_state(), before);
    }

    /// `STATE_SIZE` is derived from the field widths; this proves the
    /// serializer agrees with that derivation.
    #[test]
    fn declared_size_matches_serializer() {
        assert_eq!(stirred().save_state().len(), STATE_SIZE);
    }
}
