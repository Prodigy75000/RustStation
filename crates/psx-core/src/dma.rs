// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The DMA controller.
//!
//! Seven channels at `0x1F801080`, each with a memory address (`MADR`), a block
//! count (`BCR`) and a control word (`CHCR`), plus a global enable (`DPCR`) and
//! an interrupt register (`DICR`).
//!
//! This exists because **the GPU is nearly unreachable without it**. The
//! console's own graphics library builds an ordering table in RAM and hands the
//! whole thing to channel 2 as a linked list; almost nothing pokes GP0 word by
//! word. Channel 6 then exists purely to *build* that table, which is why the
//! two landed together.
//!
//! Every channel with a device behind it is implemented: 0 and 1 for the
//! decoder, 2 for the GPU, 3 for the drive, 4 for sound RAM, 6 for the ordering
//! table. Channel 5 is the expansion port, which has nothing behind it, so
//! transfers on it are counted rather than performed.
//!
//! Transfers here are **instantaneous**: the whole block moves in the cycle it
//! is started. Real DMA steals bus cycles from the CPU, and chopping mode exists
//! to hand some back. `docs/notes/TIMING.md` carries that as an open question.
//!
//! See `docs/notes/DMA.md`, and in particular its first section: **these
//! registers are 32 bits wide and software does not have to treat them that
//! way**, which is the trap that cost the most here.

use crate::cdrom::Cdrom;
use crate::gpu::Gpu;
use crate::irq::{self, Irq};
use crate::mdec::Mdec;
use crate::spu::Spu;

pub const CHANNELS: usize = 7;

pub const CH_MDEC_IN: usize = 0;
pub const CH_MDEC_OUT: usize = 1;
pub const CH_GPU: usize = 2;
pub const CH_CDROM: usize = 3;
pub const CH_SPU: usize = 4;
pub const CH_PIO: usize = 5;
pub const CH_OTC: usize = 6;

/// The marker that ends a linked list.
const LIST_END: u32 = 0x00FF_FFFF;

/// What each channel talks to, for the trace.
const NAMES: [&str; CHANNELS] = ["mdec", "mdec", "gpu", "cdrom", "spu", "pio", "otc"];

/// Read once: a `var` lookup per transfer would cost more than the transfer.
static MDEC_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static DMA_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[derive(Clone, Copy, Default)]
pub struct Channel {
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
}

impl Channel {
    /// Bit 24: the channel is enabled.
    fn busy(&self) -> bool {
        self.chcr & (1 << 24) != 0
    }
    /// Bit 28: the manual-mode trigger. Ignored in the other sync modes.
    fn triggered(&self) -> bool {
        self.chcr & (1 << 28) != 0
    }
    fn sync_mode(&self) -> u32 {
        (self.chcr >> 9) & 3
    }
    /// Bit 0: 0 moves toward RAM, 1 moves away from it.
    fn reads_ram(&self) -> bool {
        self.chcr & 1 != 0
    }
    /// Bit 1: the address counts down rather than up.
    fn step(&self) -> i32 {
        if self.chcr & 2 != 0 {
            -4
        } else {
            4
        }
    }
    /// Words to move, for the two block sync modes.
    fn word_count(&self) -> u32 {
        match self.sync_mode() {
            0 => {
                let n = self.bcr & 0xFFFF;
                if n == 0 {
                    0x1_0000
                } else {
                    n
                }
            }
            1 => {
                let size = self.bcr & 0xFFFF;
                let blocks = (self.bcr >> 16) & 0xFFFF;
                size * blocks.max(1)
            }
            _ => 0,
        }
    }
    /// Ready to run: enabled, and in manual mode also triggered.
    fn ready(&self) -> bool {
        if !self.busy() {
            return false;
        }
        self.sync_mode() != 0 || self.triggered()
    }
    fn finish(&mut self) {
        // Clear both the busy and trigger bits; software polls bit 24.
        self.chcr &= !((1 << 24) | (1 << 28));
    }
}

