# SPU

Written from psx-spx, "Sound Processing Unit (SPU)": the overview, ADPCM
samples and pitch, the volume and ADSR generator, voice flags, the noise
generator, the control and status registers, memory access, and the interrupt.
The register map and the transfer path came from the same document and nocash's
original PSX-SPX. No third-party emulator source consulted.

Implemented in `crates/psx-core/src/spu.rs`.

**Status: 24 voices, and games make sound.** ADPCM decoding, the pitch counter
and its interpolation, pitch modulation, noise, ADSR, volume sweeps, key-on and
key-off, ENDX, the capture buffers, and the interrupt from all three of its
sources. **Not here: reverb, CD-DA, XA-ADPCM.** Crash Bandicoot and Crash Team
Racing play their title music; games whose music streams off the disc are
silent where it would be.

## What the hardware does

One sample every 768 CPU cycles, which is 44 100 Hz exactly. Per sample:

1. Pending key-offs, then key-ons. Writes are taken at the chip's own rate, so
   both are latched and applied at the start of the next sample.
2. Every voice, keyed on or not: interpolate a sample from the decoded block
   (or take the noise level), multiply by the envelope, which is OUTX, then by
   the two voice volumes and into the mix. Then advance the pitch counter,
   fetching and decoding the next 16-byte block when it passes 28 samples,
   then step the envelope and the two volume sweeps.
3. Step the noise generator.
4. Write the four capture buffers.
5. Gate the voice mix on SPUCNT enable and unmute, add CD audio if enabled,
   apply the main volumes, clamp, output.

All voices always run because on hardware they do: psx-spx is explicit that a
voice reads sound RAM "even in Noise mode, even if the Voice Volume is zero,
and even if the ADSR pattern has finished", so an inaudible voice still raises
the interrupt.

## Numbers

| | |
|---|---|
| Registers | `0x1F801C00`..`0x1F801E80`, 640 bytes, 16-bit |
| Voice *n* | `0x1F801C00 + 0x10n`: VOLL, VOLR, PITCH, START, ADSR1, ADSR2, ENVX, REPEAT |
| Main volume | `0x1D80`/`0x1D82`, current values read at `0x1DB8`/`0x1DBA` |
| KON, KOFF, PMON, NON, EON, ENDX | `0x1D88`, `0x1D8C`, `0x1D90`, `0x1D94`, `0x1D98`, `0x1D9C`, each two halfwords |
| IRQ address | `0x1DA4`, 8-byte units |
| Transfer address, FIFO | `0x1DA6` (8-byte units), `0x1DA8` |
| SPUCNT, RAM_CTRL, SPUSTAT | `0x1DAA`, `0x1DAC`, `0x1DAE` |
| CD volume | `0x1DB0`/`0x1DB2` |
| Current voice volumes | `0x1E00 + 4n` |
| Sound RAM | 512 KB; capture buffers at 0x000 (CD L), 0x400 (CD R), 0x800 (voice 1), 0xC00 (voice 3) |
| ADPCM block | 16 bytes: shift/filter, flags, 14 bytes of nibbles, 28 samples |
| Filters | pos 0, 60, 115, 98, 122; neg 0, 0, -52, -55, -60; sum + 32, then >> 6 |
| Shift | 0..12; 13..15 act as 9 |
| Block flags | bit 0 loop end (set ENDX, jump to REPEAT), bit 1 repeat (without it: release, level 0), bit 2 loop start (REPEAT = this block) |
| Pitch | 0x1000 = 44 100 Hz; clamped to 0x4000 after modulation |
| Interpolation | 512-entry table, index = counter bits 4..11 |
| Envelope | shared by ADSR and sweeps: step `(7 - s) << max(0, 11 - shift)`, counter `0x8000 >> max(0, shift - 11)`, moves when counter bit 15 sets |

## Traps

* **SPUSTAT is derived, not stored.** Its low six bits are SPUCNT's; bit 7
  follows SPUCNT bit 5; bit 6 is the interrupt flag; bit 11 is which half of
  the capture buffers is being written, and only when RAM_CTRL bit 2 or 3 is
  set. Letting a write land on it makes it stop following.
* **Byte writes are not byte writes.** The bus has no byte lanes: a byte store
  to an odd address is dropped, and one to an even address writes the whole
  low halfword of the CPU register. `sb` of `0x12345678` stores `0x5678`.
* **The interpolator looks back three samples, across the block boundary.**
  The last three samples of the previous block have to be kept, or every
  block starts with a click.
* **Exponential decrease scales the step by the level, so it never reaches
  zero by rounding down on its own.** An arithmetic shift of a negative step
  rounds towards minus infinity, which is what gets release to 0.
* **ADSR2 of zero is not "hold".** Sustain shift 0, step 0, increase is the
  fastest rise there is, so a voice decays to its sustain level and then climbs
  straight back to full. The unit test for decay nearly asserted the opposite.
* **The transfer address is in 8-byte units**, as is the IRQ address, the
  start address and the repeat address.
* **Silence must be exactly zero.** A DC level while quiet is inaudible until
  the stream stops. `shot --wav` prints the mean for this reason.

## Why the thin stub came first

A register that always reads zero is not a missing feature, it is a **hang**.
Crash Bandicoot writes SPUCNT and polls it until its own value comes back.
Against a stubbed SPU it spun forever; read-back alone took it to its title
screen. That lesson still holds for every register above that is derived.

## Open questions

Each of these is a choice made where psx-spx is silent. Written down so a
measurement can overturn it.

1. **Key-on resets the ADPCM predictor and the interpolation history to zero.**
   The document says key-on copies START to the current address and zeroes the
   envelope. It says nothing about `old`/`older` or the three samples of
   history.
2. **Filters 5..7 are treated as 4.** The header's filter field is three bits
   and only five filters are documented.
3. **Output before advance.** Each voice outputs from its current counter, then
   advances it. The other order shifts every voice by one sample.
4. **Key-off before key-on in the same sample**, so a retrigger written as
   KOFF then KON within 768 cycles restarts the voice rather than releasing it.
5. **CD audio is mixed before the main volume**, and is not gated by the
   enable or mute bits ("don't care for CD audio"). Where exactly it joins is
   not documented.
6. **Release with shift 1Fh is frozen.** psx-spx says an all-ones rate never
   steps, "0x1f for decay/release". Decay's field is four bits, so that is read
   as applying to release only.
7. **The voice IRQ fires when a block containing the IRQ address is fetched.**
   psx-spx notes that a mid-block address "doesn't seem to trigger always",
   which this does not model.
8. **Transfers are instant.** SPUSTAT's busy bit never sets. Nothing measured
   so far waits on it.
9. **Direct volume writes apply at the next sample.** The document warns the
   hardware delays them by one sample; here they are also taken at the next
   sample, but a sweep written straight after a direct value in the same
   sample starts from the value the direct write left, which may be early.

## Verification

Unit tests in `spu.rs`. Three were proven to fail by breaking the code on
purpose: the Gaussian taps swapped, ENDX not cleared on key-on, and two pairs
of SPU fields swapped in the save state. The rest cover the ADPCM filters
against hand-computed values, the table's sums and
literal entries, a four-tap interpolation against a value worked outside the
code, the envelope's rates, the loop flags, pitch modulation, noise, all three
interrupt sources, silence being zero, and sync granularity not changing the
output.

What is **not** verified is how any of it sounds against a real console. No
hardware capture exists here. `shot --wav` produces the files a listener
compares. The next step is for `retrohost` to record audio too, so another
libretro core driven as a black box can produce the same recording: reference
data, not reference code.
