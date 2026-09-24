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
pattern: the menu draws colour noise across the two menu entries. That one has
its own section below.

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

## The libretro core, as of 2026-08-16

Everything else here links `psx_core` directly, which left **the layer that
actually ships as the one nothing exercised**. `retrohost` closes that: it
`dlopen`s the built library and drives the C ABI the way a frontend does, so a
run proves the shipped artefact rather than the code it was built from.

```
cargo run --release --bin retrohost -- target/release/psxcore_libretro.dll system/ \
    --content "Tomb Raider (USA) (Rev 6).cue" --frames 3600 --hold start --out frame.png
```

```
core: RustStation (PlayStation) 0.1.0
      extensions cue|bin|img|iso|exe|psexe, need_fullpath true
      declared 640x480 max 640x480, 60.00 fps, 44100 Hz, state 3676704 bytes
      16 input descriptors; holding Start
ran 3600 frames: 3600 video callbacks, 2646000 audio frames, last 512x240,
110428 non-black pixels
```

**110 428 is the same figure `shot` reports for the same disc**, so the two
paths agree pixel for pixel. Also confirmed by that run: content loaded from a
path, `XRGB8888` negotiated, a frame never larger than the geometry declared,
and audio emitted every frame even though it is silence, which a frontend needs
or it stalls its own pacing. Booting with no content draws the BIOS main menu,
and a PSX-EXE by path draws too.

### Three things this found

**The RetroPad ids are not in the order their names suggest.** `B` is 0, `Y` is
1 and `SELECT` is 2, so a table written from the names puts Select on the south
face button and Square on Select. Ours did, on four of the sixteen. The table is
now indexed by id, and three tests pin it: the conventional face mapping, that
every pad bit is driven exactly once, and that the labels the frontend shows
agree with what the buttons do. Each was checked against the old table and
fails on it.

**Disc loading was implemented and unreachable.** `valid_extensions` said
`exe|psexe`, so no frontend would ever offer the core a cue sheet. And
`need_fullpath` was false, which for a cue sheet is worse than useless: the
frontend reads the sheet's *text* into a buffer and nothing it points at. Both
are fixed, and a PSX-EXE now loads from a path as well as from a buffer.

**A BIOS has to be named the way the core looks for it.** The system directory
is searched for `scph1001.bin` and its siblings, so a dump named after its
release, which is how they arrive, is invisible. The failure is a core that
loads happily and then refuses content, which reads as a broken core. It is
worth knowing before wondering why a device shows nothing.

### Input, and what is still not proven

Holding a face button changes what the BIOS main menu draws, 305 999 lit pixels
against 305 996, so the frontend's button does reach the emulated pad. That is
the plumbing, not the mapping: the mapping is pinned by the unit tests above,
not by a game.

Still not proven, and unchanged by any of this: **nothing has been driven with
input that changes.** Every run holds one button from boot. Crash Bandicoot's
counters are identical with Start held and with nothing held at all.

### Android

`scripts/deploy-android-debug.sh so` cross-compiles the core to
`aarch64-linux-android`: a 580 KB `ELF64 DYN AArch64` shared object with all
twenty-five libretro entry points exported, `ruststation_state_token` beside
them, and 16 KB-aligned `LOAD` segments, which Play requires. The umbrella's
`.cargo/config.toml` supplies the NDK linker and the alignment flag; cargo finds
it by walking up from the working directory.

**A pushed `.so` will not be picked up.** TrophyHubAndroid resolves cores by
bare filename through `dlopen`, so the linker finds them in the APK's own
native library directory and the APK has to be rebuilt and reinstalled. And
until RustStation has a `CoreSlot` entry there, nothing in the app asks for this
library at all, so `so` mode is the honest one to be running.

## Real games, as of 2026-08-16

Every disc on hand, thirty-one of them, run for the same two billion instructions
with Start held from boot. `scripts/survey.sh` does this and writes a screenshot
and the counters for each; it asserts nothing and cannot fail. The point is to
sort the library by how close each game is, not to grade anything.

The same number of instructions for every disc is deliberate. It makes the rows
comparable to each other. It also means a slow loader and a game that has stopped
dead look alike from the counters alone, which is what the screenshots are for.

**Every one of the thirty-one now draws its own content, and twenty-four decode
video.** Across the whole library there are now **zero unmapped accesses, zero
transfers on unimplemented DMA channels and zero refused CD-ROM commands**. The
survey before this one had nineteen of twenty-one discs decoding nothing at all.

