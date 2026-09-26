# docs/

| File | What it is |
|---|---|
| [`TESTS.md`](TESTS.md) | The conformance baseline against the hardware suite, and the project log of what moved it |
| [`MEMORY_MAP.md`](MEMORY_MAP.md) | The address space, the devices on it, and the interrupt sources |
| [`SAVESTATE.md`](SAVESTATE.md) | The save-state layout, the rules it follows, and the tests that hold it |
| [`notes/`](notes/) | Per-subsystem implementation notes, decisions and open questions |
| [`ref/`](ref/) | Derived hardware reference for the CPU: what the machine does, with citations |

The notes, one per subsystem (index in [`notes/README.md`](notes/README.md)):

| Note | Subsystem |
|---|---|
| [`notes/CPU.md`](notes/CPU.md) | MIPS R3000A / LSI CW33300 |
| [`notes/TIMING.md`](notes/TIMING.md) | The master clock, scheduler, interrupts, video timing, root counters, instruction costs |
| [`notes/GPU.md`](notes/GPU.md) | VRAM, GP0/GP1, the rasterizer, textures, and display output |
| [`notes/GTE.md`](notes/GTE.md) | COP2: the fixed-point geometry coprocessor |
| [`notes/DMA.md`](notes/DMA.md) | The seven DMA channels and their interrupt register |
| [`notes/MDEC.md`](notes/MDEC.md) | The macroblock decoder |
| [`notes/SIO.md`](notes/SIO.md) | SIO0: controllers and memory cards |
| [`notes/CDROM.md`](notes/CDROM.md) | The CD-ROM controller and CD audio |
| [`notes/DISC.md`](notes/DISC.md) | Disc images: cue sheets, tracks and raw sectors |
| [`notes/SPU.md`](notes/SPU.md) | The SPU: voices, ADPCM, envelopes, the mixer and its interrupt |
| [`notes/HLE.md`](notes/HLE.md) | The HLE kernel: booting without a BIOS file |

`ref/` describes the **hardware**; `notes/` describes **this codebase's** take on
it, including where the two currently disagree. Everything under `docs/` that
is markdown is tracked, `ref/` included. Raw third-party material (PDFs, HTML,
archives, manuals) may sit beside it in a working copy but stays out of git.

## The clean-room rule

Subsystems are written from hardware documentation and hardware test results,
never from another emulator's source. The working method is:

1. Read the reference material: `docs/ref/` for the CPU, and psx-spx and the
   test suite's hardware logs for the rest, as each note records.
2. Distil what the hardware does into a note in `docs/notes/`, in your own
   words, with the behaviour stated as behaviour rather than as code.
3. Implement from the note.

Step 2 is not paperwork. It is what makes the implementation defensible, and it
is where the open questions get written down instead of being resolved by
guessing in the middle of a function.

Measured reference *data* (a hardware colour table, a timing measurement) may be
used as data, and is cited where it is used.
