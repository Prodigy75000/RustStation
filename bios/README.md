# bios/

Drop your PlayStation BIOS dumps here. **Nothing in this folder is committed**
except this file: a BIOS is copyrighted Sony code.

## A BIOS is optional

Without one, the core boots games on its built-in HLE kernel: a generated
512 KB ROM image whose kernel functions are done in Rust, written from the
public documentation. See [`../docs/notes/HLE.md`](../docs/notes/HLE.md) for
what it covers and how it was tested. It has no shell of its own, so with no
BIOS and no content there is nothing to show, and loading fails.

A real BIOS is still the reference: the conformance runs in
[`../docs/TESTS.md`](../docs/TESTS.md) use one, and a save state belongs to the
kernel it was made on (see [`../docs/SAVESTATE.md`](../docs/SAVESTATE.md)).

## What the core expects

A single **512 KB (524 288 byte)** image. Anything else is rejected with its
actual size in the message, rather than half-working.

Region matters. A console refuses a disc from another region and drops to its
own shell: a Japanese SCPH-5500 with an American disc goes straight to the BIOS
screen. So the libretro core reads the disc's licence region first and prefers
a BIOS of that region.

Names it looks for in the frontend's system directory, in lower or upper case:

| Region | Names, best first |
|---|---|
| America | `scph5501.bin`, `scph7001.bin`, `scph1001.bin` |
| Europe | `scph5502.bin`, `scph7002.bin`, `scph1002.bin` |
| Japan | `scph5500.bin`, `scph1000.bin` |
| Any | `psxonpsp660.bin`, `bios.bin` |

The disc's own region is tried first, then the "any" names, then every other
name as a last resort (with a warning), since a mismatched BIOS still boots
EXEs and its own menu. With no disc, or a disc whose region cannot be read,
the "any" names come first, then `scph5501`, `scph5500`, `scph5502`,
`scph7001`, `scph7002`, `scph1001`, `scph1002`, `scph1000`. A file of the wrong size is
skipped with a warning. If none is found and there is content to boot, the HLE
kernel is used.

The dev harnesses take a path, so they do not care what it is called.

## Why the BIOS is not in save states

It is this console's equivalent of a cartridge ROM: read-only, and identical on
both sides of a netplay session by assumption. Keeping it out of the state keeps
the state small and keeps `retro_serialize_size()` constant. The `fingerprint`
harness prints a checksum of the BIOS alongside the state checksum, so two hosts
comparing fingerprints can tell "different state" from "different BIOS".
