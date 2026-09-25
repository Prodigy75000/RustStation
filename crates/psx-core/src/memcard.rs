// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The memory card: 128 KB behind address 81h on the controller port.
//!
//! Written from psx-spx, "Memory Card Read/Write Commands" and "Memory Card
//! Data Format". A card shares its slot's select line with the pad, and the
//! address byte decides which of the two answers. Three commands exist: read
//! a 128-byte sector (52h "R"), write one (57h "W"), and get the card's ID
//! (53h "S"); anything else ends the transfer after the command byte.
//!
//! What is on the card belongs to the frontend, like the disc image: it is
//! loaded from and saved to a file there, and **it is not in a save state**,
//! so loading an old state never takes back a save the player made since.
//! What the card is doing, mid-transfer, and its flag byte, are in the state.
//!
//! A new card is formatted, as Sony's shipped: header frame, fifteen free
//! directory entries, an empty broken-sector list. A game finds an empty card
//! rather than offering to format one.

/// Bytes on a card: 1024 sectors of 128.
pub const CARD_SIZE: usize = 128 * 1024;
/// Bytes in a sector (psx-spx also says "frame").
pub const SECTOR: usize = 128;
const SECTORS: u16 = (CARD_SIZE / SECTOR) as u16;

/// FLAG bit 3: the directory has not been read since the card went in. Set at
/// power-on and insertion, and (oddly, per psx-spx) cleared by a write, not a
/// read; games write sector 3Fh to clear it.
const FLAG_NEW: u8 = 0x08;

/// Cycles from a byte to the card's /ACK: "circa 1500".
pub(crate) const ACK_DELAY: u64 = 1500;
/// The extra wait a Sony card adds after the seventh byte of a read, while it
/// fetches the sector: "circa 31000".
pub(crate) const READ_DELAY: u64 = 31_000;

#[derive(Clone)]
pub struct MemoryCard {
    pub connected: bool,
    /// The card's contents. The frontend's, like the disc: not serialized.
    pub data: Vec<u8>,
    /// Set whenever a write lands, for a frontend that wants to know when to
    /// save. Host side.
    pub written: bool,

    pub(crate) flag: u8,
    /// This transfer's command, and the sector it names.
    pub(crate) command: u8,
    pub(crate) address: u16,
    /// Running XOR of the address and data, and a write's incoming sector.
    pub(crate) checksum: u8,
    pub(crate) buffer: [u8; SECTOR],
    /// The byte software sent last, which several replies echo.
    pub(crate) previous: u8,
}

impl Default for MemoryCard {
    fn default() -> MemoryCard {
        MemoryCard::new()
    }
}

impl MemoryCard {
    pub fn new() -> MemoryCard {
        MemoryCard {
            connected: false,
            data: formatted(),
            written: false,
            flag: FLAG_NEW,
            command: 0,
            address: 0,
            checksum: 0,
            buffer: [0; SECTOR],
            previous: 0,
        }
    }

    /// Put a card image in, as a player plugging one in does: the flag says
    /// the directory is unread again. `image` shorter than a card is padded
    /// with a formatted card's bytes, longer is cut.
    pub fn insert(&mut self, image: &[u8]) {
        let mut data = formatted();
        let n = image.len().min(CARD_SIZE);
        data[..n].copy_from_slice(&image[..n]);
        self.data = data;
        self.connected = true;
        self.flag = FLAG_NEW;
    }

    fn sector_valid(&self) -> bool {
        self.address < SECTORS
    }

    fn at(&self) -> usize {
        self.address as usize * SECTOR
    }

    /// Take byte `step` of a transfer, `tx`, and answer: the reply, and how
    /// long until /ACK, or `None` for no /ACK, which ends the transfer.
    pub(crate) fn exchange(&mut self, step: u32, tx: u8) -> (u8, Option<u64>) {
        let ack = Some(ACK_DELAY);
        let previous = std::mem::replace(&mut self.previous, tx);
        match step {
            0 => (0xFF, ack),
            1 => {
                self.command = tx;
                let flag = self.flag;
                match tx {
                    b'R' | b'W' | b'S' => (flag, ack),
                    // Anything else: the transfer stops after the command.
                    _ => (flag, None),
                }
            }
            2 => (0x5A, ack),
            3 => (0x5D, ack),
            _ => match self.command {
                b'R' => self.read(step, tx),
                b'W' => self.write(step, tx, previous),
                _ => self.id(step),
            },
        }
    }

