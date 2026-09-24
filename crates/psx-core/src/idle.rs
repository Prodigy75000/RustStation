// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Skipping a game's vsync wait without changing what it computes.
//!
//! Most of the library spends part of every frame waiting for the vertical
//! blank, and the wait is a loop polling a counter in RAM that only the vblank
//! interrupt handler moves. Crash Bandicoot spends about 60% of its gameplay
//! instructions in it. Stepping every iteration is correct and, on a phone,
//! too slow: the tablet needed 17 ms of a 16.7 ms frame to emulate Crash.
//!
//! This recognises one such loop by its exact instructions and, whenever the
//! CPU is at its head and nothing can happen before the next scheduled device
//! event, runs as many whole iterations as fit in one go. That is only sound
//! because the loop's effect over `k` iterations is known exactly:
//!
//! * the clock and the retired-instruction count advance by 14 per iteration;
//! * the timeout word on the stack goes down by one per iteration;
//! * every register comes back to the value it had at the head.
//!
//! Nothing else is touched, and nothing else can observe the loop, because no
//! device runs between events and the interrupt that ends the wait is itself
//! an event. So the machine lands on exactly the state stepping would reach,
//! which `skipping_is_exact` checks byte for byte through a save state.
//!
//! The loop, as the game has it (addresses from Crash Bandicoot USA):
//!
//! ```text
//! head+00  lw   v0, 0x10(sp)      ; the timeout
//! head+04  nop
//! head+08  addiu v0, v0, -1
//! head+0C  sw   v0, 0x10(sp)
//! head+10  lw   v0, 0x10(sp)
//! head+14  nop
//! head+18  bne  v0, v1, head+4C   ; v1 = -1: out of time goes to the error path
//! head+1C  nop
//! head+4C  lui  v0, hi(counter)
//! head+50  lw   v0, lo(counter)(v0)
//! head+54  nop
//! head+58  slt  v0, v0, a0        ; the vblank count has not reached the target
//! head+5C  bne  v0, zero, head
//! head+60  nop
//! ```

use crate::bus::{mask_region, RAM_SIZE};
use crate::Psx;

/// Steps between looks for a loop, while none is known.
pub(crate) const PROBE_INTERVAL: u32 = 4096;

/// Instructions in one iteration.
const ITERATION: u64 = 14;

/// Words of the loop that must match, as `(word offset, value, mask)`. The
/// counter's address is the only part that differs between games.
const PATTERN: [(u32, u32, u32); 14] = [
    (0, 0x8FA2_0010, !0),
    (1, 0x0000_0000, !0),
    (2, 0x2442_FFFF, !0),
    (3, 0xAFA2_0010, !0),
    (4, 0x8FA2_0010, !0),
    (5, 0x0000_0000, !0),
    (6, 0x1443_000C, !0),
    (7, 0x0000_0000, !0),
    (19, 0x3C02_0000, 0xFFFF_0000),
    (20, 0x8C42_0000, 0xFFFF_0000),
    (21, 0x0000_0000, !0),
    (22, 0x0044_102A, !0),
    (23, 0x1440_FFE8, !0),
    (24, 0x0000_0000, !0),
];

/// Last word of the loop, relative to the head.
const SPAN_WORDS: u32 = 24;

const V0: usize = 2;
const V1: usize = 3;
const A0: usize = 4;
const SP: usize = 29;

/// Offset into RAM of a CPU address, if it is RAM.
fn ram_offset(addr: u32) -> Option<usize> {
    let phys = mask_region(addr);
    (phys < 8 * 1024 * 1024).then_some(phys as usize & (RAM_SIZE - 1))
}

