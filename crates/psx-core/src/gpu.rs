// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The GPU: VRAM, the GP0/GP1 command ports, and the rasterizer.
//!
//! The timing half of this chip already landed separately in [`crate::video`],
//! which owns the beam, HBlank and VBlank. This module owns everything that
//! draws. The two meet in [`Gpu::display_mode`], which tells the video timing
//! what the dot clock divider and display window are.
//!
//! ## What draws today
//!
//! Flat and Gouraud triangles and quads, rectangles (including the fixed 1x1,
//! 8x8 and 16x16 forms), lines and poly-lines, the four semi-transparency
//! blend modes, the mask bit, the drawing area and drawing offset, and all four
//! VRAM transfer commands (fill, CPU to VRAM, VRAM to CPU, VRAM to VRAM).
//!
//! ## What does not
//!
//! **Textures.** Their command words are decoded and skipped correctly, so a
//! textured primitive draws as a solid polygon in its base colour rather than
//! desynchronising the command FIFO, and [`Gpu::textured_primitives`] counts
//! them. That is the next piece of work and it is a large one: texture pages,
//! 4/8/15-bit CLUT lookup, the texture window, and blending.
//!
//! Also absent: 24-bit display output, interlace, and the texture cache the
//! `clut-cache` test exercises.

use crate::video::{Standard, DOT_DIVIDER_256, DOT_DIVIDER_320, DOT_DIVIDER_368};
use crate::video::{DOT_DIVIDER_512, DOT_DIVIDER_640};

pub const VRAM_WIDTH: usize = 1024;
pub const VRAM_HEIGHT: usize = 512;
pub const VRAM_WORDS: usize = VRAM_WIDTH * VRAM_HEIGHT;

/// A vertex as the rasterizer wants it: signed VRAM coordinates plus a colour.
#[derive(Clone, Copy, Default, Debug)]
struct Vertex {
    x: i32,
    y: i32,
    r: i32,
    g: i32,
    b: i32,
}

/// How a primitive combines with what is already in VRAM.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Blend {
    Opaque,
    /// `B/2 + F/2`
    Half,
    /// `B + F`
    Add,
    /// `B - F`
    Subtract,
    /// `B + F/4`
    AddQuarter,
}

impl Blend {
    fn for_mode(mode: u32) -> Blend {
        match mode & 3 {
            0 => Blend::Half,
            1 => Blend::Add,
            2 => Blend::Subtract,
            _ => Blend::AddQuarter,
        }
    }
}

/// A CPU-to-VRAM or VRAM-to-CPU transfer in progress.
#[derive(Clone, Copy, Default)]
struct Transfer {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    /// Halfwords moved so far.
    done: u32,
}

impl Transfer {
    fn total(&self) -> u32 {
        self.width * self.height
    }
    fn finished(&self) -> bool {
        self.done >= self.total()
    }
    /// VRAM coordinates of the next halfword, wrapping inside the rectangle.
    fn next(&self) -> (u32, u32) {
        let x = (self.x + self.done % self.width) & 0x3FF;
        let y = (self.y + self.done / self.width) & 0x1FF;
        (x, y)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Port {
    /// Assembling a GP0 command.
    Command,
    /// Streaming halfwords into VRAM.
    ToVram,
    /// Streaming halfwords out of VRAM.
    FromVram,
    /// A poly-line, which ends on a terminator rather than a word count.
    PolyLine,
}

#[derive(Clone)]
pub struct Gpu {
    pub vram: Vec<u16>,

    fifo: Vec<u32>,
    port: Port,
    transfer: Transfer,

    // Drawing state (GP0 0xE1..0xE6).
    draw_mode: u32,
    texture_window: u32,
    draw_left: i32,
    draw_top: i32,
    draw_right: i32,
    draw_bottom: i32,
    draw_offset_x: i32,
    draw_offset_y: i32,
    /// Force bit 15 set on every pixel written.
    mask_set: bool,
    /// Refuse to write over a pixel that already has bit 15 set.
    mask_check: bool,

    // Display state (GP1).
    display_start_x: u32,
    display_start_y: u32,
    display_range_h: u32,
    display_range_v: u32,
    display_mode: u32,
    display_disabled: bool,
    dma_direction: u32,
    irq: bool,
    /// Latched reply for the next GPUREAD, for the GP1 info commands.
    gpuread_latch: u32,

    /// Textured primitives drawn as flat colour because textures are not
    /// implemented. A number rather than a memory.
    pub textured_primitives: u64,
    /// Primitives discarded for exceeding the GPU's maximum extent.
    pub oversized_primitives: u64,
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu::new()
    }
}

impl Gpu {
    pub fn new() -> Gpu {
        let mut gpu = Gpu {
            vram: vec![0; VRAM_WORDS],
            fifo: Vec::with_capacity(16),
            port: Port::Command,
            transfer: Transfer::default(),
            draw_mode: 0,
            texture_window: 0,
            draw_left: 0,
            draw_top: 0,
            draw_right: 0,
            draw_bottom: 0,
            draw_offset_x: 0,
            draw_offset_y: 0,
            mask_set: false,
            mask_check: false,
            display_start_x: 0,
            display_start_y: 0,
            display_range_h: 0,
            display_range_v: 0,
            display_mode: 0,
            display_disabled: true,
            dma_direction: 0,
            irq: false,
            gpuread_latch: 0,
            textured_primitives: 0,
            oversized_primitives: 0,
        };
        gpu.reset();
        gpu
    }

