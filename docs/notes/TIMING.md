# Timing, scheduling and interrupts

**Written from:** general knowledge, pending confirmation against the reference
set in `../ref/`. The scanline constants and the synchronisation modes are the
parts to check first. The arithmetic is written out below so the numbers can be
checked rather than trusted.

Implemented in `crates/psx-core/src/video.rs`, `timers.rs`, `irq.rs`, and the
scheduler in `bus.rs`.

## Why this landed before the GPU

The tempting shortcut, once a GPU exists, is to run a frame's worth of CPU and
then draw. That collapses the frame into a single phase, and it is the root of
a whole class of bugs (raster splits landing in the wrong place, frameskip,
shear) in a sibling core, where the fix now needs a save-state format bump and
is still deferred as a result.

This core is at `format_version 2` with no states in the wild, so the bump was
free. Doing it later would not have been.

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

## Open questions

1. **Instruction cycle costs.** Every instruction is one cycle
   (`CYCLES_PER_INSTRUCTION`). Real ones cost roughly 1 to 40 depending on where
   they fetch from and what they touch. Everything downstream is written against
   the master clock rather than against that constant, so making it real is a
   change to one function.

   This is also why `cpu/access-time` cannot pass yet. It measures cycles per
   access per memory region, and those numbers are dominated by **instruction
   fetch**, so they are not meaningful until there is an I-cache. Calibrating
   region wait states before then would be fitting constants to the wrong model.
2. **`MULT`/`DIV` do not stall `MFHI`/`MFLO`.** One of the standard causes of a
   core running too fast.
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
