// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Disc images: the cue sheet, the tracks, and raw sector access.
//!
//! Deliberately a layer of its own, with the CD-ROM controller knowing nothing
//! about file formats. A disc is a **table of contents plus a function from LBA
//! to 2352 raw bytes**, and that is the whole interface. BIN/CUE is one
//! implementation of it; CHD would be another, and adding it should not touch
//! `cdrom.rs` at all.
//!
//! ## Why raw 2352-byte sectors, and not `.iso`
//!
//! A 2048-byte-per-sector image keeps only the user data, which throws away
//! four things this machine can observe:
//!
//! * the 4-byte **header** (minute, second, frame, mode), which is exactly what
//!   `GetlocL` returns,
//! * the 8-byte **subheader** (file, channel, submode, coding), which is what
//!   `Setfilter` selects on,
//! * **Mode 2 Form 2** sectors, where XA audio and MDEC video live, and
//! * **CD-DA** tracks, which have no user-data area at all.
//!
//! So the sector here is always the full 2352 bytes, and a 2048-byte image is
//! widened into one rather than being treated as a sector in its own right.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes in a raw CD sector.
pub const RAW_SECTOR: usize = 2352;

/// Sectors in the lead-in gap before LBA 0.
///
/// The disc's own addressing starts at `00:02:00`, so an absolute MSF position
/// is always 150 sectors ahead of the logical block address. Getting this
/// backwards puts every seek two seconds out, which reads as a disc that almost
/// works.
pub const LEAD_IN: u32 = 150;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackMode {
    Audio,
    Mode1,
    Mode2,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u8,
    pub mode: TrackMode,
    /// Bytes per sector in the backing file: 2048, 2336 or 2352.
    pub sector_size: usize,
    /// Which backing file this track's data lives in.
    file: usize,
    /// Sector offset of this track's `INDEX 01` within that file.
    file_sector: u32,
    /// Absolute LBA of `INDEX 01`, which is where the track's content starts.
    pub start_lba: u32,
    /// Absolute LBA of `INDEX 00`, the track's pregap. Equal to `start_lba`
    /// when the track has no pregap.
    pub pregap_lba: u32,
    /// Length in sectors, from `start_lba`.
    pub length: u32,
}

enum Backing {
    /// An image already in memory: what [`Disc::from_memory`] builds, and the
    /// shape a decompressed CHD hunk would arrive in.
    Memory(Vec<u8>),
    File(File),
}

impl Backing {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        match self {
            Backing::Memory(v) => {
                let Ok(start) = usize::try_from(offset) else {
                    return false;
                };
                let Some(end) = start.checked_add(buf.len()) else {
                    return false;
                };
                match v.get(start..end) {
                    Some(s) => {
                        buf.copy_from_slice(s);
                        true
                    }
                    None => false,
                }
            }
            Backing::File(f) => {
                f.seek(SeekFrom::Start(offset)).is_ok() && f.read_exact(buf).is_ok()
            }
        }
    }
}

pub struct Disc {
    files: Vec<Backing>,
    pub tracks: Vec<Track>,
    /// Total length in sectors, which is where the lead-out begins.
    pub length: u32,
    /// Where the image came from, for diagnostics.
    pub label: String,
}

/// Which market a disc was licensed for, from the text in its system area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    America,
    Europe,
    Japan,
}

impl Region {
    /// The four `SCEx` bytes `GetID` reports for this region.
    pub fn scex(self) -> [u8; 4] {
        match self {
            Region::America => *b"SCEA",
            Region::Europe => *b"SCEE",
            Region::Japan => *b"SCEI",
        }
    }
}

