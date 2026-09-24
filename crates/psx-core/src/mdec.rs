// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The Macroblock Decoder.
//!
//! A JPEG-like still-image decompressor in silicon. It decodes one macroblock at
//! a time and knows nothing about frames, motion or time: full-motion video on
//! this console is a stream of independently compressed frames read off the
//! disc, pushed through here, and blitted into VRAM by software.
//!
//! Compressed words arrive on DMA channel 0 and decoded pixels leave on channel
//! 1. See `docs/notes/MDEC.md`.

/// `RSTA_MDEC_TRACE=1` logs the tables software loads and the first blocks
/// decoded. The tables are the interesting half: the chip does not know the
/// DCT, it is handed the matrix, so the matrix is the only thing that says what
/// scale the IDCT's output is in.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RSTA_MDEC_TRACE").is_ok_and(|v| v != "0"))
}

/// Halfwords in a block, and the side of one.
const BLOCK: usize = 64;
const BLOCK_SIDE: usize = 8;

/// The end-of-block marker in the run-length stream. Not a run/level pair, and
/// decoding it as one yields a plausible block rather than an obvious failure.
const END_OF_BLOCK: u16 = 0xFE00;

/// Output depths, from the decode command's bits 28..27.
const DEPTH_4: u32 = 0;
const DEPTH_8: u32 = 1;
const DEPTH_24: u32 = 2;
const DEPTH_15: u32 = 3;

/// Which block of a colour macroblock is being filled. The order on the wire is
/// chroma first, which is the opposite of the natural guess and produces a
/// structurally perfect, wrongly coloured picture when assumed backwards.
const BLOCK_CR: usize = 0;
const BLOCK_CB: usize = 1;
const BLOCK_Y1: usize = 2;

/// The zigzag order coefficients arrive in. Index by position in the stream,
/// get the offset into the 8x8 block.
#[rustfmt::skip]
const ZIGZAG: [usize; BLOCK] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// The output FIFO's capacity in words.
///
/// A 16x16 macroblock at 24 bits per pixel is 768 bytes, which is 192 words,
/// and that is the largest single result the chip produces. Sized to hold one
/// so a decode never has to be suspended halfway.
const OUT_MAX: usize = 192;

/// What the chip is currently expecting on the command port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Between commands.
    Idle,
    /// Taking compressed macroblock data.
    Decode,
    /// Taking a quantisation table.
    Quant,
    /// Taking the IDCT scale table.
    Scale,
}

impl Phase {
    fn code(self) -> u8 {
        match self {
            Phase::Idle => 0,
            Phase::Decode => 1,
            Phase::Quant => 2,
            Phase::Scale => 3,
        }
    }
    fn from_code(code: u8) -> Phase {
        match code {
            1 => Phase::Decode,
            2 => Phase::Quant,
            3 => Phase::Scale,
            _ => Phase::Idle,
        }
    }
}

pub struct Mdec {
    phase: Phase,
    /// Parameter words still expected for the running command.
    remaining: u16,

    /// Output depth, and the two output flags, from the decode command.
    depth: u32,
    signed: bool,
    bit15: bool,

    /// Set by the control register, and reported back in the status word.
    dma_in_enabled: bool,
    dma_out_enabled: bool,

    /// Quantisation tables: luminance, then chrominance.
    quant: [[u8; BLOCK]; 2],
    /// True while a colour quant-table load is still expecting the chrominance
    /// half.
    quant_colour: bool,
    /// How many bytes of the quant tables, and halfwords of the scale table,
    /// have arrived for the load in progress.
    quant_pos: usize,
    scale_pos: usize,
    /// The IDCT cosine matrix, which software supplies rather than the chip
    /// knowing it.
    scale: [i16; BLOCK],

    /// Coefficients for the block being assembled, in natural (de-zigzagged)
    /// order.
    coeffs: [i16; BLOCK],
    /// How far through the current block's run-length stream we are, as a
    /// position in zigzag order.
    coeff_pos: usize,
    /// The current block's quantisation factor, from its first halfword.
    quant_factor: u8,
    /// True once the block's first halfword has been consumed.
    block_started: bool,

    /// Which block of the macroblock is being filled.
    block_index: usize,
    /// Decoded 8x8 blocks, in the order they arrive: Cr, Cb, Y1..Y4.
    blocks: [[i8; BLOCK]; 6],

