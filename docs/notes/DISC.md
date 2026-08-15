# Disc images

Written from the Red Book / Yellow Book sector layouts and the cue sheet format,
as described in the public PlayStation hardware documentation. No third-party
emulator source consulted.

Implemented in `crates/psx-core/src/disc.rs`.

## The interface

A disc is a **table of contents plus a function from LBA to 2352 raw bytes**.
That is the whole of it, and keeping it that small is the point: the CD-ROM
controller never learns what a cue sheet is, so CHD can be added later as a
second implementation without touching `cdrom.rs`.

## Why raw 2352-byte sectors

A 2048-byte-per-sector image keeps only the user data. Four things this machine
can observe are not in it:

* the 4-byte **header**, minute, second, frame and mode, which is exactly what
  `GetlocL` returns,
* the 8-byte **subheader**, file, channel, submode and coding, which is what
  `Setfilter` selects on,
* **Mode 2 Form 2** sectors, where XA audio and MDEC video live, and
* **CD-DA** tracks, which have no user-data area at all.

A 2048-byte image is still accepted, and widened on read with its sync pattern
and header synthesised. That is honest for `GetlocL` and wrong for anything
checking the ECC, which is a trade worth making but not worth hiding.

## Numbers

| | |
|---|---|
| Raw sector | 2352 bytes |
| Sync | bytes 0..12 |
| Header | bytes 12..16, minute, second, frame, mode, and the MSF is BCD |
| Subheader | bytes 16..24, twice over |
| Mode 2 Form 1 user data | bytes 24..2072 |
| Mode 1 user data | bytes 16..2064 |
| Lead-in | 150 sectors, so LBA 0 is MSF `00:02:00` |
| Frames per second | 75 |

## Traps

* **LBA 0 is `00:02:00`, not `00:00:00`.** Reversing that puts every seek two
  seconds out, which reads as a disc that almost works rather than one that
  obviously does not.
* **MSF is BCD.** Minute 39 is `0x39`, not `0x27`.
* **A track ends where the next track's pregap begins**, not where its audio
  does. Using `INDEX 01` for both the start and the previous track's end loses
  the gap.
* **`PREGAP` and `INDEX 00` are not the same thing.** `INDEX 00` is a pregap
  that *is* in the file; `PREGAP` is one that is not, and it shifts every later
  LBA without consuming a byte. Treating them alike misplaces every track after
  the first gap.
* **An unrecognised track mode is refused, not guessed at.** A wrong sector size
  reads as a corrupt disc, which is a far worse thing to debug than a clear
  error at load time.

## Open questions

1. **CHD is not supported.** It holds the same raw 2352-byte sectors, so nothing
   is lost by it; the cost is purely decoding, which means hunk decompression
   across several codecs plus a metadata format. Worth doing, and it belongs
   entirely in this file.
2. **Multi-file cue sheets are implemented but untested.** One `FILE` per track
   is common in the wild. The accumulation of the base LBA across files is the
   part most likely to be wrong, and no test here covers it because building a
   convincing fixture needs more than one image.
3. **Sub-channel data is not modelled at all.** `GetlocP` currently derives the
   position arithmetically from the track table rather than reading a Q
   sub-channel, which is right for a data track and approximate for audio.