| Game | What is on screen | Non-black | Textured | Macroblocks |
|---|---|---|---|---|
| **Tekken 3** | A fight in progress, on the temple stage | 162 508 | 1 172 835 | 224 |
| **Ace Combat 2** (PAL) | **Its main menu**, over the cloud backdrop | 153 301 | 145 483 | 800 |
| Digimon World 2 | Its intro video | 130 445 | 600 | 390 000 |
| **Harry Potter** | **The language-select screen**, three flags over cloud | 122 368 | 563 012 | 37 500 |
| Spider-Man | Its intro video | 120 616 | 880 | 60 900 |
| Tony Hawk's Pro Skater 2 | A loading or logo screen | 116 520 | 23 659 | 0 |
| **Spyro the Dragon** | **Its title screen**, Spyro on the plinth, "press start" | 114 680 | 674 341 | 0 |
| **Tomb Raider** | **Its title screen**, Lara posed, after the whole intro video | 110 428 | 17 610 | 46 860 |
| **CTR: Crash Team Racing** | **Its main menu** (2026-09-24, from the SPU interrupt; was "something, barely") | 117 080 | 254 440 | 0 |
| **Crash Bandicoot** | **Its main menu** | 106 415 | 1 073 262 | 0 |
| Legacy of Kain: Soul Reaver | Its intro video | 103 059 | 6 920 | 95 700 |
| **Need for Speed III** | **The "game setup" menu**, fully laid out | 100 952 | 558 571 | 480 |
| Mega Man X6 | Its intro video | 76 800 | 3 068 | 219 600 |
| Tenchu 2 | Its intro video | 76 800 | 819 | 265 200 |
| Yu-Gi-Oh! Forbidden Memories | The Konami logo | 76 800 | 1 288 | 0 |
| Medal of Honor | Its intro video | 76 636 | 4 506 | 133 800 |
| **Mortal Kombat 4** | Its attract sequence | 76 333 | 313 713 | 1 220 |
| Beyblade | Its intro video | 76 306 | 1 376 | 193 800 |
| Mega Man X5 | Its intro video | 76 292 | 600 | 228 900 |
| **Disney's Hercules** | Its intro, with 3D drawing alongside | 75 235 | 76 833 | 117 300 |
| Medal of Honor: Underground | Its intro video | 66 777 | 3 881 | 137 700 |
| Silent Hill | Its intro video | 64 845 | 3 812 | 160 160 |
| **Dino Crisis 2** | An in-game menu over a moving grid | 60 905 | 282 219 | 0 |
| **Grand Theft Auto 2** | Its intro | 58 117 | 95 275 | 300 |
| Disney's Tarzan | Its intro video | 54 439 | 632 | 217 500 |
| Dino Crisis | The content warning, over a lit 3D corridor | 42 116 | 600 | 0 |
| **Metal Slug X** | Its attract-mode high score table | 31 727 | 763 283 | 0 |
| Resident Evil 3 | Its intro video, the "NEMESIS" card mid-fade | 4 489 | 4 412 | 46 200 |
| Crash Bash | Something, barely | 4 135 | 600 | 0 |
| Dragon Ball GT | Black at the sampling instant, 4 800 macroblocks decoded | 0 | 22 728 | 4 800 |
| Suikoden II | Black at the sampling instant, 84 560 macroblocks decoded | 0 | 2 478 | 84 560 |

The last two are not stalls. Both are decoding video and both landed on a black
frame at the instruction the screenshot was taken, which is the survey's fixed
clock doing what it is supposed to do rather than a fault.

**Nothing regressed.** Crash Bandicoot's counters are identical to the previous
survey to the pixel and the primitive, and so are Metal Slug X's and Dino
Crisis 2's.

### With sound, 2026-09-24

The survey was re-run twice after the SPU gained its voices and again after CD
audio, against the table above. **Nothing regressed**: 29 of 31 discs match to
the pixel, the sector, the macroblock and the primitive, and the CD audio build
matches the SPU build exactly on every disc.

**One disc moved, a long way.** CTR went from 3 791 lit pixels and 600
textured primitives to 117 080 and 254 440, and from a nearly black screen to
its main menu. psx-spx names Crash Team Racing among the games that rely on
the SPU interrupt and its capture buffers, which did not exist before. Its
screenshot shows a strip of noise along the bottom edge, not yet looked at.

Harry Potter reads 1 269 sectors instead of 1 343 and Need for Speed III 6 400
instead of 6 248, both with the same picture, and Crash Bandicoot's primitive
count moved by under 1%. Those are timing shifts from the SPU now taking sync
points, not changes in what the games reach.

What the survey does not measure is sound, and nothing here grades it. The
recordings `shot --wav` makes are for a listener.

### What "gameplay" does and does not mean

Tekken 3 renders a round in progress and Crash reaches its main menu. Neither is
a claim that either game is *playable*. Every run here holds a single button from
boot, and Crash's counters are identical with Start held and with nothing held at
all, so **nothing here has tested that anything responds to input**, only that
games get far enough to ask for it. There is no sound anywhere in this core
either. The next honest step is a scripted input sequence rather than another
screenshot.

