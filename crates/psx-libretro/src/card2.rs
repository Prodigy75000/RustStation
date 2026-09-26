// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The memory card in slot 2, which the core keeps itself.
//!
//! libretro gives a core one save RAM, and slot 1's card is it: the frontend
//! keeps that per game, like a cartridge's battery RAM. Slot 2 has no such
//! channel, so the core reads and writes it as a file in the frontend's save
//! directory. It is one card for every game, as a second card on a console
//! was: a player kept saves from many games on it and carried it between
//! them. That is also what makes it useful, because copying a save from one
//! card to the other in a game's own menu, or in the BIOS's, needs two.
//!
//! The file is written once the card has gone quiet for [`SETTLE_FRAMES`]
//! frames after a write, rather than on every sector, so a game saving
//! sixteen sectors writes the file once; and again on unload, so that a save
//! made in the last second before quitting is not lost.

use std::path::{Path, PathBuf};

use psx_core::memcard::CARD_SIZE;
use psx_core::Psx;

/// The card's file, in the save directory.
pub const FILE_NAME: &str = "ruststation-card2.mcd";

/// Frames without a write before the file is written.
pub const SETTLE_FRAMES: u32 = 60;

/// Where slot 2's card lives, and whether it has changed since it was last
/// written there.
#[derive(Default)]
pub struct Card2 {
    path: Option<PathBuf>,
    /// Frames since the last write, while there is one not yet on disk.
    quiet: Option<u32>,
}

impl Card2 {
    /// Put the card from `dir` in slot 2, or a formatted one if there is none
    /// yet. With no directory the card still works, it just is not kept.
    pub fn attach(psx: &mut Psx, dir: Option<&Path>) -> Card2 {
        let card = &mut psx.bus.sio.cards[1];
        let path = dir.map(|d| d.join(FILE_NAME));
        match path.as_deref().map(std::fs::read) {
            Some(Ok(image)) if image.len() == CARD_SIZE => {
                card.insert(&image);
                info!("memory card 2 from {}", path.as_ref().unwrap().display());
            }
            Some(Ok(image)) => {
                warn!(
                    "ignoring {}: {} bytes, a card is {CARD_SIZE}; slot 2 starts formatted",
                    path.as_ref().unwrap().display(),
                    image.len()
                );
                card.connected = true;
            }
            _ => card.connected = true,
        }
        card.written = false;
        Card2 { path, quiet: None }
    }

    /// Once a frame: note a write, and write the file once the card settles.
    pub fn frame(&mut self, psx: &mut Psx) {
        let card = &mut psx.bus.sio.cards[1];
        if card.written {
            card.written = false;
            self.quiet = Some(0);
        } else if let Some(n) = self.quiet.as_mut() {
            *n += 1;
            if *n >= SETTLE_FRAMES {
                self.flush(psx);
            }
        }
    }

    /// Write the card now if it has changed. For unload.
    pub fn flush(&mut self, psx: &Psx) {
        if self.quiet.take().is_none() {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        // Written aside and renamed over, so a crash mid-write leaves the
        // old card rather than half of a new one.
        let tmp = path.with_extension("mcd.tmp");
        let data = &psx.bus.sio.cards[1].data;
        match std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path)) {
            Ok(()) => info!("memory card 2 saved to {}", path.display()),
            Err(e) => error!("memory card 2: cannot write {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Psx {
        Psx::new(psx_core::hle::rom()).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rsta-card2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A write reaches the file only after the card has been quiet for the
    /// settle time, and then on the next machine, in slot 2 and not slot 1.
    #[test]
    fn a_write_to_slot_two_is_kept_for_the_next_game() {
        let dir = scratch("keep");
        let mut psx = machine();
        let mut card2 = Card2::attach(&mut psx, Some(&dir));
        assert!(psx.bus.sio.cards[1].connected);
        let file = dir.join(FILE_NAME);

        psx.bus.sio.cards[1].data[0x2000] = 0x5A;
        psx.bus.sio.cards[1].written = true;
        for _ in 0..SETTLE_FRAMES {
            card2.frame(&mut psx);
        }
        assert!(!file.exists(), "written before the card settled");
        card2.frame(&mut psx);
        assert_eq!(std::fs::read(&file).unwrap()[0x2000], 0x5A);

        let mut next = machine();
        Card2::attach(&mut next, Some(&dir));
        assert_eq!(next.bus.sio.cards[1].data[0x2000], 0x5A);
        assert_ne!(next.bus.sio.cards[0].data[0x2000], 0x5A);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quitting straight after a save still keeps it.
    #[test]
    fn unload_writes_a_card_that_has_not_settled() {
        let dir = scratch("unload");
        let mut psx = machine();
        let mut card2 = Card2::attach(&mut psx, Some(&dir));
        psx.bus.sio.cards[1].data[0x2001] = 0xA5;
        psx.bus.sio.cards[1].written = true;
        card2.frame(&mut psx);
        card2.flush(&psx);
        assert_eq!(std::fs::read(dir.join(FILE_NAME)).unwrap()[0x2001], 0xA5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An untouched card is never written, so a game that only reads does
    /// not rewrite the player's card every time it quits.
    #[test]
    fn a_card_nothing_wrote_is_not_written() {
        let dir = scratch("untouched");
        let mut psx = machine();
        let mut card2 = Card2::attach(&mut psx, Some(&dir));
        for _ in 0..3 * SETTLE_FRAMES {
            card2.frame(&mut psx);
        }
        card2.flush(&psx);
        assert!(!dir.join(FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