    /// GP1(0x00): everything back to its power-on state except VRAM, which the
    /// hardware does not clear either.
    pub fn reset(&mut self) {
        self.fifo.clear();
        self.port = Port::Command;
        self.transfer = Transfer::default();
        self.draw_mode = 0;
        self.texture_window = 0;
        self.draw_left = 0;
        self.draw_top = 0;
        self.draw_right = 0;
        self.draw_bottom = 0;
        self.draw_offset_x = 0;
        self.draw_offset_y = 0;
        self.mask_set = false;
        self.mask_check = false;
        self.display_start_x = 0;
        self.display_start_y = 0;
        self.display_range_h = 0x00C6_0260;
        self.display_range_v = 0x0004_0010;
        self.display_mode = 0;
        self.display_disabled = true;
        self.dma_direction = 0;
        self.irq = false;
    }

    // -- registers --------------------------------------------------------

    /// GPUSTAT (`0x1F801814` read).
    pub fn status(&self) -> u32 {
        let mut s = 0u32;
        s |= self.draw_mode & 0x7FF;
        s |= ((self.draw_mode >> 11) & 1) << 15; // texture disable
        s |= (self.mask_set as u32) << 11;
        s |= (self.mask_check as u32) << 12;
        // Bit 13 is the interlace field. Always "odd" while interlace is off.
        s |= 1 << 13;
        s |= (self.display_mode & 0x3F) << 16;
        s |= ((self.display_mode >> 6) & 1) << 14; // reverse flag
        s |= (self.display_disabled as u32) << 23;
        s |= (self.irq as u32) << 24;
        s |= self.dma_direction << 29;

        // Ready flags. This core executes every command the instant its last
        // word arrives, so it is never busy. Reporting otherwise would be a
        // lie in the other direction, and the BIOS spins on these at boot.
        s |= 1 << 26; // ready to receive a command
        s |= ((self.port == Port::FromVram) as u32) << 27; // ready to send VRAM
        s |= 1 << 28; // ready to receive a DMA block

        // Bit 25 is the DMA request line, whose meaning depends on direction.
        let dma_request = match self.dma_direction {
            0 => 0,
            1 => 1,
            2 => 1,                                    // CPU to GP0
            _ => (self.port == Port::FromVram) as u32, // GPUREAD to CPU
        };
        s |= dma_request << 25;
        s
    }

    /// GPUREAD (`0x1F801810` read).
    pub fn read(&mut self) -> u32 {
        if self.port == Port::FromVram {
            let lo = self.take_vram_halfword() as u32;
            let hi = self.take_vram_halfword() as u32;
            return lo | (hi << 16);
        }
        self.gpuread_latch
    }

    fn take_vram_halfword(&mut self) -> u16 {
        if self.transfer.finished() {
            self.port = Port::Command;
            return 0;
        }
        let (x, y) = self.transfer.next();
        self.transfer.done += 1;
        let v = self.vram[y as usize * VRAM_WIDTH + x as usize];
        if self.transfer.finished() {
            self.port = Port::Command;
        }
        v
    }

    /// GP1 (`0x1F801814` write): display and control.
    pub fn gp1(&mut self, word: u32) {
        let cmd = (word >> 24) & 0xFF;
        let args = word & 0x00FF_FFFF;
        match cmd {
            0x00 => self.reset(),
            0x01 => {
                self.fifo.clear();
                self.port = Port::Command;
            }
            0x02 => self.irq = false,
            0x03 => self.display_disabled = args & 1 != 0,
            0x04 => self.dma_direction = args & 3,
            0x05 => {
                self.display_start_x = args & 0x3FF;
                self.display_start_y = (args >> 10) & 0x1FF;
            }
            0x06 => self.display_range_h = args,
            0x07 => self.display_range_v = args,
            0x08 => self.display_mode = args & 0x7F,
            0x09 => {
                // Texture disable. Kept in the draw-mode word's bit 11 so
                // GPUSTAT reports it from one place.
                self.draw_mode = (self.draw_mode & !(1 << 11)) | ((args & 1) << 11);
            }
            0x10..=0x1F => self.gpuread_latch = self.info(args),
            _ => {}
        }
    }

    fn info(&self, args: u32) -> u32 {
        match args & 0x0F {
            2 => self.texture_window,
            3 => (self.draw_left as u32 & 0x3FF) | ((self.draw_top as u32 & 0x3FF) << 10),
            4 => (self.draw_right as u32 & 0x3FF) | ((self.draw_bottom as u32 & 0x3FF) << 10),
            5 => {
                (self.draw_offset_x as u32 & 0x7FF) | ((self.draw_offset_y as u32 & 0x7FF) << 11)
            }
            _ => self.gpuread_latch,
        }
    }

    /// GP0 (`0x1F801810` write): commands and data.
    pub fn gp0(&mut self, word: u32) {
        match self.port {
            Port::ToVram => {
                self.push_vram_halfword(word as u16);
                self.push_vram_halfword((word >> 16) as u16);
                if self.transfer.finished() {
                    self.port = Port::Command;
                }
            }
            Port::FromVram => {
                // Writes during a read transfer are ignored; the CPU is
                // supposed to be reading GPUREAD instead.
            }
            Port::PolyLine => {
                // A poly-line runs until a terminator word arrives.
                if word & 0xF000_F000 == 0x5000_5000 {
                    self.port = Port::Command;
                    self.fifo.clear();
                } else {
                    self.fifo.push(word);
                    self.continue_polyline();
                }
            }
            Port::Command => {
                self.fifo.push(word);
                let cmd = (self.fifo[0] >> 24) as u8;
                if let Some(len) = command_length(cmd) {
                    if self.fifo.len() >= len {
                        self.execute();
                    }
                } else {
                    // Unknown command: drop it rather than stalling the FIFO.
                    self.fifo.clear();
                }
            }
        }
    }

