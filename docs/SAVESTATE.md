# Save states

The invariant: **`retro_serialize` output is a pure function of emulator state,
byte-identical across every target triple**, achieved by construction rather
than by matching toolchains. The code is `crates/psx-core/src/save.rs`; this
file records the rules it follows and what its numbers are.

## The rules

1. **Fixed size per version.** A state is always exactly `STATE_SIZE` bytes for
   a given `FORMAT_VERSION`, whatever the machine is doing: every FIFO, queue
   and ring is written as a fixed-width array, padded with zeros. So
   `retro_serialize_size()` is a constant, and the loader can validate the
   whole buffer before touching the machine.
2. **Little-endian, explicit widths.** Every field is written one at a time
   through a byte cursor. Nothing native-layout goes into the stream: no
   `memcpy` of a struct, no `transmute`, no derived serializer. `usize` is never
   serialized, `bool` is a `u8` 0 or 1, and there are no floats anywhere in
   emulator state.
3. **Fixed field order.** No map iteration, nothing whose order depends on the
   host.
4. **A magic and version header.** Eight bytes of magic, then a `u16`
   `FORMAT_VERSION`.
5. **Bump `FORMAT_VERSION` on any layout change.** Added, removed, reordered or
   re-widened fields all count. The netplay handshake trusts the version, so it
   has to be honest.
6. **Target-independent bytes.** Rules 2 and 3 are what give this: the same
   machine state produces the same bytes on x86-64 and arm64, Windows and
   Android. The core is integer-only, and has no hash maps, clocks or threads
   that could leak host behaviour into the state.
7. **Host-side data is excluded.** Anything the frontend owns, and anything that
   only observes the machine, stays out. See below.

## Identity

| | |
|---|---|
| `CORE_ID` | `ruststation-psx` |
| Magic | `RSTAPSX1` (8 bytes) |
| `FORMAT_VERSION` | 18 |
| `STATE_SIZE` | 3 746 100 bytes |

The netplay handshake token is the triple `(CORE_ID, FORMAT_VERSION,
state_size())`. The libretro core also exports it as a string,
`ruststation-psx:18:3746100`, from `ruststation_state_token()`, so a frontend
can refuse a state transfer between mismatched peers up front.

The trailing digit of the magic is a generation marker: it changes only if the
stream stops being a RustStation state at all. Layout changes move the version.

## Layout

Header, then the CPU, then the bus and every device on it, then the frame
carries, in this order:

```
magic            8 bytes  "RSTAPSX1"
format_version   u16

CPU
  regs           32 x u32   register file as the current instruction sees it
  out_regs       32 x u32   register file as it will be after it
  hi, lo         u32 x 2
  pc             u32
  next_pc        u32        the branch delay slot
  current_pc     u32
  load           u8 + u32   pending load: target register, value
  branch         u8
  delay_slot     u8
  cycles         u64
  icache tags    256 x u32  the I-cache's tags and valid bits
  muldiv_ready   u64        cycle the multiplier is done
  gte_ready      u64        cycle the GTE is done
COP0
  bpc bda jump_dest dcic bad_vaddr bdam bpcm sr cause epc prid   11 x u32
COP2 (GTE)       its logical fields: vectors, colour, FIFOs, accumulators,
                 matrices, offsets and FLAG
BUS
  ram            u32 length (always 2097152), then 2 MB
  scratchpad     u32 length (always 1024), then 1 KB
  mem_ctrl       9 x u32
  ram_size       u32
  cache_ctrl     u32
  cycle, next_event, synced_to   u64 x 3   the master clock and scheduler
  irq            stat, mask     u16 x 2
  video          standard, beam position, dot clock fractions, vblank, frames
  root counters  3 x (counter, mode, target, prescaler, irq and sync latches)
  GPU            u32 length, then 1 MB of VRAM; the GP0 FIFO as 16 fixed
                 slots; the transfer in progress; drawing area, offset,
                 draw mode, window and display registers; flags
  DMA            7 channels x (MADR, BCR, CHCR), DPCR, DICR
  SIO0           the port's registers, transfer step and /ACK countdown;
                 each pad's mode (analog, locked, config, rumble mapping,
                 command in progress, L3+R3 latch); each memory card's transfer state
  CD-ROM         registers, parameter and response FIFOs, the response
                 queue, head position, the read in progress and its sector,
                 CD audio (volume matrix, mutes, CD-DA playback, the XA
                 decoder's predictor and resampler, queued frames), the lid
  MDEC           quant and scale tables, the block being assembled, the six
                 decoded blocks and the output buffer
  SPU            the register file; u32 length, then 512 KB of sound RAM;
                 the transfer pointer; each voice's pitch counter, block,
                 predictor, envelope and volumes; key latches, ENDX, noise,
                 main volumes, capture position, interrupt flag, and the
                 cycles owed towards the next sample
FRAME
  frame_frac     u64        the fraction of a cycle carried between frames
  frame_over     u64        cycles the last frame ran past its end
```

