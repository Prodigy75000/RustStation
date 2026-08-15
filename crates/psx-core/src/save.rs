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
use crate::{cdrom, dma, gpu, sio, Psx};

/// Core magic. The trailing digit is a generation marker: it only changes if
/// the stream stops being a RustStation state at all.
pub const MAGIC: &[u8; 8] = b"RSTAPSX1";

/// Bump on **any** layout change.
///
/// History:
/// * 1: CPU, COP0, GTE register file, RAM, scratchpad, memory control.
/// * 2: adds the master clock and the timed devices (interrupt controller,
///   video timing, root counters).
/// * 3: adds the GPU (including 1 MB of VRAM, which doubles the state) and the
///   DMA controller.
/// * 4: the GTE gains real state, serialized as its logical fields rather than
///   as the register slots software sees.
/// * 5: adds SIO0, the controller port. The pads themselves are **not** in
///   here: button state is an input the host supplies each frame, and a state
///   that carried it would replay the buttons held when it was taken.
/// * 6: adds the CD-ROM controller, including its queue of scheduled responses.
///   Unlike the pads, the drive's own state *is* serialized: whether an
///   interrupt is outstanding is machine state, and a state restored without it
///   leaves software waiting for a response that will never arrive.
pub const FORMAT_VERSION: u16 = 6;

const HEADER_BYTES: usize = 8 + 2;
const CPU_BYTES: usize = 32 * 4     // regs
    + 32 * 4                        // out_regs
    + 4 * 5                         // hi, lo, pc, next_pc, current_pc
    + 1 + 4                         // pending load: register, value
    + 1 + 1                         // branch, delay_slot
    + 8; // cycles
const COP0_BYTES: usize = 11 * 4;
/// The GTE's logical state, not its 64 register slots. See `write_cpu`.
const GTE_BYTES: usize = 9 * 2      // V0..V2
    + 4                             // RGBC
    + 2                             // OTZ
    + 4 * 2                         // IR0..IR3
    + 3 * 4                         // screen XY FIFO
    + 4 * 2                         // screen Z FIFO
    + 3 * 4                         // colour FIFO
    + 4                             // RES1
    + 4 * 4                         // MAC0..MAC3
    + 4 + 4                         // LZCS, LZCR
    + 3 * 9 * 2                     // RT, LLM, LCM
    + 3 * 3 * 4                     // TR, BK, FC
    + 2 * 4                         // screen offset
    + 2 + 2 + 4                     // H, DQA, DQB
    + 2 + 2                         // ZSF3, ZSF4
    + 4; // FLAG
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
    + 8                                           // timers.sync_uses
    + GPU_BYTES
    + DMA_BYTES
    + SIO_BYTES
    + CDROM_BYTES;

/// The GP0 FIFO is serialized as a fixed-size array so the state stays a
/// constant length. The longest real command is 12 words (a Gouraud textured
/// quad), and a poly-line is trimmed to its last segment as it goes, so this
/// has headroom.
const FIFO_SLOTS: usize = 16;

/// The GPU, added in format version 3. VRAM alone is 1 MB.
const GPU_BYTES: usize = 4 + gpu::VRAM_WORDS * 2   // length-prefixed VRAM
    + 1 + FIFO_SLOTS * 4                           // FIFO depth, then the slots
    + 1                                            // port
    + 4 * 5                                        // transfer: x, y, w, h, done
    + 6 * 4                                        // drawing area and offset
    + 9 * 4                                        // draw mode, window, display
    + 4; // mask set / mask check / display disabled / irq

const DMA_BYTES: usize = dma::CHANNELS * 3 * 4 + 4 + 4;

/// SIO0, added in format version 5. The port only, not the pads.
const SIO_BYTES: usize = 2 * 3      // mode, ctrl, baud
    + 2                             // the RX latch, with its presence bit
    + 4                             // step
    + 1                             // target
    + 8 + 1                         // /ACK countdown, and whether one is armed
    + 1; // ack level, interrupt latch

