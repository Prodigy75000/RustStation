# GPU

**Written from:** general knowledge plus the hardware suite's own reference
images, which turned out to be the more useful source. Implemented in
`crates/psx-core/src/gpu.rs`, with DMA in `dma.rs`.

The **timing** half of this chip lives in `video.rs` and landed first; see
[`TIMING.md`](TIMING.md). This note is about drawing.

## Shape

VRAM is 1024x512 16-bit pixels, 1 MB, and is both the drawing surface and the
display source. There is no separate framebuffer: what you see is a window onto
VRAM at `display_start`, sized by GP1(0x08).

Two ports:

* **GP0** (`0x1F801810` write) takes drawing commands and transfer data.
* **GP1** (`0x1F801814` write) takes display and control commands.
* Reading those addresses gives GPUREAD and GPUSTAT respectively.

GP0 is a word stream, not a register. Each command's length is implied by its
opcode, so `command_length` has to be right for every one: a wrong length does
not draw a wrong picture, it desynchronises the FIFO and everything after it is
garbage. That function has its own test for exactly this reason.

## Why DMA landed at the same time

Almost nothing pokes GP0 word by word. The console's graphics library builds an
ordering table in RAM, a linked list of drawing commands, and hands the head of
it to DMA channel 2. Channel 6 exists purely to *build* that table (it writes a
run of words each pointing at the one before). Without both, a test binary draws
nothing at all, so the GPU would have been unreachable.

Channels 2 and 6 are implemented; the rest are counted. Transfers are
instantaneous, which is wrong: real DMA steals bus cycles from the CPU, and
chopping mode exists to hand some back.

## What draws

Flat and Gouraud triangles and quads, rectangles including the fixed 1x1, 8x8
and 16x16 forms, lines and poly-lines, the four semi-transparency blend modes,
dithering, the mask bit, the drawing area and offset, and all four VRAM
transfers (fill, CPU to VRAM, VRAM to CPU, VRAM to VRAM).

## Traps met so far

* **Vertex coordinates are 11-bit signed.** `2000` is not 2000, it is -48. The
  "primitive too large" rule is about the *span* between vertices exceeding
  1023, which needs coordinates at opposite ends of the range, not one big
  number. A test asserting the discard with a single large coordinate silently
  tests nothing.
* **The command word carries the first vertex's colour**, so vertex data starts
  at word 1. Starting at 0 parses the command byte as a coordinate and draws
  nothing, which looks exactly like the rasterizer being broken.
* **Dithering is not cosmetic.** Without it a Gouraud gradient bands into
  visible 5-bit steps. It applies to shaded and textured primitives only; a flat
  colour has nothing to dither.
* **VRAM-to-VRAM copies may overlap**, so the source has to be read in full
  before the destination is written. The suite has a test for precisely this.
* **Two 5-bit-to-8-bit conversions exist** and disagree at the top: `c << 3`
  gives 248 for full scale, `(c << 3) | (c >> 2)` gives 255. The suite's
  reference dumps use the former. Mixing them up turns a pixel-exact match into
  a whole-image mismatch and reads like a rasterizer bug. `shot` uses the
  suite's convention for `--vram` and the display convention for the libretro
  framebuffer, and says so.

## Textures

Implemented: texture pages, 4-bit and 8-bit CLUT lookup, 15-bit direct colour,
the texture window, the textured-rectangle flip bits, transparent texels, the
per-texel semi-transparency bit, and raw versus modulated colour. UV
interpolates affinely across a polygon, and steps one-for-one across a
rectangle.

Three things that each cost a debugging round:

* **A texel of all zeroes means "draw nothing", not "draw black".** An opaque
  black texel has bit 15 set. Treating zero as black puts a solid box around
  every sprite.
* **Modulation is centred on 0x80, not 0xFF.** A mid-grey vertex colour leaves
  the texture untouched, and brighter values lighten it.
* **The draw mode is fourteen bits wide, not eleven.** Bits 12 and 13 are the
  textured-rectangle flips. Masking `GP0(0xE1)` to `0x7FF` discards them, which
  is invisible until something flips: `texture-flip` drew four identical copies
  of a texture the hardware mirrors into four quadrants, and that one mask was
  worth 51 percentage points on that test.

## Open questions