#[derive(Clone)]
pub struct Dma {
    pub channels: [Channel; CHANNELS],
    /// `DPCR`, the per-channel master enables.
    pub control: u32,
    /// `DICR`.
    interrupt: u32,
    /// Transfers requested on a channel that is decoded but does nothing.
    pub unimplemented_transfers: u64,
    /// Which channels those were, as a bit per channel. Channels 0 and 1 are
    /// MDEC's, so this is the difference between "a game wants video decoding"
    /// and "a game wants something else entirely", and a bare total cannot say
    /// which.
    pub unimplemented_channels: u8,
    /// Linked-list nodes walked on channel 2, and lists that came back on
    /// themselves. Host-side observation only, never serialized.
    pub list_nodes: u64,
    pub list_cycles: u64,
}

impl Default for Dma {
    fn default() -> Self {
        Dma::new()
    }
}

impl Dma {
    pub fn new() -> Dma {
        Dma {
            channels: [Channel::default(); CHANNELS],
            // Reset value. Software normally rewrites it, but the BIOS reads it
            // back before it does.
            control: 0x0765_4321,
            interrupt: 0,
            unimplemented_transfers: 0,
            unimplemented_channels: 0,
            list_nodes: 0,
            list_cycles: 0,
        }
    }

    /// Is this channel enabled in `DPCR`?
    fn enabled(&self, channel: usize) -> bool {
        self.control & (1 << (channel * 4 + 3)) != 0
    }

    /// `DICR` bit 31 is computed, not stored.
    fn interrupt_word(&self) -> u32 {
        let force = self.interrupt & (1 << 15) != 0;
        let master_enable = self.interrupt & (1 << 23) != 0;
        let enables = (self.interrupt >> 16) & 0x7F;
        let flags = (self.interrupt >> 24) & 0x7F;
        let master = force || (master_enable && (enables & flags) != 0);
        (self.interrupt & 0x7FFF_FFFF) | ((master as u32) << 31)
    }

    /// Which bits of a register a sub-word access touches, and how far the
    /// value has to move to line up with them.
    ///
    /// **Every register here is 32 bits wide and software does not have to
    /// treat them that way.** A program that wants one field writes the byte
    /// that holds it, and reading the whole word for a byte access hands it a
    /// different field entirely. `DICR` is where that stops being academic: its
    /// per-channel interrupt enables live in bits 16 to 22, so arming one is a
    /// single `sb` to `DICR+2`, and a controller that answers with the low byte
    /// and then stores the reply as a whole word wipes the enables and the
    /// master enable together. No DMA interrupt is delivered afterwards, which
    /// looks like a missing interrupt and is a missing byte lane.
    fn lane(offset: u32, width: u32) -> (u32, u32) {
        let shift = (offset & 3) * 8;
        let bits: u32 = match width {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => 0xFFFF_FFFF,
        };
        (bits << shift, shift)
    }

    pub fn read(&self, offset: u32, width: u32) -> u32 {
        let (mask, shift) = Self::lane(offset, width);
        (self.read_word(offset & !3) & mask) >> shift
    }

    fn read_word(&self, reg: u32) -> u32 {
        match reg {
            0x00..=0x6F => {
                let channel = (reg / 0x10) as usize;
                match reg & 0x0F {
                    0x0 => self.channels[channel].madr,
                    0x4 => self.channels[channel].bcr,
                    0x8 => self.channels[channel].chcr,
                    _ => 0,
                }
            }
            0x70..=0x73 => self.control,
            0x74..=0x77 => self.interrupt_word(),
            _ => 0,
        }
    }

