// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! COP2: the Geometry Transformation Engine.
//!
//! A fixed-point vector coprocessor. It does the perspective transforms, the
//! lighting and the ordering-table depth that the GPU then draws, and it has no
//! floating point anywhere, which is the reason a PlayStation's geometry has
//! its particular character.
//!
//! ## Fixed-point conventions
//!
//! Nearly everything is a signed 1.3.12 or 1.19.12 fixed-point value: the low
//! twelve bits are fractional. The `sf` bit in a command word selects whether
//! results are shifted right by 12 (back to integer scale) or left as-is, and
//! getting it backwards produces output that is wrong by a factor of 4096 and
//! looks like a completely broken transform rather than a scaling error.
//!
//! ## Saturation is the point, not an edge case
//!
//! Every intermediate has a defined width, and every overflow both saturates
//! *and* latches a bit in [`Gte::flag`]. Game code reads that register to
//! decide whether a polygon is off-screen or degenerate, so the flags are as
//! much a result as the numbers are. The accumulators are 44-bit, the `IR`
//! registers 16-bit, colours 8-bit, and each has its own flag.
//!
//! `FLAG` bit 31 is not stored: it is the OR of the bits that matter, and is
//! recomputed on every write.
//!
//! ## What is not modelled
//!
//! Command timing. Each command takes a documented number of cycles and stalls
//! the CPU if it reads a result too early; here every command completes
//! instantly. See `docs/notes/GTE.md`.

/// Reciprocal seed table for the Newton-Raphson divider.
///
/// This is **measured hardware data**, used as data: the GTE's divider is a
/// table lookup followed by two refinement steps, and the results only match
/// hardware if the seeds do. Cited in `docs/notes/GTE.md`.
const UNR_TABLE: [u8; 257] = [
    0xFF, 0xFD, 0xFB, 0xF9, 0xF7, 0xF5, 0xF3, 0xF1, 0xEF, 0xEE, 0xEC, 0xEA, 0xE8, 0xE6, 0xE4, 0xE3,
    0xE1, 0xDF, 0xDD, 0xDC, 0xDA, 0xD8, 0xD6, 0xD5, 0xD3, 0xD1, 0xD0, 0xCE, 0xCD, 0xCB, 0xC9, 0xC8,
    0xC6, 0xC5, 0xC3, 0xC1, 0xC0, 0xBE, 0xBD, 0xBB, 0xBA, 0xB8, 0xB7, 0xB5, 0xB4, 0xB2, 0xB1, 0xB0,
    0xAE, 0xAD, 0xAB, 0xAA, 0xA9, 0xA7, 0xA6, 0xA4, 0xA3, 0xA2, 0xA0, 0x9F, 0x9E, 0x9C, 0x9B, 0x9A,
    0x99, 0x97, 0x96, 0x95, 0x94, 0x92, 0x91, 0x90, 0x8F, 0x8D, 0x8C, 0x8B, 0x8A, 0x89, 0x87, 0x86,
    0x85, 0x84, 0x83, 0x82, 0x81, 0x7F, 0x7E, 0x7D, 0x7C, 0x7B, 0x7A, 0x79, 0x78, 0x77, 0x75, 0x74,
    0x73, 0x72, 0x71, 0x70, 0x6F, 0x6E, 0x6D, 0x6C, 0x6B, 0x6A, 0x69, 0x68, 0x67, 0x66, 0x65, 0x64,
    0x63, 0x62, 0x61, 0x60, 0x5F, 0x5E, 0x5D, 0x5D, 0x5C, 0x5B, 0x5A, 0x59, 0x58, 0x57, 0x56, 0x55,
    0x54, 0x53, 0x53, 0x52, 0x51, 0x50, 0x4F, 0x4E, 0x4D, 0x4D, 0x4C, 0x4B, 0x4A, 0x49, 0x48, 0x48,
    0x47, 0x46, 0x45, 0x44, 0x43, 0x43, 0x42, 0x41, 0x40, 0x3F, 0x3F, 0x3E, 0x3D, 0x3C, 0x3C, 0x3B,
    0x3A, 0x39, 0x39, 0x38, 0x37, 0x36, 0x36, 0x35, 0x34, 0x33, 0x33, 0x32, 0x31, 0x31, 0x30, 0x2F,
    0x2E, 0x2E, 0x2D, 0x2C, 0x2C, 0x2B, 0x2A, 0x2A, 0x29, 0x28, 0x28, 0x27, 0x26, 0x26, 0x25, 0x24,
    0x24, 0x23, 0x22, 0x22, 0x21, 0x20, 0x20, 0x1F, 0x1E, 0x1E, 0x1D, 0x1D, 0x1C, 0x1B, 0x1B, 0x1A,
    0x19, 0x19, 0x18, 0x18, 0x17, 0x16, 0x16, 0x15, 0x15, 0x14, 0x14, 0x13, 0x12, 0x12, 0x11, 0x11,
    0x10, 0x0F, 0x0F, 0x0E, 0x0E, 0x0D, 0x0D, 0x0C, 0x0C, 0x0B, 0x0A, 0x0A, 0x09, 0x09, 0x08, 0x08,
    0x07, 0x07, 0x06, 0x06, 0x05, 0x05, 0x04, 0x04, 0x03, 0x03, 0x02, 0x02, 0x01, 0x01, 0x00, 0x00,
    0x00,
];

// FLAG bits. Named because a bare number here is unreviewable, and tabulated
// rather than computed because the two MAC runs are not adjacent: positive
// overflow is 30..28 and negative is 27..25, so any single expression covering
// both is off by one somewhere.
const F_MAC_POS: [u32; 3] = [30, 29, 28];
const F_MAC_NEG: [u32; 3] = [27, 26, 25];
const F_MAC0_POS: u32 = 16;
const F_MAC0_NEG: u32 = 15;
const F_DIVIDE: u32 = 17;
const F_SZ3: u32 = 18;
const F_SX2: u32 = 14;
const F_SY2: u32 = 13;
const F_IR0: u32 = 12;
/// Bits 30..23 and 18..13, the ones that feed the master error bit.
const F_ERROR_MASK: u32 = 0x7F87_E000;

/// Truncate to the accumulator's real width and sign extend from bit 43.
///
/// MAC1..MAC3 are 44-bit hardware registers. Passing that range does not just
/// raise a flag and keep the wider value: the value **wraps**, so a large
/// positive result comes back out negative. Nothing downstream can tell the
/// difference through `MAC1`..`MAC3` themselves, which are read as 32 bits and
/// so carry the same low bits either way. It shows up in the consumers that
/// take the full accumulator, which is why `SZ3` was the register that caught
/// this and `MAC3` was not.
fn wrap44(value: i64) -> i64 {
    (value << 20) >> 20
}

/// `RSTA_GTE_TRACE=1` dumps the register file around every command.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RSTA_GTE_TRACE").is_ok_and(|v| v != "0"))
}

