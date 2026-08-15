// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! COP0: the R3000A system control coprocessor.
//!
//! Only the exception/status half is fitted on the PlayStation: there is no TLB,
//! so the register file is sparse. The hardware breakpoint registers (BPC, BDA,
//! DCIC, ...) exist and are writable; games and the BIOS do write them, so they
//! are stored, but nothing acts on them yet.

/// The exception codes COP0 Cause bits 6..2 can carry. Only the ones the
/// PlayStation can actually raise are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// External interrupt (Cause bit 10 vs Status IM).
    Interrupt = 0x0,
    /// Unaligned or otherwise bad address on a load.
    AddressErrorLoad = 0x4,
    /// Unaligned or otherwise bad address on a store.
    AddressErrorStore = 0x5,
    /// `SYSCALL`.
    SysCall = 0x8,
    /// `BREAK`.
    Break = 0x9,
    /// Undefined opcode.
    IllegalInstruction = 0xA,
    /// Coprocessor instruction for a coprocessor disabled in Status.
    CoprocessorError = 0xB,
    /// `ADD`/`ADDI`/`SUB` signed overflow.
    Overflow = 0xC,
}

#[derive(Clone)]
pub struct Cop0 {
    /// cop0r3: breakpoint on execute.
    pub bpc: u32,
    /// cop0r5: breakpoint on data access.
    pub bda: u32,
    /// cop0r6: memory reference to the branch target (read-only on hardware).
    pub jump_dest: u32,
    /// cop0r7: breakpoint control.
    pub dcic: u32,
    /// cop0r8: the address that caused the last address error.
    pub bad_vaddr: u32,
    /// cop0r9: data access breakpoint mask.
    pub bdam: u32,
    /// cop0r11: execute breakpoint mask.
    pub bpcm: u32,
    /// cop0r12: Status. Bit 0 IEc, 1 KUc, 2 IEp, 3 KUp, 4 IEo, 5 KUo,
    /// 8..15 IM, 16 Isc (isolate cache), 22 BEV (boot exception vectors).
    pub sr: u32,
    /// cop0r13: Cause.
    pub cause: u32,
    /// cop0r14: Exception PC.
    pub epc: u32,
    /// cop0r15: Processor ID. Fixed for the R3000A in a PlayStation.
    pub prid: u32,
}

/// Status bit 16. When set, stores go to the I-cache instead of memory. The
/// BIOS uses it to scrub the cache at boot. Until an I-cache is modelled, a
/// store made while isolated must simply be *dropped*, never written to RAM.
pub const SR_ISOLATE_CACHE: u32 = 1 << 16;
/// Status bit 22. Chooses the exception vector base.
pub const SR_BOOT_EXCEPTION_VECTORS: u32 = 1 << 22;

impl Default for Cop0 {
    fn default() -> Self {
        Cop0::new()
    }
}

impl Cop0 {
    pub fn new() -> Cop0 {
        Cop0 {
            bpc: 0,
            bda: 0,
            jump_dest: 0,
            dcic: 0,
            bad_vaddr: 0,
            bdam: 0,
            bpcm: 0,
            // Reset state: BEV set, so the first exception vectors into ROM at
            // 0xBFC00180 rather than into RAM that nothing has written yet.
            sr: SR_BOOT_EXCEPTION_VECTORS,
            cause: 0,
            epc: 0,
            prid: 0x0000_0002,
        }
    }

    #[inline(always)]
    pub fn cache_isolated(&self) -> bool {
        self.sr & SR_ISOLATE_CACHE != 0
    }

    /// Where an exception vectors to, per Status BEV.
    #[inline(always)]
    pub fn exception_handler(&self) -> u32 {
        if self.sr & SR_BOOT_EXCEPTION_VECTORS != 0 {
            0xBFC0_0180
        } else {
            0x8000_0080
        }
    }

    /// Push the interrupt-enable / kernel-user mode stack (Status bits 5..0
    /// shift left by two on entry) and record the cause.
    pub fn enter_exception(&mut self, cause: Exception, epc: u32, in_delay_slot: bool) {
        let mode = self.sr & 0x3F;
        self.sr &= !0x3F;
        self.sr |= (mode << 2) & 0x3F;

        self.cause &= !0x7C;
        self.cause |= (cause as u32) << 2;

        if in_delay_slot {
            // BD: the faulting instruction was in a branch delay slot, so EPC
            // points at the *branch*, and the handler re-executes it.
            self.epc = epc.wrapping_sub(4);
            self.cause |= 1 << 31;
        } else {
            self.epc = epc;
            self.cause &= !(1 << 31);
        }
    }

    /// `RFE`: pop the mode stack. Note it only shifts the two-deep stack back
    /// down; the "old" pair keeps its value, which is what the hardware does.
    pub fn return_from_exception(&mut self) {
        let mode = self.sr & 0x3F;
        self.sr &= !0xF;
        self.sr |= mode >> 2;
    }

    /// True when an external interrupt is both pending and unmasked, and
    /// interrupts are enabled at all.
    #[inline(always)]
    pub fn interrupt_ready(&self) -> bool {
        let pending = (self.cause & self.sr) & 0x0000_FF00;
        self.sr & 1 != 0 && pending != 0
    }

    /// Set or clear Cause bit 10, the single external interrupt line the
    /// PlayStation's interrupt controller drives.
    #[inline(always)]
    pub fn set_external_irq(&mut self, active: bool) {
        if active {
            self.cause |= 1 << 10;
        } else {
            self.cause &= !(1 << 10);
        }
    }

    pub fn read(&self, index: u32) -> u32 {
        match index {
            3 => self.bpc,
            5 => self.bda,
            6 => self.jump_dest,
            7 => self.dcic,
            8 => self.bad_vaddr,
            9 => self.bdam,
            11 => self.bpcm,
            12 => self.sr,
            13 => self.cause,
            14 => self.epc,
            15 => self.prid,
            // Unassigned COP0 registers read as garbage on hardware; zero is
            // the deterministic choice, and determinism outranks fidelity here
            // because save states have to agree across builds.
            _ => 0,
        }
    }

    pub fn write(&mut self, index: u32, val: u32) {
        match index {
            3 => self.bpc = val,
            5 => self.bda = val,
            6 => self.jump_dest = val,
            7 => self.dcic = val,
            9 => self.bdam = val,
            11 => self.bpcm = val,
            12 => self.sr = val,
            // Only the two software-interrupt bits of Cause are writable.
            13 => self.cause = (self.cause & !0x300) | (val & 0x300),
            // r8 BadVaddr, r14 EPC and r15 PRID are read-only; r6 JumpDest is
            // read-only on hardware too but is harmless to allow.
            _ => {}
        }
    }
}
