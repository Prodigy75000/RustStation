# MDEC

**Written from:** hardware documentation, plus the JPEG/MPEG baseline the chip
is a fixed-function implementation of. No third-party emulator source consulted.
Implemented in `crates/psx-core/src/mdec.rs`.

The Macroblock Decoder. It is a **JPEG-like still-image decompressor in
silicon**, not a video codec: it decodes one macroblock at a time and knows
nothing about frames, motion or time. Full-motion video on this console is a
stream of independently compressed frames read off the disc, pushed through here
a macroblock at a time, and blitted into VRAM. The "video player" is entirely
software.

That is why it is worth building: fourteen of the twenty-one discs on hand load
their whole intro and then stop with transfers queued on this chip's DMA
channels. It is not a nice-to-have for a handful of cutscenes; it is the single
largest thing standing between this core and the games it already loads.

## Shape

Two registers, and almost all the traffic goes past them by DMA.

| Address | Write | Read |
|---|---|---|
| `0x1F801820` | command, then its parameter words | decoded data out |
| `0x1F801824` | control and reset | status |

* **DMA channel 0** feeds compressed words in, **channel 1** takes decoded words
  out. A game sets up channel 0 with the compressed macroblocks, then channel 1
  with a destination, and the chip is the pipe between them.
* Both directions are FIFOs. The status register's request bits say which one
  the chip currently wants serviced, and the control register's enable bits say
  which ones the program has armed.

### Status, `0x1F801824` read

| Bits | Meaning |
|---|---|
| 31 | data-out FIFO empty |
| 30 | data-in FIFO full |
| 29 | command busy |
| 28 | data-in request (channel 0 armed *and* wanted) |
| 27 | data-out request (channel 1 armed *and* wanted) |
| 26-25 | output depth: 0 = 4-bit, 1 = 8-bit, 2 = 24-bit, 3 = 15-bit |
| 24 | output is signed |
| 23 | bit 15 of 15-bit output pixels |
| 18-16 | which block is being decoded: 0-3 = Y1..Y4, 4 = Cr, 5 = Cb |
| 15-0 | parameter words still expected, **minus one** |

The off-by-one in the low half is not decoration. A command that wants N words
reports `N - 1` immediately, so a program that reads the count to decide whether
to send more sees `0xFFFF` when the chip wants nothing, and that is the value it
tests against.

### Control, `0x1F801824` write

| Bit | Meaning |
|---|---|
| 31 | reset: empties both FIFOs, aborts the command, restores the power-on status |
| 30 | enable the data-in request |
| 29 | enable the data-out request |

## Commands

The command word's top three bits select. The rest of the word is that
command's own header.

**1: decode macroblocks.** `(1 << 29) | (depth << 27) | (signed << 26) |
(bit15 << 25) | words`, where `words` is how many 16-bit halfword pairs of
compressed data follow, expressed in 32-bit words. Everything after is data
until that many words have arrived.

**2: load the quant tables.** One parameter bit: set means colour, so 64 bytes
of luminance table followed by 64 bytes of chrominance (32 words in total);
clear means monochrome, luminance only (16 words).

**3: load the IDCT scale table.** 64 signed 16-bit values, 32 words. This is the
cosine matrix, supplied by software rather than baked into the chip.

Command 3 being *software-supplied* is the detail worth pausing on. The chip
does not know the DCT; it is handed the matrix. Games all load the standard
table, so a decoder that ignored it would look correct, and would then produce
garbage for anything that did not.

## The decode, per block

Each 8x8 block arrives run-length coded as 16-bit halfwords.

1. **The first halfword is special**: `(quant_factor << 10) | dc`, where `dc` is
   a **10-bit signed** value and `quant_factor` is 6 bits.
2. **Then run/level pairs**: `(run << 10) | level`, `run` being how many zeroes
   to skip and `level` another 10-bit signed value.
3. **`0xFE00` ends the block.** It is not a run/level pair, and reading it as
   one produces a plausible-looking block rather than an obvious failure, which
   is the sort of bug that survives a screenshot.

Coefficients land in **zigzag order**, so the run counts step through the
zigzag, not through rows.

Dequantisation differs between the DC coefficient and the rest, which is a
JPEG-ism the chip keeps: the DC is multiplied by the quant table's first entry
alone, and the AC coefficients by the table entry *and* the block's quant
factor, with a rounding term. Getting the AC path's shift wrong by one produces
an image with the right shapes at the wrong contrast, which reads as a colour
bug rather than an arithmetic one.

