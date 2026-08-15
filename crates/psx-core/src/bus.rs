// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The system bus: address decode, RAM, scratchpad, BIOS ROM, and the I/O
//! window.
//!
//! The R3000A sees a 32-bit virtual address space carved into the standard MIPS
//! segments. The PlayStation has no MMU and no TLB fitted, so translation is
//! nothing but a mask per segment: KUSEG, KSEG0 and KSEG1 are three views of the
//! same 512 MB of physical space, differing only in cacheability. That is why
//! `mask_region` below is the whole of "virtual memory" here. See
//! `docs/MEMORY_MAP.md`.

/// 2 MB of main RAM. (Development units had 8 MB; retail is 2 MB and KUSEG
/// mirrors it four times over the first 8 MB, which some games rely on.)
pub const RAM_SIZE: usize = 2 * 1024 * 1024;
/// 1 KB of scratchpad, the D-cache wired as fast RAM. Physically it is *not*
/// reachable through KSEG1, because KSEG1 is the uncached view.
pub const SCRATCHPAD_SIZE: usize = 1024;
/// 512 KB BIOS ROM (SCPH-xxxx).
pub const BIOS_SIZE: usize = 512 * 1024;

/// Per-segment address mask, indexed by the top three bits of the address.
///
/// KUSEG (0x0000_0000..0x8000_0000) is untranslated, KSEG0 strips bit 31,
/// KSEG1 strips bits 31..29, KSEG2 is untranslated. Physical addresses then
/// land in the 0x0000_0000..0x2000_0000 window that `decode` matches on.
const REGION_MASK: [u32; 8] = [
    // KUSEG: 2 GB, no translation.
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    0xFFFF_FFFF,
    // KSEG0: 512 MB, cached.
    0x7FFF_FFFF,
    // KSEG1: 512 MB, uncached.
    0x1FFF_FFFF,
    // KSEG2: 1 GB, no translation (only the cache-control port lives here).
    0xFFFF_FFFF,
    0xFFFF_FFFF,
];

/// Translate a virtual address to a physical one.
#[inline(always)]
pub fn mask_region(addr: u32) -> u32 {
    addr & REGION_MASK[(addr >> 29) as usize]
}

/// A half-open physical address range `[start, start + len)`.
struct Range(u32, u32);

impl Range {
    /// Offset of `addr` within the range, or `None` if outside.
    #[inline(always)]
    fn contains(&self, addr: u32) -> Option<u32> {
        if addr >= self.0 && addr < self.0 + self.1 {
            Some(addr - self.0)
        } else {
            None
        }
    }
}

const RAM: Range = Range(0x0000_0000, 8 * 1024 * 1024); // 2 MB mirrored to 8 MB
const EXPANSION_1: Range = Range(0x1F00_0000, 8 * 1024 * 1024);
const SCRATCHPAD: Range = Range(0x1F80_0000, 1024);
const MEM_CTRL: Range = Range(0x1F80_1000, 36);
const PERIPHERAL: Range = Range(0x1F80_1040, 32); // joypad + serial
const RAM_SIZE_REG: Range = Range(0x1F80_1060, 4);
const IRQ_CTRL: Range = Range(0x1F80_1070, 8);
const DMA: Range = Range(0x1F80_1080, 0x80);
const TIMERS: Range = Range(0x1F80_1100, 0x30);
const CDROM: Range = Range(0x1F80_1800, 4);
const GPU: Range = Range(0x1F80_1810, 8);
const MDEC: Range = Range(0x1F80_1820, 8);
const SPU: Range = Range(0x1F80_1C00, 640);
const EXPANSION_2: Range = Range(0x1F80_2000, 66);
const BIOS: Range = Range(0x1FC0_0000, BIOS_SIZE as u32);
const CACHE_CTRL: Range = Range(0xFFFE_0130, 4);

/// Why a `Bus::new` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiosError {
    /// The image was not exactly 512 KB.
    WrongSize(usize),
}

