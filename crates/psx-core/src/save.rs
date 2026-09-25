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
use crate::{cdrom, dma, gpu, mdec, sio, spu, Psx};

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
/// * 7: adds the SPU's register file and its 512 KB of sound RAM. No audio is
///   produced, but the registers are still machine state: software polls what
///   it wrote, and a state that loses them resumes into a spin.
/// * 10: the SPU makes sound, so it gains the state behind the registers: each
///   voice's pitch counter, block, predictor, envelope and volumes, and the
///   chip's key latches, ENDX, noise generator, main volumes, capture position,
///   interrupt flag and the cycles owed towards the next sample.
/// * 11: CD audio. The drive gains its volume matrix, both mutes, CD-DA
///   playback, the XA decoder's predictor and resampler, and the frames waiting
///   for the SPU. Those are written oldest first and padded with zeros, so the
///   bytes do not depend on where the ring happens to start.
/// * 12: the drive's lid, open or closed, for disc swapping. The shell-open
///   status bit it latches was already part of the drive status.
/// * 13: the pads became DualShocks, and each one's mode goes in: analog,
///   locked, config, the rumble mapping, and the command in progress. What
///   is held on them stays out, as before: that is the frontend's input.
/// * 14: memory cards. What each is doing mid-transfer and its flag byte go
///   in; what is on it does not, being the frontend's like the disc, so that
///   loading a state never takes back a save made since.
/// * 15: instructions cost what they cost on the console, so the CPU gains
///   what that depends on: the I-cache's tags, and the cycles at which the
///   multiplier and the GTE are done.
pub const FORMAT_VERSION: u16 = 15;

const HEADER_BYTES: usize = 8 + 2;
const CPU_BYTES: usize = 32 * 4     // regs
    + 32 * 4                        // out_regs
    + 4 * 5                         // hi, lo, pc, next_pc, current_pc
    + 1 + 4                         // pending load: register, value
    + 1 + 1                         // branch, delay_slot
    + 8                             // cycles
    + crate::timing::ICACHE_LINES * 4 // I-cache tags
    + 8 + 8; // multiplier and GTE ready
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
    + CDROM_BYTES
    + SPU_BYTES
    + MDEC_BYTES;

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
    + 1  // ack level, interrupt latch
    + 2 * PAD_BYTES
    + 2 * CARD_BYTES;

/// One memory card's own state, added in format version 14.
const CARD_BYTES: usize = 1 + 1 + 2 + 1 // flag, command, address, checksum
    + crate::memcard::SECTOR            // a write's incoming sector
    + 1; // the byte software sent last

/// One pad's own state, added in format version 13.
const PAD_BYTES: usize = 1 + 1 + 1 // analog, locked, config
    + 6                             // the rumble mapping
    + 1 + 1 + 1; // this transfer's command, its parameter, config to come

/// The SPU, added in format version 7: the register file, then sound RAM
/// behind a length prefix, then the transfer pointer. Version 10 adds the
/// voices and the chip state behind them.
const SPU_BYTES: usize = spu::REG_BYTES + 4 + spu::RAM_BYTES + 4
    + spu::VOICES * SPU_VOICE_BYTES
    + 4 + 4 + 4                   // key-on and key-off latches, ENDX
    + 2 + 4                       // noise level and timer
    + 2 * 2 + 2 * 4               // main volumes and their sweep counters
    + 2 + 1 + 4; // capture position, interrupt flag, cycles towards the next sample

const SPU_VOICE_BYTES: usize = 4 + 4 + 1     // pitch counter, address, block flags
    + 28 * 2 + 3 * 2                          // the block and the three before it
    + 2 + 2                                   // ADPCM predictor
    + 1 + 2 + 4                               // envelope phase, level, counter
    + 2 * 2 + 2 * 4                           // volumes and their sweep counters
    + 2; // OUTX

