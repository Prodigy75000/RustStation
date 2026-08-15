// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! `testrom`: run a PSX-EXE conformance binary and read its own verdict.
//!
//!   testrom <bios.bin> <test.exe> [--steps N] [--boot-steps N]
//!   testrom <bios.bin> --dir dumps/cpu [--steps N]
//!
//! The suites are PSX-EXEs that print `pass - name` / `fail - name` lines to
//! the BIOS TTY. So the harness boots the BIOS normally, sideloads the binary
//! when the shell hands over, runs it, and grades on what it printed.
//!
//! There are **three** verdicts, not two, and that is the important part. Some
//! tests in the suite carry no assertions at all: they print measurements and
//! say to compare them against the `psx.log` shipped beside them. Grading those
//! on "nothing looked like a failure" calls them PASS while every number is
//! zero, which is precisely how a core that emulates nothing gets a green run.
//! So a test with no `pass`/`fail` lines, or one that says it has no
//! assertions, is **UNGRADED**, and is never counted as passed.
//!
//! Where a `psx.log` sits beside the binary, its output is diffed against ours
//! and the result reported alongside. Only the reference's own line count is
//! compared: our TTY keeps going afterwards (currently with `VSync: timeout`
//! forever, since there is no GPU), and that tail is not the test's fault.
//!
//! Exit status is non-zero if anything FAILED. UNGRADED does not fail the run,
//! but it is printed loudly enough that it cannot be mistaken for a pass.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psx_core::{exe::Exe, Psx};