    fn push_vram_halfword(&mut self, value: u16) {
        if self.transfer.finished() {
            return;
        }
        let (x, y) = self.transfer.next();
        self.transfer.done += 1;
        // A transfer writes VRAM directly: no mask check, but the mask-set bit
        // does apply.
        let v = if self.mask_set { value | 0x8000 } else { value };
        self.vram[y as usize * VRAM_WIDTH + x as usize] = v;
    }

    fn execute(&mut self) {
        let words = std::mem::take(&mut self.fifo);
        let cmd = (words[0] >> 24) as u8;

        match cmd {
            0x00 | 0x01 | 0x03..=0x1E => {}
            0x02 => self.fill_rectangle(&words),
            0x1F => self.irq = true,
            0x20..=0x3F => self.draw_polygon(cmd, &words),
            0x40..=0x5F => self.draw_line_command(cmd, &words),
            0x60..=0x7F => self.draw_rectangle(cmd, &words),
            0x80..=0x9F => self.vram_to_vram(&words),
            0xA0..=0xBF => self.begin_transfer(&words, Port::ToVram),
            0xC0..=0xDF => self.begin_transfer(&words, Port::FromVram),
            0xE1 => self.draw_mode = (self.draw_mode & !0x7FF) | (words[0] & 0x7FF),
            0xE2 => self.texture_window = words[0] & 0x000F_FFFF,
            0xE3 => {
                self.draw_left = (words[0] & 0x3FF) as i32;
                self.draw_top = ((words[0] >> 10) & 0x3FF) as i32;
            }
            0xE4 => {
                self.draw_right = (words[0] & 0x3FF) as i32;
                self.draw_bottom = ((words[0] >> 10) & 0x3FF) as i32;
            }
            0xE5 => {
                self.draw_offset_x = sign_extend_11(words[0] & 0x7FF);
                self.draw_offset_y = sign_extend_11((words[0] >> 11) & 0x7FF);
            }
            0xE6 => {
                self.mask_set = words[0] & 1 != 0;
                self.mask_check = words[0] & 2 != 0;
            }
            _ => {}
        }
    }

    // -- transfers --------------------------------------------------------

