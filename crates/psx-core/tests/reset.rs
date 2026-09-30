// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Reset is a power cycle: a reset machine is a new machine, except for what
//! is plugged into it.

use psx_core::{hle, Psx};

/// Enough to be well into the kernel's boot: devices configured, interrupts
/// enabled, timers and the drive running.
const WARM: u64 = 60_000_000;
const AFTER: u64 = 20_000_000;

#[test]
fn a_reset_machine_runs_exactly_as_a_new_one() {
    let mut used = Psx::new(hle::rom()).unwrap();
    used.run(WARM);
    used.reset();
    used.run(AFTER);

    let mut new = Psx::new(hle::rom()).unwrap();
    new.run(AFTER);

    assert_eq!(used.cpu.pc, new.cpu.pc);
    assert!(
        used.save_state() == new.save_state(),
        "the reset machine kept something a new one does not have"
    );
}

#[test]
fn a_reset_keeps_what_is_plugged_in() {
    let mut psx = Psx::new(hle::rom()).unwrap();
    psx.bus.sio.pads[1].connected = true;
    psx.bus.sio.pads[1].buttons = 0x0808;
    psx.bus.sio.cards[1].connected = true;
    psx.bus.sio.cards[1].data[0x2000] = 0x5A;
    let ram = psx.bus.ram.as_ptr();
    let scratchpad = psx.bus.scratchpad.as_ptr();
    let card = psx.bus.sio.cards[0].data.as_ptr();
    psx.run(WARM);
    psx.reset();

    // A frontend holds pointers to all three, for achievements and save RAM.
    assert_eq!(psx.bus.ram.as_ptr(), ram, "system RAM moved");
    assert_eq!(psx.bus.scratchpad.as_ptr(), scratchpad, "scratchpad moved");
    assert_eq!(psx.bus.sio.cards[0].data.as_ptr(), card, "save RAM moved");
    assert!(psx.bus.sio.pads[0].connected && psx.bus.sio.pads[1].connected);
    assert_eq!(psx.bus.sio.pads[1].buttons, 0x0808, "still held");
    assert!(psx.bus.sio.cards[1].connected);
    assert_eq!(
        psx.bus.sio.cards[1].data[0x2000], 0x5A,
        "the save is still on the card"
    );
}
