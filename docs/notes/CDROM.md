# CD-ROM controller

Written from the public PlayStation hardware documentation (psx-spx and nocash's
original PSX-SPX) for the CD-ROM register block, the command set, the response
protocol and the interrupt path. No third-party emulator source consulted.

Implemented in `crates/psx-core/src/cdrom.rs`.

**Status: the controller and sector reads.** Commands, responses, interrupts,
the drive status byte, seeking, and a running read that delivers a sector at a
time into the data FIFO and out through DMA channel 3. Discs come from
[`DISC.md`](DISC.md)'s layer, which the controller knows nothing about beyond
"table of contents plus a function from LBA to 2352 bytes".

Two milestones, both reproducible:

* With **no disc**, the BIOS gets past its boot logo to its own main menu, which
  is what a real console does with an empty tray.
* With a **synthetic disc** (`tools/fakedisc.py`), the BIOS runs its whole
  recognition sequence and draws the PlayStation licence screen, with the text
  on it read out of sector 4 of the disc.

Not yet done: XA audio, CD-DA playback, sub-channel Q, and the region check
against a real game.

## What it is

A separate microcontroller with its own firmware, not a memory-mapped register
block. The CPU hands it a command plus parameters and gets **interrupts** back
carrying response bytes. Almost nothing is synchronous, and almost nothing is
instant.

The consequence that shapes the code: a command produces one, two, or a stream
of responses, and each one arrives as its own interrupt. `Init` acknowledges
first and completes later. `GetID` acknowledges, then reports what is in the
drive. `ReadN` acknowledges, then delivers a sector at a time until told to stop.
So the model here is a **queue of scheduled responses**, not a function that
returns a value.

## Registers

Four addresses, `0x1F801800` to `0x1F801803`, and three of them mean different
things depending on a 2-bit **index** written to the first. The same address is
five different registers. Missing that turns the whole block into noise.

| Address | Index 0 | Index 1 | Index 2 | Index 3 |
|---|---|---|---|---|
| `1800` w | index select | index select | index select | index select |
| `1801` w | command | sound map data | sound map coding | right CD to right SPU |
| `1801` r | response FIFO | response FIFO | response FIFO | response FIFO |
| `1802` w | parameter FIFO | interrupt enable | left CD to left SPU | right CD to left SPU |
| `1802` r | data FIFO | data FIFO | data FIFO | data FIFO |
| `1803` w | request | interrupt flags | left CD to right SPU | apply volume |
| `1803` r | interrupt enable | interrupt flags | interrupt enable | interrupt flags |

`0x1F801800` read is the status register, and it is not the drive status:

| Bit | Meaning |
|---|---|
| 0..1 | Current index |
| 2 | ADPCM FIFO empty |
| 3 | Parameter FIFO **empty** (1 = empty) |
| 4 | Parameter FIFO **not full** (1 = has room) |
| 5 | Response FIFO **not empty** |
| 6 | Data FIFO not empty |
| 7 | Command busy |

Bits 3 and 4 read as "everything is fine" in opposite directions, which is the
sort of thing that works by accident when a FIFO is never full.

## The drive status byte

A different byte, returned by most commands as their first response:

| Bit | Meaning |
|---|---|
| 0 | Error |
| 1 | Motor on |
| 2 | Seek error |
| 3 | ID error |
| 4 | Shell open |
| 5 | Reading |
| 6 | Seeking |
| 7 | Playing |

**Bit 4 latches.** It is set while the lid is open *and* stays set after it
closes, until the next command reads it. That is how software tells "the lid is
open now" from "the disc may have been swapped since you last looked", and it is
the whole mechanism behind disc swapping.

## Interrupts

The response carries a 3-bit code in the low bits of the interrupt flags:

| Code | Meaning |
|---|---|
| 1 | A data sector is ready |
| 2 | The command finished |
| 3 | The command was acknowledged |
| 4 | The data stream ended |
| 5 | Error |

Software acknowledges by writing the bits back to `0x1F801803` at index 1.