/// The CD-ROM controller, added in format version 6. Every array is fixed width
/// so the state stays a constant length whatever is queued.
const CDROM_BYTES: usize = 8      // index, irq enable/flags, stat, mode, three lengths
    + 3                           // the Setloc target
    + 16                          // parameter FIFO
    + 16                          // response FIFO
    + 8                           // the countdown to the next response
    + 1                           // how many responses are queued
    + 2 * (2 + 16)                // and the queue itself
    + 1; // disc present

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

    // COP2 (GTE). Serialized as its logical fields rather than as the 64
    // register slots software sees, because several of those slots are derived
    // views: IRGB is IR1..IR3 squeezed to five bits each, so round-tripping
    // through the register interface would quietly lose precision.
    let (v, rgbc, otz, ir, sxy, sz, rgb, res1, mac, lzcs, lzcr) = cpu.gte.data_parts();
    for vec in &v {
        for c in vec {
            w.u16(*c as u16);
        }
    }
    w.bytes(&rgbc);
    w.u16(otz);
    for i in &ir {
        w.u16(*i as u16);
    }
    for (x, y) in &sxy {
        w.u16(*x as u16);
        w.u16(*y as u16);
    }
    for z in &sz {
        w.u16(*z);
    }
    for c in &rgb {
        w.bytes(c);
    }
    w.u32(res1);
    for m in &mac {
        w.u32(*m as u32);
    }
    w.u32(lzcs);
    w.u32(lzcr);

    let (rt, tr, llm, bk, lcm, fc, of, h, dqa, dqb, zsf3, zsf4, flag) =
        cpu.gte.control_parts();
    for m in [&rt, &llm, &lcm] {
        for row in m {
            for c in row {
                w.u16(*c as u16);
            }
        }
    }
    for t in [&tr, &bk, &fc] {
        for c in t {
            w.u32(*c as u32);
        }
    }
    w.u32(of[0] as u32);
    w.u32(of[1] as u32);
    w.u16(h);
    w.u16(dqa as u16);
    w.u32(dqb as u32);
    w.u16(zsf3 as u16);
    w.u16(zsf4 as u16);
    w.u32(flag);
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

    write_gpu(w, &b.gpu);
    write_dma(w, &b.dma);
    write_sio(w, &b.sio);
    write_cdrom(w, &b.cdrom);
}

fn write_gpu(w: &mut Writer, g: &gpu::Gpu) {
    w.u32(g.vram.len() as u32);
    for px in &g.vram {
        w.u16(*px);
    }

    let fifo = g.fifo();
    let depth = fifo.len().min(FIFO_SLOTS);
    w.u8(depth as u8);
    for slot in 0..FIFO_SLOTS {
        w.u32(fifo.get(slot).copied().unwrap_or(0));
    }

    let (areas, regs, (port, done, mask_set, mask_check, display_disabled)) = g.parts();
    w.u8(port as u8);
    let (tx, ty, tw, th) = g.transfer_parts();
    w.u32(tx);
    w.u32(ty);
    w.u32(tw);
    w.u32(th);
    w.u32(done);
    for a in areas {
        w.u32(a as u32);
    }
    w.u32s(&regs);
    w.bool(mask_set);
    w.bool(mask_check);
    w.bool(display_disabled);
    w.bool(g.irq_raised());
}

fn write_dma(w: &mut Writer, d: &dma::Dma) {
    for ch in &d.channels {
        w.u32(ch.madr);
        w.u32(ch.bcr);
        w.u32(ch.chcr);
    }
    w.u32(d.control);
    w.u32(d.interrupt_raw());
}

fn write_cdrom(w: &mut Writer, c: &cdrom::Cdrom) {
    let (regs, seek_loc, params, response, countdown) = c.parts();
    w.bytes(&regs);
    w.bytes(&seek_loc);
    w.bytes(&params);
    w.bytes(&response);
    w.u64(countdown);
    let (len, pending) = c.pending_parts();
    w.u8(len);
    w.bytes(&pending);
    w.bool(c.disc);
}

