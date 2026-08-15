# bios/

Drop your PlayStation BIOS dumps here. **Nothing in this folder is committed**
except this file: a BIOS is copyrighted Sony code.

## What the core expects

A single **512 KB (524 288 byte)** image. Anything else is rejected at load with
its actual size in the message, rather than half-working.

Any region or revision will do for CPU work, since none of it depends on the
BIOS's own behaviour beyond reaching the shell hand-over. Once the CD-ROM and
GPU land, revision will start to matter and this file gets a table.

Names the libretro shim looks for in the frontend's system directory, best
first:

```
scph5501.bin  scph5500.bin  scph5502.bin
scph7001.bin  scph1001.bin  scph1000.bin
psxonpsp660.bin  bios.bin
```

The dev harnesses take a path, so they do not care what it is called.

## Why the BIOS is not in save states

It is this console's equivalent of a cartridge ROM: read-only, and identical on
both sides of a netplay session by assumption. Keeping it out of the state keeps
the state small and keeps `retro_serialize_size()` constant. The `fingerprint`
harness prints a checksum of the BIOS alongside the state checksum, so two hosts
comparing fingerprints can tell "different state" from "different BIOS".