    /// Where DMA channel 0's block sits in RAM, and how much of it is left.
    ///
    /// The compressed data is **read from RAM as it is needed** rather than
    /// copied into a FIFO here. See `queue_input`.
    in_addr: u32,
    in_remaining: u32,
    /// Whether the cursor holds compressed macroblock data rather than a table.
    ///
    /// The two are accounted differently: a table is consumed the moment it
    /// arrives, so the command's outstanding-word count can be decremented as
    /// each word is read. Macroblock data is consumed lazily, long after the
    /// transfer that delivered it, so the count has to be settled up front or
    /// the chip stays busy forever and eats the *next* command as data.
    decoding: bool,

    /// Decoded words waiting to be read.
    out: [u32; OUT_MAX],
    out_len: usize,
    out_pos: usize,

    /// Macroblocks decoded. Host-side observation, never serialized: the figure
    /// that says whether a video is actually being decoded or merely requested.
    pub macroblocks: u64,
}

impl Default for Mdec {
    fn default() -> Mdec {
        Mdec::new()
    }
}

impl Mdec {
    pub fn new() -> Mdec {
        Mdec {
            phase: Phase::Idle,
            remaining: 0,
            depth: DEPTH_4,
            signed: false,
            bit15: false,
            dma_in_enabled: false,
            dma_out_enabled: false,
            quant: [[0; BLOCK]; 2],
            quant_colour: false,
            quant_pos: 0,
            scale_pos: 0,
            scale: [0; BLOCK],
            coeffs: [0; BLOCK],
            coeff_pos: 0,
            quant_factor: 0,
            block_started: false,
            block_index: 0,
            blocks: [[0; BLOCK]; 6],
            in_addr: 0,
            in_remaining: 0,
            decoding: false,
            out: [0; OUT_MAX],
            out_len: 0,
            out_pos: 0,
            macroblocks: 0,
        }
    }

    // ---- registers -------------------------------------------------------

    /// `offset` is relative to `0x1F801820`.
    pub fn read(&mut self, offset: u32) -> u32 {
        if offset & 4 == 0 {
            self.read_data()
        } else {
            self.status()
        }
    }

    /// `offset` is relative to `0x1F801820`.
    pub fn write(&mut self, offset: u32, value: u32) {
        if offset & 4 == 0 {
            self.write_command(value);
        } else {
            self.write_control(value);
        }
    }

    pub fn status(&self) -> u32 {
        let mut s = 0u32;
        // The low half is the outstanding parameter count *minus one*, so a
        // chip that wants nothing reports 0xFFFF. Software tests against that
        // value, so an honest zero here reads as "one more word please".
        s |= self.remaining.wrapping_sub(1) as u32 & 0xFFFF;
        s |= ((self.current_block() as u32) & 7) << 16;
        s |= (self.bit15 as u32) << 23;
        s |= (self.signed as u32) << 24;
        s |= (self.depth & 3) << 25;

        s |= (self.wants_data_out() as u32) << 27;
        s |= (self.wants_data_in() as u32) << 28;
        s |= ((self.phase != Phase::Idle) as u32) << 29;
        // Bit 30, the input FIFO being full, stays clear: a word handed to this
        // core is consumed at once, and claiming otherwise would stall software
        // waiting for room.
        s |= ((self.out_pos >= self.out_len) as u32) << 31;
        s
    }

    fn wants_data_in(&self) -> bool {
        self.dma_in_enabled && self.phase != Phase::Idle && self.in_remaining == 0
    }

    fn wants_data_out(&self) -> bool {
        self.dma_out_enabled && self.out_pos < self.out_len
    }

    /// Which block the status register reports. Colour macroblocks count
    /// Y1..Y4 as 0..3 and the chroma pair as 4 and 5, which is not the order
    /// they arrive in, so this is a translation and not the index itself.
    fn current_block(&self) -> usize {
        match self.block_index {
            BLOCK_CR => 4,
            BLOCK_CB => 5,
            n => n - BLOCK_Y1,
        }
    }

    fn write_control(&mut self, value: u32) {
        if value & (1 << 31) != 0 {
            let quant = self.quant;
            let scale = self.scale;
            let macroblocks = self.macroblocks;
            *self = Mdec::new();
            // A reset empties the FIFOs and abandons the command. It does not
            // wipe the tables: software loads them once at the start of a video
            // and resets between frames, so clearing them here costs every
            // frame after the first its colour.
            self.quant = quant;
            self.scale = scale;
            self.macroblocks = macroblocks;
            return;
        }
        self.dma_out_enabled = value & (1 << 29) != 0;
        self.dma_in_enabled = value & (1 << 30) != 0;
    }

