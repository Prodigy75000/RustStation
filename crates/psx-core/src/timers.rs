// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The three root counters.
//!
//! Each is a 16-bit up-counter with a `COUNT`, a `MODE` and a `TARGET`, at
//! `0x1F801100 + n * 0x10`. They differ only in what they can be clocked from:
//!
//! | Timer | Clock sources |
//! |---|---|
//! | 0 | system clock, or the **dot clock** |
//! | 1 | system clock, or **HBlank** |
//! | 2 | system clock, or system clock / 8 |
//!
//! Two of those come out of [`crate::video`], which is why the video timing had
//! to land first.
//!
//! ## Traps
//!
//! * **`MODE` bit 10 is inverted.** It reads 1 for "no interrupt requested".
//! * **Reading `MODE` clears bits 11 and 12** (reached target / reached
//!   0xFFFF). They are read-once flags, so a debugger that prints the register
//!   changes the program's behaviour.
//! * **Writing `MODE` resets `COUNT` to 0** and re-arms a one-shot interrupt.
//! * With bit 3 set the counter wraps at `TARGET`, which makes 0xFFFF
//!   unreachable, so the "reached 0xFFFF" interrupt can never fire. Off by one
//!   here means the counter wraps a tick early or late forever.
//!
//! ## What is not verified
//!
//! The **synchronisation modes** (`MODE` bits 0 to 2) are implemented from
//! general knowledge and are **not confirmed against a manual**. For timer 2
//! the behaviour is simple and confidently known: sync modes 0 and 3 stop the
//! counter, 1 and 2 free-run. For timers 0 and 1 the pause- and reset-on-blank
//! modes are best-effort, and are applied at scheduler granularity rather than
//! at the exact cycle. `sync_uses` counts how often software actually enables
//! sync, so the size of this gap is a number rather than a worry. See
//! `docs/notes/TIMING.md`.

use crate::irq::{self, Irq};
use crate::video::{Ticks, Video};

const SYNC_ENABLE: u16 = 1 << 0;
const RESET_ON_TARGET: u16 = 1 << 3;
const IRQ_ON_TARGET: u16 = 1 << 4;
const IRQ_ON_MAX: u16 = 1 << 5;
const IRQ_REPEAT: u16 = 1 << 6;
const IRQ_TOGGLE: u16 = 1 << 7;
/// Inverted: set means *no* interrupt is being requested.
const IRQ_NOT_REQUESTED: u16 = 1 << 10;
const REACHED_TARGET: u16 = 1 << 11;
const REACHED_MAX: u16 = 1 << 12;

/// Bits software may write. Everything above is status.
const MODE_WRITE_MASK: u16 = 0x03FF;

const MAX: u64 = 0xFFFF;
const PERIOD: u64 = 0x1_0000;

#[derive(Clone, Default)]
pub struct Timer {
    counter: u16,
    mode: u16,
    target: u16,
    /// Remainder for timer 2's system-clock/8 source.
    div8: u32,
    /// A one-shot interrupt fires once until `MODE` is written again.
    irq_fired: bool,
    /// Sync mode 3 latch: paused until the first blank, free-running after.
    sync_released: bool,
}

impl Timer {
    fn new() -> Timer {
        Timer {
            mode: IRQ_NOT_REQUESTED,
            ..Default::default()
        }
    }

    fn sync_mode(&self) -> u16 {
        (self.mode >> 1) & 3
    }
    fn clock_source(&self) -> u16 {
        (self.mode >> 8) & 3
    }

    /// A read of `MODE`. Bits 11 and 12 are cleared by the act of reading.
    fn read_mode(&mut self) -> u16 {
        let v = self.mode;
        self.mode &= !(REACHED_TARGET | REACHED_MAX);
        v
    }

    fn write_mode(&mut self, val: u16) {
        self.mode = (val & MODE_WRITE_MASK) | IRQ_NOT_REQUESTED;
        self.counter = 0;
        self.div8 = 0;
        self.irq_fired = false;
        self.sync_released = false;
    }