    fn read(&mut self, step: u32, tx: u8) -> (u8, Option<u64>) {
        let ack = Some(ACK_DELAY);
        match step {
            4 => {
                self.address = (tx as u16) << 8;
                (0x00, ack)
            }
            5 => {
                self.address |= tx as u16;
                // The reply is the byte before, the address MSB.
                (self.previous_reply(), Some(ACK_DELAY + READ_DELAY))
            }
            6 => (0x5C, ack),
            7 => (0x5D, ack),
            // An address past the end: Sony's cards confirm FFFFh and stop.
            8 if !self.sector_valid() => (0xFF, ack),
            9 if !self.sector_valid() => (0xFF, None),
            8 => {
                let msb = (self.address >> 8) as u8;
                self.checksum = msb;
                (msb, ack)
            }
            9 => {
                let lsb = self.address as u8;
                self.checksum ^= lsb;
                (lsb, ack)
            }
            10..=137 => {
                let b = self.data[self.at() + (step - 10) as usize];
                self.checksum ^= b;
                (b, ack)
            }
            138 => (self.checksum, ack),
            139 => (0x47, None),
            _ => (0xFF, None),
        }
    }

    /// psx-spx's "(pre)": the reply to the address LSB is the MSB, and the
    /// data and checksum bytes echo what came before them.
    fn previous_reply(&self) -> u8 {
        (self.address >> 8) as u8
    }

    fn write(&mut self, step: u32, tx: u8, previous: u8) -> (u8, Option<u64>) {
        let ack = Some(ACK_DELAY);
        match step {
            4 => {
                self.address = (tx as u16) << 8;
                self.checksum = tx;
                (0x00, ack)
            }
            5 => {
                self.address |= tx as u16;
                self.checksum ^= tx;
                (previous, ack)
            }
            6..=133 => {
                self.buffer[(step - 6) as usize] = tx;
                self.checksum ^= tx;
                (previous, ack)
            }
            // The checksum software computed; ours is compared at the end.
            134 => {
                self.checksum ^= tx;
                (previous, ack)
            }
            135 => (0x5C, ack),
            136 => (0x5D, ack),
            137 => {
                let end = if !self.sector_valid() {
                    0xFF
                } else if self.checksum != 0 {
                    // Ours XOR theirs is zero when they agree.
                    0x4E
                } else {
                    let at = self.at();
                    self.data[at..at + SECTOR].copy_from_slice(&self.buffer);
                    self.written = true;
                    self.flag &= !FLAG_NEW;
                    0x47
                };
                (end, None)
            }
            _ => (0xFF, None),
        }
    }

    fn id(&mut self, step: u32) -> (u8, Option<u64>) {
        let ack = Some(ACK_DELAY);
        match step {
            4 => (0x5C, ack),
            5 => (0x5D, ack),
            6 => (0x04, ack),
            7 => (0x00, ack),
            8 => (0x00, ack),
            9 => (0x80, None),
            _ => (0xFF, None),
        }
    }
}

