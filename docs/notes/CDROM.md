# CD-ROM controller

Written from the public PlayStation hardware documentation (psx-spx and nocash's
original PSX-SPX) for the CD-ROM register block, the command set, the response
protocol and the interrupt path. No third-party emulator source consulted.

Implemented in `crates/psx-core/src/cdrom.rs`.

**Status: the controller only, with an empty drive.** Commands, responses,
interrupts and the drive status byte are here. There is no disc image support and
no sector data, so every command that would need to read something answers with
the error the hardware gives for an empty tray. The milestone this was built to
is the BIOS getting past its boot logo to its own "no disc" screen, which is what
it does on a real console with nothing in the drive.

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

1. **The timing constants are approximate.** `cdrom/timing` measures them
   precisely and ships a hardware log, but every measurement in it needs a disc
   in the drive, so none of it can be checked yet. The values here are the
   commonly cited ones: roughly 50 000 cycles to the first acknowledgement, and
   longer for a completion. What would settle it: a disc image, then that test.
2. **No disc image support.** Sector reads, the data FIFO, sub-channel
   position, the table of contents and XA audio all wait on it. Three of the
   four tests in `cdrom/` need one, and the fourth needs the lid opened by hand
   part way through.
3. **The status byte for an empty tray with the lid closed** is reported here as
   motor off with the shell-open bit latched until first read. That is inferred
   from what makes the BIOS behave, not measured.
