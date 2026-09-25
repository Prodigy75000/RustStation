// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! What an instruction costs beyond its one cycle: fetching it, the data it
//! loads, and waiting on the multiplier or the GTE.
//!
//! From psx-spx ("Memory Map", "Memory Control", "CPU Specifications" and
//! "GTE"), and from the hardware log of the suite's `cpu/access-time`, whose
//! source says what it measures: a cached loop of `nop; addiu; load; bnez;
//! nop`, less the same loop without the load. So each figure there is the
//! whole cost of one load instruction with four independent instructions
//! after it, which is how compiled code mostly issues them.
//!
//! What is not here: the write queue (stores cost their one cycle however
//! slow the device), the load shadow as its own mechanism (the table below
//! already has it, for loads with independent work behind them; back to back
//! they cost a couple of cycles more on the console), DRAM refresh, and DMA
//! taking the bus from the CPU. See `docs/notes/TIMING.md`.

use crate::bus::mask_region;

/// Cycles a load stalls beyond its own, by what it reads and how many bytes
/// it takes. LWL and LWR take one to four, only the ones they need: at
/// 1F801DAAh the suite's word read is such a pair, two bytes each, and the
/// console times it at two halfword reads. Three bytes have no figure of
/// their own and are costed as a halfword.
///
/// The access-time log, rounded, less one for the instruction:
///
/// | Region | 8 | 16 | 32 |
/// |---|---|---|---|
/// | RAM | 5.21 | 5.3 | 5.14 |
/// | BIOS | 7.6 | 12.94 | 24.94 |
/// | Scratchpad | 1.5 | 1.1 | 0.94 |
/// | Expansion 1 | 6.94 | 13.7 | 25.7 |
/// | Expansion 2 | 10.99 | 25.99 | 55.98 |
/// | Expansion 3 | 6.7 | 6.1 | 9.95 |
/// | CD-ROM | 8.0 | 14.0 | 25.93 |
/// | SPU | 17.99 | 17.99 | 38.94 |
/// | Cache control | 0.95 | 1.9 | 1.9 |
///
/// Every other I/O register (DMA, pads, serial, memory control, interrupts,
/// timers, GPU, MDEC) reads in 3 to 4: psx-spx says they sit behind one
/// decoder and cost the same, so they get 3.
#[inline(always)]
pub fn load_stall(addr: u32, width: u32) -> u64 {
    let phys = mask_region(addr);
    // RAM first, and without the table: it is nearly every load.
    if phys < 0x0080_0000 {
        return 4;
    }
    let w = (width >> 1).min(2) as usize;
    let row: [u64; 3] = match phys {
        0x1F00_0000..=0x1F7F_FFFF => [6, 13, 25],
        0x1F80_0000..=0x1F80_03FF => [0, 0, 0],
        0x1F80_1800..=0x1F80_180F => [7, 13, 25],
        0x1F80_1C00..=0x1F80_1FFF => [17, 17, 38],
        0x1F80_1000..=0x1F80_1FFF => [2, 2, 2],
        0x1F80_2000..=0x1F80_3FFF => [10, 25, 55],
        0x1FA0_0000..=0x1FBF_FFFF => [6, 5, 9],
        0x1FC0_0000..=0x1FC7_FFFF => [7, 12, 24],
        0xFFFE_0000..=0xFFFE_01FF => [0, 1, 1],
        _ => [2, 2, 2],
    };
    row[w]
}

/// Cycles an instruction fetch stalls when it does not come from the
/// I-cache: the word read with nothing to overlap it.
///
/// Main RAM is 7 cycles a word read back to back (psx-spx, "Load Timing"),
/// so 6 beyond the instruction's own. The BIOS ROM is an 8-bit chip read
/// four times for a word; psx-spx gives 27 to 33 a word depending on the
/// console, and its memory control formula gives 29 for the delays every
/// BIOS sets, so 28. Anywhere else, the word load above.
#[inline(always)]
pub fn fetch_stall(addr: u32) -> u64 {
    let phys = mask_region(addr);
    if phys < 0x0080_0000 {
        6
    } else if (0x1FC0_0000..0x1FC8_0000).contains(&phys) {
        28
    } else {
        load_stall(addr, 4)
    }
}

