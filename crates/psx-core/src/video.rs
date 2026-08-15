// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Video timing: the scanline counter, HBlank, VBlank, and the dot clock.
//!
//! This is the **timing half of the GPU**, landed before the drawing half on
//! purpose. Two of the three root counters take their clock from here (timer 0
//! from the dot clock, timer 1 from HBlank), and VBlank is the console's first
//! real interrupt source, so nothing downstream can be timed correctly until
//! this exists. It draws nothing and knows nothing about VRAM.
//!
//! ## The clocks
//!
//! The CPU runs at 33.868800 MHz (44100 x 768) and the video clock at
//! 53.2224 MHz, which is **exactly 11/7 of it**.
//!
//! That ratio is not a guess or an approximation. The `timers` test in the
//! ps1-tests suite prints its own dot-clock frequencies as it runs: 5.32224 MHz
//! at 256 wide and 6.65280 MHz at 320 wide, whose dividers are 10 and 8. Both
//! give 53.2224 MHz, and 53.2224 / 33.8688 = 11/7 to the digit.
//!
//! An earlier version of this file used 53.693175 MHz (a ratio of
//! 715_909/451_584), on the reasoning that it produced the 59.82 Hz frame rate
//! the console is often quoted at. That was wrong, and the same hardware test
//! is what disproved it: with 11/7 the dot-clock counts match the captured log
//! and the frame period comes out at ~571 200 CPU cycles, which the log's
//! timer readings independently confirm. The frame rate really is ~59.29 Hz.
//! The lesson is the ordinary one: a constant chosen to make a remembered
//! figure come out right is a constant fitted to the wrong evidence.
//!
//! The ratio is applied with an integer remainder carried across calls and
//! serialized, so no floating point enters the timing path and two builds stay
//! bit-identical.
//!
//! The scanline constants below are still provisional. See
//! `docs/notes/TIMING.md`.

/// Video-clock-to-CPU-clock ratio: 53.2224 MHz over 33.8688 MHz.
const GPU_CLOCK_NUM: u64 = 11;
const GPU_CLOCK_DEN: u64 = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standard {
    Ntsc,
    Pal,
}

impl Standard {
    /// Video clocks in one scanline, including the horizontal blank.
    fn cycles_per_line(self) -> u64 {
        match self {
            Standard::Ntsc => 3413,
            Standard::Pal => 3406,
        }
    }
    /// Scanlines in one frame, including the vertical blank.
    fn lines_per_frame(self) -> u32 {
        match self {
            Standard::Ntsc => 263,
            Standard::Pal => 314,
        }
    }
    /// The first line of the vertical blank.
    fn vblank_line(self) -> u32 {
        match self {
            Standard::Ntsc => 240,
            Standard::Pal => 288,
        }
    }
}

/// What elapsed during a `run`. The root counters consume these; nothing else
/// does, which is why they are returned rather than stored.
#[derive(Clone, Copy, Default, Debug)]
pub struct Ticks {
    /// Dot-clock ticks, the clock source for timer 0.
    pub dots: u64,
    /// Scanlines completed, which is also the HBlank count and the clock source
    /// for timer 1.
    pub hblanks: u64,
    /// Times the vertical blank *began*. Normally 0 or 1, but a long run can
    /// legitimately span several frames.
    pub vblank_edges: u64,
}

#[derive(Clone)]
pub struct Video {
    standard: Standard,
    /// Video clocks into the current scanline.
    dot_in_line: u64,
    /// The scanline being generated.
    line: u32,
    /// Remainder of the CPU-to-video clock conversion, in units of
    /// 1/`GPU_CLOCK_DEN` of a video clock. Always less than the denominator.
    clock_frac: u64,
    /// Remainder of the video-clock-to-dot-clock division.
    dot_frac: u64,
    /// Video clocks per dot clock. Set by the GPU's horizontal resolution;
    /// fixed at the 320-wide value until there is a GPU to change it.
    dot_divider: u64,
    in_vblank: bool,
    /// Completed frames since reset. Diagnostic, and the harnesses print it.
    pub frames: u64,
}

/// Video clocks per dot clock, indexed by horizontal resolution. The GPU will
/// select one of these once it exists; 320-wide is the common case and the
/// default.
pub const DOT_DIVIDER_256: u64 = 10;
pub const DOT_DIVIDER_320: u64 = 8;
pub const DOT_DIVIDER_368: u64 = 7;
pub const DOT_DIVIDER_512: u64 = 5;
pub const DOT_DIVIDER_640: u64 = 4;

impl Default for Video {
    fn default() -> Self {
        Video::new(Standard::Ntsc)
    }
}

impl Video {
    pub fn new(standard: Standard) -> Video {
        Video {
            standard,
            dot_in_line: 0,
            line: 0,
            clock_frac: 0,
            dot_frac: 0,
            dot_divider: DOT_DIVIDER_320,
            in_vblank: false,
            frames: 0,
        }
    }

