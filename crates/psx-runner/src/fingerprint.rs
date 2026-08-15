// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! `fingerprint`: the netplay byte-parity proof.
//!
//!   fingerprint <bios.bin> [--steps N] [--exe file.exe]
//!
//! Runs a fixed number of instructions and prints the state-identity token plus
//! a checksum over the serialized state. Run it on the Windows build and on the
//! Android `.so`'s host equivalent: the numbers must match exactly. If they do
//! not, cross-platform netplay is unsafe and the serializer has picked up
//! something target-dependent.

use std::process::ExitCode;

use psx_core::{exe::Exe, save, Psx, CORE_ID};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: fingerprint <bios.bin> [--steps N] [--exe file.exe]");
        return ExitCode::FAILURE;
    }

    let mut bios_path = None;
    let mut exe_path: Option<String> = None;
    let mut steps: u64 = 1_000_000;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--steps" => {
                i += 1;
                steps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(steps);
            }
            "--exe" => {
                i += 1;
                exe_path = args.get(i).cloned();
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

    // The BIOS is not in the state, but it is in the *inputs*, so two hosts must
    // be running the same image for the comparison to mean anything, so its
    // checksum is printed alongside.
    let bios_hash = fnv1a64(&bios);

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

    psx.run(steps);

    let state = psx.save_state();

    println!("core_id        {CORE_ID}");
    println!("format_version {}", save::FORMAT_VERSION);
    println!("state_size     {}", Psx::state_size());
    println!("bios_fnv1a64   {bios_hash:#018x}");
    println!("steps          {steps}");
    println!("pc             {:#010X}", psx.cpu.pc);
    println!("cycles         {}", psx.cpu.cycles);
    println!("state_len      {}", state.len());
    println!("state_fnv1a64  {:#018x}", fnv1a64(&state));

    ExitCode::SUCCESS
}

/// FNV-1a-64. Integer-only and dependency-free, so the checksum has no
/// target-dependent behaviour of its own to muddy the comparison.
fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}
