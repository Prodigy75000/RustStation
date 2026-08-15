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
//!   512 KB BIOS, and a decoded-but-stubbed I/O window.
//! * BIOS TTY capture, so a conformance binary's own verdict is readable.
//! * PSX-EXE sideload at the BIOS shell hook.
//! * Save states that satisfy the in-house byte-identical contract.
//!
//! ## What does not exist yet
//!
//! GPU, SPU, CD-ROM, DMA, timers, controllers, and the GTE's 15 commands. No
//! instruction timing model. A disc will not boot. The near-term bar is the
//! R3000A passing a CPU conformance suite, not a game rendering.

pub mod bus;
pub mod cop0;
pub mod cpu;
pub mod exe;
pub mod gte;
pub mod irq;
pub mod save;
pub mod timers;
pub mod video;

use bus::{Bus, BiosError};
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
        })
    }

    /// Power cycle: CPU back to the reset vector, RAM and scratchpad cleared.
    /// The BIOS image and any queued EXE survive, as they would across a real
    /// reset button press.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        self.bus.ram.iter_mut().for_each(|b| *b = 0);
        self.bus.scratchpad.iter_mut().for_each(|b| *b = 0);
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
    pub fn step(&mut self) {
        self.service_hooks();
        self.cpu.step(&mut self.bus);
    }

    /// Execute up to `n` instructions.
    pub fn run(&mut self, n: u64) {
        for _ in 0..n {
            self.step();
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