    fn write_command(&mut self, word: u32) {
        if trace_enabled() {
            eprintln!(
                "mdec: cmd {word:08X} phase {:?} remaining {} in {} out {}/{}",
                self.phase, self.remaining, self.in_remaining, self.out_pos, self.out_len
            );
        }
        if self.phase != Phase::Idle {
            self.feed_parameter(word);
            return;
        }
        match word >> 29 {
            1 => {
                self.depth = (word >> 27) & 3;
                self.signed = word & (1 << 26) != 0;
                self.bit15 = word & (1 << 25) != 0;
                self.remaining = (word & 0xFFFF) as u16;
                self.phase = Phase::Decode;
                if trace_enabled() {
                    eprintln!(
                        "mdec: decode depth {} signed {} bit15 {} words {}",
                        self.depth, self.signed, self.bit15, self.remaining
                    );
                }
                self.start_macroblock();
                self.out_len = 0;
                self.out_pos = 0;
            }
            2 => {
                self.quant_colour = word & 1 != 0;
                self.remaining = if self.quant_colour { 32 } else { 16 };
                self.phase = Phase::Quant;
                self.quant_pos = 0;
            }
            3 => {
                self.remaining = 32;
                self.phase = Phase::Scale;
                self.scale_pos = 0;
            }
            // Everything else is accepted and ignored rather than latched: an
            // unrecognised command that left the chip busy would hang the DMA
            // that follows it.
            _ => {}
        }
        if self.remaining == 0 {
            self.phase = Phase::Idle;
        }
    }

    fn feed_parameter(&mut self, word: u32) {
        match self.phase {
            Phase::Decode => {
                self.feed_coefficients(word as u16);
                self.feed_coefficients((word >> 16) as u16);
            }
            Phase::Quant => {
                let table = usize::from(self.quant_pos >= BLOCK);
                let base = self.quant_pos % BLOCK;
                for i in 0..4 {
                    if base + i < BLOCK {
                        self.quant[table][base + i] = (word >> (8 * i)) as u8;
                    }
                }
                self.quant_pos += 4;
            }
            Phase::Scale => {
                if self.scale_pos + 1 < BLOCK {
                    self.scale[self.scale_pos] = word as i16;
                    self.scale[self.scale_pos + 1] = (word >> 16) as i16;
                }
                self.scale_pos += 2;
            }
            Phase::Idle => {}
        }
        self.remaining = self.remaining.saturating_sub(1);
        if self.remaining == 0 {
            if trace_enabled() {
                match self.phase {
                    Phase::Quant => eprintln!(
                        "mdec: quant luma {:?} chroma {:?}",
                        &self.quant[0][..8],
                        &self.quant[1][..8]
                    ),
                    Phase::Scale => eprintln!("mdec: scale {:?}", &self.scale[..16]),
                    _ => {}
                }
            }
            self.phase = Phase::Idle;
        }
    }

    // ---- the decode ------------------------------------------------------

    fn start_macroblock(&mut self) {
        self.block_index = if self.monochrome() {
            BLOCK_Y1
        } else {
            BLOCK_CR
        };
        self.start_block();
    }

    fn start_block(&mut self) {
        self.coeffs = [0; BLOCK];
        self.coeff_pos = 0;
        self.block_started = false;
    }

    fn monochrome(&self) -> bool {
        self.depth == DEPTH_4 || self.depth == DEPTH_8
    }

    /// Take one halfword of the run-length stream.
    fn feed_coefficients(&mut self, half: u16) {
        if !self.block_started {
            // Padding between macroblocks is written as end-of-block markers,
            // so one arriving before a block has started is skipped rather
            // than treated as a quantisation factor of 0x3F.
            if half == END_OF_BLOCK {
                return;
            }
            self.quant_factor = (half >> 10) as u8 & 0x3F;
            let dc = sign_extend_10(half);
            self.coeffs[0] = self.dequant_dc(dc);
            self.coeff_pos = 1;
            self.block_started = true;
            return;
        }

        if half == END_OF_BLOCK {
            self.finish_block();
            return;
        }

        let run = (half >> 10) as usize & 0x3F;
        let level = sign_extend_10(half);
        self.coeff_pos += run;
        if self.coeff_pos >= BLOCK {
            // A run past the end of the block is corrupt data, not a wrap.
            // Ending the block keeps the stream in step rather than scribbling
            // into the next one.
            self.finish_block();
            return;
        }
        self.coeffs[ZIGZAG[self.coeff_pos]] = self.dequant_ac(level);
        self.coeff_pos += 1;
    }

    /// The DC coefficient takes the quantisation table's first entry alone. It
    /// does **not** take the block's quantisation factor, which is the one
    /// asymmetry in the dequantiser and the reason a wrong implementation shows
    /// correct detail on a wrong background.
    fn dequant_dc(&self, value: i16) -> i16 {
        let table = usize::from(self.chroma_block());
        saturate16(value as i32 * self.quant[table][0] as i32)
    }