fn write_sio(w: &mut Writer, s: &sio::Sio) {
    let (mode, ctrl, baud, rx, step, target, ack, armed, flags) = s.parts();
    w.u16(mode);
    w.u16(ctrl);
    w.u16(baud);
    w.u16(rx);
    w.u32(step);
    w.u8(target);
    w.u64(ack);
    w.u8(armed);
    w.u8(flags);
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

    let mut v = [[0i16; 3]; 3];
    for vec in v.iter_mut() {
        for c in vec.iter_mut() {
            *c = r.u16()? as i16;
        }
    }
    let mut rgbc = [0u8; 4];
    for b in rgbc.iter_mut() {
        *b = r.u8()?;
    }
    let otz = r.u16()?;
    let mut ir = [0i16; 4];
    for i in ir.iter_mut() {
        *i = r.u16()? as i16;
    }
    let mut sxy = [(0i16, 0i16); 3];
    for s in sxy.iter_mut() {
        *s = (r.u16()? as i16, r.u16()? as i16);
    }
    let mut sz = [0u16; 4];
    for z in sz.iter_mut() {
        *z = r.u16()?;
    }
    let mut rgb = [[0u8; 4]; 3];
    for c in rgb.iter_mut() {
        for b in c.iter_mut() {
            *b = r.u8()?;
        }
    }
    let res1 = r.u32()?;
    let mut mac = [0i32; 4];
    for m in mac.iter_mut() {
        *m = r.u32()? as i32;
    }
    let lzcs = r.u32()?;
    let lzcr = r.u32()?;
    cpu.gte
        .restore_data(v, rgbc, otz, ir, sxy, sz, rgb, res1, mac, lzcs, lzcr);

    let mut mats = [[[0i16; 3]; 3]; 3];
    for m in mats.iter_mut() {
        for row in m.iter_mut() {
            for c in row.iter_mut() {
                *c = r.u16()? as i16;
            }
        }
    }
    let mut vecs = [[0i32; 3]; 3];
    for t in vecs.iter_mut() {
        for c in t.iter_mut() {
            *c = r.u32()? as i32;
        }
    }
    let of = [r.u32()? as i32, r.u32()? as i32];
    let h = r.u16()?;
    let dqa = r.u16()? as i16;
    let dqb = r.u32()? as i32;
    let zsf3 = r.u16()? as i16;
    let zsf4 = r.u16()? as i16;
    let flag = r.u32()?;
    cpu.gte.restore_control(
        mats[0], vecs[0], mats[1], vecs[1], mats[2], vecs[2], of, h, dqa, dqb, zsf3, zsf4, flag,
    );

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

    read_gpu(r, &mut b.gpu)?;
    read_dma(r, &mut b.dma)?;
    read_sio(r, &mut b.sio)?;
    read_cdrom(r, &mut b.cdrom)?;

    Some(())
}

