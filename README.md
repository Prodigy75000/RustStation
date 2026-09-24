<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Prodigy75000
-->

# RustStation

A clean-room **Sony PlayStation (PS1)** emulator core, written from scratch in
Rust: accuracy-first, with **byte-identical, platform-agnostic save states** as a
hard design constraint (for cross-engine netplay and rollback). FamiRust (NES),
SuperRust (SNES), UltraRust (N64), MegaRust (Mega Drive) and PocketRust (GB/GBC)
are sibling cores built the same way.

It is early. Read "Where it actually is" before "Design goals".

## Design goals, in priority order

1. **Correctness / accuracy.** A MIPS R3000A (LSI CW33300) interpreter with both
   pipeline artifacts software can see: the branch delay slot and the load delay
   slot. The CPU is ground against a conformance suite before it is allowed to
   drive anything else, in the same way the SNES 65C816 and the GBA ARM7 were.
2. **Byte-identical, deterministic save states**, a requirement, not a feature.
   Cross-platform cross-engine netplay and rollback are only sound when two
   machines in the same logical state serialize to the *same bytes* on every
   platform, ABI and compiler. See
   [`crates/psx-core/src/save.rs`](crates/psx-core/src/save.rs): every mutable
   field is serialized little-endian in a fixed order; no `usize`, pointer,
   float, or hash-ordering ever enters a state; load is the strict inverse and
   *refuses* truncated, malformed, or over-long buffers. The contract is
   `TrophyHubResources/specs/play/IN_HOUSE_CORE_SAVESTATE_SPEC.md`, and FamiRust
   is its reference implementation.
3. **Clean-room.** Built from hardware documentation only, distilled into
   [`docs/notes/`](docs/notes/). No third-party emulator source is consulted.
   Measured reference *data* may be used as data, and is cited where it is.
   (There is an unrelated open-source Rust PS1 emulator called *Rustation*. It
   is not consulted either; the name similarity is a coincidence worth naming
   once so nobody assumes otherwise.)

## Where it actually is

Working, and confirmed against real hardware behaviour rather than asserted:

- **The BIOS kernel boots** and prints its banner, on every supplied BIOS image,
  with **zero unmapped bus accesses**.
- **`cpu/cop` from the ps1-tests suite passes 17/17.** The rest of the CPU
  baseline, honestly graded, is in [`docs/TESTS.md`](docs/TESTS.md).
- **CPU**: the full R3000A user instruction set, COP0, both delay slots, all
  eight exception causes the console can raise, `LWL`/`LWR`/`SWL`/`SWR`, the
  hardware's fixed divide-by-zero results, and Status `Isc` cache isolation.
- **Memory map**: 2 MB RAM with its KUSEG mirrors, 1 KB scratchpad, 512 KB BIOS,
  and a decoded I/O window whose ports are stubbed and *counted*.
- **Timing**: a master clock with a run-until-next-event scheduler, the
  interrupt controller, video timing (scanlines, HBlank, VBlank) and the three
  root counters. Short-delay timer measurements match the captured hardware log
  exactly; per-frame ones are within 0.2%. Instruction cycle costs are the
  remaining gap, and are blocked on the I-cache.
- **GPU**: VRAM, the GP0/GP1 ports, flat and Gouraud triangles and quads,
  rectangles, lines, semi-transparency, dithering, the mask bit, all four VRAM
  transfers, and **textures** (4/8/15-bit, CLUTs, the texture window, the
  rectangle flips). Three of the suite's image tests are **pixel-exact**
  against their references (`clipping`, `rectangles`, `texture-overflow`) and
  seven of eleven are within 1.3%. See [`docs/TESTS.md`](docs/TESTS.md).
- **DMA**: channel 2 (block and linked-list) and channel 6 (ordering table),
  which is what makes the GPU reachable at all.
- **GTE**: all 15 commands, the register file and the hardware divider.
  **`gte/test-all` passes 1150 of 1150, and `gte-fuzz` matches the hardware log
  byte for byte across all 150 625 lines** of randomised arguments. Nine
  separate hardware behaviours had to be got right to reach that, and each is
  pinned by a unit test; the walk is in [`docs/TESTS.md`](docs/TESTS.md).
- **The BIOS reaches its own main menu**, with the memory card and CD player
  entries and the animated background, which it could not do before the CD-ROM
  controller existed. `cargo run --release --bin shot -- <bios.bin>
  --steps 300000000`. One rendering fault is visible and recorded in
  [`docs/notes/GPU.md`](docs/notes/GPU.md).
- **Commercial games render their own content, all thirty-one on hand.** Tekken 3
  draws a fight in progress, two textured and lit characters with cast shadows on
  a temple stage. Spyro, Tomb Raider, Ace Combat 2, Crash Bandicoot, Harry Potter
  and Need for Speed III reach their title screens and menus. Across the whole
  library there is not one unmapped access, one transfer on an unimplemented DMA
  channel, or one refused CD-ROM command. See [`docs/TESTS.md`](docs/TESTS.md)
  for how far each disc gets, and for what that does and does not mean.
- **MDEC**, the macroblock decoder: the quantisation and IDCT tables, the
  run-length format, the IDCT, colour and monochrome macroblocks, all four
  output depths, and both DMA channels. **Full-motion video plays**: twenty-four
  of the thirty-one discs decode their intro, Tomb Raider's through to its title
  screen. See [`docs/notes/MDEC.md`](docs/notes/MDEC.md).
