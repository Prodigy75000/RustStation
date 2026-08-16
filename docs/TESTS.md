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

## GPU, as of 2026-08-15

These are image comparisons, not pass/fail tests. `shot --compare` diffs our
VRAM against the `vram.png` the suite ships and buckets the differences, because
a single percentage cannot tell "the same picture, rounded differently" from "a
different picture". One step of a 5-bit channel is 8 in 8-bit terms.

```
cargo run --release --bin shot -- <bios.bin> \
    --exe tests/test-suite/gpu/triangle/triangle.exe \
    --compare tests/test-suite/gpu/triangle/vram.png --out out/shots/triangle.png
```

Sorted by the column that matters, which is the third one, not the second.

| Test | Pixels differing | Of which beyond a rounding step | Before textures | Reading |
|---|---|---|---|---|
| `texture-overflow` | **0.000%** | 0.000% | 6.246% | Pixel-exact |
| `rectangles` | **0.000%** | 0.000% | 1.619% | Pixel-exact |
| `clipping` | **0.000%** | 0.000% | 0.000% | Pixel-exact |
| `lines` | 0.188% | 0.019% | 0.188% | Essentially correct |
| `clut-cache` | 0.176% | 0.174% | 0.977% | |
| `triangle` | 5.179% | 0.189% | 5.179% | Dither phase, plus edges |
| `quad` | 0.324% | 0.300% | 0.324% | Polygon edges |
| `texture-flip` | 25.635% | 0.849% | 52.782% | Structurally right, see below |
| `vram-to-vram-overlap` | 1.441% | 1.224% | 8.778% | |
| `uv-interpolation` | 4.699% | 3.220% | 7.900% | Affine interpolation precision |
| `transparency` | 85.352% | 85.352% | 85.352% | Background fill, see below |

Two entries need reading rather than scanning.

**`triangle`**: 5% of pixels differ, but all except 0.19% differ by exactly
**one** 5-bit step. That is a dithering phase difference, not a broken
rasterizer. Before dithering was implemented it was 6.75%, with the same 0.19%
of real differences.

**`texture-flip`**: 25.6% differ but only 0.85% by more than a rounding step,
down from 51.6%. The structural half was `GP0(0xE1)` masking the draw mode to
eleven bits and so discarding the textured-rectangle flip bits (12 and 13). The
hardware mirrors one texture into four quadrants; this core drew four identical
copies until that mask was widened.

**`transparency`'s 85% is misleading and was checked by eye.** The blended
colour swatches, which is what the test is actually about, match the reference.
The entire difference is the background: hardware ends up with all of VRAM
filled light grey, and this core fills only 320x240 of it. That is a
`fill_rectangle` question, recorded in `docs/notes/GPU.md`, not a
semi-transparency one.

### Four hypotheses tested and rejected

Worth recording, because each looked plausible and cost only a measurement:

| Hypothesis | Result |
|---|---|
| The dither matrix is transposed | `triangle` got **worse**, 5.18% to 6.09% |
| Hardware does not dither textured polygons | No change at all |
| Modulation rounds rather than truncates | No change at all |
| A flipped rectangle anchors its UV at the far edge | **Worse**, 25.6% to 37.9% |

The two "no change at all" results were themselves the finding: byte-identical
output from three different changes meant the code path was not being reached,
because those tests use **raw** textures, which bypass both modulation and
dithering. The remaining error there is in the texel fetch, not the blend.

Everything else outside `cpu/`, `timers/`, `gpu/`, `gte/` and `input/` (CD-ROM,
SPU, MDEC) is untested because none of those subsystems exist.

## GTE, as of 2026-08-16

**`gte/test-all`: 1150 of 1150.** All 15 command opcodes, the register file and
the divider, matching the hardware log line for line apart from a `Total tests:`
line the log predates.

It started at 50. The walk from there is worth recording as a sequence, because
the shape of it is the point: the suite stops at the first mismatch and prints
which registers disagree, so each fix reveals exactly one more.

