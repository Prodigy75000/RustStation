// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The MIPS R3000A CPU (LSI CW33300 core): interpreter.
//!
//! Two pipeline artifacts are visible to software on this chip and both are
//! modelled here, because a great deal of PlayStation code depends on them:
//!
//! * **The branch delay slot.** The instruction *after* a branch always
//!   executes, before the branch takes effect. Modelled with the
//!   `pc` / `next_pc` pair: `pc` is the instruction to fetch, `next_pc` is what
//!   it will become, and a branch rewrites `next_pc` rather than `pc`.
//! * **The load delay slot.** A load's result is not readable by the very next
//!   instruction. That instruction still sees the *old* register. Modelled
//!   with a shadow register file: reads come from `regs`, writes go to
//!   `out_regs`, and the pending load is applied to `out_regs` *before* the
//!   instruction executes, so an explicit write by that instruction wins over
//!   the arriving load.
//!
//!   Three consequences that pull in different directions, so each is pinned by
//!   its own test: an explicit write in the delay slot **beats** the arriving
//!   load; a second plain load to the same register **cancels** the first,
//!   which is therefore never architecturally visible; and `LWL`/`LWR`
//!   **merge** with a pending load, so the in-flight value has to reach them.
//!
//!   `LWL`/`LWR` bypass [`Cpu::set_load`] and assign `load` directly. Whether
//!   they should *also* cancel is genuinely unsettled: it changes only what a
//!   read in their own delay slot sees. Not cancelling is the conservative
//!   choice; see the open questions in `docs/notes/CPU.md`.
//!
//! Instructions cost a flat cycle each; see `docs/notes/TIMING.md` for what
//! that still owes and why it is blocked on the instruction cache.

use crate::bus::Bus;
use crate::cop0::{Cop0, Exception};
use crate::gte::Gte;

/// A decoded-on-demand 32-bit instruction word.
#[derive(Clone, Copy)]
pub struct Instruction(pub u32);

impl Instruction {
    /// Bits 31..26: the primary opcode.
    #[inline(always)]
    fn opcode(self) -> u32 {
        self.0 >> 26
    }
    /// Bits 5..0: the SPECIAL sub-opcode.
    #[inline(always)]
    fn funct(self) -> u32 {
        self.0 & 0x3F
    }
    /// Bits 25..21: `rs`, and the coprocessor sub-opcode field.
    #[inline(always)]
    fn s(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
    /// Bits 20..16: `rt`.
    #[inline(always)]
    fn t(self) -> u32 {
        (self.0 >> 16) & 0x1F
    }
    /// Bits 15..11: `rd`.
    #[inline(always)]
    fn d(self) -> u32 {
        (self.0 >> 11) & 0x1F
    }
    /// Bits 10..6: the shift amount.
    #[inline(always)]
    fn shamt(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }
    /// Bits 15..0, zero-extended.
    #[inline(always)]
    fn imm(self) -> u32 {
        self.0 & 0xFFFF
    }
    /// Bits 15..0, sign-extended.
    #[inline(always)]
    fn imm_se(self) -> u32 {
        (self.0 & 0xFFFF) as i16 as u32
    }
    /// Bits 25..0: the jump target.
    #[inline(always)]
    fn imm_jump(self) -> u32 {
        self.0 & 0x03FF_FFFF
    }
}

#[derive(Clone)]
pub struct Cpu {
    /// The architectural register file, as the *current* instruction sees it.
    pub(crate) regs: [u32; 32],
    /// The register file as it will be after this instruction. See the load
    /// delay note above.
    pub(crate) out_regs: [u32; 32],

    pub hi: u32,
    pub lo: u32,

    /// Address of the next instruction to fetch.
    pub pc: u32,
    /// What `pc` becomes after this instruction: the branch delay slot.
    pub next_pc: u32,
    /// Address of the instruction currently executing. EPC is taken from here.
    pub current_pc: u32,

