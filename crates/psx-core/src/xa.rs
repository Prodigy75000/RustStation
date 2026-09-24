// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! XA-ADPCM: the CD-ROM controller's own audio decoder.
//!
//! Written from psx-spx, "CDROM XA Audio ADPCM Compression" and "CDROM XA
//! Subheader, File, Channel, Interleave". Notes in `docs/notes/CDROM.md`.
//!
//! A real-time Form 2 audio sector carries 18 portions of 128 bytes, each a
//! 16-byte header of shift/filter bytes and 28 words of packed samples. The
//! prediction is the same as SPU-ADPCM's, with four filters instead of five.
//! The decoded stream is at 37 800 or 18 900 Hz, and the controller resamples
//! it to the SPU's 44 100 Hz with a 7-phase "zigzag" filter: seven outputs for
//! every six inputs.

/// Portions per sector, and their size.
const PORTIONS: usize = 18;
const PORTION_BYTES: usize = 128;

/// The sector buffer this reads from begins at the sector header, so the
/// four header bytes and eight subheader bytes come first.
pub const DATA_OFFSET: usize = 12;

const POS: [i32; 4] = [0, 60, 115, 98];
const NEG: [i32; 4] = [0, 0, -52, -55];

/// The coding-info byte from the subheader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coding {
    pub stereo: bool,
    pub half_rate: bool,
    pub eight_bit: bool,
}

impl Coding {
    pub fn from_byte(ci: u8) -> Coding {
        Coding {
            stereo: ci & 0x03 == 1,
            half_rate: ci & 0x0C == 0x04,
            eight_bit: ci & 0x30 == 0x10,
        }
    }
}

/// The decoder's memory between sectors: the predictor for each channel and
/// the resampler's ring.
#[derive(Clone)]
pub struct Decoder {
    /// `old`, `older` for left (or mono), then for right.
    pub(crate) prev: [[i16; 2]; 2],
    /// The last 32 samples at the decoded rate, per channel.
    pub(crate) ring: [[i16; 32]; 2],
    pub(crate) ring_pos: u8,
    /// Counts down the six inputs that produce seven outputs.
    pub(crate) six: u8,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            prev: [[0; 2]; 2],
            ring: [[0; 32]; 2],
            ring_pos: 0,
            six: 6,
        }
    }

    /// Decode a sector, handing each 44 100 Hz stereo frame to `out`.
    ///
    /// `sector` starts at the header, as the drive's buffer does, and must
    /// hold at least the 0x900 bytes of audio after the subheader.
    pub fn decode_sector(&mut self, sector: &[u8], coding: Coding, out: &mut impl FnMut([i16; 2])) {
        let mut left = Vec::with_capacity(4032);
        let mut right = Vec::with_capacity(2016);
        for p in 0..PORTIONS {
            let base = DATA_OFFSET + p * PORTION_BYTES;
            let portion = &sector[base..base + PORTION_BYTES];
            if coding.eight_bit {
                self.portion_8bit(portion, coding.stereo, &mut left, &mut right);
            } else {
                self.portion_4bit(portion, coding.stereo, &mut left, &mut right);
            }
        }

        // 18 900 Hz is fed through the 37 800 Hz resampler twice over. psx-spx
        // does not give the half-rate filter; this is the guess recorded in the
        // notes.
        let repeat = if coding.half_rate { 2 } else { 1 };
        for (i, &l) in left.iter().enumerate() {
            let r = if coding.stereo { right[i] } else { l };
            for _ in 0..repeat {
                self.resample([l, r], out);
            }
        }
    }

    fn portion_4bit(
        &mut self,
        portion: &[u8],
        stereo: bool,
        left: &mut Vec<i16>,
        right: &mut Vec<i16>,
    ) {
        for blk in 0..4 {
            for nibble in 0..2 {
                // Stereo: the low nibble is left, the high one right. Mono: the
                // low nibbles are 28 samples, then the high nibbles 28 more.
                let ch = if stereo { nibble } else { 0 };
                let header = portion[4 + blk * 2 + nibble];
                let (shift, filter) = shift_filter(header);
                let [mut old, mut older] = self.prev[ch].map(|x| x as i32);
                for j in 0..28 {
                    let byte = portion[16 + blk + j * 4];
                    let t = (((byte >> (nibble * 4)) & 0x0F) as i32) << 28 >> 28;
                    let s = predict(t << 12, shift, filter, old, older);
                    older = old;
                    old = s;
                    if ch == 0 {
                        left.push(s as i16)
                    } else {
                        right.push(s as i16)
                    }
                }
                self.prev[ch] = [old as i16, older as i16];
            }
        }
    }

    fn portion_8bit(
        &mut self,
        portion: &[u8],
        stereo: bool,
        left: &mut Vec<i16>,
        right: &mut Vec<i16>,
    ) {
        for blk in 0..4 {
            let ch = if stereo { blk & 1 } else { 0 };
            let header = portion[4 + blk];
            let (shift, filter) = shift_filter(header);
            let [mut old, mut older] = self.prev[ch].map(|x| x as i32);
            for j in 0..28 {
                let t = portion[16 + blk + j * 4] as i8 as i32;
                let s = predict(t << 8, shift, filter, old, older);
                older = old;
                old = s;
                if ch == 0 {
                    left.push(s as i16)
                } else {
                    right.push(s as i16)
                }
            }
            self.prev[ch] = [old as i16, older as i16];
        }
    }

    /// One frame at the decoded rate in; every sixth, seven frames out.
    fn resample(&mut self, frame: [i16; 2], out: &mut impl FnMut([i16; 2])) {
        let p = self.ring_pos as usize;
        self.ring[0][p & 0x1F] = frame[0];
        self.ring[1][p & 0x1F] = frame[1];
        self.ring_pos = self.ring_pos.wrapping_add(1);
        self.six -= 1;
        if self.six != 0 {
            return;
        }
        self.six = 6;
        let p = self.ring_pos as usize;
        for table in &ZIGZAG {
            let mut o = [0i32; 2];
            for (ch, ring) in self.ring.iter().enumerate() {
                for (i, &tap) in table.iter().enumerate() {
                    o[ch] += (ring[(p.wrapping_sub(i + 1)) & 0x1F] as i32 * tap as i32) >> 15;
                }
            }
            out([clamp16(o[0]) as i16, clamp16(o[1]) as i16]);
        }
    }
}

