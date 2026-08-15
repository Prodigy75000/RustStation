// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! `shot`: run something and write out what the GPU drew.
//!
//!   shot <bios.bin> [--exe file.exe] [--steps N] [--out shot.png] [--vram]
//!
//! Two views, and the difference matters when a picture looks wrong:
//!
//! * the **display area** (default), which is what a television would show, and
//! * the whole **1024x512 VRAM** (`--vram`), which is what the GPU actually
//!   drew, including the parts nothing is displaying.
//!
//! A primitive drawn correctly into the wrong place looks identical to one
//! drawn wrongly, until you look at VRAM. The hardware test suite ships its
//! reference images as full VRAM dumps (`vram.png`) for the same reason, so
//! `--vram` is the one to compare against them.
//!
//! `--compare <reference.png>` does that comparison and prints a number, which
//! beats squinting at two images.
//!
//! ## The two 5-bit-to-8-bit conversions
//!
//! VRAM stores 5 bits per channel. There are two ways to widen that, and they
//! disagree at the top of the range:
//!
//! * `c << 3`, which maps 31 to 248. This is what the suite's reference dumps
//!   use, so it is what `--vram` writes and compares against.
//! * `(c << 3) | (c >> 2)`, which maps 31 to 255. Better on a display, because
//!   full-scale white stays white, and what the libretro framebuffer uses.
//!
//! Neither is more correct in the abstract; mixing them up turns a pixel-exact
//! match into a whole-image mismatch of 7 per channel, which is exactly the
//! sort of difference that gets misread as a rasterizer bug.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use psx_core::gpu::{VRAM_HEIGHT, VRAM_WIDTH};
use psx_core::{exe::Exe, Psx};