impl core::fmt::Display for BiosError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BiosError::WrongSize(n) => write!(
                f,
                "BIOS image is {n} bytes, expected exactly {BIOS_SIZE} (512 KB)"
            ),
        }
    }
}

pub struct Bus {
    pub ram: Vec<u8>,
    pub scratchpad: Vec<u8>,
    /// The BIOS image. Read-only, and deliberately **excluded from save states**
    /// because it is this console's equivalent of the cartridge ROM.
    bios: Vec<u8>,

    /// 0x1F801000..0x1F801024: expansion base/size and bus timing. The BIOS
    /// writes fixed constants here at boot; nothing reads them back but itself.
    pub(crate) mem_ctrl: [u32; 9],
    pub(crate) ram_size: u32,
    pub(crate) cache_ctrl: u32,
    pub(crate) i_stat: u32,
    pub(crate) i_mask: u32,

    /// Counters for ports we decode but do not emulate yet. These exist so the
    /// harnesses can answer "what did the BIOS touch that we ignore?" without a
    /// tracing build. The first GPU/SPU/CDROM work is scheduled off them.
    pub stub_reads: u64,
    pub stub_writes: u64,
    /// Accesses that hit no range at all. Distinct from a stub: an unmapped hit
    /// is usually *our* decode being wrong, not a missing subsystem.
    pub unmapped_reads: u64,
    pub unmapped_writes: u64,
}

impl Bus {
    pub fn new(bios: Vec<u8>) -> Result<Bus, BiosError> {
        if bios.len() != BIOS_SIZE {
            return Err(BiosError::WrongSize(bios.len()));
        }
        Ok(Bus {
            ram: vec![0; RAM_SIZE],
            scratchpad: vec![0; SCRATCHPAD_SIZE],
            bios,
            mem_ctrl: [0; 9],
            ram_size: 0,
            cache_ctrl: 0,
            i_stat: 0,
            i_mask: 0,
            stub_reads: 0,
            stub_writes: 0,
            unmapped_reads: 0,
            unmapped_writes: 0,
        })
    }

    /// Read `N` bytes little-endian out of a byte slice at `offset`.
    #[inline(always)]
    fn read_le(buf: &[u8], offset: u32, width: u32) -> u32 {
        let o = offset as usize;
        let mut v = 0u32;
        for i in 0..width as usize {
            v |= (buf[o + i] as u32) << (8 * i);
        }
        v
    }

    #[inline(always)]
    fn write_le(buf: &mut [u8], offset: u32, width: u32, val: u32) {
        let o = offset as usize;
        for i in 0..width as usize {
            buf[o + i] = (val >> (8 * i)) as u8;
        }
    }

    /// One decode path for all three access widths. Width is 1, 2 or 4; the CPU
    /// has already enforced alignment, so a load never straddles a range.
    pub fn load(&mut self, addr: u32, width: u32) -> u32 {
        let abs = mask_region(addr);

        if let Some(off) = RAM.contains(abs) {
            // Retail RAM is 2 MB mirrored four times across the 8 MB window.
            return Self::read_le(&self.ram, off & (RAM_SIZE as u32 - 1), width);
        }
        if let Some(off) = BIOS.contains(abs) {
            return Self::read_le(&self.bios, off, width);
        }
        if let Some(off) = SCRATCHPAD.contains(abs) {
            return Self::read_le(&self.scratchpad, off, width);
        }
        if let Some(off) = MEM_CTRL.contains(abs) {
            return self.mem_ctrl[(off / 4) as usize];
        }
        if RAM_SIZE_REG.contains(abs).is_some() {
            return self.ram_size;
        }
        if CACHE_CTRL.contains(abs).is_some() {
            return self.cache_ctrl;
        }
        if let Some(off) = IRQ_CTRL.contains(abs) {
            return if off == 0 { self.i_stat } else { self.i_mask };
        }
        if let Some(off) = GPU.contains(abs) {
            self.stub_reads += 1;
            // GPUSTAT. Bits 26/27/28 are "ready for command / ready to send
            // VRAM / ready to receive DMA"; the BIOS spins on them at boot, so
            // a zero here is an instant hang. Everything else is still a lie.
            return if off == 4 { 0x1C00_0000 } else { 0 };
        }
        if SPU.contains(abs).is_some()
            || CDROM.contains(abs).is_some()
            || MDEC.contains(abs).is_some()
            || DMA.contains(abs).is_some()
            || TIMERS.contains(abs).is_some()
            || PERIPHERAL.contains(abs).is_some()
            || EXPANSION_1.contains(abs).is_some()
            || EXPANSION_2.contains(abs).is_some()
        {
            self.stub_reads += 1;
            // Open bus on an absent expansion reads as all-ones on hardware;
            // the rest of these read back as zero until their subsystem lands.
            return if EXPANSION_1.contains(abs).is_some() {
                !0 >> (32 - 8 * width)
            } else {
                0
            };
        }

        self.unmapped_reads += 1;
        0
    }

