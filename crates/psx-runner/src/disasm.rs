// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A one-instruction MIPS-I disassembler, for reading the machine's own memory.
//!
//! This exists so that an address out of `--pchist` can be turned into
//! something readable without leaving the repository. Hand-decoding six words
//! is easy once; hand-decoding them every time a hang moves is how a decoding
//! slip gets mistaken for an emulation bug.
//!
//! Deliberately **not** shared with the CPU's own decoder. The interpreter's
//! job is to be right about semantics and fast; this one's job is to be legible
//! and to never panic on a word that is not an instruction at all, because it
//! will routinely be pointed at data. Two readings of the same encoding that
//! must agree are also a small, free cross-check: a mistake in one is unlikely
//! to be mirrored in the other.

/// Register names as the R3000A's assembly conventions use them.
const REG: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp",
    "ra",
];

fn r(n: u32) -> &'static str {
    REG[(n & 31) as usize]
}

/// The assembly name of a register, so a dump and a disassembly agree.
pub fn reg_name(n: u32) -> &'static str {
    r(n)
}

/// Sign-extended 16-bit immediate, printed the way it would be written.
fn imm(word: u32) -> String {
    let v = word as i16;
    if v < 0 {
        format!("-0x{:X}", -(v as i32))
    } else {
        format!("0x{v:X}")
    }
}