1. **The BIOS main menu draws colour noise across its two entries**, on four of
   the five BIOS images here. Not a texturing bug: the chain from the texture to
   the screen is faithful and the source picture is already wrong when it is
   captured. Traced end to end in `docs/TESTS.md`, along with the three suspects
   eliminated by counter (the readback path, transfers cut short, the GTE). What
   is left is the vertex colours the BIOS hands to a Gouraud mesh it renders
   offscreen and then grabs as a texture, which are fully saturated when every
   neighbouring primitive's are not. Reproduce with `shot <bios.bin> --steps
   500000000` and no `--exe`.
2. **The residual texture error is in the fetch, not the blend.** `texture-flip`
   still differs on 24.8% of pixels by exactly one 5-bit step. Three separate
   experiments (transposing the dither matrix, not dithering textured polygons,
   rounding the modulation) all produced *byte-identical* output, which is
   itself the finding: those tests use raw textures, so none of those paths run.
   Suspect the texel coordinate, since a one-step error across a gradient is
   what an off-by-one in U or V looks like. Anchoring a flipped rectangle's UV
   at the far edge instead of decrementing was tried and is wrong (25.6% to
   37.9%).
3. **The `transparency` background fill.** The blended swatches match the
   reference, but hardware ends up with the whole of VRAM filled light grey and
   this core only fills 320x240 of it. The fill command masks its width to
   10 bits and rounds up to a multiple of 16, which makes a 1024-wide fill
   compute as zero. Either the test does something else, or that masking is
   wrong. Worth settling before trusting `fill_rectangle`.
4. **The dither phase.** Dithering is in and clearly helps, but `triangle` still
   differs from the reference on about 5% of pixels, all by exactly one 5-bit
   step. That signature says the matrix orientation, sign, or the point at which
   it is applied is slightly off, rather than anything structural.
5. **Fill-rule at polygon edges: closed 2026-09-25.** psx-spx: polygons are
   drawn "up to excluding their lower-right coordinates". A pixel on an edge
   now belongs to the triangle only if the edge is a top or a left one, so two
   polygons sharing an edge draw it once. `quad` went from 0.324% to
   pixel-exact, `triangle`'s differences beyond a rounding step from 0.189% to
   none, and `texture-flip` and `uv-interpolation` improved a little; no test
   got worse. Found through Metal Gear Solid, whose title screens are two quads
   meeting at x = 160: both drew that column, the left one last, sampling one
   texel past its image, and a white line ran down the middle.
6a. **Display width from GP1(06h): closed 2026-09-25.** psx-spx gives the pixels
   shown as `((X2 - X1) / clocks_per_pixel + 2) AND NOT 3`. The usual ranges
   give the nominal widths back; Metal Gear Solid's radio screen draws 320
   pixels in the 368 mode with a range to match, and showing 368 put the
   textures beside it in VRAM down the right of the screen.
6. **GPUSTAT's busy bits are always ready.** Commands execute the instant their
   last word arrives, so the core is never busy. That is a lie in the forgiving
   direction, but code that polls for *busy* before proceeding would spin.
7. **Not started**: interlaced *rendering*, the texture cache, and any notion
   of how long drawing takes.
8. **Display output, done 2026-09-24, from the owner playing on a phone.** Two
   bugs, both in what is handed to the frontend and neither in emulation:
   * **24-bit display** (GP1(08h) bit 4) was read as 15-bit, so every MDEC
     video in 24-bit colour came out as rainbow stripes: GTA 2 and Twisted Metal
     reported as "looks very strange" until the menu, which is 15-bit. Three
     bytes a pixel, red first, packed across halfwords with no padding.
   * **The height ignored GP1(07h).** It was a fixed 240 (480 interlaced), so
     a game showing fewer lines showed whatever sat below its picture in VRAM:
     the BIOS licence screen asks for lines 16 to 255, 239 of them, which left
     two stray rows at 480i; CTR's menu asks for 28 to 244, a 24-row strip. The
     height is now Y2 - Y1, doubled at 480i. The *horizontal* range is still
     ignored and the width is the mode's nominal one, which is why this core
     fills the screen where Beetle shows a border; the owner prefers it.

## Tracing, when a picture is wrong

Three environment variables, all off and free unless set:

* `RSTA_GPU_TRACE=1` logs every VRAM transfer with its rectangle, a checksum of
  what VRAM holds there, how many of those pixels are non-zero, and twelve
  pixels from the middle of it. The checksum is the useful part: copying a
  picture through the CPU is a read transfer and a write transfer, so the same
  number twice says the round trip is intact and the source was already wrong,
  and two different numbers say the round trip is where it broke.
* `RSTA_GPU_REGION=x0,y0,x1,y1` logs every triangle overlapping that rectangle
  of VRAM, with its vertex colours, texture coordinates and texture state. A
  whole-run primitive log is thousands of lines of which two matter; the
  question is nearly always "what drew *this* corner", and a rectangle is how
  that is asked. Overlap, not containment, or the large primitives are exactly
  the ones that go missing.
* `shot --film 0` writes the whole of VRAM out once per transfer, which is the
  granularity a texture is actually built at.

Two counters are always on because their answers cannot be recovered later.
`Gpu::abandoned_transfers` counts transfers a GP1 reset threw away part-drained,
which leaves the tail of a destination buffer holding stale memory and imitates
a texturing bug convincingly. `Gte::colour_saturations` counts colour channels
the GTE clamped, which is what separates "these vertex colours are all exactly 0
or 255 because something overflowed" from "they never came from the GTE".

## GPUSTAT bit 31 is not a status bit

Bit 31 reports the parity of the line currently being drawn, and reads 0
throughout vertical blank whichever parity the beam happens to be sitting on.
With interlace on it reports the field instead, a field being one parity of
lines by definition.

It is worth its own heading because it is not really a *status* bit at all: it
is the beam position, sampled. That has two consequences the rest of GPUSTAT
does not have.

* **The read has to be synchronised.** Every other bit is register state that
  only the CPU changes, so reading it against a stale clock gives the same
  answer. This one changes on its own. Reading it without first advancing the
  timed devices returns whatever parity was current when the scheduler last
  stopped, which for a poll loop is a constant, and the loop never ends.
* **It is a boot requirement, not a refinement.** The PAL BIOS latches GPUSTAT
  and then spins waiting for bit 31 to differ from the latched copy, before it
  will bring the display up at all. SCPH-1002 with this bit stuck at 0 clears
  sound RAM and stops there with the screen off: no primitives, no CD-ROM, no
  unmapped accesses, nothing that looks like a graphics fault. With it, the
  same image boots to its main menu.

For that reason [`Gpu::status`] takes the beam position as a parameter rather
than reading a cached copy. There is exactly one place the raster lives, in
`video`, and a caller cannot forget to refresh what it has to pass in.

The field itself, while interlace is off, is taken as the frame count's parity.
That is a derivation and not a measurement: it guarantees only that consecutive
frames report different fields, which is enough for software waiting for a
change and not enough for software that cares which field it got.
