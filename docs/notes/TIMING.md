# Timing, scheduling and interrupts

**Written from:** first drafted from general knowledge, then checked against the
ps1-tests suite's hardware logs (the timer tests and `cpu/access-time`; see
[`../TESTS.md`](../TESTS.md)). The instruction costs are from psx-spx ("Memory
Map", "Memory Control", "CPU Specifications", "GTE") and that access-time log.
Still provisional: the scanline constants and the synchronisation modes of
timers 0 and 1 (open questions below). The arithmetic is written out so the
numbers can be checked rather than trusted.

Implemented in `crates/psx-core/src/video.rs`, `timers.rs`, `irq.rs`, and the
scheduler in `bus.rs`.

## Why this landed before the GPU

The tempting shortcut, once a GPU exists, is to run a frame's worth of CPU and
then draw. That collapses the frame into a single phase, and it is the root of
a whole class of bugs (raster splits landing in the wrong place, frameskip,
shear) that is known in emulator cores generally, and expensive to fix late,
because the fix changes what a save state has to carry.

This core went to `format_version 2` for it with no states in the wild, so the
bump was free. Doing it later would not have been.

## The shape

There is one master clock: `Bus::cycle`, CPU cycles since reset. The CPU
advances it once per instruction. Every timed device is a pure function of that
clock plus its own registers, and holds `last synced` state rather than being
ticked.

```
Cpu::step  ->  Bus::tick(n)  ->  cycle += n
                                 if cycle >= next_event { sync() }

Bus::sync  ->  video.run(elapsed)   -> dot / hblank / vblank tick counts
               timers.run(elapsed, ticks)
               next_event = cycle + min(video.cycles_to_vblank(),
                                        timers.cycles_to_irq())
```

Two properties hold this together, and both are tested:

* **Granularity independence.** Advancing 50 000 cycles in one call and in
  50 000 calls reaches byte-identical state. Every device carries integer
  remainders rather than rounding at each step. This is what lets the libretro
  shim pick any frame granularity, and it is what netplay rollback rests on.
* **The CPU never runs past a pending event.** Asserted every instruction in
  `the_cpu_never_runs_past_a_pending_event`.

Register reads and writes call `sync()` first, so software never sees a counter
that is stale by however long the scheduler happened to sleep.

## The clocks

The CPU runs at 33.868800 MHz (44100 x 768) and the video clock at 53.2224 MHz,
which is **exactly 11/7 of it**. The ratio is applied with an integer remainder
carried between calls, so no floating point is anywhere in the timing path,
which is what keeps two builds bit-identical.

The ratio is not a guess. The `timers` test prints its own dot-clock frequencies
as it runs, 5.32224 MHz at 256 wide and 6.65280 MHz at 320 wide, whose dividers
are 10 and 8; both give 53.2224 MHz.

> **A correction worth keeping.** This started at 53.693175 MHz (715 909/451 584),
> picked because it produces the 59.82 Hz frame rate the console is usually
> quoted at, and the code comments dismissed 11/7 as a visibly wrong
> approximation. That was backwards. Switching to 11/7 moved the per-frame
> measurements from 4.5% out to 0.08% out and made the dot-clock counts exact.
> A constant chosen to make a remembered figure come out right is a constant
> fitted to the wrong evidence.

| | NTSC | PAL |
|---|---|---|
| Video clocks per scanline | 3413 | 3406 |
| Scanlines per frame | 263 | 314 |
| First line of vertical blank | 240 | 288 |
| Frame period | 897 619 video clocks = **571 212.09 CPU cycles** | 1 069 484 video clocks |
| Frame rate | 59.29 Hz | 50.19 Hz |

Two numbers worth keeping apart, because confusing them looks exactly like an
interrupt firing early:

* **Frame period**: 571 213 CPU cycles.
* **First VBlank**: cycle **521 259**, not 571 213. The blank starts at line 240
  of 263, so it arrives at 240 x 3413 = 819 120 video clocks, which is
  521 258.18 CPU cycles.

### The rounding trap

`cycles_to_vblank` predicts when the next blank begins. It has to subtract the
fractional video clock already banked in `clock_frac`, or it over-estimates by
up to a whole CPU cycle. Leaving that term out put the first VBlank one cycle
late, every time.

One cycle is small, but the error is in the dangerous direction. A prediction
that is **early** costs one wasted sync; a prediction that is **late** is a
missed interrupt. Every conversion in the scheduler is therefore explicitly
directional: `cpu_cycles_for_video_cycles` floors, `..._ceil` rounds up, and
each caller picks the one that cannot be late.

`cycles_to_vblank_lands_on_the_edge_from_any_offset` covers this by predicting
from seven different starting offsets. The original version of that test started
from a fresh device, where `clock_frac` is zero and the bug is invisible.

## Interrupts

Eleven sources latch into `I_STAT`, are gated by `I_MASK`, and arrive at the CPU
as COP0 Cause bit 10.

`I_STAT` is **write-acknowledge with inverted polarity**: writing a `0` clears a
bit, writing a `1` leaves it alone, so a handler dismisses one source by writing
`!(1 << bit)`. A plain `stat = val` or `stat |= val` leaves the line stuck high
forever. Sources latch whether or not they were masked at the time.

Taking an exception **commits the pending load** first, so the handler starts
with an empty load-delay slot. This was unreachable before interrupts could
fire; it is reachable now, and it matters because the BIOS exception handler has
load-delay-slot code of its own.

## Root counters

See the module comment in `timers.rs` for the register-level traps (inverted
bit 10, read-clears-flags on bits 11 and 12, writing MODE zeroing COUNT, and
0xFFFF being unreachable when the counter wraps at TARGET).

Clock sources: timer 0 from the system clock or the dot clock, timer 1 from the
system clock or HBlank, timer 2 from the system clock or the system clock over
8. Two of those come out of the video timing, which is why it had to land first.

## A frontend frame is one vblank to the next

`Psx::run_frame` runs to the start of the next vertical blank, and the
libretro core declares the console's own rate: 59.29 Hz NTSC, 49.76 Hz PAL
(`Standard::frame_rate`), chosen from the disc's region at load. Until
2026-09-25 each frontend frame was a fixed 564 480 cycles, a 60th of a second,
against an NTSC frame of about 571 212: one frontend frame in 84 held no
vblank, the frontend showed the same picture twice, and on a phone Final
Fantasy VIII's opening video hitched about once a second where Beetle PSX's did not.
A PAL game repeated ten frames a second. `a_frame_is_one_vblank_to_the_next` in
`tests/timing.rs` fails on the fixed length.

The rate is declared once. Following the GPU was tried: games and the BIOS
reset it between screens, which puts it in NTSC for a second or two even on a
PAL console, and Metal Gear Solid flipped four times in 20 seconds. So while
the video is in the other standard, a frame is instead exactly one declared
frame of CPU time, remainder carried (`Psx::run_frame_at`): the machine and
its sound keep real time, and only the pictures suffer, a repeat or a drop now
and then on logos and blank screens. The first version ran vblank to vblank
there too, and the BIOS boot sound dragged at 84% with an
American BIOS and a European disc.

## Skipping the vsync wait

Most games end each frame spinning on a RAM counter that only the vblank
handler moves. In Crash Bandicoot's gameplay that is 64% of all instructions,
and on the test tablet it was the difference between 17 ms per frame (over the
16.7 ms budget, so the phone played about 20% slow) and 9.8 ms.

`crates/psx-core/src/idle.rs` recognises one such loop, the vsync wait with a
stack timeout that Crash, Spyro and Twisted Metal 2 share, by its exact
instructions. At its head, with nothing in flight and no interrupt waiting, it
runs every whole pass that fits before the next scheduled device event: the
clock goes up by what a pass costs (see below), the timeout word down by
one, and nothing else changes. That is exactly what stepping them does, so it is an optimisation and
not an approximation. The unit tests check it byte for byte through a save
state, and so did 20 seconds of each of Crash, Twisted Metal 2 and Spyro
(`shot --noidle --save-end` against the default). `Psx::run` now counts
cycles rather than steps, which is the same number while every instruction
costs one.

There is also a general form, `idle::poll`, for waits that count nothing
down: run one real pass from an address back to itself while the bus watches,
and if it touched no device, every store wrote the value already there, and
the CPU (registers, delay slots, both coprocessors) is back where it started,
then every later pass up to the next event is the same pass, and they are
skipped. Its tests include a loop that differs from an idle one only by a
store, which must not be skipped. It does not help Crash Bash, whose wait
calls `VSync(-1)`, which reads a root counter: a device, so never idle by this
test, and correctly so.

With instruction costs (below) the pass costs 26 cycles, not 14, and the skip
steps a pass normally until the loop is in the I-cache.

**The average frame is not the budget; the worst one is.** Crash does its game
work in one frame and idles through the next. With the skip the pair averaged
9.8 ms on the tablet but the busy frame was still 19 ms, and a frontend that
waits for vsync after each frame turns that into two game frames per three
refreshes. Charging two cycles per instruction to spread the work was tried
and rejected: Crash drops to 20 frames a second, so the real console runs its
code faster than that, and the knob belongs to a real timing model, not to
performance.

What did help, each checked byte-identical on end states of Crash and Crash
Bash, measured on the tablet (Crash busy frame / Crash Bash demo average):

| change | Crash | Crash Bash |
|---|---|---|
| vsync skip only | 19.5 ms | 19.8 ms |
| RAM fast paths for fetch, loads and stores | | 17.6 |
| edge functions stepped along the row, pixel path inlined | 19.0 | 17.2 |
| copy back only the registers an instruction wrote (was 16% on ARM) | 15.5 | 14.5 |
| call-gate hooks only at the call gates; carried division in triangles | 15.8 | 13.8 average, 16.0 worst, rested |

The tablet throttles when warm, by about 10%, so compare runs after a rest.
Profile with `simpleperf record` on the device and the NDK's `annotate.py`
on the host for per-line costs.

Other games wait with other loops (Tekken 3 and Metal Slug X skip nothing).
They are cheap to add one at a time, each with the same byte-identical check.

## What an instruction costs (2026-09-26)

Until this date every instruction cost one cycle, and the CPU ran about twice
as fast as a console's. `crates/psx-core/src/timing.rs` now charges what
psx-spx and the suite's `cpu/access-time` log say:

* **Fetch.** KUSEG and KSEG0 go through the I-cache when the cache control
  register (FFFE0130h) has it on: 256 lines of four words, a tag of physical
  address bits 31 to 12 and a valid bit per word, filled from the word asked
  for to the end of the line. A hit costs nothing more. A miss from RAM costs
  6, the first word's latency (the CPU runs the rest as they stream in); a
  fetch from KSEG1 RAM costs the same 6, and from the BIOS ROM 28, since it is
  an 8-bit chip read four times a word. Only the tags are kept, which is all
  timing needs. The BIOS flushes the cache by writing tags with the cache
  isolated, and that is modelled; the HLE kernel's FlushCache clears them.
* **Loads**, by region and by the bytes they take, from the access-time log:
  RAM 4 more, the scratchpad nothing, the on-die I/O 2, the CD-ROM 7 to 25,
  the SPU 17 to 38, the BIOS 7 to 24. LWL and LWR take only the bytes they
  need, which the SPU's row in that log shows: its "32-bit" read of a 16-bit
  register at an odd halfword is compiled to such a pair and costs two
  halfword reads.
* **MFHI and MFLO wait for the multiplier**: 6, 9 or 13 cycles by the size of
  the first operand, 36 for a divide.
* **The GTE**: a command takes psx-spx's cycles, and MFC2, CFC2, SWC2 or the
  next command wait for it.

Stores still cost one cycle, however slow the device (the write queue is not
modelled), and DMA still takes no time from the CPU.

`cpu/access-time` against the console, SCPH-1001 BIOS:

| Region | 8 | 16 | 32 | Console 8 / 16 / 32 |
|---|---|---|---|---|
| RAM | 5.1 | 5.1 | 5.7 | 5.21 / 5.3 / 5.14 |
| BIOS | 8.13 | 13.7 | 25.1 | 7.6 / 12.94 / 24.94 |
| Scratchpad | 0.99 | 0.99 | 0.99 | 1.5 / 1.1 / 0.94 |
| Expansion 1 | 7.1 | 14.1 | 26.7 | 6.94 / 13.7 / 25.7 |
| Expansion 2 | 11.6 | 26.0 | 56.0 | 10.99 / 25.99 / 55.98 |
| Expansion 3 | 7.19 | 6.1 | 10.7 | 6.7 / 6.1 / 9.95 |
| I/O registers | 3.0 to 3.6 | | | 2.92 to 3.8 |
| CD-ROM | 8.0 | 14.0 | 26.0 | 8.0 / 14.0 / 25.93 |
| SPU | 18.6 | 18.0 | 36.6 | 17.99 / 17.99 / 38.94 |
| Cache control | 1.1 | 2.7 | 2.1 | 0.95 / 1.9 / 1.9 |

Before, every row read about 1.0.

**What it changed.** Final Fantasy VIII's opening video, which showed a
picture every three frames and then stood still for about seventeen, once a
second, now shows one every four frames without a gap: its player shows each
picture as soon as it is decoded, and the decoding is CPU work. And
the real BIOS's libetc stopped printing "VSync: timeout" about once a frame
in the suite's tests: its VSync wait counts passes of a loop, and at one
cycle an instruction the count ran out before the blank came.

`Psx::run(n)` counts cycles, and now stops at the end of the instruction
that reaches `n`, which can be tens of cycles past it. `run_frame` goes
vblank to vblank so it does not care; `run_frame_at` in the other standard
carries the overshoot into the next frame, so the machine keeps real time
(`a_frame_in_the_other_standard_keeps_real_time` runs from the uncached ROM,
29 cycles an instruction, to hold that). The vsync-wait skip charges a pass
what it costs, 14 instructions and three RAM loads, and only once the loop is
in the cache.

## Open questions

1. **The rest of the timing model**: the write queue, back-to-back loads
   costing more than the log's figures (which have independent work after
   each load), DRAM refresh, and DMA stealing the bus. Each is a place a game
   can still run faster than on a console.
2. **The I-cache's contents.** Only its tags are kept. A game that relies on
   running code the cache still holds after RAM under it changed (psx-spx
   names Formula One 2001) runs the new code here.
3. **Synchronisation modes** (`MODE` bits 0 to 2) are unverified for timers 0
   and 1, and are applied at scheduler granularity rather than at the exact
   cycle. Timer 2's behaviour (sync modes 0 and 3 stop the counter, 1 and 2 free
   run) is confidently known and implemented. `Timers::sync_uses` counts how
   often software actually enables sync, so the size of this gap is a number.
4. **HBlank as a level** is a placeholder: the last quarter of a scanline. The
   real display window comes from the GPU. Nothing depends on it yet, but the
   pause-during-blank sync modes will.
5. **The dot clock divider** is fixed at the 320-wide value (8). The GPU selects
   it from the horizontal resolution; the other dividers are already named
   constants.
6. **Scanline constants and VBlank start line** are provisional, and are the
   most likely home of the residual error. Against the captured hardware log the
   short-delay measurements are exact while the per-frame ones are 0.02% to 0.2%
   out, which points at 3413 / 263 rather than at the clock ratio. See
   [`../TESTS.md`](../TESTS.md) for the table.
