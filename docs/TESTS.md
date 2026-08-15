# Conformance baseline

Where the core stands against the hardware test suite. Update this whenever a
verdict moves, and say what moved it.

**Suite:** [ps1-tests](https://github.com/JaCzekanski/ps1-tests) by JaCzekanski.
Not vendored: it is gitignored under `tests/test-suite/` (see the repo
`.gitignore` for why). Bring your own copy and point `testrom --dir` at it.

**BIOS used for the runs below:** SCPH-1001 (US, v2.2, 1995-12-04).

```
cargo run --release --bin testrom -- <bios.bin> --dir tests/test-suite/cpu \
    --boot-steps 60000000 --steps 30000000
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
| `cpu/access-time` | UNGRADED | Measures cycles per access. Every number reads 0.0 against a reference of 0.94 to 55.98, because there is no timing model and no timer to measure with. |
| `cpu/io-access-bitwidth` | UNGRADED | Prints no verdict lines. Differs from the reference: needs real I/O devices, which are all stubbed. |
| `cpu/code-in-io` | **FAIL** 2 of 3 | Executing code out of the scratchpad and out of I/O space. Needs the scratchpad's real access rules. |

Everything outside `cpu/` (GPU, DMA, timers, CD-ROM, SPU, MDEC, GTE, input) is
untested because none of those subsystems exist yet. Running them now would
produce noise, not a baseline.

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

Both are pinned in `crates/psx-core/tests/cpu_semantics.rs`
(`coprocessor_usability_follows_the_status_bit`,
`unknown_cop0_subopcode_does_not_trap`) so a refactor cannot quietly undo them
between suite runs.

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
and are expected to be non-zero (513 reads, 7799 writes over 5M instructions).

The BIOS does not reach its shell hand-over on its own, because it is waiting on
a CD-ROM that does not exist. Sideloading a PSX-EXE works regardless: the
harness runs until the hand-over point is reached and swaps the binary in there.
