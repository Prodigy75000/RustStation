// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Hand-assembled tests for the R3000A behaviours that are easy to get wrong
//! and hard to notice: the two delay slots, the unaligned load/store pairs,
//! divide-by-zero's fixed junk, overflow trapping, and cache isolation.
//!
//! These are *not* a substitute for a conformance suite. They are the floor
//! that keeps a refactor honest between suite runs. Programs are written into
//! a synthetic BIOS image so no copyrighted dump is needed.

mod common;
use common::*;

// ---- tests -----------------------------------------------------------------

/// The instruction after a taken branch runs anyway, and the branch target is
/// relative to the *delay slot*, not to the branch.
#[test]
fn branch_delay_slot_executes() {
    // beq $0, $0, +2   (skips the instruction after the delay slot)
    // addiu $1, $0, 7  <- delay slot, must run
    // addiu $2, $0, 9  <- must be skipped
    // addiu $3, $0, 11 <- branch lands here
    let mut psx = machine(&[
        beq(0, 0, 2),
        addiu(1, 0, 7),
        addiu(2, 0, 9),
        addiu(3, 0, 11),
    ]);
    steps(&mut psx, 3);

    assert_eq!(psx.cpu.reg(1), 7, "delay slot did not execute");
    assert_eq!(
        psx.cpu.reg(2),
        0,
        "instruction after the delay slot was not skipped"
    );
    assert_eq!(psx.cpu.reg(3), 11, "branch landed in the wrong place");
}

/// `jal` links to the instruction *after* the delay slot.
#[test]
fn jal_links_past_the_delay_slot() {
    let target = RESET + 0x40;
    let mut psx = machine(&[jal(target), addiu(1, 0, 7)]);
    steps(&mut psx, 2);

    assert_eq!(psx.cpu.reg(31), RESET + 8);
    assert_eq!(psx.cpu.reg(1), 7);
    assert_eq!(psx.cpu.pc, target);
}

/// A load's result is invisible to the very next instruction.
#[test]
fn load_delay_slot_hides_the_result() {
    let mut psx = machine(&[
        addiu(1, 0, 0x0AAA), // r1 = 0xAAA
        addiu(4, 0, 0),      // r4 = 0 (RAM address 0)
        sw(1, 0, 4),         // [0] = 0xAAA
        addiu(1, 0, 0x0BBB), // r1 = 0xBBB
        lw(1, 0, 4),         // r1 <- 0xAAA, but not yet
        addiu(2, 1, 0),      // delay slot: sees the OLD r1
        addiu(3, 1, 0),      // sees the loaded value
    ]);
    steps(&mut psx, 7);

    assert_eq!(
        psx.cpu.reg(2),
        0x0BBB,
        "delay slot saw the loaded value too early"
    );
    assert_eq!(psx.cpu.reg(3), 0x0AAA, "loaded value never landed");
}

/// An explicit write in the load delay slot beats the arriving load. This is
/// the one piece of delay-slot behaviour taken on reasoning rather than a
/// documented statement. If a conformance suite disagrees, this test is where
/// the disagreement gets recorded.
#[test]
fn explicit_write_in_the_delay_slot_beats_the_load() {
    let mut psx = machine(&[
        addiu(1, 0, 0x0AAA),
        addiu(4, 0, 0),
        sw(1, 0, 4),
        lw(1, 0, 4),         // r1 <- 0xAAA, pending
        addiu(1, 0, 0x0CCC), // ... but this writes r1 in the same slot
        nop(),
    ]);
    steps(&mut psx, 6);

    assert_eq!(psx.cpu.reg(1), 0x0CCC);
}

/// A second plain load to the same register discards the first, which is
/// therefore never architecturally visible.
///
/// ```asm
/// lw   $1, (a)
/// lw   $1, (b)
/// move $2, $1     ; the value $1 held before BOTH loads
/// ```
///
/// A silent divergence: nothing crashes, values are just subtly wrong, which is
/// why it is worth a test rather than waiting to notice.
#[test]
fn a_second_load_cancels_the_first() {
    let mut psx = machine(&[
        addiu(4, 0, 0),  // $4 = 0
        addiu(7, 0, 16), // $7 = 16
        lui(5, 0x0AAA),
        sw(5, 0, 4), // [0]  = 0x0AAA0000
        lui(8, 0x0BBB),
        sw(8, 0, 7),         // [16] = 0x0BBB0000
        addiu(1, 0, 0x0123), // $1 = 0x123, the value that must survive
        lw(1, 0, 4),         // in flight: 0x0AAA0000
        lw(1, 0, 7),         // cancels it; in flight: 0x0BBB0000
        addiu(2, 1, 0),      // $2 = $1
        nop(),
    ]);
    steps(&mut psx, 10);

    assert_eq!(
        psx.cpu.reg(2),
        0x0123,
        "the first load's value became visible; it should have been discarded"
    );

    steps(&mut psx, 1);
    assert_eq!(
        psx.cpu.reg(1),
        0x0BBB_0000,
        "the second load should still land normally"
    );
}

