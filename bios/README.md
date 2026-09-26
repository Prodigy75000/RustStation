# bios/

PlayStation BIOS dumps for the dev harnesses go here. **Nothing in this folder
is committed** except this file: a BIOS is copyrighted Sony code.

## The libretro core does not use one

The core always boots its built-in HLE kernel: a generated 512 KB ROM image
whose kernel functions are done in Rust, written from the public
documentation. See [`../docs/notes/HLE.md`](../docs/notes/HLE.md) for what it
covers and how it was tested. A BIOS file in the frontend's system directory
is ignored.

That is deliberate. A save state does not carry the BIOS, so two netplay peers
only run the same machine if they run the same kernel, and a BIOS file one of
them forgot about would split them a few frames in. With the kernel built in,
every copy of the core is running the same one.

The HLE kernel has no shell, so the core needs content to boot.

## What the harnesses use one for

A real BIOS is the reference the HLE kernel is measured against. The
conformance runs in [`../docs/TESTS.md`](../docs/TESTS.md) use one, and the
kernel's behaviour was taken from running a real BIOS as a black box. The
harnesses (`shot`, `testrom`, `psx`, `fingerprint`) take the image's path as
their first argument, so its name does not matter, and take `hle` in its place
for the built-in kernel.

An image must be **512 KB (524 288 bytes)**. Anything else is rejected with
its actual size in the message.

## Why the BIOS is not in save states

It is this console's equivalent of a cartridge ROM: read-only, and identical on
both sides of a netplay session. Keeping it out of the state keeps the state
small and keeps `retro_serialize_size()` constant. The `fingerprint` harness
prints a checksum of the BIOS alongside the state checksum, so two hosts
comparing fingerprints can tell "different state" from "different BIOS".