Tomb Raider's 57-track, one-file-per-track cue sheet parsed correctly, which is
the multi-file case `docs/notes/DISC.md` listed as implemented but untested.

### Grand Theft Auto 2: closed

**Was:** 2 177 CD-ROM commands for 351 sectors, a retry loop re-reading LBA 16,
the ISO 9660 primary volume descriptor, 256 times. It read the licence area, the
volume descriptor, the path table and the root directory successfully and then
never opened a file.

**Was not,** and both of these were checked before anything was changed: not bad
sector data, because the bytes our FIFO served were byte-identical to the disc
image, and not a stubbed port, because the whole run made two stub reads and no
unmapped access at all.

**Is:** re-arming the CD-ROM's request register partway through a sector rewound
the data FIFO to the start of it. The game reads the twelve bytes of header and
subheader from a whole-sector read, sets the bit again, and expects the user data
to follow; it got the header again, so every file it read was twelve bytes out of
step. See `docs/notes/CDROM.md`.

**What actually found it** is worth keeping, because none of the counters could
have. Every one of them said the CD-ROM was fine. The trace was changed to print
the FIFO's read *position* at the moment of each reload rather than the data it
was serving: 255 of 606 reloads happened at position 12, and a reload at a
non-zero position states the bug outright. The general form of that is to
instrument the thing that is *supposed* to be invariant, not the thing that looks
wrong.

### The DMA controller's byte lanes: closed, and it was most of the library

**Was:** the largest single blocker this core has had. Nineteen of twenty-one
discs decoded zero macroblocks. Tomb Raider decoded exactly one frame of its
intro and then waited forever on a flag nothing wrote. Resident Evil 3 read 6 429
sectors, drew nothing at all, and sat in the BIOS kernel's `TestEvent` waiting on
an event that never fired. Medal of Honor made 125 million unmapped reads walking
consecutive addresses past the end of RAM. Those looked like four unrelated bugs.

**Was not:** the decoder, which was finished and correct; the drive, which was
delivering everything asked of it; or the interrupt controller. Every instrument
said the machine was healthy. For Tomb Raider the interrupt histogram read
`vblank 5201/5252, cdrom 12600/12601, dma 719/719`, delivered over raised, so
handlers were running and acknowledging; no command was refused, no port stubbed,
no access unmapped.

**Is:** `DICR`, the DMA controller's interrupt register, is laid out so that one
byte holds all seven per-channel interrupt enables plus the master enable. A game
arms a channel with a single `sb` to `DICR+2`. This core ignored access width
entirely: it answered the byte read with the *low* byte of the word, a different
field, and then stored the byte the game wrote back as the whole register. The
enables and the master enable were wiped in the same instruction, and no DMA
completion interrupt was ever delivered again.

Everything downstream followed from that. Tomb Raider's CD sector handler marks a
ring slot "transfer started" and relies on the channel-3 completion interrupt to
promote it to "ready"; with no interrupt the slot never advanced, its `StGetNext`
equivalent spun two million times, timed out, and handed the decoder an empty
buffer. See `docs/notes/DMA.md`.

Afterwards, from the same two billion instructions: Tomb Raider **46 860**
macroblocks rather than 480, and its title screen; Resident Evil 3 **46 200**
rather than nothing at all; Digimon World 2 **390 000**; and Medal of Honor's 125
million unmapped reads gone entirely, along with every other unmapped access in
the library.

**How it was found**, because the counters could not:

1. `--pchist` gave the spin loop and `--peek` disassembled it into a two-part
   test on a stream context: open at `+0x88`, frame-ready at `+0x30`.
2. `--watch` on the frame-ready flag said **nothing ever wrote it**, and a scan
   of every non-stack store to `+0x30` in RAM found its one setter, which runs
   from the decoder's output-DMA completion.
3. The region view added to `--pchist` for this showed the game's CD interrupt
   handler *was* running during the stall, at 0.04% of a million instructions.
   That ruled out "the interrupt is not arriving" by measurement rather than by
   argument. It needed a cut at a few hundredths of a percent to see, which is
   why the cut is there now.
4. `RSTA_DMA_TRACE=1` on every channel, not just the decoder's, showed the game
   reading nine chunks of a frame into its ring and then handing the decoder a
   different buffer that was still empty.
5. `--watch` on the ring slot's state word gave `0 -> 0x0160 -> 3`, and its
   `StGetNext` only accepts 2. Nothing in the whole run ever wrote 2, and what
   writes 2 is the DMA completion path.

The generalisable form is in `docs/notes/DMA.md`: **a register block that is 32
bits wide is not accessed 32 bits at a time**, and a handler that ignores width
destroys the fields around whichever one software was poking at. The other
register blocks here have not been audited for it yet.