/// The counterpart to the rule above: `lwl` **merges** with a pending load, so
/// the in-flight value reaches the merge rather than being thrown away.
///
/// Note what this does *not* prove. `op_lwl` samples `out_regs` before any
/// cancellation would apply, so routing `lwl` through `Cpu::set_load` as well
/// leaves this test green: the two only differ for a read in `lwl`'s own delay
/// slot. That case is untested and unsettled; see
/// `lwl_delay_slot_sees_the_cancelled_load` below.
#[test]
fn lwl_merges_with_a_pending_load() {
    let mut psx = machine(&[
        addiu(4, 0, 0),
        lui(5, 0x1122),
        ori(5, 5, 0x3344),
        sw(5, 0, 4), // [0] = 0x11223344
        addiu(6, 0, 8),
        lui(7, 0xAABB),
        ori(7, 7, 0xCCDD),
        sw(7, 0, 6), // [8] = 0xAABBCCDD
        lui(1, 0x9999),
        ori(1, 1, 0x9999), // $1 = 0x99999999
        lw(1, 0, 4),       // in flight: 0x11223344
        lwl(1, 8, 4),      // merges the top byte of [8] into it
        nop(),
        nop(),
    ]);
    steps(&mut psx, 14);

    // Top byte from [8], the rest from the load that was still in flight. If
    // the in-flight value had not reached the merge at all, the low bytes would
    // read 0x999999.
    assert_eq!(psx.cpu.reg(1), 0xDD22_3344);
}

/// **Unsettled.** Does an `lwl` that merges with a pending load *also* cancel
/// it, the way a second plain load does?
///
/// The two behaviours differ only in what a read inside `lwl`'s own delay slot
/// sees: the pending load's value (no cancel, what this core does) or the value
/// from before it (cancel). The reference set documents that chained pairs see
/// each other's results and that plain loads cancel, but not this crossing of
/// the two, so the current behaviour is the conservative guess rather than a
/// known fact.
///
/// Written down as an ignored test per the convention in `docs/ref/README.md`:
/// do not assert an uncertain behaviour. Un-ignore it once hardware settles it,
/// with whichever expectation turns out to be right.
#[test]
#[ignore = "unsettled: see docs/notes/CPU.md open questions"]
fn lwl_delay_slot_sees_the_cancelled_load() {
    let mut psx = machine(&[
        addiu(4, 0, 0),
        lui(5, 0x1122),
        ori(5, 5, 0x3344),
        sw(5, 0, 4), // [0] = 0x11223344
        addiu(6, 0, 8),
        lui(7, 0xAABB),
        ori(7, 7, 0xCCDD),
        sw(7, 0, 6), // [8] = 0xAABBCCDD
        lui(1, 0x9999),
        ori(1, 1, 0x9999), // $1 = 0x99999999
        lw(1, 0, 4),       // in flight: 0x11223344
        lwl(1, 8, 4),      // merges, and maybe cancels
        addiu(2, 1, 0),    // reads $1 in lwl's delay slot: which value?
        nop(),
    ]);
    steps(&mut psx, 14);

    // This core currently produces 0x11223344 here. The alternative reading is
    // 0x99999999, the value from before the cancelled load.
    assert_eq!(psx.cpu.reg(2), 0x9999_9999);
}

/// `lwl`/`lwr` assemble an unaligned word, and the second of the pair must see
/// the first's partial result, so they deliberately bypass the load delay.
#[test]
fn unaligned_load_pair_merges() {
    // Put 0x11223344 at [0] and 0x55667788 at [4], then read the word at
    // address 1: little-endian, that is bytes [1],[2],[3],[4] = 0x88112233.
    let mut psx = machine(&[
        lui(1, 0x1122),
        ori(1, 1, 0x3344),
        addiu(4, 0, 0),
        sw(1, 0, 4),
        lui(2, 0x5566),
        ori(2, 2, 0x7788),
        sw(2, 4, 4),
        lwr(3, 1, 4),
        lwl(3, 4, 4),
        nop(),
    ]);
    steps(&mut psx, 10);

    assert_eq!(psx.cpu.reg(3), 0x8811_2233);
}

/// Divide by zero does not trap on this part; it produces fixed values that
/// unchecked code depends on.
#[test]
fn divide_by_zero_returns_hardware_junk() {
    let mut psx = machine(&[
        addiu(1, 0, 5),
        addiu(2, 0, 0),
        div(1, 2),
        mflo(3),
        mfhi(4),
        nop(),
    ]);
    steps(&mut psx, 6);

    assert_eq!(
        psx.cpu.reg(3),
        0xFFFF_FFFF,
        "lo should be -1 for a non-negative dividend"
    );
    assert_eq!(psx.cpu.reg(4), 5, "hi should be the dividend");
}