**The controller holds the next response until the current interrupt is
acknowledged.** A second response does not overwrite the first and does not
arrive early. Delivering responses on a timer without that gate makes `Init`
look like it completed before it was acknowledged, and software that waits for
the acknowledgement waits forever.

## An empty drive

`GetID` is the command that settles it, and it answers in two parts: an
acknowledgement carrying the status byte, then an error carrying `08h, 40h` and
six zeroes, which means "no disc". A closed lid with nothing in it is not an
error condition at the status-byte level; the drive reports the absence only
when asked what the disc is.

With a disc, the second response is a completion instead, carrying `02h, 00h,
20h, 00h` and the four region bytes.

## Reading

`ReadN` does not return sectors. It starts the drive, and sectors then arrive on
their own at 75 per second, or 150 at double speed, each as its own `INT1`,
until something stops it. So a read is **not** a queued response: the response
queue is two deep and a read is unbounded, and modelling it as a queue entry
either truncates it at two sectors or grows the queue without limit.

A delivered sector sits in the drive and is **not readable yet**. Software sets
bit 7 of the request register to hand it to the data FIFO, and only then can it
be read a byte at a time or pulled out by DMA channel 3. Loading the FIFO when
the sector arrives instead looks right until software reads a sector it never
asked for.

`Setmode` bit 5 decides what the FIFO contains: the 2048-byte user data, or all
2340 bytes from the sector header onwards. The 12-byte sync pattern is never
part of it.

**Setting bit 7 again partway through a sector does nothing.** Only an empty
FIFO reloads. This matters more than it sounds: software is entitled to re-arm
the bit while it is still working through a sector, and rewinding to the start
there hands it the beginning a second time. Grand Theft Auto 2 reads the twelve
bytes of header and subheader from a whole-sector read, sets the bit again, and
expects the 2048 bytes of user data to follow. With a rewind it got the header
instead, so every file it read was twelve bytes out of step, and it rejected the
ISO volume descriptor and re-read it 256 times rather than opening anything.

Nothing about that failure pointed here. The sector bytes we served were
byte-identical to the disc image, every command was recognised, no port was
stubbed and no read was unmapped. What found it was tracing the read *position*
at the moment of each reload rather than the data: 255 of 606 reloads happened
at position 12, and a reload at a non-zero position is the bug stated outright.

## Traps

* **The index register changes what an address means**, including for reads. A
  read of `0x1F801803` is the interrupt enable at index 0 and the interrupt
  flags at index 1.
* **Writing the interrupt flags acknowledges, it does not assign.** Only the
  bits written as 1 are cleared.
* **The parameter FIFO is per command.** It has to be emptied when the command
  is taken, not when the next one is written, or a command with no parameters
  inherits the last one's.

## Open questions

1. **The command timing constants are approximate.** Roughly 50 000 cycles to
   the first acknowledgement and longer for a completion, both commonly cited
   and neither measured. `cdrom/timing` would settle them, and now that discs
   load it can be run: it needs any disc in the tray, not a particular one, and
   its per-command figures are drive properties rather than disc properties.

   The **sector rate** is the exception and is not a guess: the drive turns at
   exactly 75 sectors per second, so it is the CPU clock over 75, which is
   451 584 cycles. The hardware log measures 446 040 against that, and 222 222
   against the double-speed 225 792, both within about 1%.
2. **No real game has been tried.** The disc path is proven as far as a
   synthetic disc can prove it, which stops at "the BIOS likes this disc". The
   synthetic image has no filesystem, no `SYSTEM.CNF` and no executable, so
   everything past recognition is untested.
3. **XA audio, CD-DA playback and the Q sub-channel do not exist.** `GetlocP`
   derives its position arithmetically from the track table instead of reading
   a sub-channel, which is right for a data track and approximate for audio.
4. **Disc swapping is not wired up.** The shell-open bit latches correctly, but
   nothing yet opens the lid: there is no host-facing way to eject. That is what
   `cdrom/disc-swap` tests, and it also needs a person to open the tray part way
   through, so it may never be gradeable unattended.
5. **The status byte for an empty tray with the lid closed** is reported here as
   motor off with the shell-open bit latched until first read. That is inferred
   from what makes the BIOS behave, not measured.
