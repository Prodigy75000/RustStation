// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A small R3000A assembler, for the few kernel routines that have to be real
//! guest code: the exception vector and handler, `ReturnFromException`, and
//! the loops that call back into the game.
//!
//! Only what those routines use is here. Branch targets are labels, resolved
//! when the routine is finished; everything else is encoded on the spot.

use std::collections::HashMap;

pub const ZERO: u32 = 0;
pub const AT: u32 = 1;
pub const V0: u32 = 2;
pub const V1: u32 = 3;
pub const A0: u32 = 4;
pub const A1: u32 = 5;
pub const A2: u32 = 6;
pub const A3: u32 = 7;
pub const T0: u32 = 8;
pub const T1: u32 = 9;
pub const T2: u32 = 10;
pub const T3: u32 = 11;
pub const S0: u32 = 16;
pub const S1: u32 = 17;
pub const S2: u32 = 18;
pub const K0: u32 = 26;
pub const K1: u32 = 27;
pub const GP: u32 = 28;
pub const SP: u32 = 29;
pub const FP: u32 = 30;
pub const RA: u32 = 31;

/// COP0 registers the kernel touches.
pub const C0_SR: u32 = 12;
pub const C0_CAUSE: u32 = 13;
pub const C0_EPC: u32 = 14;

enum Fixup {
    /// A 16-bit branch offset, relative to the delay slot.
    Branch,
}

