// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! An MDEC-out transfer ends later than it starts.

use psx_core::{hle, Psx};

const MDEC_CMD: u32 = 0x1F80_1820;
const MDEC_CTRL: u32 = 0x1F80_1824;
const DPCR: u32 = 0x1F80_10F0;
const DICR: u32 = 0x1F80_10F4;
const CH0: u32 = 0x1F80_1080;
const CH1: u32 = 0x1F80_1090;

/// A video player starts the output transfer and then records that one is in
/// flight, so the completion interrupt has to come after the start, not
/// inside it. With it inside, Dino Crisis 2 and Batman of the Future lost the
/// race and decoded two frames of video, then nothing.
#[test]
fn an_mdec_out_transfer_completes_after_it_starts() {
    let mut psx = Psx::new(hle::rom()).unwrap();
    let bus = &mut psx.bus;

    // One colour macroblock of six blocks, each only a DC term and its end
    // code, at 0x1000.
    for (i, &h) in [0x0400u16, 0xFE00].repeat(6).iter().enumerate() {
        bus.ram[0x1000 + i * 2..0x1002 + i * 2].copy_from_slice(&h.to_le_bytes());
    }
    let dpcr = bus.load(DPCR, 4);
    bus.store(DPCR, 4, dpcr | 0x88); // channels 0 and 1 on
    bus.store(DICR, 4, (1 << 23) | (1 << 17)); // channel 1's interrupt on
    bus.store(MDEC_CTRL, 4, 0x6000_0000); // both DMA requests on
    bus.store(MDEC_CMD, 4, (1 << 29) | (3 << 27) | 6); // decode, 15-bit, 6 words

    bus.store(CH0, 4, 0x1000);
    bus.store(CH0 + 4, 4, (1 << 16) | 6);
    bus.store(CH0 + 8, 4, 0x0100_0201); // in, block mode, start

    // 16x16 pixels at 15 bits: 128 words out.
    bus.store(CH1, 4, 0x2000);
    bus.store(CH1 + 4, 4, (4 << 16) | 32);
    bus.store(CH1 + 8, 4, 0x0100_0200); // out, block mode, start

    let busy = |bus: &mut psx_core::bus::Bus| bus.load(CH1 + 8, 4) & (1 << 24) != 0;
    let flagged = |bus: &mut psx_core::bus::Bus| bus.load(DICR, 4) & (1 << 25) != 0;
    assert!(busy(bus), "still in flight the moment it starts");
    assert!(!flagged(bus), "and not yet complete");
    assert!(
        bus.ram[0x2000..0x2200].iter().any(|&b| b != 0),
        "though its words are already in RAM"
    );

    bus.tick(1_000);
    bus.sync();
    assert!(!busy(bus), "done once the time has passed");
    assert!(flagged(bus), "with its completion flagged");
}
