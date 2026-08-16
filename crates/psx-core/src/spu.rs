// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The SPU: its register file and its sound RAM. **No audio.**
//!
//! Nothing here produces a sample. What it does is let software *drive* the
//! SPU: the registers read back what was written, `SPUSTAT` follows `SPUCNT`,
//! and sound RAM can be filled through the transfer port or DMA channel 4.
//!
//! That sounds like a stub, and it is, but it is the difference between a game
//! booting and not booting. Crash Bandicoot writes `SPUCNT` and then polls it
//! until its own value comes back; against a register that always reads zero it
//! spins there forever. With read-back it goes from 473 sectors loaded and 600
//! primitives drawn to its title screen.
//!
//! The lesson generalises, and is worth stating plainly: **a register that
//! reads back what was written is not emulation, but a register that always
//! reads zero is a hang.** Software polls what it writes.

/// Registers, `0x1F801C00` to `0x1F801E80`. 640 bytes, addressed as 16-bit.
pub const REG_BYTES: usize = 640;
const REGS: usize = REG_BYTES / 2;

/// Sound RAM. Half a megabyte, and every address wraps into it.
pub const RAM_BYTES: usize = 512 * 1024;

/// Register offsets that behave as something other than plain storage.
const SPUCNT: usize = 0x1AA;
const SPUSTAT: usize = 0x1AE;
const TRANSFER_ADDR: usize = 0x1A6;
const TRANSFER_FIFO: usize = 0x1A8;

#[derive(Clone)]
pub struct Spu {
    regs: [u16; REGS],
    pub ram: Vec<u8>,
    /// Where the next transfer writes, in bytes. The register holds this in
    /// 8-byte units, which is the sort of factor that silently works when
    /// everything you test writes to address zero.
    transfer_addr: u32,

    /// Bytes pushed into sound RAM, by either route. Host-side observation
    /// only, never serialized.
    pub bytes_written: u64,
}

impl Default for Spu {
    fn default() -> Spu {
        Spu::new()
    }
}

impl Spu {
    pub fn new() -> Spu {
        Spu {
            regs: [0; REGS],
            ram: vec![0; RAM_BYTES],
            transfer_addr: 0,
            bytes_written: 0,
        }
    }

    /// `offset` is relative to `0x1F801C00`.
    pub fn read(&self, offset: u32, width: u32) -> u32 {
        let o = (offset & !1) as usize;
        let lo = self.reg(o) as u32;
        if width == 4 {
            lo | ((self.reg(o + 2) as u32) << 16)
        } else if width == 1 {
            (lo >> (8 * (offset & 1))) & 0xFF
        } else {
            lo
        }
    }

    fn reg(&self, o: usize) -> u16 {
        match o {
            // Status follows control. The low six bits are the same bits, and
            // software waits on them; the transfer-busy bits stay clear because
            // transfers here complete inside the write that starts them.
            SPUSTAT => self.regs[SPUCNT / 2] & 0x3F,
            _ => self.regs.get(o / 2).copied().unwrap_or(0),
        }
    }

    /// `offset` is relative to `0x1F801C00`.
    pub fn write(&mut self, offset: u32, width: u32, val: u32) {
        let o = (offset & !1) as usize;
        if width == 4 {
            self.write16(o, val as u16);
            self.write16(o + 2, (val >> 16) as u16);
        } else if width == 1 {
            // A byte write leaves the other half of the halfword alone.
            let mut v = self.reg(o);
            let shift = 8 * (offset & 1);
            v = (v & !(0xFF << shift)) | (((val & 0xFF) as u16) << shift);
            self.write16(o, v);
        } else {
            self.write16(o, val as u16);
        }
    }

    fn write16(&mut self, o: usize, val: u16) {
        if o / 2 >= REGS {
            return;
        }
        match o {
            TRANSFER_ADDR => {
                self.regs[o / 2] = val;
                // Held in 8-byte units.
                self.transfer_addr = (val as u32) * 8;
            }
            TRANSFER_FIFO => self.push(val),
            // Read only: it is derived, and letting a write land here would
            // make it stop following SPUCNT.
            SPUSTAT => {}
            _ => self.regs[o / 2] = val,
        }
    }

    /// Push one halfword into sound RAM at the transfer pointer.
    fn push(&mut self, val: u16) {
        let a = (self.transfer_addr as usize) & (RAM_BYTES - 1);
        self.ram[a] = val as u8;
        self.ram[(a + 1) & (RAM_BYTES - 1)] = (val >> 8) as u8;
        self.transfer_addr = self.transfer_addr.wrapping_add(2);
        self.bytes_written += 2;
    }