    /// The load waiting on the delay slot: `(register, value)`. Register 0 is
    /// the idle marker, which is safe because writes to `$zero` are discarded.
    pub(crate) load: (u8, u32),
    /// The register the in-flight load was committed into at the start of this
    /// instruction, and the value it displaced.
    ///
    /// Deliberately **not** serialized: both are set at the top of every
    /// `step` before anything reads them, and save states are only ever taken
    /// between instructions, so they carry no state across one.
    cancel_reg: u8,
    cancel_val: u32,
    /// The register this instruction wrote through [`Self::set_reg`], or 0.
    ///
    /// With `cancel_reg` it names every register `out_regs` can differ from
    /// `regs` in at the end of a step (no instruction writes two), so those
    /// two are all that need copying back. Copying the whole file was 16% of
    /// the time on an ARM tablet. Transient like `cancel_*`, not serialized.
    written: u8,
    /// The instruction just executed took a branch.
    pub(crate) branch: bool,
    /// The instruction currently executing sits in a branch delay slot.
    pub(crate) delay_slot: bool,

    pub cop0: Cop0,
    pub gte: Gte,

    /// Instructions retired. One per cycle until a timing model exists. The
    /// name is deliberately `cycles` so the rest of the system can be written
    /// against the eventual meaning.
    pub cycles: u64,
}

/// Where the R3000A starts: the uncached (KSEG1) view of the BIOS ROM.
pub const RESET_VECTOR: u32 = 0xBFC0_0000;

/// Master-clock cycles charged per instruction.
///
/// One, for now, which is honest rather than right: real instructions cost
/// between roughly 1 and 40 cycles depending on where they fetch from and what
/// they touch. Everything downstream is written against the master clock rather
/// than against this constant, so making it real later is a change to one
/// function and not to the shape of the system.
pub const CYCLES_PER_INSTRUCTION: u64 = 1;

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            // Hardware leaves the register file undefined at reset; zero is the
            // deterministic choice, and determinism is a save-state requirement.
            regs: [0; 32],
            out_regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: RESET_VECTOR,
            next_pc: RESET_VECTOR.wrapping_add(4),
            current_pc: 0,
            load: (0, 0),
            cancel_reg: 0,
            cancel_val: 0,
            written: 0,
            branch: false,
            delay_slot: false,
            cop0: Cop0::new(),
            gte: Gte::new(),
            cycles: 0,
        }
    }

    /// Read a register as this instruction sees it.
    #[inline(always)]
    pub fn reg(&self, index: u32) -> u32 {
        // Register fields are five bits; the mask lets the compiler drop the
        // bounds check on every operand read.
        self.regs[index as usize & 31]
    }

    /// Schedule a register write for the end of this instruction.
    #[inline(always)]
    fn set_reg(&mut self, index: u32, val: u32) {
        self.out_regs[index as usize & 31] = val;
        self.out_regs[0] = 0;
        self.written = index as u8;
    }

    /// Force a register immediately, bypassing the shadow file. Only for the
    /// host (EXE sideload, debugger), never for emulated instructions.
    pub fn force_reg(&mut self, index: u32, val: u32) {
        self.regs[index as usize] = val;
        self.out_regs[index as usize] = val;
        self.regs[0] = 0;
        self.out_regs[0] = 0;
    }

    /// Full register file, for the debugger and the serializer.
    pub fn regs(&self) -> &[u32; 32] {
        &self.regs
    }
    pub fn out_regs(&self) -> &[u32; 32] {
        &self.out_regs
    }
    pub fn pending_load(&self) -> (u8, u32) {
        self.load
    }
    pub fn in_delay_slot(&self) -> bool {
        self.delay_slot
    }
    pub fn branch_taken(&self) -> bool {
        self.branch
    }

    /// Everything an instruction can read or leave behind, compared. The
    /// retired count is not state, and `cancel_*` are rewritten at the top of
    /// every step before anything reads them.
    pub(crate) fn same_state(&self, other: &Cpu) -> bool {
        self.regs == other.regs
            && self.out_regs == other.out_regs
            && self.hi == other.hi
            && self.lo == other.lo
            && self.pc == other.pc
            && self.next_pc == other.next_pc
            && self.load == other.load
            && self.branch == other.branch
            && self.delay_slot == other.delay_slot
            && self.cop0 == other.cop0
            && self.gte == other.gte
    }

    /// Jump to `pc`, discarding any in-flight branch. For EXE sideload.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.branch = false;
        self.delay_slot = false;
        self.load = (0, 0);
    }

    /// Land the load waiting in the delay slot.
    ///
    /// Also called on the way into an exception. The handler has to start with
    /// an empty load-delay slot, or a load issued just before the interrupt
    /// arrives lands on the handler's *first* instruction instead. The register
    /// ends up with the right value either way, so this only shifts by one
    /// instruction the point at which it becomes visible, which is exactly the
    /// kind of thing that is invisible until the BIOS exception handler (which
    /// has load-delay-slot code of its own) reads the wrong register.
    #[inline]
    fn commit_pending_load(&mut self) {
        let (reg, val) = self.load;
        self.cancel_reg = reg;
        self.cancel_val = self.out_regs[reg as usize];
        self.set_reg(reg as u32, val);
        self.load = (0, 0);
    }

    /// Issue a load into the delay slot.
    ///
    /// If a load to the **same register** was already in flight, its value is
    /// discarded: hardware never makes the first one architecturally visible.
    ///
    /// ```asm
    /// lw   $1, (a)
    /// lw   $1, (b)
    /// move $2, $1     ; $2 is the value $1 held before BOTH loads
    /// ```
    ///
    /// The in-flight value has already been written into `out_regs` by
    /// [`Self::commit_pending_load`] at the top of this instruction, so
    /// cancelling means putting back what it displaced.
    ///
    /// `LWL`/`LWR` must **not** go through here. They are the deliberate
    /// exception: a chained pair has to see the pending load's value in order
    /// to merge with it, so they read `out_regs` and assign `load` directly.
    #[inline]
    fn set_load(&mut self, reg: u32, val: u32) {
        if reg != 0 && reg as u8 == self.cancel_reg {
            self.out_regs[reg as usize] = self.cancel_val;
        }
        self.load = (reg as u8, val);
    }

    /// Is the instruction about to run a GTE command (`COP2 imm25`)?
    ///
    /// Hardware **executes** a GTE command and only then takes a pending
    /// interrupt, with `EPC` pointing at the command; the BIOS handler knows
    /// this and steps `EPC` past it. Taking the interrupt first instead means
    /// the command is skipped on the way in and skipped again by the handler,
    /// so it never runs at all. Every interrupt that happens to land on one
    /// silently drops a geometry operation, which is why Crash Bandicoot and
    /// Spyro come out misshapen on cores that get this wrong.
    ///
    /// Deferring by one instruction is the cheaper of the two fixes the
    /// reference offers, and the one that does not need the exception path to
    /// know about coprocessors.
    fn pending_is_gte_command(&self, bus: &mut Bus) -> bool {
        if !self.pc.is_multiple_of(4) {
            return false;
        }
        // COP2 opcode (0x12) with bit 25 set: the command form rather than a
        // register move.
        bus.load32(self.pc) & 0xFE00_0000 == 0x4A00_0000
    }

    /// Execute one instruction.
    pub fn step(&mut self, bus: &mut Bus) {
        // The interrupt controller drives the single external line into Cause
        // bit 10; the check is a level test, so it is refreshed every step.
        self.cop0.set_external_irq(bus.irq_pending());
        if self.cop0.interrupt_ready() && !self.pending_is_gte_command(bus) {
            // An interrupt is taken *at* the next instruction boundary, so
            // current_pc has to name the instruction that will be re-run.
            self.current_pc = self.pc;
            self.delay_slot = self.branch;
            self.commit_pending_load();
            self.exception(Exception::Interrupt);
            self.regs = self.out_regs;
            self.retire(bus);
            return;
        }

        self.current_pc = self.pc;

        if !self.current_pc.is_multiple_of(4) {
            self.delay_slot = self.branch;
            self.commit_pending_load();
            self.cop0.bad_vaddr = self.current_pc;
            self.exception(Exception::AddressErrorLoad);
            self.regs = self.out_regs;
            self.retire(bus);
            return;
        }

        let instruction = Instruction(bus.load32(self.current_pc));

        // Advance the delay-slot machinery before executing: a branch taken by
        // *this* instruction must flag the *next* one, not itself.
        self.delay_slot = self.branch;
        self.branch = false;

        self.pc = self.next_pc;
        self.next_pc = self.pc.wrapping_add(4);

        // The load from the previous instruction lands now, before this
        // instruction runs, so this instruction still reads the old value out
        // of `regs`, and an explicit write here overwrites the arriving load.
        self.commit_pending_load();
        self.written = 0;

        self.execute(instruction, bus);

        // The same as `self.regs = self.out_regs`: see `written`.
        let c = self.cancel_reg as usize & 31;
        self.regs[c] = self.out_regs[c];
        let w = self.written as usize & 31;
        self.regs[w] = self.out_regs[w];
        debug_assert!(self.regs == self.out_regs);
        self.retire(bus);
    }

    /// End of an instruction: commit the shadow register file's counter and
    /// advance the master clock, which is what lets the timed devices run.
    ///
    /// The cost is a flat [`CYCLES_PER_INSTRUCTION`] for now. That is the axis
    /// this does *not* model yet, and `docs/notes/TIMING.md` says what it will
    /// take: memory access penalties are meaningless until there is an I-cache,
    /// because instruction fetch would dominate them.
    #[inline]
    fn retire(&mut self, bus: &mut Bus) {
        self.cycles = self.cycles.wrapping_add(1);
        bus.tick(CYCLES_PER_INSTRUCTION);
    }

    fn execute(&mut self, instr: Instruction, bus: &mut Bus) {
        match instr.opcode() {
            0x00 => self.op_special(instr, bus),
            0x01 => self.op_bcondz(instr),
            0x02 => self.op_j(instr),
            0x03 => self.op_jal(instr),
            0x04 => self.op_beq(instr),
            0x05 => self.op_bne(instr),
            0x06 => self.op_blez(instr),
            0x07 => self.op_bgtz(instr),
            0x08 => self.op_addi(instr),
            0x09 => self.op_addiu(instr),
            0x0A => self.op_slti(instr),
            0x0B => self.op_sltiu(instr),
            0x0C => self.op_andi(instr),
            0x0D => self.op_ori(instr),
            0x0E => self.op_xori(instr),
            0x0F => self.op_lui(instr),
            0x10 => self.op_cop0(instr),
            0x11 => self.op_absent_cop(1),
            0x12 => self.op_cop2(instr),
            0x13 => self.op_absent_cop(3),
            0x20 => self.op_lb(instr, bus),
            0x21 => self.op_lh(instr, bus),
            0x22 => self.op_lwl(instr, bus),
            0x23 => self.op_lw(instr, bus),
            0x24 => self.op_lbu(instr, bus),
            0x25 => self.op_lhu(instr, bus),
            0x26 => self.op_lwr(instr, bus),
            0x28 => self.op_sb(instr, bus),
            0x29 => self.op_sh(instr, bus),
            0x2A => self.op_swl(instr, bus),
            0x2B => self.op_sw(instr, bus),
            0x2E => self.op_swr(instr, bus),
            // LWC0/1/3 and SWC0/1/3. No such coprocessor is fitted, so the
            // only question these can answer is the usability one.
            0x30 | 0x38 => self.op_absent_cop(0),
            0x31 | 0x39 => self.op_absent_cop(1),
            0x33 | 0x3B => self.op_absent_cop(3),
            0x32 => self.op_lwc2(instr, bus),
            0x3A => self.op_swc2(instr, bus),
            _ => self.exception(Exception::IllegalInstruction),
        }
    }

    fn op_special(&mut self, instr: Instruction, bus: &mut Bus) {
        let _ = bus;
        match instr.funct() {
            0x00 => self.op_sll(instr),
            0x02 => self.op_srl(instr),
            0x03 => self.op_sra(instr),
            0x04 => self.op_sllv(instr),
            0x06 => self.op_srlv(instr),
            0x07 => self.op_srav(instr),
            0x08 => self.op_jr(instr),
            0x09 => self.op_jalr(instr),
            0x0C => self.exception(Exception::SysCall),
            0x0D => self.exception(Exception::Break),
            0x10 => self.op_mfhi(instr),
            0x11 => self.op_mthi(instr),
            0x12 => self.op_mflo(instr),
            0x13 => self.op_mtlo(instr),
            0x18 => self.op_mult(instr),
            0x19 => self.op_multu(instr),
            0x1A => self.op_div(instr),
            0x1B => self.op_divu(instr),
            0x20 => self.op_add(instr),
            0x21 => self.op_addu(instr),
            0x22 => self.op_sub(instr),
            0x23 => self.op_subu(instr),
            0x24 => self.op_and(instr),
            0x25 => self.op_or(instr),
            0x26 => self.op_xor(instr),
            0x27 => self.op_nor(instr),
            0x2A => self.op_slt(instr),
            0x2B => self.op_sltu(instr),
            _ => self.exception(Exception::IllegalInstruction),
        }
    }

    // ---- branches -------------------------------------------------------

    /// Retarget the delay slot's successor. `pc` already points at the delay
    /// slot when this runs, which is exactly the base MIPS specifies.
    #[inline(always)]
    fn branch_to(&mut self, offset: u32) {
        self.next_pc = self.pc.wrapping_add(offset << 2);
        self.branch = true;
    }

    fn op_j(&mut self, instr: Instruction) {
        self.next_pc = (self.pc & 0xF000_0000) | (instr.imm_jump() << 2);
        self.branch = true;
    }

    fn op_jal(&mut self, instr: Instruction) {
        let ra = self.next_pc;
        self.set_reg(31, ra);
        self.op_j(instr);
    }

    fn op_jr(&mut self, instr: Instruction) {
        self.next_pc = self.reg(instr.s());
        self.branch = true;
    }

    fn op_jalr(&mut self, instr: Instruction) {
        let ra = self.next_pc;
        // Read rs first: `jalr $ra, $ra` is legal and must use the old value.
        self.next_pc = self.reg(instr.s());
        self.set_reg(instr.d(), ra);
        self.branch = true;
    }

    fn op_beq(&mut self, instr: Instruction) {
        if self.reg(instr.s()) == self.reg(instr.t()) {
            self.branch_to(instr.imm_se());
        }
    }

    fn op_bne(&mut self, instr: Instruction) {
        if self.reg(instr.s()) != self.reg(instr.t()) {
            self.branch_to(instr.imm_se());
        }
    }

    fn op_blez(&mut self, instr: Instruction) {
        if (self.reg(instr.s()) as i32) <= 0 {
            self.branch_to(instr.imm_se());
        }
    }

    fn op_bgtz(&mut self, instr: Instruction) {
        if (self.reg(instr.s()) as i32) > 0 {
            self.branch_to(instr.imm_se());
        }
    }

    /// BLTZ / BGEZ / BLTZAL / BGEZAL share opcode 0x01 and are told apart by
    /// bits of `rt`: bit 16 picks the comparison, bits 20..17 == 0b1000 makes
    /// it a linking form. The link happens *whether or not the branch is
    /// taken*, and `$ra` is written even when `rs` is `$ra` itself.
    fn op_bcondz(&mut self, instr: Instruction) {
        let is_bgez = instr.t() & 1;
        let is_link = (instr.t() >> 1) & 0xF == 8;

        let v = self.reg(instr.s()) as i32;
        let test = ((v < 0) as u32) ^ is_bgez;

        if is_link {
            let ra = self.next_pc;
            self.set_reg(31, ra);
        }
        if test != 0 {
            self.branch_to(instr.imm_se());
        }
    }

    // ---- arithmetic / logic --------------------------------------------

    fn op_addi(&mut self, instr: Instruction) {
        let s = self.reg(instr.s()) as i32;
        match s.checked_add(instr.imm_se() as i32) {
            Some(v) => self.set_reg(instr.t(), v as u32),
            None => self.exception(Exception::Overflow),
        }
    }

    fn op_addiu(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()).wrapping_add(instr.imm_se());
        self.set_reg(instr.t(), v);
    }

    fn op_slti(&mut self, instr: Instruction) {
        let v = ((self.reg(instr.s()) as i32) < (instr.imm_se() as i32)) as u32;
        self.set_reg(instr.t(), v);
    }

    fn op_sltiu(&mut self, instr: Instruction) {
        let v = (self.reg(instr.s()) < instr.imm_se()) as u32;
        self.set_reg(instr.t(), v);
    }

    fn op_andi(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) & instr.imm();
        self.set_reg(instr.t(), v);
    }

    fn op_ori(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) | instr.imm();
        self.set_reg(instr.t(), v);
    }

    fn op_xori(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) ^ instr.imm();
        self.set_reg(instr.t(), v);
    }

    fn op_lui(&mut self, instr: Instruction) {
        self.set_reg(instr.t(), instr.imm() << 16);
    }

    fn op_add(&mut self, instr: Instruction) {
        let s = self.reg(instr.s()) as i32;
        let t = self.reg(instr.t()) as i32;
        match s.checked_add(t) {
            Some(v) => self.set_reg(instr.d(), v as u32),
            None => self.exception(Exception::Overflow),
        }
    }

    fn op_addu(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()).wrapping_add(self.reg(instr.t()));
        self.set_reg(instr.d(), v);
    }

    fn op_sub(&mut self, instr: Instruction) {
        let s = self.reg(instr.s()) as i32;
        let t = self.reg(instr.t()) as i32;
        match s.checked_sub(t) {
            Some(v) => self.set_reg(instr.d(), v as u32),
            None => self.exception(Exception::Overflow),
        }
    }

    fn op_subu(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()).wrapping_sub(self.reg(instr.t()));
        self.set_reg(instr.d(), v);
    }

    fn op_and(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) & self.reg(instr.t());
        self.set_reg(instr.d(), v);
    }

    fn op_or(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) | self.reg(instr.t());
        self.set_reg(instr.d(), v);
    }

    fn op_xor(&mut self, instr: Instruction) {
        let v = self.reg(instr.s()) ^ self.reg(instr.t());
        self.set_reg(instr.d(), v);
    }

    fn op_nor(&mut self, instr: Instruction) {
        let v = !(self.reg(instr.s()) | self.reg(instr.t()));
        self.set_reg(instr.d(), v);
    }

    fn op_slt(&mut self, instr: Instruction) {
        let v = ((self.reg(instr.s()) as i32) < (self.reg(instr.t()) as i32)) as u32;
        self.set_reg(instr.d(), v);
    }

    fn op_sltu(&mut self, instr: Instruction) {
        let v = (self.reg(instr.s()) < self.reg(instr.t())) as u32;
        self.set_reg(instr.d(), v);
    }

    // ---- shifts ---------------------------------------------------------

    fn op_sll(&mut self, instr: Instruction) {
        let v = self.reg(instr.t()) << instr.shamt();
        self.set_reg(instr.d(), v);
    }

    fn op_srl(&mut self, instr: Instruction) {
        let v = self.reg(instr.t()) >> instr.shamt();
        self.set_reg(instr.d(), v);
    }

    fn op_sra(&mut self, instr: Instruction) {
        let v = (self.reg(instr.t()) as i32) >> instr.shamt();
        self.set_reg(instr.d(), v as u32);
    }

    fn op_sllv(&mut self, instr: Instruction) {
        // Only the low five bits of rs count; the rest are ignored, not masked
        // into a 32-shift (which would be UB in Rust anyway).
        let v = self.reg(instr.t()) << (self.reg(instr.s()) & 0x1F);
        self.set_reg(instr.d(), v);
    }

    fn op_srlv(&mut self, instr: Instruction) {
        let v = self.reg(instr.t()) >> (self.reg(instr.s()) & 0x1F);
        self.set_reg(instr.d(), v);
    }

    fn op_srav(&mut self, instr: Instruction) {
        let v = (self.reg(instr.t()) as i32) >> (self.reg(instr.s()) & 0x1F);
        self.set_reg(instr.d(), v as u32);
    }

    // ---- HI / LO --------------------------------------------------------

    fn op_mfhi(&mut self, instr: Instruction) {
        let hi = self.hi;
        self.set_reg(instr.d(), hi);
    }

    fn op_mthi(&mut self, instr: Instruction) {
        self.hi = self.reg(instr.s());
    }

    fn op_mflo(&mut self, instr: Instruction) {
        let lo = self.lo;
        self.set_reg(instr.d(), lo);
    }

    fn op_mtlo(&mut self, instr: Instruction) {
        self.lo = self.reg(instr.s());
    }

    fn op_mult(&mut self, instr: Instruction) {
        let a = self.reg(instr.s()) as i32 as i64;
        let b = self.reg(instr.t()) as i32 as i64;
        let r = (a * b) as u64;
        self.hi = (r >> 32) as u32;
        self.lo = r as u32;
    }

    fn op_multu(&mut self, instr: Instruction) {
        let a = self.reg(instr.s()) as u64;
        let b = self.reg(instr.t()) as u64;
        let r = a * b;
        self.hi = (r >> 32) as u32;
        self.lo = r as u32;
    }

    /// Signed divide. The R3000A does not trap on divide-by-zero or on the
    /// `INT_MIN / -1` overflow; it returns fixed junk, and code that divides by
    /// a zero it never checks depends on exactly which junk.
    fn op_div(&mut self, instr: Instruction) {
        let n = self.reg(instr.s()) as i32;
        let d = self.reg(instr.t()) as i32;

        if d == 0 {
            self.hi = n as u32;
            self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
        } else if n as u32 == 0x8000_0000 && d == -1 {
            self.hi = 0;
            self.lo = 0x8000_0000;
        } else {
            self.hi = (n % d) as u32;
            self.lo = (n / d) as u32;
        }
    }

    fn op_divu(&mut self, instr: Instruction) {
        let n = self.reg(instr.s());
        let d = self.reg(instr.t());

        if d == 0 {
            self.hi = n;
            self.lo = 0xFFFF_FFFF;
        } else {
            self.hi = n % d;
            self.lo = n / d;
        }
    }

    // ---- coprocessors ---------------------------------------------------

    /// Status bits 31..28 are CU3..CU0: which coprocessors software has
    /// declared usable.
    #[inline(always)]
    fn cop_usable(&self, n: u32) -> bool {
        self.cop0.sr & (1 << (28 + n)) != 0
    }

    /// An instruction for a coprocessor that is not fitted (COP1, COP3, and the
    /// COP0 load/store forms).
    ///
    /// Usability is decided by the Status CU bit **alone**, not by whether the
    /// hardware exists: with the bit set, the instruction is accepted and does
    /// nothing observable; with it clear, it traps. `cpu/cop` in the ps1-tests
    /// suite is what settled this, and it is the reason five of its cases used
    /// to fail here.
    fn op_absent_cop(&mut self, n: u32) {
        if !self.cop_usable(n) {
            self.exception(Exception::CoprocessorError);
        }
    }

    fn op_cop0(&mut self, instr: Instruction) {
        match instr.s() {
            // MFC0. Goes through the load delay slot like a memory load does.
            0x00 => {
                let v = self.cop0.read(instr.d());
                self.set_load(instr.t(), v);
            }
            // MTC0
            0x04 => {
                let v = self.reg(instr.t());
                self.cop0.write(instr.d(), v);
            }
            // RFE. The rest of the CO field is a don't-care on this part, but
            // funct must be 0x10, and anything else here is not an RFE.
            0x10 if instr.funct() == 0x10 => self.cop0.return_from_exception(),
            // An unrecognised COP0 sub-opcode does *not* trap. `cpu/cop`'s
            // testCop0InvalidOpcode checks exactly this, and a reserved-
            // instruction exception here is wrong.
            _ => {}
        }
    }

    fn op_cop2(&mut self, instr: Instruction) {
        // COP2 is only usable with Status CU2 set. The BIOS sets it early; a
        // game that hits this without setting it really does take the trap.
        if self.cop0.sr & (1 << 30) == 0 {
            self.exception(Exception::CoprocessorError);
            return;
        }

        match instr.s() {
            0x00 => {
                // MFC2: load-delayed, same as MFC0.
                let v = self.gte.read_data(instr.d());
                self.set_load(instr.t(), v);
            }
            0x02 => {
                // CFC2
                let v = self.gte.read_control(instr.d());
                self.set_load(instr.t(), v);
            }
            0x04 => {
                let v = self.reg(instr.t());
                self.gte.write_data(instr.d(), v);
            }
            0x06 => {
                let v = self.reg(instr.t());
                self.gte.write_control(instr.d(), v);
            }
            // Bit 25 set: a GTE command rather than a register move.
            s if s & 0x10 != 0 => self.gte.command(instr.0 & 0x1FF_FFFF),
            _ => self.exception(Exception::IllegalInstruction),
        }
    }

    fn op_lwc2(&mut self, instr: Instruction, bus: &mut Bus) {
        if self.cop0.sr & (1 << 30) == 0 {
            self.exception(Exception::CoprocessorError);
            return;
        }
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(4) {
            self.address_error(Exception::AddressErrorLoad, addr);
            return;
        }
        let v = bus.load32(addr);
        self.gte.write_data(instr.t(), v);
    }

    fn op_swc2(&mut self, instr: Instruction, bus: &mut Bus) {
        if self.cop0.sr & (1 << 30) == 0 {
            self.exception(Exception::CoprocessorError);
            return;
        }
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(4) {
            self.address_error(Exception::AddressErrorStore, addr);
            return;
        }
        let v = self.gte.read_data(instr.t());
        bus.store32(addr, v);
    }

    // ---- loads ----------------------------------------------------------

    fn op_lb(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let v = bus.load8(addr) as i8 as u32;
        self.set_load(instr.t(), v);
    }

    fn op_lbu(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let v = bus.load8(addr) as u32;
        self.set_load(instr.t(), v);
    }

    fn op_lh(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(2) {
            self.address_error(Exception::AddressErrorLoad, addr);
            return;
        }
        let v = bus.load16(addr) as i16 as u32;
        self.set_load(instr.t(), v);
    }

    fn op_lhu(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(2) {
            self.address_error(Exception::AddressErrorLoad, addr);
            return;
        }
        let v = bus.load16(addr) as u32;
        self.set_load(instr.t(), v);
    }

    fn op_lw(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(4) {
            self.address_error(Exception::AddressErrorLoad, addr);
            return;
        }
        let v = bus.load32(addr);
        self.set_load(instr.t(), v);
    }

    /// `LWL`/`LWR` are the unaligned-load pair, and they are the one place the
    /// load delay slot is deliberately bypassed: the second of the pair must
    /// see the first's partial result, so the merge reads `out_regs`, not
    /// `regs`.
    fn op_lwl(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let cur = self.out_regs[instr.t() as usize];
        let aligned = bus.load32(addr & !3);

        let v = match addr & 3 {
            0 => (cur & 0x00FF_FFFF) | (aligned << 24),
            1 => (cur & 0x0000_FFFF) | (aligned << 16),
            2 => (cur & 0x0000_00FF) | (aligned << 8),
            _ => aligned,
        };
        self.load = (instr.t() as u8, v);
    }

    fn op_lwr(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let cur = self.out_regs[instr.t() as usize];
        let aligned = bus.load32(addr & !3);

        let v = match addr & 3 {
            0 => aligned,
            1 => (cur & 0xFF00_0000) | (aligned >> 8),
            2 => (cur & 0xFFFF_0000) | (aligned >> 16),
            _ => (cur & 0xFFFF_FF00) | (aligned >> 24),
        };
        self.load = (instr.t() as u8, v);
    }

    // ---- stores ---------------------------------------------------------

    /// Every store funnels through here so the cache-isolation check exists in
    /// exactly one place. With Status Isc set the write goes to the I-cache,
    /// which is not modelled, so dropping it is right, writing RAM is not.
    #[inline(always)]
    fn store(&mut self, bus: &mut Bus, addr: u32, width: u32, val: u32) {
        if self.cop0.cache_isolated() {
            return;
        }
        match width {
            4 => bus.store32(addr, val),
            2 => bus.store16(addr, val as u16),
            _ => bus.store8(addr, val as u8),
        }
    }

    fn op_sb(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let v = self.reg(instr.t());
        self.store(bus, addr, 1, v);
    }

    fn op_sh(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(2) {
            self.address_error(Exception::AddressErrorStore, addr);
            return;
        }
        let v = self.reg(instr.t());
        self.store(bus, addr, 2, v);
    }

    fn op_sw(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        if !addr.is_multiple_of(4) {
            self.address_error(Exception::AddressErrorStore, addr);
            return;
        }
        let v = self.reg(instr.t());
        self.store(bus, addr, 4, v);
    }

    fn op_swl(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let v = self.reg(instr.t());
        let aligned_addr = addr & !3;
        let cur = bus.load32(aligned_addr);

        let merged = match addr & 3 {
            0 => (cur & 0xFFFF_FF00) | (v >> 24),
            1 => (cur & 0xFFFF_0000) | (v >> 16),
            2 => (cur & 0xFF00_0000) | (v >> 8),
            _ => v,
        };
        self.store(bus, aligned_addr, 4, merged);
    }

    fn op_swr(&mut self, instr: Instruction, bus: &mut Bus) {
        let addr = self.reg(instr.s()).wrapping_add(instr.imm_se());
        let v = self.reg(instr.t());
        let aligned_addr = addr & !3;
        let cur = bus.load32(aligned_addr);

        let merged = match addr & 3 {
            0 => v,
            1 => (cur & 0x0000_00FF) | (v << 8),
            2 => (cur & 0x0000_FFFF) | (v << 16),
            _ => (cur & 0x00FF_FFFF) | (v << 24),
        };
        self.store(bus, aligned_addr, 4, merged);
    }

    // ---- exceptions -----------------------------------------------------

    fn address_error(&mut self, kind: Exception, addr: u32) {
        self.cop0.bad_vaddr = addr;
        self.exception(kind);
    }

    fn exception(&mut self, cause: Exception) {
        self.cop0
            .enter_exception(cause, self.current_pc, self.delay_slot);

        self.pc = self.cop0.exception_handler();
        self.next_pc = self.pc.wrapping_add(4);
        // The handler's first instruction is not in a delay slot, whatever the
        // faulting instruction was.
        self.branch = false;
        // Belt and braces: every load path returns before setting `load` when
        // it faults, so this should already be empty. Enforcing the invariant
        // is cheaper than relying on that staying true.
        self.load = (0, 0);
    }
}