    fn begin_transfer(&mut self, words: &[u32], port: Port) {
        let x = words[1] & 0x3FF;
        let y = (words[1] >> 16) & 0x1FF;
        // A width or height of zero means the maximum, not nothing.
        let width = ((words[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let height = (((words[2] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1;

        self.transfer = Transfer {
            x,
            y,
            width,
            height,
            done: 0,
        };
        self.port = port;
    }

    fn fill_rectangle(&mut self, words: &[u32]) {
        let colour = to_rgb555(words[0] & 0xFF, (words[0] >> 8) & 0xFF, (words[0] >> 16) & 0xFF);
        // The fill works in 16-pixel units and ignores the drawing area, the
        // mask bit and semi-transparency entirely. It is the one primitive
        // that writes VRAM without going through the usual pixel path.
        let x0 = (words[1] & 0x3F0) as usize;
        let y0 = ((words[1] >> 16) & 0x1FF) as usize;
        let w = (((words[2] & 0x3FF) + 0x0F) & !0x0F) as usize;
        let h = ((words[2] >> 16) & 0x1FF) as usize;

        for dy in 0..h {
            let y = (y0 + dy) & (VRAM_HEIGHT - 1);
            for dx in 0..w {
                let x = (x0 + dx) & (VRAM_WIDTH - 1);
                self.vram[y * VRAM_WIDTH + x] = colour;
            }
        }
    }

    fn vram_to_vram(&mut self, words: &[u32]) {
        let sx = words[1] & 0x3FF;
        let sy = (words[1] >> 16) & 0x1FF;
        let dx = words[2] & 0x3FF;
        let dy = (words[2] >> 16) & 0x1FF;
        let w = ((words[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let h = (((words[3] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1;

        // Read the whole source first. Source and destination are allowed to
        // overlap, and the `vram-to-vram-overlap` test exists precisely because
        // copying in place gives a different (wrong) answer.
        let mut buf = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                let x = (sx + col) & 0x3FF;
                let y = (sy + row) & 0x1FF;
                buf.push(self.vram[y as usize * VRAM_WIDTH + x as usize]);
            }
        }
        let mut i = 0;
        for row in 0..h {
            for col in 0..w {
                let x = (dx + col) & 0x3FF;
                let y = (dy + row) & 0x1FF;
                let src = buf[i];
                i += 1;
                let idx = y as usize * VRAM_WIDTH + x as usize;
                if self.mask_check && self.vram[idx] & 0x8000 != 0 {
                    continue;
                }
                self.vram[idx] = if self.mask_set { src | 0x8000 } else { src };
            }
        }
    }

    // -- primitives -------------------------------------------------------

    fn blend_mode(&self, semi: bool) -> Blend {
        if semi {
            Blend::for_mode(self.draw_mode >> 5)
        } else {
            Blend::Opaque
        }
    }

    fn draw_polygon(&mut self, cmd: u8, words: &[u32]) {
        let gouraud = cmd & 0x10 != 0;
        let quad = cmd & 0x08 != 0;
        let textured = cmd & 0x04 != 0;
        let semi = cmd & 0x02 != 0;
        if textured {
            self.textured_primitives += 1;
        }

        let count = if quad { 4 } else { 3 };
        let mut v = [Vertex::default(); 4];
        // Word 0 is the command and the first vertex's colour, so the vertex
        // data starts at 1.
        let mut i = 1usize;
        let base = words[0];

        for (n, slot) in v.iter_mut().enumerate().take(count) {
            let colour_word = if gouraud && n > 0 {
                let c = words[i];
                i += 1;
                c
            } else {
                base
            };
            let pos = words[i];
            i += 1;
            if textured {
                i += 1; // CLUT / texpage / UV word, decoded but unused
            }
            *slot = self.vertex(pos, colour_word);
        }

        let blend = self.blend_mode(semi);
        self.triangle(v[0], v[1], v[2], gouraud, blend);
        if quad {
            self.triangle(v[1], v[2], v[3], gouraud, blend);
        }
    }

    fn vertex(&self, pos: u32, colour: u32) -> Vertex {
        Vertex {
            x: sign_extend_11(pos & 0x7FF) + self.draw_offset_x,
            y: sign_extend_11((pos >> 16) & 0x7FF) + self.draw_offset_y,
            r: (colour & 0xFF) as i32,
            g: ((colour >> 8) & 0xFF) as i32,
            b: ((colour >> 16) & 0xFF) as i32,
        }
    }

    fn draw_rectangle(&mut self, cmd: u8, words: &[u32]) {
        let size = (cmd >> 3) & 3;
        let textured = cmd & 0x04 != 0;
        let semi = cmd & 0x02 != 0;
        if textured {
            self.textured_primitives += 1;
        }

        let mut i = 2usize;
        if textured {
            i += 1;
        }
        let (w, h) = match size {
            1 => (1i32, 1i32),
            2 => (8, 8),
            3 => (16, 16),
            _ => {
                let d = words[i];
                (
                    (d & 0x3FF) as i32,
                    ((d >> 16) & 0x1FF) as i32,
                )
            }
        };

        let v = self.vertex(words[1], words[0]);
        let blend = self.blend_mode(semi);
        let colour = (v.r, v.g, v.b);

        for y in v.y..v.y + h {
            for x in v.x..v.x + w {
                self.plot(x, y, colour, blend);
            }
        }
    }

    fn draw_line_command(&mut self, cmd: u8, words: &[u32]) {
        let gouraud = cmd & 0x10 != 0;
        let poly = cmd & 0x08 != 0;
        let semi = cmd & 0x02 != 0;
        let blend = self.blend_mode(semi);

        let step = if gouraud { 2 } else { 1 };
        let a = self.vertex(words[1], words[0]);
        let b = if gouraud {
            self.vertex(words[3], words[2])
        } else {
            self.vertex(words[2], words[0])
        };
        self.line(a, b, gouraud, blend);

        if poly {
            // Carry on consuming vertices until the terminator arrives.
            self.fifo.clear();
            self.fifo.push(words[0]);
            self.fifo.extend_from_slice(&words[words.len() - step..]);
            self.port = Port::PolyLine;
        }
    }

    /// One more vertex of a poly-line has arrived.
    fn continue_polyline(&mut self) {
        let gouraud = (self.fifo[0] >> 24) as u8 & 0x10 != 0;
        let step = if gouraud { 2 } else { 1 };
        // fifo[0] is the command word, then the previous vertex, then the new
        // one once enough words have accumulated.
        if self.fifo.len() < 1 + step * 2 {
            return;
        }
        let semi = (self.fifo[0] >> 24) as u8 & 0x02 != 0;
        let blend = self.blend_mode(semi);

        let (a, b) = if gouraud {
            (
                self.vertex(self.fifo[2], self.fifo[1]),
                self.vertex(self.fifo[4], self.fifo[3]),
            )
        } else {
            (
                self.vertex(self.fifo[1], self.fifo[0]),
                self.vertex(self.fifo[2], self.fifo[0]),
            )
        };
        self.line(a, b, gouraud, blend);

        // Keep the command word and the vertex just drawn to.
        let keep: Vec<u32> = self.fifo[1 + step..].to_vec();
        self.fifo.truncate(1);
        self.fifo.extend_from_slice(&keep);
    }

    // -- rasterization ----------------------------------------------------

    /// The GPU refuses primitives wider than 1023 or taller than 511.
    fn oversized(&mut self, xs: [i32; 3], ys: [i32; 3]) -> bool {
        let w = xs.iter().max().unwrap() - xs.iter().min().unwrap();
        let h = ys.iter().max().unwrap() - ys.iter().min().unwrap();
        if w > 1023 || h > 511 {
            self.oversized_primitives += 1;
            return true;
        }
        false
    }

    fn triangle(&mut self, a: Vertex, b: Vertex, c: Vertex, gouraud: bool, blend: Blend) {
        if self.oversized([a.x, b.x, c.x], [a.y, b.y, c.y]) {
            return;
        }

        // Work in a consistent winding so the edge tests share a sign.
        let (a, b, c) = if orient(a, b, c) < 0 { (a, c, b) } else { (a, b, c) };
        let area = orient(a, b, c);
        if area == 0 {
            return;
        }

        let min_x = a.x.min(b.x).min(c.x).max(self.draw_left);
        let max_x = a.x.max(b.x).max(c.x).min(self.draw_right);
        let min_y = a.y.min(b.y).min(c.y).max(self.draw_top);
        let max_y = a.y.max(b.y).max(c.y).min(self.draw_bottom);

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let p = Vertex {
                    x,
                    y,
                    ..Default::default()
                };
                let w0 = orient(b, c, p);
                let w1 = orient(c, a, p);
                let w2 = orient(a, b, p);
                if w0 < 0 || w1 < 0 || w2 < 0 {
                    continue;
                }

                let colour = if gouraud {
                    (
                        (w0 * a.r + w1 * b.r + w2 * c.r) / area,
                        (w0 * a.g + w1 * b.g + w2 * c.g) / area,
                        (w0 * a.b + w1 * b.b + w2 * c.b) / area,
                    )
                } else {
                    (a.r, a.g, a.b)
                };
                // A flat colour is already exact in 5 bits; only interpolated
                // output has anything to dither.
                self.plot_dithered(x, y, colour, blend, gouraud);
            }
        }
    }

    fn line(&mut self, a: Vertex, b: Vertex, gouraud: bool, blend: Blend) {
        if self.oversized([a.x, b.x, a.x], [a.y, b.y, a.y]) {
            return;
        }

        let dx = (b.x - a.x).abs();
        let dy = -(b.y - a.y).abs();
        let sx = if a.x < b.x { 1 } else { -1 };
        let sy = if a.y < b.y { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (a.x, a.y);

        // Interpolate along the longer axis so the shading is even.
        let steps = dx.max(-dy).max(1);
        let mut step = 0i32;

        loop {
            let colour = if gouraud {
                (
                    a.r + (b.r - a.r) * step / steps,
                    a.g + (b.g - a.g) * step / steps,
                    a.b + (b.b - a.b) * step / steps,
                )
            } else {
                (a.r, a.g, a.b)
            };
            self.plot_dithered(x, y, colour, blend, gouraud);

            if x == b.x && y == b.y {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
            step = (step + 1).min(steps);
        }
    }

    /// Draw-mode bit 9: dither 24-bit colour down to 15.
    fn dithering(&self) -> bool {
        self.draw_mode & (1 << 9) != 0
    }

    /// Write one pixel, honouring the drawing area, the mask bit and the blend
    /// mode. Every primitive goes through here; only the VRAM fill does not.
    fn plot(&mut self, x: i32, y: i32, colour: (i32, i32, i32), blend: Blend) {
        self.plot_dithered(x, y, colour, blend, false)
    }

    fn plot_dithered(
        &mut self,
        x: i32,
        y: i32,
        colour: (i32, i32, i32),
        blend: Blend,
        dither: bool,
    ) {
        if x < self.draw_left || x > self.draw_right || y < self.draw_top || y > self.draw_bottom {
            return;
        }
        if !(0..VRAM_WIDTH as i32).contains(&x) || !(0..VRAM_HEIGHT as i32).contains(&y) {
            return;
        }
        let idx = y as usize * VRAM_WIDTH + x as usize;
        let dst = self.vram[idx];
        if self.mask_check && dst & 0x8000 != 0 {
            return;
        }

        let offset = if dither && self.dithering() {
            DITHER[(y & 3) as usize][(x & 3) as usize]
        } else {
            0
        };
        let src = to_rgb555(
            (colour.0 + offset).clamp(0, 255) as u32,
            (colour.1 + offset).clamp(0, 255) as u32,
            (colour.2 + offset).clamp(0, 255) as u32,
        );
        let mut out = if blend == Blend::Opaque {
            src
        } else {
            blend_pixels(dst, src, blend)
        };
        if self.mask_set {
            out |= 0x8000;
        }
        self.vram[idx] = out;
    }

    // -- display ----------------------------------------------------------

    /// Horizontal resolution in pixels, from GP1(0x08).
    pub fn display_width(&self) -> u32 {
        if self.display_mode & (1 << 6) != 0 {
            return 368;
        }
        match self.display_mode & 3 {
            0 => 256,
            1 => 320,
            2 => 512,
            _ => 640,
        }
    }

    pub fn display_height(&self) -> u32 {
        // Bit 2 selects 480, but only with interlace (bit 5) on.
        if self.display_mode & (1 << 2) != 0 && self.display_mode & (1 << 5) != 0 {
            480
        } else {
            240
        }
    }

    /// Video clocks per dot clock, for [`crate::video`].
    pub fn dot_divider(&self) -> u64 {
        if self.display_mode & (1 << 6) != 0 {
            return DOT_DIVIDER_368;
        }
        match self.display_mode & 3 {
            0 => DOT_DIVIDER_256,
            1 => DOT_DIVIDER_320,
            2 => DOT_DIVIDER_512,
            _ => DOT_DIVIDER_640,
        }
    }

    /// NTSC or PAL, from GP1(0x08) bit 3.
    pub fn standard(&self) -> Standard {
        if self.display_mode & (1 << 3) != 0 {
            Standard::Pal
        } else {
            Standard::Ntsc
        }
    }

    pub fn display_disabled(&self) -> bool {
        self.display_disabled
    }

    /// Convert the displayed part of VRAM into XRGB8888 for a frontend.
    ///
    /// `out` must hold at least `display_width() * display_height()` pixels. A
    /// disabled display is black, which is what the hardware shows.
    pub fn framebuffer(&self, out: &mut [u32]) {
        let w = self.display_width() as usize;
        let h = self.display_height() as usize;

        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if i >= out.len() {
                    return;
                }
                out[i] = if self.display_disabled {
                    0
                } else {
                    let vx = (self.display_start_x as usize + x) & (VRAM_WIDTH - 1);
                    let vy = (self.display_start_y as usize + y) & (VRAM_HEIGHT - 1);
                    from_rgb555(self.vram[vy * VRAM_WIDTH + vx])
                };
            }
        }
    }

    // -- save state -------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn parts(&self) -> ([i32; 6], [u32; 9], (u32, u32, bool, bool, bool)) {
        (
            [
                self.draw_left,
                self.draw_top,
                self.draw_right,
                self.draw_bottom,
                self.draw_offset_x,
                self.draw_offset_y,
            ],
            [
                self.draw_mode,
                self.texture_window,
                self.display_start_x,
                self.display_start_y,
                self.display_range_h,
                self.display_range_v,
                self.display_mode,
                self.dma_direction,
                self.gpuread_latch,
            ],
            (
                self.port as u32,
                self.transfer.done,
                self.mask_set,
                self.mask_check,
                self.display_disabled,
            ),
        )
    }

    pub(crate) fn transfer_parts(&self) -> (u32, u32, u32, u32) {
        (
            self.transfer.x,
            self.transfer.y,
            self.transfer.width,
            self.transfer.height,
        )
    }

    pub(crate) fn fifo(&self) -> &[u32] {
        &self.fifo
    }

    pub(crate) fn irq_raised(&self) -> bool {
        self.irq
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        &mut self,
        areas: [i32; 6],
        regs: [u32; 9],
        port: u32,
        transfer: (u32, u32, u32, u32, u32),
        flags: (bool, bool, bool, bool),
        fifo: Vec<u32>,
    ) {
        self.draw_left = areas[0];
        self.draw_top = areas[1];
        self.draw_right = areas[2];
        self.draw_bottom = areas[3];
        self.draw_offset_x = areas[4];
        self.draw_offset_y = areas[5];

        self.draw_mode = regs[0];
        self.texture_window = regs[1];
        self.display_start_x = regs[2];
        self.display_start_y = regs[3];
        self.display_range_h = regs[4];
        self.display_range_v = regs[5];
        self.display_mode = regs[6];
        self.dma_direction = regs[7] & 3;
        self.gpuread_latch = regs[8];

        self.port = match port {
            1 => Port::ToVram,
            2 => Port::FromVram,
            3 => Port::PolyLine,
            _ => Port::Command,
        };
        self.transfer = Transfer {
            x: transfer.0,
            y: transfer.1,
            // Restored verbatim, including a zero. `Transfer::next` divides by
            // the width, but a zero width makes `total` zero, so `finished`
            // short-circuits every caller before it can get there. Clamping to
            // 1 instead would round-trip a fresh GPU into a different state.
            width: transfer.2,
            height: transfer.3,
            done: transfer.4,
        };
        self.mask_set = flags.0;
        self.mask_check = flags.1;
        self.display_disabled = flags.2;
        self.irq = flags.3;
        self.fifo = fifo;
    }
}

// -- helpers --------------------------------------------------------------

/// The 4x4 dither matrix, added to each 8-bit channel before it is truncated
/// to 5 bits.
///
/// Without this, a Gouraud gradient bands: every pixel rounds the same way, so
/// the 8-bit interpolation collapses into visible 5-bit steps. Dithering
/// perturbs the rounding per pixel so the average is right and the banding
/// breaks up into the crosshatch the hardware shows.
///
/// It applies to shaded and textured primitives only, and only when the draw
/// mode's dither bit is set. A flat-coloured primitive has nothing to dither.
const DITHER: [[i32; 4]; 4] = [
    [-4, 0, -3, 1],
    [2, -2, 3, -1],
    [-3, 1, -4, 0],
    [3, -1, 2, -2],
];

/// Twice the signed area of the triangle, and the edge test for a point.
#[inline]
fn orient(a: Vertex, b: Vertex, c: Vertex) -> i32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

#[inline]
fn sign_extend_11(v: u32) -> i32 {
    ((v & 0x7FF) as i32) << 21 >> 21
}

#[inline]
fn to_rgb555(r: u32, g: u32, b: u32) -> u16 {
    (((b >> 3) << 10) | ((g >> 3) << 5) | (r >> 3)) as u16
}

#[inline]
fn from_rgb555(p: u16) -> u32 {
    // 5 bits to 8 by replicating the high bits, so full-scale stays full-scale.
    let r = ((p & 0x1F) as u32) << 3;
    let g = (((p >> 5) & 0x1F) as u32) << 3;
    let b = (((p >> 10) & 0x1F) as u32) << 3;
    let expand = |c: u32| c | (c >> 5);
    (expand(r) << 16) | (expand(g) << 8) | expand(b)
}

fn blend_pixels(dst: u16, src: u16, blend: Blend) -> u16 {
    let unpack = |p: u16| {
        (
            (p & 0x1F) as i32,
            ((p >> 5) & 0x1F) as i32,
            ((p >> 10) & 0x1F) as i32,
        )
    };
    let (br, bg, bb) = unpack(dst);
    let (fr, fg, fb) = unpack(src);

    let f = |b: i32, fr: i32| -> i32 {
        match blend {
            Blend::Half => (b + fr) / 2,
            Blend::Add => b + fr,
            Blend::Subtract => b - fr,
            Blend::AddQuarter => b + fr / 4,
            Blend::Opaque => fr,
        }
    };
    let r = f(br, fr).clamp(0, 31) as u16;
    let g = f(bg, fg).clamp(0, 31) as u16;
    let b = f(bb, fb).clamp(0, 31) as u16;
    (b << 10) | (g << 5) | r
}

/// Words a GP0 command occupies, or `None` if the opcode is unknown.
///
/// Getting one of these wrong does not draw a wrong picture, it desynchronises
/// the FIFO and every command after it is garbage, so the table is worth
/// reading twice.
fn command_length(cmd: u8) -> Option<usize> {
    Some(match cmd {
        0x00 | 0x01 | 0x03..=0x1F => 1,
        0x02 => 3,
        0x20..=0x3F => {
            let gouraud = cmd & 0x10 != 0;
            let quad = cmd & 0x08 != 0;
            let textured = cmd & 0x04 != 0;
            let vertices = if quad { 4 } else { 3 };
            // One word of command+colour, then per vertex a position, plus a
            // texture word if textured, plus a colour for every vertex after
            // the first when Gouraud shaded.
            1 + vertices * (1 + textured as usize) + if gouraud { vertices - 1 } else { 0 }
        }
        0x40..=0x5F => {
            let gouraud = cmd & 0x10 != 0;
            // A poly-line's length is unbounded; take the first segment and let
            // the terminator end it.
            if gouraud {
                4
            } else {
                3
            }
        }
        0x60..=0x7F => {
            let size = (cmd >> 3) & 3;
            let textured = cmd & 0x04 != 0;
            1 + 1 + textured as usize + if size == 0 { 1 } else { 0 }
        }
        0x80..=0x9F => 4,
        0xA0..=0xDF => 3,
        0xE1..=0xE6 => 1,
        0xE0 | 0xE7..=0xFF => 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Gpu {
        let mut g = Gpu::new();
        // A drawing area covering the whole of VRAM, so tests are about the
        // primitives rather than about clipping.
        g.gp0(0xE300_0000);
        g.gp0(0xE400_0000 | 511 << 10 | 1023);
        g
    }

    fn px(g: &Gpu, x: usize, y: usize) -> u16 {
        g.vram[y * VRAM_WIDTH + x]
    }

    #[test]
    fn fill_writes_a_block_of_vram() {
        let mut g = gpu();
        // Fill 32x16 at (16, 8) with pure red.
        g.gp0(0x0200_00FF);
        g.gp0((8 << 16) | 16);
        g.gp0((16 << 16) | 32);

        assert_eq!(px(&g, 16, 8), to_rgb555(255, 0, 0));
        assert_eq!(px(&g, 47, 23), to_rgb555(255, 0, 0));
        assert_eq!(px(&g, 15, 8), 0, "wrote outside the fill");
        assert_eq!(px(&g, 16, 24), 0, "wrote below the fill");
    }

    #[test]
    fn cpu_to_vram_and_back() {
        let mut g = gpu();
        // Upload a 2x2 block at (4, 4).
        g.gp0(0xA000_0000);
        g.gp0((4 << 16) | 4);
        g.gp0((2 << 16) | 2);
        g.gp0(0x2222_1111);
        g.gp0(0x4444_3333);

        assert_eq!(px(&g, 4, 4), 0x1111);
        assert_eq!(px(&g, 5, 4), 0x2222);
        assert_eq!(px(&g, 4, 5), 0x3333);
        assert_eq!(px(&g, 5, 5), 0x4444);

        // And read it back out.
        g.gp0(0xC000_0000);
        g.gp0((4 << 16) | 4);
        g.gp0((2 << 16) | 2);
        assert_eq!(g.read(), 0x2222_1111);
        assert_eq!(g.read(), 0x4444_3333);
    }

    /// The source is read in full before the destination is written, so an
    /// overlapping copy moves the original pixels rather than ones it has just
    /// written.
    #[test]
    fn vram_to_vram_handles_overlap() {
        let mut g = gpu();
        for x in 0..4u16 {
            g.vram[x as usize] = x + 1;
        }
        // Copy (0,0)-(4,1) to (2,0), which overlaps.
        g.gp0(0x8000_0000);
        g.gp0(0);
        g.gp0(2);
        g.gp0((1 << 16) | 4);

        assert_eq!(px(&g, 2, 0), 1);
        assert_eq!(px(&g, 3, 0), 2);
        assert_eq!(px(&g, 4, 0), 3);
        assert_eq!(px(&g, 5, 0), 4);
    }

    #[test]
    fn flat_triangle_fills_its_interior() {
        let mut g = gpu();
        // Green triangle (0,0), (16,0), (0,16).
        g.gp0(0x2000_FF00);
        g.gp0(0);
        g.gp0(16);
        g.gp0(16 << 16);

        let green = to_rgb555(0, 255, 0);
        assert_eq!(px(&g, 1, 1), green, "inside");
        assert_eq!(px(&g, 14, 14), 0, "outside the hypotenuse");
    }

    #[test]
    fn gouraud_triangle_interpolates() {
        let mut g = gpu();
        // Red at the origin, blue at the far corners.
        g.gp0(0x3000_00FF);
        g.gp0(0);
        g.gp0(0x00FF_0000);
        g.gp0(64);
        g.gp0(0x00FF_0000);
        g.gp0(64 << 16);

        let near = px(&g, 1, 1);
        let far = px(&g, 30, 30);
        assert_ne!(near, far, "the shading did not vary across the triangle");
        assert!(near & 0x1F > (far & 0x1F), "red should fall off from the origin");
    }

    #[test]
    fn quad_covers_both_halves() {
        let mut g = gpu();
        g.gp0(0x2800_00FF); // flat quad, red
        g.gp0(0);
        g.gp0(32);
        g.gp0(32 << 16);
        g.gp0((32 << 16) | 32);

        let red = to_rgb555(255, 0, 0);
        assert_eq!(px(&g, 2, 2), red, "first triangle");
        assert_eq!(px(&g, 30, 30), red, "second triangle");
    }

    #[test]
    fn rectangles_use_their_fixed_sizes() {
        let mut g = gpu();
        g.gp0(0x7000_00FF); // 8x8, red
        g.gp0((4 << 16) | 4);

        let red = to_rgb555(255, 0, 0);
        assert_eq!(px(&g, 4, 4), red);
        assert_eq!(px(&g, 11, 11), red);
        assert_eq!(px(&g, 12, 12), 0, "8x8 should stop at 11");
    }

    #[test]
    fn drawing_area_clips() {
        let mut g = gpu();
        g.gp0(0xE300_0000 | (4 << 10) | 4); // top-left (4,4)
        g.gp0(0xE400_0000 | (8 << 10) | 8); // bottom-right (8,8)

        g.gp0(0x6000_00FF); // variable-size rectangle
        g.gp0(0);
        g.gp0((32 << 16) | 32);

        assert_eq!(px(&g, 3, 3), 0, "outside the drawing area");
        assert_ne!(px(&g, 4, 4), 0, "inside");
        assert_ne!(px(&g, 8, 8), 0, "inside, at the corner");
        assert_eq!(px(&g, 9, 9), 0, "past the bottom-right");
    }

    #[test]
    fn drawing_offset_shifts_primitives() {
        let mut g = gpu();
        g.gp0(0xE500_0000 | (10 << 11) | 10); // offset (10, 10)
        g.gp0(0x6800_00FF); // 1x1 rectangle at (0,0)
        g.gp0(0);

        assert_ne!(px(&g, 10, 10), 0, "the offset was not applied");
        assert_eq!(px(&g, 0, 0), 0);
    }

    #[test]
    fn mask_check_protects_pixels() {
        let mut g = gpu();
        g.vram[5 * VRAM_WIDTH + 5] = 0x8000; // already masked
        g.gp0(0xE600_0002); // check before drawing, do not set

        g.gp0(0x6000_00FF);
        g.gp0(0);
        g.gp0((16 << 16) | 16);

        assert_eq!(px(&g, 5, 5), 0x8000, "drew over a masked pixel");
        assert_ne!(px(&g, 6, 6), 0, "should have drawn elsewhere");
    }

    #[test]
    fn semi_transparency_blends_with_the_background() {
        let mut g = gpu();
        // Background: mid grey.
        g.vram[0] = to_rgb555(128, 128, 128);
        // Blend mode 0 (B/2 + F/2) is the draw-mode default.
        g.gp0(0x6A00_0000); // semi-transparent 1x1 rectangle... size 1 = bit 3
        g.gp0(0);

        // With black drawn at half weight over grey, the result must be darker
        // than the background but not zero.
        let out = px(&g, 0, 0);
        assert!(out & 0x1F < (to_rgb555(128, 0, 0) & 0x1F));
    }

    /// Vertex coordinates are 11-bit **signed**, so the way to exceed the
    /// 1023-wide limit is a span from one end of the range to the other, not a
    /// single large number: `2000` in eleven bits is -48.
    #[test]
    fn oversized_primitives_are_discarded() {
        let mut g = gpu();
        g.gp0(0x2000_00FF);
        g.gp0(0x418); // x = -1000
        g.gp0(1000); // x = +1000, so the span is 2000
        g.gp0(16 << 16);

        assert_eq!(g.oversized_primitives, 1);
        assert_eq!(px(&g, 1, 1), 0, "an oversized primitive drew anyway");
    }

    /// The companion to the above: an eleven-bit coordinate wraps to negative
    /// past 1023, which is a real behaviour and not a clamp.
    #[test]
    fn vertex_coordinates_are_eleven_bit_signed() {
        let mut g = gpu();
        g.gp0(0x6800_00FF); // 1x1 rectangle
        g.gp0(0x0410_0410); // x = y = 0x410, which is -1008

        // Nothing should appear at 1040; the vertex went negative and clipped.
        assert_eq!(g.oversized_primitives, 0);
        assert_eq!(px(&g, 16, 16), 0);
    }

    /// A wrong length here does not draw a wrong picture, it desynchronises
    /// every command after it.
    #[test]
    fn command_lengths_match_the_encoding() {
        assert_eq!(command_length(0x20), Some(4)); // flat triangle
        assert_eq!(command_length(0x28), Some(5)); // flat quad
        assert_eq!(command_length(0x30), Some(6)); // gouraud triangle
        assert_eq!(command_length(0x38), Some(8)); // gouraud quad
        assert_eq!(command_length(0x24), Some(7)); // textured triangle
        assert_eq!(command_length(0x2C), Some(9)); // textured quad
        assert_eq!(command_length(0x34), Some(9)); // gouraud textured triangle
        assert_eq!(command_length(0x3C), Some(12)); // gouraud textured quad
        assert_eq!(command_length(0x60), Some(3)); // variable rectangle
        assert_eq!(command_length(0x68), Some(2)); // 1x1
        assert_eq!(command_length(0x64), Some(4)); // textured variable
        assert_eq!(command_length(0x40), Some(3)); // flat line
        assert_eq!(command_length(0x50), Some(4)); // gouraud line
        assert_eq!(command_length(0x02), Some(3)); // fill
        assert_eq!(command_length(0x80), Some(4)); // vram to vram
        assert_eq!(command_length(0xA0), Some(3)); // cpu to vram
    }

    #[test]
    fn status_reports_the_ready_flags_the_bios_spins_on() {
        let g = Gpu::new();
        let s = g.status();
        assert_ne!(s & (1 << 26), 0, "ready to receive a command");
        assert_ne!(s & (1 << 28), 0, "ready to receive a DMA block");
    }

    #[test]
    fn display_mode_selects_resolution_and_dot_clock() {
        let mut g = Gpu::new();
        g.gp1(0x0800_0000); // 256 wide, NTSC
        assert_eq!(g.display_width(), 256);
        assert_eq!(g.dot_divider(), DOT_DIVIDER_256);

        g.gp1(0x0800_0001); // 320
        assert_eq!(g.display_width(), 320);
        assert_eq!(g.dot_divider(), DOT_DIVIDER_320);

        g.gp1(0x0800_0003); // 640
        assert_eq!(g.display_width(), 640);
        assert_eq!(g.dot_divider(), DOT_DIVIDER_640);

        g.gp1(0x0800_0008); // PAL
        assert_eq!(g.standard(), Standard::Pal);
    }

    #[test]
    fn framebuffer_converts_the_display_area() {
        let mut g = Gpu::new();
        g.gp1(0x0800_0001); // 320x240
        g.gp1(0x0300_0000); // display enabled
        g.vram[0] = to_rgb555(255, 0, 0);

        let mut out = vec![0u32; 320 * 240];
        g.framebuffer(&mut out);
        assert_eq!(out[0] >> 16 & 0xFF, 0xFF, "red channel");
        assert_eq!(out[0] & 0xFF, 0x00, "blue channel");
    }

    #[test]
    fn a_disabled_display_is_black() {
        let mut g = Gpu::new();
        g.gp1(0x0800_0001);
        g.gp1(0x0300_0001); // disabled
        g.vram[0] = to_rgb555(255, 255, 255);

        let mut out = vec![0xDEAD_BEEFu32; 320 * 240];
        g.framebuffer(&mut out);
        assert_eq!(out[0], 0);
    }
}