impl Disc {
    /// The region the disc's licence text names: sectors 4 to 15 of the
    /// system area carry "Licensed by Sony Computer Entertainment" followed by
    /// America, Europe or Inc. (Japan). `None` for a disc with no readable
    /// system area; a disc that has one but names neither Europe nor Japan is
    /// taken as American, which is the licence text's own default form.
    pub fn licence_region(&mut self) -> Option<Region> {
        let mut raw = [0u8; RAW_SECTOR];
        let mut text = String::new();
        for lba in 4..16 {
            if !self.read_sector(lba, &mut raw) {
                break;
            }
            text.extend(raw.iter().map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    ' '
                }
            }));
        }
        if text.is_empty() {
            return None;
        }
        Some(if text.contains("Europe") {
            Region::Europe
        } else if text.contains("Japan") {
            Region::Japan
        } else {
            Region::America
        })
    }

    /// Load from a `.cue`, or from a bare `.bin`/`.iso` with no cue beside it.
    pub fn open(path: &Path) -> Result<Disc, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "cue" => Self::open_cue(path),
            _ => Self::open_bare(path),
        }
    }

    /// A single-track image with no cue sheet.
    ///
    /// The sector size is inferred from the file length, because that is the
    /// only evidence a bare file carries. A 2048-byte-per-sector image is
    /// accepted and widened on read, but it cannot answer everything: see the
    /// module comment for what such an image does not contain.
    fn open_bare(path: &Path) -> Result<Disc, String> {
        let len = std::fs::metadata(path)
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
            .len();

        let (sector_size, mode) = if len % RAW_SECTOR as u64 == 0 {
            (RAW_SECTOR, TrackMode::Mode2)
        } else if len % 2048 == 0 {
            (2048, TrackMode::Mode1)
        } else {
            return Err(format!(
                "{} is {len} bytes, which is not a whole number of 2352- or \
                 2048-byte sectors, so its sector size cannot be inferred. \
                 Supply a .cue sheet.",
                path.display()
            ));
        };

        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let sectors = (len / sector_size as u64) as u32;

        Ok(Disc {
            files: vec![Backing::File(file)],
            tracks: vec![Track {
                number: 1,
                mode,
                sector_size,
                file: 0,
                file_sector: 0,
                start_lba: 0,
                pregap_lba: 0,
                length: sectors,
            }],
            length: sectors,
            label: path.display().to_string(),
        })
    }

    /// Build from a cue sheet and images already in memory.
    ///
    /// The path CHD will take once it exists, and what the tests in other
    /// modules use to drive the controller against a disc without shipping one.
    pub fn from_memory(cue: &str, images: Vec<Vec<u8>>) -> Result<Disc, String> {
        let mut it = images.into_iter();
        let mut disc = Self::parse_cue(cue, |name| {
            it.next()
                .map(Backing::Memory)
                .ok_or_else(|| format!("cue names {name:?} but no image was supplied for it"))
        })?;
        disc.label = "<memory>".to_string();
        Ok(disc)
    }

    fn open_cue(path: &Path) -> Result<Disc, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut disc = Self::parse_cue(&text, |name| {
            let p = resolve(dir, name);
            File::open(&p)
                .map(Backing::File)
                .map_err(|e| format!("cannot open {}: {e}", p.display()))
        })?;
        disc.label = path.display().to_string();
        Ok(disc)
    }

    /// Parse a cue sheet, opening each `FILE` through `open_file`.
    ///
    /// Split from the filesystem so the parser can be tested against a sheet
    /// and an in-memory image, which is the only way to check the track layout
    /// without shipping a disc.
    fn parse_cue<F>(text: &str, mut open_file: F) -> Result<Disc, String>
    where
        F: FnMut(&str) -> Result<Backing, String>,
    {
        let mut files: Vec<Backing> = Vec::new();
        let mut file_lengths: Vec<u64> = Vec::new();
        let mut tracks: Vec<Track> = Vec::new();

        // The LBA that file offset 0 of the current file corresponds to. With
        // one FILE this is zero and index times are absolute; with several,
        // each file restarts its own numbering and this accumulates.
        let mut file_base_lba: u32 = 0;
        let mut current_file: Option<usize> = None;
        let mut pending: Option<Track> = None;
        let mut pregap: u32 = 0;

        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with("REM") {
                continue;
            }
            let err = |m: String| format!("{m} (cue line {})", n + 1);
            let mut words = split_cue(line);
            let Some(key) = words.next() else { continue };

            match key.to_ascii_uppercase().as_str() {
                "FILE" => {
                    let name = words
                        .next()
                        .ok_or_else(|| err("FILE with no name".into()))?;
                    // Everything belonging to the previous file is now known,
                    // so its length fixes where the next one starts.
                    if let Some(t) = pending.take() {
                        tracks.push(t);
                    }
                    if let Some(i) = current_file {
                        file_base_lba += sectors_in(&file_lengths, &tracks, i);
                    }
                    let backing = open_file(&name)?;
                    file_lengths.push(match &backing {
                        Backing::Memory(v) => v.len() as u64,
                        Backing::File(f) => f.metadata().map(|m| m.len()).unwrap_or(0),
                    });
                    files.push(backing);
                    current_file = Some(files.len() - 1);
                }
                "TRACK" => {
                    let number = words
                        .next()
                        .and_then(|w| w.parse::<u8>().ok())
                        .ok_or_else(|| err("TRACK with no number".into()))?;
                    let kind = words
                        .next()
                        .ok_or_else(|| err("TRACK with no mode".into()))?
                        .to_ascii_uppercase();
                    let (mode, sector_size) = match kind.as_str() {
                        "AUDIO" => (TrackMode::Audio, RAW_SECTOR),
                        "MODE1/2048" => (TrackMode::Mode1, 2048),
                        "MODE1/2352" => (TrackMode::Mode1, RAW_SECTOR),
                        "MODE2/2336" => (TrackMode::Mode2, 2336),
                        "MODE2/2352" => (TrackMode::Mode2, RAW_SECTOR),
                        other => {
                            return Err(err(format!(
                                "track mode {other:?} is not supported. Refusing \
                                 rather than guessing: a wrong sector size reads \
                                 as a corrupt disc"
                            )))
                        }
                    };
                    let file = current_file
                        .ok_or_else(|| err("TRACK before any FILE".into()))?;
                    if let Some(t) = pending.take() {
                        tracks.push(t);
                    }
                    pending = Some(Track {
                        number,
                        mode,
                        sector_size,
                        file,
                        file_sector: 0,
                        start_lba: 0,
                        pregap_lba: 0,
                        length: 0,
                    });
                    pregap = 0;
                }
                "PREGAP" => {
                    // A gap that is *not* in the file: it exists on the disc but
                    // has no bytes behind it, so it shifts every later LBA
                    // without consuming any data.
                    let v = words
                        .next()
                        .ok_or_else(|| err("PREGAP with no time".into()))?;
                    pregap += parse_msf(&v).ok_or_else(|| err(format!("bad time {v:?}")))?;
                }
                "INDEX" => {
                    let idx = words
                        .next()
                        .and_then(|w| w.parse::<u8>().ok())
                        .ok_or_else(|| err("INDEX with no number".into()))?;
                    let v = words
                        .next()
                        .ok_or_else(|| err("INDEX with no time".into()))?;
                    let at = parse_msf(&v).ok_or_else(|| err(format!("bad time {v:?}")))?;
                    let t = pending
                        .as_mut()
                        .ok_or_else(|| err("INDEX outside a TRACK".into()))?;
                    match idx {
                        0 => t.pregap_lba = file_base_lba + pregap + at,
                        1 => {
                            t.file_sector = at;
                            t.start_lba = file_base_lba + pregap + at;
                            if t.pregap_lba == 0 {
                                t.pregap_lba = t.start_lba;
                            }
                        }
                        _ => {} // higher indices exist but nothing here uses them
                    }
                }
                _ => {} // CATALOG, PERFORMER, TITLE, FLAGS and friends
            }
        }

        if let Some(t) = pending.take() {
            tracks.push(t);
        }
        if tracks.is_empty() {
            return Err("cue sheet declares no tracks".into());
        }

        // A track runs until the next one starts, or to the end of its file.
        let starts: Vec<u32> = tracks.iter().map(|t| t.pregap_lba).collect();
        for (i, t) in tracks.iter_mut().enumerate() {
            let end = starts
                .get(i + 1)
                .copied()
                .unwrap_or_else(|| track_file_end(t, &file_lengths));
            t.length = end.saturating_sub(t.start_lba);
        }
        let length = tracks
            .last()
            .map(|t| t.start_lba + t.length)
            .unwrap_or_default();

        Ok(Disc {
            files,
            tracks,
            length,
            label: String::new(),
        })
    }

    /// The track containing `lba`, if any.
    pub fn track_at(&self, lba: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .rev()
            .find(|t| lba >= t.pregap_lba && lba < t.start_lba + t.length)
    }

    /// Read one sector as the full 2352 raw bytes.
    ///
    /// Images that store less than that are widened here rather than at the
    /// call site, so the controller only ever sees whole sectors. For a
    /// 2048-byte image the sync pattern and header are **synthesised**, which
    /// is honest for `GetlocL` and wrong for anything that checks the ECC.
    pub fn read_sector(&mut self, lba: u32, out: &mut [u8; RAW_SECTOR]) -> bool {
        let Some(track) = self.track_at(lba).cloned() else {
            return false;
        };
        // The pregap has no bytes behind it: it reads as a blank sector with a
        // valid header, which is what a drive returns there.
        if lba < track.start_lba {
            out.fill(0);
            write_sync_and_header(out, lba, track.mode);
            return true;
        }

        let within = lba - track.start_lba;
        let offset = (track.file_sector as u64 + within as u64) * track.sector_size as u64;

        match track.sector_size {
            RAW_SECTOR => self.files[track.file].read_at(offset, out),
            2336 => {
                // Mode 2 without sync or header: the subheader onwards.
                out.fill(0);
                write_sync_and_header(out, lba, track.mode);
                self.files[track.file].read_at(offset, &mut out[16..16 + 2336])
            }
            2048 => {
                out.fill(0);
                write_sync_and_header(out, lba, track.mode);
                self.files[track.file].read_at(offset, &mut out[24..24 + 2048])
            }
            _ => false,
        }
    }
}

