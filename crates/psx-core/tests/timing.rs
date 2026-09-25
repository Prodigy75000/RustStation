// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! End-to-end timing tests: the master clock, the scheduler, and a real
//! interrupt travelling all the way from a device to a CPU exception.
//!
//! The unit tests in `video.rs`, `timers.rs` and `irq.rs` check each piece on
//! its own. These check the wiring between them, which is where a timing model
//! usually goes wrong: every part behaves, and nothing ever fires.

mod common;
use common::*;

use psx_core::{irq, video, Psx};

/// CPU cycles in one NTSC frame: 263 lines x 3413 video clocks at 11/7 of the
/// CPU clock. Derived in `video.rs`; restated here so a failure says which
/// number moved.
const NTSC_FRAME_CYCLES: u64 = 571_213;

/// Cycle during which the **first** vertical blank begins.
///
/// Not the frame period, which is the trap this constant exists to avoid: the
/// blank starts at line 240 of 263, so it arrives at 240 x 3413 = 819 120 video
/// clocks, which is 521 258.18 CPU cycles. Expecting a VBlank one whole frame
/// after reset is off by 50 000 cycles and looks like the interrupt firing
/// early.
const FIRST_VBLANK_CYCLE: u64 = 521_259;

/// A program that unmasks VBlank, enables interrupts, and then spins.
///
/// Status is set to `0x00400401`: IEc (bit 0) so interrupts are taken at all,
/// IM2 (bit 10) which is the single hardware line the interrupt controller
/// drives, and BEV (bit 22) so the handler is the one in ROM.
fn spin_with_vblank_enabled() -> Psx {
    machine(&[
        lui(1, 0x1F80),
        ori(1, 1, 0x1074), // I_MASK
        addiu(2, 0, 1),    // bit 0 = VBlank
        sw(2, 0, 1),
        lui(3, 0x0040),
        ori(3, 3, 0x0401), // BEV | IM2 | IEc
        mtc0(3, 12),       // Status
        beq(0, 0, -1),     // spin here forever
        nop(),
    ])
}

#[test]
fn vblank_latches_on_the_right_cycle() {
    let mut psx = machine(&[beq(0, 0, -1), nop()]);

    psx.run(FIRST_VBLANK_CYCLE - 1);
    assert_eq!(
        psx.bus.irq.stat() & (1 << irq::VBLANK),
        0,
        "VBlank fired early"
    );

    psx.run(1);
    assert_ne!(
        psx.bus.irq.stat() & (1 << irq::VBLANK),
        0,
        "VBlank never latched"
    );
}

/// The blanks keep coming at the frame rate, and the fractional part of the
/// frame period is carried rather than dropped, so ten frames of stepping land
/// within a cycle of ten exact periods instead of drifting by ten.
#[test]
fn vblank_does_not_drift_over_many_frames() {
    let mut psx = machine(&[beq(0, 0, -1), nop()]);
    psx.run(FIRST_VBLANK_CYCLE + NTSC_FRAME_CYCLES * 10);

    assert_eq!(psx.bus.video.frames, 10);
}

/// Every frontend frame holds exactly one vertical blank, and ends at it.
///
/// A frame of a fixed 1/60 s used to be the unit, and one in about 84 of them
/// held no blank at all: the frontend showed the same picture twice, a hitch
/// every second and a half. Checked over 600 frames, NTSC and PAL, by where
/// the beam is after each one and how many blanks the interrupt controller
/// latched.
#[test]
fn a_frame_is_one_vblank_to_the_next() {
    for standard in [video::Standard::Ntsc, video::Standard::Pal] {
        let mut psx = machine(&[beq(0, 0, -1), nop()]);
        psx.bus.video.set_standard(standard);
        let vblank_line = if standard == video::Standard::Ntsc {
            240
        } else {
            288
        };
        for frame in 0..600 {
            psx.bus.irq.ack(!(1 << irq::VBLANK));
            psx.run_frame();
            psx.bus.sync();
            assert_eq!(
                psx.bus.video.line(),
                vblank_line,
                "{standard:?} frame {frame}"
            );
            assert_ne!(
                psx.bus.irq.stat() & (1 << irq::VBLANK),
                0,
                "{standard:?} frame {frame} held no vblank"
            );
        }
    }
}

#[test]
fn the_declared_frame_rates_are_the_consoles() {
    let ntsc = video::Standard::Ntsc.frame_rate();
    let pal = video::Standard::Pal.frame_rate();
    assert!((ntsc - 59.29286).abs() < 0.00001, "{ntsc}");
    assert!((pal - 49.76456).abs() < 0.00001, "{pal}");
}

