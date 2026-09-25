# GTE (COP2)

**Status: conformant.** `gte/test-all` passes 1150 of 1150, and `gte-fuzz`
matches the hardware log **byte for byte across all 150 625 lines**: 50
randomised argument sets through every valid opcode, with the whole register
file dumped after each. All 15 command opcodes, the full register file and the
divider are in.

Getting there took seven distinct fixes, each found the same way: the suite
stops at the first mismatch and prints a per-register diff, so the register that
disagrees names the stage that is wrong. They are written up under "Traps"
below because every one of them is the kind of thing that reads as correct
until a number says otherwise.

Implemented in `crates/psx-core/src/gte.rs`.

## What it is

A fixed-point vector coprocessor. It does the perspective transforms, the
lighting and the ordering-table depth that the GPU then draws. No floating point
anywhere, which is a large part of why PlayStation geometry looks the way it
does.

Nearly everything is signed fixed-point with twelve fractional bits. The `sf`
bit in a command word chooses whether results are shifted back down by 12, and
getting it inverted is wrong by a factor of 4096, which reads as a completely
broken transform rather than a scaling error.

## Saturation is a result, not an edge case

Every intermediate has a defined width. Overflow both saturates *and* latches a
bit in `FLAG`, and game code reads `FLAG` to decide whether a polygon is
off-screen or degenerate. So the flags are as much an output as the numbers.
The accumulators are 44 bits, `IR1`-`IR3` are 16, colours are 8, and each has
its own flag. `FLAG` bit 31 is not stored: it is the OR of the bits that matter
and is recomputed on every write.

## The divider

`RTPS` needs `H / SZ3`, and it is not a plain division. The hardware seeds a
reciprocal from a 257-entry table and refines it twice, and the low bits do not
match a true divide. The table is **measured hardware data used as data**, which
the clean-room rule allows and this note is the citation for. A quotient above
`0x1FFFF` saturates and sets the divide-overflow flag, which is how software
detects a vertex at or behind the eye.

## Traps met so far

* **The 44-bit accumulators wrap.** Going past the range does not merely raise
  a flag and keep the wider value: the result is stored modulo 2^44 and sign
  extended, so a large positive comes back out negative. `MAC1`..`MAC3` cannot
  show this, because they are read as 32 bits and carry the same low bits
  either way. It surfaces only in the consumers that take the full accumulator,
  which is why the register that caught it was `SZ3` while `MAC3` sat there
  matching.
* **A row is accumulated one term at a time**, with the range checked and the
  accumulator wrapped after each product, not evaluated as a single expression
  and checked once. A running total that overflows on the second term and comes
  back on the third leaves no trace in the total, so a single check misses it
  entirely. The giveaway is that one accumulator can carry the positive *and*
  the negative overflow flag from the same command, which no single check can
  produce.
* **The colour multiply is not scaled by `sf`.** `MAC = [R*IR1, G*IR2, B*IR3]
  SHL 4` is exactly that, and `sf` belongs to the step after it. For the
  depth-cued commands that next step is the interpolation, which needs the full
  unshifted product, so shifting early throws away twelve bits of the colour
  term. Invisible on any channel whose colour byte is zero, which is how it
  survived to be found by a single blue channel in one `NCDS` case.
* **The interpolation's difference from the far colour raises the accumulator
  overflow flags without storing.** The flag is an output of that subtraction
  even though `MAC` keeps its previous contents.
* **That difference narrows to 32 bits before it saturates**, because `IR`
  saturates from the 32-bit `MAC` register rather than from the wider
  accumulator behind it. With `sf` clear there is no shift, so a far colour near
  the top of its range gives a difference past 32 bits, and hardware wraps it to
  a negative value instead of clamping to `+0x7FFF`. Measured both ways:
  saturating from the full width costs half the suite.
* **`CDP` (`0x14`) is not `DCPL` (`0x29`).** The mnemonics are near enough to
  read as the same command, and both end in a depth cue, but `CDP` runs the
  light-colour step first, so the colour it modulates is not the one already in
  `IR`. Two opcodes pointing at one function is a bug no amount of staring at
  that function will find.
* **`OTZ` comes from the full-precision product, not from `MAC0` after
  truncation.** A 16-bit scale factor times four 16-bit depths needs 34 bits, so
  it overflows routinely and the truncated register can read positive where the
  real value is negative. Same rule as the screen coordinates below.
* **Screen coordinates come from the full-precision intermediate, not from
  `MAC0` after truncation.** `MAC0` is a 32-bit register and still stores the
  truncated value, and still flags the overflow. But `SX2`/`SY2` are derived
  from the untruncated result and then saturated, so a vertex that should clamp
  to the screen edge at +1023 does exactly that, rather than wrapping to a small
  negative coordinate. Reading them back out of `MAC0` also loses the saturation
  flag. This was worth 19 test cases on its own.
* **`cop2r15` is not a register.** Reading it mirrors `SXY2`; *writing* it
  pushes the screen FIFO, which is how software appends a vertex without
  shuffling the other two.
* **`LZCR` counts leading ones for negative values**, not leading zeros.
* **`IRGB` is a derived view**, five bits per channel over `IR1`-`IR3`. That is
  why the save state serializes the GTE's logical fields rather than its 64
  register slots: round-tripping through the register interface would quietly
  lose eleven bits per channel.
* **`MVMVA` with translation vector 2 is bugged on hardware.** The first two
  components are computed with the third missing. Reproduced because it is
  reachable, not because anything sensible relies on it.
* **`MVMVA` with matrix 3 does not read a matrix.** The multiplexer is left
  half-driven and the three rows come out of unrelated registers: only the first
  involves the colour register, and the other two are `RT13` and `RT22`, each
  repeated across its row. Also reachable, also reproduced.
* **Every command clears `FLAG` first.** It reports what *this* command did, not
  an accumulated history.

## Reading the suite

`gte/test-all` prints the registers that disagree, by index, and nothing else.
Two habits made that enough to work from:

* **The register that disagrees is not always the one that is wrong.** Work
  backwards to the earliest stage that could produce it, and pay attention to
  what *matches*: `MAC3` agreeing while `SZ3` disagreed is what proved the
  accumulator wraps, because the only difference between those two is the width
  each reads at.
* **The test does not print its own inputs.** `RSTA_GTE_TRACE=1` dumps the whole
  register file around every command, and since the suite stops at the first
  mismatch, the failing command is always the last one traced. Recovering the
  operands that way turns a guess into arithmetic: the `CDP` bug was found by
  hand-computing the expected result from a trace and noticing that no integer
  multiplier could produce it, which meant the command was not the one being
  executed.

## Open questions

1. **Command timing** is the CPU's side of it, since 2026-09-26: a command
   takes psx-spx's cycles, and MFC2, CFC2, SWC2 or the next command wait for
   it to finish (`timing::gte_cycles`). MTC2 and CTC2 do not wait, as the
   pipeline page says. What is not modelled is the other half of that page:
   an input register overwritten while a command is still running changes
   nothing here, because the command has already run.
2. **`gte-fuzz` needs a controller.** It waits on Start before it will run, so
   it could not be used until the SIO0 port existed. Now that it can, it is the
   better of the two oracles: `test-all` is a hand-written list of cases and
   this is not.
