# docs/notes/

Distilled hardware notes. **This is the clean-room source of truth**: each
subsystem is implemented from a note here, and each note is written from the
reference material in `../ref/` in our own words.

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
| [`SIO.md`](SIO.md) | SIO0: the controller and memory card port |
| [`CDROM.md`](CDROM.md) | The CD-ROM controller |
| [`DISC.md`](DISC.md) | Disc images: cue sheets, tracks and raw sectors |
| [`SPU.md`](SPU.md) | The SPU's register file and sound RAM, without audio |

Still to write, in the order the work is likely to happen: MDEC, and memory
cards. Memory cards share `SIO.md`'s port and will be added to it.
