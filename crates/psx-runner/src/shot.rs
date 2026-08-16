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

mod disasm;

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
            "usage: shot <bios.bin> [--exe file.exe] [--disc game.cue] [--steps N]\n\
             \x20            [--out shot.png] [--vram] [--compare ref.png] [--hold BUTTON,..]\n\
             diagnostics: [--pchist] [--peek ADDR[:N]] [--regs] [--film N]\n\
             \x20            --film 0 writes VRAM once per transfer, not every N steps"
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
    let mut disc_path: Option<String> = None;
    let mut pchist = false;
    let mut peeks: Vec<(u32, u32)> = Vec::new();
    let mut regs = false;
    let mut film: Option<u64> = None;
    let mut watch: Option<u32> = None;

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
            "--pchist" => pchist = true,
            "--regs" => regs = true,
            "--watch" => {
                i += 1;
                watch = args
                    .get(i)
                    .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
            }
            "--film" => {
                i += 1;
                film = args.get(i).and_then(|s| s.parse().ok());
            }
            "--peek" => {
                i += 1;
                match args.get(i).and_then(|s| parse_peek(s)) {
                    Some(peek) => peeks.push(peek),
                    None => {
                        eprintln!("--peek wants ADDR[:COUNT] in hex, e.g. 800592F4:8");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--disc" => {
                i += 1;
                disc_path = args.get(i).cloned();
            }
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

    if let Some(path) = &disc_path {
        match psx_core::disc::Disc::open(std::path::Path::new(path)) {
            Ok(d) => {
                println!("disc: {} tracks, {} sectors", d.tracks.len(), d.length);
                psx.bus.cdrom.disc = Some(d);
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let history = if let Some(addr) = watch {
        run_watching(&mut psx, steps, addr);
        None
    } else if pchist {
        Some(run_recording(&mut psx, steps))
    } else if let Some(every) = film {
        if every == 0 {
            run_filmed_by_transfer(&mut psx, steps, &out_path);
        } else {
            run_filmed(&mut psx, steps, every, &out_path);
        }
        None
    } else {
        psx.run(steps);
        None
    };

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
        "stubs: {} reads, {} writes on decoded-but-unemulated ports; {} unmapped reads, {} unmapped writes",
        psx.bus.stub_reads, psx.bus.stub_writes, psx.bus.unmapped_reads, psx.bus.unmapped_writes
    );
    println!(
        "spu: {} bytes into sound RAM (no audio produced)",
        psx.bus.spu.bytes_written
    );
    println!(
        "cdrom: {} commands ({} unknown), {} sectors read ({} XA audio kept by the drive)",
        psx.bus.cdrom.commands,
        psx.bus.cdrom.unknown_commands,
        psx.bus.cdrom.sectors_read,
        psx.bus.cdrom.xa_sectors
    );
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
        "gpu: {} textured primitives, {} oversized discarded",
        psx.bus.gpu.textured_primitives, psx.bus.gpu.oversized_primitives
    );
    let waiting: Vec<String> = (0..7u8)
        .filter(|n| psx.bus.dma.unimplemented_channels & (1 << n) != 0)
        .map(|n| n.to_string())
        .collect();
    println!(
        "dma: {} transfers on unimplemented channels{}",
        psx.bus.dma.unimplemented_transfers,
        if waiting.is_empty() {
            String::new()
        } else {
            format!(" ({})", waiting.join(", "))
        }
    );
    for (addr, count) in psx.bus.unmapped_sites {
        if count > 0 {
            println!("    unmapped {addr:08X} touched {count} times");
        }
    }
    println!("mdec: {} macroblocks decoded", psx.bus.mdec.macroblocks);
    let irqs: Vec<String> = (0..psx_core::irq::SOURCES)
        .filter(|n| psx.bus.irq.raised[*n] > 0)
        .map(|n| {
            format!(
                "{} {}/{}",
                psx_core::irq::NAMES[n],
                psx.bus.irq.delivered[n],
                psx.bus.irq.raised[n]
            )
        })
        .collect();
    println!("irq (delivered/raised): {}", irqs.join(", "));
    println!(
        "gte: {} colour channels clamped, {} unknown commands",
        psx.cpu.gte.colour_saturations, psx.cpu.gte.unknown_commands
    );
    if psx.bus.gpu.abandoned_transfers > 0 {
        println!(
            "gpu: {} VRAM transfers cut short by a new command",
            psx.bus.gpu.abandoned_transfers
        );
    }
    if let Some(history) = &history {
        report_pchist(history);
    }
    if regs {
        println!("pc {:08X}", psx.cpu.pc);
        for (n, value) in psx.cpu.regs().iter().enumerate() {
            print!("    {:>4} {value:08X}", disasm::reg_name(n as u32));
            if n % 4 == 3 {
                println!();
            }
        }
    }
    for (addr, count) in peeks {
        println!("peek {addr:08X}:");
        report_peek(&psx, addr, count);
    }

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

/// `800592F4:8` into an address and an instruction count. Hex without a `0x`,
/// because every address in this program's own output is printed that way and
/// pasting one back in should just work.
fn parse_peek(spec: &str) -> Option<(u32, u32)> {
    let (addr, count) = match spec.split_once(':') {
        Some((a, c)) => (a, c.parse().ok()?),
        None => (spec, 8),
    };
    Some((u32::from_str_radix(addr.trim_start_matches("0x"), 16).ok()?, count))
}

/// Fetch a word the way an observer would, not the way the CPU does.
///
/// Straight out of the RAM and BIOS arrays, never through [`psx_core::bus::Bus`]
/// `load`, so that reading memory to find out why something hung can never
/// itself advance the clock, drain a FIFO or acknowledge an interrupt.
fn peek_word(psx: &Psx, addr: u32) -> Option<u32> {
    let phys = psx_core::bus::mask_region(addr) as usize;
    let bytes = if phys < psx_core::bus::RAM_SIZE {
        &psx.bus.ram[phys..]
    } else if (0x1FC0_0000..0x1FC0_0000 + psx_core::bus::BIOS_SIZE).contains(&phys) {
        &psx.bus.bios()[phys - 0x1FC0_0000..]
    } else {
        return None;
    };
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Disassemble `count` instructions from `addr`.
fn report_peek(psx: &Psx, addr: u32, count: u32) {
    for i in 0..count {
        let pc = addr.wrapping_add(i * 4);
        match peek_word(psx, pc) {
            Some(word) => println!("    {pc:08X}  {word:08X}  {}", disasm::disasm(word, pc)),
            None => println!("    {pc:08X}  (not RAM or BIOS)"),
        }
    }
}

/// How many changes `--watch` reports before it stops printing.
const WATCH_REPORTS: usize = 24;

/// Run, reporting which instruction changed the word at `addr`.
///
/// The complement of `--pchist`. That one answers "what is it executing"; this
/// one answers "who wrote this", which is the question left when a program is
/// spinning on a flag in its own memory and the interesting party is whatever
/// was supposed to set it. Sampling the word once per instruction rather than
/// hooking the store path keeps the core free of a debugging concern and costs
/// one load per step.
///
/// The program counter reported is the instruction *before* the change was
/// observed, which is the store itself unless the write came from DMA, in which
/// case it is whatever instruction triggered the transfer. Both are the answer
/// to the question being asked.
fn run_watching(psx: &mut Psx, steps: u64, addr: u32) {
    let mut last = peek_word(psx, addr).unwrap_or(0);
    let mut reports = 0;
    let mut changes: u64 = 0;
    for _ in 0..steps {
        let pc = psx.cpu.pc;
        psx.step();
        let now = peek_word(psx, addr).unwrap_or(0);
        if now != last {
            changes += 1;
            if reports < WATCH_REPORTS {
                println!("watch {addr:08X}: {last:08X} -> {now:08X} at pc {pc:08X}");
                reports += 1;
            }
            last = now;
        }
    }
    println!("watch {addr:08X}: {changes} changes, now {last:08X}");
}

/// Run, writing the whole of VRAM out every `every` instructions.
///
/// The end of a run says what went wrong; this says *when*, which for anything
/// the machine builds in stages is the more useful half. A texture that is
/// garbage in the final dump was either uploaded as garbage or was fine and got
/// overwritten, and those two need opposite fixes.
fn run_filmed(psx: &mut Psx, steps: u64, every: u64, out_path: &str) {
    let stem = out_path.strip_suffix(".png").unwrap_or(out_path);
    let mut frame = 0;
    for n in 0..steps {
        if n % every == 0 {
            write_vram(psx, &format!("{stem}-{frame:04}.png"));
            frame += 1;
        }
        psx.step();
    }
    write_vram(psx, &format!("{stem}-{frame:04}.png"));
}

/// Run, writing VRAM out once per VRAM transfer instead of once per N
/// instructions.
///
/// Instruction counts are the wrong clock for watching a texture get built: a
/// texture appears in one transfer, and the interval that catches it is
/// different in every run. Transfers are the units the work actually happens
/// in, so one frame each is both the finest useful granularity and a bounded
/// number of files.
fn run_filmed_by_transfer(psx: &mut Psx, steps: u64, out_path: &str) {
    let stem = out_path.strip_suffix(".png").unwrap_or(out_path);
    let mut seen = psx.bus.gpu.transfers;
    let mut frame = 0;
    // A transfer's *last* word is what completes it, so dump after stepping,
    // not before: dumping on the instruction that started it shows the state
    // before any of the data arrived.
    for _ in 0..steps {
        psx.step();
        if psx.bus.gpu.transfers != seen {
            seen = psx.bus.gpu.transfers;
            write_vram(psx, &format!("{stem}-t{frame:04}.png"));
            frame += 1;
        }
    }
}

/// How many of the most recent instruction addresses `--pchist` keeps. A power
/// of two so the ring index is a mask, and large enough that a loop with a slow
/// outer iteration still shows its whole body.
const HISTORY: usize = 1 << 20;

/// Run, remembering the last [`HISTORY`] program counters.
///
/// A ring buffer rather than a live histogram on purpose: a hash lookup per
/// instruction is affordable for a million steps and not for sixty million, and
/// the question this answers is always "what was it doing *at the end*". The
/// beginning of a run that hangs is not interesting; the last few thousand
/// instructions are the whole of it.
fn run_recording(psx: &mut Psx, steps: u64) -> Vec<u32> {
    let mut ring = vec![0u32; HISTORY];
    let mut n: usize = 0;
    for _ in 0..steps {
        ring[n & (HISTORY - 1)] = psx.cpu.pc;
        n += 1;
        psx.step();
    }
    if n < HISTORY {
        ring.truncate(n);
        return ring;
    }
    // Put it back in execution order, oldest first.
    ring.rotate_left(n & (HISTORY - 1));
    ring
}

/// Report where a run spent its final instructions.
///
/// Two numbers matter more than the ranking. **How many distinct addresses**
/// separates a tight spin (a handful) from a machine that is still doing work
/// but never finishing (thousands). And the **extent**, low to high, says
/// whether the loop lives in one function or is a real call graph.
fn report_pchist(history: &[u32]) {
    if history.is_empty() {
        return;
    }
    let mut counts: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for pc in history {
        *counts.entry(*pc).or_insert(0) += 1;
    }
    let mut ranked: Vec<(u32, u64)> = counts.into_iter().collect();
    // Count first, then address, so equal counts print in address order and two
    // runs of the same hang produce the same listing.
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let low = history.iter().min().expect("non-empty");
    let high = history.iter().max().expect("non-empty");
    println!(
        "pc: {} instructions, {} distinct addresses, {low:08X}..{high:08X}",
        history.len(),
        ranked.len()
    );
    println!("    last executed {:08X}", history[history.len() - 1]);
    for (pc, count) in ranked.iter().take(16) {
        let share = *count as f64 * 100.0 / history.len() as f64;
        println!("    {pc:08X}  {count:>9}  {share:5.1}%");
    }

    // The same data grouped into contiguous stretches of code. One hot spin
    // loop fills the ranking above and hides everything else, and "everything
    // else" is the interesting half when the question is what a waiting program
    // still has running: an interrupt handler that fires ten thousand times is
    // a hundred addresses with a hundred counts each, and it never places.
    let mut addresses: Vec<(u32, u64)> = ranked.clone();
    addresses.sort_unstable_by_key(|a| a.0);
    let mut regions: Vec<(u32, u32, u64)> = Vec::new();
    for (pc, count) in addresses {
        match regions.last_mut() {
            // A gap of one instruction is a delay slot or a short forward
            // branch over one; anything wider is a different piece of code.
            Some(last) if pc <= last.1 + 32 => {
                last.1 = pc;
                last.2 += count;
            }
            _ => regions.push((pc, pc, count)),
        }
    }
    regions.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    println!("    {} contiguous regions, the busiest:", regions.len());
    for (low, high, count) in regions.iter().take(24) {
        // The cut has to be very low. A device interrupt handler in a program
        // that is otherwise spinning fires a handful of times per million
        // instructions, so it is a few hundredths of a percent, and it is
        // exactly the thing worth seeing: whether it runs at all answers most
        // of the question.
        if *count * 10_000 < history.len() as u64 {
            break;
        }
        let share = *count as f64 * 100.0 / history.len() as f64;
        println!("    {low:08X}..{high:08X}  {count:>9}  {share:5.2}%");
    }
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
/// Write the whole of VRAM to a PNG. Used by `--film`, which is how a picture
/// that is wrong by the time the run ends gets watched being built.
fn write_vram(psx: &Psx, path: &str) {
    let mut buf = vec![0u8; VRAM_WIDTH * VRAM_HEIGHT * 3];
    for (i, px) in psx.bus.gpu.vram.iter().enumerate() {
        let (r, g, b) = rgb555_to_rgb888(*px);
        buf[i * 3] = r;
        buf[i * 3 + 1] = g;
        buf[i * 3 + 2] = b;
    }
    let Ok(file) = File::create(path) else {
        eprintln!("cannot write {path}");
        return;
    };
    let mut encoder = png::Encoder::new(BufWriter::new(file), VRAM_WIDTH as u32, VRAM_HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    if let Err(e) = encoder.write_header().and_then(|mut w| w.write_image_data(&buf)) {
        eprintln!("cannot encode {path}: {e}");
    }
}

fn rgb555_to_rgb888(p: u16) -> (u8, u8, u8) {
    let expand = |c: u16| (c << 3) as u8;
    (
        expand(p & 0x1F),
        expand((p >> 5) & 0x1F),
        expand((p >> 10) & 0x1F),
    )
}
