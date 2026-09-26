// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! `discdiff A B`: whether two disc images are the same disc.
//!
//! Prints each image's track table and compares every sector, as the drive
//! would read it (2352 raw bytes). This is how a CHD reader is checked: build
//! the CHD from a BIN/CUE with chdman, then the two must agree on every track
//! and every byte.

use std::path::Path;
use std::process::ExitCode;

use psx_core::disc::{Disc, RAW_SECTOR};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: discdiff <image> <image>");
        return ExitCode::FAILURE;
    }
    let mut discs = Vec::new();
    for p in &args {
        match psx_chd::open_any(Path::new(p)) {
            Ok(d) => discs.push(d),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let table = |d: &Disc| -> Vec<String> {
        d.tracks
            .iter()
            .map(|t| {
                format!(
                    "track {:2} {:?} pregap {} start {} length {}",
                    t.number, t.mode, t.pregap_lba, t.start_lba, t.length
                )
            })
            .collect()
    };
    let (ta, tb) = (table(&discs[0]), table(&discs[1]));
    for (i, line) in ta.iter().enumerate() {
        let same = tb.get(i) == Some(line);
        println!("{} {line}", if same { " " } else { "!" });
    }
    for line in tb.iter().skip(ta.len()) {
        println!("! (B only) {line}");
    }
    println!("length {} / {}", discs[0].length, discs[1].length);

    let end = discs[0].length.max(discs[1].length);
    let (mut a, mut b) = ([0u8; RAW_SECTOR], [0u8; RAW_SECTOR]);
    let mut differ = 0u64;
    for lba in 0..end {
        let ra = discs[0].read_sector(lba, &mut a);
        let rb = discs[1].read_sector(lba, &mut b);
        if ra != rb || (ra && a != b) {
            if differ < 5 {
                let first = a.iter().zip(&b).position(|(x, y)| x != y);
                println!("sector {lba} differs (read {ra}/{rb}, first byte {first:?})");
            }
            differ += 1;
        }
    }
    println!("{end} sectors compared, {differ} differ");
    if differ == 0 && ta == tb {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
