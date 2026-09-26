# Memory map

The R3000A sees a 32-bit virtual address space in the standard MIPS segments.
The PlayStation fits no TLB, so translation is a mask per segment and nothing
more. `crates/psx-core/src/bus.rs` is the whole of it: the segment masks, the
address decode, and the dispatch to each device.

## Segments

| Segment | Virtual range | Mask | Notes |
|---|---|---|---|
| KUSEG | `0x00000000..0x80000000` | none | User space, cached |
| KSEG0 | `0x80000000..0xA0000000` | strip bit 31 | Kernel, cached |
| KSEG1 | `0xA0000000..0xC0000000` | strip bits 31..29 | Kernel, **uncached** |
| KSEG2 | `0xC0000000..0xFFFFFFFF` | none | Only the cache-control port lives here |

KUSEG, KSEG0 and KSEG1 are three views of the same physical space. `0x00000010`,
`0x80000010` and `0xA0000010` are the same byte, which is why the reset vector
`0xBFC00000` is just the uncached view of physical `0x1FC00000`.

## Physical map

| Physical | Size | What | State today |
|---|---|---|---|
| `0x00000000` | 8 MB window | Main RAM | **2 MB, mirrored four times** across the window |
| `0x1F000000` | 8 MB | Expansion Region 1 | absent: reads all-ones, writes dropped |
| `0x1F800000` | 1 KB | Scratchpad (D-cache as fast RAM) | implemented |
| `0x1F801000` | 36 B | Memory control | stored and read back; the delays it sets are not applied |
| `0x1F801040` | 16 B | SIO0: controllers and memory cards | implemented (`sio.rs`, `memcard.rs`) |
| `0x1F801050` | 16 B | SIO1: the serial link port | absent: reads zero, writes dropped |
| `0x1F801060` | 4 B | RAM size register | stored |
| `0x1F801070` | 8 B | I_STAT / I_MASK | implemented (`irq.rs`) |
| `0x1F801080` | 128 B | DMA | implemented (`dma.rs`): every channel with a device behind it |
| `0x1F801100` | 48 B | Timers | implemented (`timers.rs`): the three root counters |
| `0x1F801800` | 4 B | CD-ROM | implemented (`cdrom.rs`), including CD-DA and XA audio |
| `0x1F801810` | 8 B | GPU | implemented (`gpu.rs`): GP0, GP1, GPUREAD, GPUSTAT |
| `0x1F801820` | 8 B | MDEC | implemented (`mdec.rs`) |
| `0x1F801C00` | 640 B | SPU | implemented (`spu.rs`), except reverb |
| `0x1F802000` | 66 B | Expansion Region 2 | absent: reads zero, writes dropped |
| `0x1FA00000` | | Expansion Region 3 | not decoded: counted as unmapped |
| `0x1FC00000` | 512 KB | BIOS ROM | implemented, writes dropped. A real BIOS, or the built-in HLE image |
| `0xFFFE0130` | 4 B | Cache control | implemented: turns the I-cache on, selects tag writes |

Retail RAM is 2 MB and mirrors four times across the 8 MB window. Development
units had 8 MB, and some software probes the mirroring to tell them apart, so
the mirroring is modelled rather than being an accident of masking.

The scratchpad is the data cache wired as addressable memory. On hardware it is
not reachable through KSEG1, because KSEG1 is by definition the uncached view.
The bus does not enforce that yet; it is listed in `notes/CPU.md` as an open
item, because getting it wrong makes a real bug look like a working one.

Memory control holds the per-device access delays. They are stored and read
back, but instruction and load timing (`timing.rs`) uses fixed figures, taken
with the delays every BIOS sets, rather than deriving them from these
registers. See `notes/TIMING.md`.

Cache control is acted on: bit 11 turns the I-cache on for KUSEG and KSEG0
fetches, and bit 2, with Status `IsC` set, makes a store write a cache tag,
which is how the BIOS flushes the cache. See `notes/CPU.md`.

## Interrupt sources

`I_STAT` bits, and what raises them:

| Bit | Source | Raised by |
|---|---|---|
| 0 | VBlank | video timing (`video.rs`), at the start of vertical blank |
| 1 | GPU | not raised: `GP0(1Fh)` sets GPUSTAT bit 24, which is not forwarded |
| 2 | CD-ROM | the drive's responses (`cdrom.rs`) |
| 3 | DMA | `DICR`, when an enabled channel completes (`dma.rs`) |
| 4, 5, 6 | Timers 0, 1, 2 | the root counters (`timers.rs`) |
| 7 | Controller and memory card | SIO0's /ACK, when enabled (`sio.rs`) |
| 8 | SIO1 | not raised: the port is absent |
| 9 | SPU | the SPU's IRQ address being reached (`spu.rs`) |
| 10 | Lightpen | not raised |

Devices are advanced by the scheduler in `bus.rs`: each reports how many cycles
remain until it next needs attention, and a read of a register whose value
moves on its own (the interrupt controller, the timers, GPUSTAT, SIO0, the
CD-ROM, the SPU) first brings the devices up to the present. See
`notes/TIMING.md`.

## Absent vs unmapped

The bus counts two different failures and keeps them apart on purpose:

- **stub** hit a range we decode but deliberately do not emulate: Expansion
  Regions 1 and 2 and the SIO1 port, which have nothing fitted behind them on a
  retail console.
- **unmapped** hit no range at all. Usually *our decode* is wrong, not a missing
  device. The bus also records which addresses they were, because a large count
  is usually a machine spinning on one location.

Both counters are printed by the `psx` harness and are deliberately kept out of
save states, so watching them cannot change a state's bytes.

## GPUSTAT

GPUSTAT (`0x1F801814`) is built from the GPU's real state: the draw mode, the
mask bits, the display mode, the display-disable and interrupt flags, the DMA
direction and request line, and bit 31, the parity of the line being drawn (or
the field, when interlaced). A read first brings video timing up to the present,
because software polls that bit: the PAL BIOS waits for it to change before it
brings the display up.

The ready bits are the one simplification left. Every command executes the
instant its last word arrives, so the GPU is never busy: bits 26 and 28 always
read ready, and bit 27 whenever a VRAM read is waiting. That errs in the
forgiving direction; see `notes/GPU.md`.
