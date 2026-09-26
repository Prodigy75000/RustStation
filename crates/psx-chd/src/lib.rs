// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! CHD disc images, as a [`psx_core::disc::Disc`].
//!
//! A CD CHD is a list of tracks, in its metadata, and the disc's sectors in
//! order, compressed a hunk at a time. The decompression is the [`chd`]
//! crate's, used as a library: this file reads the track list, lays the
//! tracks out on the disc the way a cue sheet would, and decompresses a hunk
//! when a sector in it is read.
//!
//! Three details of the stored layout were established by building CHDs from
//! BIN/CUE images with `chdman` and comparing every sector (see the tests):
//!
//! * each stored sector is 2352 bytes of data followed by 96 of subcode;
//! * each track's stored sectors are padded to a multiple of four;
//! * audio samples are stored big-endian, so they are swapped back here.
//!
//! A pregap is laid out on the disc either way, and reads blank, as it does
//! from a cue sheet. When the metadata marks it as stored (its type starts
//! with `V`), its sectors are skipped over.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use chd::Chd;
use psx_core::disc::{Disc, SectorSource, Track, TrackMode, RAW_SECTOR};

/// Stored sectors per track are padded to a multiple of this.
const TRACK_PADDING: u32 = 4;

/// Open a disc image of any kind this crate or the core reads: a `.chd`
/// here, anything else through [`Disc::open`].
pub fn open_any(path: &Path) -> Result<Disc, String> {
    let is_chd = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("chd"));
    if is_chd {
        open(path)
    } else {
        Disc::open(path)
    }
}

/// Open a CD CHD.
pub fn open(path: &Path) -> Result<Disc, String> {
    let label = path.display().to_string();
    let file = File::open(path).map_err(|e| format!("cannot open {label}: {e}"))?;
    let mut chd =
        Chd::open(BufReader::new(file), None).map_err(|e| format!("{label}: not a CHD: {e}"))?;

    let refs: Vec<_> = chd.metadata_refs().collect();
    let mut entries = Vec::new();
    for r in refs {
        let meta = r
            .read(chd.inner())
            .map_err(|e| format!("{label}: metadata: {e}"))?;
        if meta.metatag == tag(b"CHT2") || meta.metatag == tag(b"CHTR") {
            let text = String::from_utf8_lossy(&meta.value);
            entries.push(
                parse_track(text.trim_end_matches('\0'))
                    .ok_or_else(|| format!("{label}: cannot read track metadata {text:?}"))?,
            );
        }
    }
    if entries.is_empty() {
        return Err(format!("{label}: no CD track metadata, so not a CD image"));
    }
    entries.sort_by_key(|t| t.number);

    let unit = chd.header().unit_bytes() as usize;
    let hunk = chd.header().hunk_size() as usize;
    if unit < RAW_SECTOR || !hunk.is_multiple_of(unit) {
        return Err(format!(
            "{label}: {unit}-byte sectors in {hunk}-byte hunks is not a CD layout"
        ));
    }

    let (tracks, audio) = lay_out(&entries)?;
    let source = ChdSource {
        buffer: chd.get_hunksized_buffer(),
        chd,
        unit,
        per_hunk: (hunk / unit) as u64,
        cached: None,
        compressed: Vec::new(),
        audio,
    };
    Disc::from_source(tracks, Box::new(source), label)
}

fn tag(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

/// One track as the metadata describes it.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    number: u8,
    kind: String,
    frames: u32,
    pregap: u32,
    pregap_stored: bool,
    postgap: u32,
}

/// `TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:268934 PREGAP:0 PGTYPE:MODE1
/// PGSUB:NONE POSTGAP:0`. The older tag has only the first four fields.
fn parse_track(text: &str) -> Option<Entry> {
    let field = |key: &str| {
        text.split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.strip_prefix(':'))
    };
    let number = field("TRACK")?.parse().ok()?;
    let kind = field("TYPE")?.to_string();
    let frames = field("FRAMES")?.parse().ok()?;
    let pregap = field("PREGAP").map_or(Some(0), |v| v.parse().ok())?;
    let pregap_stored = field("PGTYPE").is_some_and(|t| t.starts_with('V'));
    let postgap = field("POSTGAP").map_or(Some(0), |v| v.parse().ok())?;
    Some(Entry {
        number,
        kind,
        frames,
        pregap,
        pregap_stored,
        postgap,
    })
}

/// A track type's mode and the bytes of each stored sector that are its data.
fn kind_of(kind: &str) -> Option<(TrackMode, usize)> {
    Some(match kind {
        "AUDIO" => (TrackMode::Audio, RAW_SECTOR),
        "MODE1_RAW" => (TrackMode::Mode1, RAW_SECTOR),
        "MODE1" => (TrackMode::Mode1, 2048),
        "MODE2_RAW" => (TrackMode::Mode2, RAW_SECTOR),
        "MODE2" | "MODE2_FORM_MIX" => (TrackMode::Mode2, 2336),
        "MODE2_FORM1" => (TrackMode::Mode2, 2048),
        _ => return None,
    })
}

