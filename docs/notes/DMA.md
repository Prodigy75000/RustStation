# DMA

**Written from:** hardware documentation of the DMA controller's register block
and channel behaviour. No third-party emulator source consulted. Implemented in
`crates/psx-core/src/dma.rs`.

Seven channels at `0x1F801080`. Each has three 32-bit registers: a memory
address (`MADR`), a block count (`BCR`) and a control word (`CHCR`). Two more
registers govern the lot: `DPCR` at `0x1F8010F0` enables channels and gives them
priorities, and `DICR` at `0x1F8010F4` carries the completion interrupts.

| Channel | Device | Direction |
|---|---|---|
| 0 | MDEC | compressed data in |
| 1 | MDEC | decoded pixels out |
| 2 | GPU | commands and pixels, both ways |
| 3 | CD-ROM | sector data in |
| 4 | SPU | samples, both ways |
| 5 | PIO | the expansion port |
| 6 | OTC | builds an ordering table in RAM, touches no device |

## Registers are 32 bits wide and software does not have to treat them that way

This is the trap that cost the most here, so it goes first.

`DICR`'s fields are laid out one per byte group: bit 15 forces an interrupt,
bits 16 to 22 enable one per channel, bit 23 is the master enable, bits 24 to 30
are the per-channel flags, and bit 31 is computed from the rest. A program that
wants to arm one channel's completion interrupt therefore does not read, modify
and write the whole word. It writes **one byte** at `DICR+2`, because that byte
is exactly the enables and the master enable.

A controller that ignores the access width answers a byte read with the low byte
of the word, which is a different field, and then stores the byte the program
writes back as though it were the whole register. The enables and the master
enable are wiped in the same instruction. Nothing afterwards raises a DMA
interrupt, which reads as a missing interrupt and is a missing byte lane.

So every access here resolves to a lane: `(offset & 3) * 8` is the shift, and
the width gives the mask. A read returns that slice; a write merges into what is
stored and leaves the rest.

**Write-1-to-acknowledge makes the merge less obvious than it looks.** `DICR`'s
flag bits are cleared by writing a one to them, so the merged word carries the
current flags in the bits the program did not write, and acting on those would
acknowledge interrupts nobody has seen. The acknowledgement is confined to the
lane actually written.

## What starts a transfer

A channel runs when `CHCR` bit 24 is set, the channel is enabled in `DPCR`, and,
in manual sync mode only, bit 28 is also set. Writing `DPCR` can therefore start
a channel that was armed earlier and waiting for its enable.

Sync modes, from `CHCR` bits 9 and 10:

* **0, manual.** `BCR`'s low half is the word count. Zero means 65536.
* **1, request.** `BCR` is a block size and a block count; the transfer is their
  product. Real hardware moves one block per device request; this core moves the
  lot.
* **2, linked list.** `MADR` points at a header word: the high byte is how many
  words follow, the low three bytes the next header. `0x00FFFFFF` ends it. Only
  the GPU uses this, and it is how almost every game reaches the GPU at all.
  **A list can come back on itself**, and this core stops at the first node it
  has already walked, having run every node once. See the open question.

Bit 0 chooses direction, bit 1 makes the address count down. Channel 6 forces
both: hardware ignores what is written to those bits for the ordering table.

## Completion

When a transfer ends, the channel's busy and trigger bits clear, and if that
channel's `DICR` enable is set the matching flag is set. The interrupt line is
raised only if the master enable is also set. Software polls `CHCR` bit 24 for
the synchronous case and takes the interrupt for the asynchronous one, and a
game may well do both for different channels in the same frame.

**A game can watch another channel's busy bit as a lock.** Tomb Raider's CD
sector handler reads `CHCR` for channel 1, the decoder's output, and declines to
accept a sector while it is set. A channel left busy forever is therefore not a
private failure: it stops unrelated devices.

## Transfers are instantaneous

The whole block moves in the cycle it is started. Real DMA steals bus cycles from
the CPU, and chopping mode exists to hand some back. `TIMING.md` carries that as
an open question. Two places where the difference shows:

* Channel 0 hands the decoder a **cursor into RAM** rather than a copy, because
  a copy would be thirty kilobytes of state for no observable difference.
* Channel 1 **stops rather than inventing pixels** when the decoder runs dry,
  records what is left, stays busy, and is resumed when channel 0 delivers more.
  See `MDEC.md`, which is where the interleaving this stands in for is described.

## Open questions

* **Cycle cost.** Nothing here charges the CPU for a transfer.
* **Priorities.** `DPCR`'s priority nibbles are stored and ignored. Only one
  channel ever runs at a time here, so there is nothing to arbitrate yet.
* **Chopping.** `CHCR` bits 8, 16-18 and 20-22 are stored and ignored.
* **A circular linked list stops; on hardware it would not.** psx-spx: the CPU
  runs "after SyncMode 2 list entries", so on a console a list that loops only
  keeps the GPU busy in the background until the game rebuilds it. Here a whole
  list runs inside the store that starts it, so a loop has to be cut. Metal
  Slug X builds one on the way into a stage (318 nodes, then three pointing in a
  circle); walked to the old million-node bound it was 1.6 million GP0 commands,
  150 seconds inside one frame, and on a phone a game that froze and
  could not be unloaded. Stopping at the first revisit draws every node once and
  finishes the channel. What this cannot give is the hardware's timing: a game
  that polled for the channel still being busy would see it done. The real fix
  is list DMA that runs between CPU slices, which would also be the start of
  charging transfers for their cycles.
* **Channel 5.** The expansion port has nothing behind it, so transfers on it are
  counted rather than performed.