/// BIU/cache control (FFFE0130h) bit 11: the I-cache is on.
pub const BCC_IS1: u32 = 1 << 11;
/// Bit 2: with Status IsC, a store writes a cache tag.
pub const BCC_TAG: u32 = 1 << 2;

/// Lines in the I-cache: 4 KB of four-word lines.
pub const ICACHE_LINES: usize = 256;

/// The instruction cache's tags, which is all timing needs: whether a fetch
/// hits. psx-spx: direct mapped, 256 lines of four words, indexed by address
/// bits 11..4; a tag holds physical address bits 31..12 and one valid bit per
/// word. KUSEG and KSEG0 are cached, KSEG1 is not.
///
/// The instructions themselves still come from RAM. On the console a line
/// keeps the code it was filled with until it is flushed, even if RAM under
/// it changes, and a few games depend on that; that is not modelled.
#[derive(Clone, PartialEq, Eq)]
pub struct ICache {
    pub tags: [u32; ICACHE_LINES],
}

impl Default for ICache {
    fn default() -> Self {
        ICache::new()
    }
}

impl ICache {
    pub fn new() -> ICache {
        ICache {
            tags: [0; ICACHE_LINES],
        }
    }

    /// Every line invalid, as the kernel's FlushCache leaves it.
    pub fn flush(&mut self) {
        self.tags = [0; ICACHE_LINES];
    }

    /// Whether fetching `pc` would hit, without filling anything.
    #[inline(always)]
    pub fn hits(&self, pc: u32) -> bool {
        let t = self.tags[(pc >> 4) as usize & 0xFF];
        t & !0xF == pc & 0x1FFF_F000 && t & (1 << ((pc >> 2) & 3)) != 0
    }

    /// Fetch `pc` from a cached segment: 0 on a hit, else the fill's stall,
    /// with the line filled.
    ///
    /// A miss fills from the word asked for to the end of the line, with no
    /// wrap; with IBLKSZ 0 (BCC bits 8-9) a miss at word 0 fills two words.
    /// A miss on a line whose tag matches refills all four. From RAM the CPU
    /// runs each word as it arrives (streaming, BCC bit 17 clear), so a fill
    /// costs the first word's latency; anywhere else, every word in full.
    #[inline(always)]
    pub fn fetch(&mut self, pc: u32, bcc: u32) -> u64 {
        let line = (pc >> 4) as usize & 0xFF;
        let word = (pc >> 2) & 3;
        let tag = pc & 0x1FFF_F000;
        let t = self.tags[line];
        let words = if t & !0xF == tag {
            if t & (1 << word) != 0 {
                return 0;
            }
            self.tags[line] = tag | 0xF;
            4
        } else {
            let n = if word == 0 && (bcc >> 8) & 3 == 0 {
                2
            } else {
                4 - word
            };
            self.tags[line] = tag | (((1 << n) - 1) << word);
            n
        };
        if mask_region(pc) < 0x0080_0000 {
            fetch_stall(pc)
        } else {
            fetch_stall(pc) * words as u64
        }
    }

    /// A store with the cache isolated and BCC's TAG bit set: psx-spx, the
    /// tag becomes the address's upper bits and the data's low four bits
    /// become the valid bits. The BIOS flushes the cache this way.
    pub fn write_tag(&mut self, addr: u32, val: u32) {
        self.tags[(addr >> 4) as usize & 0xFF] = (addr & 0xFFFF_F000) | (val & 0xF);
    }
}

/// Cycles a multiply occupies the unit, from psx-spx: by the size of `rs`,
/// signed or not.
pub fn mult_cycles(rs: u32, signed: bool) -> u64 {
    let v = if signed && (rs as i32) < 0 { !rs } else { rs };
    if v < 0x800 {
        6
    } else if v < 0x10_0000 {
        9
    } else {
        13
    }
}

/// Every divide takes 36 cycles.
pub const DIV_CYCLES: u64 = 36;