/// A routine being assembled at a fixed address.
pub struct Asm {
    pub base: u32,
    pub words: Vec<u32>,
    labels: HashMap<&'static str, u32>,
    fixups: Vec<(usize, &'static str, Fixup)>,
}

fn r(op: u32, rs: u32, rt: u32, rd: u32, sa: u32, funct: u32) -> u32 {
    op << 26 | rs << 21 | rt << 16 | rd << 11 | sa << 6 | funct
}

fn i(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
    op << 26 | rs << 21 | rt << 16 | (imm & 0xFFFF)
}

/// The two halves of an address for `lui`/`addiu`, the upper one adjusted
/// for the sign of the lower.
pub fn hi_lo(addr: u32) -> (u32, u32) {
    let lo = addr & 0xFFFF;
    let hi = (addr >> 16).wrapping_add((lo >> 15) & 1) & 0xFFFF;
    (hi, lo)
}

impl Asm {
    pub fn new(base: u32) -> Asm {
        Asm {
            base,
            words: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    /// Address of the next instruction.
    pub fn here(&self) -> u32 {
        self.base.wrapping_add(self.words.len() as u32 * 4)
    }

    pub fn label(&mut self, name: &'static str) {
        let at = self.here();
        assert!(
            self.labels.insert(name, at).is_none(),
            "label {name} defined twice"
        );
    }

    pub fn addr_of(&self, name: &str) -> u32 {
        self.labels[name]
    }

    pub fn word(&mut self, w: u32) {
        self.words.push(w);
    }

    /// Pad with `nop` up to `offset` bytes from the start.
    pub fn pad_to(&mut self, offset: u32) {
        assert!(
            self.words.len() as u32 * 4 <= offset,
            "routine overran {offset:#X}"
        );
        while (self.words.len() as u32 * 4) < offset {
            self.nop();
        }
    }

    /// Resolve the labels and hand back the words.
    pub fn finish(mut self) -> Vec<u32> {
        for (at, name, kind) in std::mem::take(&mut self.fixups) {
            let target = *self
                .labels
                .get(name)
                .unwrap_or_else(|| panic!("undefined label {name}"));
            match kind {
                Fixup::Branch => {
                    let slot = self.base.wrapping_add(at as u32 * 4 + 4);
                    let off = (target.wrapping_sub(slot) as i32) >> 2;
                    assert!(
                        (-0x8000..0x8000).contains(&off),
                        "branch to {name} out of range"
                    );
                    self.words[at] |= off as u32 & 0xFFFF;
                }
            }
        }
        self.words
    }

    pub fn nop(&mut self) {
        self.word(0);
    }
    pub fn lui(&mut self, rt: u32, imm: u32) {
        self.word(i(0x0F, 0, rt, imm));
    }
    pub fn ori(&mut self, rt: u32, rs: u32, imm: u32) {
        self.word(i(0x0D, rs, rt, imm));
    }
    pub fn sltiu(&mut self, rt: u32, rs: u32, imm: u32) {
        self.word(i(0x0B, rs, rt, imm));
    }
    pub fn andi(&mut self, rt: u32, rs: u32, imm: u32) {
        self.word(i(0x0C, rs, rt, imm));
    }
    pub fn addiu(&mut self, rt: u32, rs: u32, imm: i32) {
        self.word(i(0x09, rs, rt, imm as u32));
    }
    /// The trapping add, as the BIOS's own prologue writes it.
    pub fn addi(&mut self, rt: u32, rs: u32, imm: i32) {
        self.word(i(0x08, rs, rt, imm as u32));
    }
    /// The trapping register add, which the kernel's dispatchers use.
    pub fn add(&mut self, rd: u32, rs: u32, rt: u32) {
        self.word(r(0, rs, rt, rd, 0, 0x20));
    }
    pub fn addu(&mut self, rd: u32, rs: u32, rt: u32) {
        self.word(r(0, rs, rt, rd, 0, 0x21));
    }
    pub fn or(&mut self, rd: u32, rs: u32, rt: u32) {
        self.word(r(0, rs, rt, rd, 0, 0x25));
    }
    pub fn mov(&mut self, rd: u32, rs: u32) {
        self.addu(rd, rs, ZERO);
    }
    pub fn sll(&mut self, rd: u32, rt: u32, sa: u32) {
        self.word(r(0, 0, rt, rd, sa, 0x00));
    }
    pub fn lw(&mut self, rt: u32, off: i32, base: u32) {
        self.word(i(0x23, base, rt, off as u32));
    }
    pub fn sw(&mut self, rt: u32, off: i32, base: u32) {
        self.word(i(0x2B, base, rt, off as u32));
    }
    pub fn mfhi(&mut self, rd: u32) {
        self.word(r(0, 0, 0, rd, 0, 0x10));
    }
    pub fn mthi(&mut self, rs: u32) {
        self.word(r(0, rs, 0, 0, 0, 0x11));
    }
    pub fn mflo(&mut self, rd: u32) {
        self.word(r(0, 0, 0, rd, 0, 0x12));
    }
    pub fn mtlo(&mut self, rs: u32) {
        self.word(r(0, rs, 0, 0, 0, 0x13));
    }
    pub fn jr(&mut self, rs: u32) {
        self.word(r(0, rs, 0, 0, 0, 0x08));
    }
    pub fn jalr(&mut self, rs: u32) {
        self.word(r(0, rs, 0, RA, 0, 0x09));
    }
    pub fn syscall(&mut self) {
        self.word(0x0000_000C);
    }
    /// `j target`, which must share the top four address bits with the slot.
    pub fn j(&mut self, target: u32) {
        assert_eq!(
            target & 0xF000_0000,
            self.here() & 0xF000_0000,
            "j out of segment"
        );
        self.word(0x02 << 26 | (target >> 2 & 0x03FF_FFFF));
    }
    pub fn jal(&mut self, target: u32) {
        assert_eq!(
            target & 0xF000_0000,
            self.here() & 0xF000_0000,
            "jal out of segment"
        );
        self.word(0x03 << 26 | (target >> 2 & 0x03FF_FFFF));
    }
    fn branch(&mut self, word: u32, label: &'static str) {
        self.fixups.push((self.words.len(), label, Fixup::Branch));
        self.word(word);
    }
    pub fn beq(&mut self, rs: u32, rt: u32, label: &'static str) {
        self.branch(i(0x04, rs, rt, 0), label);
    }
    pub fn bne(&mut self, rs: u32, rt: u32, label: &'static str) {
        self.branch(i(0x05, rs, rt, 0), label);
    }
    pub fn b(&mut self, label: &'static str) {
        self.beq(ZERO, ZERO, label);
    }
    pub fn mfc0(&mut self, rt: u32, rd: u32) {
        self.word(r(0x10, 0x00, rt, rd, 0, 0));
    }
    pub fn mtc0(&mut self, rt: u32, rd: u32) {
        self.word(r(0x10, 0x04, rt, rd, 0, 0));
    }
    pub fn rfe(&mut self) {
        self.word(0x4200_0010);
    }
    /// Load a full 32-bit constant.
    pub fn li(&mut self, rt: u32, val: u32) {
        let (hi, lo) = hi_lo(val);
        self.lui(rt, hi);
        self.addiu(rt, rt, lo as u16 as i16 as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodings checked against the opcode tables in psx-spx "CPU Opcode
    /// Encoding", and against the words psx-spx quotes from the BIOS
    /// exception vector and from games' kernel patches.
    #[test]
    fn encodings_match_the_documented_words() {
        let mut a = Asm::new(0x0000_0080);
        a.lui(K0, 0);
        a.addiu(K0, K0, 0x0C80);
        a.jr(K0);
        a.nop();
        a.sw(1, 4, K0);
        a.sw(RA, 0x7C, K0);
        a.mfc0(V0, C0_CAUSE);
        a.mfc0(V1, C0_EPC);
        a.addi(K0, K0, 8);
        a.lw(K0, 8, K0);
        a.jalr(T2);
        a.addiu(T1, ZERO, 0x56);
        a.rfe();
        assert_eq!(
            a.finish(),
            vec![
                0x3C1A_0000,
                0x275A_0C80,
                0x0340_0008,
                0x0000_0000,
                0xAF41_0004,
                0xAF5F_007C,
                0x4002_6800,
                0x4003_7000,
                0x235A_0008,
                0x8F5A_0008,
                0x0140_F809,
                0x2409_0056,
                0x4200_0010,
            ]
        );
    }

    #[test]
    fn branches_count_from_the_delay_slot() {
        let mut a = Asm::new(0x1000);
        a.label("top");
        a.nop();
        a.bne(T0, T1, "top");
        a.nop();
        a.beq(ZERO, ZERO, "end");
        a.nop();
        a.nop();
        a.label("end");
        let w = a.finish();
        // Back two words from the slot at 0x1008 to 0x1000.
        assert_eq!(w[1], 0x1509_FFFE);
        // Forward two words from the slot at 0x1010 to 0x1018.
        assert_eq!(w[3], 0x1000_0002);
    }

    #[test]
    fn li_carries_into_the_upper_half_when_the_lower_is_negative() {
        let mut a = Asm::new(0);
        a.li(T0, 0x8001_8000);
        let w = a.finish();
        assert_eq!(w, vec![0x3C08_8002, 0x2508_8000]);
    }
}
