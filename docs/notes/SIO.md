# SIO0: the controller and memory card port

Written from the public PlayStation hardware documentation (psx-spx and nocash's
original PSX-SPX) for the SIO0 register block, the pad transfer protocol and the
interrupt path. No third-party emulator source consulted.

Implemented in `crates/psx-core/src/sio.rs`.

## What it is

One synchronous serial port, shared by four devices: two controllers and two
memory cards. It is not four ports. Every device hangs off the same clock and
the same data lines, and the port asserts one of two select lines to decide
which pair is listening. Which of that pair answers is decided by the **first
byte of the transfer**, not by any register: `0x01` addresses the controller and
`0x81` the memory card. A device that is not addressed stays quiet for the whole
transaction.

That is the single most important thing about this port, and it is why the code
has one transfer state machine with a `target` rather than four device objects
that each get poked independently.

Every byte is an **exchange**, not a send: the byte the port shifts out and the
byte it shifts in cross on the wire simultaneously. So the response to byte *n*
is already on its way while byte *n* is still going out, and the reply a device
gives is always one step behind the command that asked for it. Reading the
protocol tables below as request-then-response gets everything off by one.

## Registers

| Address | Width | Name |
|---|---|---|
| `0x1F801040` | 8 | `JOY_TX_DATA` write, `JOY_RX_DATA` read |
| `0x1F801044` | 32 | `JOY_STAT`, read only |
| `0x1F801048` | 16 | `JOY_MODE` |
| `0x1F80104A` | 16 | `JOY_CTRL` |
| `0x1F80104E` | 16 | `JOY_BAUD` |

`JOY_STAT`:

| Bit | Meaning |
|---|---|
| 0 | TX ready 1: the transmit register is free |
| 1 | RX FIFO has a byte |
| 2 | TX ready 2: the transfer has finished shifting |
| 3 | RX parity error |
| 7 | `/ACK` input level, **1 while the line is pulled low** |
| 9 | Interrupt request |
| 11..31 | Baud rate timer |

`JOY_CTRL`:

| Bit | Meaning |
|---|---|
| 0 | TX enable |
| 1 | `/JOYn` output: assert the select line |
| 2 | RX enable |
| 4 | Acknowledge: write 1 to clear `STAT` bits 3 and 9 |
| 6 | Reset: write 1 to clear the whole port |
| 8..9 | RX interrupt FIFO threshold |
| 10 | TX interrupt enable |
| 11 | RX interrupt enable |
| 12 | `/ACK` interrupt enable |
| 13 | Slot: which of the two select lines to assert |

Bits 4 and 6 are strobes. They read back as zero, and treating them as stored
state means a later read-modify-write of `JOY_CTRL` silently re-fires them.

## The controller transfer

Five bytes for a digital pad. `->` is what the port sends, `<-` what comes back.

| Step | -> | <- | Notes |
|---|---|---|---|
| 0 | `0x01` | `0xFF` | Address the controller |
| 1 | `0x42` | `0x41` | Read command; `0x41` identifies a digital pad |
| 2 | anything | `0x5A` | Fixed |
| 3 | anything | buttons, low | |
| 4 | anything | buttons, high | No `/ACK` after this one |

Button bits are **active low**: a `0` means pressed. The order on the wire is

```
low  byte: Select L3 R3 Start Up Right Down Left
high byte: L2 R2 L1 R1 Triangle Circle Cross Square
```

L3 and R3 exist in the layout but a digital pad never asserts them.

The absence of an `/ACK` pulse after the last byte is the only thing that tells
software the transfer is over. A device that acknowledges forever and one that
is not there at all are distinguished by exactly this, so getting the last-byte
case wrong makes the BIOS either hang or decide no pad is connected.

## An absent device

Nothing pulls the data line, so every byte reads back `0xFF`, and nothing pulls
`/ACK`, so no interrupt arrives and software times out. That is the whole of
it: there is no "not connected" status bit to set.

## Traps

* **`/ACK` is delayed, and software depends on the delay.** The acknowledge has
  to be a scheduled event, not something that happens inside the register write.
* **The BIOS does not use the controller interrupt.** It leaves bit 7 masked off
  in `I_MASK` and polls `I_STAT` bit 7 in a tight loop instead: write a byte,
  spin until the bit appears, clear it, read the reply, write the next byte. So
  the `/ACK` delay is what paces the entire transfer. This was measured from a
  register trace, and it contradicts the obvious assumption; anyone reasoning
  about pad timing from "the handler runs when the device acknowledges" will
  reason about the wrong loop.
* **A new byte restarts the pulse, it does not queue behind the old one.**
  Software is entitled to write the next byte while the previous `/ACK` is still
  low. Modelling the pulse as a single countdown with an implicit assert-then-
  release phase, and not resetting the phase when a byte starts, makes the next
  expiry read as the *release* of the old pulse rather than the assertion of the
  new one. The interrupt for that byte then never fires at all. The symptom is
  specific and misleading: the BIOS reads three bytes of the five-byte pad
  report, gives up, and reports every button held.
* **`STAT` bit 7 is inverted relative to the signal name.** `/ACK` is an
  active-low line, and the bit reads 1 while the line is *low*.
* **Deasserting the select line ends the transaction**, and the next transfer
  starts again from the address byte. A state machine that only advances on
  bytes, and never resets on select, works right up until software aborts a
  transfer part way and then reads a pad reply as a memory card reply.

## Open questions

1. **The `/ACK` timing constants are approximate.** A controller is commonly
   cited at roughly 338 cycles from the end of a byte to the acknowledge, and a
   memory card at roughly half that. Nothing here has been measured, and
   `input/pad` does not test it. What would settle it: a timing test that counts
   CPU cycles between the transfer write and the interrupt.

   Worth recording *how* this was nearly fitted wrongly. With the pulse-phase
   bug above still present, sweeping the delay produced complete pad reads at
   175, 200, 275 and 300 cycles and nothing at 150, 225, 250 or 338. Bands, not
   a threshold: the delay was deciding whether the previous pulse happened to
   have released before the next byte started. Picking one of the values that
   worked would have made the test pass and left the actual bug in place, which
   is the same mistake as the video clock constant in `TIMING.md`.
2. **Reading `JOY_RX_DATA` with an empty FIFO** returns `0xFF` here. Hardware
   returns something, and it may be the previous byte still in the latch rather
   than a fixed value. No test in the suite reads an empty FIFO, so this is a
   choice, not a finding.
3. **The baud rate timer in `STAT` bits 11..31 reads as zero.** Software polls
   `STAT` for the ready bits and does not appear to read the timer, but that is
   an observation about the BIOS, not a guarantee about games.
4. **Only the digital pad is implemented.** The analog pad, mouse and multitap
   all sit behind the same address byte and differ from step 1 onward.