    /// How many increments before this timer would request an interrupt, or
    /// `None` if it never would from where it is now.
    fn ticks_to_irq(&self) -> Option<u64> {
        if self.mode & IRQ_REPEAT == 0 && self.irq_fired {
            return None;
        }

        let count = self.counter as u64;
        let target = self.target as u64;
        let reset_on_target = self.mode & RESET_ON_TARGET != 0;

        let mut best: Option<u64> = None;

        if self.mode & IRQ_ON_TARGET != 0 {
            let period = if reset_on_target { target + 1 } else { PERIOD };
            best = Some(distance(count % period, target % period, period));
        }
        // With the counter wrapping at TARGET, 0xFFFF is simply never seen.
        if self.mode & IRQ_ON_MAX != 0 && !reset_on_target {
            let d = distance(count, MAX, PERIOD);
            best = Some(best.map_or(d, |b| b.min(d)));
        }
        best
    }

    /// Advance by `n` increments, raising `index`'s interrupt if it is due.
    fn advance(&mut self, n: u64, index: usize, irq: &mut Irq) {
        if n == 0 {
            return;
        }

        let target = self.target as u64;
        let reset_on_target = self.mode & RESET_ON_TARGET != 0;
        let period = if reset_on_target { target + 1 } else { PERIOD };

        let start = (self.counter as u64) % period;
        let total = start + n;
        let wraps = total / period;
        self.counter = (total % period) as u16;

        let (hit_target, hit_max) = if reset_on_target {
            // The counter reaches TARGET exactly at each wrap. 0xFFFF is only
            // among the values visited if TARGET happens to be 0xFFFF.
            (wraps > 0, wraps > 0 && target == MAX)
        } else {
            (
                n >= distance(start, target, PERIOD),
                n >= distance(start, MAX, PERIOD),
            )
        };

        if hit_target {
            self.mode |= REACHED_TARGET;
        }
        if hit_max {
            self.mode |= REACHED_MAX;
        }

        let due = (hit_target && self.mode & IRQ_ON_TARGET != 0)
            || (hit_max && self.mode & IRQ_ON_MAX != 0);

        if due && (self.mode & IRQ_REPEAT != 0 || !self.irq_fired) {
            self.irq_fired = true;
            if self.mode & IRQ_TOGGLE != 0 {
                // Toggle mode flips the (inverted) request bit and leaves it.
                self.mode ^= IRQ_NOT_REQUESTED;
            } else {
                // Pulse mode drives it low for a few cycles. Nothing can
                // observe a window that short through this interface, so it
                // reads back as "no request", which is what hardware shows.
                self.mode |= IRQ_NOT_REQUESTED;
            }
            irq.raise(irq::TIMER0 + index as u32);
        }
    }
}

/// Increments from `from` before `value` is next seen, given a wrap `period`.
///
/// Sitting exactly on `value` counts as a full lap, not as zero: the counter
/// has to leave and come all the way back to reach it again.
fn distance(from: u64, value: u64, period: u64) -> u64 {
    let d = (value + period - from % period) % period;
    if d == 0 {
        period
    } else {
        d
    }
}

#[derive(Clone)]
pub struct Timers {
    pub t: [Timer; 3],
    /// How many times software has enabled a synchronisation mode. The sync
    /// behaviour is the unverified part of this module, so its blast radius is
    /// kept measurable rather than assumed to be zero.
    pub sync_uses: u64,
}

impl Default for Timers {
    fn default() -> Self {
        Timers::new()
    }
}

impl Timers {
    pub fn new() -> Timers {
        Timers {
            t: [Timer::new(), Timer::new(), Timer::new()],
            sync_uses: 0,
        }
    }