/// Cycles a GTE command takes, from psx-spx's command list. Numbers it does
/// not list do nothing and are given one.
pub fn gte_cycles(command: u32) -> u64 {
    match command & 0x3F {
        0x01 => 15, // RTPS
        0x06 => 8,  // NCLIP
        0x0C => 6,  // OP
        0x10 => 8,  // DPCS
        0x11 => 8,  // INTPL
        0x12 => 8,  // MVMVA
        0x13 => 19, // NCDS
        0x14 => 13, // CDP
        0x16 => 44, // NCDT
        0x1B => 17, // NCCS
        0x1C => 11, // CC
        0x1E => 14, // NCS
        0x20 => 30, // NCT
        0x28 => 5,  // SQR
        0x29 => 8,  // DCPL
        0x2A => 17, // DPCT
        0x2D => 5,  // AVSZ3
        0x2E => 6,  // AVSZ4
        0x30 => 23, // RTPT
        0x3D => 5,  // GPF
        0x3E => 5,  // GPL
        0x3F => 39, // NCCT
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_miss_fills_from_the_word_to_the_end_of_the_line() {
        let mut c = ICache::new();
        let bcc = 0x0001_E988;
        // Entering a line at word 2 fills words 2 and 3 only.
        assert_eq!(c.fetch(0x8001_0008, bcc), 6);
        assert!(!c.hits(0x8001_0000));
        assert!(!c.hits(0x8001_0004));
        assert!(c.hits(0x8001_0008));
        assert!(c.hits(0x8001_000C));
        assert_eq!(c.fetch(0x8001_000C, bcc), 0);
        // Word 0 of the same tag: a full refill.
        assert_eq!(c.fetch(0x8001_0000, bcc), 6);
        assert!(c.hits(0x8001_0004));
        // KUSEG and KSEG0 share tags: they are one physical address.
        assert!(c.hits(0x0001_0004));
        // 4 KB further on is the same line with another tag.
        assert!(!c.hits(0x8001_1000));
        assert_eq!(c.fetch(0x8001_1000, bcc), 6);
        assert!(!c.hits(0x8001_0000));
    }

    #[test]
    fn a_two_word_refill_stops_at_word_one() {
        let mut c = ICache::new();
        c.fetch(0x8001_0100, 0x0001_E888);
        assert!(c.hits(0x8001_0104));
        assert!(!c.hits(0x8001_0108));
    }

    #[test]
    fn a_tag_write_of_zero_invalidates_the_line() {
        let mut c = ICache::new();
        c.fetch(0x8000_0230, 0x0001_E988);
        assert!(c.hits(0x8000_0230));
        c.write_tag(0x230, 0);
        assert!(!c.hits(0x8000_0230));
    }

    /// The access-time log's figures, whole instruction, less one.
    #[test]
    fn loads_cost_what_the_console_measured() {
        assert_eq!(load_stall(0x8000_0000, 4), 4);
        assert_eq!(load_stall(0xA010_0000, 1), 4);
        assert_eq!(load_stall(0x1F80_0000, 4), 0);
        assert_eq!(load_stall(0xBFC0_0000, 1), 7);
        assert_eq!(load_stall(0xBFC0_0000, 4), 24);
        assert_eq!(load_stall(0x1F80_1DAA, 2), 17);
        assert_eq!(load_stall(0x1F80_1DA8, 4), 38);
        assert_eq!(load_stall(0x1F80_1800, 1), 7);
        assert_eq!(load_stall(0x1F80_1070, 4), 2);
        assert_eq!(load_stall(0x1F80_2000, 4), 55);
        assert_eq!(load_stall(0xFFFE_0130, 4), 1);
    }

    #[test]
    fn multiplies_take_longer_for_a_bigger_first_operand() {
        assert_eq!(mult_cycles(0x7FF, false), 6);
        assert_eq!(mult_cycles(0x800, false), 9);
        assert_eq!(mult_cycles(0xF_FFFF, false), 9);
        assert_eq!(mult_cycles(0x10_0000, false), 13);
        assert_eq!(mult_cycles(0xFFFF_F800, true), 6);
        assert_eq!(mult_cycles(0xFFFF_F800, false), 13);
        assert_eq!(mult_cycles(0xFFF0_0000, true), 9);
        assert_eq!(mult_cycles(0x8000_0000, true), 13);
    }
}