#[derive(Clone, Default)]
pub struct Gte {
    // ---- data registers (cop2r0..31) ----
    /// V0, V1, V2, each an (x, y, z) triple.
    v: [[i16; 3]; 3],
    /// RGBC: colour plus the `code` byte, which is passed through untouched.
    rgbc: [u8; 4],
    otz: u16,
    /// IR0 through IR3.
    ir: [i16; 4],
    /// Screen XY FIFO, three deep.
    sxy: [(i16, i16); 3],
    /// Screen Z FIFO, four deep.
    sz: [u16; 4],
    /// Colour FIFO, three deep.
    rgb: [[u8; 4]; 3],
    /// cop2r23, which software may use as scratch.
    res1: u32,
    /// MAC0 through MAC3.
    mac: [i32; 4],
    lzcs: u32,
    lzcr: u32,

    // ---- control registers (cop2r32..63) ----
    /// Rotation matrix.
    rt: [[i16; 3]; 3],
    /// Translation vector.
    tr: [i32; 3],
    /// Light matrix.
    llm: [[i16; 3]; 3],
    /// Background colour.
    bk: [i32; 3],
    /// Light colour matrix.
    lcm: [[i16; 3]; 3],
    /// Far colour.
    fc: [i32; 3],
    /// Screen offset.
    of: [i32; 2],
    /// Projection plane distance.
    h: u16,
    dqa: i16,
    dqb: i32,
    zsf3: i16,
    zsf4: i16,
    pub flag: u32,

    /// Commands with an opcode this core does not recognise.
    pub unknown_commands: u64,
    /// Colour channels clamped on the way into the colour FIFO.
    ///
    /// Host-side observation. Saturation is legal and routine in small
    /// amounts, so this is not an error count; it is here because "are these
    /// vertex colours the GTE's doing?" has no other cheap answer, and a
    /// primitive whose channels are all exactly 0 or 255 either came from
    /// clamping or did not come from here at all.
    pub colour_saturations: u64,
}

impl Gte {
    pub fn new() -> Gte {
        Gte {
            lzcr: 32,
            ..Default::default()
        }
    }

    // ---- flags and saturation -------------------------------------------

    fn set_flag(&mut self, bit: u32) {
        self.flag |= 1 << bit;
    }

    fn refresh_error_bit(&mut self) {
        if self.flag & F_ERROR_MASK != 0 {
            self.flag |= 1 << 31;
        } else {
            self.flag &= !(1 << 31);
        }
    }

    /// Raise the 44-bit overflow flags for accumulator `n` without storing.
    ///
    /// The check is on the **unshifted** value: the accumulator is 44 bits wide
    /// regardless of whether the result is scaled down afterwards.
    ///
    /// Split out from [`Gte::set_mac`] because the interpolation commands run
    /// their difference from the far colour through the range check but keep
    /// the previous `MAC` contents: the flag is an output of that subtraction
    /// even though the register is not.
    fn check_mac(&mut self, n: usize, value: i64) {
        if value > 0x7FF_FFFF_FFFF {
            self.set_flag(F_MAC_POS[n - 1]);
        }
        if value < -0x800_0000_0000 {
            self.set_flag(F_MAC_NEG[n - 1]);
        }
    }

    /// One row of a matrix-vector product, accumulated the way the hardware
    /// does it: `base`, then the three products added **one at a time**, with
    /// the range checked and the accumulator wrapped after each.
    ///
    /// Evaluating the row as a single 64-bit expression and checking once gets
    /// the value right whenever nothing overflows, and gets the flags wrong
    /// whenever something does. A running total that overflows on the second
    /// term and comes back on the third leaves no trace in the total, and a row
    /// can set the positive *and* the negative flag for the same accumulator in
    /// one command, which no single check can produce.
    ///
    /// Returns the wrapped accumulator, before the command's shift. `MAC` is
    /// left holding the shifted value.
    fn mac_row(&mut self, n: usize, base: i64, terms: [i64; 3], sf: bool) -> i64 {
        let mut acc = base;
        for term in terms {
            acc += term;
            self.check_mac(n, acc);
            acc = wrap44(acc);
        }
        self.mac[n] = if sf { acc >> 12 } else { acc } as i32;
        acc
    }

    /// Write MAC1..MAC3: flag, wrap to the accumulator's real width, then apply
    /// the command's shift.
    fn set_mac(&mut self, n: usize, value: i64, sf: bool) -> i32 {
        self.check_mac(n, value);
        let value = wrap44(value);
        let out = if sf { value >> 12 } else { value } as i32;
        self.mac[n] = out;
        out
    }

    fn set_mac0(&mut self, value: i64) -> i32 {
        if value > 0x7FFF_FFFF {
            self.set_flag(F_MAC0_POS);
        }
        if value < -0x8000_0000 {
            self.set_flag(F_MAC0_NEG);
        }
        self.mac[0] = value as i32;
        self.mac[0]
    }

    /// Write IR1..IR3, saturating. `lm` clamps to non-negative.
    fn set_ir(&mut self, n: usize, value: i32, lm: bool) {
        let min = if lm { 0 } else { -0x8000 };
        if value < min || value > 0x7FFF {
            self.set_flag(25 - n as u32);
        }
        self.ir[n] = value.clamp(min, 0x7FFF) as i16;
    }

    fn set_mac_and_ir(&mut self, n: usize, value: i64, sf: bool, lm: bool) {
        let m = self.set_mac(n, value, sf);
        self.set_ir(n, m, lm);
    }

    fn saturate_colour(&mut self, value: i32, bit: u32) -> u8 {
        if !(0..=255).contains(&value) {
            self.set_flag(bit);
            self.colour_saturations += 1;
        }
        value.clamp(0, 255) as u8
    }

    /// Push MAC1..3 into the colour FIFO, saturating each channel.
    fn push_colour(&mut self) {
        let r = self.saturate_colour(self.mac[1] >> 4, 21);
        let g = self.saturate_colour(self.mac[2] >> 4, 20);
        let b = self.saturate_colour(self.mac[3] >> 4, 19);
        self.rgb[0] = self.rgb[1];
        self.rgb[1] = self.rgb[2];
        self.rgb[2] = [r, g, b, self.rgbc[3]];
    }

    fn push_sz(&mut self, value: i64) {
        if !(0..=0xFFFF).contains(&value) {
            self.set_flag(F_SZ3);
        }
        self.sz[0] = self.sz[1];
        self.sz[1] = self.sz[2];
        self.sz[2] = self.sz[3];
        self.sz[3] = value.clamp(0, 0xFFFF) as u16;
    }

    fn push_sxy(&mut self, x: i64, y: i64) {
        if !(-0x400..=0x3FF).contains(&x) {
            self.set_flag(F_SX2);
        }
        if !(-0x400..=0x3FF).contains(&y) {
            self.set_flag(F_SY2);
        }
        self.sxy[0] = self.sxy[1];
        self.sxy[1] = self.sxy[2];
        self.sxy[2] = (x.clamp(-0x400, 0x3FF) as i16, y.clamp(-0x400, 0x3FF) as i16);
    }

    // ---- the divider ----------------------------------------------------