/// A freshly formatted card, per psx-spx "Memory Card Data Format".
pub fn formatted() -> Vec<u8> {
    let mut d = vec![0u8; CARD_SIZE];
    let frame = |n: usize| n * SECTOR..(n + 1) * SECTOR;
    let seal = |f: &mut [u8]| {
        f[SECTOR - 1] = f[..SECTOR - 1].iter().fold(0, |a, b| a ^ b);
    };
    // Frame 0, and its copy in frame 63: "MC", zeros, checksum.
    for n in [0, 63] {
        let r = frame(n);
        d[r.start] = b'M';
        d[r.start + 1] = b'C';
        seal(&mut d[r]);
    }
    // Frames 1 to 15: free directory entries, no next block.
    for n in 1..16 {
        let r = frame(n);
        d[r.start] = 0xA0;
        d[r.start + 8] = 0xFF;
        d[r.start + 9] = 0xFF;
        seal(&mut d[r]);
    }
    // Frames 16 to 35: no broken sectors.
    for n in 16..36 {
        let r = frame(n);
        d[r.start..r.start + 4].fill(0xFF);
        seal(&mut d[r]);
    }
    // Frames 36 to 62: replacement data and unused, FFh-filled.
    for n in 36..63 {
        let r = frame(n);
        d[r].fill(0xFF);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a whole transfer, byte by byte, as software would, and return the
    /// replies up to the first byte the card did not acknowledge.
    fn transfer(card: &mut MemoryCard, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &tx) in bytes.iter().enumerate() {
            let (rx, ack) = card.exchange(i as u32, tx);
            out.push(rx);
            if ack.is_none() {
                break;
            }
        }
        out
    }

    fn read_cmd(sector: u16) -> Vec<u8> {
        let mut b = vec![0x81, b'R', 0, 0, (sector >> 8) as u8, sector as u8];
        b.resize(140, 0);
        b
    }

    fn write_cmd(sector: u16, data: &[u8; SECTOR], checksum: Option<u8>) -> Vec<u8> {
        let (msb, lsb) = ((sector >> 8) as u8, sector as u8);
        let mut b = vec![0x81, b'W', 0, 0, msb, lsb];
        b.extend_from_slice(data);
        let good = data.iter().fold(msb ^ lsb, |a, x| a ^ x);
        b.push(checksum.unwrap_or(good));
        b.extend_from_slice(&[0, 0, 0]);
        b
    }

    fn card() -> MemoryCard {
        let mut c = MemoryCard::new();
        c.connected = true;
        c
    }

    #[test]
    fn a_read_returns_the_sector_framed_as_psx_spx_gives_it() {
        let mut c = card();
        for (i, b) in c.data[5 * SECTOR..6 * SECTOR].iter_mut().enumerate() {
            *b = i as u8 ^ 0x5A;
        }
        let r = transfer(&mut c, &read_cmd(5));
        assert_eq!(r.len(), 140, "the whole read, the end byte unacknowledged");
        assert_eq!(&r[1..4], &[0x08, 0x5A, 0x5D], "flag, then the card ID");
        assert_eq!(r[5], 0x00, "the reply to the LSB is the MSB");
        assert_eq!(
            &r[6..10],
            &[0x5C, 0x5D, 0x00, 0x05],
            "acknowledge, confirmed address"
        );
        assert_eq!(&r[10..138], &c.data[5 * SECTOR..6 * SECTOR]);
        let sum = c.data[5 * SECTOR..6 * SECTOR]
            .iter()
            .fold(0x00 ^ 0x05, |a, x| a ^ x);
        assert_eq!(r[138], sum);
        assert_eq!(r[139], 0x47, "G for good");
    }

    #[test]
    fn a_write_lands_only_with_a_good_checksum_and_clears_the_flag() {
        let mut c = card();
        let data = [0xC3u8; SECTOR];

        let r = transfer(&mut c, &write_cmd(0x3F, &data, Some(0x00)));
        assert_eq!(r.last(), Some(&0x4E), "bad checksum");
        assert_ne!(
            &c.data[0x3F * SECTOR..0x40 * SECTOR],
            &data[..],
            "and nothing written"
        );
        assert_eq!(transfer(&mut c, &read_cmd(0))[1], 0x08, "flag still set");

        let r = transfer(&mut c, &write_cmd(0x3F, &data, None));
        assert_eq!(&r[135..138], &[0x5C, 0x5D, 0x47]);
        assert_eq!(&c.data[0x3F * SECTOR..0x40 * SECTOR], &data[..]);
        assert!(c.written);
        assert_eq!(
            transfer(&mut c, &read_cmd(0))[1],
            0x00,
            "the write cleared bit 3"
        );
    }

    #[test]
    fn a_sector_past_the_end_is_refused() {
        let mut c = card();
        let r = transfer(&mut c, &read_cmd(0x400));
        assert_eq!(
            &r[8..],
            &[0xFF, 0xFF],
            "FFFFh confirmed, then the card stops"
        );
        let r = transfer(&mut c, &write_cmd(0x400, &[1; SECTOR], None));
        assert_eq!(r.last(), Some(&0xFF));
    }

    #[test]
    fn get_id_and_an_unknown_command() {
        let mut c = card();
        let r = transfer(&mut c, &[0x81, b'S', 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            &r[1..],
            &[0x08, 0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80]
        );
        let r = transfer(&mut c, &[0x81, b'X', 0, 0]);
        assert_eq!(r.len(), 2, "stops after the command byte");
    }

    #[test]
    fn a_new_card_is_formatted() {
        let d = formatted();
        let frame = |n: usize| &d[n * SECTOR..(n + 1) * SECTOR];
        let xor = |f: &[u8]| f.iter().fold(0u8, |a, b| a ^ b);
        assert_eq!(&frame(0)[..2], b"MC");
        assert_eq!(frame(0)[0x7F], 0x0E, "psx-spx: usually 0Eh");
        for n in 1..16 {
            assert_eq!(frame(n)[0], 0xA0, "entry {n} free");
            assert_eq!(xor(frame(n)), 0, "entry {n} checksums");
        }
        assert_eq!(xor(frame(16)), 0);
        assert_eq!(frame(63), frame(0));
    }
}