- **SPU, and games make sound.** All 24 voices: ADPCM decoding with its five
  prediction filters, the pitch counter with 4-point Gaussian interpolation,
  pitch modulation, the noise generator, the ADSR envelope and the volume
  sweeps, key-on/key-off and ENDX, the capture buffers, and the SPU interrupt
  from a voice, a transfer or a capture write. Output is 44 100 Hz stereo
  through libretro, and `shot --wav` records it with peak, RMS and mean per
  channel. Crash Bandicoot and Crash Team Racing play their music through
  their title screens. See [`docs/notes/SPU.md`](docs/notes/SPU.md).
- **CD audio**: CD-DA through `Play`, with reports, auto-pause and the end of
  the disc, and XA-ADPCM decoded in the drive (4- and 8-bit, mono and stereo,
  both rates) and resampled to 44 100 Hz with the documented zigzag filter.
  Filtering by file and channel, the drive's volume matrix, and Mute. Tekken 3's
  and Mega Man X5's intro videos, silent before, now have their sound.
- **CD-ROM**: the controller, seeking, and reads that deliver a sector at a time
  through the data FIFO and DMA channel 3. Real games load through it: several
  read eight thousand sectors in the survey. **With a synthetic disc the BIOS
  also runs its whole recognition sequence and draws the PlayStation licence
  screen**, with the text on it read off the disc. Reproduce that with `python
  tools/fakedisc.py out/fakedisc`.
- **Disc images**: BIN/CUE, with the cue sheet's tracks, pregaps and indices.
  Raw 2352-byte sectors throughout, because a 2048-byte image has no sector
  header for `GetlocL` to report and no room for CD-DA. CHD would be a second
  implementation of the same interface.
- **Controllers**: SIO0, with a digital pad in each of the two ports. Verified
  end to end against the suite's `input/pad`, which prints the buttons it sees:
  holding three prints those three and nothing else.
- **BIOS TTY capture** through the A/B call gates, so a test binary's own verdict
  is readable without a screen.
- **PSX-EXE sideload** at the BIOS shell hand-over point.
- **Save states** meeting the in-house contract: golden-bytes, round-trip,
  cross-instance determinism and reject tests, with the golden test's
  sensitivity proven rather than assumed.
- **libretro core.** A disc image or a PSX-EXE as content, both pads read from
  the frontend's RetroPad with input descriptors published, save states, and the
  RetroAchievements memory surface. It cross-compiles to an Android arm64
  `.so` with `scripts/deploy-android-debug.sh so`. `retrohost` drives the built
  library through the real C ABI and gets the same picture, pixel for pixel, as
  the direct harness does.

Not started:

- **Reverb**, so everything is dry.
- Memory cards, CHD images, the CD-ROM's sub-channel.
- Per-instruction cycle costs. Every instruction is one cycle and
  multiply/divide do not stall.

All five BIOS images on hand, American, European and Japanese, boot to their
main menu. Four of the five draw colour noise where the menu's two icons belong,
which is the oldest open graphics bug here; the fifth, SCPH-1002, draws them
correctly, and that disagreement is the lead. Recorded in
[`docs/TESTS.md`](docs/TESTS.md).

It is still early. Every one of the thirty-one discs on hand draws its own
content and most play their intro video, but that means title screens and menus,
not gameplay. Sound has everything but reverb.
No game has been driven with **changing** input, so "playable" is not a claim
being made. Homebrew and test binaries sideloaded as PSX-EXEs also run, and draw.
`scripts/survey.sh` runs the whole library and writes the table in
[`docs/TESTS.md`](docs/TESTS.md).

## Layout

```
crates/psx-core/       the emulator: cpu, gte, gpu, sio, cdrom, disc, bus, save
crates/psx-libretro/   the C ABI shim (cdylib)
crates/psx-runner/     dev harnesses: psx, testrom, shot, fingerprint, retrohost
bios/                  your BIOS dumps (gitignored)
dumps/                 your disc images and loose binaries (gitignored)
tests/                 vendored third-party test suites (gitignored)
docs/notes/            distilled hardware notes, the clean-room source of truth
docs/ref/              raw third-party reference drops (gitignored)
docs/TESTS.md          the conformance baseline
```

Nothing copyrighted is committed: no BIOS, no disc image, no third-party test
binary, no reference manual. `.gitignore` is written so none of it can land by
accident, and only our own markdown is tracked under `docs/`.

## Running it

Everything needs a BIOS you supply. See [`bios/README.md`](bios/README.md).

```bash
# Boot the BIOS and see how far it gets.
cargo run --release --bin psx -- bios/scph5501.bin --steps 5000000

# Run a conformance binary and read its own verdict.
cargo run --release --bin testrom -- bios/scph5501.bin tests/test-suite/cpu/cop/cop.exe

# Run a whole folder of them. Three verdicts: PASS, FAIL, UNGRADED.
cargo run --release --bin testrom -- bios/scph5501.bin --dir tests/test-suite/cpu \
    --boot-steps 60000000 --steps 30000000

# Prove save-state byte parity across two builds.
cargo run --release --bin fingerprint -- bios/scph5501.bin --steps 1000000

# Boot a disc.
cargo run --release --bin shot -- bios/scph5501.bin --disc game.cue --steps 400000000

# Build the libretro core and drive it the way a frontend would. The system
# directory must hold a BIOS under a name the core looks for (scph1001.bin and
# friends): a dump named after its release is invisible to it.
cargo build --release -p psx-libretro
cargo run --release --bin retrohost -- target/release/psxcore_libretro.dll system/     --content game.cue --frames 3600 --hold start --out frame.png

# The same core for an Android device, arm64.
scripts/deploy-android-debug.sh so

cargo test --workspace
```

## Legal

GPL-3.0-or-later. No BIOS, disc image, or other copyrighted material is
distributed with this repository, and `.gitignore` is written so none can be
committed by accident.
