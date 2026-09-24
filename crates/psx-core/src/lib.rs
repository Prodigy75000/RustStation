// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! RustStation: a clean-room Sony PlayStation (PS1) emulator core.
//!
//! Built from hardware documentation only; no third-party emulator source is
//! consulted. See the repository `README.md` for the clean-room rule and
//! `docs/notes/` for the distilled hardware notes each subsystem was written
//! from.
//!
//! ## What exists today
//!
//! * R3000A interpreter: the full user + COP0 instruction set, both delay
//!   slots, exceptions.
//! * The memory map: 2 MB RAM (with its KUSEG mirrors), 1 KB scratchpad,
//!   512 KB BIOS, and a decoded I/O window.
//! * The master clock and its run-until-next-event scheduler, the interrupt
//!   controller, video timing and the three root counters.
//! * The GPU: VRAM, GP0/GP1, the rasterizer, textures, and the DMA that reaches
//!   it.
//! * The GTE, conformant against `gte/test-all`.
//! * SIO0: the controller port, with a digital pad.
//! * The CD-ROM controller and BIN/CUE disc images.
//! * MDEC, the macroblock decoder, which is what full-motion video goes
//!   through.
//! * The SPU's 24 voices, their envelopes and the mixer, and CD audio: CD-DA
//!   and XA-ADPCM. No reverb yet.
//! * BIOS TTY capture, so a conformance binary's own verdict is readable.
//! * PSX-EXE sideload at the BIOS shell hook.
//! * Save states that satisfy the in-house byte-identical contract.
//!
//! ## What does not exist yet
//!
//! Reverb, memory cards, CHD images. No per-instruction timing
//! model: every instruction costs one cycle.

pub mod bus;
pub mod cdrom;
pub mod cop0;
pub mod cpu;
pub mod disc;
pub mod dma;
pub mod exe;
pub mod gpu;
pub mod gte;
mod idle;
pub mod irq;
pub mod mdec;
pub mod save;
pub mod sio;
pub mod spu;
pub mod timers;
pub mod video;
pub mod xa;

use bus::{BiosError, Bus};
use cpu::Cpu;
use exe::Exe;

/// Netplay state-identity token, part 1. Paired with
/// [`save::FORMAT_VERSION`] and `state_size()` in the READY handshake.
pub const CORE_ID: &str = "ruststation-psx";

/// The full machine.
pub struct Psx {
    pub cpu: Cpu,
    pub bus: Bus,

    /// Bytes the BIOS has printed through its TTY entry points. Host-side
    /// observation only: never serialized, never read by emulated code.
    tty: Vec<u8>,
    /// An executable to substitute for the disc when the BIOS shell hands over.
    /// Host-side too, so a state saved before the swap will not carry it.
    pending_exe: Option<Exe>,
    /// Set once `pending_exe` has been applied.
    pub exe_loaded: bool,
    /// Where the game's vsync wait loop starts, once one has been seen. Host
    /// side and never serialized: it only decides where [`idle`] looks, and
    /// the skip it enables is exact, so a state is the same with or without it.
    idle_head: Option<u32>,
    /// Steps since the last look for a wait loop.
    idle_probe: u32,
    /// A loop found to come back to exactly where it started, see
    /// [`idle::poll`]. Host side, like `idle_head`.
    poll_head: Option<u32>,
    /// Whether `poll_head` was reached since the last probe.
    poll_seen: bool,
    /// Checked passes in a row at `poll_head` that were not idle.
    poll_misses: u32,
    /// Loop iterations skipped rather than stepped. Diagnostic.
    pub idle_skipped: u64,
    /// Whether to skip the vsync wait at all. On by default; off is for
    /// checking that it changes nothing.
    pub skip_idle: bool,
}

/// Physical addresses of the BIOS A/B/C function call gates. Emulated code
/// jumps here with the function number in `$t1` (`$9`).
const CALL_GATE_A: u32 = 0x0000_00A0;
const CALL_GATE_B: u32 = 0x0000_00B0;
const CALL_GATE_C: u32 = 0x0000_00C0;