| Stopped at | The register that disagreed | What was actually wrong |
|---|---|---|
| 51 | `SX2`/`SY2` | Screen coordinates read from `MAC0` after truncation |
| 70 | `SZ3`, while `MAC3` matched | The 44-bit accumulators wrap; nothing 32 bits wide can show it |
| 203 | `MAC3`, blue channel only | The far-colour difference was scaled by `sf` twice |
| 206 | `FLAG` alone | That subtraction raises the accumulator flags without storing |
| 304 | `MAC2`, `MAC3` | `MVMVA` matrix 3 takes its rows from `RT13`/`RT22`, not `IR0` |
| 353 | `MAC3`, blue channel only | The colour multiply is not scaled by `sf` |
| 390 | `FLAG` alone, both directions | Rows accumulate one term at a time, so one row can overflow twice |
| 401 | `MAC1`, `MAC2` | `0x14` is `CDP`, not `DCPL`: two opcodes, one function |
| 861 | `OTZ`, while `MAC0` matched | `OTZ` saturates from the full-precision product |

All nine are pinned in `crates/psx-core/src/gte.rs`, and each test's expected
value differs from what the previous implementation produced, so none can pass
by accident.

Two of those rows are the same finding in two places, and it is the one worth
carrying to the other cores: **when a wide value is read at two widths, the
narrow register cannot tell you which is wrong.** `MAC3` matching while `SZ3`
disagreed, and `MAC0` matching while `OTZ` disagreed, were both the evidence,
not the noise.

The one that resisted analysis was 401. Hand-computing the expected result from
the traced operands showed that no integer multiplier could produce it, which
ruled out every variant of the formula rather than any particular one. That is
what pointed at the opcode table instead of the arithmetic.

### `gte-fuzz`: byte-identical against the hardware log

The stronger oracle, and it now agrees exactly. `gte-fuzz` runs 50 randomised
argument sets through every valid opcode from a fixed seed and dumps the whole
register file after each, then ships the hardware's own dump of the same run.

```
testrom <bios.bin> tests/test-suite/gte-fuzz/gte-fuzz.exe     --hold start --boot-steps 60000000 --steps 3000000000
```

**150 625 lines, zero differences.** It waits on Start to begin, which is why it
could not be run before the controller port existed, and it takes about 1.26
billion instructions.

This is worth more than `test-all` passing, because `test-all` is a hand-written
list of cases and this is not: it covers argument combinations nobody chose. The
two together are what make the GTE section of this file a statement about the
hardware rather than about the tests.

### `RSTA_GTE_TRACE=1`

The suite does not print its own inputs. This dumps the register file around
every command, and since the run stops at the first mismatch, the failing
command is always the last one traced. Four of the nine fixes above came from
reading those operands rather than from guessing at the formula.

### The grader had to be fixed first

`test-all` ends with its own tally, `Passed tests: 1150` / `Failed tests: 0`.
The harness counted verdict lines by prefix, so the tally itself read as one
pass and one failure, and a clean run graded **FAIL**. It now reads a tally as
authoritative where one exists. That direction is the harmless one, but it is
the same class of bug as the false PASS described at the top of this file: the
grader agreeing with the run by accident.

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

## Controllers, as of 2026-08-16

`input/pad` prints the name of every button it currently sees held, so it grades
UNGRADED and its value is in what it prints. The harness grew a `--hold` flag to
drive it:

```
testrom <bios.bin> tests/test-suite/input/pad/pad.exe --hold cross,start,up
```

| Held | Printed |
|---|---|
| nothing | nothing |
| `cross,start,up` | `PAD_UP`, `PAD_X`, `PAD_START` |

That comparison is the whole test. A port that answers plausibly and ignores the
pad prints the same thing either way, and the first version of this code did:
it printed **all sixteen** names with nothing held, because the BIOS was giving
up part way through the report and filling the buffer with zeroes, and zero
means pressed.

### The bug that cost the most, and what nearly hid it

The BIOS was reading three bytes of the five-byte pad report and abandoning the
transfer. The cause was that a new byte written while the previous `/ACK` pulse
was still low did not reset the pulse's phase, so the next countdown expiry was
read as the *release* of the old pulse instead of the assertion of the new one,
and that byte never raised its interrupt.

Sweeping the acknowledge delay to find the problem gave complete reads at 175,
200, 275 and 300 cycles and nothing at 150, 225, 250 or 338. **Bands, not a
threshold.** Any one of the working values would have made this table green
while leaving the bug in place, for exactly the reason the video clock constant
in this file was wrong: a constant chosen because it makes a symptom go away is
fitted to the symptom.

Two register traces settled it, and both are worth keeping in mind for the
subsystems still to come:

* The BIOS reads the pad with the controller interrupt **masked off**, polling
  `I_STAT` bit 7 in a tight loop. Timing arguments that start from "the handler
  runs when the device acknowledges" are reasoning about a loop that is not
  running.