/// Stored sectors `[from, to)`.
type Stored = (u64, u64);

/// The tracks on the disc, and the stored sectors that are audio.
fn lay_out(entries: &[Entry]) -> Result<(Vec<Track>, Vec<Stored>), String> {
    let mut tracks = Vec::new();
    let mut audio = Vec::new();
    let (mut lba, mut stored) = (0u32, 0u32);
    for e in entries {
        let (mode, size) = kind_of(&e.kind)
            .ok_or_else(|| format!("track {}: type {} unsupported", e.number, e.kind))?;
        let skipped = if e.pregap_stored { e.pregap } else { 0 };
        let length = e
            .frames
            .checked_sub(skipped)
            .ok_or_else(|| format!("track {}: pregap longer than the track", e.number))?;
        let pregap_lba = lba;
        let start_lba = lba + e.pregap;
        let from = stored + skipped;
        tracks.push(Track::stored_from(
            e.number, mode, size, from, pregap_lba, start_lba, length,
        ));
        if mode == TrackMode::Audio {
            audio.push((u64::from(from), u64::from(from + length)));
        }
        lba = start_lba + length + e.postgap;
        stored += e.frames.div_ceil(TRACK_PADDING) * TRACK_PADDING;
    }
    Ok((tracks, audio))
}

struct ChdSource {
    chd: Chd<BufReader<File>>,
    /// Bytes per stored sector, subcode included.
    unit: usize,
    per_hunk: u64,
    /// The hunk in `buffer`, decompressed.
    cached: Option<u64>,
    buffer: Vec<u8>,
    compressed: Vec<u8>,
    /// Stored sector ranges, `[from, to)`, that hold audio.
    audio: Vec<Stored>,
}

impl SectorSource for ChdSource {
    fn read_sector(&mut self, index: u64, buf: &mut [u8]) -> bool {
        let hunk = index / self.per_hunk;
        if self.cached != Some(hunk) {
            self.cached = None;
            let Ok(hunk_num) = u32::try_from(hunk) else {
                return false;
            };
            let ok = self
                .chd
                .hunk(hunk_num)
                .and_then(|mut h| h.read_hunk_in(&mut self.compressed, &mut self.buffer))
                .is_ok();
            if !ok {
                return false;
            }
            self.cached = Some(hunk);
        }
        let at = (index % self.per_hunk) as usize * self.unit;
        let Some(src) = self.buffer.get(at..at + buf.len()) else {
            return false;
        };
        buf.copy_from_slice(src);
        if self.audio.iter().any(|&(a, b)| (a..b).contains(&index)) {
            for pair in buf.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_line_reads_every_field() {
        let e = parse_track(
            "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:11778 PREGAP:150 PGTYPE:VAUDIO PGSUB:RW POSTGAP:0",
        )
        .unwrap();
        assert_eq!(
            e,
            Entry {
                number: 2,
                kind: "AUDIO".into(),
                frames: 11778,
                pregap: 150,
                pregap_stored: true,
                postgap: 0,
            }
        );
        // The older tag stops after FRAMES.
        let old = parse_track("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1000").unwrap();
        assert_eq!((old.pregap, old.pregap_stored, old.postgap), (0, false, 0));
    }

    /// Tekken 3's layout as chdman stores it, against where its cue sheet puts
    /// the tracks: a data track, then two audio tracks each with a two-second
    /// pregap stored in front of it.
    #[test]
    fn tracks_are_laid_out_as_the_cue_sheet_has_them() {
        let entries: Vec<Entry> = [
            "TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:268934 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
            "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:11778 PREGAP:150 PGTYPE:VAUDIO PGSUB:NONE POSTGAP:0",
            "TRACK:3 TYPE:AUDIO SUBTYPE:NONE FRAMES:11923 PREGAP:150 PGTYPE:VAUDIO PGSUB:NONE POSTGAP:0",
        ]
        .iter()
        .map(|l| parse_track(l).unwrap())
        .collect();
        let (tracks, audio) = lay_out(&entries).unwrap();
        let t: Vec<_> = tracks
            .iter()
            .map(|t| (t.pregap_lba, t.start_lba, t.length))
            .collect();
        assert_eq!(
            t,
            [
                (0, 0, 268934),
                (268934, 269084, 11628),
                (280712, 280862, 11773)
            ]
        );
        // 268934 pads to 268936; the audio after its stored pregap.
        assert_eq!(audio[0], (268936 + 150, 268936 + 11778));
    }
}