## Colour

A colour macroblock is **six blocks in a fixed order: Cr, Cb, Y1, Y2, Y3, Y4**,
producing one 16x16 pixel result. The two chroma blocks are 8x8 and cover the
whole macroblock, so chroma is at half resolution in each direction and each
chroma sample serves a 2x2 group of luma samples.

Cr and Cb arriving *before* the luma they belong to is worth stating plainly,
because the natural guess is the other way round and the failure is a picture
that is structurally perfect and coloured wrongly.

Monochrome output (the 4-bit and 8-bit depths) has no chroma blocks at all: a
macroblock is one 8x8 Y block.

The conversion out is the usual YCbCr, with the luma offset by 128 and the
chroma treated as signed. The `signed` output flag decides whether the result is
biased back into 0..255 or left as a signed value, and the `bit15` flag sets the
mask bit on every 15-bit pixel, which is how software gets a decoded frame that
the GPU will treat as opaque.

## Three things that each cost a debugging round

**The quantisation table is not zigzagged twice.** Coefficients arrive in zigzag
order, so the obvious move is to look the quant entry up through the zigzag as
well. The table is *already stored in zigzag order* by the software that loaded
it. Tomb Raider's luminance table reads `2, 16, 16, 19, 16, 19, 22, 22` on the
wire, which is the familiar `2, 16, 19, 22, 26, 27, 29, 34` already permuted:
recognising that is what settled it. Only the coefficient's **destination** is
de-zigzagged.

**The IDCT's shift follows from the table, and is worth deriving rather than
tuning.** The scale table holds `c(u) * cos((2x+1) u pi / 16)` scaled by 2^15,
with row zero at 23170, which is `2^15 / sqrt(2)`. A one-dimensional IDCT
carries a factor of one half, so one pass against that table gives the answer
times 2^16 and two passes times 2^32. An extra halving turns a flat -128 block
into -64, which is a picture with every shape correct and every brightness
wrong, and looks far more like a colour-space bug than an arithmetic one.

The table's entries are **truncated** toward zero, not rounded: the games load
32138, 27245, 18204, 6392 where rounding to nearest gives 27246, 18205, 6393.
That matters only for a test that rebuilds the table from its definition, which
is exactly what `mdec.rs` does, so it is written down here.

**A block handed over by DMA has to settle the command's word count at once.**
This core's DMA is instantaneous, so a decode's compressed data is handed over
as a cursor and chewed through later; the count of words the command is still
expecting has to drop when the block is *accepted*, not when it is consumed.
Getting that wrong leaves the chip permanently busy, so the next command word is
swallowed as data and no second frame ever decodes. It is a one-frame video that
looks exactly like a decoder which cannot handle frame two.

## The pipeline, and the one deliberate departure

Software feeds compressed data in blocks and pulls decoded pixels in blocks, and
expects the DMA controller to **interleave the two channels** as the chip raises
and drops its requests. Tomb Raider hands over 7 904 words in one go, the whole
compressed frame, and then asks for 1 920 words at a time, thirty-two times,
which is exactly 480 macroblocks.

This core's DMA is instantaneous and runs one channel to completion before
anything else moves, so that interleaving cannot happen as it does on hardware.
Two consequences, both deliberate:

* Channel 0 hands over a **cursor into RAM**, not a copy. Eight bytes of state
  instead of thirty kilobytes, and indistinguishable from the copy unless the
  program rewrites the buffer while blocked on the transfer, which it cannot
  usefully do.
* Channel 1 **stops rather than inventing pixels** when the decoder runs dry. It
  records what is left, stays busy, and is resumed the moment channel 0 delivers
  more. Running to completion regardless would write whatever the output buffer
  last held into the tail of a frame, which looks like a decoder bug and is not
  one.

## What this core does and does not do

Implemented: both quant tables, the software scale table, the RLE format, the
IDCT, colour and monochrome macroblocks, all four output depths, the signed and
bit15 flags, the status word, reset, and both DMA channels.

**Not** implemented: any notion of how long a decode takes. Real silicon takes
time per macroblock and software can watch the busy bit; here a command
completes the instant its last word arrives. That is the same forgiving lie the
GPU's ready bits tell, and it is recorded in both places for the same reason: it
is invisible until something polls for *busy* rather than for *done*.