* Changing what the pad *replied* changed nothing: the transfer still stopped
  after three bytes whatever the content. That ruled out every protocol
  hypothesis at once and pointed at the port rather than the device.

## CD-ROM, as of 2026-08-16

**Nothing in `cdrom/` can be graded yet, and that is the honest position.** All
four tests need something this core does not have:

| Test | What it needs |
|---|---|
| `getloc` | A disc: it seeks, reads, and checks track and index |
| `timing` | A disc, and cycle-accurate command latencies measured against it |
| `disc-swap` | A disc, plus the lid opened and closed by hand part way through |
| `terminal` | A disc |

So the controller was built to a different, checkable milestone: **the BIOS
getting past its boot logo to its own main menu**, which is what a real console
does with an empty drive. It does.

```
cargo run --release --bin shot -- <bios.bin> --steps 300000000
```

Before: the Sony Computer Entertainment logo, then nothing, because the BIOS was
waiting on a drive that never answered. After: the main menu, with the memory
card and CD player entries and the animated background.

That is a weaker check than a reference log and it is worth being clear about
what it does and does not prove. It proves the register block, the response
queue, the interrupt gating and the empty-tray answers are right enough for the
BIOS's own driver, which is real software with real expectations. It proves
nothing about timing, and the timing constants here are guesses; `cdrom/timing`
is the thing that would settle them, and it needs a disc.

It also produced the first evidence about the GPU that did not come from a test
pattern: the menu draws colour noise where its two icons should be. Recorded in
`docs/notes/GPU.md`.

### With a disc: the licence screen

The second milestone, and a much stronger one, because it exercises the whole
path rather than the register block alone.

```
python tools/fakedisc.py out/fakedisc
cargo run --release --bin shot -- <bios.bin> \
    --disc out/fakedisc/fake.cue --steps 400000000 --out out/shots/fake.png
```

`fakedisc.py` writes a disc that will not boot, but does carry a correct Mode 2
Form 1 sector layout and a licence string in its system area. The BIOS then runs
its entire recognition sequence, and the trace is worth reading in full:

```
Test(20) -> 94 09 19 c0        the drive firmware's own version
GetStat  -> 10                 shell-open, latched, on the first look
GetStat  -> 00                 and clear on the second
GetID    -> 02 00 20 00 SCEA   a licensed disc, region read off the disc
Setloc(00:02:04)               LBA 4, the licence sector
SeekL    -> INT3 40, INT2 02   seeking, then landed
Setmode(80)                    double speed
ReadN    -> INT1 sector 4      one sector, delivered
Pause
```

The BIOS then draws the PlayStation licence screen, and **the text on it is the
string written into sector 4 of the image**. That is the cue parser, the LBA to
MSF conversion, the seek, the sector fetch, the data FIFO and DMA channel 3 all
agreeing with each other, checked by a picture rather than by an assertion each
of them individually passes.

It is committed as a tool rather than described in prose because a milestone
nobody can reproduce is not much of a milestone, and no disc image can go in
this repository.

**What it still does not prove:** that a real game boots. The synthetic disc has
no filesystem, no `SYSTEM.CNF` and no executable, so everything past "the BIOS
likes this disc" is untested. The timing constants remain approximate.

## Real games, as of 2026-08-16

Four commercial discs, BIN/CUE, run with `shot --disc`. This is the first time
this core has been pointed at software it was not written against.

| Game | BIOS | How far it gets |
|---|---|---|
| **Crash Bandicoot (USA)** | SCPH-1001 | **Into gameplay.** Title screen, opening cutscene, then N. Sanity Beach with its level geometry rendering |
| Tomb Raider (USA) Rev 6 | SCPH-1001 | The BIOS licence screen with its 3D logo, then hangs at 27 sectors |
| Grand Theft Auto 2 | SCPH-1001 | Reads its filesystem, then loops. See below |
| Silent Hill (USA) | SCPH-1001 | 424 sectors, then noise on screen. Its intro is an MDEC video, and there is no MDEC |
| Ace Combat 2 (SCES) | SCPH-1002 | Untried since the PAL BIOS started booting |

**What "into gameplay" does and does not mean.** With Start held from boot,
Crash goes title screen, then the opening cutscene (Tawna and Cortex's machine,
textured 3D characters with lighting), then the first level, N. Sanity Beach,
with its sand, palm trees and structures rendering under the level title card.
5 483 sectors and 5.2 million textured primitives at 512x240.