    pub fn standard(&self) -> Standard {
        self.standard
    }
    pub fn line(&self) -> u32 {
        self.line
    }
    pub fn in_vblank(&self) -> bool {
        self.in_vblank
    }
    /// True while the beam is in the horizontal blank of the current line.
    ///
    /// The visible portion is taken as the part of the line the dot clock
    /// covers; this is a placeholder shape until the GPU supplies the real
    /// display window, and nothing depends on it yet.
    pub fn in_hblank(&self) -> bool {
        self.dot_in_line >= self.standard.cycles_per_line() * 3 / 4
    }

    pub fn set_dot_divider(&mut self, divider: u64) {
        let d = divider.max(1);
        if d != self.dot_divider {
            // The remainder is in units of the old divider, so carrying it over
            // would scale it by the wrong amount.
            self.dot_frac = 0;
            self.dot_divider = d;
        }
    }

    /// Switch video standard. Clamps the beam into the new raster, which is
    /// smaller for NTSC than for PAL.
    pub fn set_standard(&mut self, standard: Standard) {
        if standard == self.standard {
            return;
        }
        self.standard = standard;
        self.line %= standard.lines_per_frame();
        self.dot_in_line %= standard.cycles_per_line();
        self.in_vblank = self.line >= standard.vblank_line();
    }

    /// Advance by `cpu_cycles` and report what ticked.
    pub fn run(&mut self, cpu_cycles: u64) -> Ticks {
        let total = self.clock_frac + cpu_cycles * GPU_CLOCK_NUM;
        let video_cycles = total / GPU_CLOCK_DEN;
        self.clock_frac = total % GPU_CLOCK_DEN;

        let mut ticks = Ticks::default();

        let dots = self.dot_frac + video_cycles;
        ticks.dots = dots / self.dot_divider;
        self.dot_frac = dots % self.dot_divider;

        let per_line = self.standard.cycles_per_line();
        let lines_per_frame = self.standard.lines_per_frame();
        let vblank_line = self.standard.vblank_line();

        let mut remaining = video_cycles;
        while remaining > 0 {
            let left_in_line = per_line - self.dot_in_line;
            if remaining < left_in_line {
                self.dot_in_line += remaining;
                break;
            }
            remaining -= left_in_line;
            self.dot_in_line = 0;

            ticks.hblanks += 1;
            self.line += 1;

            if self.line == vblank_line {
                self.in_vblank = true;
                ticks.vblank_edges += 1;
            }
            if self.line >= lines_per_frame {
                self.line = 0;
                self.in_vblank = false;
                self.frames += 1;
            }
        }

        ticks
    }

    /// CPU cycles until the vertical blank next begins, which is the only
    /// interrupt this device raises. Never returns 0, so the scheduler always
    /// makes progress.
    pub fn cycles_to_vblank(&self) -> u64 {
        let per_line = self.standard.cycles_per_line();
        let lines_per_frame = self.standard.lines_per_frame() as u64;
        let vblank_line = self.standard.vblank_line() as u64;
        let line = self.line as u64;

        let lines_to_go = if line < vblank_line {
            vblank_line - line
        } else {
            lines_per_frame - line + vblank_line
        };

        let video_cycles = lines_to_go * per_line - self.dot_in_line;
        self.cpu_cycles_for_video_cycles_ceil(video_cycles).max(1)
    }

    /// A CPU-cycle count that will not overshoot `dots` dot-clock ticks.
    ///
    /// The root counters use this to decide when to next wake up, so it rounds
    /// **down**: waking early costs one wasted sync, waking late is a missed
    /// interrupt.
    pub fn cpu_cycles_for_dots(&self, dots: u64) -> u64 {
        let video_cycles = dots
            .saturating_mul(self.dot_divider)
            .saturating_sub(self.dot_frac);
        self.cpu_cycles_for_video_cycles(video_cycles)
    }

    /// As above, for whole scanlines (the HBlank clock).
    pub fn cpu_cycles_for_lines(&self, lines: u64) -> u64 {
        let video_cycles = lines
            .saturating_mul(self.standard.cycles_per_line())
            .saturating_sub(self.dot_in_line);
        self.cpu_cycles_for_video_cycles(video_cycles)
    }

    /// CPU cycles needed to produce `video_cycles` more video clocks.
    ///
    /// The subtraction of `clock_frac` is the part that is easy to leave out
    /// and hard to see: part of a video clock has usually already been banked
    /// by the time this is asked, so ignoring it over-estimates by up to a
    /// whole CPU cycle. That made the first VBlank land on cycle 516 689
    /// instead of 516 688. One cycle is small, but the error is in the
    /// dangerous direction (late), and it does not cancel out.
    fn cpu_cycles_remaining(&self, video_cycles: u64) -> u64 {
        video_cycles
            .saturating_mul(GPU_CLOCK_DEN)
            .saturating_sub(self.clock_frac)
    }