/// How long to let the BIOS run before giving up on ever reaching the shell
/// hand-over point. Generous: the BIOS spends a long time on memory setup.
const DEFAULT_BOOT_STEPS: u64 = 20_000_000;
/// How long the test binary itself gets once it is running.
const DEFAULT_RUN_STEPS: u64 = 200_000_000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: testrom <bios.bin> <test.exe|--dir DIR> [--steps N] [--boot-steps N]");
        return ExitCode::FAILURE;
    }

    let mut positional: Vec<String> = Vec::new();
    let mut dir: Option<String> = None;
    let mut run_steps = DEFAULT_RUN_STEPS;
    let mut boot_steps = DEFAULT_BOOT_STEPS;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = args.get(i).cloned();
            }
            "--steps" => {
                i += 1;
                run_steps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(run_steps);
            }
            "--boot-steps" => {
                i += 1;
                boot_steps = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(boot_steps);
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    let Some(bios_path) = positional.first().cloned() else {
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

    let targets: Vec<PathBuf> = match &dir {
        Some(d) => match collect_exes(Path::new(d)) {
            Ok(v) if v.is_empty() => {
                eprintln!("no *.exe found under {d}");
                return ExitCode::FAILURE;
            }
            Ok(v) => v,
            Err(e) => {
                eprintln!("cannot scan {d}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => match positional.get(1) {
            Some(p) => vec![PathBuf::from(p)],
            None => {
                eprintln!("no test EXE given");
                return ExitCode::FAILURE;
            }
        },
    };

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut ungraded = 0usize;

    for target in &targets {
        match run_one(&bios, target, boot_steps, run_steps) {
            Ok(report) => {
                println!("=== {} ===", target.display());
                print!("{}", report.tty);
                if !report.tty.ends_with('\n') {
                    println!();
                }
                println!(
                    "--- {}: {} ({}) [{} instructions, {} TTY bytes]{}",
                    target.display(),
                    report.verdict.label(),
                    report.detail,
                    report.cycles,
                    report.tty.len(),
                    match report.reference {
                        Some(true) => " reference: MATCHES psx.log",
                        Some(false) => " reference: DIFFERS from psx.log",
                        None => "",
                    }
                );
                match report.verdict {
                    Verdict::Pass => passed += 1,
                    Verdict::Fail => failed += 1,
                    Verdict::Ungraded => ungraded += 1,
                }
            }
            Err(e) => {
                println!("--- {}: ERROR: {e}", target.display());
                failed += 1;
            }
        }
    }

    if targets.len() > 1 {
        println!(
            "{} passed, {} failed, {} ungraded, of {}",
            passed,
            failed,
            ungraded,
            targets.len()
        );
    }

    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    /// The run produced no verdict of its own. Never counted as a pass.
    Ungraded,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Ungraded => "UNGRADED",
        }
    }
}

struct Report {
    tty: String,
    cycles: u64,
    verdict: Verdict,
    detail: String,
    /// Whether our output matched the `psx.log` beside the binary, when there
    /// is one to match against.
    reference: Option<bool>,
}

fn run_one(bios: &[u8], path: &Path, boot_steps: u64, run_steps: u64) -> Result<Report, String> {
    let image = std::fs::read(path).map_err(|e| e.to_string())?;
    let exe = Exe::parse(&image).map_err(|e| e.to_string())?;

    let mut psx = Psx::new(bios.to_vec()).map_err(|e| e.to_string())?;
    psx.sideload_exe(exe);

    // Phase 1: BIOS boot, until the shell hands over and the EXE goes in.
    let mut booted = false;
    for _ in 0..boot_steps {
        psx.step();
        if psx.exe_loaded {
            booted = true;
            break;
        }
    }
    if !booted {
        return Err(format!(
            "BIOS never reached the shell hand-over in {boot_steps} instructions \
             (pc={:#010X})",
            psx.cpu.pc
        ));
    }

    // Phase 2: the test binary. Stop early once it stops printing and stops
    // moving. A suite that has finished usually parks in a tight loop.
    let mut last_tty = 0usize;
    let mut idle = 0u64;
    for _ in 0..run_steps {
        psx.step();
        let n = psx.tty().len();
        if n != last_tty {
            last_tty = n;
            idle = 0;
        } else {
            idle += 1;
            if idle > 50_000_000 {
                break;
            }
        }
    }

    let tty = psx.take_tty();
    let (verdict, detail) = grade(&tty);
    let reference = compare_to_reference(&tty, path);

    Ok(Report {
        cycles: psx.cpu.cycles,
        tty,
        verdict,
        detail,
        reference,
    })
}

/// Grade on the suite's own verdict lines, and refuse to invent one when it did
/// not print any. A false FAIL is cheap to investigate; a false PASS is how a
/// core that emulates nothing gets called green.
fn grade(tty: &str) -> (Verdict, String) {
    if tty.is_empty() {
        return (
            Verdict::Fail,
            "printed nothing, so it probably never ran".to_string(),
        );
    }

    // Tests that say so up front are measurements, not assertions.
    if tty.to_ascii_lowercase().contains("no assertions") {
        return (
            Verdict::Ungraded,
            "the test states it has no assertions; compare with psx.log".to_string(),
        );
    }

    let mut passes = 0usize;
    let mut fails = 0usize;
    for line in tty.lines() {
        let l = line.trim_start().to_ascii_lowercase();
        if l.starts_with("pass") {
            passes += 1;
        } else if l.starts_with("fail") {
            fails += 1;
        }
    }

    match (passes, fails) {
        (_, f) if f > 0 => (Verdict::Fail, format!("{f} failed, {passes} passed")),
        (0, _) => (
            Verdict::Ungraded,
            "printed no pass/fail lines".to_string(),
        ),
        _ => (Verdict::Pass, format!("{passes} passed")),
    }
}

/// Diff our TTY against the `psx.log` shipped beside the binary.
///
/// Only the reference's own line count is compared. Our output runs on past the
/// end of the test, and the tail is not evidence of anything.
fn compare_to_reference(tty: &str, exe: &Path) -> Option<bool> {
    let expected = std::fs::read_to_string(exe.with_file_name("psx.log")).ok()?;
    let expected: Vec<&str> = expected.lines().map(|l| l.trim_end()).collect();
    let first = expected.iter().find(|l| !l.trim().is_empty())?;

    // The reference starts at the test's own banner; ours is preceded by the
    // BIOS kernel's. Align on that first line.
    let ours: Vec<&str> = tty.lines().map(|l| l.trim_end()).collect();
    let start = ours.iter().position(|l| l == first)?;
    let ours = &ours[start..];

    if ours.len() < expected.len() {
        return Some(false);
    }
    Some(ours[..expected.len()] == expected[..])
}

fn collect_exes(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_exes(&path)?);
        } else if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("psexe"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    // Directory order is filesystem-dependent; sort so a run is reproducible.
    out.sort();
    Ok(out)
}