The `*_BYTES` constants at the top of `save.rs` are the field-by-field
statement of this layout, and `STATE_SIZE` is derived from them rather than
from `save_state().len()`.

Some choices worth knowing:

- **Both register files are serialized**, not just `regs`. The load delay slot
  means they can genuinely differ at an instruction boundary, and dropping
  `out_regs` would silently lose a pending write.
- **The GTE is written as its logical fields**, not as the 64 register slots
  software sees. Several slots are derived views (`IRGB` is `IR1`..`IR3`
  squeezed to five bits each), so a round trip through the register interface
  would lose precision.
- **`synced_to` is in the stream.** Without it a restored machine would replay,
  or skip, the cycles between the last device sync and the moment the state was
  taken.
- **Queues are written oldest first**, padded with zeros, so the bytes do not
  depend on where a ring buffer happens to start.
- **The frame carries are machine state.** When the frontend paces at one video
  standard while the game runs the other, a frame ends after the instruction
  that crosses its end, and the overshoot is carried into the next. That
  decides the cycle the next input lands on. See "Netplay" below.

## Version history

From the doc comment on `FORMAT_VERSION`, and for 8 and 9 from the commits
that made them:

| Version | Adds |
|---|---|
| 1 | CPU, COP0, the GTE register file, RAM, scratchpad, memory control |
| 2 | The master clock and the timed devices: interrupt controller, video timing, root counters |
| 3 | The GPU (1 MB of VRAM, which doubles the state) and the DMA controller |
| 4 | The GTE's real state, as its logical fields |
| 5 | SIO0, the controller port. Not the pads' buttons |
| 6 | The CD-ROM controller, including its queue of scheduled responses |
| 7 | The SPU's register file and its 512 KB of sound RAM |
| 8 | The CD-ROM's XA filter (file and channel), so `Getparam` reads back what `Setfilter` set |
| 9 | MDEC |
| 10 | The SPU's voices and chip state, once it made sound |
| 11 | CD audio: volume matrix, mutes, CD-DA playback, the XA decoder, queued frames |
| 12 | The drive's lid, for disc swapping |
| 13 | DualShock pad modes: analog, locked, config, the rumble mapping, the command in progress |
| 14 | Memory cards' transfer state and flag byte, not their contents |
| 15 | Instruction timing: the I-cache's tags, and when the multiplier and GTE are done |
| 16 | The frame carries, appended last |
| 17 | Each pad's L3+R3 latch, so both sticks clicked together press the Analog button once, identically on a peer that loaded the state mid-press |
| 18 | The cycles until an MDEC-out DMA transfer in flight completes |

## What is excluded, and why

- **The BIOS image.** This console's ROM, identical on both sides of a session
  by assumption, exactly as a cartridge is. A state belongs to the kernel it was
  made on: a state made on a real BIOS carries that kernel's tables in RAM,
  pointing into its ROM, and does not load usefully on the built-in HLE kernel,
  or the other way round. See [`../bios/README.md`](../bios/README.md) and
  [`notes/HLE.md`](notes/HLE.md).
