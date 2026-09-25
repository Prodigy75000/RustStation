// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A very small MIPS assembler, shared by the integration tests.
//!
//! Programs are written into a synthetic BIOS image, so no copyrighted dump is
//! needed to test CPU or timing behaviour.

#![allow(dead_code)]

use psx_core::{bus, Psx};

pub fn i_type(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF)
}

pub fn r_type(rs: u32, rt: u32, rd: u32, shamt: u32, funct: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (shamt << 6) | funct
}

pub fn nop() -> u32 {
    0
}
pub fn addiu(rt: u32, rs: u32, imm: i32) -> u32 {
    i_type(0x09, rs, rt, imm as u32)
}
pub fn addi(rt: u32, rs: u32, imm: i32) -> u32 {
    i_type(0x08, rs, rt, imm as u32)
}
pub fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
    i_type(0x0D, rs, rt, imm)
}
pub fn lui(rt: u32, imm: u32) -> u32 {
    i_type(0x0F, 0, rt, imm)
}
pub fn lw(rt: u32, off: i32, rs: u32) -> u32 {
    i_type(0x23, rs, rt, off as u32)
}
pub fn lwl(rt: u32, off: i32, rs: u32) -> u32 {
    i_type(0x22, rs, rt, off as u32)
}
pub fn lwr(rt: u32, off: i32, rs: u32) -> u32 {
    i_type(0x26, rs, rt, off as u32)
}
pub fn sw(rt: u32, off: i32, rs: u32) -> u32 {
    i_type(0x2B, rs, rt, off as u32)
}
pub fn beq(rs: u32, rt: u32, off: i32) -> u32 {
    i_type(0x04, rs, rt, off as u32)
}
pub fn jal(target: u32) -> u32 {
    (0x03 << 26) | ((target & 0x0FFF_FFFF) >> 2)
}
pub fn mtc0(rt: u32, rd: u32) -> u32 {
    (0x10 << 26) | (0x04 << 21) | (rt << 16) | (rd << 11)
}
pub fn mfc0(rt: u32, rd: u32) -> u32 {
    (0x10 << 26) | (rt << 16) | (rd << 11)
}
pub fn div(rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, 0, 0, 0x1A)
}
pub fn mfhi(rd: u32) -> u32 {
    r_type(0, 0, rd, 0, 0x10)
}
pub fn mflo(rd: u32) -> u32 {
    r_type(0, 0, rd, 0, 0x12)
}

pub const RESET: u32 = 0xBFC0_0000;
/// Where an exception vectors with Status BEV set, which is the reset default.
pub const BEV_HANDLER: u32 = 0xBFC0_0180;

/// Run `n` instructions, however many cycles they take. `Psx::run` counts
/// cycles, and an instruction fetched from the ROM costs 29 of them.
pub fn steps(psx: &mut Psx, n: u64) {
    for _ in 0..n {
        psx.step();
    }
}

pub fn jr(rs: u32) -> u32 {
    r_type(rs, 0, 0, 0, 0x08)
}

/// A machine that turns the I-cache on, as a BIOS does, and runs `program`
/// from the ROM's cached mirror, so a loop in it costs a cycle an
/// instruction once its first pass has filled the cache. From the ROM
/// itself every instruction costs 29, which hides a cycle-exact event.
pub fn cached(program: &[u32]) -> Psx {
    const PROLOGUE: u32 = 9;
    let mut words = vec![
        lui(1, 0xFFFE),
        ori(1, 1, 0x0130),
        lui(2, 0x0001),
        ori(2, 2, 0xE988),
        sw(2, 0, 1),
        lui(3, 0x9FC0),
        ori(3, 3, PROLOGUE * 4),
        jr(3),
        nop(),
    ];
    assert_eq!(words.len() as u32, PROLOGUE);
    words.extend_from_slice(program);
    machine(&words)
}

/// A machine whose BIOS is the given program, starting at the reset vector.
pub fn machine(program: &[u32]) -> Psx {
    let mut bios = vec![0u8; bus::BIOS_SIZE];
    for (i, word) in program.iter().enumerate() {
        bios[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    Psx::new(bios).expect("synthetic BIOS is the right size")
}