/// Shift 13 to 15 are reserved and act as 9.
fn shift_filter(header: u8) -> (i32, usize) {
    let mut shift = (header & 0x0F) as i32;
    if shift > 12 {
        shift = 9;
    }
    (shift, ((header >> 4) & 3) as usize)
}

/// `raw` is the sample already in the top of a 16-bit word.
fn predict(raw: i32, shift: i32, filter: usize, old: i32, older: i32) -> i32 {
    clamp16((raw >> shift) + ((old * POS[filter] + older * NEG[filter] + 32) >> 6))
}

fn clamp16(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

/// The seven zigzag phases, from psx-spx, 29 taps each, newest first. They
/// sum to 0x73EB..0x741D rather than 0x8000, so the resampler has a gain of
/// about 0.91; that is the document's data, kept as it is.
#[rustfmt::skip]
const ZIGZAG: [[i16; 29]; 7] = [
    [0, 0, 0, 0, 0, -2, 10, -34, 65, -84, 52, 9, -266, 1024, -2680, 9036, 26516, -6016, 3021, -1571, 848, -365, 107, 10, -16, 17, -8, 3, -1],
    [0, 0, 0, -2, 0, 3, -19, 60, -75, 162, -227, 306, -67, -615, 3229, 29883, -4532, 2488, -1471, 882, -424, 166, -27, 5, 6, -8, 3, -1, 0],
    [0, 0, -1, 3, -2, -5, 31, -74, 179, -402, 689, -926, 1272, -1446, 31033, -1446, 1272, -926, 689, -402, 179, -74, 31, -5, -2, 3, -1, 0, 0],
    [0, -1, 3, -8, 6, 5, -27, 166, -424, 882, -1471, 2488, -4532, 29883, 3229, -615, -67, 306, -227, 162, -75, 60, -19, 3, 0, -2, 0, 0, 0],
    [-1, 3, -8, 17, -16, 10, 107, -365, 848, -1571, 3021, -6016, 26516, 9036, -2680, 1024, -266, 9, 52, -84, 65, -34, 10, -1, 0, 1, 0, 0, 0],
    [2, -8, 16, -35, 43, 26, -235, 635, -1352, 2810, -5882, 21472, 15367, -4681, 2062, -839, 347, -68, -23, 70, -35, 17, -5, 0, 0, 0, 0, 0, 0],
    [-5, 17, -35, 70, -23, -68, 347, -839, 2062, -4681, 15367, 21472, -5882, 2810, -1352, 635, -235, 26, 43, -35, 16, -8, 2, 0, 0, 0, 0, 0, 0],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_info_bits() {
        assert_eq!(
            Coding::from_byte(0x01),
            Coding {
                stereo: true,
                half_rate: false,
                eight_bit: false
            }
        );
        assert_eq!(
            Coding::from_byte(0x14),
            Coding {
                stereo: false,
                half_rate: true,
                eight_bit: true
            }
        );
        // The reserved values are neither.
        assert!(!Coding::from_byte(0x02).stereo);
    }

    #[test]
    fn four_bit_stereo_puts_low_nibbles_left_and_high_right() {
        let mut portion = [0u8; 128];
        // Shift 12 on every block: a nibble comes out as itself.
        for h in 4..12 {
            portion[h] = 0x0C;
        }
        // Block 0, sample 0: left 3, right -2.
        portion[16] = 0xE3;
        // Block 1, sample 0: left 5.
        portion[17] = 0x05;
        let mut d = Decoder::new();
        let (mut l, mut r) = (Vec::new(), Vec::new());
        d.portion_4bit(&portion, true, &mut l, &mut r);
        assert_eq!(l.len(), 112);
        assert_eq!(r.len(), 112);
        assert_eq!(l[0], 3);
        assert_eq!(r[0], -2);
        assert_eq!(l[28], 5, "block 1 follows block 0 on the same channel");
    }

    #[test]
    fn four_bit_mono_is_low_nibbles_then_high_nibbles() {
        let mut portion = [0u8; 128];
        for h in 4..12 {
            portion[h] = 0x0C;
        }
        portion[16] = 0x71; // sample 0: low 1, high 7
        portion[20] = 0x02; // sample 1, low
        let mut d = Decoder::new();
        let (mut l, mut r) = (Vec::new(), Vec::new());
        d.portion_4bit(&portion, false, &mut l, &mut r);
        assert_eq!(l.len(), 224);
        assert!(r.is_empty());
        assert_eq!(&l[0..2], &[1, 2]);
        assert_eq!(l[28], 7, "then the high nibbles of block 0");
    }

    #[test]
    fn prediction_uses_the_four_xa_filters() {
        // Filter 2 on an impulse: s1 = 115/64 of it, s2 = 115/64 s1 - 52/64 s0.
        assert_eq!(predict(0x100 << 4, 0, 2, 0, 0), 0x1000);
        let s1 = predict(0, 0, 2, 0x1000, 0);
        assert_eq!(s1, (0x1000 * 115 + 32) >> 6);
        assert_eq!(
            predict(0, 0, 2, s1, 0x1000),
            (s1 * 115 - 0x1000 * 52 + 32) >> 6
        );
        // And the shift: 13 to 15 behave as 9.
        assert_eq!(shift_filter(0x2D), (9, 2));
    }

    #[test]
    fn eight_bit_takes_a_byte_per_sample() {
        let mut portion = [0u8; 128];
        for h in 4..8 {
            portion[h] = 0x08; // shift 8: a byte comes out as itself
        }
        portion[16] = 0x80; // block 0 (left), sample 0: -128
        portion[17] = 0x7F; // block 1 (right), sample 0: 127
        let mut d = Decoder::new();
        let (mut l, mut r) = (Vec::new(), Vec::new());
        d.portion_8bit(&portion, true, &mut l, &mut r);
        assert_eq!((l.len(), r.len()), (56, 56));
        assert_eq!((l[0], r[0]), (-128, 127));
    }

    #[test]
    fn the_resampler_makes_seven_frames_from_six() {
        let mut d = Decoder::new();
        let mut n = 0;
        for i in 0..600 {
            d.resample([i, -i], &mut |_| n += 1);
        }
        assert_eq!(n, 700);
    }

    #[test]
    fn the_resampler_gain_is_the_table_sum() {
        // A constant in, once the ring is full, comes out scaled by each
        // table's sum over 0x8000, give or take the per-tap rounding.
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for _ in 0..120 {
            d.resample([0x4000, 0x4000], &mut |f| out.push(f[0] as i32));
        }
        let tail = &out[out.len() - 7..];
        for (k, &v) in tail.iter().enumerate() {
            let sum: i32 = ZIGZAG[k].iter().map(|&t| t as i32).sum();
            let ideal = 0x4000 * sum >> 15;
            assert!((v - ideal).abs() <= 29, "phase {k}: {v} vs {ideal}");
        }
    }
}
