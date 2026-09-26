// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A CHD read back against the BIN/CUE it was made from, sector for sector.
//!
//! No game can be committed, so the disc is synthetic: [`write_bin_cue`]
//! builds it, and `tests/data/synthetic.chd` is what `chdman createcd` (MAME
//! 0.289, default codecs) made of exactly those files. The test builds them
//! again and requires the two images to be the same disc. The disc has what
//! the layout depends on: a data track; an audio track whose two-second pregap
//! is stored in its file; and an audio track whose length is not a multiple of
//! four, with audible, asymmetric samples, so byte order shows.
//!
//! To rebuild the fixture after changing the disc:
//!
//! ```text
//! RSTA_WRITE_SYNTHETIC=$PWD/out/synthetic cargo test -p psx-chd --test synthetic
//! chdman createcd -i out/synthetic/synthetic.cue -o crates/psx-chd/tests/data/synthetic.chd
//! ```

use std::path::{Path, PathBuf};

use psx_core::disc::{lba_to_msf_bcd, Disc, RAW_SECTOR};

const DATA_SECTORS: u32 = 60;
const PREGAP: u32 = 150;
const AUDIO1_SECTORS: u32 = 40;
const AUDIO2_SECTORS: u32 = 23;

fn data_sector(lba: u32) -> Vec<u8> {
    let mut s = vec![0u8; RAW_SECTOR];
    s[1..11].fill(0xFF);
    let msf = lba_to_msf_bcd(lba + 150);
    s[12..15].copy_from_slice(&msf);
    s[15] = 2;
    s[16..24].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
    for (i, b) in s[24..24 + 2048].iter_mut().enumerate() {
        *b = (lba as usize * 31 + i * 7 + (i >> 5)) as u8;
    }
    s
}

/// Stereo 16-bit samples of a ramp, left and right different, each sample's
/// two bytes different, so a swapped or shifted read cannot match.
fn audio_sector(track: u32, n: u32) -> Vec<u8> {
    let mut s = Vec::with_capacity(RAW_SECTOR);
    for i in 0..RAW_SECTOR / 4 {
        let t = (n * 588 + i as u32) as i32;
        let left = ((t * (37 + track as i32)) % 20000 - 10000) as i16;
        let right = ((t * 53) % 16000 - 8000 + 3) as i16;
        s.extend_from_slice(&left.to_le_bytes());
        s.extend_from_slice(&right.to_le_bytes());
    }
    s
}

fn write_bin_cue(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let data: Vec<u8> = (0..DATA_SECTORS).flat_map(data_sector).collect();
    std::fs::write(dir.join("synthetic (Track 1).bin"), data).unwrap();
    // The pregap stored in the file, as silence, then the track.
    let mut a1 = vec![0u8; PREGAP as usize * RAW_SECTOR];
    a1.extend((0..AUDIO1_SECTORS).flat_map(|n| audio_sector(2, n)));
    std::fs::write(dir.join("synthetic (Track 2).bin"), a1).unwrap();
    let a2: Vec<u8> = (0..AUDIO2_SECTORS)
        .flat_map(|n| audio_sector(3, n))
        .collect();
    std::fs::write(dir.join("synthetic (Track 3).bin"), a2).unwrap();
    let cue = "FILE \"synthetic (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
               FILE \"synthetic (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n\
               FILE \"synthetic (Track 3).bin\" BINARY\n  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n";
    let path = dir.join("synthetic.cue");
    std::fs::write(&path, cue).unwrap();
    path
}

fn tracks(d: &Disc) -> Vec<(u8, u32, u32, u32)> {
    d.tracks
        .iter()
        .map(|t| (t.number, t.pregap_lba, t.start_lba, t.length))
        .collect()
}

#[test]
fn the_chd_is_the_disc_it_was_made_from() {
    let dir = match std::env::var_os("RSTA_WRITE_SYNTHETIC") {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join(format!("rsta-synthetic-{}", std::process::id())),
    };
    let cue = write_bin_cue(&dir);
    let mut bin = Disc::open(&cue).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/synthetic.chd");
    let mut chd = psx_chd::open(&fixture).unwrap();

    assert_eq!(tracks(&chd), tracks(&bin));
    assert_eq!(
        tracks(&chd),
        [
            (1, 0, 0, DATA_SECTORS),
            (2, DATA_SECTORS, DATA_SECTORS + PREGAP, AUDIO1_SECTORS),
            (
                3,
                DATA_SECTORS + PREGAP + AUDIO1_SECTORS,
                DATA_SECTORS + PREGAP + AUDIO1_SECTORS,
                AUDIO2_SECTORS
            ),
        ]
    );
    assert_eq!(chd.length, bin.length);

    let (mut a, mut b) = ([0u8; RAW_SECTOR], [0u8; RAW_SECTOR]);
    for lba in 0..bin.length {
        assert!(bin.read_sector(lba, &mut a), "BIN/CUE sector {lba}");
        assert!(chd.read_sector(lba, &mut b), "CHD sector {lba}");
        assert!(a == b, "sector {lba} differs");
    }
    // The audio really is audio, so the comparison above covered byte order.
    assert!(bin.read_sector(DATA_SECTORS + PREGAP + 3, &mut a));
    assert!(a.iter().filter(|&&x| x != 0).count() > RAW_SECTOR / 2);

    if std::env::var_os("RSTA_WRITE_SYNTHETIC").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_file_that_is_not_a_chd_is_refused() {
    let dir = std::env::temp_dir().join(format!("rsta-notchd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake.chd");
    std::fs::write(&path, b"not a CHD at all, only some bytes").unwrap();
    assert!(psx_chd::open(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