/// Where a track's data ends within its own file, as an absolute LBA.
fn track_file_end(t: &Track, lengths: &[u64]) -> u32 {
    let sectors = lengths
        .get(t.file)
        .map(|l| (l / t.sector_size as u64) as u32)
        .unwrap_or(0);
    t.start_lba + sectors.saturating_sub(t.file_sector)
}

fn sectors_in(lengths: &[u64], tracks: &[Track], file: usize) -> u32 {
    let size = tracks
        .iter()
        .rev()
        .find(|t| t.file == file)
        .map(|t| t.sector_size)
        .unwrap_or(RAW_SECTOR);
    lengths.get(file).map(|l| (l / size as u64) as u32).unwrap_or(0)
}

/// `FILE "name with spaces.bin" BINARY` needs quote-aware splitting.
fn split_cue(line: &str) -> impl Iterator<Item = String> + '_ {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
}

/// `MM:SS:FF` to a sector count. 75 frames per second, 60 seconds per minute.
fn parse_msf(s: &str) -> Option<u32> {
    let mut it = s.split(':');
    let m: u32 = it.next()?.parse().ok()?;
    let s2: u32 = it.next()?.parse().ok()?;
    let f: u32 = it.next()?.parse().ok()?;
    if s2 >= 60 || f >= 75 {
        return None;
    }
    Some((m * 60 + s2) * 75 + f)
}

