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

use crate::cdrom::Cdrom;
use crate::dma::Dma;
use crate::gpu::Gpu;
use crate::irq::{self, Irq};
use crate::mdec::Mdec;
use crate::sio::Sio;
use crate::spu::Spu;
use crate::timers::Timers;
use crate::video::{Standard, Video};

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
const SIO0: Range = Range(0x1F80_1040, 16); // controllers and memory cards
const SIO1: Range = Range(0x1F80_1050, 16); // the serial link port, unpopulated
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

    /// The master clock: CPU cycles since reset. The CPU advances it, and every
    /// timed device is a pure function of it plus its own registers.
    pub cycle: u64,
    /// Set while [`crate::idle`] checks a pass of a loop. Any access to a
    /// device, and any store that changes memory, sets `watch_dirty`. Host
    /// side: never serialized.
    pub(crate) watching: bool,
    pub(crate) watch_dirty: bool,
    /// Cycle at which some device next needs attention. The CPU checks this
    /// after each instruction, which is what stops a frame from collapsing into
    /// a single "run everything, then catch up" phase.
    next_event: u64,
    /// Cycle the timed devices have been advanced to. Never ahead of `cycle`.
    synced_to: u64,

    pub irq: Irq,
    pub video: Video,
    pub timers: Timers,
    pub gpu: Gpu,
    pub dma: Dma,
    pub sio: Sio,
    pub cdrom: Cdrom,
    pub spu: Spu,
    pub mdec: Mdec,

    /// Counters for ports we decode but do not emulate yet. These exist so the
    /// harnesses can answer "what did the BIOS touch that we ignore?" without a
    /// tracing build. The first GPU/SPU/CDROM work is scheduled off them.
    pub stub_reads: u64,
    pub stub_writes: u64,
    /// Accesses that hit no range at all. Distinct from a stub: an unmapped hit
    /// is usually *our* decode being wrong, not a missing subsystem.
    pub unmapped_reads: u64,
    pub unmapped_writes: u64,
    /// The distinct addresses those hit, with a count each.
    ///
    /// A total says something is wrong; an address says what. Ninety million
    /// unmapped reads is a machine spinning on one location, and which location
    /// is the entire diagnosis, so it is worth the eight slots to keep it. Full
    /// is full: later addresses are still counted in the totals above and are
    /// simply not named, which is honest and keeps this off the hot path.
    pub unmapped_sites: [(u32, u64); UNMAPPED_SITES],
}