    /// Returns the channel that should now run, if any.
    #[must_use]
    pub fn write(&mut self, offset: u32, width: u32, value: u32) -> Option<usize> {
        let (mask, shift) = Self::lane(offset, width);
        let reg = offset & !3;
        // A sub-word write leaves the rest of the register alone, so the bits
        // outside the lane come from what is stored. `interrupt` rather than
        // `interrupt_word` for `DICR`: bit 31 is computed and must not be fed
        // back in.
        let stored = match reg {
            0x74..=0x77 => self.interrupt,
            _ => self.read_word(reg),
        };
        let value = (stored & !mask) | ((value << shift) & mask);

        match reg {
            0x00..=0x6F => {
                let channel = (reg / 0x10) as usize;
                match reg & 0x0F {
                    0x0 => self.channels[channel].madr = value & 0x00FF_FFFF,
                    0x4 => self.channels[channel].bcr = value,
                    0x8 => {
                        // Channel 6 only supports one direction and one step,
                        // and hardware forces those bits.
                        self.channels[channel].chcr = if channel == CH_OTC {
                            (value & 0x5100_0000) | 0x0000_0002
                        } else {
                            value
                        };
                        if self.channels[channel].ready() && self.enabled(channel) {
                            return Some(channel);
                        }
                    }
                    _ => {}
                }
            }
            0x70..=0x73 => {
                self.control = value;
                // Enabling a channel that was already armed starts it.
                for channel in 0..CHANNELS {
                    if self.channels[channel].ready() && self.enabled(channel) {
                        return Some(channel);
                    }
                }
            }
            0x74..=0x77 => {
                // Bits 24..30 are write-1-to-acknowledge; the rest are stored.
                // The acknowledgement is confined to the lane written, because
                // the merge above put the *current* flags back into the bits
                // outside it and acting on those would clear interrupts nobody
                // claimed to have seen.
                let ack = ((value >> 24) & 0x7F) & ((mask >> 24) & 0x7F);
                let flags = ((self.interrupt >> 24) & 0x7F) & !ack;
                self.interrupt = (value & 0x00FF_803F) | (flags << 24);
            }
            _ => {}
        }
        None
    }

    /// Raise this channel's completion interrupt, if it is unmasked.
    fn complete(&mut self, channel: usize, irq: &mut Irq) {
        let enables = (self.interrupt >> 16) & 0x7F;
        if enables & (1 << channel) != 0 {
            self.interrupt |= 1 << (24 + channel);
            if self.interrupt & (1 << 23) != 0 {
                irq.raise(irq::DMA);
            }
        }
    }

