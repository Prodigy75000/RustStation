# SPU

Written from the public PlayStation hardware documentation (psx-spx and nocash's
original PSX-SPX) for the SPU register map and the sound RAM transfer path. No
third-party emulator source consulted.

Implemented in `crates/psx-core/src/spu.rs`.

**Status: the register file and sound RAM. No audio.** Nothing here produces a
sample. Voices, ADSR envelopes, ADPCM decoding, reverb and the capture buffers
do not exist.

## Why a stub this thin was worth building

Because a register that always reads zero is not a missing feature, it is a
**hang**. Software polls what it writes.

Crash Bandicoot writes `SPUCNT` and then reads it back in a tight loop until its
own value appears. Against a stubbed SPU it spun there forever, having loaded
473 sectors and drawn 600 primitives. With nothing more than read-back it
reaches its title screen: 1 647 sectors and 1.3 million primitives.

That generalises to every subsystem still stubbed here, and is the reason the
bus keeps a counter of reads to unemulated ports rather than silently returning
zero.

## Numbers

| | |
|---|---|
| Registers | `0x1F801C00`..`0x1F801E80`, 640 bytes, 16-bit |
| Sound RAM | 512 KB |
| `SPUCNT` | `0x1F801DAA` |
| `SPUSTAT` | `0x1F801DAE` |
| Transfer address | `0x1F801DA6`, in **8-byte units** |
| Transfer FIFO | `0x1F801DA8`, write only |
| DMA channel | 4, both directions |

## Traps

* **`SPUSTAT` is derived, not stored.** Its low six bits are `SPUCNT`'s low six
  bits. Letting a write land on it makes it stop following, and software that
  waits for the two to agree then waits forever.
* **The transfer address is in 8-byte units**, so the register value is one
  eighth of the byte address. The sort of factor that works perfectly until
  something writes somewhere other than address zero.
* **A byte write must leave the other half of the halfword alone.** These are
  16-bit registers and software does write single bytes to them.

## Open questions

1. **No audio at all.** The next real step is ADPCM decode plus the 24 voices,
   which is a subsystem in its own right rather than an extension of this.
2. **The transfer is instant.** `SPUSTAT`'s transfer-busy bits therefore never
   set. Nothing seen so far waits on them, but "nothing seen so far" covers four
   games.
3. **The IRQ address register is stored and ignored.** A game using an SPU
   interrupt to pace streaming audio would not get one.
