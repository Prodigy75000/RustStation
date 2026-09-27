# Conformance baseline

This file is a chronological lab notebook. Sections are dated and record what
was true when they were written, so a newer finding can supersede an older
one; where it does, the older text carries a short note rather than being
rewritten. For the current state, read "Real games" and its subsections, and
"Netplay" for determinism across machines.

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
| `cpu/access-time` | UNGRADED | Within about a cycle of the console's log in every region since 2026-09-26, but its figures are where the load costs came from. See below |
| `cpu/io-access-bitwidth` | UNGRADED | Prints no verdict lines. Differed from the reference: needs real I/O devices, which were all stubbed at the time |
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

### `access-time`, and what matching it does and does not show

It measures CPU cycles per access to each memory region. Before the timing work
it printed `0.0` everywhere, because it had no working timer to measure with;
then a flat ~1.0, every instruction costing one cycle.

Since 2026-09-26 (the I-cache, load costs, multiplier and GTE waits,
`docs/notes/TIMING.md`) every region is within about a cycle of the log:
RAM 5.1 / 5.1 / 5.7 against 5.21 / 5.3 / 5.14, BIOS 8.13 / 13.7 / 25.1 against
7.6 / 12.94 / 24.94, the SPU 18.6 / 18.0 / 36.6 against 17.99 / 17.99 / 38.94,
the full table in TIMING.md. **This is not independent evidence**: the load
costs were read off this same log. What it does show is that the costs reach
the clock at all, that the test's loop runs from the I-cache at a cycle an
instruction (the figure is a difference of two loops, so a wrong fetch cost
would show), and that LWL and LWR cost by the bytes they take, which the SPU
row's word read, compiled to such a pair, needed.

The independent checks are elsewhere: Final Fantasy VIII's video cadence
(below, and TIMING.md) and libetc's "VSync: timeout", which the suite's own
tests printed about once a frame on the real BIOS and no longer print.

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
| `quad` | **0.000%** | 0.000% | 0.324% | Pixel-exact since the fill rule, 2026-09-25 |
| `lines` | 0.188% | 0.019% | 0.188% | Essentially correct |
| `clut-cache` | 0.176% | 0.174% | 0.977% | |
| `triangle` | 4.990% | **0.000%** | 5.179% | Only the dither phase is left |
| `texture-flip` | 25.586% | 0.800% | 52.782% | Structurally right, see below |
| `vram-to-vram-overlap` | 1.441% | 1.224% | 8.778% | |
| `uv-interpolation` | 4.504% | 3.025% | 7.900% | Affine interpolation precision |
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
SPU, MDEC) was untested at the time because none of those subsystems existed.
(Since superseded: all three have been built; see "CD-ROM", "With sound" and
"Without a BIOS", where the `cdrom`, `spu` and `mdec` folders are run.)

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
carrying beyond the GTE: **when a wide value is read at two widths, the
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

At the time, the BIOS did not reach its shell hand-over on its own, because it
was waiting on a CD-ROM that did not exist yet. (Since superseded: see "CD-ROM",
below, where it reaches its main menu and then reads discs.) Sideloading a PSX-EXE works regardless: the
harness runs until the hand-over point is reached and swaps the binary in there.

## Without a BIOS: the HLE kernel, 2026-09-25

The priority was to catch up with Beetle PSX for players who have no BIOS file.
Design and every finding in [`notes/HLE.md`](notes/HLE.md). What was measured:

**The CPU and hardware suites give the same verdicts on HLE as on the
SCPH-1001 BIOS**, folder by folder (cpu, gte, timers, dma, gpu, mdec, spu,
cdrom, input), with `testrom hle`. Two needed work first. `cpu/cop` stopped
after three of its seventeen checks until an unhandled exception went through
the A table's entry 40h, which the test fills with its own handler.
`cpu/code-in-io` gets further on HLE than on the BIOS only because it boots in
fewer steps; it fails the same checks on both, a CPU gap (code run from the
scratchpad should raise a bus error).

**Kernel tests, seventeen, each proven by breaking what it covers**:
events marked and consumed, callback events running guest code, the heap,
psx-spx's documented memcmp and strstr bugs, setjmp and longjmp, an interrupt
through a queued handler with every register of the interrupted code intact,
ChangeTh there and back, a memory card file created, written, read back and
found, the call gates and dispatchers word for word as the console's,
InitHeap writing nothing, and an unhandled exception through A(40h).