    /// `(H * 0x20000 / SZ3 + 1) / 2`, by the hardware's own method.
    ///
    /// Not a plain division: the GTE seeds a reciprocal from [`UNR_TABLE`] and
    /// refines it twice, and the low bits differ from a true divide. A result
    /// larger than 0x1FFFF saturates and sets the divide-overflow flag, which
    /// is how software detects a vertex at or behind the eye.
    fn divide(&mut self, h: u16, sz3: u16) -> u32 {
        let h = h as u32;
        let sz3 = sz3 as u32;

        if h >= sz3.wrapping_mul(2) {
            self.set_flag(F_DIVIDE);
            return 0x1FFFF;
        }

        let z = (sz3 as u16).leading_zeros();
        let n = (h << z) as u64;
        let d = (sz3 << z) as u64;
        let u = UNR_TABLE[(((d as i64 - 0x7FC0) >> 7) as usize).min(256)] as u64 + 0x101;
        let d = ((0x0200_0080 - (d * u)) >> 8) as i64;
        let d = (0x0000_0080 + (d as u64 * u)) >> 8;
        (((n * d) + 0x8000) >> 16).min(0x1FFFF) as u32
    }

    // ---- register access -------------------------------------------------

    /// `MFC2`: read a data register.
    pub fn read_data(&self, index: u32) -> u32 {
        match index & 31 {
            0 => pack_xy(self.v[0][0], self.v[0][1]),
            1 => self.v[0][2] as i32 as u32,
            2 => pack_xy(self.v[1][0], self.v[1][1]),
            3 => self.v[1][2] as i32 as u32,
            4 => pack_xy(self.v[2][0], self.v[2][1]),
            5 => self.v[2][2] as i32 as u32,
            6 => u32::from_le_bytes(self.rgbc),
            7 => self.otz as u32,
            8 => self.ir[0] as i32 as u32,
            9 => self.ir[1] as i32 as u32,
            10 => self.ir[2] as i32 as u32,
            11 => self.ir[3] as i32 as u32,
            12 => pack_xy(self.sxy[0].0, self.sxy[0].1),
            13 => pack_xy(self.sxy[1].0, self.sxy[1].1),
            // cop2r14 and cop2r15 both read the newest entry.
            14 | 15 => pack_xy(self.sxy[2].0, self.sxy[2].1),
            16 => self.sz[0] as u32,
            17 => self.sz[1] as u32,
            18 => self.sz[2] as u32,
            19 => self.sz[3] as u32,
            20 => u32::from_le_bytes(self.rgb[0]),
            21 => u32::from_le_bytes(self.rgb[1]),
            22 => u32::from_le_bytes(self.rgb[2]),
            23 => self.res1,
            24 => self.mac[0] as u32,
            25 => self.mac[1] as u32,
            26 => self.mac[2] as u32,
            27 => self.mac[3] as u32,
            // IRGB and ORGB are the same derived value: IR1..IR3 squeezed into
            // five bits each.
            28 | 29 => {
                let c = |v: i16| (v / 0x80).clamp(0, 0x1F) as u32;
                c(self.ir[1]) | (c(self.ir[2]) << 5) | (c(self.ir[3]) << 10)
            }
            30 => self.lzcs,
            _ => self.lzcr,
        }
    }

    /// `MTC2`: write a data register.
    pub fn write_data(&mut self, index: u32, val: u32) {
        let lo = val as i16;
        let hi = (val >> 16) as i16;
        match index & 31 {
            0 => {
                self.v[0][0] = lo;
                self.v[0][1] = hi;
            }
            1 => self.v[0][2] = lo,
            2 => {
                self.v[1][0] = lo;
                self.v[1][1] = hi;
            }
            3 => self.v[1][2] = lo,
            4 => {
                self.v[2][0] = lo;
                self.v[2][1] = hi;
            }
            5 => self.v[2][2] = lo,
            6 => self.rgbc = val.to_le_bytes(),
            7 => self.otz = val as u16,
            8 => self.ir[0] = lo,
            9 => self.ir[1] = lo,
            10 => self.ir[2] = lo,
            11 => self.ir[3] = lo,
            12 => self.sxy[0] = (lo, hi),
            13 => self.sxy[1] = (lo, hi),
            14 => self.sxy[2] = (lo, hi),
            // cop2r15 is not a register: writing it *pushes* the FIFO, which is
            // how software appends a vertex without shuffling the others.
            15 => {
                self.sxy[0] = self.sxy[1];
                self.sxy[1] = self.sxy[2];
                self.sxy[2] = (lo, hi);
            }
            16 => self.sz[0] = val as u16,
            17 => self.sz[1] = val as u16,
            18 => self.sz[2] = val as u16,
            19 => self.sz[3] = val as u16,
            20 => self.rgb[0] = val.to_le_bytes(),
            21 => self.rgb[1] = val.to_le_bytes(),
            22 => self.rgb[2] = val.to_le_bytes(),
            23 => self.res1 = val,
            24 => self.mac[0] = val as i32,
            25 => self.mac[1] = val as i32,
            26 => self.mac[2] = val as i32,
            27 => self.mac[3] = val as i32,
            // Writing IRGB expands five-bit channels back out into IR1..IR3.
            28 => {
                self.ir[1] = ((val & 0x1F) * 0x80) as i16;
                self.ir[2] = (((val >> 5) & 0x1F) * 0x80) as i16;
                self.ir[3] = (((val >> 10) & 0x1F) * 0x80) as i16;
            }
            // ORGB is read-only.
            29 => {}
            // Writing LZCS computes the leading-bit count immediately; LZCR is
            // just where the answer is read from.
            30 => {
                self.lzcs = val;
                self.lzcr = if (val as i32) < 0 {
                    (!val).leading_zeros()
                } else {
                    val.leading_zeros()
                };
            }
            _ => {}
        }
    }

    /// `CFC2`: read a control register.
    pub fn read_control(&self, index: u32) -> u32 {
        match index & 31 {
            0 => pack_xy(self.rt[0][0], self.rt[0][1]),
            1 => pack_xy(self.rt[0][2], self.rt[1][0]),
            2 => pack_xy(self.rt[1][1], self.rt[1][2]),
            3 => pack_xy(self.rt[2][0], self.rt[2][1]),
            // Sign-extended on read, unlike the packed pairs.
            4 => self.rt[2][2] as i32 as u32,
            5 => self.tr[0] as u32,
            6 => self.tr[1] as u32,
            7 => self.tr[2] as u32,
            8 => pack_xy(self.llm[0][0], self.llm[0][1]),
            9 => pack_xy(self.llm[0][2], self.llm[1][0]),
            10 => pack_xy(self.llm[1][1], self.llm[1][2]),
            11 => pack_xy(self.llm[2][0], self.llm[2][1]),
            12 => self.llm[2][2] as i32 as u32,
            13 => self.bk[0] as u32,
            14 => self.bk[1] as u32,
            15 => self.bk[2] as u32,
            16 => pack_xy(self.lcm[0][0], self.lcm[0][1]),
            17 => pack_xy(self.lcm[0][2], self.lcm[1][0]),
            18 => pack_xy(self.lcm[1][1], self.lcm[1][2]),
            19 => pack_xy(self.lcm[2][0], self.lcm[2][1]),
            20 => self.lcm[2][2] as i32 as u32,
            21 => self.fc[0] as u32,
            22 => self.fc[1] as u32,
            23 => self.fc[2] as u32,
            24 => self.of[0] as u32,
            25 => self.of[1] as u32,
            // H reads back sign-extended even though it is used unsigned.
            26 => self.h as i16 as i32 as u32,
            27 => self.dqa as i32 as u32,
            28 => self.dqb as u32,
            29 => self.zsf3 as i32 as u32,
            30 => self.zsf4 as i32 as u32,
            _ => self.flag,
        }
    }