/// An LBA as the absolute minute, second and frame the drive reports, in BCD.
pub fn lba_to_msf_bcd(lba: u32) -> [u8; 3] {
    frames_to_msf_bcd(lba + LEAD_IN)
}

/// A plain count of frames as BCD minute, second and frame, with no lead-in
/// added: what a position *within* a track is reported as.
pub fn frames_to_msf_bcd(frames: u32) -> [u8; 3] {
    let m = frames / (60 * 75);
    let s = (frames / 75) % 60;
    let f = frames % 75;
    [to_bcd(m as u8), to_bcd(s as u8), to_bcd(f as u8)]
}

/// A BCD minute, second and frame back to an LBA.
pub fn msf_bcd_to_lba(msf: [u8; 3]) -> u32 {
    let m = from_bcd(msf[0]) as u32;
    let s = from_bcd(msf[1]) as u32;
    let f = from_bcd(msf[2]) as u32;
    ((m * 60 + s) * 75 + f).saturating_sub(LEAD_IN)
}

pub fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

pub fn from_bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

/// The 12-byte sync pattern every raw data sector begins with.
const SYNC: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

fn write_sync_and_header(out: &mut [u8; RAW_SECTOR], lba: u32, mode: TrackMode) {
    out[..12].copy_from_slice(&SYNC);
    let msf = lba_to_msf_bcd(lba);
    out[12..15].copy_from_slice(&msf);
    out[15] = match mode {
        TrackMode::Mode1 => 1,
        _ => 2,
    };
}