    /// Advance all three counters. `sys` is elapsed CPU cycles; `ticks` carries
    /// the dot-clock and HBlank counts from the video timing.
    pub fn run(&mut self, sys: u64, ticks: &Ticks, blank: (bool, bool), irq: &mut Irq) {
        let (hblank, vblank) = blank;

        for index in 0..3 {
            let source = self.t[index].clock_source();
            let increments = match index {
                // Timer 0: sources 1 and 3 are the dot clock, 0 and 2 the
                // system clock.
                0 => {
                    if source & 1 == 1 {
                        ticks.dots
                    } else {
                        sys
                    }
                }
                // Timer 1: sources 1 and 3 are HBlank.
                1 => {
                    if source & 1 == 1 {
                        ticks.hblanks
                    } else {
                        sys
                    }
                }
                // Timer 2: sources 2 and 3 are the system clock divided by 8.
                _ => {
                    if source & 2 == 2 {
                        let total = self.t[index].div8 as u64 + sys;
                        self.t[index].div8 = (total % 8) as u32;
                        total / 8
                    } else {
                        sys
                    }
                }
            };

            let blank_level = if index == 1 { vblank } else { hblank };
            let edges = if index == 1 {
                ticks.vblank_edges
            } else {
                ticks.hblanks
            };

            if self.apply_sync(index, blank_level, edges) {
                continue;
            }
            self.t[index].advance(increments, index, irq);
        }
    }

    /// Apply the synchronisation modes. Returns true if the counter is stopped
    /// for this run.
    ///
    /// Unverified for timers 0 and 1; see the module note.
    fn apply_sync(&mut self, index: usize, blank: bool, blank_edges: u64) -> bool {
        let timer = &mut self.t[index];
        if timer.mode & SYNC_ENABLE == 0 {
            return false;
        }

        match (index, timer.sync_mode()) {
            // Timer 2 is the confidently known case: stop, or free-run.
            (2, 0) | (2, 3) => true,
            (2, _) => false,

            // Pause while inside the blank.
            (_, 0) => blank,
            // Reset to zero at each blank, but never pause.
            (_, 1) => {
                if blank_edges > 0 {
                    timer.counter = 0;
                }
                false
            }
            // Reset at the blank, and only run inside it.
            (_, 2) => {
                if blank_edges > 0 {
                    timer.counter = 0;
                }
                !blank
            }
            // Wait for one blank, then free-run forever.
            (_, _) => {
                if !timer.sync_released {
                    if blank_edges > 0 || blank {
                        timer.sync_released = true;
                        return false;
                    }
                    return true;
                }
                false
            }
        }
    }

    /// CPU cycles until any counter would raise an interrupt.
    ///
    /// Conversions from dot-clock and HBlank ticks round **down**, so this is
    /// allowed to be early but never late. An early wake costs one wasted sync;
    /// a late one is a missed interrupt.
    pub fn cycles_to_irq(&self, video: &Video) -> Option<u64> {
        let mut best: Option<u64> = None;

        for (index, timer) in self.t.iter().enumerate() {
            // A stopped counter never gets anywhere on its own.
            if timer.mode & SYNC_ENABLE != 0 && index == 2 {
                let m = timer.sync_mode();
                if m == 0 || m == 3 {
                    continue;
                }
            }

            let Some(ticks) = timer.ticks_to_irq() else {
                continue;
            };
            let source = timer.clock_source();

            let cycles = match index {
                0 if source & 1 == 1 => video.cpu_cycles_for_dots(ticks),
                1 if source & 1 == 1 => video.cpu_cycles_for_lines(ticks),
                2 if source & 2 == 2 => ticks.saturating_mul(8),
                _ => ticks,
            };

            let cycles = cycles.max(1);
            best = Some(best.map_or(cycles, |b: u64| b.min(cycles)));
        }

        best
    }