**Spyro saves on HLE** through the libretro core with an empty system
directory: 90 s of mashed input left `BASCUS-94228SPYRO` on the card, its
directory entry byte-identical to the one the same run writes on the real
BIOS, and the same title.

**Three games found three things**, each now in `notes/HLE.md`:

- Crash Bandicoot's libcd timed out on its first command until the CD-ROM
  controller's interrupts were left enabled, as a real BIOS leaves them; and
  its timer 2 went unacknowledged until the kernel's root counter handlers
  acknowledged by default.
- Grand Theft Auto 2 jumped into its own heap: InitHeap wrote a block header
  into memory the game keeps using. It now writes nothing until the first
  malloc, and GTA 2 plays its DMA Design intro. On the same phone,
  Beetle's HLE black-screens on GTA 2.
- Tony Hawk's Pro Skater 2 wrote a 1 through a null pointer into the B table,
  then jumped to address 1 on its next OpenEvent. The tables now sit where the
  console's do, and it draws exactly the picture the BIOS run draws.

**Confirmed on a phone the same evening**, with no BIOS file on it:
Crash Bandicoot runs at full speed on the HLE kernel, where on a real BIOS it
ran about 20% slow (the pinned bug), and its controls work after 1903e7a (the
kernel's pad buffer kept the 5Ah after the pad's ID, so every button landed a
byte late and Crash took no input). So the slowness belongs to the real-BIOS
path, not to the emulator as a whole; the lead below is where to look if it
matters for players who do have a BIOS.

**The maintainer's own library, HLE kernel on an arm64 phone,
2026-09-26** (the phone handles fast-forward up to 3x on this core):

