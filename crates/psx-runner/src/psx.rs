// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! `psx`: boot a BIOS and step it.
//!
//!   psx <bios.bin> [--steps N] [--exe file.exe] [--trace N] [--quiet]
//!
//! Prints whatever the BIOS puts on its TTY, then a one-line summary of where
//! the CPU ended up and what it touched. This is the harness that answers "did
//! the BIOS get anywhere?" before any subsystem exists to look at.

use std::process::ExitCode;

use psx_core::{exe::Exe, Psx};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: psx <bios.bin> [--steps N] [--exe file.exe] [--trace N] [--quiet]");
        return ExitCode::FAILURE;
    }

    let mut bios_path = None;
    let mut exe_path: Option<String> = None;
    let mut steps: u64 = 10_000_000;
    let mut trace: u64 = 0;
    let mut quiet = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--steps" => {
                i += 1;
                steps = parse_num(args.get(i)).unwrap_or(steps);
            }
            "--trace" => {
                i += 1;
                trace = parse_num(args.get(i)).unwrap_or(0);
            }
            "--exe" => {
                i += 1;
                exe_path = args.get(i).cloned();
            }
            "--quiet" => quiet = true,
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
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|img| {
            Exe::parse(&img).map_err(|e| e.to_string())
        }) {
            Ok(exe) => {
                println!(
                    "exe {path}: pc={:#010X} dest={:#010X} text={} bytes",
                    exe.initial_pc,
                    exe.dest,
                    exe.text.len()
                );
                psx.sideload_exe(exe);
            }
            Err(e) => {
                eprintln!("cannot load EXE {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    for n in 0..steps {
        if n < trace {
            println!(
                "{:>8} pc={:#010X} ra={:#010X} sp={:#010X} sr={:#010X}",
                n,
                psx.cpu.pc,
                psx.cpu.reg(31),
                psx.cpu.reg(29),
                psx.cpu.cop0.sr
            );
        }
        psx.step();
    }

    let tty = psx.take_tty();
    if !tty.is_empty() && !quiet {
        println!("--- BIOS TTY ---");
        print!("{tty}");
        if !tty.ends_with('\n') {
            println!();
        }
        println!("--- end TTY ---");
    }

    println!(
        "stopped: pc={:#010X} cycles={} exe_loaded={}",
        psx.cpu.pc, psx.cpu.cycles, psx.exe_loaded
    );
    println!(
        "bus: {} stub reads, {} stub writes, {} unmapped reads, {} unmapped writes",
        psx.bus.stub_reads, psx.bus.stub_writes, psx.bus.unmapped_reads, psx.bus.unmapped_writes
    );
    println!(
        "gte: {} unknown commands, flag={:#010X}",
        psx.cpu.gte.unknown_commands, psx.cpu.gte.flag
    );

    ExitCode::SUCCESS
}

fn parse_num(s: Option<&String>) -> Option<u64> {
    let s = s?;
    let s = s.replace('_', "");
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}