It is *not* a claim that the game is playable. Every run so far has held a
single button from boot, so nothing has tested that it responds to **changing**
input, and there is no sound at all. The next honest step is a scripted input
sequence rather than another screenshot.

Twenty-one discs are now available locally; five have been tried.

Crash reads 1 647 sectors, draws 1.33 million textured primitives and pushes
619 KB of samples into sound RAM. Tomb Raider's 57-track, one-file-per-track cue
sheet parsed correctly, which is the multi-file case `docs/notes/DISC.md` listed
as implemented but untested.

### What Crash cost, and it was one register

It stopped at 473 sectors and 600 primitives, spinning on `0x1F801DAA`,
**`SPUCNT`**, 3 841 times in the last four thousand reads before the stall. The
game writes that register and polls it until its own value comes back; a stub
returning zero forever is not a missing feature, it is a hang.

Making the SPU register file read back what was written, and nothing else, took
it to the title screen. No audio was implemented and none is produced.

The diagnostic that found it is worth keeping: a histogram of reads to
decoded-but-unemulated ports. "The game is stuck" and "the game is stuck reading
this one address" are very different starting points.

### Grand Theft Auto 2 reads its filesystem and then waits

Worth writing down carefully, because the obvious diagnosis is wrong twice over.

It issues 2 177 CD-ROM commands and reads only 351 sectors: a retry loop. The
loop is `Setloc(00:02:16)`, `Setmode(A0)`, `ReadN`, one sector, `Pause`, over and
over, 256 times on that one sector. LBA 16 is the ISO 9660 primary volume
descriptor.

Two things it is **not**:

* **Not bad sector data.** `Setmode(A0)` selects whole-sector reads, 2340 bytes
  from the header rather than 2048 of user data, which no other game here uses.
  The bytes our FIFO serves for that sector are byte-identical to the image:
  `00 02 16 02 | 00 00 09 00 00 00 09 00 | 01 43 44 30`, the header, the
  subheader, and the start of `CD001`.
* **Not a stubbed port.** Two stub reads in the entire run, both to the
  expansion region. It is not spinning on an unemulated register the way Crash
  was spinning on `SPUCNT`.

And it gets **further than the loop suggests**: the sectors actually delivered
are 4 to 11, then 16, 18, 22, 23, 24, 25. That is the licence area, the volume
descriptor, the path table and the root directory. Its filesystem reader works.
It then goes back to re-reading the volume descriptor and never opens a file.

So the failure is after a successful directory read, and the periodic
volume-descriptor read is most likely a heartbeat while it waits for something
else. It has drawn nothing at all beyond the BIOS logo. Open, and not yet
guessed at.

### The PAL BIOS: closed

**Was:** both European BIOS images failed and both American ones worked, with or
without a disc. SCPH-1002 got as far as clearing sound RAM and stopped, with one
stub read, nine stub writes, zero GPU primitives and zero unmapped accesses. It
never reached the display.

**Was not:** PAL video timing, which is implemented, and which the failure was
far too early to have depended on. Nor a disc problem, nor a region check.

**Is:** GPUSTAT bit 31, the parity of the line being drawn, which this core
returned as a constant 0. The PAL BIOS latches GPUSTAT and spins until bit 31
differs from the latched copy. See `docs/notes/GPU.md`. SCPH-1002 now boots to
its main menu, and draws its two menu icons correctly, which is more than the
American BIOS manages: SCPH-1001 still draws colour noise in their place. Two
BIOS versions disagreeing about the same two icons is a better lead on that bug
than either one alone.

The instructive part is how little the symptom said about the cause. Every
counter pointed away from the GPU: nothing was drawn, so the rasterizer was
never suspect, and no port was unmapped or stubbed, so the "software polls what
it writes" rule had nothing to bite on. What found it in one step was asking a
different question, not a better version of the same one: **not what is missing,
but what is it executing.** A histogram of the last million program counters
gave a six-instruction loop, `--peek` disassembled it, `--regs` said the address
it was polling was `1F801814` and the mask was `0x80000000`, and that was the
whole diagnosis.

Both are in `shot` now, and cost nothing when unused:

```
shot bios.bin --pchist              # where the last million instructions went
shot bios.bin --peek 800592F4:12    # disassemble twelve instructions there
shot bios.bin --regs                # the register file at the end of the run
```