    /// Run one channel to completion.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        dma: &mut Dma,
        ram: &mut [u8],
        gpu: &mut Gpu,
        cdrom: &mut Cdrom,
        spu: &mut Spu,
        mdec: &mut Mdec,
        irq: &mut Irq,
        channel: usize,
    ) {
        match channel {
            CH_MDEC_IN => {
                Self::run_mdec_in(dma, ram, mdec);
                dma.channels[channel].finish();
                dma.complete(channel, irq);
                // Fresh compressed data may be exactly what an output transfer
                // that starved was waiting for. See `run_mdec_out`.
                Self::resume_mdec_out(dma, ram, mdec, irq);
                return;
            }
            CH_MDEC_OUT => {
                Self::resume_mdec_out(dma, ram, mdec, irq);
                return;
            }
            CH_GPU => Self::run_gpu(dma, ram, gpu),
            CH_CDROM => Self::run_cdrom(dma, ram, cdrom),
            CH_SPU => Self::run_spu(dma, ram, spu),
            CH_OTC => Self::run_otc(dma, ram),
            _ => {
                dma.unimplemented_transfers += 1;
                dma.unimplemented_channels |= 1 << channel;
            }
        }
        dma.channels[channel].finish();
        dma.complete(channel, irq);
    }

    fn run_gpu(dma: &mut Dma, ram: &mut [u8], gpu: &mut Gpu) {
        let ch = dma.channels[CH_GPU];
        match ch.sync_mode() {
            2 => {
                // Linked list: each node is a header word carrying the next
                // address in its low 24 bits and a word count in its high 8,
                // followed by that many GP0 words.
                let mut addr = ch.madr & 0x1F_FFFC;
                // A list can come back on itself. Metal Slug X builds one on
                // the way into a stage: 318 good nodes, then three that point
                // round in a circle, most likely one primitive linked into its
                // ordering table twice. On hardware the CPU runs between list
                // entries, so a circular list only keeps the GPU busy in the
                // background until the game rebuilds it. Here the whole list
                // runs inside the store that starts it, so the same loop
                // walked to the old million-node bound: 1.6 million GP0
                // commands, 150 seconds in one frame, and on a phone a frozen
                // game the frontend could not unload.
                //
                // So stop at the first node seen twice, having run every node
                // once. One bit per word of RAM, 64 KB, cleared per walk. The
                // node bound stays as a second line of defence.
                let mut seen = vec![0u64; crate::bus::RAM_SIZE / 4 / 64];
                for _ in 0..0x10_0000 {
                    let word = (addr >> 2) as usize;
                    let (slot, bit) = (word / 64, 1u64 << (word % 64));
                    if seen[slot] & bit != 0 {
                        dma.list_cycles += 1;
                        break;
                    }
                    seen[slot] |= bit;
                    dma.list_nodes += 1;
                    let header = read_ram(ram, addr);
                    let count = header >> 24;
                    for i in 0..count {
                        let word = read_ram(ram, addr.wrapping_add(4 * (i + 1)) & 0x1F_FFFC);
                        gpu.gp0(word);
                    }
                    let next = header & 0x00FF_FFFF;
                    if next == LIST_END || next & 0x80_0000 != 0 {
                        break;
                    }
                    addr = next & 0x1F_FFFC;
                }
                dma.channels[CH_GPU].madr = LIST_END;
            }
            _ => {
                let mut addr = ch.madr & 0x1F_FFFC;
                let count = ch.word_count();
                for _ in 0..count {
                    if ch.reads_ram() {
                        gpu.gp0(read_ram(ram, addr));
                    } else {
                        let word = gpu.read();
                        write_ram(ram, addr, word);
                    }
                    addr = addr.wrapping_add_signed(ch.step()) & 0x1F_FFFC;
                }
                dma.channels[CH_GPU].madr = addr;
            }
        }
    }

    /// Channel 0: hand the decoder a cursor to the compressed block.
    ///
    /// Not copied. See `Mdec::queue_input` for why, and for what it costs.
    fn run_mdec_in(dma: &mut Dma, ram: &mut [u8], mdec: &mut Mdec) {
        let ch = dma.channels[CH_MDEC_IN];
        let addr = ch.madr & 0x1F_FFFC;
        let count = ch.word_count();
        Self::trace_channel(CH_MDEC_IN, &ch, addr);
        mdec.queue_input(addr, count);
        mdec.pump(ram);
        dma.channels[CH_MDEC_IN].madr =
            addr.wrapping_add_signed(ch.step() * count as i32) & 0x1F_FFFC;
    }

    /// Channel 1: drain decoded pixels, and **stop rather than invent them**
    /// when the decoder runs dry.
    ///
    /// This is the one channel here that can starve, and it has to be allowed
    /// to. Software feeds compressed data in small blocks and asks for a large
    /// output block spanning several of them, expecting the DMA controller to
    /// interleave the two channels as the chip raises and drops its requests.
    /// This core's DMA is instantaneous and runs one channel at a time, so
    /// instead the transfer runs as far as the decoder can feed it, records
    /// what is left in `bcr`, and stays **busy**. Delivering the rest is then
    /// `run_mdec_in`'s job, which calls back here the moment more data arrives.
    ///
    /// Running to completion regardless would write whatever the output buffer
    /// last held into the tail of the frame, which looks like a decoder bug and
    /// is not one.
    fn resume_mdec_out(dma: &mut Dma, ram: &mut [u8], mdec: &mut Mdec, irq: &mut Irq) {
        let ch = dma.channels[CH_MDEC_OUT];
        if !ch.busy() {
            return;
        }
        let mut addr = ch.madr & 0x1F_FFFC;
        let mut left = ch.word_count();
        Self::trace_channel(CH_MDEC_OUT, &ch, addr);
        while left > 0 {
            if !mdec.has_output() {
                mdec.pump(ram);
            }
            if !mdec.has_output() {
                break;
            }
            let word = mdec.read_word();
            write_ram(ram, addr, word);
            addr = addr.wrapping_add_signed(ch.step()) & 0x1F_FFFC;
            left -= 1;
        }
        dma.channels[CH_MDEC_OUT].madr = addr;
        // The remainder goes back as a plain word count: a block size with no
        // block count, which `word_count` reads as one block of that size.
        dma.channels[CH_MDEC_OUT].bcr = left;
        if left == 0 {
            dma.channels[CH_MDEC_OUT].finish();
            dma.complete(CH_MDEC_OUT, irq);
        }
    }

    /// `RSTA_DMA_TRACE=1` logs every transfer; `RSTA_MDEC_TRACE=1` logs the
    /// decoder's two channels alone.
    ///
    /// Where a transfer *goes* is worth as much as that it happened. A game
    /// that reads the disc and then hands the decoder an empty buffer has
    /// either not read what it thinks it read or written it somewhere else,
    /// and the two destinations side by side say which.
    fn trace_channel(channel: usize, ch: &Channel, addr: u32) {
        let mdec_only = *MDEC_TRACE.get_or_init(|| std::env::var("RSTA_MDEC_TRACE").is_ok());
        let all = *DMA_TRACE.get_or_init(|| std::env::var("RSTA_DMA_TRACE").is_ok());
        if !all && !(mdec_only && matches!(channel, CH_MDEC_IN | CH_MDEC_OUT)) {
            return;
        }
        eprintln!(
            "dma: {} ch{channel} sync{} {} words at {addr:06X}",
            NAMES[channel],
            ch.sync_mode(),
            ch.word_count()
        );
    }

    /// Channel 3 drains the CD-ROM's data FIFO into RAM. One direction only:
    /// the drive is a source, and a transfer the other way has nothing to
    /// write into it.
    fn run_cdrom(dma: &mut Dma, ram: &mut [u8], cdrom: &mut Cdrom) {
        let ch = dma.channels[CH_CDROM];
        let mut addr = ch.madr & 0x1F_FFFC;
        Self::trace_channel(CH_CDROM, &ch, addr);
        for _ in 0..ch.word_count() {
            let word = cdrom.read_word();
            write_ram(ram, addr, word);
            addr = addr.wrapping_add_signed(ch.step()) & 0x1F_FFFC;
        }
        dma.channels[CH_CDROM].madr = addr;
    }

    /// Channel 4 moves samples between RAM and sound RAM. Both directions
    /// exist: a game uploads samples, and reads them back to find out where the
    /// hardware got to.
    fn run_spu(dma: &mut Dma, ram: &mut [u8], spu: &mut Spu) {
        let ch = dma.channels[CH_SPU];
        let mut addr = ch.madr & 0x1F_FFFC;
        Self::trace_channel(CH_SPU, &ch, addr);
        for _ in 0..ch.word_count() {
            if ch.reads_ram() {
                spu.write_word(read_ram(ram, addr));
            } else {
                let word = spu.read_word();
                write_ram(ram, addr, word);
            }
            addr = addr.wrapping_add_signed(ch.step()) & 0x1F_FFFC;
        }
        dma.channels[CH_SPU].madr = addr;
    }

    /// Channel 6 builds a reverse ordering table: a run of words each pointing
    /// at the one before it, ending with the list terminator. It is the only
    /// channel that writes RAM from nothing.
    fn run_otc(dma: &mut Dma, ram: &mut [u8]) {
        let ch = dma.channels[CH_OTC];
        let count = ch.word_count();
        let mut addr = ch.madr & 0x1F_FFFC;

        for i in 0..count {
            let next = if i == count - 1 {
                LIST_END
            } else {
                addr.wrapping_sub(4) & 0x1F_FFFF
            };
            write_ram(ram, addr, next);
            addr = addr.wrapping_sub(4) & 0x1F_FFFC;
        }
        dma.channels[CH_OTC].madr = addr;
    }

    pub(crate) fn interrupt_raw(&self) -> u32 {
        self.interrupt
    }
    pub(crate) fn restore_interrupt(&mut self, v: u32) {
        self.interrupt = v;
    }
}

