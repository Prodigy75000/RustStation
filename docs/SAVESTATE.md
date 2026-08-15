# Save states

RustStation is bound by the in-house core save-state contract,
`TrophyHubResources/specs/play/IN_HOUSE_CORE_SAVESTATE_SPEC.md`. That file is the
authority; this one records how this core satisfies it and what its numbers are.

The invariant: **`retro_serialize` output is a pure function of emulator state,
byte-identical across every target triple**, achieved by construction rather
than by matching toolchains.

## Identity

| | |
|---|---|
| `CORE_ID` | `ruststation-psx` |
| Magic | `RSTAPSX1` (8 bytes) |
| `FORMAT_VERSION` | 1 |
| `STATE_SIZE` | 2 098 837 bytes |

The netplay handshake token is the triple `(CORE_ID, FORMAT_VERSION,
state_size())`. The libretro shim also exports it as a string from
`ruststation_state_token()`.

**Bump `FORMAT_VERSION` on any layout change.** The handshake refuses
state transfer between mismatched peers up front, which only works if the
version is honest.

## Layout

Header, then CPU, then bus, in this order. All integers little-endian, explicit
widths, `bool` as `u8`.

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
  load_reg       u8         pending load target
  load_val       u32        pending load value
  branch         u8
  delay_slot     u8
  cycles         u64
COP0
  bpc bda jump_dest dcic bad_vaddr bdam bpcm sr cause epc prid   11 x u32
COP2 (GTE)
  data           32 x u32
  control        32 x u32
BUS
  ram_len        u32        always 2097152
  ram            2 MB
  scratchpad_len u32        always 1024
  scratchpad     1 KB
  mem_ctrl       9 x u32
  ram_size       u32
  cache_ctrl     u32
  i_stat         u32
  i_mask         u32
```

Both register files are serialized, not just `regs`. The load delay slot means
they can genuinely differ at an instruction boundary, and dropping `out_regs`
would silently lose a pending write.

The GTE register file is serialized even though the GTE's commands are not
implemented, so landing them later does not move the layout and does not force
a version bump.

## What is excluded, and why

- **The BIOS image.** This console's ROM. See [`../bios/README.md`](../bios/README.md).
- **The TTY buffer, the queued PSX-EXE, and the bus / GTE diagnostic counters.**
  Host-side observation. They do not influence emulated behaviour, and keeping
  them out means a debug session cannot change a state's bytes.

## Restore behaviour

Every rejection happens *before* the first byte is written, so a bad state can
never leave a half-restored machine. That is sound only because the format is
fixed-size: once the length, the magic, the version and the two length prefixes
agree, no read in the restore pass can run off the end.

Rejected: any length other than `STATE_SIZE` (which covers truncated and
oversized in one check), wrong magic, any `format_version` that is not this
build's, and a length prefix that disagrees with the console's fixed region
sizes. All of them return `false`, with no panic and no out-of-bounds read.

## Tests

In `crates/psx-core/src/save.rs`:

- **golden-bytes**: exact header bytes, total length pinned to a **literal**
  (2 098 837, deliberately *not* compared against `state_size()`, which would
  compare the layout to itself), and an FNV-1a-64 checksum over the whole
  buffer.
- **round-trip**: `serialize -> unserialize -> serialize` is byte-identical, and
  the restored machine compares equal field by field.
- **cross-instance determinism**: two independently built machines, driven
  identically, serialize identically. Also after 10 000 instructions of
  execution, which catches nondeterminism that only appears once the CPU runs.
- **reject**: truncated, oversized, wrong-magic, newer-version, and "a rejected
  state leaves the machine untouched".

The golden test's sensitivity was **proven, not assumed**: swapping two
same-width fields in the serializer turns the checksum red while the length
stays put, which is exactly the failure a length check alone would miss. Redo
that check whenever the golden constants are regenerated. A golden test nobody
has seen fail is not evidence of anything.