    /// `coeff_pos` indexes the quantisation table **directly**, without going
    /// through the zigzag.
    ///
    /// This is the trap in the whole format. Coefficients arrive in zigzag
    /// order, so the obvious move is to zigzag the table index too, and the
    /// table is *already stored in zigzag order* by the software that loaded
    /// it. Applying the zigzag twice is what it looks like: Tomb Raider's
    /// luminance table reads `2, 16, 16, 19, 16, 19, 22, 22` on the wire, which
    /// is the standard table's `2, 16, 19, 22, 26, 27, 29, 34` already
    /// permuted. Only the coefficient's *destination* is de-zigzagged.
    fn dequant_ac(&self, value: i16) -> i16 {
        let table = usize::from(self.chroma_block());
        let q = self.quant[table][self.coeff_pos] as i32;
        // The rounding term and the shift are part of the format, not a
        // refinement: dropping them leaves the shapes intact and the contrast
        // wrong, which reads as a colour bug rather than an arithmetic one.
        saturate16((value as i32 * q * self.quant_factor as i32 + 4) >> 3)
    }

    /// Whether the block being decoded uses the chrominance quant table.
    fn chroma_block(&self) -> bool {
        !self.monochrome() && self.block_index <= BLOCK_CB
    }

    fn finish_block(&mut self) {
        let mut out = [0i8; BLOCK];
        idct(&self.coeffs, &self.scale, &mut out);
        if trace_enabled() && self.macroblocks < 2 {
            eprintln!(
                "mdec: block {} quant {} coeffs {:?} -> {:?}",
                self.block_index,
                self.quant_factor,
                &self.coeffs[..8],
                &out[..8]
            );
        }
        self.blocks[self.block_index] = out;
        self.block_index += 1;

        let done = if self.monochrome() {
            self.block_index > BLOCK_Y1
        } else {
            self.block_index >= 6
        };
        if done {
            self.emit_macroblock();
            self.macroblocks += 1;
            self.start_macroblock();
        } else {
            self.start_block();
        }
    }

    // ---- output ----------------------------------------------------------

    fn emit_macroblock(&mut self) {
        match self.depth {
            DEPTH_4 => self.emit_indexed(4),
            DEPTH_8 => self.emit_indexed(8),
            DEPTH_24 => self.emit_colour_24(),
            DEPTH_15 => self.emit_colour_15(),
            // Unreachable: the depth is masked to two bits on the way in. The
            // arm exists so that a future third colour depth is a compile
            // error here rather than a silently wrong picture.
            other => unreachable!("output depth {other}"),
        }
    }

    /// The two monochrome depths emit the luma block directly, packed two or
    /// eight pixels to a word.
    fn emit_indexed(&mut self, bits: u32) {
        let mut words: Vec<u32> = Vec::new();
        let per_word = 32 / bits;
        let mut word = 0u32;
        let mut n = 0;
        for i in 0..BLOCK {
            let v = self.blocks[BLOCK_Y1][i] as i32;
            let v = if self.signed { v } else { v + 128 };
            let mask = (1u32 << bits) - 1;
            let v = (v as u32) & mask;
            word |= v << (bits * n);
            n += 1;
            if n == per_word {
                words.push(word);
                word = 0;
                n = 0;
            }
        }
        if n != 0 {
            words.push(word);
        }
        self.push_out(&words);
    }

