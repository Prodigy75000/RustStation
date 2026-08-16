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
//! Channels 2 (GPU) and 6 (OTC) are implemented. The rest are decoded and
//! counted: an MDEC or CD-ROM transfer completes instantly and moves nothing,
//! which is wrong but visible, rather than hanging.
//!
//! Transfers here are **instantaneous**: the whole block moves in the cycle it
//! is started. Real DMA steals bus cycles from the CPU, and chopping mode exists
//! to hand some back. `docs/notes/TIMING.md` carries that as an open question.

use crate::cdrom::Cdrom;
use crate::spu::Spu;
use crate::gpu::Gpu;
use crate::irq::{self, Irq};

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

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00..=0x6F => {
                let channel = (offset / 0x10) as usize;
                let reg = offset & 0x0F;
                match reg {
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
    pub fn write(&mut self, offset: u32, value: u32) -> Option<usize> {
        match offset {
            0x00..=0x6F => {
                let channel = (offset / 0x10) as usize;
                match offset & 0x0F {
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
                let ack = (value >> 24) & 0x7F;
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
    pub fn run(
        dma: &mut Dma,
        ram: &mut [u8],
        gpu: &mut Gpu,
        cdrom: &mut Cdrom,
        spu: &mut Spu,
        irq: &mut Irq,
        channel: usize,
    ) {
        match channel {
            CH_GPU => Self::run_gpu(dma, ram, gpu),
            CH_CDROM => Self::run_cdrom(dma, ram, cdrom),
            CH_SPU => Self::run_spu(dma, ram, spu),
            CH_OTC => Self::run_otc(dma, ram),
            _ => {
                dma.unimplemented_transfers += 1;
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
                // A malformed list can point at itself. Bound the walk rather
                // than hanging the emulator.
                for _ in 0..0x10_0000 {
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

    /// Channel 3 drains the CD-ROM's data FIFO into RAM. One direction only:
    /// the drive is a source, and a transfer the other way has nothing to
    /// write into it.
    fn run_cdrom(dma: &mut Dma, ram: &mut [u8], cdrom: &mut Cdrom) {
        let ch = dma.channels[CH_CDROM];
        let mut addr = ch.madr & 0x1F_FFFC;
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
        assert!(dma.write(0x60, 0x1000).is_none());
        assert!(dma.write(0x64, 4).is_none());
        let ch = dma.write(0x68, 0x1100_0002);
        assert_eq!(ch, Some(CH_OTC));
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_OTC);

        assert_eq!(read_ram(&ram, 0x1000), 0x0FFC, "should point at the previous");
        assert_eq!(read_ram(&ram, 0x0FFC), 0x0FF8);
        assert_eq!(read_ram(&ram, 0x0FF8), 0x0FF4);
        assert_eq!(read_ram(&ram, 0x0FF4), LIST_END, "the list must terminate");
        assert!(!dma.channels[CH_OTC].busy(), "the channel should have cleared");
    }

    #[test]
    fn gpu_block_transfer_feeds_gp0() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();

        // A fill command: three words at 0x100.
        write_ram(&mut ram, 0x100, 0x0200_00FF);
        write_ram(&mut ram, 0x104, 0);
        write_ram(&mut ram, 0x108, (16 << 16) | 32);

        assert!(dma.write(0x20, 0x100).is_none());
        assert!(dma.write(0x24, 3).is_none()); // three words, manual mode
        let ch = dma.write(0x28, 0x0100_0201); // enable + trigger, from RAM
        assert_eq!(ch, Some(CH_GPU));
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_GPU);

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

        assert!(dma.write(0x20, 0x200).is_none());
        // Enable (bit 24), sync mode 2 (bits 9-10), from RAM (bit 0).
        let ch = dma.write(0x28, 0x0100_0401);
        assert_eq!(ch, Some(CH_GPU));
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_GPU);

        assert_ne!(gpu.vram[0], 0, "the list's fill did not run");
    }

    /// A list that points at itself must not hang the emulator.
    #[test]
    fn a_self_referencing_list_terminates() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        write_ram(&mut ram, 0x400, 0x400); // zero words, points at itself

        assert!(dma.write(0x20, 0x400).is_none());
        let _ = dma.write(0x28, 0x0100_0401);
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_GPU);
        // Reaching here at all is the assertion.
    }

    #[test]
    fn completion_raises_the_interrupt_only_when_unmasked() {
        let (mut dma, mut ram, mut gpu, mut irq) = rig();
        assert!(dma.write(0x60, 0x1000).is_none());
        assert!(dma.write(0x64, 2).is_none());

        // Masked: no interrupt.
        let _ = dma.write(0x68, 0x1100_0002);
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_OTC);
        assert_eq!(irq.stat(), 0);

        // Enable channel 6 and the master bit, then run again.
        assert!(dma.write(0x74, (1 << 23) | (1 << (16 + CH_OTC))).is_none());
        assert!(dma.write(0x60, 0x1000).is_none());
        assert!(dma.write(0x64, 2).is_none());
        let _ = dma.write(0x68, 0x1100_0002);
        Dma::run(&mut dma, &mut ram, &mut gpu, &mut Cdrom::new(), &mut Spu::new(), &mut irq, CH_OTC);
        assert_ne!(irq.stat() & (1 << irq::DMA), 0);
    }

    #[test]
    fn dicr_bit_31_is_computed_and_flags_acknowledge() {
        let mut dma = Dma::new();
        assert!(dma.write(0x74, 1 << 15).is_none()); // force IRQ
        assert_ne!(dma.read(0x74) & (1 << 31), 0);

        assert!(dma.write(0x74, 0).is_none());
        assert_eq!(dma.read(0x74) & (1 << 31), 0);
    }

    #[test]
    fn a_disabled_channel_does_not_start() {
        let (mut dma, _, _, _) = rig();
        dma.control = 0; // nothing enabled
        assert!(dma.write(0x60, 0x1000).is_none());
        assert!(dma.write(0x64, 4).is_none());
        assert_eq!(dma.write(0x68, 0x1100_0002), None, "started while disabled");
    }
}
