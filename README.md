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
- **BIOS TTY capture** through the A/B call gates, so a test binary's own verdict
  is readable without a screen.
- **PSX-EXE sideload** at the BIOS shell hand-over point.
- **Save states** meeting the in-house contract: golden-bytes, round-trip,
  cross-instance determinism and reject tests, with the golden test's
  sensitivity proven rather than assumed.
- **libretro shim** with the state and RetroAchievements memory surfaces wired.

Not started:

- GPU, SPU, CD-ROM, DMA, timers, controllers, memory cards.
- The GTE's 15 commands. The register file exists and is serialized; the
  commands are counted and dropped.
- Any instruction timing model. Every instruction is one cycle and
  multiply/divide do not stall.

**A disc will not boot.** The near-term bar is the R3000A passing a CPU
conformance suite, not a game rendering.

## Layout

```
crates/psx-core/       the emulator: cpu, cop0, gte, bus, exe, save
crates/psx-libretro/   the C ABI shim (cdylib)
crates/psx-runner/     dev harnesses: psx, testrom, fingerprint
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

cargo test --workspace
```

## Legal

GPL-3.0-or-later. No BIOS, disc image, or other copyrighted material is
distributed with this repository, and `.gitignore` is written so none can be
committed by accident.
