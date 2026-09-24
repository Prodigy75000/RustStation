// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! PSX-EXE parsing and sideload.
//!
//! The CPU conformance suites (and every homebrew test binary) ship as
//! PSX-EXEs, not as discs. Sideloading one means letting the BIOS boot
//! normally until it reaches the point where it would hand control to the
//! disc's executable, then substituting ours: the BIOS has by then set up the
//! kernel, the A/B/C function tables and the TTY, which is exactly the
//! environment a test binary expects.
//!
//! The header is 2048 bytes; only the first 0x40 carry anything.

/// Every PSX-EXE starts with this, followed by eight bytes of zero.
pub const MAGIC: &[u8; 8] = b"PS-X EXE";
/// The header is padded out to one CD sector.
pub const HEADER_SIZE: usize = 2048;

/// The address the BIOS shell jumps to when it launches the disc executable.
/// Swapping in a sideloaded EXE at this exact PC is the whole trick.
pub const SHELL_HOOK: u32 = 0x8003_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExeError {
    TooShort(usize),
    BadMagic,
    /// The header's `size` field disagrees with the file it came in.
    SizeMismatch {
        declared: u32,
        available: usize,
    },
    /// The load destination is not in the 2 MB of RAM, or wraps past the end.
    BadDestination {
        dest: u32,
        size: u32,
    },
}

impl core::fmt::Display for ExeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ExeError::TooShort(n) => write!(f, "PSX-EXE is {n} bytes, shorter than its header"),
            ExeError::BadMagic => write!(f, "not a PSX-EXE (missing \"PS-X EXE\" magic)"),
            ExeError::SizeMismatch {
                declared,
                available,
            } => write!(
                f,
                "PSX-EXE header declares {declared} bytes of text, file carries {available}"
            ),
            ExeError::BadDestination { dest, size } => write!(
                f,
                "PSX-EXE loads {size} bytes at {dest:#010X}, which is not inside main RAM"
            ),
        }
    }
}

/// A parsed PSX-EXE, ready to be written into RAM.
#[derive(Clone)]
pub struct Exe {
    pub initial_pc: u32,
    pub initial_gp: u32,
    /// Where `text` goes in RAM (a virtual address, usually in KSEG0).
    pub dest: u32,
    pub text: Vec<u8>,
    /// A region the loader zeroes before jumping. Ignored when `size` is 0.
    pub memfill_start: u32,
    pub memfill_size: u32,
    /// `$sp`/`$fp` are set to `sp_base + sp_offset`, unless base is 0, in which
    /// case the BIOS leaves the stack where the kernel put it.
    pub sp_base: u32,
    pub sp_offset: u32,
}

fn le32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

impl Exe {
    pub fn parse(image: &[u8]) -> Result<Exe, ExeError> {
        if image.len() < HEADER_SIZE {
            return Err(ExeError::TooShort(image.len()));
        }
        if &image[0..8] != MAGIC {
            return Err(ExeError::BadMagic);
        }

        let initial_pc = le32(image, 0x10);
        let initial_gp = le32(image, 0x14);
        let dest = le32(image, 0x18);
        let size = le32(image, 0x1C);
        let memfill_start = le32(image, 0x30);
        let memfill_size = le32(image, 0x34);
        let sp_base = le32(image, 0x38);
        let sp_offset = le32(image, 0x3C);

        let available = image.len() - HEADER_SIZE;
        // Some dumps are padded past their declared size; that is fine. Coming
        // up *short* is not, because it would load whatever follows in the file.
        if (size as usize) > available {
            return Err(ExeError::SizeMismatch {
                declared: size,
                available,
            });
        }

        let phys = crate::bus::mask_region(dest);
        let end = phys as u64 + size as u64;
        if end > crate::bus::RAM_SIZE as u64 {
            return Err(ExeError::BadDestination { dest, size });
        }

        Ok(Exe {
            initial_pc,
            initial_gp,
            dest,
            text: image[HEADER_SIZE..HEADER_SIZE + size as usize].to_vec(),
            memfill_start,
            memfill_size,
            sp_base,
            sp_offset,
        })
    }
}