fn ram_word(psx: &Psx, offset: usize) -> u32 {
    let b = &psx.bus.ram[offset..offset + 4];
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// The counter's address, if the loop at `head` is intact.
fn matches(psx: &Psx, head: u32) -> Option<u32> {
    if !head.is_multiple_of(4) {
        return None;
    }
    let base = ram_offset(head)?;
    if base + 4 * (SPAN_WORDS as usize + 1) > RAM_SIZE {
        return None;
    }
    for (i, value, mask) in PATTERN {
        if ram_word(psx, base + 4 * i as usize) & mask != value {
            return None;
        }
    }
    let hi = ram_word(psx, base + 4 * 19) << 16;
    let lo = ram_word(psx, base + 4 * 20) as u16 as i16 as i32 as u32;
    Some(hi.wrapping_add(lo))
}

/// Look for the loop around where the CPU is now.
pub(crate) fn find(psx: &Psx) -> Option<u32> {
    let pc = psx.cpu.pc;
    (0..=SPAN_WORDS)
        .map(|i| pc.wrapping_sub(4 * i))
        .find(|&head| matches(psx, head).is_some())
}

/// With the CPU at the loop's head, run as many whole iterations as can
/// happen before the next device event and before `target`. Returns how many
/// were skipped (possibly none), or `None` if the loop is no longer there.
pub(crate) fn skip(psx: &mut Psx, target: u64) -> Option<u64> {
    let head = psx.cpu.pc;
    let counter = matches(psx, head)?;
    let cpu = &psx.cpu;
    let regs = cpu.regs();

    // The CPU has to be exactly where one iteration leaves it: at the head
    // with nothing in flight, the delay slot of the backward branch just run,
    // and the registers the loop reaches at the end of every pass.
    let at_rest = cpu.next_pc == head.wrapping_add(4)
        && cpu.load == (0, 0)
        && !cpu.branch
        && regs == cpu.out_regs()
        && regs[V0] == 1
        && regs[V1] == 0xFFFF_FFFF;
    // Nothing that could interrupt the loop from inside: an interrupt already
    // waiting, or stores going nowhere.
    let quiet = !psx.bus.irq_pending() && !cpu.cop0.interrupt_ready() && !cpu.cop0.cache_isolated();
    if !at_rest || !quiet {
        return Some(0);
    }

    let counter_at = ram_offset(counter).filter(|o| counter.is_multiple_of(4) && o + 4 <= RAM_SIZE);
    let slot = regs[SP].wrapping_add(0x10);
    let slot_at = ram_offset(slot).filter(|o| slot.is_multiple_of(4) && o + 4 <= RAM_SIZE);
    let (Some(counter_at), Some(slot_at)) = (counter_at, slot_at) else {
        return Some(0);
    };
    // The stack slot must be its own word: not the counter, not the code.
    let code_at = ram_offset(head)?;
    let code_end = code_at + 4 * (SPAN_WORDS as usize + 1);
    if slot_at == counter_at || (code_at..code_end).contains(&slot_at) {
        return Some(0);
    }

    // The pass would leave the loop if the target is already reached.
    if (ram_word(psx, counter_at) as i32) >= regs[A0] as i32 {
        return Some(0);
    }

    // Iterations before a device needs attention: stepping ticks after every
    // instruction and syncs the moment the clock reaches the event, so the
    // skip has to stop strictly short of it. Nor may it cross the end of the
    // caller's run, or the timeout reaching -1, which takes the other exit.
    let now = psx.bus.cycle;
    let until_event = psx.bus.next_event().saturating_sub(now + 1) / ITERATION;
    let until_target = target.saturating_sub(now) / ITERATION;
    let timeout = ram_word(psx, slot_at) as u64;
    let k = until_event.min(until_target).min(timeout);
    if k == 0 {
        return Some(0);
    }

    let left = (ram_word(psx, slot_at) as u64 - k) as u32;
    psx.bus.ram[slot_at..slot_at + 4].copy_from_slice(&left.to_le_bytes());
    psx.bus.cycle += k * ITERATION;
    psx.cpu.cycles = psx.cpu.cycles.wrapping_add(k * ITERATION);
    Some(k)
}

/// Longest pass [`poll`] will follow before deciding it is not a loop.
const MAX_PASS: u32 = 2048;

/// Run one pass from here back to here, and if it changed nothing, skip the
/// passes that fit before the next device event and before `target`.
///
/// The general case, for the waits that do not count anything down. Crash
/// Bash waits on a timer with `VSync(-1)` inside a state machine, about sixty
/// instructions round, with its own call and its own stack frame. A pass
/// is *nothing changed* when it touched no device, every store wrote the value
/// already there, and the CPU (registers, delay slots, both coprocessors) is
/// back where it started. The next pass then reads the same memory with the
/// same registers and does the same thing, and so does every pass after it
/// until an event: an interrupt, or a device writing memory. None can happen
/// before the next scheduled event, so up to it the passes are all the same.
///
/// Returns the passes skipped, or `None` if this pass was not such a loop.
/// Either way the one pass it ran was real, so the caller has made progress.
pub(crate) fn poll(psx: &mut Psx, target: u64) -> Option<u64> {
    let head = psx.cpu.pc;
    let before = psx.cpu.clone();
    let start = psx.bus.cycle;
    psx.bus.watching = true;
    psx.bus.watch_dirty = false;
    let mut back = false;
    for _ in 0..MAX_PASS {
        if psx.bus.cycle >= target {
            break;
        }
        psx.step();
        if psx.cpu.pc == head {
            back = true;
            break;
        }
    }
    psx.bus.watching = false;
    if !back || psx.bus.watch_dirty || !psx.cpu.same_state(&before) {
        return None;
    }

    // An interrupt the CPU would take at the next boundary ends the loop.
    let mut cop0 = psx.cpu.cop0.clone();
    cop0.set_external_irq(psx.bus.irq_pending());
    if cop0.interrupt_ready() {
        return Some(0);
    }

    let pass = psx.bus.cycle - start;
    let now = psx.bus.cycle;
    let until_event = psx.bus.next_event().saturating_sub(now + 1) / pass;
    let until_target = target.saturating_sub(now) / pass;
    let k = until_event.min(until_target);
    psx.bus.cycle += k * pass;
    psx.cpu.cycles = psx.cpu.cycles.wrapping_add(k * pass);
    Some(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: u32 = 0x8000_1000;
    const COUNTER: u32 = 0x8000_2000;
    const STACK: u32 = 0x8000_3000;

    fn put(psx: &mut Psx, addr: u32, word: u32) {
        let o = ram_offset(addr).unwrap();
        psx.bus.ram[o..o + 4].copy_from_slice(&word.to_le_bytes());
    }

    /// Crash Bandicoot's wait loop at `HEAD`, waiting on a counter nothing
    /// moves, with interrupts off. It spins until the timeout runs out and
    /// then parks in the error path, so both exits get exercised.
    fn machine(timeout: u32) -> Psx {
        let mut psx = Psx::new(vec![0; crate::bus::BIOS_SIZE]).unwrap();
        for i in 0..=SPAN_WORDS {
            put(&mut psx, HEAD + 4 * i, 0);
        }
        for (i, value, _) in PATTERN {
            put(&mut psx, HEAD + 4 * i, value);
        }
        put(&mut psx, HEAD + 4 * 19, 0x3C02_0000 | (COUNTER >> 16));
        put(&mut psx, HEAD + 4 * 20, 0x8C42_0000 | (COUNTER & 0xFFFF));
        // The timeout exit, head+20: j head+20, a loop of its own.
        let park = HEAD + 0x20;
        put(&mut psx, park, 0x0800_0000 | ((park & 0x0FFF_FFFF) >> 2));
        put(&mut psx, COUNTER, 7);
        put(&mut psx, STACK + 0x10, timeout);
        psx.cpu.force_reg(V0 as u32, 1);
        psx.cpu.force_reg(V1 as u32, 0xFFFF_FFFF);
        psx.cpu.force_reg(A0 as u32, 100);
        psx.cpu.force_reg(SP as u32, STACK);
        psx.cpu.set_pc(HEAD);
        psx
    }

    fn run_both(timeout: u32, cycles: u64) -> (Psx, Psx) {
        let mut skipped = machine(timeout);
        let mut stepped = machine(timeout);
        stepped.skip_idle = false;
        // In uneven pieces, so skips end on run boundaries as well as events.
        for n in [1u64, 999, 123_457, cycles] {
            skipped.run(n);
            stepped.run(n);
        }
        (skipped, stepped)
    }

    /// A loop that calls a function to read a flag nothing sets: its own call,
    /// its own stack frame. With `count` the
    /// function also bumps a word in RAM every pass, which is not idle.
    fn polling_machine(count: bool) -> Psx {
        let mut psx = Psx::new(vec![0; crate::bus::BIOS_SIZE]).unwrap();
        let main = [
            0x0C00_0440, // jal 0x80001100
            0x0000_0000,
            0x1440_FFFD, // bne v0, zero, 0x80001000
            0x0000_0000,
            0x0800_0404, // j 0x80001010
            0x0000_0000,
        ];
        let func = [
            0x27BD_FFF0,                         // addiu sp, sp, -16
            0xAFBF_0000,                         // sw ra, 0(sp)
            0x3C08_8000,                         // lui t0, 0x8000
            0x8D02_2000,                         // lw v0, 0x2000(t0)
            if count { 0x8D09_2004 } else { 0 }, // lw t1, 0x2004(t0)
            0x0000_0000,
            if count { 0x2529_0001 } else { 0 }, // addiu t1, t1, 1
            if count { 0xAD09_2004 } else { 0 }, // sw t1, 0x2004(t0)
            0x8FBF_0000,                         // lw ra, 0(sp)
            // Registers come back the same even when counting, so only the
            // store can tell the two apart.
            if count { 0x0000_4821 } else { 0 }, // addu t1, zero, zero
            0x03E0_0008,                         // jr ra
            0x27BD_0010,                         // addiu sp, sp, 16
        ];
        for (i, w) in main.iter().enumerate() {
            put(&mut psx, HEAD + 4 * i as u32, *w);
        }
        for (i, w) in func.iter().enumerate() {
            put(&mut psx, 0x8000_1100 + 4 * i as u32, *w);
        }
        put(&mut psx, COUNTER, 1);
        psx.cpu.force_reg(SP as u32, STACK);
        psx.cpu.set_pc(HEAD);
        psx
    }

    #[test]
    fn a_loop_that_changes_nothing_is_skipped_exactly() {
        for count in [false, true] {
            let mut skipped = polling_machine(count);
            let mut stepped = polling_machine(count);
            stepped.skip_idle = false;
            for n in [1u64, 999, 123_457, 3_000_000] {
                skipped.run(n);
                stepped.run(n);
            }
            if count {
                assert_eq!(skipped.idle_skipped, 0, "a counting loop is not idle");
            } else {
                assert!(
                    skipped.idle_skipped > 50_000,
                    "skipped {}",
                    skipped.idle_skipped
                );
            }
            assert_eq!(skipped.bus.cycle, stepped.bus.cycle);
            assert_eq!(skipped.cpu.cycles, stepped.cpu.cycles);
            assert!(
                skipped.save_state() == stepped.save_state(),
                "states differ, count {count}"
            );
        }
    }

    #[test]
    fn skipping_is_exact() {
        let (skipped, stepped) = run_both(5_000_000, 3_000_000);
        assert!(
            skipped.idle_skipped > 100_000,
            "skipped {}",
            skipped.idle_skipped
        );
        assert_eq!(stepped.idle_skipped, 0);
        assert_eq!(skipped.bus.cycle, stepped.bus.cycle);
        assert_eq!(skipped.cpu.cycles, stepped.cpu.cycles);
        assert_eq!(skipped.cpu.pc, stepped.cpu.pc);
        assert!(
            skipped.save_state() == stepped.save_state(),
            "states differ"
        );
    }

    #[test]
    fn the_timeout_exit_is_taken_on_the_same_cycle() {
        // 30 000 passes is 420 000 cycles, well inside the run.
        let (skipped, stepped) = run_both(30_000, 1_000_000);
        assert!(
            skipped.idle_skipped > 1000,
            "skipped {}",
            skipped.idle_skipped
        );
        assert_eq!(skipped.cpu.pc, stepped.cpu.pc);
        let park = HEAD + 0x20;
        assert!(
            skipped.cpu.pc == park || skipped.cpu.pc == park + 4,
            "pc {:08X}",
            skipped.cpu.pc
        );
        assert!(
            skipped.save_state() == stepped.save_state(),
            "states differ"
        );
    }
}