/// Disassemble one instruction. `pc` is only needed to resolve branch and jump
/// targets to absolute addresses, which is the whole point of reading these.
pub fn disasm(word: u32, pc: u32) -> String {
    let op = word >> 26;
    let rs = (word >> 21) & 31;
    let rt = (word >> 16) & 31;
    let rd = (word >> 11) & 31;
    let sa = (word >> 6) & 31;
    let funct = word & 63;
    // A branch displacement counts from the delay slot, not from the branch.
    let target = pc.wrapping_add(4).wrapping_add(((word as i16 as i32) << 2) as u32);
    let jump = (pc.wrapping_add(4) & 0xF000_0000) | ((word & 0x03FF_FFFF) << 2);

    match op {
        0x00 => match funct {
            0x00 if word == 0 => "nop".to_string(),
            0x00 => format!("sll {}, {}, {sa}", r(rd), r(rt)),
            0x02 => format!("srl {}, {}, {sa}", r(rd), r(rt)),
            0x03 => format!("sra {}, {}, {sa}", r(rd), r(rt)),
            0x04 => format!("sllv {}, {}, {}", r(rd), r(rt), r(rs)),
            0x06 => format!("srlv {}, {}, {}", r(rd), r(rt), r(rs)),
            0x07 => format!("srav {}, {}, {}", r(rd), r(rt), r(rs)),
            0x08 => format!("jr {}", r(rs)),
            0x09 => format!("jalr {}, {}", r(rd), r(rs)),
            0x0C => format!("syscall 0x{:X}", (word >> 6) & 0xF_FFFF),
            0x0D => format!("break 0x{:X}", (word >> 6) & 0xF_FFFF),
            0x10 => format!("mfhi {}", r(rd)),
            0x11 => format!("mthi {}", r(rs)),
            0x12 => format!("mflo {}", r(rd)),
            0x13 => format!("mtlo {}", r(rs)),
            0x18 => format!("mult {}, {}", r(rs), r(rt)),
            0x19 => format!("multu {}, {}", r(rs), r(rt)),
            0x1A => format!("div {}, {}", r(rs), r(rt)),
            0x1B => format!("divu {}, {}", r(rs), r(rt)),
            0x20 => format!("add {}, {}, {}", r(rd), r(rs), r(rt)),
            0x21 => format!("addu {}, {}, {}", r(rd), r(rs), r(rt)),
            0x22 => format!("sub {}, {}, {}", r(rd), r(rs), r(rt)),
            0x23 => format!("subu {}, {}, {}", r(rd), r(rs), r(rt)),
            0x24 => format!("and {}, {}, {}", r(rd), r(rs), r(rt)),
            0x25 => format!("or {}, {}, {}", r(rd), r(rs), r(rt)),
            0x26 => format!("xor {}, {}, {}", r(rd), r(rs), r(rt)),
            0x27 => format!("nor {}, {}, {}", r(rd), r(rs), r(rt)),
            0x2A => format!("slt {}, {}, {}", r(rd), r(rs), r(rt)),
            0x2B => format!("sltu {}, {}, {}", r(rd), r(rs), r(rt)),
            _ => format!(".word 0x{word:08X}"),
        },
        0x01 => match rt {
            0x00 => format!("bltz {}, {target:08X}", r(rs)),
            0x01 => format!("bgez {}, {target:08X}", r(rs)),
            0x10 => format!("bltzal {}, {target:08X}", r(rs)),
            0x11 => format!("bgezal {}, {target:08X}", r(rs)),
            _ => format!(".word 0x{word:08X}"),
        },
        0x02 => format!("j {jump:08X}"),
        0x03 => format!("jal {jump:08X}"),
        0x04 if rt == 0 && rs == 0 => format!("b {target:08X}"),
        0x04 => format!("beq {}, {}, {target:08X}", r(rs), r(rt)),
        0x05 => format!("bne {}, {}, {target:08X}", r(rs), r(rt)),
        0x06 => format!("blez {}, {target:08X}", r(rs)),
        0x07 => format!("bgtz {}, {target:08X}", r(rs)),
        0x08 => format!("addi {}, {}, {}", r(rt), r(rs), imm(word)),
        0x09 => format!("addiu {}, {}, {}", r(rt), r(rs), imm(word)),
        0x0A => format!("slti {}, {}, {}", r(rt), r(rs), imm(word)),
        0x0B => format!("sltiu {}, {}, {}", r(rt), r(rs), imm(word)),
        0x0C => format!("andi {}, {}, 0x{:X}", r(rt), r(rs), word & 0xFFFF),
        0x0D => format!("ori {}, {}, 0x{:X}", r(rt), r(rs), word & 0xFFFF),
        0x0E => format!("xori {}, {}, 0x{:X}", r(rt), r(rs), word & 0xFFFF),
        0x0F => format!("lui {}, 0x{:X}", r(rt), word & 0xFFFF),
        0x10..=0x13 => cop(op - 0x10, word, rs, rt, rd),
        0x20 => format!("lb {}, {}({})", r(rt), imm(word), r(rs)),
        0x21 => format!("lh {}, {}({})", r(rt), imm(word), r(rs)),
        0x22 => format!("lwl {}, {}({})", r(rt), imm(word), r(rs)),
        0x23 => format!("lw {}, {}({})", r(rt), imm(word), r(rs)),
        0x24 => format!("lbu {}, {}({})", r(rt), imm(word), r(rs)),
        0x25 => format!("lhu {}, {}({})", r(rt), imm(word), r(rs)),
        0x26 => format!("lwr {}, {}({})", r(rt), imm(word), r(rs)),
        0x28 => format!("sb {}, {}({})", r(rt), imm(word), r(rs)),
        0x29 => format!("sh {}, {}({})", r(rt), imm(word), r(rs)),
        0x2A => format!("swl {}, {}({})", r(rt), imm(word), r(rs)),
        0x2B => format!("sw {}, {}({})", r(rt), imm(word), r(rs)),
        0x2E => format!("swr {}, {}({})", r(rt), imm(word), r(rs)),
        0x32 => format!("lwc2 cpr{rt}, {}({})", imm(word), r(rs)),
        0x3A => format!("swc2 cpr{rt}, {}({})", imm(word), r(rs)),
        _ => format!(".word 0x{word:08X}"),
    }
}

fn cop(n: u32, word: u32, rs: u32, rt: u32, rd: u32) -> String {
    match rs {
        0x00 => format!("mfc{n} {}, cpr{rd}", r(rt)),
        0x02 => format!("cfc{n} {}, ccr{rd}", r(rt)),
        0x04 => format!("mtc{n} {}, cpr{rd}", r(rt)),
        0x06 => format!("ctc{n} {}, ccr{rd}", r(rt)),
        0x10 if n == 0 && (word & 63) == 0x10 => "rfe".to_string(),
        _ if n == 2 => format!("gte 0x{:07X}", word & 0x1FF_FFFF),
        _ => format!("cop{n} 0x{:07X}", word & 0x1FF_FFFF),
    }
}
