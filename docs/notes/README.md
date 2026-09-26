# docs/notes/

Distilled hardware notes. **This is the clean-room source of truth**: each
subsystem is implemented from a note here, and each note is written from
hardware documentation and hardware test results, in our own words. `../ref/`
holds the derived CPU reference; the other subsystems cite their sources
directly.

One note per subsystem. Head each one with what it was written from, by
document and section, so a reader can check the derivation.

Structure that works:

- **What the hardware does**, stated as behaviour. Not code.
- **Numbers**: register layouts, bit meanings, fixed values, ranges.
- **Traps**: the parts that look simple and are not.
- **Open questions**: what is unsettled, and what would settle it. This section
  is the important one. It is where "we guessed" gets written down instead of
  disappearing into a function body.

| Note | Subsystem |
|---|---|
| [`CPU.md`](CPU.md) | MIPS R3000A / LSI CW33300 |
| [`TIMING.md`](TIMING.md) | The master clock, scheduler, interrupts, video timing, root counters |
| [`GPU.md`](GPU.md) | VRAM, GP0/GP1, the rasterizer, textures, and DMA |
| [`GTE.md`](GTE.md) | COP2: the fixed-point geometry coprocessor |
| [`DMA.md`](DMA.md) | The seven DMA channels and their interrupt register |
| [`MDEC.md`](MDEC.md) | The macroblock decoder |
| [`SIO.md`](SIO.md) | SIO0: the controller and memory card port, and the memory cards on it |
| [`CDROM.md`](CDROM.md) | The CD-ROM controller |
| [`DISC.md`](DISC.md) | Disc images: cue sheets, tracks and raw sectors |
| [`SPU.md`](SPU.md) | The SPU: voices, ADPCM, envelopes, the mixer and its interrupt |
| [`HLE.md`](HLE.md) | The HLE kernel: booting without a BIOS file |

The main sources are psx-spx (and nocash's original PSX-SPX), the IDT R30xx
manuals for the CPU, and the hardware logs of the
[ps1-tests](https://github.com/JaCzekanski/ps1-tests) suite; each note says
which it used. Where the conformance results stand is in
[`../TESTS.md`](../TESTS.md), and the save-state layout in
[`../SAVESTATE.md`](../SAVESTATE.md).