    pub fn read(&mut self, offset: u32) -> u32 {
        let index = (offset / 0x10) as usize;
        if index > 2 {
            return 0;
        }
        match offset & 0xF {
            0x0 => self.t[index].counter as u32,
            0x4 => self.t[index].read_mode() as u32,
            0x8 => self.t[index].target as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32) {
        let index = (offset / 0x10) as usize;
        if index > 2 {
            return;
        }
        match offset & 0xF {
            0x0 => self.t[index].counter = val as u16,
            0x4 => {
                self.t[index].write_mode(val as u16);
                if val as u16 & SYNC_ENABLE != 0 {
                    self.sync_uses += 1;
                }
            }
            0x8 => self.t[index].target = val as u16,
            _ => {}
        }
    }

    pub(crate) fn parts(&self, index: usize) -> (u16, u16, u16, u32, bool, bool) {
        let t = &self.t[index];
        (
            t.counter,
            t.mode,
            t.target,
            t.div8,
            t.irq_fired,
            t.sync_released,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        index: usize,
        counter: u16,
        mode: u16,
        target: u16,
        div8: u32,
        irq_fired: bool,
        sync_released: bool,
    ) {
        let t = &mut self.t[index];
        t.counter = counter;
        t.mode = mode;
        t.target = target;
        t.div8 = div8 % 8;
        t.irq_fired = irq_fired;
        t.sync_released = sync_released;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timers() -> (Timers, Irq) {
        (Timers::new(), Irq::new())
    }

    fn no_ticks() -> Ticks {
        Ticks::default()
    }

    #[test]
    fn writing_mode_resets_the_counter() {
        let (mut t, mut irq) = timers();
        t.run(500, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x00), 500);

        t.write(0x04, 0);
        assert_eq!(t.read(0x00), 0, "writing MODE must zero COUNT");
    }

    #[test]
    fn reading_mode_clears_the_reached_flags() {
        let (mut t, mut irq) = timers();
        t.write(0x08, 10); // target
        t.write(0x04, RESET_ON_TARGET as u32);
        t.run(11, &no_ticks(), (false, false), &mut irq);

        let first = t.read(0x04);
        assert_ne!(first & REACHED_TARGET as u32, 0, "target flag not set");

        let second = t.read(0x04);
        assert_eq!(
            second & REACHED_TARGET as u32,
            0,
            "reading MODE must clear the reached flags"
        );
    }

    #[test]
    fn counter_wraps_at_target_when_bit3_is_set() {
        let (mut t, mut irq) = timers();
        t.write(0x08, 9);
        t.write(0x04, RESET_ON_TARGET as u32);

        // Period is TARGET + 1, so ten increments land back on zero.
        t.run(10, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x00), 0);

        t.run(3, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x00), 3);
    }

