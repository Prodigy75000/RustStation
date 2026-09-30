<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Prodigy75000
-->

# RustStation

A clean-room Sony PlayStation (PS1) emulator core written from scratch in Rust.
No C, no bindings, and no lifted code, just the hardware modelled from the docs.
It boots commercial games on its own built-in BIOS, plays them at full speed
on a phone, and its save states are byte-identical on every machine, which is
what makes netplay between a PC and a phone sound.

## Status

| Component | State |
|-----------|-------|
| CPU (MIPS R3000A) | ✅ full user instruction set, COP0, both delay slots, every exception the console raises, `LWL`/`LWR`/`SWL`/`SWR`, the hardware's divide-by-zero results, cache isolation. ps1-tests `cpu/cop` 17/17 |
| Instruction timing | ✅ the I-cache (tags, line fills, `FlushCache`), load costs by region from the hardware access-time log, multiply/divide and GTE interlocks (`docs/notes/TIMING.md`). Not yet: the write queue, and DMA bus time, except that a transfer out of the MDEC finishes a word a cycle after it starts, not at once |
| GTE | ✅ all 15 commands, the register file and the divider. `gte/test-all` 1150/1150, and `gte-fuzz` byte-identical to the hardware log over all 150 625 lines |
| GPU | ✅ flat, Gouraud and textured polygons, rectangles, lines, semi-transparency, dithering, the mask bit, VRAM transfers, 4/8/15-bit textures and the texture window, 15- and 24-bit display, display size from GP1. Three of the suite's image tests pixel-exact |
| MDEC | ✅ run-length decoding, the IDCT, colour and monochrome macroblocks, all four output depths. Full-motion video plays |
| SPU | ✅ all 24 voices: ADPCM, Gaussian interpolation, pitch modulation, noise, ADSR, volume sweeps, capture buffers, the SPU interrupt. Not yet: reverb, so everything is dry |
| CD-ROM | ✅ the controller, seeks, data reads through the FIFO and DMA, CD-DA with reports, XA-ADPCM decoded and resampled in the drive, the volume matrix, the lid for disc swaps |
| Disc images | ✅ BIN/CUE (tracks, pregaps, indices), CHD (every CD codec, zstd included), and `.m3u` playlists of either for multi-disc games |
| DMA, timers, interrupts | ✅ every DMA channel with something behind it, the three root counters, video timing, the interrupt controller |
| Controllers | ✅ a DualShock in each port: digital and analog modes, config mode, rumble mapping. L3 and R3 clicked together press the Analog button, so games that leave analog mode to the player can have it |
| Memory cards | ✅ both slots. Slot 1 is libretro save RAM, kept per game by the frontend; slot 2 is one card shared by every game, kept by the core in the save directory. Card contents stay out of save states, so loading one never takes back a save |
| HLE kernel | ✅ boots every game, with no BIOS file: the kernel's A, B and C functions, exceptions, events, threads, pads, memory card files and the CD file system, written from psx-spx (`docs/notes/HLE.md`) |
| Save states | ✅ fixed size, little-endian, versioned; byte-identical on x86-64 and arm64 (`docs/SAVESTATE.md`) |
| Netplay | ✅ proven deterministic across machines: the same presses give the same state, video and audio on a Windows PC and two arm64 Android devices, and a peer that loads its host's state mid-game stays in step (`docs/TESTS.md`, "Netplay") |
| libretro | ✅ content as disc image, playlist or PSX-EXE, both pads with input descriptors, disk control, save states, save RAM, a memory map (main RAM and scratchpad) for RetroAchievements |

Compatibility: every disc in a 34-disc test library boots and plays its own
content on both the real BIOS and the HLE kernel, and twenty-seven commercial games
have been played on a phone at full speed with no BIOS file, among them Final
Fantasy VIII, Metal Gear Solid, Silent Hill, Castlevania: Symphony of the Night, Gran Turismo, Ape Escape,
Alien Resurrection, Armored Core, Dino Crisis 2, Tekken 3, Crash Team Racing,
Resident Evil 3, Rayman, Spyro the Dragon, Tony Hawk's Pro Skater 2, Yu-Gi-Oh!
Forbidden Memories and Grand Theft Auto 2. No disc tried so far has failed to
boot on the built-in kernel. The running
record, including what did not work and why, is [`docs/TESTS.md`](docs/TESTS.md).

## How it is timed