/// `addi` traps on signed overflow (and leaves its target alone); `addiu` does
/// not, despite the name: the `u` means "no trap", not "unsigned".
#[test]
fn addi_traps_on_overflow_and_addiu_does_not() {
    let mut psx = machine(&[
        lui(1, 0x7FFF),
        ori(1, 1, 0xFFFF), // r1 = 0x7FFFFFFF
        addi(2, 1, 1),     // overflow -> exception
        nop(),
    ]);
    steps(&mut psx, 3);

    assert_eq!(
        psx.cpu.reg(2),
        0,
        "the trapping add must not write its target"
    );
    assert_eq!(psx.cpu.pc, 0xBFC0_0180, "did not vector to the BEV handler");
    assert_eq!(
        (psx.cpu.cop0.cause >> 2) & 0x1F,
        0xC,
        "wrong exception code"
    );
    assert_eq!(
        psx.cpu.cop0.epc,
        RESET + 8,
        "EPC does not name the faulting add"
    );

    let mut psx = machine(&[lui(1, 0x7FFF), ori(1, 1, 0xFFFF), addiu(2, 1, 1), nop()]);
    steps(&mut psx, 4);
    assert_eq!(psx.cpu.reg(2), 0x8000_0000);
    assert_eq!(psx.cpu.pc, RESET + 0x10);
}

/// An exception taken from a delay slot points EPC at the *branch*, and sets
/// Cause bit 31 so the handler knows to expect it.
#[test]
fn exception_in_a_delay_slot_backs_up_to_the_branch() {
    let mut psx = machine(&[
        lui(1, 0x7FFF),
        ori(1, 1, 0xFFFF),
        beq(0, 0, 4),  // taken branch at RESET + 8
        addi(2, 1, 1), // delay slot: overflows
        nop(),
    ]);
    steps(&mut psx, 4);

    assert_eq!(psx.cpu.cop0.epc, RESET + 8, "EPC should name the branch");
    assert_ne!(psx.cpu.cop0.cause & (1 << 31), 0, "Cause BD not set");
}

/// With Status Isc set, stores go to the I-cache rather than memory. The cache
/// is not modelled, so the write must be *dropped*. Writing RAM anyway is how
/// a core corrupts memory during the BIOS's boot-time cache scrub.
#[test]
fn isolated_cache_swallows_stores() {
    let mut psx = machine(&[
        addiu(4, 0, 0),
        addiu(1, 0, 0x0777),
        lui(5, 0x0001), // r5 = 0x00010000, Status Isc
        mtc0(5, 12),    // SR = Isc (BEV cleared too, which is fine here)
        sw(1, 0, 4),    // dropped
        mtc0(0, 12),    // SR = 0
        lw(2, 0, 4),    // reads RAM
        nop(),
        nop(),
    ]);
    steps(&mut psx, 9);

    assert_eq!(psx.cpu.reg(2), 0, "an isolated-cache store reached RAM");
    assert_eq!(psx.bus.ram[0], 0);
}

/// Coprocessor usability is decided by the Status CU bit alone, not by whether
/// the coprocessor is fitted. COP1 does not exist on this machine, so with CU1
/// set a COP1 instruction is simply accepted and does nothing.
///
/// Settled by `cpu/cop` in the ps1-tests suite, which is also what caught the
/// original "no FPU, therefore always trap" reading.
#[test]
fn coprocessor_usability_follows_the_status_bit() {
    const COP1_OP: u32 = 0x11 << 26;

    // CU1 clear at reset: traps, and BEV is set so it vectors into ROM.
    let mut psx = machine(&[COP1_OP, nop()]);
    steps(&mut psx, 1);
    assert_eq!(psx.cpu.pc, 0xBFC0_0180);
    assert_eq!(
        (psx.cpu.cop0.cause >> 2) & 0x1F,
        0xB,
        "expected CoprocessorError"
    );

    // CU1 set (Status bit 29): accepted, and execution simply continues.
    let mut psx = machine(&[lui(5, 0x2000), mtc0(5, 12), COP1_OP, nop()]);
    steps(&mut psx, 4);
    assert_eq!(
        psx.cpu.pc,
        RESET + 0x10,
        "a usable coprocessor must not trap"
    );
    assert_eq!(
        (psx.cpu.cop0.cause >> 2) & 0x1F,
        0,
        "no exception should have been taken"
    );
}

/// An unrecognised COP0 sub-opcode is ignored, not a reserved-instruction trap.
#[test]
fn unknown_cop0_subopcode_does_not_trap() {
    let mut psx = machine(&[(0x10 << 26) | (0x08 << 21), nop()]);
    steps(&mut psx, 1);
    assert_eq!(psx.cpu.pc, RESET + 4);
}

/// Retail RAM is 2 MB, mirrored across the 8 MB KUSEG window.
#[test]
fn ram_mirrors_across_the_kuseg_window() {
    let mut psx = machine(&[]);
    psx.bus.store32(0x0000_0010, 0xCAFE_F00D);

    assert_eq!(psx.bus.load32(0x0020_0010), 0xCAFE_F00D, "2 MB mirror");
    assert_eq!(psx.bus.load32(0x8000_0010), 0xCAFE_F00D, "KSEG0 view");
    assert_eq!(psx.bus.load32(0xA000_0010), 0xCAFE_F00D, "KSEG1 view");
}
