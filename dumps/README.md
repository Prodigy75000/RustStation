# dumps/

Test binaries, disc images, save states and harness output. **Nothing in this
folder is committed** except this file. The ignore rule is a blanket one on the
whole tree, so no copyrighted image can be committed by accident regardless of
which subfolder it lands in.

## Suggested shape

```
dumps/cpu/       R3000A conformance binaries (PSX-EXE)
dumps/gte/       GTE conformance binaries (PSX-EXE)
dumps/gpu/       GPU / timing demos
dumps/discs/     disc images, one folder per game
dumps/states/    save states captured from a session
```

The layout is a suggestion, not a contract. `testrom --dir` walks whatever it is
given, recursively, and sorts the results so a run is reproducible.

## Content formats

- **Disc images**: a `.cue` sheet with its `.bin` tracks, which is the preferred
  form, since it keeps the raw 2352-byte sectors, the audio tracks and the
  sector headers software can see. A bare `.bin`, `.img` or `.iso` with no cue
  is also read, as a single data track whose sector size (2352 or 2048) is
  inferred from the file's length. A `.chd` made by `chdman createcd` is
  read too, with any of its CD codecs. See
  [`../docs/notes/DISC.md`](../docs/notes/DISC.md). For a game on more than one
  disc, an `.m3u` playlist lists one image per line, resolved against the
  playlist's own folder, for example:

  ```
  Game (Disc 1).cue
  Game (Disc 2).cue
  ```

- **PSX-EXE**: the 2048-byte `PS-X EXE` header followed by the text. That is
  what the CPU and GTE conformance suites ship as, and what `testrom` runs.

## What the harness expects

`testrom` takes a BIOS path and a PSX-EXE, or `--dir` and a folder of them.
With a real BIOS it boots the BIOS first and swaps the binary in at the shell
hand-over point (PC `0x80030000`), so the test runs with the kernel, the A/B/C
function tables and the TTY already up. Given `hle` in place of the BIOS path,
it uses the built-in HLE kernel instead (see
[`../docs/notes/HLE.md`](../docs/notes/HLE.md)). Either way the printed output
is captured through the kernel's TTY calls and is what the run is graded on.

A suite that prints **nothing** is graded as a failure, not a pass: silence is
exactly what a core that never reached the test looks like.

`shot` takes a disc with `--disc game.cue`, and `retrohost` loads any of the
formats above through the libretro core, as a frontend would.