/// `--hold cross,start` into a pad button mask. Duplicated from `testrom`
/// rather than shared: these two binaries have no common module, and a
/// sixteen-line table is cheaper than inventing one for it.
fn psx_runner_buttons(list: &str) -> Result<u16, String> {
    use psx_core::sio::button as b;
    let mut bits = 0u16;
    for name in list.split(',').filter(|s| !s.is_empty()) {
        let bit = match name.trim().to_ascii_lowercase().as_str() {
            "select" | "sel" => b::SELECT,
            "l3" => b::L3,
            "r3" => b::R3,
            "start" => b::START,
            "up" => b::UP,
            "right" => b::RIGHT,
            "down" => b::DOWN,
            "left" => b::LEFT,
            "l2" => b::L2,
            "r2" => b::R2,
            "l1" => b::L1,
            "r1" => b::R1,
            "triangle" => b::TRIANGLE,
            "circle" => b::CIRCLE,
            "cross" | "x" => b::CROSS,
            "square" => b::SQUARE,
            other => return Err(other.to_string()),
        };
        bits |= 1 << bit;
    }
    Ok(bits)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: shot <bios.bin> [--exe file.exe] [--steps N] [--out shot.png] [--vram] \n             [--hold BUTTON,BUTTON]"
        );
        return ExitCode::FAILURE;
    }

    let mut bios_path = None;
    let mut exe_path: Option<String> = None;
    let mut out_path = "shot.png".to_string();
    let mut steps: u64 = 60_000_000;
    let mut whole_vram = false;
    let mut compare_path: Option<String> = None;
    let mut hold = 0u16;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--exe" => {
                i += 1;
                exe_path = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out_path = args.get(i).cloned().unwrap_or(out_path);
            }
            "--steps" => {
                i += 1;
                steps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(steps);
            }
            "--vram" => whole_vram = true,
            "--hold" => {
                i += 1;
                hold = args
                    .get(i)
                    .map(|s| psx_runner_buttons(s))
                    .unwrap_or(Ok(0))
                    .unwrap_or_else(|name| {
                        eprintln!("unknown button {name:?}, holding nothing");
                        0
                    });
            }
            "--compare" => {
                i += 1;
                compare_path = args.get(i).cloned();
                whole_vram = true;
            }
            other => bios_path = Some(other.to_string()),
        }
        i += 1;
    }

    let Some(bios_path) = bios_path else {
        eprintln!("no BIOS path given");
        return ExitCode::FAILURE;
    };
    let bios = match std::fs::read(&bios_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read BIOS {bios_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut psx = match Psx::new(bios) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(path) = &exe_path {
        let image = match std::fs::read(path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cannot read EXE {path}: {e}");
                return ExitCode::FAILURE;
            }
        };
        match Exe::parse(&image) {
            Ok(exe) => psx.sideload_exe(exe),
            Err(e) => {
                eprintln!("cannot parse EXE {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    psx.bus.sio.pads[0].buttons = hold;

    psx.run(steps);

    let (width, height, pixels) = if whole_vram {
        let mut buf = vec![0u8; VRAM_WIDTH * VRAM_HEIGHT * 3];
        for (i, px) in psx.bus.gpu.vram.iter().enumerate() {
            let (r, g, b) = rgb555_to_rgb888(*px);
            buf[i * 3] = r;
            buf[i * 3 + 1] = g;
            buf[i * 3 + 2] = b;
        }
        (VRAM_WIDTH, VRAM_HEIGHT, buf)
    } else {
        let w = psx.bus.gpu.display_width() as usize;
        let h = psx.bus.gpu.display_height() as usize;
        let mut fb = vec![0u32; w * h];
        psx.bus.gpu.framebuffer(&mut fb);
        let mut buf = vec![0u8; w * h * 3];
        for (i, px) in fb.iter().enumerate() {
            buf[i * 3] = (px >> 16) as u8;
            buf[i * 3 + 1] = (px >> 8) as u8;
            buf[i * 3 + 2] = *px as u8;
        }
        (w, h, buf)
    };

    let file = match File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot write {out_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    match encoder.write_header().and_then(|mut w| w.write_image_data(&pixels)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("cannot encode {out_path}: {e}");
            return ExitCode::FAILURE;
        }
    }

    let non_black = pixels.chunks(3).filter(|p| p != &[0, 0, 0]).count();
    println!("wrote {out_path} ({width}x{height})");
    println!(
        "sio: {} bytes exchanged, {} answered by a device",
        psx.bus.sio.transfers, psx.bus.sio.acknowledged
    );
    println!(
        "{non_black} non-black pixels, display {}x{}{}",
        psx.bus.gpu.display_width(),
        psx.bus.gpu.display_height(),
        if psx.bus.gpu.display_disabled() {
            " (display OFF)"
        } else {
            ""
        }
    );
    println!(
        "gpu: {} textured primitives drawn untextured, {} oversized discarded",
        psx.bus.gpu.textured_primitives, psx.bus.gpu.oversized_primitives
    );
    println!(
        "dma: {} transfers on unimplemented channels",
        psx.bus.dma.unimplemented_transfers
    );

    if let Some(reference) = compare_path {
        return match compare(&reference, width, height, &pixels) {
            Ok(exact) => {
                if exact {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("cannot compare against {reference}: {e}");
                ExitCode::FAILURE
            }
        };
    }

    ExitCode::SUCCESS
}

/// Diff our VRAM against a reference dump and report a number.
///
/// Returns whether every pixel matched exactly.
fn compare(path: &str, width: usize, height: usize, ours: &[u8]) -> Result<bool, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(file);
    // Some of the suite's dumps are palettised. Expand them so one comparison
    // path handles every reference.
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;

    if info.width as usize != width || info.height as usize != height {
        return Err(format!(
            "reference is {}x{}, ours is {width}x{height}",
            info.width, info.height
        ));
    }
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => return Err(format!("unsupported reference colour type {other:?}")),
    };

    let mut differing = 0usize;
    let mut worst = 0i32;
    // One step of a 5-bit channel is 8 in 8-bit terms. Bucketing by that
    // separates "the same picture, rounded differently" (dithering, fill-rule
    // edges) from "a structurally different picture", which the single
    // percentage cannot.
    let mut one_step = 0usize;
    let mut few_steps = 0usize;
    let mut gross = 0usize;

    for i in 0..width * height {
        let r = &buf[i * channels..i * channels + 3];
        let o = &ours[i * 3..i * 3 + 3];
        let delta = (0..3)
            .map(|c| (r[c] as i32 - o[c] as i32).abs())
            .max()
            .unwrap_or(0);
        if delta != 0 {
            differing += 1;
            worst = worst.max(delta);
            match delta {
                d if d <= 8 => one_step += 1,
                d if d <= 32 => few_steps += 1,
                _ => gross += 1,
            }
        }
    }

    let total = width * height;
    let pct = |n: usize| n as f64 * 100.0 / total as f64;
    println!("--- compared against {path}");
    println!(
        "    {differing} of {total} pixels differ ({:.3}%), worst channel delta {worst}",
        pct(differing)
    );
    println!(
        "    within one 5-bit step: {one_step} ({:.3}%)   within four: {few_steps} ({:.3}%)   beyond: {gross} ({:.3}%)",
        pct(one_step),
        pct(few_steps),
        pct(gross)
    );
    Ok(differing == 0)
}

/// The suite's reference dumps widen 5-bit channels with a plain shift, so 31
/// becomes 248 rather than 255. Matching that is what makes a pixel-exact
/// comparison possible; see the module comment.
fn rgb555_to_rgb888(p: u16) -> (u8, u8, u8) {
    let expand = |c: u16| (c << 3) as u8;
    (
        expand(p & 0x1F),
        expand((p >> 5) & 0x1F),
        expand((p >> 10) & 0x1F),
    )
}