    /// One word from DMA channel 4, which is how a game moves samples in bulk.
    pub fn write_word(&mut self, word: u32) {
        self.push(word as u16);
        self.push((word >> 16) as u16);
    }

    /// One word back out, for a transfer the other way.
    pub fn read_word(&mut self) -> u32 {
        let mut w = 0u32;
        for i in 0..4 {
            let a = (self.transfer_addr as usize + i) & (RAM_BYTES - 1);
            w |= (self.ram[a] as u32) << (8 * i);
        }
        self.transfer_addr = self.transfer_addr.wrapping_add(4);
        w
    }

    // ---- save state ------------------------------------------------------

    pub(crate) fn parts(&self) -> (&[u16; REGS], &[u8], u32) {
        (&self.regs, &self.ram, self.transfer_addr)
    }

    pub(crate) fn restore(&mut self, regs: [u16; REGS], ram: &[u8], transfer_addr: u32) {
        self.regs = regs;
        if ram.len() == RAM_BYTES {
            self.ram.copy_from_slice(ram);
        }
        self.transfer_addr = transfer_addr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_read_back_what_was_written() {
        let mut s = Spu::new();
        s.write(0x1AA, 2, 0xC000); // SPUCNT: enable, unmute
        assert_eq!(s.read(0x1AA, 2), 0xC000);
        // The whole thing that unblocked Crash Bandicoot. A register that reads
        // zero forever is a hang, not a missing feature.
        s.write(0x000, 2, 0x1234); // voice 0 volume left
        assert_eq!(s.read(0x000, 2), 0x1234);
    }

    #[test]
    fn status_follows_control() {
        let mut s = Spu::new();
        s.write(0x1AA, 2, 0x803F);
        assert_eq!(s.read(0x1AE, 2), 0x3F, "the low six bits, and no more");

        // And it stays derived: a write to it must not stick.
        s.write(0x1AE, 2, 0xFFFF);
        assert_eq!(s.read(0x1AE, 2), 0x3F);
    }

    #[test]
    fn the_transfer_address_is_in_eight_byte_units() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0x0100); // 0x100 * 8 = 0x800
        s.write(0x1A8, 2, 0xBEEF);
        assert_eq!(s.ram[0x800], 0xEF);
        assert_eq!(s.ram[0x801], 0xBE);
        // Reading the register back gives the unscaled value software wrote.
        assert_eq!(s.read(0x1A6, 2), 0x0100);
    }

    #[test]
    fn the_transfer_pointer_advances() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0);
        for v in [0x1111u32, 0x2222, 0x3333] {
            s.write(0x1A8, 2, v);
        }
        assert_eq!(&s.ram[0..6], &[0x11, 0x11, 0x22, 0x22, 0x33, 0x33]);
        assert_eq!(s.bytes_written, 6);
    }

    #[test]
    fn a_dma_word_becomes_two_halfwords_in_order() {
        let mut s = Spu::new();
        s.write(0x1A6, 2, 0);
        s.write_word(0xBBBB_AAAA);
        assert_eq!(&s.ram[0..4], &[0xAA, 0xAA, 0xBB, 0xBB], "low half first");
    }

    #[test]
    fn sound_ram_wraps_rather_than_panicking() {
        let mut s = Spu::new();
        // The register is 16 bits and holds 8-byte units, so it can address
        // 512 KB exactly; the wrap matters for the pointer running off the end.
        s.write(0x1A6, 2, 0xFFFF);
        s.write(0x1A8, 2, 0x1234);
        s.write(0x1A8, 2, 0x5678);
        assert_eq!(s.ram[RAM_BYTES - 8], 0x34);
        assert_eq!(s.bytes_written, 4);
    }

    #[test]
    fn a_byte_write_leaves_the_other_half_alone() {
        let mut s = Spu::new();
        s.write(0x100, 2, 0xAABB);
        s.write(0x101, 1, 0xCC);
        assert_eq!(s.read(0x100, 2), 0xCCBB);
        s.write(0x100, 1, 0xDD);
        assert_eq!(s.read(0x100, 2), 0xCCDD);
    }

    #[test]
    fn a_word_access_covers_two_registers() {
        let mut s = Spu::new();
        s.write(0x200, 4, 0xDDDD_CCCC);
        assert_eq!(s.read(0x200, 2), 0xCCCC);
        assert_eq!(s.read(0x202, 2), 0xDDDD);
        assert_eq!(s.read(0x200, 4), 0xDDDD_CCCC);
    }
}
