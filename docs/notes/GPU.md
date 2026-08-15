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

## Open questions

1. **Textures.** The single biggest gap. Command words are decoded and skipped
   correctly, so a textured primitive draws as a flat polygon rather than
   desynchronising the FIFO, and `Gpu::textured_primitives` counts them. Needs
   texture pages, 4/8/15-bit CLUT lookup, the texture window, and blending.
   Accounts for essentially all of the remaining error on `rectangles`,
   `texture-flip`, `uv-interpolation`, `texture-overflow` and `clut-cache`.
2. **The `transparency` background fill.** The blended swatches match the
   reference, but hardware ends up with the whole of VRAM filled light grey and
   this core only fills 320x240 of it. The fill command masks its width to
   10 bits and rounds up to a multiple of 16, which makes a 1024-wide fill
   compute as zero. Either the test does something else, or that masking is
   wrong. Worth settling before trusting `fill_rectangle`.
3. **The dither phase.** Dithering is in and clearly helps, but `triangle` still
   differs from the reference on about 5% of pixels, all by exactly one 5-bit
   step. That signature says the matrix orientation, sign, or the point at which
   it is applied is slightly off, rather than anything structural.
4. **Fill-rule at polygon edges.** Around 0.19% of `triangle` and 0.3% of `quad`
   differ by more than a rounding step, which is the edge pixels. The hardware
   has a specific rule about which edge a shared boundary belongs to; this core
   uses a plain `>= 0` test on all three edges.
5. **GPUSTAT's busy bits are always ready.** Commands execute the instant their
   last word arrives, so the core is never busy. That is a lie in the forgiving
   direction, but code that polls for *busy* before proceeding would spin.
6. **Not started**: 24-bit display output, interlace, the texture cache, the
   display range registers (stored but unused), and any notion of how long
   drawing takes.