/// MDEC, added in format version 9. Everything fixed width: the tables, the
/// block being assembled, the six decoded blocks and the output buffer.
const MDEC_BYTES: usize = 9       // phase, depth, output flags, DMA enables, quant state, decoding
    + 2 * 64                      // luminance and chrominance quant tables
    + 64 * 2                      // the IDCT scale table
    + 64 * 2                      // the coefficients of the block in progress
    + 3 * 2                       // words outstanding, coefficient position, block index
    + 6 * 4                       // quant and scale load positions, output length and position, input cursor
    + 6 * 64                      // the six decoded blocks
    + 192 * 4; // and the output buffer

/// The CD-ROM controller, added in format version 6. Every array is fixed width
/// so the state stays a constant length whatever is queued.
const CDROM_BYTES: usize = 10     // index, irq enable/flags, stat, mode, three lengths, filter
    + 3                           // the Setloc target
    + 16                          // parameter FIFO
    + 16                          // response FIFO
    + 8                           // the countdown to the next response
    + 1                           // how many responses are queued
    + 2 * (2 + 16)                // and the queue itself
    + 4 + 4                       // the head position and the Setloc target
    + 1                           // whether a read is running
    + 8                           // cycles to the next sector
    + 2 + 2                       // how much of the sector software asked for
    + 2340                        // and the sector itself
    + CD_AUDIO_BYTES;

/// CD audio, added in format version 11.
const CD_AUDIO_BYTES: usize = 4 + 4   // the volume matrix, applied and pending
    + 1 + 1                       // XA mute, Mute
    + 1 + 8 + 1                   // playing, cycles to the next sector, Setloc pending
    + 2 * 2 * 2                   // XA predictor, both channels
    + 2 * 32 * 2 + 1 + 1          // the resampler ring, its position, the six-step count
    + 2 + cdrom::AUDIO_FIFO * 4  // frames queued, then the queue, oldest first
    + 1; // the lid, added in format version 12

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
    w.u32s(&cpu.icache.tags);
    w.u64(cpu.muldiv_ready);
    w.u64(cpu.gte_ready);

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

    let (rt, tr, llm, bk, lcm, fc, of, h, dqa, dqb, zsf3, zsf4, flag) = cpu.gte.control_parts();
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
    let (dot_in_line, line, clock_frac, dot_frac, dot_divider, in_vblank, frames) = b.video.parts();
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
    write_mdec(w, &b.mdec);
    write_spu(w, &b.spu);
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

fn write_spu(w: &mut Writer, s: &spu::Spu) {
    let (regs, ram, transfer) = s.parts();
    for r in regs {
        w.u16(*r);
    }
    w.u32(ram.len() as u32);
    w.bytes(ram);
    w.u32(transfer);

    for v in &s.voices {
        w.u32(v.counter);
        w.u32(v.addr);
        w.u8(v.flags);
        for x in v.samples.iter().chain(v.history.iter()) {
            w.u16(*x as u16);
        }
        w.u16(v.old as u16);
        w.u16(v.older as u16);
        w.u8(v.phase as u8);
        w.u16(v.env as u16);
        w.u32(v.env_counter);
        w.u16(v.vol[0] as u16);
        w.u16(v.vol[1] as u16);
        w.u32(v.vol_counter[0]);
        w.u32(v.vol_counter[1]);
        w.u16(v.outx as u16);
    }
    w.u32(s.pending_on);
    w.u32(s.pending_off);
    w.u32(s.endx);
    w.u16(s.noise_level);
    w.u32(s.noise_timer as u32);
    w.u16(s.main_vol[0] as u16);
    w.u16(s.main_vol[1] as u16);
    w.u32(s.main_counter[0]);
    w.u32(s.main_counter[1]);
    w.u16(s.capture);
    w.bool(s.irq_flag);
    w.u32(s.cycle_frac);
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

    let (read_lba, seek_target, reading, countdown, data_len, data_pos, sector) = c.drive_parts();
    w.u32(read_lba);
    w.u32(seek_target);
    w.u8(reading);
    w.u64(countdown);
    w.u16(data_len);
    w.u16(data_pos);
    w.bytes(sector);

    w.bytes(&c.atv);
    w.bytes(&c.atv_next);
    w.bool(c.xa_mute);
    w.bool(c.muted);
    w.bool(c.playing);
    w.u64(c.play_countdown);
    w.bool(c.setloc_pending);
    for ch in &c.xa.prev {
        w.u16(ch[0] as u16);
        w.u16(ch[1] as u16);
    }
    for ring in &c.xa.ring {
        for x in ring {
            w.u16(*x as u16);
        }
    }
    w.u8(c.xa.ring_pos);
    w.u8(c.xa.six);
    let len = c.audio_len as usize;
    w.u16(c.audio_len);
    for i in 0..cdrom::AUDIO_FIFO {
        let [l, r] = if i < len {
            c.audio[(c.audio_head as usize + i) % cdrom::AUDIO_FIFO]
        } else {
            [0, 0]
        };
        w.u16(l as u16);
        w.u16(r as u16);
    }
    w.bool(c.lid_open);
}