- **The disc image, and which disc is in the drive.** The frontend's, like the
  image itself. The lid is in the state.
- **What is on a memory card.** The frontend's, like the disc, so that loading
  a state never takes back a save made since. What a card is doing mid-transfer
  is in the state.
- **The buttons held on the pads.** An input the frontend supplies each frame; a
  state that carried them would replay the buttons held when it was taken. The
  pads' own modes are in the state.
- **Host-side observation**: the TTY buffer, the queued PSX-EXE, the bus and GTE
  diagnostic counters, trace output. They do not influence emulated behaviour,
  and keeping them out means a debug session cannot change a state's bytes.

## Restore behaviour

Every rejection happens *before* the first byte is written, so a bad state can
never leave a half-restored machine. That is sound only because the format is
fixed-size: once the length, the magic, the version and the two length prefixes
agree, no read in the restore pass can run off the end.

Rejected: any length other than `STATE_SIZE` (which covers truncated and
oversized in one check), wrong magic, any `format_version` that is not this
build's, and a RAM or scratchpad length prefix that disagrees with the console's
fixed region sizes. All of them return `false`, with no panic and no
out-of-bounds read. There is no migration from older versions: a state from
another version is refused.

## Tests

In `crates/psx-core/src/save.rs`, against a machine driven into a state that
touches every serialized field, with a generated BIOS-shaped image so no
copyrighted dump is needed:

- **`golden_bytes`**: exact header bytes (magic, then version 18 as `12 00`),
  total length pinned to a **literal** (3 746 100, deliberately *not* compared
  against `state_size()`, which would compare the layout to itself), and an
  FNV-1a-64 checksum over the whole buffer.
- **`round_trip_is_byte_identical`**: `serialize -> unserialize -> serialize`
  gives the same bytes.
- **`restored_machine_matches_the_original`**: byte equality would still hold if
  a whole region were dropped from both sides, so the restored machine is also
  compared field by field.
- **`cross_instance_determinism`**: two independently built machines, driven
  identically, serialize identically.
- **`execution_is_deterministic`**: the same after 10 000 instructions of
  execution, which catches nondeterminism that only appears once the CPU runs.
- **`rejects_truncated`, `rejects_oversized`, `rejects_wrong_magic`,
  `rejects_newer_version`**, and **`rejection_does_not_disturb_the_machine`**.
- **`declared_size_matches_serializer`**: the serializer agrees with the size
  derived from the field widths.

The golden test's sensitivity was **proven, not assumed**: swapping two
same-width fields in the serializer turns the checksum red while the length
stays put, which is exactly the failure a length check alone would miss. Redo
that check whenever the golden constants are regenerated. A golden test nobody
has seen fail is not evidence of anything.

## Netplay

Netplay over libretro sends only the buttons. Each peer runs its own copy of
the game, peers compare hashes of `retro_serialize` to notice a split, and a
peer that has split loads the host's state. Two things have to hold:

1. **The same content and presses give the same bytes on every machine.**
2. **A peer that loads the host's state carries on exactly as the host does**,
   whatever it was doing before.

`a_resynced_peer_ends_its_frames_where_the_host_does`, in
`crates/psx-core/tests/timing.rs`, holds down the second at the core level: a
host and a peer with different histories, the peer loads the host's state, and
for 20 frames afterwards both end every frame on the same cycle and finish with
identical states. It failed by 29 cycles on the first frame while the frame
carries were kept out of the state, and passes since version 16.

Across machines, through the shipped libretro library, `retrohost --hash-every
N` prints a hash of the serialized state, the video frames and the audio
samples every N frames, and `--save-at` / `--load-at` take and restore a state
mid-run. The same image and presses on an x86-64 Windows PC and two arm64
Android devices gave equal lines throughout, and a peer with a shifted history
that loaded the host's state matched it from then on. The runs and their
numbers are in [`TESTS.md`](TESTS.md), "Netplay".

What this does not cover: each peer's memory card. A game that finds its own
save on one peer's card and not the other's will split them, and a resync
cannot fix that, since card contents are not in the state.