    pub fn store(&mut self, addr: u32, width: u32, val: u32) {
        let abs = mask_region(addr);

        if let Some(off) = RAM.contains(abs) {
            Self::write_le(&mut self.ram, off & (RAM_SIZE as u32 - 1), width, val);
            return;
        }
        if let Some(off) = SCRATCHPAD.contains(abs) {
            Self::write_le(&mut self.scratchpad, off, width, val);
            return;
        }
        if BIOS.contains(abs).is_some() {
            // ROM. Writes are dropped, not an error: the BIOS itself does this.
            return;
        }
        if let Some(off) = MEM_CTRL.contains(abs) {
            self.mem_ctrl[(off / 4) as usize] = val;
            return;
        }
        if RAM_SIZE_REG.contains(abs).is_some() {
            self.ram_size = val;
            return;
        }
        if CACHE_CTRL.contains(abs).is_some() {
            self.cache_ctrl = val;
            return;
        }
        if let Some(off) = IRQ_CTRL.contains(abs) {
            if off == 0 {
                // I_STAT is write-acknowledge: a zero bit clears, a one keeps.
                self.i_stat &= val;
            } else {
                self.i_mask = val;
            }
            return;
        }
        if GPU.contains(abs).is_some()
            || SPU.contains(abs).is_some()
            || CDROM.contains(abs).is_some()
            || MDEC.contains(abs).is_some()
            || DMA.contains(abs).is_some()
            || TIMERS.contains(abs).is_some()
            || PERIPHERAL.contains(abs).is_some()
            || EXPANSION_1.contains(abs).is_some()
            || EXPANSION_2.contains(abs).is_some()
        {
            self.stub_writes += 1;
            return;
        }

        self.unmapped_writes += 1;
    }

    #[inline(always)]
    pub fn load32(&mut self, addr: u32) -> u32 {
        self.load(addr, 4)
    }
    #[inline(always)]
    pub fn load16(&mut self, addr: u32) -> u16 {
        self.load(addr, 2) as u16
    }
    #[inline(always)]
    pub fn load8(&mut self, addr: u32) -> u8 {
        self.load(addr, 1) as u8
    }
    #[inline(always)]
    pub fn store32(&mut self, addr: u32, val: u32) {
        self.store(addr, 4, val)
    }
    #[inline(always)]
    pub fn store16(&mut self, addr: u32, val: u16) {
        self.store(addr, 2, val as u32)
    }
    #[inline(always)]
    pub fn store8(&mut self, addr: u32, val: u8) {
        self.store(addr, 1, val as u32)
    }

    pub fn bios(&self) -> &[u8] {
        &self.bios
    }

    /// Interrupt line state, for the CPU's COP0 Cause bit 10.
    #[inline(always)]
    pub fn irq_pending(&self) -> bool {
        self.i_stat & self.i_mask != 0
    }

    /// Raise an interrupt source. Nothing calls this yet (no peripheral is
    /// emulated), but the plumbing is here so the first one to land is a
    /// one-liner rather than a bus refactor.
    pub fn raise_irq(&mut self, bit: u32) {
        self.i_stat |= 1 << bit;
    }
}