A master clock and a run-until-next-event scheduler drive the video timing, the
root counters, the CD-ROM, the SPU and DMA; the CPU never runs past the next
pending event. Each instruction costs what it costs on the console: fetches go
through an I-cache tag model, loads pay by region and width, and reads of the
multiplier or the GTE wait until the result is ready. A game's own vsync wait
loop is recognised and skipped in one step, charged exactly what the loop would
have cost, so idle frames are cheap on a phone without changing the machine.

## Clean room

Built from hardware documentation only, distilled into
[`docs/notes/`](docs/notes/) and [`docs/ref/`](docs/ref/), and from measured
behaviour: the ps1-tests hardware logs, and the real BIOS observed as a black
box. No other emulator's source is consulted. (There is an unrelated
open-source Rust PS1 emulator called *Rustation*. It is not consulted either;
the name similarity is a coincidence worth naming once.)

## Layout

```
crates/
  psx-core/       the emulator library: cpu, timing, gte, gpu, mdec, spu,
                  cdrom, disc, dma, sio (pads and cards), hle, save
    tests/        CPU semantics and timing integration tests
  psx-chd/        CHD disc images, as a psx-core disc
    tests/        a synthetic CHD read back against its BIN/CUE
  psx-libretro/   the libretro core (cdylib)
  psx-runner/     dev harnesses: testrom, shot, retrohost, discdiff, fingerprint, psx
scripts/          library survey, Android build and deploy
tools/            survey comparison, synthetic disc builder
docs/             hardware notes, reference write-ups, the test log
bios/  dumps/  tests/   your BIOS, discs and test suites (gitignored)
```

Nothing copyrighted is committed: no BIOS, no disc image, no third-party test
binary, no reference manual. `.gitignore` is written so none of it can land by
accident.

## Running

The libretro core always boots its built-in kernel and never reads a BIOS
file, so every copy of it runs the same machine, which netplay depends on. The
harnesses take a real BIOS's path as the reference, or `hle` for the built-in
kernel. See [`bios/README.md`](bios/README.md).

```sh
# Boot a disc headless and write what is on screen.
cargo run --release --bin shot -- hle --disc game.cue --steps 400000000 --out frame.png

# The same, printing what the game and the kernel wrote to the TTY.
cargo run --release --bin shot -- hle --disc game.cue --steps 400000000 --tty

# Run a conformance binary and read its own verdict, or a whole folder of them.
cargo run --release --bin testrom -- hle tests/test-suite/cpu/cop/cop.exe
cargo run --release --bin testrom -- hle --dir tests/test-suite/cpu

# Drive the built libretro core through its C ABI, the way a frontend does.
cargo build --release -p psx-libretro
cargo run --release --bin retrohost -- target/release/psxcore_libretro.dll system/ \
    --content game.cue --frames 3600 --mash --out frame.png

# The unit and integration tests.
cargo test --workspace --release
```

## Netplay check

`retrohost --hash-every N` prints a hash of the serialized state, and of the
video and audio since the last line, every N frames. Run the same content and
presses on two machines and every line must match:

```sh
retrohost <core> system/ --content game.cue --frames 7200 --mash --hash-every 60
```

`--save-at N PATH` and `--load-at N PATH` take and restore a state mid-run, the
way a peer resyncs to its host; the run that loaded it must then match the run
that saved it line for line.

## libretro core

`psx-libretro` builds one `cdylib`:

| Platform | Target triple | Output file |
|----------|---------------|-------------|
| Linux    | host          | `libpsxcore_libretro.so` |
| Windows  | host          | `psxcore_libretro.dll` |
| macOS    | host          | `libpsxcore_libretro.dylib` |
| Android arm64 | `aarch64-linux-android` | `libpsxcore_libretro.so` |

Builds are release + LTO. The emulator itself (`psx-core`) has no dependencies
beyond `std`. CHD reading lives in `psx-chd`, on the pure-Rust
[`chd`](https://crates.io/crates/chd) crate, so nothing needs a C toolchain and
cross-compiling only needs a linker for the target.

```sh
cargo build --release -p psx-libretro
```

For Android, point cargo at the NDK's clang for `aarch64-linux-android` in a
`.cargo/config.toml` and link with `-Wl,-z,max-page-size=16384` so the library
is 16 KB aligned, which the Play Store requires for API 35+. Then:

```sh
rustup target add aarch64-linux-android
cargo build --release -p psx-libretro --target aarch64-linux-android
# or: scripts/deploy-android-debug.sh so
```

Netplay frontends can compare `ruststation_state_token()`, an exported C string
of core id, save format version and state size, before letting two peers play.

## License

GNU General Public License v3.0 or later. See [LICENSE](LICENSE). No BIOS, disc
image or other copyrighted material is distributed with this repository.