    /// `CTC2`: write a control register.
    pub fn write_control(&mut self, index: u32, val: u32) {
        let lo = val as i16;
        let hi = (val >> 16) as i16;
        match index & 31 {
            0 => {
                self.rt[0][0] = lo;
                self.rt[0][1] = hi;
            }
            1 => {
                self.rt[0][2] = lo;
                self.rt[1][0] = hi;
            }
            2 => {
                self.rt[1][1] = lo;
                self.rt[1][2] = hi;
            }
            3 => {
                self.rt[2][0] = lo;
                self.rt[2][1] = hi;
            }
            4 => self.rt[2][2] = lo,
            5 => self.tr[0] = val as i32,
            6 => self.tr[1] = val as i32,
            7 => self.tr[2] = val as i32,
            8 => {
                self.llm[0][0] = lo;
                self.llm[0][1] = hi;
            }
            9 => {
                self.llm[0][2] = lo;
                self.llm[1][0] = hi;
            }
            10 => {
                self.llm[1][1] = lo;
                self.llm[1][2] = hi;
            }
            11 => {
                self.llm[2][0] = lo;
                self.llm[2][1] = hi;
            }
            12 => self.llm[2][2] = lo,
            13 => self.bk[0] = val as i32,
            14 => self.bk[1] = val as i32,
            15 => self.bk[2] = val as i32,
            16 => {
                self.lcm[0][0] = lo;
                self.lcm[0][1] = hi;
            }
            17 => {
                self.lcm[0][2] = lo;
                self.lcm[1][0] = hi;
            }
            18 => {
                self.lcm[1][1] = lo;
                self.lcm[1][2] = hi;
            }
            19 => {
                self.lcm[2][0] = lo;
                self.lcm[2][1] = hi;
            }
            20 => self.lcm[2][2] = lo,
            21 => self.fc[0] = val as i32,
            22 => self.fc[1] = val as i32,
            23 => self.fc[2] = val as i32,
            24 => self.of[0] = val as i32,
            25 => self.of[1] = val as i32,
            26 => self.h = val as u16,
            27 => self.dqa = lo,
            28 => self.dqb = val as i32,
            29 => self.zsf3 = lo,
            30 => self.zsf4 = lo,
            // Only bits 30..12 are writable; bit 31 is recomputed.
            _ => {
                self.flag = val & 0x7FFF_F000;
                self.refresh_error_bit();
            }
        }
    }

    // ---- commands --------------------------------------------------------

    /// Execute `COP2 imm25`.
    pub fn command(&mut self, word: u32) {
        if trace_enabled() {
            self.trace("in ", word);
        }

        // Every command clears the flags first: they report what *this* command
        // did, not an accumulated history.
        self.flag = 0;

        let op = word & 0x3F;
        let sf = (word >> 19) & 1 != 0;
        let lm = (word >> 10) & 1 != 0;

        match op {
            0x01 => self.rtps(0, sf, lm, true),
            0x06 => self.nclip(),
            0x0C => self.op(sf, lm),
            0x10 => self.dpcs(sf, lm, false),
            0x11 => self.intpl(sf, lm),
            0x12 => self.mvmva(word, sf, lm),
            0x13 => self.ncd(0, sf, lm),
            0x14 => self.cdp(sf, lm),
            0x16 => {
                for i in 0..3 {
                    self.ncd(i, sf, lm);
                }
            }
            0x1B => self.ncc(0, sf, lm),
            0x1C => self.cc(sf, lm),
            0x1E => self.nc(0, sf, lm),
            0x20 => {
                for i in 0..3 {
                    self.nc(i, sf, lm);
                }
            }
            0x28 => self.sqr(sf, lm),
            0x29 => self.dcpl(sf, lm),
            0x2A => self.dpcs(sf, lm, true),
            0x2D => self.avsz3(),
            0x2E => self.avsz4(),
            0x30 => {
                for i in 0..3 {
                    self.rtps(i, sf, lm, i == 2);
                }
            }
            0x3D => self.gpf(sf, lm),
            0x3E => self.gpl(sf, lm),
            0x3F => {
                for i in 0..3 {
                    self.ncc(i, sf, lm);
                }
            }
            _ => self.unknown_commands += 1,
        }

        self.refresh_error_bit();

        if trace_enabled() {
            self.trace("out", word);
        }
    }

    /// Dump the whole register file around a command, for `RSTA_GTE_TRACE=1`.
    ///
    /// `gte/test-all` stops at the first mismatch and names the registers that
    /// disagree, so the failing command is always the last one traced. This is
    /// how its operands are recovered: the test does not print its own inputs.
    fn trace(&self, when: &str, word: u32) {
        let mut line = format!("gte {when} cmd={word:08x}");
        for i in 0..32 {
            line.push_str(&format!(" d{i}={:08x}", self.read_data(i)));
        }
        for i in 0..32 {
            line.push_str(&format!(" c{i}={:08x}", self.read_control(i)));
        }
        eprintln!("{line}");
    }

    /// Perspective transform of vertex `n`.
    ///
    /// `last` marks the final vertex of an RTPT, which is the only one that
    /// updates IR0 with the depth-cue factor.
    fn rtps(&mut self, n: usize, sf: bool, lm: bool, last: bool) {
        let v = self.v[n];
        let mut z = 0i64;
        for row in 0..3 {
            let terms = [
                self.rt[row][0] as i64 * v[0] as i64,
                self.rt[row][1] as i64 * v[1] as i64,
                self.rt[row][2] as i64 * v[2] as i64,
            ];
            let acc = self.mac_row(row + 1, (self.tr[row] as i64) << 12, terms, sf);
            let m = self.mac[row + 1];
            if row < 2 {
                self.set_ir(row + 1, m, lm);
            } else {
                // From the wrapped accumulator, and always shifted by 12
                // whatever `sf` says: SZ3 is a depth in screen units, not a
                // scaled fixed-point value.
                z = acc >> 12;
                // IR3's saturation flag is judged against the *shifted* value
                // even when sf is clear, which is a documented quirk and not a
                // convenience: the flag and the stored value disagree.
                let stored = self.mac[3];
                if !(-0x8000..=0x7FFF).contains(&z) {
                    self.set_flag(22);
                }
                let min = if lm { 0 } else { -0x8000 };
                self.ir[3] = stored.clamp(min, 0x7FFF) as i16;
            }
        }

        self.push_sz(z);
        let sz3 = self.sz[3];
        let n_div = self.divide(self.h, sz3) as i64;

        // The screen coordinate comes from the **full-precision** intermediate,
        // not from MAC0 after it has been truncated to 32 bits. MAC0 still
        // stores the truncated value and still flags the overflow, but a result
        // that overflowed must saturate to the screen edge rather than wrap
        // into a small negative coordinate. Reading it back out of MAC0 turns a
        // vertex that should clamp to +1023 into one at -2, and loses the
        // saturation flag with it.
        let mac0_x = n_div * self.ir[1] as i64 + self.of[0] as i64;
        self.set_mac0(mac0_x);
        let mac0_y = n_div * self.ir[2] as i64 + self.of[1] as i64;
        self.set_mac0(mac0_y);
        self.push_sxy(mac0_x >> 16, mac0_y >> 16);

        if last {
            let depth = n_div * self.dqa as i64 + self.dqb as i64;
            self.set_mac0(depth);
            let ir0 = depth >> 12;
            if !(0..=0x1000).contains(&ir0) {
                self.set_flag(F_IR0);
            }
            self.ir[0] = ir0.clamp(0, 0x1000) as i16;
        }
    }

