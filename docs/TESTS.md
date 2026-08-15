# Conformance baseline

Where the core stands against the hardware test suite. Update this whenever a
verdict moves, and say what moved it.

**Suite:** [ps1-tests](https://github.com/JaCzekanski/ps1-tests) by JaCzekanski.
Not vendored: it is gitignored under `tests/test-suite/` (see the repo
`.gitignore` for why). Bring your own copy and point `testrom --dir` at it.

**BIOS used for the runs below:** SCPH-1001 (US, v2.2, 1995-12-04).

```
cargo run --release --bin testrom -- <bios.bin> --dir tests/test-suite/cpu \
    --boot-steps 60000000 --steps 40000000
```

## Three verdicts, not two

- **PASS**: the test printed `pass` lines and no `fail` lines.
- **FAIL**: it printed at least one `fail` line.
- **UNGRADED**: it printed no verdict of its own, or states up front that it has
  no assertions and its numbers must be compared with the `psx.log` beside it.

UNGRADED is never counted as a pass. Several of these tests are pure
measurements, and grading them on "nothing looked like a failure" reports a
green run for a core that emulates none of what they measure. Both
`access-time` and `io-access-bitwidth` were briefly recorded as PASS here for
exactly that reason before the grader was fixed. Where a `psx.log` exists, the
harness also reports whether our output matches it.

## CPU, as of 2026-08-15

| Test | Verdict | Note |
|---|---|---|
| `cpu/cop` | **PASS** 17/17 | See below |
| `cpu/access-time` | UNGRADED | Now measures; every region reads a flat ~1.0 because instructions cost one cycle and there are no wait states. See below |
| `cpu/io-access-bitwidth` | UNGRADED | Prints no verdict lines. Differs from the reference: needs real I/O devices, which are all stubbed |
| `cpu/code-in-io` | **FAIL** 2 of 3 | Executing code out of the scratchpad and out of I/O space. Needs the scratchpad's real access rules |

## Timers, as of 2026-08-15

`timers` and `timer-dump` are measurement tests, so both grade UNGRADED. Their
value is in the numbers. Against the captured hardware log:

| Measurement | Ours | Hardware | Delta |
|---|---|---|---|
| System clock, 1000-cycle delay | 1011 | 1011 | **exact** |
| System clock, 5000-cycle delay | 5011 | 5011 | **exact** |
| Timer 1 from HBlank, per frame | 263 | 263 | **exact** |
| Timer 1 from HBlank, 1000 / 5000 | 0-1 / 2-3 | 0-1 / 2-3 | **exact** |
| Dot clock, 1000-cycle delay | 198-199 | 198-199 | **exact** |
| Timer 2 at sysclock/8, 1000 / 5000 | 126-127 / 626-627 | 126-127 / 626-627 | **exact** |
| Timer 2 at sysclock/8, per frame | 71 397 | 71 407-71 412 | -0.02% |
| System clock, per frame | 112 438-112 442 | 112 516-112 557 | -0.08% |
| Dot clock, per frame | 112 198 | 112 025-112 034 | +0.15% |
| Dot clock, 5000-cycle delay | 984-985 | 982-983 | +0.2% |

The residual sub-0.2% error is most likely in the scanline constants (3413
video clocks per line, 263 lines) rather than in the clock ratio, since the
short-delay measurements are exact and only the per-frame ones drift. That is
the next thing to check against a manual.

**Synchronisation modes do not match** and are known-unverified. Timer 2's
(modes 0 and 3 stop the counter) is confidently known and behaves. Timers 0 and
1 disagree with the log on modes 0, 1 and 3, and the likely cause is that
HBlank is currently a placeholder level (the last quarter of a scanline) rather
than the GPU's real display window. `Timers::sync_uses` counts how often
software enables sync, so the size of this gap stays measurable.

### What the clock ratio cost, and how it was caught

The video clock was initially set to 53.693175 MHz, a ratio of 715 909/451 584
to the CPU, chosen because it produces the 59.82 Hz frame rate the console is
commonly quoted at. The 11/7 ratio that other references give was written off in
the code comments as a visibly wrong approximation.

**That was backwards.** The `timers` test prints its own dot-clock frequencies
as it runs: 5.32224 MHz at 256 wide and 6.65280 MHz at 320 wide, whose dividers
are 10 and 8. Both give a 53.2224 MHz video clock, and 53.2224 / 33.8688 is
exactly 11/7. The real NTSC frame rate is ~59.29 Hz.

The correction moved the per-frame figures from 4.5% out to 0.08% out, and the
dot-clock delay counts from wrong to exact. A constant picked to make a
remembered figure come out right is a constant fitted to the wrong evidence;
the hardware log was the thing worth fitting to all along.

### Why `access-time` still cannot pass

It measures CPU cycles per access to each memory region. Before the timing work
it printed `0.0` everywhere, because it had no working timer to measure with.
It now measures correctly and reports a flat ~1.0:

| Region | Ours | Hardware |
|---|---|---|
| RAM | 1.1 | 5.14-5.21 |
| BIOS | 1.1 | 7.6 / 12.94 / 24.94 |
| Scratchpad | 0.99 | 0.94-1.5 |
| SPUCNT | 1.0 / 1.0 / 2.0 | 17.99 / 17.99 / 38.94 |

That is the honest picture of a machine where every instruction costs one cycle
and no region has wait states. Scratchpad is close because it genuinely has
none.

Calibrating the rest is **blocked on the instruction cache**, not on effort:
these numbers are dominated by instruction fetch, so fitting per-region wait
states now would be fitting constants to the wrong model. See
`docs/notes/TIMING.md`.

## What `cpu/cop` settled

It started at 12/17 and found one wrong rule that five cases turned on:

**Coprocessor usability is decided by the Status CU bit alone, not by whether
the coprocessor is fitted.** COP1 and COP3 do not exist on this machine, and the
original reading was "therefore they always trap". Hardware says otherwise: with
CU*n* set, the instruction is accepted and does nothing observable; only with it
clear does it raise a coprocessor-unusable exception. The same holds for the
COP0 load/store forms, `LWC0`/`SWC0`.

It also settled that an **unrecognised COP0 sub-opcode does not trap**, where
the original raised a reserved-instruction exception.

Both are pinned in `crates/psx-core/tests/cpu_semantics.rs` so a refactor cannot
quietly undo them between suite runs.

## BIOS boot

The kernel boots and prints its banner on all supplied BIOS images, with **zero
unmapped bus accesses**:

```
PS-X Realtime Kernel Ver.2.5
Copyright 1993,1994 (C) Sony Computer Entertainment Inc.
KERNEL SETUP!
Configuration : EvCB   0x10    TCB    0x04
System ROM Version 2.2 12/04/95 A
```

Zero *unmapped* accesses matters more than the banner: it means the address
decode has no holes the BIOS can find. Stubbed accesses are a different counter
and are expected to be non-zero.

The BIOS does not reach its shell hand-over on its own, because it is waiting on
a CD-ROM that does not exist. Sideloading a PSX-EXE works regardless: the
harness runs until the hand-over point is reached and swaps the binary in there.