    #[test]
    fn counter_wraps_at_ffff_otherwise() {
        let (mut t, mut irq) = timers();
        t.write(0x04, 0);
        t.run(0x1_0005, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x00), 5);
    }

    #[test]
    fn target_interrupt_fires_and_respects_one_shot() {
        let (mut t, mut irq) = timers();
        t.write(0x08, 4);
        // IRQ on target, wrap at target, one-shot (no repeat).
        t.write(0x04, (RESET_ON_TARGET | IRQ_ON_TARGET) as u32);

        t.run(5, &no_ticks(), (false, false), &mut irq);
        assert_eq!(irq.stat(), 1 << irq::TIMER0);

        irq.ack(!(1 << irq::TIMER0));
        t.run(50, &no_ticks(), (false, false), &mut irq);
        assert_eq!(irq.stat(), 0, "a one-shot must not fire again");
    }

    #[test]
    fn repeat_mode_fires_every_lap() {
        let (mut t, mut irq) = timers();
        t.write(0x08, 4);
        t.write(
            0x04,
            (RESET_ON_TARGET | IRQ_ON_TARGET | IRQ_REPEAT) as u32,
        );

        t.run(5, &no_ticks(), (false, false), &mut irq);
        assert_eq!(irq.stat(), 1 << irq::TIMER0);
        irq.ack(!(1 << irq::TIMER0));

        t.run(5, &no_ticks(), (false, false), &mut irq);
        assert_eq!(irq.stat(), 1 << irq::TIMER0, "repeat mode should re-fire");
    }

    /// With the counter wrapping at TARGET, 0xFFFF is never among the values it
    /// takes, so the "reached 0xFFFF" interrupt can never fire.
    #[test]
    fn max_interrupt_cannot_fire_when_wrapping_at_target() {
        let (mut t, mut irq) = timers();
        t.write(0x08, 4);
        t.write(0x04, (RESET_ON_TARGET | IRQ_ON_MAX | IRQ_REPEAT) as u32);

        t.run(1_000_000, &no_ticks(), (false, false), &mut irq);
        assert_eq!(irq.stat(), 0);
    }

    #[test]
    fn timer1_can_be_clocked_from_hblank() {
        let (mut t, mut irq) = timers();
        // Timer 1, clock source 1 = HBlank.
        t.write(0x14, 1 << 8);
        let ticks = Ticks {
            dots: 999,
            hblanks: 7,
            vblank_edges: 0,
        };
        t.run(10_000, &ticks, (false, false), &mut irq);
        assert_eq!(t.read(0x10), 7, "should have counted HBlanks, not cycles");
    }

    #[test]
    fn timer0_can_be_clocked_from_the_dot_clock() {
        let (mut t, mut irq) = timers();
        t.write(0x04, 1 << 8);
        let ticks = Ticks {
            dots: 42,
            hblanks: 3,
            vblank_edges: 0,
        };
        t.run(10_000, &ticks, (false, false), &mut irq);
        assert_eq!(t.read(0x00), 42);
    }

    #[test]
    fn timer2_divides_the_system_clock_by_eight() {
        let (mut t, mut irq) = timers();
        // Timer 2, clock source 2 = system clock / 8.
        t.write(0x24, 2 << 8);

        t.run(7, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x20), 0, "seven cycles is not yet one tick");

        t.run(1, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x20), 1);

        // The remainder must carry, so granularity cannot change the total.
        t.run(16, &no_ticks(), (false, false), &mut irq);
        assert_eq!(t.read(0x20), 3);
    }

    #[test]
    fn timer2_sync_modes_0_and_3_stop_the_counter() {
        for mode in [0u16, 3] {
            let (mut t, mut irq) = timers();
            t.write(0x24, (SYNC_ENABLE | (mode << 1)) as u32);
            t.run(1000, &no_ticks(), (false, false), &mut irq);
            assert_eq!(t.read(0x20), 0, "sync mode {mode} should stop timer 2");
        }
        for mode in [1u16, 2] {
            let (mut t, mut irq) = timers();
            t.write(0x24, (SYNC_ENABLE | (mode << 1)) as u32);
            t.run(1000, &no_ticks(), (false, false), &mut irq);
            assert_eq!(t.read(0x20), 1000, "sync mode {mode} should free-run");
        }
    }

    /// Advancing in one step and in many must land identically. This is what
    /// lets the scheduler choose its own granularity without changing results.
    #[test]
    fn granularity_does_not_change_the_outcome() {
        let (mut coarse, mut irq_a) = timers();
        let (mut fine, mut irq_b) = timers();
        for t in [&mut coarse, &mut fine] {
            t.write(0x08, 977);
            t.write(0x04, (IRQ_ON_TARGET | IRQ_REPEAT) as u32);
            t.write(0x24, (2 << 8) as u32);
        }

        coarse.run(10_000, &no_ticks(), (false, false), &mut irq_a);
        for _ in 0..10_000 {
            fine.run(1, &no_ticks(), (false, false), &mut irq_b);
        }

        assert_eq!(coarse.read(0x00), fine.read(0x00));
        assert_eq!(coarse.read(0x20), fine.read(0x20));
        assert_eq!(irq_a.stat(), irq_b.stat());
    }

    #[test]
    fn distance_treats_sitting_on_the_value_as_a_full_lap() {
        assert_eq!(distance(0, 5, 16), 5);
        assert_eq!(distance(5, 5, 16), 16);
        assert_eq!(distance(10, 5, 16), 11);
    }
}
