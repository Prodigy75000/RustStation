# Memory map

The R3000A sees a 32-bit virtual address space in the standard MIPS segments.
The PlayStation fits no TLB, so translation is a mask per segment and nothing
more. `crates/psx-core/src/bus.rs` is the whole of it.

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
| `0x1F801000` | 36 B | Memory control | stored, never acted on |
| `0x1F801040` | 32 B | Joypad + serial | stubbed |
| `0x1F801060` | 4 B | RAM size register | stored |
| `0x1F801070` | 8 B | I_STAT / I_MASK | implemented (no source raises yet) |
| `0x1F801080` | 128 B | DMA | stubbed |
| `0x1F801100` | 48 B | Timers | stubbed |
| `0x1F801800` | 4 B | CD-ROM | stubbed |
| `0x1F801810` | 8 B | GPU | GPUSTAT lies (see below), rest stubbed |
| `0x1F801820` | 8 B | MDEC | stubbed |
| `0x1F801C00` | 640 B | SPU | stubbed |
| `0x1F802000` | 66 B | Expansion Region 2 | stubbed |
| `0x1FC00000` | 512 KB | BIOS ROM | implemented, writes dropped |
| `0xFFFE0130` | 4 B | Cache control | stored |

Retail RAM is 2 MB and mirrors four times across the 8 MB window. Development
units had 8 MB, and some software probes the mirroring to tell them apart, so
the mirroring is modelled rather than being an accident of masking.

The scratchpad is the data cache wired as addressable memory. On hardware it is
not reachable through KSEG1, because KSEG1 is by definition the uncached view.
The bus does not enforce that yet; it is listed in `notes/CPU.md` as an open
item, because getting it wrong makes a real bug look like a working one.

## Stubbed vs unmapped

The bus counts two different failures and keeps them apart on purpose:

- **stub** hit a range we decode but do not emulate. Expected, and the counter
  is how the next subsystem gets prioritised.
- **unmapped** hit no range at all. Usually *our decode* is wrong, not a missing
  subsystem.

Both counters are printed by the `psx` harness and are deliberately kept out of
save states, so watching them cannot change a state's bytes.

## The one lie

GPUSTAT (`0x1F801814`) reads back `0x1C000000`: bits 26, 27 and 28, meaning
"ready for a command", "ready to send VRAM" and "ready to receive DMA". The BIOS
spins on those at boot, so a truthful zero is an instant hang. Everything else
about that register is still a lie, and it is the first thing the GPU work
replaces.