| Game | On HLE |
|---|---|
| Twisted Metal 2 | Works perfectly |
| Grand Theft Auto 2 | Works perfectly (Beetle's HLE black-screens it) |
| Crash Bandicoot | Works perfectly, full speed, controls fixed (1903e7a) |
| Metal Gear Solid | Works perfectly; disc swap not yet reached |
| Final Fantasy VIII | The known video stutter; white screen after the opening video |
| Metal Slug X | Hung on "checking memory card"; fixed the next morning, in two steps |
| Crash Bash | Hung on "Sony Computer Entertainment America presents"; fixed the same day |

No new visual glitches were seen since the Metal Gear Solid briefing fix.

The two hangs were one bug, and only showed with a memory card in, which the
phone always has and the PC survey did not. The kernel's card file functions
work on the card image directly and reported nothing on the low-level card
event (F0000011h); on the console they read the card through the kernel's own
sector routine, which does. Metal Slug X runs firstfile and then waits for
that event. Through the libretro core with a card and no BIOS, Metal Slug X
now reaches its title screen and Crash Bash its game-type menu.

Metal Slug X was then found still hanging on the phone, where Crash Bash
was fixed. The difference was the card: the phone's holds a save made on the real
BIOS, and the PC runs used an empty one. With a save there, the game opens it
asynchronously and loops on read until read returns 0: an asynchronous read
answers "accepted", not a byte count, and the data comes with the event. That
card, copied off the phone, reproduced the hang and now reaches the title
screen, untouched. Lesson: test card paths with a card that has a save on it.

**A lead for the pinned Crash slowness, found on the way and not followed.**
Crash Bandicoot prints "VSync: timeout" about once a frame once it is running,
and Final Fantasy VIII does too, on the real BIOS as much as on HLE (981 and 926
in the runs above). That is Psy-Q's libetc saying its VSync wait ran out
without seeing the vblank count move, which is exactly where a game's pace
comes from. It is an emulator question, not a kernel one.

**The library, 2 billion steps with Start held, HLE against the SCPH-1001
BIOS** (`SURVEY_HLE=1 scripts/survey.sh`, compared with
`tools/survey-compare.mjs`): all thirty-four discs boot on HLE, and not one
logs a kernel complaint. None has an unmapped access or an unknown CD command.
Most are further along, having skipped a 200-million-step intro: 22 read more
sectors, and every one that decodes video decodes more of it (Dragon Ball GT
went from a black screen to 276 000 macroblocks). Grand Theft Auto 2 and Tony
Hawk's Pro Skater 2 draw exactly the picture the BIOS run does. The rows with
fewer lit pixels (Crash, Final Fantasy VIII, Tarzan, Ace Combat 2) were each
looked at: a loading screen, the black between two credits, and two videos
caught mid-fade, not failures. Final Fantasy VIII's CD commands on the two
kernels are the same sequence, HLE's just further on.

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

**At the time, nothing in `cdrom/` could be graded, and that was the honest
position.** All four tests needed something this core did not have. (Since
superseded in part: the drive now reads real discs; see "With a disc" below and
"Real games".)

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

**What it did not yet prove:** that a real game boots. The synthetic disc has
no filesystem, no `SYSTEM.CNF` and no executable, so everything past "the BIOS
likes this disc" was untested. The timing constants remained approximate.
(Since superseded: real games boot; see "Real games".)

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
and audio emitted every frame even though at the time it was silence (the SPU has
since gained its voices: see "With sound"), which a frontend needs
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

At the time still not proven, and unchanged by any of this: **nothing had been
driven with input that changes.** Every run held one button from boot. Crash
Bandicoot's counters were identical with Start held and with nothing held at all.
(Since superseded: `retrohost --mash` and `--press` drive changing input, and
the core has been played by hand on a phone; see "On a phone" and "Netplay".)

### Android

`scripts/deploy-android-debug.sh so` cross-compiles the core to
`aarch64-linux-android`: a 580 KB `ELF64 DYN AArch64` shared object with all
twenty-five libretro entry points exported, `ruststation_state_token` beside
them, and 16 KB-aligned `LOAD` segments, which Play requires. The NDK linker
and the alignment flag have to be supplied through cargo configuration.

**A pushed `.so` will not be picked up** by an Android frontend that resolves
cores by bare filename through `dlopen`: the linker finds them in the APK's own
native library directory, so the APK has to be rebuilt and reinstalled.

## Netplay: the same on every machine, 2026-09-26

Netplay over libretro sends only the buttons. Each peer runs its own copy of
the game, peers compare hashes of `retro_serialize` to notice a split, and a
peer that has split loads the host's state. So two things have to hold, and
both are tested through the shipped library, not the core directly:

1. **The same content and presses give the same bytes on every machine.**
2. **A peer that loads the host's state carries on exactly as the host does**,
   whatever it was doing before.

`retrohost --hash-every N` prints, every N frames, a hash of the serialized
state and of the video frames and audio samples since the last line.
`--save-at` and `--load-at` take and restore a state mid-run.

**Across machines.** Tekken 3 for 7 200 frames with `--mash` on a Windows
PC (x86-64), a phone and a tablet (both arm64), all three from
the same image by md5: all 120 lines equal, state, video and audio, and the
states saved at frame 3 600 byte-identical. Final Fantasy VIII (its opening
video, so the MDEC and XA audio) and Metal Gear Solid (Europe, PAL), 3 600
frames each on the PC and the tablet: 60 of 60 lines equal for both.

**Resync.** A run with its presses shifted by 3 000 frames, so a different
history, loads the PC's frame-3 600 Tekken state at its own frame 600: every
line after that equals the PC's, on the tablet and on the phone. The same test on the PC with
Tekken and MGS, at five load points for MGS: equal throughout. Shifting the
presses by one frame instead makes all 15 compared lines differ, which is the
check that the comparison can fail.

**What it found.** `run_frame_at`, for a frontend pacing at one standard
while the video runs the other, carries the cycles its last instruction ran
past the frame's end and a fraction of a cycle. Both were kept out of the
state as host-side, on the reasoning that where frames end does not change
the machine. It does under netplay: the frame's end is where the next input
lands. A resynced peer kept its own carry and ended every later frame a few
cycles off the host's. `a_resynced_peer_ends_its_frames_where_the_host_does`
failed at 29 cycles on the first frame, and passes since both are serialized
(save format 16). None of the game runs above tripped it: the carry is only
non-zero while the video standard differs from the declared one.

**Why it holds.** The core is integer-only: its one floating-point use is in
a test. It has no `unsafe`, no hash maps, no clocks and no threads, and it
reads environment variables only to turn on trace output.

**Not proven: the memory card.** What is on a card stays out of the state on
purpose, so loading one never takes back a save. Under netplay, each peer has
its own card. Tekken 3 with a card holding another game's save and with none
hashes identically over 1 800 frames, because it finds no save of its own
either way. A game that finds its own save on one peer's card and not the
other's will split them, and a resync cannot fix that. It needs the peers to
play from the same card.

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

The survey was re-run once after the SPU gained its voices and once after CD
audio, against the table above. **Nothing regressed**: 27 of 31 discs match to
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

What the survey does not measure is sound. The recordings `shot --wav` makes
are for a listener, and they have had one: on 2026-09-24 the maintainer listened to
30 seconds from power-on of six games and recognised every one, the boot
sound first and then Crash Bandicoot, CTR, Mega Man X5, Metal Slug X, Spyro
and Tekken 3, all judged to sound right. The last three exercise CD audio.

That is recognition by ear, not a comparison against a console, and no
reverb exists yet, so it says the voices, envelopes, pitch and CD paths are
broadly right, not that they are exact. Metal Slug X is silent until about 27
seconds; the recording measures exact zeros from 8 s to 25 s, so that is the
game loading, not missing audio.

### On a phone, 2026-09-24

The first time this core ran anywhere but the desktop: the libretro core in an
Android libretro frontend, in a debug build beside Beetle PSX, played by hand
with real, changing input.

- **60 fps flat on every disc tried**, five of them including Twisted Metal.
  The interpreter's speed was the open risk; on this phone it is not one.
- **RetroAchievements rich presence worked** (GTA 2 showed the lives count), which
  is the memory map exercised end to end by something other than this repo.
- The real PlayStation boot sequence, and no long black gap after it.

Found, and fixed the same day, each reproduced on the desktop first:

- **Videos in 24-bit colour looked "very strange"** (GTA 2, Twisted Metal) and
  the games were perfect from their menus on. The display read 24-bit as
  15-bit. **Confirmed fixed on the device.**
- **A garbage row at the bottom of the licence screen**, and the strip under
  CTR's menu: the height ignored GP1(07h). **Confirmed fixed on the device.**
- **A Japanese BIOS next to an American one sent every American game to the
  Japanese BIOS menu.** The BIOS is now chosen by the disc's region. Reproduced
  and fixed through retrohost; not yet re-tried on the device.

Still open:

- **An occasional stall** after which the frontend reports the game as still loaded
  until it is restarted. **Found and fixed.** retrohost timed every frame of
  every disc under 60 seconds of mashed input: 30 discs never exceeded 35 ms,
  and Metal Slug X took **137 seconds on one frame**. Replayed from a state
  saved before it, the time was one `sw` starting DMA channel 2 on an ordering
  table with a three-node loop, walked to the million-node bound. The walk now
  stops at the first node it revisits (`docs/notes/DMA.md`); the same frame
  takes 0.08 s and the run reaches gameplay. On a phone that frame is the
  freeze, and the unload waiting behind it is "game already loaded".
- **Which BIOS the phone ran.** None was installed, yet the core then refused
  a disc without one and had no fallback. The core now logs the directory it
  searched and the BIOS it took through the frontend's logger, so the next run
  on the phone answers it. (Since superseded 2026-09-25: with no BIOS the core
  falls back to its HLE kernel; see "Without a BIOS".)

### Against Beetle PSX on the same phone, 2026-09-24

Switching between the two cores on the same discs:

- **GTA 2 runs on this core and black-screens on Beetle** (with and without a
  BIOS).
- **No long black waits before a game starts**; Beetle has them.
- The real Sony boot sequence, where Beetle without a BIOS shows its own.
- Every disc tried plays at 60 fps, except the two multi-disc games (Final
  Fantasy VIII, Metal Gear Solid), which did not load: there was no .m3u or
  disc-swap support. **Added 2026-09-25**: `.m3u` playlists and the libretro
  disk-control interface, with the drive's lid (`docs/notes/CDROM.md`).
  Metal Gear Solid (Europe) boots from a two-disc playlist like the Android app
  writes, and `retrohost --swap-at FRAME DISC` swaps the way an Android
  frontend does. What is not tested: a game's own "insert disc 2" prompt, which
  is hours into both games and could not be reached without memory cards
  (since added: see "Memory cards, 2026-09-25").

Crash Bash, which stalled on the "SCEA presents" screen, was an anti-modchip
check (CD Test 04h/05h, now answered with zero SCEx counts). It reaches its
menu here and **attract-mode gameplay on the tablet**, confirmed on device.
Dino Crisis had the same check and now reaches its in-engine intro.

**Crash Bandicoot ran about 20% slow on the phone** while the counter read 60.
Not a timing error: the emulated console was on time (30 game frames and 59
to 60 vblanks per second). The tablet simply needed 17.0 ms per frame, 22 ms
at worst, to emulate a saved gameplay state, against 16.7 ms. Twisted Metal
2's gameplay needed 14.0 ms, which is why it ran at full speed. 64% of
Crash's instructions were its vsync wait, which is now skipped exactly
(`docs/notes/TIMING.md`): **9.8 ms on the tablet**, Twisted Metal 2 6.7 ms.
`shot --pace` prints vblanks, flips and wall time per second of frames, and
the ARM build of `shot` runs on the tablet over adb.

**Still open, pinned by the maintainer: Crash Bandicoot plays about 20% slow by
eye on the phone, and it is not performance.** The two speed-ups above (Crash's
busy frame 19.5 ms to 14.8 on the tablet, host perf line at 60 fps) did not
change what is seen on the phone. Music plays at the right speed, the counter reads
60, and every other game tried plays at the right speed, Crash Bash, Twisted
Metal 2 and Metal Slug X included. The spin "does not consistently spin" when
jump and spin are pressed together. What is established: the emulated console
is on time (60 frontend frames = 33 868 800 cycles, 59 to 60 vblanks a second)
and Crash flips its picture 30 times a second in gameplay. So the next place to
look is how Crash measures time or input: whether it paces movement from a
root counter or the vblank count rather than from frames, whether the
one-cycle-per-instruction CPU (roughly twice a real one) changes that, and
whether the pad is read in time for a same-frame jump and spin. The quickest
discriminator is a side-by-side against Beetle PSX on the same save state,
timing one fixed stretch of a level in real seconds. (Since then: on the HLE
kernel Crash runs at full speed on the phone, which places the slowness on the
real-BIOS path; see "Without a BIOS". The one-cycle-per-instruction CPU was
replaced by instruction timing on 2026-09-26.)

**First run of the multi-disc build on the phone, 2026-09-25.** Final Fantasy
VIII and Metal Gear Solid boot, and the Disc entry in the pause menu lists four
and two discs with the first selected. Then, as reported from the phone:

- **Metal Gear Solid took no input**: nothing skipped the intro, nothing
  worked on "press start". It asks the pad whether it is a DualShock (`43h`,
  `45h`) and never reads the buttons until something says yes. The pad is a
  DualShock now (`docs/notes/SIO.md`); the game reads its buttons and reaches
  the difficulty menu. Resident Evil 3 now switches the pad to analog itself,
  as it would on a console.
- **A white line down the middle of the Konami logo**: two quads sharing the
  column at x = 160, both drawing it. The polygon fill rule, which also made
  the suite's `quad` test pixel-exact (`docs/notes/GPU.md`, item 5).
- **Final Fantasy VIII's opening video hitched about once a second**: the fixed
  1/60 s frame, one in 84 without a vblank (`docs/notes/TIMING.md`).
- **Final Fantasy VIII stops on a white screen after its logo.** Not
  reproduced. With disc 1 here, the opening video ends in a fade to white of
  about two seconds and the game goes on into the infirmary scene, untouched.
  Needs a state from the phone taken on the white screen.
  **Fixed 2026-09-26 by instruction timing (821bf93).** It was confirmed on
  the phone that it hung before that build and goes on after it. A state taken on the logo
  just before the white reaches the infirmary here in 30 seconds. Why the
  one-cycle CPU hung it was not found: it never reproduced on the PC, which
  had no memory card and one disc where the phone had a card and a 4-disc
  m3u.

**All seven of the maintainer's discs play on the HLE kernel, 2026-09-26**:
Twisted Metal 2, Grand Theft Auto 2, Crash Bandicoot, Metal Gear Solid,
Crash Bash, Metal Slug X and Final Fantasy VIII, with no BIOS file.
The same day, five more pushed to the phone: Spyro the Dragon, Tekken 3,
Crash Team Racing, Tony Hawk's Pro Skater 2 and Resident Evil 3.
All play well at full speed. Resident Evil 3 unlocked a RetroAchievement
("Easy", a dodge before the warehouse) from a real dodge in play, so the
memory RetroAchievements reads is the game's own. Twelve of twelve on HLE.

Rayman, Silent Hill (USA) and Yu-Gi-Oh! Forbidden Memories then played
perfectly on HLE too (2026-09-26): fifteen of fifteen, and no disc tried has
yet failed to boot on the built-in kernel. On that record the libretro core
stopped reading BIOS files at all: it always boots the HLE kernel, so every
copy runs the same kernel and a forgotten BIOS file cannot split two netplay
peers (one had been sitting unseen in a tablet's system directory). Rayman
also popped a RetroAchievement ("refill health from one hit point") on
loading a save state. The core's memory was right, since rich presence read
correctly; the frontend did not reset the achievement runtime across the
load, so the jump in memory looked like the condition. Reported to the
frontend, and not a core bug.

**CHD, 2026-09-26.** CHD images are read (`psx-chd`, on the pure-Rust `chd`
crate), checked against BIN/CUE by building CHDs with chdman and comparing
every sector: Tekken 3 and Tomb Raider (57 tracks, default codecs and zstd)
are identical, and each layout detail broken on purpose makes thousands of
sectors differ. Through the shipped library, Tekken 3 hashes the same (state,
video, audio) from its CHD, from its cue sheet, and from an `.m3u` listing the
CHD, on the PC and on an arm64 tablet. Details in
[`notes/DISC.md`](notes/DISC.md).
Then a CHD not made here: a player's own Bomberman (USA) image, made by
other tools, imported on a phone, recognised as PlayStation, and played well
on v0.2.0.

**Port 2 had no pad, v0.2.1.** In the first two-player netplay session on a
phone, Bomberman: Party Edition and Twisted Metal 2 would not let player 2 be
anything but the computer, with the session itself stable. The libretro core
connected the pad in port 1 and never the one in port 2, so a game polling
port 2 got FFh and no acknowledge, which is exactly how an empty port looks,
and correctly concluded nobody was there. The frontend's port 2 input was
written into a pad the SIO would not answer for. Everything measured had been
on one pad: the netplay and reload runs drive port 1, and the survey holds
Start on it, and a port nothing polls cannot report that it is missing. Both
pads are connected at load now, and `a_pad_answers_in_both_ports` sends the
address byte to each port and requires the acknowledge (it fails for port 2
without the fix). Found and diagnosed by the frontend side from the live
session.

Confirmed end to end the same day: a two-player netplay session of a
PlayStation game loaded from a CHD, on v0.2.1, between an arm64 tablet and an
arm64 phone running identical builds. That one run covered the CHD reader, the
HLE kernel, both controller ports and lockstep across two different devices;
the player called it a perfect session, with faster joins than the core it
replaced and no freeze. The README had said "a DualShock in each port" since
long before the code did it, which is the lesson worth keeping: a document
claiming a capability nothing tests reads exactly like one describing a
capability that works.

Then a longer one: Twisted Metal 2, two players, a tablet joining a phone
(both arm64, v0.2.1), fifteen minutes with no freeze and no divergence between
the two machines, and a quick join, which is the state being serialized and
sent across.

**Reset left the devices running, v0.2.2.** Reset black-screened Rayman and
Bomberman: Party Edition on a phone, not every time. It reset the CPU, RAM,
GPU and DMA, and left the drive, the SPU, the timers and the interrupt
controller as they were, so a reset in the middle of a track came back to a
drive still playing and interrupts still raised, and the rebooted kernel spent
its time in its handlers. Reproduced with `retrohost --reset-at`: on v0.2.1
Rayman stayed black after resets at frames 400, 900 and 1 500 (200 and 600
recovered), Bomberman after three of four. Reset is now a power cycle, a new
machine around the same BIOS with the disc, the cards' contents and the
plugged-in pads moved across. After a reset at any of eight points across the
two games, the state 2 100 frames later is byte-identical to a fresh boot's at
2 100. `tests/reset.rs` holds a machine reset after the kernel has woken its
devices to being identical to a new one (the old reset fails it), and checks
that the cards, the pads and the addresses of RAM and save RAM, which a
frontend holds pointers to, survive.
Confirmed on the phone with v0.2.2 installed: resets in both games come back
to a normal boot.

**An Analog button, v0.3.0.** A DualShock powers up digital, and some games
leave switching to analog to the player, who presses the Analog button between
the sticks; with no way to press it, the on-screen sticks did nothing in those
games. The RetroPad has no spare button, so L3 and R3 clicked together are the
Analog button, once per press. They reach the core as ordinary input, so
netplay carries them, and a physical pad has them too. Which frame the pair
was last held on is kept per pad and serialized (format 17): without it, a
peer that loaded its host's state mid-press would toggle on the next frame and
the host would not. The first version tested the pair against `L3 | R3` as a
mask, but the button constants are bit numbers, so it matched Select and L3;
the core test built its input the same wrong way and passed. The libretro test,
which feeds RetroPad ids 14 and 15 through the real input poll, did not share
the mistake and failed, and the core test now includes Select with L3.

v0.3.1 exports `ruststation_analog_mode()`, a bitmask of which pads are in
analog mode, so a frontend's Analog button can light up with the machine's
actual mode rather than a count of presses, which a game locking the mode
would falsify. Through the library in Rayman: no press reads digital, one
L3+R3 press analog, a second digital, and L3 alone nothing.

Ape Escape plays on a phone on the HLE kernel (2026-09-26). It requires a
DualShock and is driven by both sticks, so it is the first game confirmed to
play through the analog path end to end. Sixteen commercial games confirmed on
HLE; none tried has failed to boot.

Alien Resurrection too (2026-09-27): a DualShock game where the left stick,
the right stick and the D-pad each do something different, all three
responding, at a flat 60 fps on the phone. Seventeen on HLE, still none that
fails.

Alone in the Dark: One-Eyed Jack's Revenge and Armored Core play too. The
Armored Core dump is European and runs at 50 fps, which is right: the core
takes the region from the licence text and paces a PAL disc at the PAL
console's 49.76 Hz. Nineteen on HLE, none that fails.

**The first three that failed, 2026-09-27**: Dino Crisis 2 (German), Dead Ball
Zone and Batman of the Future (both European). Three bugs, none what it
first looked like (all three are PAL, and several PAL games use LibCrypt, but
none of them read the subchannel):

* **A(72h) CdRemove did nothing.** Batman calls it, then waits on handle 0,
  which is the kernel's first CD-ROM event. The real BIOS, watched doing it,
  closes the kernel's five CD-ROM events and takes both CD-ROM handlers off
  the interrupt chain, so the wait falls straight through; on the HLE kernel
  it waited forever with the display off. Found by comparing the two kernels'
  calls (`RSTA_HLE_TRACE`): identical up to 512 calls of WaitEvent(0) on the
  console, endless on HLE, then the event table read out of both.
  `cd_remove_closes_the_kernels_cd_events` fails on the old no-op.
* **An MDEC-out transfer completed inside its own start.** Past that, Batman
  and Dino Crisis 2 (the US release too) decoded two frames of video and
  stopped, on the real BIOS as well: the player lost a race it never loses on
  a console. See [`notes/DMA.md`](notes/DMA.md). Both now play: Batman into
  its gameplay, Dino Crisis 2 through its intro.

* **A(70h) _bu_init left a new card new.** Dead Ball Zone hung after its
  piracy screen only with a memory card in, which a phone always has and the
  PC harness did not. The real BIOS, watched with a card in each slot, reads
  each card's sector 0 and writes it back as sector 3Fh, which clears the
  card's "new" flag, so a game asking _card_info next is told the card is
  known. The HLE kernel did not, the game took its new-card path and waited.
  The second card in slot 2, which the libretro core has had since v0.2.0,
  was enough on its own. `bu_init_clears_both_cards_new_flags`; shot gained
  `--card2` to put a card in slot 2.

The survey over every disc, before and after each fix, shows nothing else
moving beyond timing noise, and ten games that use the card play the same with
cards in both slots. The survey also caught one more: the original Dino Crisis
had the same video race, and its video now plays (400 macroblocks decoded
before, 95 400 after). All three failing games now play on the PC with two
cards: Batman into its gameplay, Dino Crisis 2 through its intro, Dead Ball
Zone to its language menu.

**Tekken 3 fights dropped to the low 40s on the phone** where Beetle holds 60
(reported with a state mid-fight). The fight draws at 368x480; the phone
needed about 14 ms a frame at worst times, spiking past 20. Profiled on
the tablet with simpleperf: 63% drawing triangles (44% the loop, 18% texel
fetch), 28% the CPU. Three changes, each checked byte for byte against the
old build's end state after 6 seconds of the fight, VRAM included:

- a row's inside pixels are found as one run from the three edges, rather than
  testing every pixel of the bounding box (about half of which is outside);
- the texture window's masks are worked out once per primitive, not per texel;
- each attribute's per-pixel step is divided once per triangle, not per row.

PC 8.5 to 5.7 ms a frame (worst 12.2 to 8.0); on the phone, the two builds run
back to back, 8.4 to 9.6 against 5.9 to 7.7. The phone is noisy between runs
(heat, which core), and both builds still spike near 20 ms now and then.
`a_row_run_covers_exactly_the_pixels_inside` holds the run to the
per-pixel test over 3 000 random triangles; the suite's GPU images are
unchanged. Also: `shot`'s default run is now 300M cycles, since at 60M
the BIOS, running from its ROM at real cost, had not reached the GPU tests.

**Confirmed on the phone:** Tekken 3 through stage 6 with no
frame drops.
- **Metal Gear Solid's briefing screen showed textures down its right side**
  (second phone run): 320 pixels drawn in the 368 mode with a display range
  to match, and the core showing all 368. The width now comes from GP1(06h)
  the way the height already came from GP1(07h) (`docs/notes/GPU.md`).

**Final Fantasy VIII's video still hitches once a second after the vblank fix,
and this is why.** `RSTA_TIMELINE=1 shot --pace` prints every frame's flip,
sectors and SPU interrupts; `RSTA_SPU_TRACE=1` the voice registers. The
movie's audio is two SPU voices streamed from the data sectors (no XA), and
it is on time: an interrupt every 2 940 samples, 1/15 s. The pictures are
not tied to it. They come every 3 frames, 20 a second, until the buffer runs
dry, the game pauses the audio (pitch 0) and waits about 18 frames, once a
second. With every instruction charged 2 cycles instead of 1, as an
experiment, they come every 4 frames with no gap at all. So the player shows
a picture as soon as it is decoded, and on a console that takes four frames;
this core's CPU is about twice as fast as one and its MDEC instant. A flat
two cycles is not the fix (Crash drops to 20 frames a second with it): the
fix is instruction timing, the I-cache and memory waits, which
`cpu/access-time` in the hardware suite measures.

**Fixed by instruction timing, 2026-09-26.** From the same point in the
video (a state about 770 frames in, reached on the HLE kernel by pressing
Cross, Up, Cross with `retrohost --press`), 3 600 frames: before, 884 pictures, every 3
frames with a stall of up to 19 once a second; after, 900, every 4 frames
(now and then 5) and no stall. Crash Bandicoot keeps its
30 pictures a second in the same run, and both take less host time than
before (Crash 2.4 ms a frame against 2.8 on the PC), because fewer
instructions fit in an emulated frame.

The survey after the pad change, compared against the one before it, moved in the ways a game that finds a DualShock would: Tenchu 2 reaches
its memory card prompt before its video, Resident Evil 3 goes into analog mode
and is earlier in its intro at the snapshot, Dino Crisis is on a video rather
than in-engine. No disc lost its picture or stopped reading.

**Memory cards, 2026-09-25** (`docs/notes/SIO.md`). Every disc with a fresh card and 90 seconds
of mashed input: Spyro wrote a real save, `BASCUS-94228SPYRO`, one block,
checksummed directory entry, title "SPYRO THE DRAGON" and a three-frame icon; the BIOS
memory card screen lists it with its gem icon. Tenchu 2, which stopped on "MEMORY CARD is
not inserted", goes on into its intro and makes the dummy write to sector 3Fh that clears
the new-card flag.

**Confirmed on a phone the same day:** Metal Slug X saved manually from its
options, the game was quit and restarted, and it loaded the save back ("load complete").
The card survives a relaunch through the libretro save RAM interface. When it reaches
storage is up to the frontend; the one tested writes it on pause and on quit, not
periodically, so a crash mid-session loses what was saved since the last pause.

Where Beetle is still ahead: reverb, and years of compatibility across far more than 31 discs.

**Reading the next survey:** `node tools/survey-compare.mjs OLD NEW`. Lit-pixel
counts moved on 2026-09-24 for an output-only reason: the display height now
follows GP1(07h), so CTR's menu is 216 lines rather than 240 and counts fewer
pixels with its sectors and primitives unchanged. Judge emulation changes by
sectors, macroblocks and primitives, not by pixels alone.

### What "gameplay" does and does not mean

As of 2026-08-16: Tekken 3 renders a round in progress and Crash reaches its main
menu. Neither is a claim that either game is *playable*. Every run here held a
single button from boot, and Crash's counters were identical with Start held and
with nothing held at all, so **nothing here tested that anything responds to
input**, only that games get far enough to ask for it. There was no sound
anywhere in this core either. The next honest step was a scripted input sequence
rather than another screenshot. (Since superseded: the SPU, CD audio and
changing input have all arrived, and games have been played by hand; see "With
sound", "On a phone" and "Netplay".)

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