/// The whole point of the slice: a device raises a request, the interrupt
/// controller gates it, and the CPU takes an exception. If any link is missing
/// this spins forever and the machine simply never notices time passing.
#[test]
fn vblank_reaches_the_cpu_as_an_exception() {
    let mut psx = spin_with_vblank_enabled();

    let mut taken_at = None;
    for _ in 0..NTSC_FRAME_CYCLES * 2 {
        psx.step();
        if psx.cpu.pc == BEV_HANDLER {
            taken_at = Some(psx.bus.cycle);
            break;
        }
    }

    let at = taken_at.expect("the CPU never took the VBlank interrupt");

    // It should land on the first VBlank, not a frame later and not
    // immediately. The slack covers the instruction boundary the exception is
    // actually taken at.
    assert!(
        (FIRST_VBLANK_CYCLE..FIRST_VBLANK_CYCLE + 8).contains(&at),
        "interrupt taken at cycle {at}, expected close to {FIRST_VBLANK_CYCLE}"
    );

    // Cause should name an external interrupt (code 0) with the hardware line
    // asserted, and EPC should point back into the spin loop.
    assert_eq!((psx.cpu.cop0.cause >> 2) & 0x1F, 0, "wrong exception code");
    assert_ne!(psx.cpu.cop0.cause & (1 << 10), 0, "Cause bit 10 not set");
    assert!(psx.cpu.cop0.epc >= RESET, "EPC is not in the program");
}

/// A masked source must not interrupt, however long it runs.
#[test]
fn a_masked_source_never_interrupts() {
    // Same as above but without the I_MASK write.
    let mut psx = machine(&[
        lui(3, 0x0040),
        ori(3, 3, 0x0401),
        mtc0(3, 12),
        beq(0, 0, -1),
        nop(),
    ]);

    psx.run(NTSC_FRAME_CYCLES * 3);
    assert_ne!(
        psx.bus.irq.stat() & 1,
        0,
        "VBlank should still have latched"
    );
    assert_ne!(
        psx.cpu.pc, BEV_HANDLER,
        "a masked source must not reach the CPU"
    );
}

/// The scheduler must never let the CPU run past the cycle at which a device
/// needs attention.
#[test]
fn the_cpu_never_runs_past_a_pending_event() {
    let mut psx = machine(&[beq(0, 0, -1), nop()]);

    for _ in 0..200_000 {
        psx.step();
        assert!(
            psx.bus.cycle <= psx.bus.next_event(),
            "master clock {} overran the scheduled event {}",
            psx.bus.cycle,
            psx.bus.next_event()
        );
    }
}

/// Running in one long burst and stepping one instruction at a time must reach
/// byte-identical state. This is what lets the libretro shim pick any frame
/// granularity it likes, and it is the property netplay rollback rests on.
#[test]
fn run_granularity_does_not_change_the_state() {
    let program: &[u32] = &[
        lui(1, 0x1F80),
        ori(1, 1, 0x1104), // timer 0 MODE
        addiu(2, 0, 0x0058),
        sw(2, 0, 1),
        beq(0, 0, -1),
        nop(),
    ];

    let mut burst = machine(program);
    let mut stepped = machine(program);

    burst.run(50_000);
    for _ in 0..50_000 {
        stepped.step();
    }

    assert_eq!(
        burst.save_state(),
        stepped.save_state(),
        "granularity changed the machine state"
    );
}

/// A save state taken mid-frame must restore the beam, the counters and the
/// scheduler exactly, so that the interrupt after the restore lands on the same
/// cycle it would have without it.
#[test]
fn a_restored_state_resumes_the_same_timing() {
    let mut reference = spin_with_vblank_enabled();
    let mut restored = spin_with_vblank_enabled();

    // Get well into the frame, then round-trip one of them through a state.
    reference.run(400_000);
    restored.run(400_000);
    let snapshot = restored.save_state();
    let mut restored = spin_with_vblank_enabled();
    assert!(restored.load_state(&snapshot));

    let cycle_of_interrupt = |psx: &mut Psx| -> u64 {
        for _ in 0..NTSC_FRAME_CYCLES * 2 {
            psx.step();
            if psx.cpu.pc == BEV_HANDLER {
                return psx.bus.cycle;
            }
        }
        panic!("no interrupt was taken");
    };

    assert_eq!(
        cycle_of_interrupt(&mut reference),
        cycle_of_interrupt(&mut restored),
        "the restored machine drifted"
    );
}