    /// Cross product of the screen-space edges: positive means the triangle
    /// faces the camera. This is how a game culls back faces.
    fn nclip(&mut self) {
        let (x0, y0) = self.sxy[0];
        let (x1, y1) = self.sxy[1];
        let (x2, y2) = self.sxy[2];
        let v = x0 as i64 * (y1 as i64 - y2 as i64)
            + x1 as i64 * (y2 as i64 - y0 as i64)
            + x2 as i64 * (y0 as i64 - y1 as i64);
        self.set_mac0(v);
    }

    /// Outer product of IR with the rotation matrix's diagonal.
    fn op(&mut self, sf: bool, lm: bool) {
        let (d1, d2, d3) = (
            self.rt[0][0] as i64,
            self.rt[1][1] as i64,
            self.rt[2][2] as i64,
        );
        let (i1, i2, i3) = (self.ir[1] as i64, self.ir[2] as i64, self.ir[3] as i64);
        self.set_mac_and_ir(1, d2 * i3 - d3 * i2, sf, lm);
        self.set_mac_and_ir(2, d3 * i1 - d1 * i3, sf, lm);
        self.set_mac_and_ir(3, d1 * i2 - d2 * i1, sf, lm);
    }

    fn sqr(&mut self, sf: bool, lm: bool) {
        for n in 1..4 {
            let i = self.ir[n] as i64;
            self.set_mac_and_ir(n, i * i, sf, lm);
        }
    }

    /// The ordering-table depth of three or four screen Z values.
    ///
    /// `OTZ` comes from the **full-precision** product, not from `MAC0` after
    /// it has been truncated to 32 bits. A 16-bit scale factor times four
    /// 16-bit depths needs 34 bits, so the product overflows routinely, and the
    /// truncated register can read positive where the real value is negative.
    /// `MAC0` still stores the truncated value; only the saturation sees the
    /// wider one. Same rule as the screen coordinates in [`Gte::rtps`].
    fn avsz3(&mut self) {
        let sum = self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
        let value = self.zsf3 as i64 * sum;
        self.set_mac0(value);
        self.set_otz(value >> 12);
    }

    fn avsz4(&mut self) {
        let sum = self.sz[0] as i64 + self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
        let value = self.zsf4 as i64 * sum;
        self.set_mac0(value);
        self.set_otz(value >> 12);
    }

    fn set_otz(&mut self, value: i64) {
        if !(0..=0xFFFF).contains(&value) {
            self.set_flag(F_SZ3);
        }
        self.otz = value.clamp(0, 0xFFFF) as u16;
    }

    /// Multiply a vector by a matrix and add a translation, with all three
    /// chosen by the command word. The general form the others are built on.
    fn mvmva(&mut self, word: u32, sf: bool, lm: bool) {
        let mx = (word >> 17) & 3;
        let sv = (word >> 15) & 3;
        let cv = (word >> 13) & 3;

        let matrix = match mx {
            0 => self.rt,
            1 => self.llm,
            2 => self.lcm,
            // Selecting the reserved matrix does not read a matrix at all: the
            // multiplexer is left half-driven and the three rows come out of
            // unrelated registers. Only the first row involves the colour
            // register; the other two are one element of the rotation matrix
            // each, repeated across the row. Reproduced because it is
            // reachable, not because anything sensible relies on it.
            _ => {
                let r = -((self.rgbc[0] as i16) << 4);
                let g = (self.rgbc[0] as i16) << 4;
                let (rt13, rt22) = (self.rt[0][2], self.rt[1][1]);
                [[r, g, self.ir[0]], [rt13; 3], [rt22; 3]]
            }
        };

        let vector = match sv {
            0 => [self.v[0][0], self.v[0][1], self.v[0][2]],
            1 => [self.v[1][0], self.v[1][1], self.v[1][2]],
            2 => [self.v[2][0], self.v[2][1], self.v[2][2]],
            _ => [self.ir[1], self.ir[2], self.ir[3]],
        };

        // Translation vector 2 is the far colour, and it is *bugged* on
        // hardware: the first two components are computed with the third
        // missing, so only the last row is right. Games avoid it; the flags
        // still have to match.
        if cv == 2 {
            for (row, mrow) in matrix.iter().enumerate() {
                let partial = ((self.fc[row] as i64) << 12) + mrow[0] as i64 * vector[0] as i64;
                self.set_mac(row + 1, partial, sf);
                self.set_ir(row + 1, self.mac[row + 1], false);

                let full = mrow[1] as i64 * vector[1] as i64 + mrow[2] as i64 * vector[2] as i64;
                let m = self.set_mac(row + 1, full, sf);
                self.set_ir(row + 1, m, lm);
            }
            return;
        }

        let translation = match cv {
            0 => self.tr,
            1 => self.bk,
            _ => [0, 0, 0],
        };

        for row in 0..3 {
            let terms = [
                matrix[row][0] as i64 * vector[0] as i64,
                matrix[row][1] as i64 * vector[1] as i64,
                matrix[row][2] as i64 * vector[2] as i64,
            ];
            self.mac_row(row + 1, (translation[row] as i64) << 12, terms, sf);
            self.set_ir(row + 1, self.mac[row + 1], lm);
        }
    }

    /// `IR = (matrix * vector + translation) >> sf`, the shared core of the
    /// lighting commands.
    fn transform(
        &mut self,
        matrix: [[i16; 3]; 3],
        vector: [i16; 3],
        translation: [i32; 3],
        sf: bool,
        lm: bool,
    ) {
        for row in 0..3 {
            let terms = [
                matrix[row][0] as i64 * vector[0] as i64,
                matrix[row][1] as i64 * vector[1] as i64,
                matrix[row][2] as i64 * vector[2] as i64,
            ];
            self.mac_row(row + 1, (translation[row] as i64) << 12, terms, sf);
            self.set_ir(row + 1, self.mac[row + 1], lm);
        }
    }

    /// Light a normal: local light, then light colour.
    fn light(&mut self, n: usize, sf: bool, lm: bool) {
        let v = self.v[n];
        self.transform(self.llm, v, [0, 0, 0], sf, lm);
        let ir = [self.ir[1], self.ir[2], self.ir[3]];
        self.transform(self.lcm, ir, self.bk, sf, lm);
    }

    /// `MAC = [R*IR1, G*IR2, B*IR3] SHL 4`, the colour modulation shared by
    /// the NCC, NCD, CC and DCPL family.
    ///
    /// Deliberately unshifted, and it writes no IR: `sf` belongs to the *next*
    /// step. For the depth-cued commands that next step is the interpolation
    /// toward the far colour, which needs the full unshifted product, so
    /// shifting here throws away twelve bits of the colour term before it is
    /// used. That is invisible on any channel whose colour byte is zero, which
    /// is why only the blue channel of one NCDS case exposed it.
    ///
    /// It cannot overflow the accumulator: an 8-bit colour times a 16-bit IR
    /// times sixteen is at most 28 bits, so there is no flag to raise.
    fn shade_by_rgbc(&mut self) {
        for n in 1..4 {
            self.mac[n] = self.rgbc[n - 1] as i32 * self.ir[n] as i32 * 16;
        }
    }

