// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The interrupt controller.
//!
//! Eleven sources funnel into a single line to the CPU. `I_STAT` latches a
//! request, `I_MASK` decides whether it reaches the CPU, and the CPU sees the
//! result as COP0 Cause bit 10.
//!
//! Two details that are easy to get backwards:
//!
//! * `I_STAT` is **write-acknowledge, and the polarity is inverted**. Writing a
//!   `0` to a bit clears it; writing a `1` leaves it alone. Handlers therefore
//!   write `!(1 << bit)` to dismiss one source, and a naive `stat |= val` or
//!   `stat = val` leaves the line stuck high forever.
//! * A source **latches**. Raising it sets the bit and the bit stays set until
//!   software acknowledges it, whether or not it was masked at the time.

/// Bit positions in `I_STAT` / `I_MASK`.
pub const VBLANK: u32 = 0;
pub const GPU: u32 = 1;
pub const CDROM: u32 = 2;
pub const DMA: u32 = 3;
pub const TIMER0: u32 = 4;
pub const TIMER1: u32 = 5;
pub const TIMER2: u32 = 6;
pub const CONTROLLER: u32 = 7;
pub const SIO: u32 = 8;
pub const SPU: u32 = 9;
pub const LIGHTPEN: u32 = 10;

/// Only 11 bits are implemented; the rest read back as zero.
const IMPLEMENTED: u16 = 0x07FF;

/// How many sources there are, for the counters below.
pub const SOURCES: usize = 11;

/// Names for the eleven sources, so a harness can print a histogram without
/// keeping its own copy of the numbering.
pub const NAMES: [&str; SOURCES] = [
    "vblank", "gpu", "cdrom", "dma", "timer0", "timer1", "timer2", "pad", "sio", "spu", "lightpen",
];

#[derive(Clone, Default)]
pub struct Irq {
    stat: u16,
    mask: u16,

    /// Requests latched, and requests latched while unmasked, per source.
    ///
    /// Host-side observation, never serialized. The difference between the two
    /// is the whole point: a device that is firing correctly into a mask the
    /// program never opened looks identical, from the device's side, to one
    /// that is not firing at all. This is the cheapest instrument that tells
    /// "nothing raised it" from "nobody was listening".
    pub raised: [u64; SOURCES],
    pub delivered: [u64; SOURCES],
}

impl Irq {
    pub fn new() -> Irq {
        Irq::default()
    }

    /// Latch a request from a source. Called by the devices.
    #[inline]
    pub fn raise(&mut self, bit: u32) {
        self.stat |= (1 << bit) & IMPLEMENTED;
        if let Some(n) = self.raised.get_mut(bit as usize) {
            *n += 1;
        }
        if self.mask & (1 << bit) != 0 {
            if let Some(n) = self.delivered.get_mut(bit as usize) {
                *n += 1;
            }
        }
    }

    /// Is any unmasked request outstanding? This drives Cause bit 10.
    #[inline]
    pub fn pending(&self) -> bool {
        self.stat & self.mask != 0
    }

    pub fn stat(&self) -> u16 {
        self.stat
    }
    pub fn mask(&self) -> u16 {
        self.mask
    }

    /// A write to `I_STAT`. Zero bits clear, one bits are left alone.
    pub fn ack(&mut self, val: u16) {
        self.stat &= val & IMPLEMENTED;
    }

    /// A write to `I_MASK`.
    pub fn set_mask(&mut self, val: u16) {
        self.mask = val & IMPLEMENTED;
    }

    pub(crate) fn restore(&mut self, stat: u16, mask: u16) {
        self.stat = stat & IMPLEMENTED;
        self.mask = mask & IMPLEMENTED;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_is_write_acknowledge_with_inverted_polarity() {
        let mut irq = Irq::new();
        irq.raise(VBLANK);
        irq.raise(TIMER1);
        assert_eq!(irq.stat(), (1 << VBLANK) | (1 << TIMER1));

        // Dismiss VBLANK the way a handler does: write ones everywhere except
        // the bit being acknowledged.
        irq.ack(!(1 << VBLANK));
        assert_eq!(irq.stat(), 1 << TIMER1, "the wrong bit was cleared");

        // Writing all ones is a no-op, not "raise everything".
        irq.ack(0xFFFF);
        assert_eq!(irq.stat(), 1 << TIMER1);
    }

    #[test]
    fn masking_gates_the_line_but_not_the_latch() {
        let mut irq = Irq::new();
        irq.raise(TIMER0);
        assert!(!irq.pending(), "masked source must not reach the CPU");

        irq.set_mask(1 << TIMER0);
        assert!(irq.pending(), "the request should still have been latched");
    }
}