fn read_gpu(r: &mut Reader, g: &mut gpu::Gpu) -> Option<()> {
    let _len = r.u32()?;
    for px in g.vram.iter_mut() {
        *px = r.u16()?;
    }

    let depth = (r.u8()? as usize).min(FIFO_SLOTS);
    let mut fifo = Vec::with_capacity(depth);
    for slot in 0..FIFO_SLOTS {
        let word = r.u32()?;
        if slot < depth {
            fifo.push(word);
        }
    }

    let port = r.u8()? as u32;
    let transfer = (r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    let mut areas = [0i32; 6];
    for a in areas.iter_mut() {
        *a = r.u32()? as i32;
    }
    let mut regs = [0u32; 9];
    r.u32s(&mut regs)?;
    let flags = (r.bool()?, r.bool()?, r.bool()?, r.bool()?);

    g.restore(areas, regs, port, transfer, flags, fifo);
    Some(())
}

fn read_dma(r: &mut Reader, d: &mut dma::Dma) -> Option<()> {
    for ch in d.channels.iter_mut() {
        ch.madr = r.u32()?;
        ch.bcr = r.u32()?;
        ch.chcr = r.u32()?;
    }
    d.control = r.u32()?;
    let interrupt = r.u32()?;
    d.restore_interrupt(interrupt);
    Some(())
}

fn read_cdrom(r: &mut Reader, c: &mut cdrom::Cdrom) -> Option<()> {
    let mut regs = [0u8; 8];
    let mut seek_loc = [0u8; 3];
    let mut params = [0u8; 16];
    let mut response = [0u8; 16];
    let mut pending = [0u8; 2 * (2 + 16)];
    regs.copy_from_slice(r.take(8)?);
    seek_loc.copy_from_slice(r.take(3)?);
    params.copy_from_slice(r.take(16)?);
    response.copy_from_slice(r.take(16)?);
    let countdown = r.u64()?;
    let len = r.u8()?;
    pending.copy_from_slice(r.take(2 * (2 + 16))?);
    c.disc = r.bool()?;
    c.restore(regs, seek_loc, params, response, countdown, len, pending);
    Some(())
}

fn read_sio(r: &mut Reader, s: &mut sio::Sio) -> Option<()> {
    let mode = r.u16()?;
    let ctrl = r.u16()?;
    let baud = r.u16()?;
    let rx = r.u16()?;
    let step = r.u32()?;
    let target = r.u8()?;
    let ack = r.u64()?;
    let armed = r.u8()?;
    let flags = r.u8()?;
    s.restore(mode, ctrl, baud, rx, step, target, ack, armed, flags);
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

        // Drive the GTE through its register interface, then run a command, so
        // the snapshot holds real derived state rather than a pattern.
        for i in 0..32u32 {
            psx.cpu.gte.write_data(i, 0x0BAD_0000 | i);
            psx.cpu.gte.write_control(i, 0x0C0C_0000 | i);
        }
        psx.cpu.gte.command(0x0018_0001); // RTPS

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

        // The GPU: a display mode, a drawing area, and something actually drawn
        // so VRAM is not a uniform block.
        psx.bus.store32(0x1F80_1814, 0x0800_0001); // 320x240 NTSC
        psx.bus.store32(0x1F80_1814, 0x0300_0000); // display on
        psx.bus.store32(0x1F80_1810, 0xE300_0000); // drawing area top-left
        psx.bus.store32(0x1F80_1810, 0xE400_0000 | (255 << 10) | 511);
        psx.bus.store32(0x1F80_1810, 0xE500_0000 | (3 << 11) | 7); // offset
        psx.bus.store32(0x1F80_1810, 0x3000_00FF); // gouraud triangle
        psx.bus.store32(0x1F80_1810, 0);
        psx.bus.store32(0x1F80_1810, 0x0000_FF00);
        psx.bus.store32(0x1F80_1810, 60);
        psx.bus.store32(0x1F80_1810, 0x00FF_0000);
        psx.bus.store32(0x1F80_1810, 40 << 16);

        // A completed VRAM upload, so the transfer registers hold distinct
        // non-zero values, and then a half-finished command left in the FIFO.
        // Without both, whole fields of the GPU's state are zero in the golden
        // snapshot and a reordering of them would go unnoticed.
        psx.bus.store32(0x1F80_1810, 0xA000_0000);
        psx.bus.store32(0x1F80_1810, (7 << 16) | 13); // to (13, 7)
        psx.bus.store32(0x1F80_1810, (3 << 16) | 4); // 4 x 3
        for i in 0..6u32 {
            psx.bus.store32(0x1F80_1810, 0x1111_1111 * (i + 1));
        }
        psx.bus.store32(0x1F80_1810, 0x2000_00FF); // a triangle, left unfinished
        psx.bus.store32(0x1F80_1810, 0x0010_0010);

        // The DMA controller: one channel armed but not started.
        psx.bus.store32(0x1F80_10F0, 0x0765_4321);
        psx.bus.store32(0x1F80_1080, 0x0010_0000);
        psx.bus.store32(0x1F80_1084, 0x0002_0010);
        psx.bus.store32(0x1F80_10F4, (1 << 23) | (1 << 18));

        // SIO0, left part way through a controller read with /ACK asserted and
        // the interrupt latched. Same reasoning as the GPU transfer above: a
        // port sitting at all zeroes cannot show a reordering of its fields.
        psx.bus.store16(0x1F80_1048, 0x000D); // MODE
        psx.bus.store16(0x1F80_104E, 0x0088); // BAUD
        psx.bus.store16(0x1F80_104A, 0x1003); // TXEN | select | ACK interrupt
        psx.bus.store8(0x1F80_1040, 0x01); // address the controller
        psx.bus.store8(0x1F80_1040, 0x42); // and ask it to report
        psx.bus.tick(340); // far enough in for /ACK to be low, not yet released

        // The CD-ROM, caught mid-command: an acknowledgement delivered and
        // unread, a completion still queued behind it, a seek target set and
        // parameters left in the FIFO. Same reasoning again: every field of the
        // response queue has to be non-zero for the golden to be able to see a
        // reordering of it.
        psx.bus.store8(0x1F80_1800, 0x01); // index 1
        psx.bus.store8(0x1F80_1802, 0x1F); // interrupt enable
        psx.bus.store8(0x1F80_1800, 0x00); // index 0
        for p in [0x00u8, 0x02, 0x16] {
            psx.bus.store8(0x1F80_1802, p);
        }
        psx.bus.store8(0x1F80_1801, 0x02); // Setloc
        psx.bus.tick(60_000);
        psx.bus.store8(0x1F80_1800, 0x01);
        psx.bus.store8(0x1F80_1803, 0x07); // acknowledge it
        psx.bus.store8(0x1F80_1800, 0x00);
        psx.bus.store8(0x1F80_1801, 0x0A); // Init: acknowledges now, completes later
        psx.bus.tick(60_000);
        psx.bus.store8(0x1F80_1802, 0x42); // and a parameter for a command not yet sent

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
        assert_eq!(&snap[8..10], &[0x06, 0x00]);

        // Total length, pinned to a literal, deliberately NOT compared against
        // `Psx::state_size()`, which would only compare the layout to itself.
        assert_eq!(snap.len(), 3_147_831);

        // Whole-buffer checksum: any added, removed, reordered or re-widened
        // field moves it.
        assert_eq!(fnv1a64(&snap), 0xE27A_4764_3024_CA2E);
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
        for i in 0..32 {
            assert_eq!(
                src.cpu.gte.read_data(i),
                dst.cpu.gte.read_data(i),
                "GTE data register {i}"
            );
            assert_eq!(
                src.cpu.gte.read_control(i),
                dst.cpu.gte.read_control(i),
                "GTE control register {i}"
            );
        }
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