/// An exception must start its handler with an empty load-delay slot.
///
/// A load issued in the instruction before an interrupt arrives has to land
/// before the handler runs, not on the handler's first instruction. The
/// register ends up right either way, so this only moves *when* the value
/// becomes visible, which is invisible until a handler with load-delay-slot
/// code of its own reads the wrong register. It is only reachable at all now
/// that interrupts actually fire.
#[test]
fn an_exception_commits_the_pending_load_before_the_handler_runs() {
    // The interrupt is raised by hand rather than waited for. Waiting on a real
    // VBlank makes the result depend on which instruction of the spin loop it
    // lands on, so the test passes or fails on the clock constants rather than
    // on the behaviour it is meant to pin.
    let mut program = vec![0u32; 0x62];
    program[0] = lui(1, 0x1F80);
    program[1] = ori(1, 1, 0x1074); // I_MASK
    program[2] = addiu(2, 0, 1);
    program[3] = sw(2, 0, 1); // unmask VBlank
    program[4] = lui(3, 0x0040);
    program[5] = ori(3, 3, 0x0401); // BEV | IM2 | IEc
    program[6] = mtc0(3, 12);
    program[7] = addiu(4, 0, 0); // $4 = 0, a RAM address
    program[8] = lui(5, 0x0BAD);
    program[9] = sw(5, 0, 4); // [0] = 0x0BAD0000
    program[10] = addiu(1, 0, 0x0111); // $1 = 0x111, the value to be replaced
    program[11] = lw(1, 0, 4); // $1 <- 0x0BAD0000, delayed
    program[12] = nop(); // the load lands here, if it lands at all
    program[13] = beq(0, 0, -1);

    // The handler at 0xBFC00180 is BIOS word index 0x60. Its first instruction
    // copies $1 into $2, so a load that has not landed shows up directly.
    program[0x60] = addiu(2, 1, 0);
    program[0x61] = beq(0, 0, -1);

    let mut psx = machine(&program);

    // Execute words 0..=11 inclusive. The `lw` is now the last instruction to
    // have run, so its result is sitting in the load delay slot.
    for _ in 0..12 {
        psx.step();
    }
    assert_eq!(
        psx.cpu.pending_load().0,
        1,
        "the test needs a load in flight at this point"
    );
    assert_eq!(psx.cpu.reg(1), 0x0111, "the load must not have landed yet");

    // Interrupt exactly here.
    psx.bus.raise_irq(irq::VBLANK);
    psx.step();
    assert_eq!(psx.cpu.pc, BEV_HANDLER, "the interrupt was not taken");

    psx.step(); // the handler's first instruction
    assert_eq!(
        psx.cpu.reg(2),
        0x0BAD_0000,
        "the handler saw a load that had not landed yet"
    );
}

/// A pending interrupt must not swallow a GTE command.
///
/// Hardware runs the command and *then* takes the interrupt. Taking it first
/// drops the command entirely, because the BIOS handler steps `EPC` past it on
/// the way back out, assuming it already ran. Every interrupt that lands on a
/// GTE command then loses a geometry operation.
#[test]
fn an_interrupt_does_not_swallow_a_gte_command() {
    // COP2 command form: opcode 0x12, bit 25 set. Opcode 0 is not a real GTE
    // command, which makes `unknown_commands` a clean probe for "did the
    // instruction dispatch at all", independent of what any command computes.
    const GTE_COMMAND: u32 = 0x4A00_0000;

    let mut psx = machine(&[
        lui(1, 0x1F80),
        ori(1, 1, 0x1074), // I_MASK
        addiu(2, 0, 1),
        sw(2, 0, 1),
        lui(3, 0x4040),
        ori(3, 3, 0x0401), // CU2 | BEV | IM2 | IEc
        mtc0(3, 12),
        nop(),
        GTE_COMMAND,
        nop(),
        beq(0, 0, -1),
    ]);

    for _ in 0..8 {
        psx.step();
    }
    assert_eq!(psx.cpu.gte.unknown_commands, 0, "ran too early");

    // Interrupt exactly on the GTE command.
    psx.bus.raise_irq(irq::VBLANK);
    psx.step();

    assert_eq!(
        psx.cpu.gte.unknown_commands, 1,
        "the GTE command was skipped by the interrupt"
    );
    assert_ne!(
        psx.cpu.pc, BEV_HANDLER,
        "the interrupt should have deferred"
    );

    // It is only deferred, not lost.
    psx.step();
    assert_eq!(psx.cpu.pc, BEV_HANDLER, "the interrupt never arrived");
}

/// Timer 1 counts HBlanks, so after one frame it should hold the console's
/// scanline count rather than a cycle count.
#[test]
fn timer1_counts_scanlines_over_a_real_frame() {
    let mut psx = machine(&[
        lui(1, 0x1F80),
        ori(1, 1, 0x1114), // timer 1 MODE
        addiu(2, 0, 1 << 8),
        sw(2, 0, 1), // clock source 1 = HBlank
        beq(0, 0, -1),
        nop(),
    ]);

    psx.run(NTSC_FRAME_CYCLES);
    let lines = psx.bus.load32(0x1F80_1110);
    assert!(
        (260..=264).contains(&lines),
        "expected about one NTSC frame of scanlines, got {lines}"
    );
}