    fn emit_colour_15(&mut self) {
        let mut words: Vec<u32> = Vec::new();
        let mut pixels: Vec<u16> = Vec::with_capacity(256);
        for y in 0..16 {
            for x in 0..16 {
                let (r, g, b) = self.colour_at(x, y);
                let p = ((b as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (r as u16 >> 3);
                pixels.push(p | ((self.bit15 as u16) << 15));
            }
        }
        for pair in pixels.chunks(2) {
            words.push(pair[0] as u32 | ((pair[1] as u32) << 16));
        }
        self.push_out(&words);
    }

    fn emit_colour_24(&mut self) {
        // Three bytes per pixel packed continuously, so a pixel can straddle a
        // word boundary and the run has to be built as bytes first.
        let mut bytes: Vec<u8> = Vec::with_capacity(768);
        for y in 0..16 {
            for x in 0..16 {
                let (r, g, b) = self.colour_at(x, y);
                bytes.push(r);
                bytes.push(g);
                bytes.push(b);
            }
        }
        let words: Vec<u32> = bytes
            .chunks(4)
            .map(|c| {
                let mut w = 0u32;
                for (i, b) in c.iter().enumerate() {
                    w |= (*b as u32) << (8 * i);
                }
                w
            })
            .collect();
        self.push_out(&words);
    }

    /// One pixel of the 16x16 macroblock.
    ///
    /// The luma comes from one of the four 8x8 Y blocks, and the chroma from a
    /// single 8x8 block covering the whole macroblock, so each chroma sample
    /// serves a 2x2 group of luma samples.
    fn colour_at(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let luma_block = BLOCK_Y1 + (y / 8) * 2 + (x / 8);
        let luma = self.blocks[luma_block][(y % 8) * BLOCK_SIDE + (x % 8)] as i32;
        let c = (y / 2) * BLOCK_SIDE + (x / 2);
        let cr = self.blocks[BLOCK_CR][c] as i32;
        let cb = self.blocks[BLOCK_CB][c] as i32;

        // Fixed-point YCbCr, the coefficients scaled by 1024.
        let r = luma + ((1436 * cr) >> 10);
        let g = luma - ((352 * cb) >> 10) - ((731 * cr) >> 10);
        let b = luma + ((1815 * cb) >> 10);
        let bias = if self.signed { 0 } else { 128 };
        (
            (r + bias).clamp(0, 255) as u8,
            (g + bias).clamp(0, 255) as u8,
            (b + bias).clamp(0, 255) as u8,
        )
    }

    fn push_out(&mut self, words: &[u32]) {
        self.out_len = 0;
        self.out_pos = 0;
        for (i, w) in words.iter().enumerate().take(OUT_MAX) {
            self.out[i] = *w;
            self.out_len = i + 1;
        }
    }

    fn read_data(&mut self) -> u32 {
        if self.out_pos >= self.out_len {
            return 0;
        }
        let v = self.out[self.out_pos];
        self.out_pos += 1;
        v
    }

    /// One word out, for DMA channel 1.
    pub fn read_word(&mut self) -> u32 {
        self.read_data()
    }

    /// Note where DMA channel 0's block of compressed data is, without copying
    /// it.
    ///
    /// **This is a deliberate departure from the hardware's shape, and it is
    /// here because of a different departure elsewhere.** On a real console the
    /// input FIFO is a few words deep and the DMA controller runs channel 0 in
    /// bursts, yielding between them, so channel 1 drains decoded pixels while
    /// channel 0 is still feeding compressed ones. This core's DMA is
    /// instantaneous: a channel runs to completion before anything else moves.
    /// Pushing the whole block through a small FIFO under that rule decodes
    /// every macroblock of a frame and keeps only the last.
    ///
    /// Tomb Raider makes the shape plain: one channel-0 transfer of 7 904
    /// words, the entire compressed frame, and then channel 1 asking for 1 920
    /// words at a time until the frame is out. So the decoder has to hold that
    /// frame and decode on demand.
    ///
    /// Holding a cursor into RAM rather than a copy costs eight bytes of state
    /// instead of thirty kilobytes, and is indistinguishable from the copy
    /// unless the program rewrites the buffer mid-transfer, which it cannot
    /// usefully do while blocked on the transfer.
    pub(crate) fn queue_input(&mut self, addr: u32, words: u32) {
        self.in_addr = addr;
        self.in_remaining = words;
        self.decoding = self.phase == Phase::Decode;
        if self.decoding {
            // The chip has *accepted* the whole block. Whether the decoder has
            // chewed through it yet is a different question, and confusing the
            // two leaves the command permanently outstanding.
            self.remaining = self
                .remaining
                .saturating_sub(words.min(u16::MAX as u32) as u16);
            if self.remaining == 0 {
                self.phase = Phase::Idle;
            }
        }
    }

    /// Consume queued input until there is something to read out, or it runs
    /// dry.
    ///
    /// Table loads produce no output, so this drains them completely. A decode
    /// stops the moment a macroblock is ready and parks the rest, which is what
    /// makes the pull model work.
    pub(crate) fn pump(&mut self, ram: &[u8]) {
        while self.in_remaining > 0 && self.out_pos >= self.out_len {
            let a = (self.in_addr & 0x1F_FFFC) as usize;
            if a + 4 > ram.len() {
                self.in_remaining = 0;
                return;
            }
            let word = u32::from_le_bytes([ram[a], ram[a + 1], ram[a + 2], ram[a + 3]]);
            self.in_addr = self.in_addr.wrapping_add(4);
            self.in_remaining -= 1;
            if self.decoding {
                self.feed_coefficients(word as u16);
                self.feed_coefficients((word >> 16) as u16);
            } else {
                self.write_command(word);
            }
        }
    }

    pub(crate) fn has_output(&self) -> bool {
        self.out_pos < self.out_len
    }

    // ---- save state ------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn parts(&self) -> ([u8; 9], [u8; 2 * BLOCK], [i16; BLOCK], [i16; BLOCK]) {
        (
            [
                self.phase.code(),
                self.depth as u8,
                self.signed as u8,
                self.bit15 as u8,
                self.dma_in_enabled as u8,
                self.dma_out_enabled as u8,
                self.quant_colour as u8,
                self.quant_factor,
                self.decoding as u8,
            ],
            {
                let mut q = [0u8; 2 * BLOCK];
                q[..BLOCK].copy_from_slice(&self.quant[0]);
                q[BLOCK..].copy_from_slice(&self.quant[1]);
                q
            },
            self.scale,
            self.coeffs,
        )
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn progress(&self) -> ([u16; 3], [u32; 6], [i8; 6 * BLOCK], [u32; OUT_MAX]) {
        (
            [
                self.remaining,
                self.coeff_pos as u16,
                self.block_index as u16,
            ],
            [
                self.quant_pos as u32,
                self.scale_pos as u32,
                self.out_len as u32,
                self.out_pos as u32,
                self.in_addr,
                self.in_remaining,
            ],
            {
                let mut b = [0i8; 6 * BLOCK];
                for (i, block) in self.blocks.iter().enumerate() {
                    b[i * BLOCK..(i + 1) * BLOCK].copy_from_slice(block);
                }
                b
            },
            self.out,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        flags: [u8; 9],
        quant: [u8; 2 * BLOCK],
        scale: [i16; BLOCK],
        coeffs: [i16; BLOCK],
        counters: [u16; 3],
        positions: [u32; 6],
        blocks: [i8; 6 * BLOCK],
        out: [u32; OUT_MAX],
    ) {
        self.phase = Phase::from_code(flags[0]);
        self.depth = flags[1] as u32 & 3;
        self.signed = flags[2] != 0;
        self.bit15 = flags[3] != 0;
        self.dma_in_enabled = flags[4] != 0;
        self.dma_out_enabled = flags[5] != 0;
        self.quant_colour = flags[6] != 0;
        self.quant_factor = flags[7] & 0x3F;
        self.decoding = flags[8] != 0;
        self.quant[0].copy_from_slice(&quant[..BLOCK]);
        self.quant[1].copy_from_slice(&quant[BLOCK..]);
        self.scale = scale;
        self.coeffs = coeffs;
        self.remaining = counters[0];
        // Clamped, not trusted: every one of these indexes an array.
        self.coeff_pos = counters[1] as usize % BLOCK;
        self.block_index = counters[2] as usize % 6;
        self.quant_pos = positions[0] as usize % (2 * BLOCK + 4);
        self.scale_pos = positions[1] as usize % (BLOCK + 2);
        self.out_len = (positions[2] as usize).min(OUT_MAX);
        self.out_pos = (positions[3] as usize).min(OUT_MAX);
        self.in_addr = positions[4] & 0x1F_FFFC;
        self.in_remaining = positions[5];
        for (i, block) in self.blocks.iter_mut().enumerate() {
            block.copy_from_slice(&blocks[i * BLOCK..(i + 1) * BLOCK]);
        }
        self.out = out;
        // `block_started` is not serialized: it is exactly "the block has a
        // quantisation factor yet", which `coeff_pos` already says.
        self.block_started = self.coeff_pos != 0;
    }
}

/// Sign-extend the low 10 bits of a run-length halfword.
fn sign_extend_10(half: u16) -> i16 {
    ((half & 0x3FF) as i16) << 6 >> 6
}

/// Saturate to the width the coefficient buffer is declared at.
///
/// Where hardware saturates is an assumption here, not something measured: a
/// level of 511 with a quantisation factor of 63 and a table entry of 83 is
/// about 334 000, so it saturates somewhere. Sixteen bits is the minimal claim
/// that matches the buffer, and it is called out in `docs/notes/MDEC.md` as
/// unverified rather than presented as known.
fn saturate16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// The inverse DCT, using the cosine matrix software supplied.
///
/// Two passes of a straightforward matrix multiply rather than a fast IDCT.
/// This is the reference shape: it is what the scale table means, and a fast
/// version has to be proven equal to it before it is worth having.
///
/// **The shift is derived, not fitted.** The table software loads holds
/// `c(u) * cos((2x+1) u pi / 16)` scaled by 2^15, with row zero at 23170, which
/// is `2^15 / sqrt(2)`. A one-dimensional IDCT carries a factor of one half, so
/// one pass against that table produces the answer times 2^16, and two passes
/// times 2^32. Anything else here is a brightness error that looks like a
/// colour-space bug: an extra halving turns a flat -128 block into -64.
fn idct(input: &[i16; BLOCK], scale: &[i16; BLOCK], out: &mut [i8; BLOCK]) {
    let mut tmp = [0i64; BLOCK];
    // Pass one: columns.
    for x in 0..BLOCK_SIDE {
        for y in 0..BLOCK_SIDE {
            let mut sum = 0i64;
            for u in 0..BLOCK_SIDE {
                sum += input[u * BLOCK_SIDE + x] as i64 * scale[u * BLOCK_SIDE + y] as i64;
            }
            tmp[y * BLOCK_SIDE + x] = sum;
        }
    }
    // Pass two: rows, with the result rounded and clamped into a signed byte.
    for y in 0..BLOCK_SIDE {
        for x in 0..BLOCK_SIDE {
            let mut sum = 0i64;
            for u in 0..BLOCK_SIDE {
                sum += tmp[y * BLOCK_SIDE + u] * scale[u * BLOCK_SIDE + x] as i64;
            }
            // Round to nearest rather than truncating, which biases a whole
            // frame dark by half a level.
            out[y * BLOCK_SIDE + x] = ((sum + (1 << 31)) >> 32).clamp(-128, 127) as i8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cosine matrix software actually loads, built from its definition.
    ///
    /// Computed rather than pasted, because the point of these tests is that
    /// the IDCT's scaling follows from what the table *means*. Row zero is
    /// `2^15 / sqrt(2)` and the rest are `cos((2x+1) u pi / 16) * 2^15`,
    /// **truncated** toward zero rather than rounded. That last detail is a
    /// measurement, not a preference: Tomb Raider loads 23170 across the top
    /// and then 32138, 27245, 18204, 6392, where rounding to nearest would give
    /// 27246, 18205 and 6393. The table this test builds has to be the one the
    /// games actually load or it is testing a different machine.
    fn standard_scale() -> [i16; BLOCK] {
        let mut t = [0i16; BLOCK];
        for u in 0..BLOCK_SIDE {
            for x in 0..BLOCK_SIDE {
                let c = if u == 0 {
                    1.0 / 2f64.sqrt()
                } else {
                    ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos()
                };
                t[u * BLOCK_SIDE + x] = (c * 32768.0) as i16;
            }
        }
        t
    }

    /// A decoder with the standard scale table and a flat quantisation table,
    /// set up for 15-bit colour output.
    fn decoder(quant: u8) -> Mdec {
        let mut m = Mdec::new();
        m.scale = standard_scale();
        m.quant = [[quant; BLOCK]; 2];
        m.depth = DEPTH_15;
        m
    }

    /// Sanity: the table this file computes is the one the games load.
    #[test]
    fn the_standard_scale_table_is_what_games_load() {
        let t = standard_scale();
        assert_eq!(&t[..8], &[23170; 8], "row zero is 2^15 over root two");
        assert_eq!(t[8], 32138);
        assert_eq!(t[9], 27245);
        assert_eq!(t[10], 18204);
        assert_eq!(t[11], 6392);
    }

    /// A block with only a DC coefficient decodes to a flat value, and the
    /// value is *derived* from the table's scale, not fitted to it.
    ///
    /// With row zero at 23170, two IDCT passes multiply by 23170 squared, which
    /// is 2^32 divided by eight. So a DC of `d` gives a flat `d / 8`. This is
    /// the test that catches a stray halving or a missing round: at -1024 the
    /// answer is -128 and not -64.
    #[test]
    fn a_dc_only_block_is_flat_and_correctly_scaled() {
        let mut m = decoder(1);
        m.coeffs[0] = -1024;
        let mut out = [0i8; BLOCK];
        idct(&m.coeffs, &m.scale, &mut out);
        assert!(
            out.iter().all(|&v| v == -128),
            "expected a flat -128, got {:?}",
            &out[..8]
        );

        m.coeffs = [0; BLOCK];
        m.coeffs[0] = 512;
        idct(&m.coeffs, &m.scale, &mut out);
        assert!(out.iter().all(|&v| v == 64), "got {:?}", &out[..8]);
    }

    /// The quantisation table is indexed by the coefficient's position in the
    /// **zigzag** stream, not by its de-zigzagged destination.
    ///
    /// The table arrives already permuted, so applying the zigzag here applies
    /// it twice. Built with a table whose entries differ at exactly the two
    /// indices that tell the two readings apart: position 2 in the stream lands
    /// at offset 8, so a double zigzag would read entry 8 instead of entry 2.
    #[test]
    fn the_quant_table_is_not_zigzagged_twice() {
        let mut m = decoder(1);
        // A luma block, so the luminance table is the one consulted.
        m.block_index = BLOCK_Y1;
        m.quant[0][2] = 10;
        m.quant[0][8] = 90;
        m.quant_factor = 8;
        m.coeff_pos = 2;
        assert_eq!(ZIGZAG[2], 8, "the two readings differ here");
        // Level 1, table entry 10, factor 8: (1 * 10 * 8 + 4) >> 3 = 10.
        // The wrong reading would take entry 90 and give 90.
        assert_eq!(m.dequant_ac(1), 10, "read the table at the stream position");
    }

    /// The end-of-block marker ends the block. It is not a run/level pair, and
    /// decoding it as one gives a plausible block rather than a visible fault.
    #[test]
    fn end_of_block_ends_the_block() {
        let mut m = decoder(1);
        m.phase = Phase::Decode;
        m.start_macroblock();
        m.feed_coefficients(0x0401); // quant factor 1, DC 1
        assert!(m.block_started);
        assert_eq!(m.block_index, BLOCK_CR, "chroma comes first");
        m.feed_coefficients(END_OF_BLOCK);
        assert_eq!(m.block_index, BLOCK_CB, "moved on to the next block");
        assert!(!m.block_started, "and that one has not started");
    }

    /// A colour macroblock is Cr, Cb, then the four luma blocks, in that order.
    ///
    /// Assuming luma first produces a structurally perfect, wrongly coloured
    /// picture, which is why this is asserted rather than left to the layout.
    #[test]
    fn a_colour_macroblock_takes_chroma_first() {
        let mut m = decoder(1);
        m.phase = Phase::Decode;
        m.start_macroblock();
        for expected in [BLOCK_CR, BLOCK_CB, 2, 3, 4, 5] {
            assert_eq!(m.block_index, expected);
            m.feed_coefficients(0x0401);
            m.feed_coefficients(END_OF_BLOCK);
        }
        assert_eq!(m.macroblocks, 1, "six blocks make one macroblock");
    }

    /// Monochrome output has no chroma blocks at all: one luma block is a whole
    /// macroblock.
    #[test]
    fn a_monochrome_macroblock_is_one_block() {
        let mut m = decoder(1);
        m.depth = DEPTH_8;
        m.phase = Phase::Decode;
        m.start_macroblock();
        assert_eq!(m.block_index, BLOCK_Y1, "starts at luma, not chroma");
        m.feed_coefficients(0x0401);
        m.feed_coefficients(END_OF_BLOCK);
        assert_eq!(m.macroblocks, 1);
    }

    /// The status register's low half is the outstanding count **minus one**,
    /// so an idle chip reads 0xFFFF there.
    #[test]
    fn status_counts_down_from_one_less_than_asked() {
        let mut m = Mdec::new();
        assert_eq!(m.status() & 0xFFFF, 0xFFFF, "idle reads all ones");
        m.write_command(0x6000_0000); // load the scale table, 32 words
        assert_eq!(m.status() & 0xFFFF, 31);
        assert_ne!(
            m.status() & (1 << 29),
            0,
            "and the chip reports itself busy"
        );
    }

    /// A reset empties the FIFOs and abandons the command, and **keeps the
    /// tables**.
    ///
    /// Software loads them once at the start of a video and resets between
    /// frames. Clearing them here costs every frame after the first its colour,
    /// which looks like a decoder bug in the frames that follow a good one.
    #[test]
    fn reset_keeps_the_tables() {
        let mut m = decoder(7);
        m.write_command(0x6000_0000);
        m.write_control(1 << 31);
        assert_eq!(m.status() & 0xFFFF, 0xFFFF, "the command is gone");
        assert_eq!(m.quant[0][0], 7, "the quant table is not");
        assert_eq!(m.scale[8], 32138, "nor the scale table");
    }

    /// Compressed data handed over as a cursor settles the command's word count
    /// immediately, even though the decoding happens later.
    ///
    /// Getting this wrong leaves the chip permanently busy, so the *next*
    /// command word is swallowed as data and no further frame ever decodes.
    #[test]
    fn a_queued_block_settles_the_command_up_front() {
        let mut m = decoder(1);
        m.write_command(0x3800_0010); // decode, 15-bit, 16 words
        assert_eq!(m.remaining, 16);
        m.queue_input(0x1000, 16);
        assert_eq!(m.remaining, 0, "the chip has accepted the whole block");
        assert_eq!(m.status() & (1 << 29), 0, "so it is no longer busy");
        assert!(m.decoding, "but it still has data to chew through");
    }
}
