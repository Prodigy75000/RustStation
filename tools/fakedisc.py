#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Prodigy75000
"""Write a synthetic PlayStation disc image: a cue sheet and a raw BIN.

Nothing on it will boot. What it *does* carry is a valid Mode 2 Form 1 sector
layout and a licence string in the system area, which is enough to drive the
BIOS through its whole disc-recognition sequence: Test, GetStat, GetID, Setloc,
SeekL, Setmode, ReadN, Pause. The BIOS then draws its licence screen using text
read off the disc, which is the end-to-end check that the cue parser, the sector
reads and the data FIFO all agree with each other.

It exists because no disc image can be committed to this repository, and a
milestone nobody can reproduce is not much of a milestone.

    python tools/fakedisc.py out/fakedisc
    cargo run --release --bin shot -- <bios.bin> \
        --disc out/fakedisc/fake.cue --steps 400000000 --out out/shots/fake.png

Expect the PlayStation licence screen, reading "Sony Computer Entertainment"
and the region the string below names.
"""

import os
import sys

SECTOR = 2352
SECTORS = 600
REGION_TEXT = b"          Licensed  by          Sony Computer Entertainment of America  "


def bcd(v: int) -> int:
    return ((v // 10) << 4) | (v % 10)


def build(path: str) -> None:
    os.makedirs(path, exist_ok=True)
    img = bytearray(SECTOR * SECTORS)

    for lba in range(SECTORS):
        b = lba * SECTOR
        # The 12-byte sync pattern every raw data sector opens with.
        img[b : b + 12] = bytes([0x00] + [0xFF] * 10 + [0x00])
        # Then the header: absolute position in BCD, 150 sectors ahead of the
        # LBA because the disc's own addressing starts at 00:02:00.
        total = lba + 150
        img[b + 12] = bcd(total // (60 * 75))
        img[b + 13] = bcd((total // 75) % 60)
        img[b + 14] = bcd(total % 75)
        img[b + 15] = 2  # mode 2
        # A marker in the user data, so a read can be traced to its sector.
        img[b + 24] = lba & 0xFF

    # The licence text lives in the system area, and the BIOS puts it on screen
    # verbatim. Sector 4 is where it starts.
    off = 4 * SECTOR + 24
    img[off : off + len(REGION_TEXT)] = REGION_TEXT

    with open(os.path.join(path, "fake.bin"), "wb") as f:
        f.write(img)
    with open(os.path.join(path, "fake.cue"), "w") as f:
        f.write('FILE "fake.bin" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n')

    print(f"wrote {SECTORS} sectors to {path}/fake.bin and fake.cue")


if __name__ == "__main__":
    build(sys.argv[1] if len(sys.argv) > 1 else "out/fakedisc")
