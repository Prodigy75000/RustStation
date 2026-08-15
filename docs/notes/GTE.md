# GTE (COP2)

**Status: implemented, not yet conformant.** All 15 command opcodes, the full
register file and the divider are in. `gte/test-all` gets through its register
tests and 19 opcode cases before stopping at a specific, reproducible failure,
recorded below.

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
* **Every command clears `FLAG` first.** It reports what *this* command did, not
  an accumulated history.

## Open questions

1. **`gte/test-all` stops at test 70**, `GTE 0x01 (sf=1, lm=0, tx=1, vx=1,
   mx=2)`, with `IR0` reading `0x0000000C` where hardware gives `0x00000000`.
   Every other register matches, including `FLAG`, so the flags agree and only
   the depth-cue value is wrong. `IR0` is `(n * DQA + DQB) >> 12` saturated to
   `0..0x1000`.

   Two things already ruled out by measurement: computing it from the truncated
   `MAC0` instead (that regresses to test 51), and the screen-coordinate
   precision fix above (already applied, and what got us from 50 to 69). Since
   `FLAG` matches, the divider result `n` is probably right, which points at the
   `DQA`/`DQB` path itself.

   The test prints per-register diffs and bails on the first failure, so each
   fix reveals the next one. That makes this a walk rather than a search.
2. **Command timing.** Each command takes a documented number of cycles and
   stalls the CPU if a result is read too early. Here every command completes
   instantly, so a game that relies on the stall sees results sooner than it
   should. Harmless in isolation, but it interacts with the cycle-cost work in
   [`TIMING.md`](TIMING.md).
3. **`gte-fuzz`** has not been run yet. It ships a reference log
   (`gte_valid_0xc0ffee_50.log`) and will be the better oracle once `test-all`
   is green, because it covers argument combinations a hand-written test does
   not.
