// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Just enough ISO 9660 for the kernel's "cdrom:" device: find a file by
//! path, and list a directory.
//!
//! From ECMA-119: the primary volume descriptor is sector 16, the root
//! directory record sits at byte 156 of it, and a directory is a run of
//! records (length, extent, size, flags, name) that never crosses a sector.

use crate::disc::{Disc, TrackMode, RAW_SECTOR};

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// As stored, version suffix and all ("SLUS_000.01;1").
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub dir: bool,
}

/// The 2048 user bytes of a data sector, from Mode 1 or Mode 2 Form 1.
pub fn user_data(disc: &mut Disc, lba: u32) -> Option<[u8; 2048]> {
    // Mode 2 carries an eight-byte subheader before the data. So does a
    // widened 2048-byte image of either mode, which [`Disc`] lays out as
    // Mode 2: only a raw Mode 1 sector has its data at 16.
    let t = disc.track_at(lba)?;
    let at = if t.mode == TrackMode::Mode1 && t.sector_size == RAW_SECTOR {
        16
    } else {
        24
    };
    let mut raw = [0u8; RAW_SECTOR];
    if !disc.read_sector(lba, &mut raw) {
        return None;
    }
    let mut out = [0u8; 2048];
    out.copy_from_slice(&raw[at..at + 2048]);
    Some(out)
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The root directory, from the primary volume descriptor.
pub fn root(disc: &mut Disc) -> Option<Entry> {
    let pvd = user_data(disc, 16)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    Some(Entry {
        name: String::new(),
        lba: le32(&pvd, 156 + 2),
        size: le32(&pvd, 156 + 10),
        dir: true,
    })
}

/// Every record in a directory, including the "." and ".." entries, which
/// come first and are named "\0" and "\x01".
pub fn list(disc: &mut Disc, dir: &Entry) -> Vec<Entry> {
    let mut out = Vec::new();
    let sectors = dir.size.div_ceil(2048).min(64);
    for s in 0..sectors {
        let Some(data) = user_data(disc, dir.lba + s) else {
            break;
        };
        let mut at = 0;
        while at + 33 < 2048 {
            let len = data[at] as usize;
            if len == 0 || at + len > 2048 {
                // The rest of the sector is padding; records resume in the next.
                break;
            }
            let name_len = data[at + 32] as usize;
            let name_end = (at + 33 + name_len).min(at + len);
            let name = String::from_utf8_lossy(&data[at + 33..name_end]).into_owned();
            out.push(Entry {
                name,
                lba: le32(&data, at + 2),
                size: le32(&data, at + 10),
                dir: data[at + 25] & 2 != 0,
            });
            at += len;
        }
    }
    out
}

/// Whether a stored name answers to `want`. Case does not matter, and a
/// name asked for without a ";version" matches any version.
pub fn name_matches(stored: &str, want: &str) -> bool {
    let want_has_version = want.contains(';');
    let stored = if want_has_version {
        stored
    } else {
        stored.split(';').next().unwrap_or(stored)
    };
    stored.eq_ignore_ascii_case(want)
}

/// Resolve a path of backslash-separated components, from the root. An
/// empty path is the root itself.
pub fn find(disc: &mut Disc, path: &str) -> Option<Entry> {
    let mut at = root(disc)?;
    for part in path.split(['\\', '/']).filter(|p| !p.is_empty()) {
        if !at.dir {
            return None;
        }
        at = list(disc, &at)
            .into_iter()
            .skip(2)
            .find(|e| name_matches(&e.name, part))?;
    }
    Some(at)
}

/// Read `len` bytes of a file from byte `offset`, whole sectors at a time.
pub fn read(disc: &mut Disc, lba: u32, offset: u32, len: u32) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(len as usize);
    let mut pos = offset;
    while (out.len() as u32) < len {
        let data = user_data(disc, lba + pos / 2048)?;
        let from = (pos % 2048) as usize;
        let take = ((len - out.len() as u32) as usize).min(2048 - from);
        out.extend_from_slice(&data[from..from + take]);
        pos += take as u32;
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a 2048-byte-sector image: a volume descriptor at 16, a root
    /// directory at 20 holding a file and a subdirectory, the subdirectory at
    /// 21 holding one file, and file data from 22.
    pub(crate) fn image(files: &[(&str, &[u8])], sub: &[(&str, &[u8])]) -> Disc {
        let mut img = vec![0u8; 2048 * 40];
        let sector = |img: &mut Vec<u8>, lba: usize| -> std::ops::Range<usize> {
            let _ = img;
            lba * 2048..lba * 2048 + 2048
        };
        fn record(name: &[u8], lba: u32, size: u32, dir: bool) -> Vec<u8> {
            let len = 33 + name.len() + (1 - name.len() % 2);
            let mut r = vec![0u8; len];
            r[0] = len as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[25] = if dir { 2 } else { 0 };
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        }
        let pvd = sector(&mut img, 16);
        img[pvd.start] = 1;
        img[pvd.start + 1..pvd.start + 6].copy_from_slice(b"CD001");
        let root = record(b"\0", 20, 2048, true);
        img[pvd.start + 156..pvd.start + 156 + root.len()].copy_from_slice(&root);

        let mut next = 22u32;
        let mut dir_sector = |img: &mut Vec<u8>,
                              lba: usize,
                              parent: u32,
                              entries: &[(&str, &[u8])],
                              extra: Option<(&str, u32)>| {
            let mut recs = vec![
                record(b"\0", lba as u32, 2048, true),
                record(b"\x01", parent, 2048, true),
            ];
            for (name, data) in entries {
                let at = next as usize * 2048;
                img[at..at + data.len()].copy_from_slice(data);
                recs.push(record(name.as_bytes(), next, data.len() as u32, false));
                next += (data.len() as u32).div_ceil(2048).max(1);
            }
            if let Some((name, at)) = extra {
                recs.push(record(name.as_bytes(), at, 2048, true));
            }
            let mut at = lba * 2048;
            for r in recs {
                img[at..at + r.len()].copy_from_slice(&r);
                at += r.len();
            }
        };
        dir_sector(&mut img, 20, 20, files, Some(("DATA", 21)));
        dir_sector(&mut img, 21, 20, sub, None);
        Disc::from_memory(
            "FILE \"x.iso\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
            vec![img],
        )
        .expect("test image")
    }

    #[test]
    fn a_file_is_found_by_path_whatever_its_case_and_version() {
        let mut disc = image(
            &[("SYSTEM.CNF;1", b"BOOT = cdrom:\\GAME.EXE;1\r\n")],
            &[("LEVEL.BIN;1", b"level")],
        );
        let cnf = find(&mut disc, "\\system.cnf;1").expect("found");
        assert_eq!(cnf.size, 26);
        assert!(!cnf.dir);
        assert_eq!(find(&mut disc, "SYSTEM.CNF").map(|e| e.lba), Some(cnf.lba));
        let level = find(&mut disc, "\\DATA\\LEVEL.BIN;1").expect("in the subdirectory");
        assert_eq!(read(&mut disc, level.lba, 0, level.size).unwrap(), b"level");
        assert_eq!(find(&mut disc, "\\LEVEL.BIN;1"), None);
        assert_eq!(find(&mut disc, "\\SYSTEM.CNF;2"), None);
    }

    #[test]
    fn a_read_can_start_and_end_inside_a_sector() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut disc = image(&[("BIG.DAT;1", &data)], &[]);
        let e = find(&mut disc, "BIG.DAT").unwrap();
        assert_eq!(
            read(&mut disc, e.lba, 2000, 100).unwrap(),
            &data[2000..2100]
        );
        assert_eq!(
            read(&mut disc, e.lba, 4000, 1000).unwrap(),
            &data[4000..5000]
        );
    }
}