#[inline]
fn read_ram(ram: &[u8], addr: u32) -> u32 {
    let a = (addr as usize) & (ram.len() - 1);
    u32::from_le_bytes([ram[a], ram[a + 1], ram[a + 2], ram[a + 3]])
}

#[inline]
fn write_ram(ram: &mut [u8], addr: u32, value: u32) {
    let a = (addr as usize) & (ram.len() - 1);
    ram[a..a + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> (Dma, Vec<u8>, Gpu, Irq) {
        let mut dma = Dma::new();
        dma.control = 0x0888_8888; // every channel enabled
        (dma, vec![0; 2 * 1024 * 1024], Gpu::new(), Irq::new())
    }

    #[test]
    fn otc_builds_a_reverse_ordering_table() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();

        // Four entries ending at 0x1000, counting downward.
        assert!(dma.write(0x60, 4, 0x1000).is_none());
        assert!(dma.write(0x64, 4, 4).is_none());
        let ch = dma.write(0x68, 4, 0x1100_0002);
        assert_eq!(ch, Some(CH_OTC));
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_OTC,
        );

        assert_eq!(
            read_ram(&ram, 0x1000),
            0x0FFC,
            "should point at the previous"
        );
        assert_eq!(read_ram(&ram, 0x0FFC), 0x0FF8);
        assert_eq!(read_ram(&ram, 0x0FF8), 0x0FF4);
        assert_eq!(read_ram(&ram, 0x0FF4), LIST_END, "the list must terminate");
        assert!(
            !dma.channels[CH_OTC].busy(),
            "the channel should have cleared"
        );
    }

    #[test]
    fn gpu_block_transfer_feeds_gp0() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();

        // A fill command: three words at 0x100.
        write_ram(&mut ram, 0x100, 0x0200_00FF);
        write_ram(&mut ram, 0x104, 0);
        write_ram(&mut ram, 0x108, (16 << 16) | 32);

        assert!(dma.write(0x20, 4, 0x100).is_none());
        assert!(dma.write(0x24, 4, 3).is_none()); // three words, manual mode
        let ch = dma.write(0x28, 4, 0x0100_0201); // enable + trigger, from RAM
        assert_eq!(ch, Some(CH_GPU));
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_GPU,
        );

        assert_ne!(gpu.vram[0], 0, "the fill did not reach the GPU");
    }

    #[test]
    fn gpu_linked_list_walks_the_chain() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();

        // Node A at 0x200: one word, pointing at node B at 0x300.
        write_ram(&mut ram, 0x200, (1 << 24) | 0x300);
        write_ram(&mut ram, 0x204, 0xE300_0000); // drawing area top-left
                                                 // Node B: three words (a fill), then the terminator.
        write_ram(&mut ram, 0x300, (3 << 24) | LIST_END);
        write_ram(&mut ram, 0x304, 0x0200_00FF);
        write_ram(&mut ram, 0x308, 0);
        write_ram(&mut ram, 0x30C, (16 << 16) | 32);

        assert!(dma.write(0x20, 4, 0x200).is_none());
        // Enable (bit 24), sync mode 2 (bits 9-10), from RAM (bit 0).
        let ch = dma.write(0x28, 4, 0x0100_0401);
        assert_eq!(ch, Some(CH_GPU));
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_GPU,
        );

        assert_ne!(gpu.vram[0], 0, "the list's fill did not run");
    }

    /// A circular list runs each node once and stops, the shape Metal Slug X
    /// hands the GPU. The walk used to go round to its million-node bound.
    #[test]
    fn a_circular_list_runs_each_node_once() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        // Entry node, then a loop of three: 0x500 -> 0x600 -> 0x700 -> 0x600.
        write_ram(&mut ram, 0x500, (1 << 24) | 0x600);
        write_ram(&mut ram, 0x504, 0xE300_0000);
        write_ram(&mut ram, 0x600, (3 << 24) | 0x700);
        write_ram(&mut ram, 0x604, 0x0200_00FF); // a fill
        write_ram(&mut ram, 0x608, 0);
        write_ram(&mut ram, 0x60C, (16 << 16) | 32);
        write_ram(&mut ram, 0x700, (1 << 24) | 0x600);
        write_ram(&mut ram, 0x704, 0xE300_0000);

        assert!(dma.write(0x20, 4, 0x500).is_none());
        let _ = dma.write(0x28, 4, 0x0100_0401);
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_GPU,
        );

        assert_eq!(
            dma.list_nodes, 3,
            "the entry node and the two in the loop, once each"
        );
        assert_eq!(dma.list_cycles, 1);
        assert_ne!(gpu.vram[0], 0, "and the fill inside the loop still ran");
        assert!(!dma.channels[CH_GPU].busy(), "the channel finishes");
    }

    /// A list that points at itself must not hang the emulator.
    #[test]
    fn a_self_referencing_list_terminates() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        write_ram(&mut ram, 0x400, 0x400); // zero words, points at itself

        assert!(dma.write(0x20, 4, 0x400).is_none());
        let _ = dma.write(0x28, 4, 0x0100_0401);
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_GPU,
        );
        // Reaching here at all is the assertion.
    }

    #[test]
    fn completion_raises_the_interrupt_only_when_unmasked() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        assert!(dma.write(0x60, 4, 0x1000).is_none());
        assert!(dma.write(0x64, 4, 2).is_none());

        // Masked: no interrupt.
        let _ = dma.write(0x68, 4, 0x1100_0002);
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_OTC,
        );
        assert_eq!(irq.stat(), 0);

        // Enable channel 6 and the master bit, then run again.
        assert!(dma
            .write(0x74, 4, (1 << 23) | (1 << (16 + CH_OTC)))
            .is_none());
        assert!(dma.write(0x60, 4, 0x1000).is_none());
        assert!(dma.write(0x64, 4, 2).is_none());
        let _ = dma.write(0x68, 4, 0x1100_0002);
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            CH_OTC,
        );
        assert_ne!(irq.stat() & (1 << irq::DMA), 0);
    }

    #[test]
    fn dicr_bit_31_is_computed_and_flags_acknowledge() {
        let mut dma = Dma::new();
        assert!(dma.write(0x74, 4, 1 << 15).is_none()); // force IRQ
        assert_ne!(dma.read(0x74, 4) & (1 << 31), 0);

        assert!(dma.write(0x74, 4, 0).is_none());
        assert_eq!(dma.read(0x74, 4) & (1 << 31), 0);
    }

    #[test]
    fn a_disabled_channel_does_not_start() {
        let (mut dma, _, _, _) = rig();
        dma.control = 0; // nothing enabled
        assert!(dma.write(0x60, 4, 0x1000).is_none());
        assert!(dma.write(0x64, 4, 4).is_none());
        assert_eq!(
            dma.write(0x68, 4, 0x1100_0002),
            None,
            "started while disabled"
        );
    }
    /// `DICR`'s interrupt enables are arranged so that one byte holds all seven
    /// of them plus the master enable, and software arms a channel by writing
    /// that byte alone.
    ///
    /// A controller that ignores the access width answers the byte read with
    /// the bottom of the word, which is a different field, and then stores the
    /// reply as the whole register: the enables and the master enable are wiped
    /// in the same instruction, and no DMA interrupt is ever delivered again.
    /// That is what left Tomb Raider's video player waiting for a frame the
    /// disc had already delivered.
    #[test]
    fn a_byte_write_to_dicr_arms_one_channel_and_leaves_the_rest() {
        let mut dma = Dma::new();
        let armed = (1 << 23) | (1 << (16 + CH_GPU));
        assert!(dma.write(0x74, 4, armed).is_none());

        // Read the byte holding the enables, add channel 3, write it back.
        let byte = dma.read(0x76, 1);
        assert_eq!(byte, 0x80 | (1 << CH_GPU), "the enables, not the low byte");
        assert!(dma.write(0x76, 1, byte | (1 << CH_CDROM)).is_none());

        let after = dma.read(0x74, 4);
        assert_ne!(after & (1 << 23), 0, "the master enable survived");
        assert_ne!(after & (1 << (16 + CH_GPU)), 0, "so did the other channel");
        assert_ne!(
            after & (1 << (16 + CH_CDROM)),
            0,
            "and the new one is armed"
        );
    }

    /// A byte write cannot acknowledge flags outside the byte it wrote.
    ///
    /// The bits outside the lane are filled in from the register's current
    /// contents, so a naive merge hands the acknowledge logic the live flags
    /// and clears every pending interrupt on any write at all.
    #[test]
    fn a_byte_write_does_not_acknowledge_flags_it_did_not_touch() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        dma.control = 0x0888_8888;
        assert!(dma
            .write(0x74, 4, (1 << 23) | (1 << (16 + CH_OTC)))
            .is_none());

        assert!(dma.write(0x60, 4, 0x1000).is_none());
        assert!(dma.write(0x64, 4, 2).is_none());
        let ch = dma.write(0x68, 4, 0x1100_0002);
        Dma::run(
            &mut dma,
            &mut ram,
            &mut gpu,
            &mut Cdrom::new(),
            &mut Spu::new(),
            &mut Mdec::new(),
            &mut irq,
            ch.expect("channel 6 runs"),
        );
        assert_ne!(
            dma.read(0x74, 4) & (1 << (24 + CH_OTC)),
            0,
            "the flag is set"
        );

        // Arming another channel touches bits 16..23 only.
        assert!(dma.write(0x76, 1, 0x80 | (1 << CH_GPU)).is_none());
        assert_ne!(
            dma.read(0x74, 4) & (1 << (24 + CH_OTC)),
            0,
            "and the flag is still set"
        );
    }
}