    /// `MAC = MAC SAR (sf*12)`, then `IR = MAC`. The tail of the commands that
    /// stop after the colour modulation instead of depth-cueing it.
    fn shift_mac_to_ir(&mut self, sf: bool, lm: bool) {
        for n in 1..4 {
            let v = self.mac[n] as i64;
            self.set_mac_and_ir(n, v, sf, lm);
        }
    }

    fn nc(&mut self, n: usize, sf: bool, lm: bool) {
        self.light(n, sf, lm);
        self.push_colour();
    }

    fn ncc(&mut self, n: usize, sf: bool, lm: bool) {
        self.light(n, sf, lm);
        self.shade_by_rgbc();
        self.shift_mac_to_ir(sf, lm);
        self.push_colour();
    }

    fn ncd(&mut self, n: usize, sf: bool, lm: bool) {
        self.light(n, sf, lm);
        self.shade_by_rgbc();
        self.depth_cue(sf, lm);
        self.push_colour();
    }

    /// Colour depth cue: the light-colour step, the vertex colour, then the
    /// fade toward the far colour. `CC` with a depth cue on the end, and not
    /// the same command as `DCPL` however similar the mnemonics look.
    fn cdp(&mut self, sf: bool, lm: bool) {
        let ir = [self.ir[1], self.ir[2], self.ir[3]];
        self.transform(self.lcm, ir, self.bk, sf, lm);
        self.shade_by_rgbc();
        self.depth_cue(sf, lm);
        self.push_colour();
    }

    fn cc(&mut self, sf: bool, lm: bool) {
        let ir = [self.ir[1], self.ir[2], self.ir[3]];
        self.transform(self.lcm, ir, self.bk, sf, lm);
        self.shade_by_rgbc();
        self.shift_mac_to_ir(sf, lm);
        self.push_colour();
    }

    fn dcpl(&mut self, sf: bool, lm: bool) {
        self.shade_by_rgbc();
        self.depth_cue(sf, lm);
        self.push_colour();
    }

    /// Blend the current colour toward the far colour by IR0.
    ///
    /// The intermediate goes through `IR` with `lm` **cleared**, whatever the
    /// command word says: the difference from the far colour is signed, and
    /// clamping it to non-negative here breaks the fade. Measured, not assumed:
    /// honouring `lm` on this write halves the suite, 400 cases to 200.
    fn depth_cue(&mut self, sf: bool, lm: bool) {
        let mac = [self.mac[1], self.mac[2], self.mac[3]];
        for n in 1..4 {
            let diff = ((self.fc[n - 1] as i64) << 12) - mac[n - 1] as i64;
            self.check_mac(n, diff);
            let shifted = if sf { diff >> 12 } else { diff };
            // Narrowed to 32 bits *before* saturating, and that is not a
            // convenience: `IR` saturates from the 32-bit `MAC` register, not
            // from the wider accumulator behind it. With `sf` clear there is no
            // shift, so a far colour near the top of its range gives a
            // difference past 32 bits, and hardware wraps it to a negative
            // value rather than clamping to +0x7FFF. Measured: saturating from
            // the full width instead costs half the suite, 400 cases to 200.
            self.set_ir(n, shifted as i32, false);

            let out = self.ir[n] as i64 * self.ir[0] as i64 + mac[n - 1] as i64;
            self.set_mac_and_ir(n, out, sf, lm);
        }
    }

    /// Depth-cue the vertex colour itself. `triple` runs it over the colour
    /// FIFO three times, which is what DPCT does.
    fn dpcs(&mut self, sf: bool, lm: bool, triple: bool) {
        for round in 0..if triple { 3 } else { 1 } {
            let source = if triple { self.rgb[0] } else { self.rgbc };
            let _ = round;
            for n in 1..4 {
                self.mac[n] = (source[n - 1] as i32) << 16;
            }
            self.depth_cue(sf, lm);
            self.push_colour();
        }
    }

    /// Interpolate between the current IR and the far colour.
    fn intpl(&mut self, sf: bool, lm: bool) {
        for n in 1..4 {
            self.mac[n] = (self.ir[n] as i32) << 12;
        }
        self.depth_cue(sf, lm);
        self.push_colour();
    }

    fn gpf(&mut self, sf: bool, lm: bool) {
        for n in 1..4 {
            let v = self.ir[0] as i64 * self.ir[n] as i64;
            self.set_mac_and_ir(n, v, sf, lm);
        }
        self.push_colour();
    }

    fn gpl(&mut self, sf: bool, lm: bool) {
        for n in 1..4 {
            let base = (self.mac[n] as i64) << (12 * sf as i64);
            let v = base + self.ir[0] as i64 * self.ir[n] as i64;
            self.set_mac_and_ir(n, v, sf, lm);
        }
        self.push_colour();
    }

    // ---- save state ------------------------------------------------------