### The drive has to demultiplex the video stream

Found on the way and fixed separately, because it is right whether or not
anything currently depends on it. Full-motion video is one CD-XA stream with
video and audio sectors interleaved: Tomb Raider's is seven video then one audio,
over and over. The two are told apart only by the subheader, and the stream is
read with the 2048-byte sector size, which does not include the subheader. So
software cannot do it and the drive must.

`Setmode` bit 6 arms it: a real-time Form 2 audio sector is consumed by the drive
and never raises `INT1`. See `docs/notes/CDROM.md`.

**Honest accounting: this was not what unblocked anything.** Disabling it again,
with the DMA fix in place, leaves Tomb Raider's figures identical to the pixel.
It is in because it is what the hardware does, and because a game reading with
the small sector size has no other defence. CTR is the one disc where it shows in
the counters: 6 945 of its 8 035 sectors are XA audio, which is its streamed
music.

### The BIOS menu's colour noise, narrowed

Four of the five BIOS images draw rainbow noise across the two main-menu
entries. SCPH-1002 does not. This is the oldest open graphics bug here and it
had no characterisation at all beyond the screenshot; it now has one, and three
suspects have been eliminated by measurement rather than by argument.

**The chain, end to end.** The artefact on screen is one 216x70 quad per entry,
sampling a 108x35 fifteen-bit texture at VRAM page 896,0 at two-times
magnification. That texture arrives by a CPU-to-VRAM transfer. Sampling the
middle row of every transfer in the run shows it is byte-identical to an earlier
**read** of VRAM at 640,0, except that about 44% of its pixels have been zeroed
by the CPU in between, which is a colour key. And what is at 640,0 to be read
back is a two-by-two grid of Gouraud quads that the BIOS draws there itself.

So the whole of the defect is upstream of the GPU's texturing: the source
picture is already wrong when it is captured, and everything after the capture
is faithful.

**What those quads are drawn with.** Their vertex colours are fully saturated:
every channel is exactly `00` or `FF`, in six frames of shifting primaries. That
is the anomaly, and what makes it one is the company it keeps. Every other
primitive drawn into the same scratch area, before and after, is sensible: the
background sphere is `8080FF` at its centre and `000020` at its edges, and the
later glow meshes are `9925FF` over `17052D`, `FFCC01` over `2D1E01`, a bright
vertex and a dark surround each time. Only the six captured frames saturate.

**Eliminated, each by a counter rather than by reasoning:**

* **The VRAM readback path.** The BIOS also copies a 120x120 sphere from 640,0
  to 704,0 through the CPU in the same way. Its checksum in and its checksum out
  are identical, so read-back to RAM and write-back to VRAM are byte-exact.
* **Transfers cut short.** A read transfer abandoned halfway would leave the
  tail of the destination buffer holding stale memory, which is a very good
  imitation of this bug. `Gpu::abandoned_transfers` counts them and the whole
  boot has none, on any BIOS.
* **The GTE.** Channels that are all exactly 0 or 255 look exactly like colour
  clamping, and the GTE is where a PlayStation usually computes vertex colours.
  `Gte::colour_saturations` counts every channel it clamps, and the whole boot
  clamps none, on either a working or a failing BIOS. Those colours are not the
  GTE's.

**Not yet answered:** where the BIOS gets them, then. They arrive in the GP0
command words, so this core drew what it was told; the question has moved out of
the GPU and into whatever the BIOS computed them from. Also unsettled is whether
a rainbow is wrong *at all*: the shape being keyed out of it might be the real
defect, and the colours a red herring.

Reproduce with:

```
RSTA_GPU_TRACE=1  shot <bios.bin> --steps 500000000   # transfers, with checksums
RSTA_GPU_REGION=660,5,730,30 shot <bios.bin> ...      # what drew that corner
```

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

All of these are in `shot` now, and cost nothing when unused:

```
shot bios.bin --pchist              # where the last million instructions went,
                                    #   ranked, and grouped into code regions
shot bios.bin --peek 800592F4:12    # disassemble twelve instructions there
shot bios.bin --regs                # the register file at the end of the run
shot bios.bin --watch 801D55B0      # who writes this word, and from where
RSTA_DMA_TRACE=1   shot ...         # every DMA transfer: channel, size, address
RSTA_CDROM_TRACE=1 shot ...         # every command, response and sector
```

The **region grouping** on `--pchist` was added for the DMA fault above and is
the half that earned its place. One hot spin loop takes 97% of a million
instructions and fills the ranking, and the interesting question is what *else*
is still running: an interrupt handler firing four times in that window is four
hundred instructions, which never places in a top-16 list and settles the
question the moment you can see it.