    /// Rounds down: never overshoots the requested number of video clocks.
    fn cpu_cycles_for_video_cycles(&self, video_cycles: u64) -> u64 {
        self.cpu_cycles_remaining(video_cycles) / GPU_CLOCK_NUM
    }

    /// Rounds up: the first CPU cycle by which the video clocks have elapsed.
    fn cpu_cycles_for_video_cycles_ceil(&self, video_cycles: u64) -> u64 {
        self.cpu_cycles_remaining(video_cycles).div_ceil(GPU_CLOCK_NUM)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        standard: Standard,
        dot_in_line: u64,
        line: u32,
        clock_frac: u64,
        dot_frac: u64,
        dot_divider: u64,
        in_vblank: bool,
        frames: u64,
    ) {
        self.standard = standard;
        // Clamp rather than trust: a malformed state must not be able to push
        // the beam outside the raster and wedge the scheduler.
        self.dot_in_line = dot_in_line % standard.cycles_per_line();
        self.line = line % standard.lines_per_frame();
        self.clock_frac = clock_frac % GPU_CLOCK_DEN;
        self.dot_divider = dot_divider.clamp(1, 32);
        self.dot_frac = dot_frac % self.dot_divider;
        self.in_vblank = in_vblank;
        self.frames = frames;
    }

    pub(crate) fn parts(&self) -> (u64, u32, u64, u64, u64, bool, u64) {
        (
            self.dot_in_line,
            self.line,
            self.clock_frac,
            self.dot_frac,
            self.dot_divider,
            self.in_vblank,
            self.frames,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU cycles in one NTSC frame, from the constants above: 263 lines x 3413
    /// video clocks = 897 619 video clocks at 11/7 of the CPU clock, which is
    /// 571 212.09 CPU cycles, or 59.29 Hz.
    ///
    /// Cross-checked against the captured hardware log for the `timers` test:
    /// timer 2 clocked at system-clock/8 reads ~71 410 over a frame there,
    /// implying ~571 280 CPU cycles per frame. This model gives 571 212.
    #[test]
    fn ntsc_frame_rate_is_about_59_29_hz() {
        let mut video = Video::new(Standard::Ntsc);
        let cpu_hz = 33_868_800u64;

        video.run(cpu_hz);
        assert_eq!(video.frames, 59, "expected 59 whole frames in one second");

        // Tighter: the frame period, in CPU cycles.
        let mut video = Video::new(Standard::Ntsc);
        let mut cycles = 0u64;
        while video.frames == 0 {
            video.run(1);
            cycles += 1;
        }
        // 571 212.09, so the frame completes *during* cycle 571 213. The
        // fractional part is carried, not dropped, so the period does not drift.
        assert_eq!(cycles, 571_213);
    }

    #[test]
    fn vblank_edge_fires_once_per_frame() {
        let mut video = Video::new(Standard::Ntsc);
        let mut edges = 0;
        for _ in 0..10 {
            edges += video.run(571_212).vblank_edges;
        }
        assert_eq!(edges, 10);
    }

    /// The whole point of the fractional accumulator: advancing in small steps
    /// must land in exactly the same place as advancing in one big one.
    #[test]
    fn stepping_granularity_does_not_change_the_result() {
        let mut coarse = Video::new(Standard::Ntsc);
        let mut fine = Video::new(Standard::Ntsc);

        let mut coarse_ticks = Ticks::default();
        coarse_ticks.dots += coarse.run(100_000).dots;

        let mut fine_dots = 0;
        for _ in 0..100_000 {
            fine_dots += fine.run(1).dots;
        }

        assert_eq!(coarse.line(), fine.line());
        assert_eq!(coarse_ticks.dots, fine_dots);
        assert_eq!(coarse.parts(), fine.parts());
    }

    /// The prediction has to be exact from *any* starting point, not just from
    /// a fresh device. Starting mid-cycle is what exposed the missing
    /// `clock_frac` term: with a banked remainder the estimate was a cycle
    /// long, and the interrupt landed late.
    #[test]
    fn cycles_to_vblank_lands_on_the_edge_from_any_offset() {
        for warmup in [0u64, 1, 2, 3, 7, 12_345, 500_000] {
            let mut video = Video::new(Standard::Ntsc);
            assert_eq!(
                video.run(warmup).vblank_edges,
                0,
                "warmup {warmup} should not reach the blank"
            );

            let n = video.cycles_to_vblank();
            assert_eq!(
                video.run(n - 1).vblank_edges,
                0,
                "predicted VBlank at +{n} from {warmup}, but it fired early"
            );
            assert_eq!(
                video.run(1).vblank_edges,
                1,
                "predicted VBlank at +{n} from {warmup}, but it fired late"
            );
        }
    }

    #[test]
    fn pal_has_more_lines_and_a_slower_frame() {
        let mut pal = Video::new(Standard::Pal);
        let mut ntsc = Video::new(Standard::Ntsc);
        pal.run(33_868_800);
        ntsc.run(33_868_800);
        assert!(pal.frames < ntsc.frames);
    }
}