    #[allow(clippy::type_complexity)]
    pub(crate) fn data_parts(
        &self,
    ) -> (
        [[i16; 3]; 3],
        [u8; 4],
        u16,
        [i16; 4],
        [(i16, i16); 3],
        [u16; 4],
        [[u8; 4]; 3],
        u32,
        [i32; 4],
        u32,
        u32,
    ) {
        (
            self.v, self.rgbc, self.otz, self.ir, self.sxy, self.sz, self.rgb, self.res1, self.mac,
            self.lzcs, self.lzcr,
        )
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn control_parts(
        &self,
    ) -> (
        [[i16; 3]; 3],
        [i32; 3],
        [[i16; 3]; 3],
        [i32; 3],
        [[i16; 3]; 3],
        [i32; 3],
        [i32; 2],
        u16,
        i16,
        i32,
        i16,
        i16,
        u32,
    ) {
        (
            self.rt, self.tr, self.llm, self.bk, self.lcm, self.fc, self.of, self.h, self.dqa,
            self.dqb, self.zsf3, self.zsf4, self.flag,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore_data(
        &mut self,
        v: [[i16; 3]; 3],
        rgbc: [u8; 4],
        otz: u16,
        ir: [i16; 4],
        sxy: [(i16, i16); 3],
        sz: [u16; 4],
        rgb: [[u8; 4]; 3],
        res1: u32,
        mac: [i32; 4],
        lzcs: u32,
        lzcr: u32,
    ) {
        self.v = v;
        self.rgbc = rgbc;
        self.otz = otz;
        self.ir = ir;
        self.sxy = sxy;
        self.sz = sz;
        self.rgb = rgb;
        self.res1 = res1;
        self.mac = mac;
        self.lzcs = lzcs;
        self.lzcr = lzcr;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore_control(
        &mut self,
        rt: [[i16; 3]; 3],
        tr: [i32; 3],
        llm: [[i16; 3]; 3],
        bk: [i32; 3],
        lcm: [[i16; 3]; 3],
        fc: [i32; 3],
        of: [i32; 2],
        h: u16,
        dqa: i16,
        dqb: i32,
        zsf3: i16,
        zsf4: i16,
        flag: u32,
    ) {
        self.rt = rt;
        self.tr = tr;
        self.llm = llm;
        self.bk = bk;
        self.lcm = lcm;
        self.fc = fc;
        self.of = of;
        self.h = h;
        self.dqa = dqa;
        self.dqb = dqb;
        self.zsf3 = zsf3;
        self.zsf4 = zsf4;
        self.flag = flag;
    }
}

#[inline]
fn pack_xy(x: i16, y: i16) -> u32 {
    (x as u16 as u32) | ((y as u16 as u32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_cop2r15_pushes_the_screen_fifo() {
        let mut g = Gte::new();
        g.write_data(15, pack_xy(1, 2));
        g.write_data(15, pack_xy(3, 4));
        g.write_data(15, pack_xy(5, 6));

        assert_eq!(g.read_data(12), pack_xy(1, 2), "oldest");
        assert_eq!(g.read_data(13), pack_xy(3, 4));
        assert_eq!(g.read_data(14), pack_xy(5, 6), "newest");
        assert_eq!(g.read_data(15), g.read_data(14), "r15 mirrors r14 on read");
    }

    #[test]
    fn lzcr_counts_leading_ones_for_negatives() {
        let mut g = Gte::new();
        g.write_data(30, 0x0000_0000);
        assert_eq!(g.read_data(31), 32);
        g.write_data(30, 0xFFFF_FFFF);
        assert_eq!(g.read_data(31), 32, "leading ones, not zeros");
        g.write_data(30, 0x0FFF_FFFF);
        assert_eq!(g.read_data(31), 4);
        g.write_data(30, 0x8000_0000);
        assert_eq!(g.read_data(31), 1);
    }

    #[test]
    fn irgb_round_trips_through_five_bit_channels() {
        let mut g = Gte::new();
        g.write_data(28, 0x1F | (0x10 << 5) | (0x01 << 10));
        assert_eq!(g.read_data(9), 0x1F * 0x80);
        assert_eq!(g.read_data(10), 0x10 * 0x80);
        assert_eq!(g.read_data(11), 0x80);
        assert_eq!(g.read_data(28), 0x1F | (0x10 << 5) | (0x01 << 10));
    }

    #[test]
    fn flag_bit_31_is_the_or_of_the_error_bits() {
        let mut g = Gte::new();
        g.write_control(31, 1 << 24); // IR1 saturated
        assert_ne!(g.read_control(31) & (1 << 31), 0);

        g.write_control(31, 1 << 12); // IR0 saturated is not an error bit
        assert_eq!(g.read_control(31) & (1 << 31), 0);
    }

    /// A vertex on the projection plane projects to the screen offset.
    #[test]
    fn rtps_projects_a_vertex() {
        let mut g = Gte::new();
        // Identity rotation at 1.0 in 4.12 fixed point.
        g.write_control(0, pack_xy(0x1000, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);
        // No translation, screen centre at (0, 0), projection distance 200.
        g.write_control(24, 0);
        g.write_control(25, 0);
        g.write_control(26, 200);

        // A point straight ahead at the projection distance.
        g.write_data(0, pack_xy(0, 0));
        g.write_data(1, 200);
        g.command(0x0018_0001); // RTPS, sf = 1

        assert_eq!(g.read_data(19), 200, "SZ3 should be the transformed Z");
        assert_eq!(g.read_data(14), pack_xy(0, 0), "should land at the centre");
    }

    /// Doubling the distance halves the projected offset, which is the whole
    /// job of the divider.
    #[test]
    fn rtps_projection_scales_with_depth() {
        let mut g = Gte::new();
        g.write_control(0, pack_xy(0x1000, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);
        g.write_control(26, 200); // H

        g.write_data(0, pack_xy(100, 0));
        g.write_data(1, 200);
        g.command(0x0018_0001);
        let near = g.read_data(14) as i16;

        g.write_data(0, pack_xy(100, 0));
        g.write_data(1, 400);
        g.command(0x0018_0001);
        let far = g.read_data(14) as i16;

        assert_eq!(near, 100, "at the projection distance, x maps one to one");
        assert_eq!(far, 50, "twice as far should be half as wide");
    }

    /// A vertex at or behind the eye saturates the divider and flags it, which
    /// is how software knows to reject the polygon.
    #[test]
    fn a_vertex_too_close_sets_the_divide_flag() {
        let mut g = Gte::new();
        g.write_control(0, pack_xy(0x1000, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);
        g.write_control(26, 1000); // H far beyond 2 * SZ3

        g.write_data(0, pack_xy(0, 0));
        g.write_data(1, 100);
        g.command(0x0018_0001);

        assert_ne!(g.flag & (1 << F_DIVIDE), 0, "divide overflow not flagged");
        assert_ne!(g.flag & (1 << 31), 0, "master error bit not set");
    }

    #[test]
    fn nclip_sign_tells_the_winding() {
        let mut g = Gte::new();
        g.write_data(12, pack_xy(0, 0));
        g.write_data(13, pack_xy(10, 0));
        g.write_data(14, pack_xy(0, 10));
        g.command(0x0000_0006);
        let clockwise = g.read_data(24) as i32;

        g.write_data(12, pack_xy(0, 0));
        g.write_data(13, pack_xy(0, 10));
        g.write_data(14, pack_xy(10, 0));
        g.command(0x0000_0006);
        let anticlockwise = g.read_data(24) as i32;

        assert_eq!(clockwise, -anticlockwise);
        assert_ne!(clockwise, 0);
    }

    #[test]
    fn avsz3_averages_the_z_fifo() {
        let mut g = Gte::new();
        g.write_data(17, 100);
        g.write_data(18, 200);
        g.write_data(19, 300);
        g.write_control(29, 0x1000 / 3); // ZSF3 = 1/3 in 4.12
        g.command(0x0000_002D);

        // (100 + 200 + 300) / 3, give or take the fixed-point rounding.
        let otz = g.read_data(7);
        assert!((198..=200).contains(&otz), "OTZ was {otz}");
    }

    #[test]
    fn sqr_squares_the_ir_vector() {
        let mut g = Gte::new();
        g.write_data(9, 4);
        g.write_data(10, 5);
        g.write_data(11, 6);
        g.command(0x0000_0028); // SQR, sf = 0

        assert_eq!(g.read_data(25), 16);
        assert_eq!(g.read_data(26), 25);
        assert_eq!(g.read_data(27), 36);
    }

    #[test]
    fn mvmva_multiplies_by_the_chosen_matrix() {
        let mut g = Gte::new();
        // Identity rotation, translation (1, 2, 3).
        g.write_control(0, pack_xy(0x1000, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);
        g.write_control(5, 1);
        g.write_control(6, 2);
        g.write_control(7, 3);

        g.write_data(0, pack_xy(10, 20));
        g.write_data(1, 30);
        // MVMVA, sf = 1, matrix 0 (RT), vector 0 (V0), translation 0 (TR).
        g.command(0x0008_0012);

        assert_eq!(g.read_data(9) as i16, 11);
        assert_eq!(g.read_data(10) as i16, 22);
        assert_eq!(g.read_data(11) as i16, 33);
    }

    #[test]
    fn ir_saturation_flags_and_clamps() {
        let mut g = Gte::new();
        g.write_control(0, pack_xy(0x7FFF, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);

        g.write_data(0, pack_xy(0x7FFF, 0));
        g.write_data(1, 0);
        g.command(0x0008_0012); // MVMVA

        assert_eq!(g.read_data(9) as i16, 0x7FFF, "IR1 should have clamped");
        assert_ne!(g.flag & (1 << 24), 0, "IR1 saturation not flagged");
    }

    /// `lm` clamps IR to non-negative, which lighting relies on.
    #[test]
    fn the_lm_bit_clamps_ir_to_non_negative() {
        let mut g = Gte::new();
        g.write_control(0, pack_xy(-0x1000i32 as i16, 0));
        g.write_control(1, pack_xy(0, 0));
        g.write_control(2, pack_xy(0x1000, 0));
        g.write_control(3, pack_xy(0, 0));
        g.write_control(4, 0x1000);
        g.write_data(0, pack_xy(100, 0));
        g.write_data(1, 0);

        g.command(0x0008_0012); // lm = 0
        assert_eq!(g.read_data(9) as i16, -100);

        g.command(0x0008_0412); // lm = 1
        assert_eq!(g.read_data(9) as i16, 0);
    }

    #[test]
    fn a_command_clears_the_flags_first() {
        let mut g = Gte::new();
        g.write_control(31, 1 << 24);
        assert_ne!(g.flag, 0);
        g.command(0x0000_0006); // NCLIP, which cannot overflow here
        assert_eq!(g.flag, 0, "stale flags survived into the next command");
    }

    #[test]
    fn an_unknown_command_is_counted_not_executed() {
        let mut g = Gte::new();
        g.command(0x0000_0000);
        assert_eq!(g.unknown_commands, 1);
    }

    // ---- what gte/test-all settled --------------------------------------
    //
    // Each of these was a real failure in the suite, and each expected value
    // below differs from what the previous implementation produced, so none of
    // them can pass by accident. The distinguishing old value is named.

    #[test]
    fn sz3_comes_from_the_wrapped_44_bit_accumulator() {
        let mut g = Gte::new();
        // RT31 = 1, everything else zero, so only row 3 has a term.
        g.write_control(3, 1);
        g.write_control(7, 0x7FFF_FFFF); // TRZ, whose shift lands just under 2^43
        g.write_data(0, pack_xy(8192, 0)); // V0.x, enough to tip the row over
        g.write_data(1, 0);
        g.command(0x0008_0001); // RTPS, sf = 1

        assert_ne!(g.flag & (1 << F_MAC_POS[2]), 0, "MAC3 overflowed positive");
        // The accumulator wraps to a large negative, so the depth saturates to
        // the near clip rather than the far one. Without the wrap it is 0xFFFF.
        assert_eq!(g.read_data(19), 0, "SZ3");
        assert_ne!(g.flag & (1 << F_SZ3), 0, "and says it saturated");
    }

    #[test]
    fn a_row_can_overflow_both_ways_in_one_command() {
        let mut g = Gte::new();
        // Row 1 = (1, 1, 0) against V0 = (8192, -8192, 0): the running total
        // goes over the top on the first term and under the bottom on the
        // second, and lands back exactly where it started.
        g.write_control(0, pack_xy(1, 1));
        g.write_control(5, 0x7FFF_FFFF); // TRX
        g.write_data(0, pack_xy(8192, -8192));
        g.write_data(1, 0);
        g.command(0x0008_0001); // RTPS, sf = 1

        // Summing the row as one expression sees neither: the total is in range.
        assert_ne!(g.flag & (1 << F_MAC_POS[0]), 0, "MAC1 overflowed positive");
        assert_ne!(
            g.flag & (1 << F_MAC_NEG[0]),
            0,
            "and negative, same command"
        );
    }

    #[test]
    fn cdp_is_not_dcpl() {
        // Both end in a depth cue, but CDP runs the light-colour step first, so
        // the colour it modulates is not the one already in IR.
        let setup = |g: &mut Gte| {
            g.write_control(13, 1); // BK1 = 1
            g.write_control(16, 1); // LR1 = 1, rest of the light matrix zero
            g.write_data(6, 0x03); // RGBC, R = 3
            g.write_data(9, 10); // IR1
        };

        let mut cdp = Gte::new();
        setup(&mut cdp);
        cdp.command(0x0000_0014);
        // BK1 shifts IR1 to 4106 before the colour multiply: 3 * 4106 * 16.
        assert_eq!(cdp.read_data(25), 197_088, "CDP MAC1");

        let mut dcpl = Gte::new();
        setup(&mut dcpl);
        dcpl.command(0x0000_0029);
        // DCPL modulates IR1 as it stands: 3 * 10 * 16.
        assert_eq!(dcpl.read_data(25), 480, "DCPL MAC1");
    }

    #[test]
    fn otz_comes_from_the_full_precision_product() {
        let mut g = Gte::new();
        g.write_control(30, 0xFFFF_8000); // ZSF4 = -32768
        for r in 16..20 {
            g.write_data(r, 0xFFFF); // SZ0..SZ3 all at the top
        }
        g.command(0x0000_002E); // AVSZ4

        // The product needs 34 bits, so MAC0 keeps a positive remnant of a
        // negative number. Reading OTZ back out of it gives 32.
        assert_eq!(g.read_data(24), 131_072, "MAC0, truncated as hardware does");
        assert_eq!(g.read_data(7), 0, "OTZ, saturated from the wide value");
    }

    #[test]
    fn the_far_colour_difference_narrows_before_it_saturates() {
        let mut g = Gte::new();
        g.write_control(21, 0x7FFF_FFFF); // FC red, near the top of its range
        g.write_data(6, 0x01); // RGBC, R = 1
        g.write_data(8, 1); // IR0
        g.command(0x0000_0010); // DPCS, sf = 0

        // With no shift the difference is wider than 32 bits, and the narrowing
        // makes it negative: IR saturates to -0x8000, not +0x7FFF. Saturating
        // from the full width instead gives 98_303 here.
        assert_eq!(g.read_data(25), 32_768, "MAC1");
    }

    #[test]
    fn the_reserved_matrix_rows_come_from_the_rotation_matrix() {
        let mut g = Gte::new();
        g.write_control(1, 3); // RT13 = 3
        g.write_control(2, 5); // RT22 = 5
        g.write_data(0, pack_xy(1, 1)); // V0 = (1, 1, 1)
        g.write_data(1, 1);
        // MVMVA, mx = 3 (reserved), vx = 0, tx = 3 (none), sf = 0.
        g.command(0x0006_6012);

        // Row 1 is the colour register and IR0, all zero here. Rows 2 and 3 are
        // RT13 and RT22 repeated, which used to be IR0 repeated: 0 and 0.
        assert_eq!(g.read_data(25), 0, "MAC1");
        assert_eq!(g.read_data(26), 9, "MAC2 = RT13 * (1 + 1 + 1)");
        assert_eq!(g.read_data(27), 15, "MAC3 = RT22 * (1 + 1 + 1)");
    }

    #[test]
    fn the_colour_multiply_is_not_scaled_by_sf() {
        let mut g = Gte::new();
        g.write_data(6, 0x01); // RGBC, R = 1
        g.write_data(9, 4096); // IR1
        g.command(0x0008_0029); // DCPL, sf = 1

        // 1 * 4096 * 16, shifted once at the end and not twice. Applying sf to
        // the colour multiply as well leaves 0.
        assert_eq!(g.read_data(25), 16, "MAC1");
    }
}