/// How many distinct unmapped addresses [`Bus::unmapped_sites`] names.
pub const UNMAPPED_SITES: usize = 8;

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
            cycle: 0,
            next_event: 0,
            watching: false,
            watch_dirty: false,
            synced_to: 0,
            irq: Irq::new(),
            video: Video::new(Standard::Ntsc),
            timers: Timers::new(),
            gpu: Gpu::new(),
            dma: Dma::new(),
            sio: Sio::new(),
            cdrom: Cdrom::new(),
            spu: Spu::new(),
            mdec: Mdec::new(),
            stub_reads: 0,
            stub_writes: 0,
            unmapped_reads: 0,
            unmapped_writes: 0,
            unmapped_sites: [(0, 0); UNMAPPED_SITES],
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

    fn width_mask(width: u32) -> u32 {
        if width >= 4 {
            !0
        } else {
            (1 << (8 * width)) - 1
        }
    }

    #[inline(always)]
    fn write_le(buf: &mut [u8], offset: u32, width: u32, val: u32) {
        let o = offset as usize;
        for i in 0..width as usize {
            buf[o + i] = (val >> (8 * i)) as u8;
        }
    }

    /// Advance the master clock and service anything that has come due.
    ///
    /// Called once per instruction. The `next_event` check is the whole point
    /// of the design: devices are only touched when they have something to do,
    /// but they are never allowed to fall behind past the cycle at which they
    /// would raise an interrupt.
    #[inline]
    pub fn tick(&mut self, cycles: u64) {
        self.cycle += cycles;
        if self.cycle >= self.next_event {
            self.sync();
        }
    }

    /// Catch every timed device up to the master clock, then work out when the
    /// next one needs attention.
    ///
    /// Idempotent, and safe to call at any granularity: the devices carry
    /// integer remainders, so syncing every cycle and syncing once per frame
    /// reach the same state. `granularity_does_not_change_the_outcome` in
    /// `timers.rs` and `stepping_granularity_does_not_change_the_result` in
    /// `video.rs` are what hold that down.
    pub fn sync(&mut self) {
        let elapsed = self.cycle - self.synced_to;
        if elapsed > 0 {
            let ticks = self.video.run(elapsed);
            if ticks.vblank_edges > 0 {
                self.irq.raise(irq::VBLANK);
            }
            let blank = (self.video.in_hblank(), self.video.in_vblank());
            self.timers.run(elapsed, &ticks, blank, &mut self.irq);
            self.sio.run(elapsed, &mut self.irq);
            // The SPU before the drive. A CD sector is an event, so it always
            // lands at the end of a sync window; running the SPU first means
            // the samples before that point never see it, which is what a sync
            // on every cycle would give too.
            self.spu
                .run_with_cd(elapsed, &mut || self.cdrom.pop_audio());
            self.spu_irq();
            self.cdrom.run(elapsed, &mut self.irq);
            self.synced_to = self.cycle;
        }

        let mut next = self.video.cycles_to_vblank();
        if let Some(t) = self.timers.cycles_to_irq(&self.video) {
            next = next.min(t);
        }
        if let Some(t) = self.sio.cycles_to_event() {
            next = next.min(t);
        }
        if let Some(t) = self.cdrom.cycles_to_event() {
            next = next.min(t);
        }
        if let Some(t) = self.spu.cycles_to_event() {
            next = next.min(t);
        }
        self.next_event = self.cycle + next.max(1);
    }

    /// Forward an SPU interrupt edge to the interrupt controller.
    fn spu_irq(&mut self) {
        if self.spu.take_irq() {
            self.irq.raise(irq::SPU);
        }
    }

    /// Where the raster is, for [`crate::gpu::Gpu::status`].
    ///
    /// The field is taken as the frame count's parity. That is a derivation
    /// rather than a measurement: nothing here has yet been shown a real
    /// console alternating fields, and interlaced rendering is not implemented,
    /// so the claim is only that consecutive frames report different fields.
    /// Enough for software that waits for the field to change; not enough for
    /// software that cares which field it got.
    pub fn beam(&self) -> crate::gpu::Beam {
        crate::gpu::Beam {
            line: self.video.line(),
            in_vblank: self.video.in_vblank(),
            field: self.video.frames & 1 == 1,
        }
    }

    /// Push the GPU's display settings into the video timing.
    ///
    /// The dot clock divider and the video standard both come from GP1(0x08),
    /// and timer 0 counts dot clocks, so a resolution change silently rescales
    /// a running timer if this is not done. The devices are caught up first, or
    /// the cycles since the last sync would be re-counted at the new rate.
    fn sync_display(&mut self) {
        self.sync();
        self.video.set_dot_divider(self.gpu.dot_divider());
        self.video.set_standard(self.gpu.standard());
        self.sync();
    }

    /// Cycle at which some device next needs attention.
    pub fn next_event(&self) -> u64 {
        self.next_event
    }

    pub(crate) fn next_event_raw(&self) -> u64 {
        self.next_event
    }
    pub(crate) fn synced_to_raw(&self) -> u64 {
        self.synced_to
    }

    /// Restore the scheduler's bookkeeping from a save state.
    ///
    /// `synced_to` is clamped to `cycle`: a device that believes it has already
    /// been advanced past the master clock would compute a negative elapsed
    /// time, and on unsigned arithmetic that is not a small error.
    pub(crate) fn restore_clock(&mut self, cycle: u64, next_event: u64, synced_to: u64) {
        self.cycle = cycle;
        self.synced_to = synced_to.min(cycle);
        self.next_event = next_event;
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
        self.watch_dirty |= self.watching;
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
            // Reading a status register has to see the present, not the last
            // time the scheduler happened to stop.
            self.sync();
            return if off < 4 {
                self.irq.stat() as u32
            } else {
                self.irq.mask() as u32
            };
        }
        if let Some(off) = TIMERS.contains(abs) {
            self.sync();
            let v = self.timers.read(off);
            // A MODE read has side effects (it clears the reached flags), so
            // the next wake-up may have moved.
            self.sync();
            return v;
        }
        if let Some(off) = GPU.contains(abs) {
            return if off < 4 {
                self.gpu.read()
            } else {
                // GPUSTAT carries the beam's line parity, so it has to be read
                // against the present rather than against whenever the
                // scheduler last stopped. Without this the parity is constant
                // for the whole of a poll loop and the loop never ends, which
                // is a hang that looks nothing like a missing sync.
                self.sync();
                self.gpu.status(self.beam())
            };
        }
        if let Some(off) = DMA.contains(abs) {
            return self.dma.read(off, width);
        }
        if let Some(off) = SIO0.contains(abs) {
            // The acknowledge is a scheduled event, so a status read has to see
            // the present rather than the last time the scheduler stopped.
            self.sync();
            return self.sio.read(off, width);
        }
        if let Some(off) = CDROM.contains(abs) {
            self.sync();
            return self.cdrom.read(off);
        }
        if let Some(off) = SPU.contains(abs) {
            // Envelope levels, ENDX and the interrupt flag all move on their
            // own, so a read has to see the present.
            self.sync();
            return self.spu.read(off, width);
        }
        if let Some(off) = MDEC.contains(abs) {
            return self.mdec.read(off);
        }
        if SIO1.contains(abs).is_some()
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
        self.note_unmapped(abs);
        0
    }

    /// Record which address an unmapped access hit, up to [`UNMAPPED_SITES`]
    /// distinct ones. Address 0 is inside RAM, so a zero key means empty.
    fn note_unmapped(&mut self, addr: u32) {
        for site in self.unmapped_sites.iter_mut() {
            if site.0 == addr {
                site.1 += 1;
                return;
            }
            if site.1 == 0 {
                *site = (addr, 1);
                return;
            }
        }
    }

    pub fn store(&mut self, addr: u32, width: u32, val: u32) {
        let abs = mask_region(addr);

        if let Some(off) = RAM.contains(abs) {
            let off = off & (RAM_SIZE as u32 - 1);
            if self.watching
                && Self::read_le(&self.ram, off, width) != val & Self::width_mask(width)
            {
                self.watch_dirty = true;
            }
            Self::write_le(&mut self.ram, off, width, val);
            return;
        }
        if let Some(off) = SCRATCHPAD.contains(abs) {
            if self.watching
                && Self::read_le(&self.scratchpad, off, width) != val & Self::width_mask(width)
            {
                self.watch_dirty = true;
            }
            Self::write_le(&mut self.scratchpad, off, width, val);
            return;
        }
        self.watch_dirty |= self.watching;
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
            self.sync();
            if off < 4 {
                self.irq.ack(val as u16);
            } else {
                self.irq.set_mask(val as u16);
            }
            return;
        }
        if let Some(off) = TIMERS.contains(abs) {
            // Catch up on the old settings before applying the new ones, or the
            // cycles since the last sync get counted under the wrong mode.
            self.sync();
            self.timers.write(off, val);
            self.sync();
            return;
        }
        if let Some(off) = GPU.contains(abs) {
            if off < 4 {
                self.gpu.gp0(val);
            } else {
                self.gpu.gp1(val);
                // GP1 can change the resolution or the video standard, either
                // of which moves the dot clock and the frame length underneath
                // the timers.
                self.sync_display();
            }
            return;
        }
        if let Some(off) = DMA.contains(abs) {
            if let Some(channel) = self.dma.write(off, width, val) {
                Dma::run(
                    &mut self.dma,
                    &mut self.ram,
                    &mut self.gpu,
                    &mut self.cdrom,
                    &mut self.spu,
                    &mut self.mdec,
                    &mut self.irq,
                    channel,
                );
                // A transfer through the IRQ address raises the SPU interrupt.
                self.spu_irq();
            }
            return;
        }
        if let Some(off) = CDROM.contains(abs) {
            self.sync();
            self.cdrom.write(off, val as u8);
            // A write can queue a response or acknowledge one, either of which
            // moves when the controller next needs attention.
            self.sync();
            return;
        }
        if let Some(off) = SIO0.contains(abs) {
            self.sync();
            self.sio.write(off, width, val);
            // A write can start a transfer or drop the select line, either of
            // which moves when the port next needs attention.
            self.sync();
            return;
        }
        if let Some(off) = SPU.contains(abs) {
            // Catch the samples up first, or a key-on lands in samples that
            // were already due before it was written.
            self.sync();
            self.spu.write(off, width, val);
            self.spu_irq();
            // Arming or acknowledging the interrupt changes when the SPU next
            // needs attention.
            self.sync();
            return;
        }
        if let Some(off) = MDEC.contains(abs) {
            self.mdec.write(off, val);
            return;
        }
        if DMA.contains(abs).is_some()
            || SIO1.contains(abs).is_some()
            || EXPANSION_1.contains(abs).is_some()
            || EXPANSION_2.contains(abs).is_some()
        {
            self.stub_writes += 1;
            return;
        }

        self.unmapped_writes += 1;
        self.note_unmapped(abs);
    }

    #[inline(always)]
    pub fn load32(&mut self, addr: u32) -> u32 {
        // RAM, which is nearly every fetch and most data, without the walk
        // through the map. Same result as `load`: the CPU has already
        // enforced alignment, so the word cannot run off the end.
        if let Some(o) = Self::ram_fast(addr) {
            let b = &self.ram[o..o + 4];
            return u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        self.load(addr, 4)
    }
    #[inline(always)]
    pub fn load16(&mut self, addr: u32) -> u16 {
        if let Some(o) = Self::ram_fast(addr) {
            return u16::from_le_bytes([self.ram[o], self.ram[o + 1]]);
        }
        self.load(addr, 2) as u16
    }
    #[inline(always)]
    pub fn load8(&mut self, addr: u32) -> u8 {
        if let Some(o) = Self::ram_fast(addr) {
            return self.ram[o];
        }
        self.load(addr, 1) as u8
    }
    #[inline(always)]
    pub fn store32(&mut self, addr: u32, val: u32) {
        match Self::ram_fast(addr) {
            Some(o) if !self.watching => self.ram[o..o + 4].copy_from_slice(&val.to_le_bytes()),
            _ => self.store(addr, 4, val),
        }
    }
    #[inline(always)]
    pub fn store16(&mut self, addr: u32, val: u16) {
        match Self::ram_fast(addr) {
            Some(o) if !self.watching => self.ram[o..o + 2].copy_from_slice(&val.to_le_bytes()),
            _ => self.store(addr, 2, val as u32),
        }
    }
    #[inline(always)]
    pub fn store8(&mut self, addr: u32, val: u8) {
        match Self::ram_fast(addr) {
            Some(o) if !self.watching => self.ram[o] = val,
            _ => self.store(addr, 1, val as u32),
        }
    }

    /// Offset into RAM if `addr` is in the 8 MB RAM window, which mirrors
    /// the 2 MB four times.
    #[inline(always)]
    fn ram_fast(addr: u32) -> Option<usize> {
        let abs = mask_region(addr);
        (abs < 8 * 1024 * 1024).then_some(abs as usize & (RAM_SIZE - 1))
    }

    pub fn bios(&self) -> &[u8] {
        &self.bios
    }

    /// Interrupt line state, for the CPU's COP0 Cause bit 10.
    #[inline(always)]
    pub fn irq_pending(&self) -> bool {
        self.irq.pending()
    }

    /// Raise an interrupt source directly. For devices that do not yet exist as
    /// modules, and for tests.
    pub fn raise_irq(&mut self, bit: u32) {
        self.irq.raise(bit);
    }
}