fn resolve(dir: &Path, name: &str) -> PathBuf {
    let p = Path::new(name);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cue sheet backed by images made of numbered sectors, so a read can be
    /// checked against the sector it should have come from.
    fn build(cue: &str, sizes: &[(usize, usize)]) -> Disc {
        let mut n = 0usize;
        Disc::parse_cue(cue, |_| {
            let (sector_size, sectors) = sizes[n];
            n += 1;
            let mut v = vec![0u8; sector_size * sectors];
            for s in 0..sectors {
                v[s * sector_size] = s as u8;
                v[s * sector_size + 1] = (n * 16) as u8;
            }
            Ok(Backing::Memory(v))
        })
        .expect("cue should parse")
    }

    #[test]
    fn msf_and_lba_round_trip_through_the_lead_in() {
        // LBA 0 is 00:02:00, not 00:00:00: the disc's own addressing starts two
        // seconds in, and losing that puts every seek 150 sectors out.
        assert_eq!(lba_to_msf_bcd(0), [0x00, 0x02, 0x00]);
        assert_eq!(msf_bcd_to_lba([0x00, 0x02, 0x00]), 0);

        for lba in [0u32, 1, 74, 75, 4499, 4500, 123_456] {
            assert_eq!(msf_bcd_to_lba(lba_to_msf_bcd(lba)), lba, "lba {lba}");
        }
    }

    #[test]
    fn bcd_is_bcd_and_not_hex() {
        assert_eq!(to_bcd(39), 0x39);
        assert_eq!(from_bcd(0x39), 39);
        // The value that catches a plain integer being passed through.
        assert_eq!(lba_to_msf_bcd(74 * 75 + 74 - 150)[0], 0x01);
    }

    #[test]
    fn a_single_track_cue_covers_the_whole_file() {
        let d = build(
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
            &[(RAW_SECTOR, 10)],
        );
        assert_eq!(d.tracks.len(), 1);
        assert_eq!(d.tracks[0].mode, TrackMode::Mode2);
        assert_eq!(d.tracks[0].start_lba, 0);
        assert_eq!(d.tracks[0].length, 10);
        assert_eq!(d.length, 10);
    }

    #[test]
    fn a_multi_track_cue_gets_its_lengths_from_the_next_start() {
        let d = build(
            "FILE \"game.bin\" BINARY\n\
             TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n\
             TRACK 02 AUDIO\n INDEX 00 00:00:50\n INDEX 01 00:01:00\n",
            &[(RAW_SECTOR, 200)],
        );
        assert_eq!(d.tracks.len(), 2);
        // Track 1 ends where track 2's pregap begins, not where its audio does.
        assert_eq!(d.tracks[0].length, 50);
        assert_eq!(d.tracks[1].pregap_lba, 50);
        assert_eq!(d.tracks[1].start_lba, 75);
        assert_eq!(d.tracks[1].length, 125);
    }

    #[test]
    fn a_pregap_shifts_later_tracks_without_consuming_data() {
        let d = build(
            "FILE \"game.bin\" BINARY\n\
             TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n\
             TRACK 02 AUDIO\n PREGAP 00:01:00\n INDEX 01 00:01:00\n",
            &[(RAW_SECTOR, 300)],
        );
        // INDEX 01 is at file sector 75, and the 75-sector pregap is not in the
        // file, so the track sits at LBA 150 while its data starts at file
        // sector 75.
        assert_eq!(d.tracks[1].start_lba, 150);
        assert_eq!(d.tracks[1].file_sector, 75);
    }

    #[test]
    fn track_at_finds_the_track_containing_a_sector() {
        let d = build(
            "FILE \"game.bin\" BINARY\n\
             TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n\
             TRACK 02 AUDIO\n INDEX 01 00:01:00\n",
            &[(RAW_SECTOR, 200)],
        );
        assert_eq!(d.track_at(0).unwrap().number, 1);
        assert_eq!(d.track_at(74).unwrap().number, 1);
        assert_eq!(d.track_at(75).unwrap().number, 2);
        assert!(d.track_at(10_000).is_none(), "past the lead-out");
    }

    #[test]
    fn a_raw_image_is_read_through_unchanged() {
        let mut d = build(
            "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n",
            &[(RAW_SECTOR, 10)],
        );
        let mut buf = [0u8; RAW_SECTOR];
        assert!(d.read_sector(7, &mut buf));
        assert_eq!(buf[0], 7, "the sector's own marker, straight from the file");
    }

    #[test]
    fn a_2048_byte_image_is_widened_and_given_a_header() {
        let mut d = build(
            "FILE \"game.bin\" BINARY\n TRACK 01 MODE1/2048\n INDEX 01 00:00:00\n",
            &[(2048, 10)],
        );
        let mut buf = [0u8; RAW_SECTOR];
        assert!(d.read_sector(3, &mut buf));

        assert_eq!(&buf[..12], &SYNC, "sync synthesised");
        assert_eq!(&buf[12..15], &lba_to_msf_bcd(3), "and the header with it");
        assert_eq!(buf[15], 1, "mode 1");
        assert_eq!(buf[24], 3, "user data lands at the Mode 2 Form 1 offset");
    }

    #[test]
    fn reading_past_the_end_fails_rather_than_returning_zeroes() {
        let mut d = build(
            "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n",
            &[(RAW_SECTOR, 10)],
        );
        let mut buf = [0u8; RAW_SECTOR];
        assert!(!d.read_sector(10, &mut buf), "one past the last sector");
        assert!(!d.read_sector(1_000_000, &mut buf));
    }

    #[test]
    fn an_unsupported_track_mode_is_refused_not_guessed() {
        let r = Disc::parse_cue(
            "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2448\n INDEX 01 00:00:00\n",
            |_| Ok(Backing::Memory(vec![0; 4096])),
        );
        let e = match r {
            Err(e) => e,
            Ok(_) => panic!("an unknown sector size must not be guessed at"),
        };
        assert!(e.contains("not supported"), "{e}");
    }

    #[test]
    fn quoted_filenames_with_spaces_survive_splitting() {
        let words: Vec<String> =
            split_cue("FILE \"Some Game (USA).bin\" BINARY").collect();
        assert_eq!(words[1], "Some Game (USA).bin");
        assert_eq!(words[2], "BINARY");
    }

    #[test]
    fn a_bad_time_is_an_error_not_a_zero() {
        assert_eq!(parse_msf("00:02:00"), Some(150));
        assert_eq!(parse_msf("00:00:75"), None, "75 frames is the next second");
        assert_eq!(parse_msf("00:60:00"), None);
        assert_eq!(parse_msf("garbage"), None);
    }
}