fn write_mdec(w: &mut Writer, m: &mdec::Mdec) {
    let (flags, quant, scale, coeffs) = m.parts();
    w.bytes(&flags);
    w.bytes(&quant);
    for v in scale {
        w.u16(v as u16);
    }
    for v in coeffs {
        w.u16(v as u16);
    }
    let (counters, positions, blocks, out) = m.progress();
    for v in counters {
        w.u16(v);
    }
    for v in positions {
        w.u32(v);
    }
    for v in blocks {
        w.u8(v as u8);
    }
    for v in out {
        w.u32(v);
    }
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
    for pad in &s.pads {
        w.bool(pad.analog);
        w.bool(pad.locked);
        w.bool(pad.config);
        w.bytes(&pad.rumble);
        w.u8(pad.command);
        w.u8(pad.param);
        w.u8(pad.config_next);
    }
    for card in &s.cards {
        w.u8(card.flag);
        w.u8(card.command);
        w.u16(card.address);
        w.u8(card.checksum);
        w.bytes(&card.buffer);
        w.u8(card.previous);
    }
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
    r.u32s(&mut cpu.icache.tags)?;
    cpu.muldiv_ready = r.u64()?;
    cpu.gte_ready = r.u64()?;

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
    read_mdec(r, &mut b.mdec)?;
    read_spu(r, &mut b.spu)?;

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

fn read_spu(r: &mut Reader, s: &mut spu::Spu) -> Option<()> {
    let mut regs = [0u16; spu::REG_BYTES / 2];
    for v in regs.iter_mut() {
        *v = r.u16()?;
    }
    // Length-prefixed and checked, like RAM and VRAM: a short buffer must be
    // refused, not padded.
    if r.u32()? as usize != spu::RAM_BYTES {
        return None;
    }
    let ram = r.take(spu::RAM_BYTES)?.to_vec();
    let transfer = r.u32()?;
    s.restore(regs, &ram, transfer);

    // Values the serializer never emits are canonicalized rather than
    // refused, as with `bool`: the restore pass must not be able to fail
    // once the length checks have passed. They are also what keeps a hostile
    // state from indexing past the end of a block.
    for v in s.voices.iter_mut() {
        v.counter = r.u32()? % (28 << 12);
        v.addr = r.u32()? & (spu::RAM_BYTES as u32 - 1);
        v.flags = r.u8()?;
        for x in v.samples.iter_mut().chain(v.history.iter_mut()) {
            *x = r.u16()? as i16;
        }
        v.old = r.u16()? as i16;
        v.older = r.u16()? as i16;
        v.phase = spu::Phase::from_u8(r.u8()?).unwrap_or(spu::Phase::Off);
        v.env = r.u16()? as i16;
        v.env_counter = r.u32()?;
        v.vol[0] = r.u16()? as i16;
        v.vol[1] = r.u16()? as i16;
        v.vol_counter[0] = r.u32()?;
        v.vol_counter[1] = r.u32()?;
        v.outx = r.u16()? as i16;
    }
    s.pending_on = r.u32()?;
    s.pending_off = r.u32()?;
    s.endx = r.u32()?;
    s.noise_level = r.u16()?;
    s.noise_timer = r.u32()? as i32;
    s.main_vol[0] = r.u16()? as i16;
    s.main_vol[1] = r.u16()? as i16;
    s.main_counter[0] = r.u32()?;
    s.main_counter[1] = r.u32()?;
    s.capture = r.u16()? & 0x1FF;
    s.irq_flag = r.bool()?;
    s.cycle_frac = r.u32()? % spu::CYCLES_PER_SAMPLE as u32;
    Some(())
}

fn read_mdec(r: &mut Reader, m: &mut mdec::Mdec) -> Option<()> {
    let mut flags = [0u8; 9];
    let mut quant = [0u8; 128];
    let mut scale = [0i16; 64];
    let mut coeffs = [0i16; 64];
    let mut counters = [0u16; 3];
    let mut positions = [0u32; 6];
    let mut blocks = [0i8; 6 * 64];
    let mut out = [0u32; 192];
    flags.copy_from_slice(r.take(9)?);
    quant.copy_from_slice(r.take(128)?);
    for v in scale.iter_mut() {
        *v = r.u16()? as i16;
    }
    for v in coeffs.iter_mut() {
        *v = r.u16()? as i16;
    }
    for v in counters.iter_mut() {
        *v = r.u16()?;
    }
    for v in positions.iter_mut() {
        *v = r.u32()?;
    }
    for v in blocks.iter_mut() {
        *v = r.u8()? as i8;
    }
    for v in out.iter_mut() {
        *v = r.u32()?;
    }
    m.restore(
        flags, quant, scale, coeffs, counters, positions, blocks, out,
    );
    Some(())
}

fn read_cdrom(r: &mut Reader, c: &mut cdrom::Cdrom) -> Option<()> {
    let mut regs = [0u8; 10];
    let mut seek_loc = [0u8; 3];
    let mut params = [0u8; 16];
    let mut response = [0u8; 16];
    let mut pending = [0u8; 2 * (2 + 16)];
    regs.copy_from_slice(r.take(10)?);
    seek_loc.copy_from_slice(r.take(3)?);
    params.copy_from_slice(r.take(16)?);
    response.copy_from_slice(r.take(16)?);
    let countdown = r.u64()?;
    let len = r.u8()?;
    pending.copy_from_slice(r.take(2 * (2 + 16))?);
    c.restore(regs, seek_loc, params, response, countdown, len, pending);

    let read_lba = r.u32()?;
    let seek_target = r.u32()?;
    let reading = r.u8()?;
    let sector_countdown = r.u64()?;
    let data_len = r.u16()?;
    let data_pos = r.u16()?;
    let mut sector = [0u8; 2340];
    sector.copy_from_slice(r.take(2340)?);
    c.restore_drive(
        read_lba,
        seek_target,
        reading,
        sector_countdown,
        data_len,
        data_pos,
        sector,
    );

    c.atv.copy_from_slice(r.take(4)?);
    c.atv_next.copy_from_slice(r.take(4)?);
    c.xa_mute = r.bool()?;
    c.muted = r.bool()?;
    c.playing = r.bool()?;
    c.play_countdown = r.u64()?;
    c.setloc_pending = r.bool()?;
    for ch in c.xa.prev.iter_mut() {
        ch[0] = r.u16()? as i16;
        ch[1] = r.u16()? as i16;
    }
    for ring in c.xa.ring.iter_mut() {
        for x in ring.iter_mut() {
            *x = r.u16()? as i16;
        }
    }
    c.xa.ring_pos = r.u8()?;
    // Canonicalized: the count runs 6 down to 1, and 0 would underflow.
    c.xa.six = match r.u8()? {
        n @ 1..=6 => n,
        _ => 6,
    };
    c.audio_len = r.u16()?.min(cdrom::AUDIO_FIFO as u16);
    c.audio_head = 0;
    for frame in c.audio.iter_mut() {
        *frame = [r.u16()? as i16, r.u16()? as i16];
    }
    c.lid_open = r.bool()?;
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
    for pad in s.pads.iter_mut() {
        pad.analog = r.bool()?;
        pad.locked = r.bool()?;
        pad.config = r.bool()?;
        pad.rumble.copy_from_slice(r.take(6)?);
        pad.command = r.u8()?;
        pad.param = r.u8()?;
        // Canonicalized: only 0, 1 and 2 mean anything.
        pad.config_next = r.u8()?.min(2);
    }
    for card in s.cards.iter_mut() {
        card.flag = r.u8()?;
        card.command = r.u8()?;
        card.address = r.u16()?;
        card.checksum = r.u8()?;
        card.buffer.copy_from_slice(r.take(crate::memcard::SECTOR)?);
        card.previous = r.u8()?;
    }
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
        for (i, t) in psx.cpu.icache.tags.iter_mut().enumerate() {
            *t = 0x8001_0000 ^ ((i as u32) << 12) | (i as u32 & 0xF);
        }
        psx.cpu.muldiv_ready = 0x1122_3344_5566_7788;
        psx.cpu.gte_ready = 0x99AA_BBCC_DDEE_FF00;

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
        psx.bus
            .store32(0x1F80_1810, 0xE400_0000 | (255 << 10) | 511);
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
        //
        // That extends to the drive itself, which is why there is a disc here.
        // With an empty tray the head never moves and the sector buffer stays
        // 2340 zeroes, so a reordering of the drive's fields would be invisible
        // no matter how carefully the registers were stirred.
        let cue = "FILE \"x.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n";
        let mut image = vec![0u8; 2352 * 40];
        for (i, b) in image.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        psx.bus.cdrom.disc = crate::disc::Disc::from_memory(cue, vec![image]).ok();
        assert!(psx.bus.cdrom.disc.is_some(), "the fixture's disc must load");

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
        psx.bus.store8(0x1F80_1801, 0x15); // SeekL: moves the head and reads a sector
        psx.bus.tick(60_000);
        psx.bus.store8(0x1F80_1803, 0x80); // hand that sector to the data FIFO
        let _ = psx.bus.load(0x1F80_1802, 1); // and take a byte, so data_pos moves

        // A second Setloc, pointing somewhere the head has not gone. Without
        // this the seek target and the head position hold the same value and
        // the golden cannot tell them apart: swapping the two in the
        // serializer left the checksum unchanged until this was added.
        for p in [0x01u8, 0x20, 0x30] {
            psx.bus.store8(0x1F80_1802, p);
        }
        psx.bus.store8(0x1F80_1801, 0x02);

        // Setfilter, with two values that differ from each other and from
        // everything else here. A filter left at its default is two more zero
        // bytes, and zero bytes cannot show that a field was dropped.
        psx.bus.store8(0x1F80_1800, 0x01);
        psx.bus.store8(0x1F80_1803, 0x07);
        psx.bus.store8(0x1F80_1800, 0x00);
        for p in [0x03u8, 0x05] {
            psx.bus.store8(0x1F80_1802, p);
        }
        psx.bus.store8(0x1F80_1801, 0x0D);
        psx.bus.tick(60_000);

        psx.bus.store8(0x1F80_1802, 0x42); // a parameter for a command not yet sent

        // The SPU: control set, and samples pushed through the transfer port so
        // sound RAM is not half a megabyte of zeroes. Same reasoning as the
        // disc above; a region that is entirely zero cannot show a reordering.
        psx.bus.store16(0x1F80_1DAA, 0xC000); // SPUCNT
        psx.bus.store16(0x1F80_1DA6, 0x0040); // transfer address, in 8-byte units
        for i in 0..8u32 {
            psx.bus.store16(0x1F80_1DA8, (0x1234 + i * 0x1111) as u16);
        }
        psx.bus.store16(0x1F80_1C00, 0x3FFF); // voice 0 volume left

        // A second block behind it, flagged loop start, end and repeat, so a
        // voice that reaches it sets ENDX and then stays there.
        for i in 0..8u32 {
            let v = if i == 0 {
                0x0721
            } else {
                0x9A5C ^ (i * 0x0F0F)
            };
            psx.bus.store16(0x1F80_1DA8, v as u16);
        }

        // And a voice playing them. Every field below would otherwise be zero,
        // and zero bytes cannot show a reordering, so each is set up to move:
        //
        // * Voice 2 runs through both blocks at nearly three times pitch, so it
        //   has history from the first, ENDX from the second, and a predictor,
        //   pitch counter and envelope all partway.
        // * Its attack and both volume sweeps use slow rates, so their counters
        //   sit partway rather than resetting to zero every sample. The two
        //   sweeps use different rates so their counters differ.
        // * Both main volumes sweep, at different rates, for the same reason.
        // * The IRQ address is the first block, so the key-on raises the flag.
        // * A key-on and a key-off are written last and left latched, because
        //   the next sample has not come due.
        psx.bus.store16(0x1F80_1DA4, 0x0040); // IRQ address: the first block
        psx.bus.store16(0x1F80_1DAA, 0xC040); // SPUCNT: on, unmuted, IRQ armed
        psx.bus.store16(0x1F80_1D80, 0x8039); // main left: sweep, 8-sample counter
        psx.bus.store16(0x1F80_1D82, 0x8035); // main right: sweep, 4-sample counter
        psx.bus.store16(0x1F80_1C20, 0x9035); // voice 2 left: sweep, 4-sample counter
        psx.bus.store16(0x1F80_1C22, 0xC03A); // voice 2 right: sweep, 8-sample counter
        psx.bus.store16(0x1F80_1C24, 0x2E71); // pitch
        psx.bus.store16(0x1F80_1C26, 0x0040); // start: the first block
        psx.bus.store16(0x1F80_1C28, 0x3A93); // ADSR1: attack shift 14, 8-sample counter
        psx.bus.store16(0x1F80_1C2A, 0x8F4B); // ADSR2
        psx.bus.store16(0x1F80_1D88, 1 << 2); // key on voice 2
        psx.bus.tick(768 * 37 + 211);
        psx.bus.sync();
        psx.bus.store16(0x1F80_1D8C, 1 << 5); // key off, still latched
        psx.bus.store16(0x1F80_1D8A, 1 << 3); // key on voice 19, still latched

        // MDEC: both quant tables, the scale table, and a decode left partway
        // through so the coefficient buffer, the block index and the output
        // buffer all hold something. A chip sitting at its power-on values
        // serializes as several hundred zero bytes, and zero bytes cannot show
        // that a field was dropped or reordered.
        psx.bus.store32(0x1F80_1824, 0x6000_0000); // both DMA requests enabled
        psx.bus.store32(0x1F80_1820, 0x4000_0001); // load quant tables, colour
        for i in 0..32u32 {
            psx.bus
                .store32(0x1F80_1820, 0x0102_0304u32.wrapping_mul(i + 1));
        }
        psx.bus.store32(0x1F80_1820, 0x6000_0000); // load the IDCT scale table
        for i in 0..32u32 {
            psx.bus
                .store32(0x1F80_1820, 0x0040_0020u32.wrapping_add(i * 0x11));
        }
        // Decode, 15-bit output, bit 15 set. Two blocks' worth of run-length
        // data and no end-of-block for the second, so it stops mid-block.
        psx.bus.store32(0x1F80_1820, 0x3200_0003);
        psx.bus.store32(0x1F80_1820, 0x0123_4567);
        psx.bus.store32(0x1F80_1820, 0xFE00_0042);
        psx.bus.store32(0x1F80_1820, 0x0089_00AB);

        // And a second decode fed the way a game feeds one, through DMA channel
        // 0, so the input cursor and the flag that says the cursor holds
        // macroblock data are both something other than zero. Feeding this
        // through the command port instead leaves both at their power-on
        // values, and two adjacent fields that are both zero cannot show that
        // the serializer swapped them.
        for (i, word) in [0x1234_5678u32, 0x0011_2233, 0x4455_6677, 0x8899_AABB]
            .iter()
            .enumerate()
        {
            psx.bus.store32(0x2000 + i as u32 * 4, *word);
        }
        psx.bus.store32(0x1F80_1820, 0x3200_0004); // decode, four words
        psx.bus.store32(0x1F80_10F0, 0x8888_8888); // enable every channel
        psx.bus.store32(0x1F80_1080, 0x0000_2000); // channel 0 MADR
        psx.bus.store32(0x1F80_1084, 0x0002_0002); // two words, two blocks
        psx.bus.store32(0x1F80_1088, 0x0100_0201); // from RAM, sync 1, start

        // CD audio, last of all, because every tick above drains the queue
        // into the SPU and a playing drive refills it. Set directly rather
        // than driven, because this test is about
        // the layout, and every field has to hold something distinct and
        // non-zero for a reordering to show. The queue is given a head part way
        // round the ring and wraps past its end, which is the case the
        // oldest-first writing exists for.
        {
            let c = &mut psx.bus.cdrom;
            c.atv = [0x80, 0x11, 0x7F, 0x22];
            c.atv_next = [0x33, 0x44, 0x55, 0x66];
            c.xa_mute = true;
            c.muted = false;
            c.playing = true;
            c.play_countdown = 123_457;
            c.setloc_pending = true;
            c.xa.prev = [[101, -202], [303, -404]];
            for (i, x) in c.xa.ring[0].iter_mut().enumerate() {
                *x = 1000 + i as i16;
            }
            for (i, x) in c.xa.ring[1].iter_mut().enumerate() {
                *x = -2000 - 3 * i as i16;
            }
            c.xa.ring_pos = 77;
            c.xa.six = 4;
            c.audio_head = (cdrom::AUDIO_FIFO - 3) as u16;
            c.audio_len = 5;
            for k in 0..5 {
                let at = (c.audio_head as usize + k) % cdrom::AUDIO_FIFO;
                c.audio[at] = [7 + k as i16, -9 - k as i16];
            }
            c.lid_open = true;
        }

        // The pads' own state, every field distinct and not its default. The
        // two pads differ, so writing one twice would show.
        {
            let p = &mut psx.bus.sio.pads[0];
            p.analog = true;
            p.config = true;
            p.rumble = [0x00, 0x01, 0xFF, 0x12, 0x34, 0x56];
            p.command = 0x4D;
            p.param = 0x07;
            p.config_next = 2;
            let q = &mut psx.bus.sio.pads[1];
            q.locked = true;
            q.rumble = [0x66, 0x55, 0x44, 0x33, 0x22, 0x11];
            q.command = 0x44;
            q.param = 0x01;
            q.config_next = 1;
        }
        // The cards: every field distinct, and the two cards different.
        for (i, card) in psx.bus.sio.cards.iter_mut().enumerate() {
            let k = i as u8;
            card.flag = 0x08 >> i;
            card.command = [b'R', b'W'][i];
            card.address = 0x0123 + 0x100 * i as u16;
            card.checksum = 0x5A ^ k;
            for (j, b) in card.buffer.iter_mut().enumerate() {
                *b = (j as u8).wrapping_mul(7) ^ k;
            }
            card.previous = 0xC0 | k;
        }

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
        assert_eq!(&snap[8..10], &[0x0F, 0x00]);

        // Total length, pinned to a literal, deliberately NOT compared against
        // `Psx::state_size()`, which would only compare the layout to itself.
        assert_eq!(snap.len(), 3_746_074);

        // Whole-buffer checksum: any added, removed, reordered or re-widened
        // field moves it.
        assert_eq!(fnv1a64(&snap), 0xF1DE_BE4C_ADFF_30D0);
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