/// Longest string `std_out_puts` will follow before giving up. A runaway
/// pointer into unwritten RAM must not hang the harness.
const MAX_TTY_STRING: usize = 4096;

impl Psx {
    /// Build a machine around a 512 KB BIOS image.
    pub fn new(bios: Vec<u8>) -> Result<Psx, BiosError> {
        Ok(Psx {
            cpu: Cpu::new(),
            bus: Bus::new(bios)?,
            tty: Vec::new(),
            pending_exe: None,
            exe_loaded: false,
            idle_head: None,
            idle_probe: 0,
            poll_head: None,
            poll_seen: false,
            poll_misses: 0,
            idle_skipped: 0,
            skip_idle: true,
        })
    }

    /// Power cycle: CPU back to the reset vector, RAM and scratchpad cleared.
    /// The BIOS image and any queued EXE survive, as they would across a real
    /// reset button press.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        self.bus.ram.iter_mut().for_each(|b| *b = 0);
        self.bus.scratchpad.iter_mut().for_each(|b| *b = 0);
        // VRAM survives a reset on hardware, and so does the DMA control
        // register's power-on value, so `Gpu::reset` clears neither.
        self.bus.gpu.reset();
        self.bus.dma = crate::dma::Dma::new();
        self.bus.stub_reads = 0;
        self.bus.stub_writes = 0;
        self.bus.unmapped_reads = 0;
        self.bus.unmapped_writes = 0;
        self.tty.clear();
        self.exe_loaded = false;
    }

    /// Queue a PSX-EXE. It is written into RAM the moment the BIOS shell
    /// reaches [`exe::SHELL_HOOK`], which is late enough that the kernel, the
    /// function tables and the TTY are all up.
    pub fn sideload_exe(&mut self, exe: Exe) {
        self.pending_exe = Some(exe);
        self.exe_loaded = false;
    }

    /// Execute one instruction, having first serviced the host-side hooks.
    #[inline(always)]
    pub fn step(&mut self) {
        // Every hook lives at a call gate (physical 0xA0, 0xB0, 0xC0, in any
        // segment) or at the shell hook, so anywhere else there is nothing to
        // look at. Checking on every instruction was 5% of the time.
        let pc = self.cpu.pc;
        if pc & 0x1FFF_FFFF < 0x100 || pc == exe::SHELL_HOOK {
            self.service_hooks();
        }
        self.cpu.step(&mut self.bus);
    }

    /// Run for `n` master-clock cycles.
    ///
    /// Every instruction costs one cycle, so this is also `n` instructions,
    /// except where the game is spinning in its vsync wait: those iterations
    /// are skipped in bulk by [`idle`], which lands on exactly the state that
    /// stepping them would have, and ends on the same cycle.
    pub fn run(&mut self, n: u64) {
        let target = self.bus.cycle + n;
        while self.bus.cycle < target {
            if !self.skip_idle {
                self.step();
                continue;
            }
            if Some(self.cpu.pc) == self.idle_head {
                match idle::skip(self, target) {
                    Some(k) => {
                        self.idle_skipped += k;
                        if k > 0 {
                            continue;
                        }
                    }
                    // The code there is no longer the loop.
                    None => self.idle_head = None,
                }
            }
            if Some(self.cpu.pc) == self.poll_head {
                // One real pass decides; if it changed nothing, the passes
                // up to the next event are skipped.
                self.poll_seen = true;
                match idle::poll(self, target) {
                    Some(k) => {
                        self.idle_skipped += k;
                        self.poll_misses = 0;
                    }
                    // A loop that has stopped being idle, and would only cost
                    // a checked pass every time round.
                    None => {
                        self.poll_misses += 1;
                        if self.poll_misses >= 16 {
                            self.poll_head = None;
                            self.poll_misses = 0;
                        }
                    }
                }
                continue;
            }
            if self.idle_probe >= idle::PROBE_INTERVAL {
                self.idle_probe = 0;
                if self.idle_head.is_none() {
                    self.idle_head = idle::find(self);
                }
                if !self.poll_seen {
                    // The known loop has not come round lately: try where the
                    // CPU is now instead.
                    let here = self.cpu.pc;
                    if let Some(k) = idle::poll(self, target) {
                        self.poll_head = Some(here);
                        self.poll_misses = 0;
                        self.idle_skipped += k;
                    }
                }
                self.poll_seen = false;
                continue;
            }
            // Nothing to look at here: step until something might be, with
            // one set of compares per instruction.
            let idle_at = self.idle_head.unwrap_or(u32::MAX);
            let poll_at = self.poll_head.unwrap_or(u32::MAX);
            loop {
                self.step();
                self.idle_probe += 1;
                let pc = self.cpu.pc;
                if self.bus.cycle >= target
                    || pc == idle_at
                    || pc == poll_at
                    || self.idle_probe >= idle::PROBE_INTERVAL
                {
                    break;
                }
            }
        }
    }

    /// Everything the BIOS has printed so far.
    pub fn tty(&self) -> &[u8] {
        &self.tty
    }

    /// Take the TTY buffer, leaving it empty.
    pub fn take_tty(&mut self) -> String {
        let out = String::from_utf8_lossy(&self.tty).into_owned();
        self.tty.clear();
        out
    }

    fn service_hooks(&mut self) {
        let pc = self.cpu.pc;

        if !self.exe_loaded && pc == exe::SHELL_HOOK && self.pending_exe.is_some() {
            let exe = self.pending_exe.take().expect("checked above");
            self.apply_exe(&exe);
            self.pending_exe = Some(exe);
            self.exe_loaded = true;
            return;
        }

        // The call gates are reached through KUSEG/KSEG0/KSEG1 alike, so match
        // on the physical address.
        let func = self.cpu.reg(9);
        match (bus::mask_region(pc), func) {
            // A(3Ch) / B(3Dh) std_out_putchar
            (CALL_GATE_A, 0x3C) | (CALL_GATE_B, 0x3D) => {
                let c = self.cpu.reg(4) as u8;
                self.tty.push(c);
            }
            // A(3Eh) / B(3Fh) std_out_puts
            (CALL_GATE_A, 0x3E) | (CALL_GATE_B, 0x3F) => {
                let ptr = self.cpu.reg(4);
                self.capture_string(ptr);
            }
            (CALL_GATE_C, _) => {}
            _ => {}
        }
    }

    /// Follow a NUL-terminated string out of guest memory into the TTY buffer.
    /// Reads go through a plain RAM view rather than [`Bus::load`] so that
    /// watching the TTY can never itself perturb device state.
    fn capture_string(&mut self, ptr: u32) {
        let base = bus::mask_region(ptr) as usize;
        for i in 0..MAX_TTY_STRING {
            let addr = base.wrapping_add(i);
            if addr >= bus::RAM_SIZE {
                break;
            }
            let c = self.bus.ram[addr];
            if c == 0 {
                break;
            }
            self.tty.push(c);
        }
    }

    fn apply_exe(&mut self, exe: &Exe) {
        if exe.memfill_size != 0 {
            let start = bus::mask_region(exe.memfill_start) as usize;
            let end = (start + exe.memfill_size as usize).min(bus::RAM_SIZE);
            if start < end {
                self.bus.ram[start..end].iter_mut().for_each(|b| *b = 0);
            }
        }

        let dest = bus::mask_region(exe.dest) as usize;
        let end = dest + exe.text.len();
        // `Exe::parse` has already proven this fits; the assert is here so a
        // future loader change cannot quietly start truncating.
        assert!(end <= bus::RAM_SIZE, "EXE text does not fit in RAM");
        self.bus.ram[dest..end].copy_from_slice(&exe.text);

        self.cpu.set_pc(exe.initial_pc);
        self.cpu.force_reg(28, exe.initial_gp);
        if exe.sp_base != 0 {
            let sp = exe.sp_base.wrapping_add(exe.sp_offset);
            self.cpu.force_reg(29, sp);
            self.cpu.force_reg(30, sp);
        }
    }
}
