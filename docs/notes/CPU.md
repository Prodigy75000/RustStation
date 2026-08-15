# MIPS R3000A / LSI CW33300

**Written from:** general MIPS R3000 architecture knowledge, pending the
manuals landing in `../ref/`. **This note is provisional.** Every numbered claim
below needs checking against the supplied documentation, and the "Open
questions" section lists the ones that are load-bearing. Cite the document and
section beside each fact as it is confirmed.

Implemented in `crates/psx-core/src/cpu.rs` and `cop0.rs`.

## The shape of it

32 general-purpose registers, `$0` hardwired to zero. `HI`/`LO` for
multiply/divide results. No FPU is fitted, so COP1 is absent. COP0 is the system
control coprocessor, with the TLB half unfitted. COP2 is the GTE.

Reset vector is `0xBFC00000`, the uncached view of the BIOS ROM.

## The two delay slots

Both are visible to software and both are load-bearing for real code.

**Branch delay slot.** The instruction after a branch or jump always executes,
before the branch takes effect. The branch target is relative to the *delay
slot*, not to the branch. Modelled with the `pc` / `next_pc` pair: `pc` is the
instruction to fetch, `next_pc` is what it becomes, and a branch rewrites
`next_pc`.

`jal` and the linking `bcondz` forms write `$ra` with the address *after* the
delay slot. The `bcondz` link happens whether or not the branch is taken.

**Load delay slot.** A load's result is not readable by the very next
instruction; that instruction still sees the old register. Modelled with a
shadow register file: reads come from `regs`, writes go to `out_regs`, and the
pending load is applied to `out_regs` before the instruction executes.

`LWL`/`LWR` deliberately bypass this: the second of an unaligned pair must see
the first's partial result, so their merge reads `out_regs` rather than `regs`.

## Exceptions

Vector is `0xBFC00180` when Status `BEV` (bit 22) is set, `0x80000080` when it
is clear. Status is reset with `BEV` set, so the first exception vectors into
ROM rather than into RAM nothing has written yet.

On entry: the six-bit interrupt-enable / kernel-user mode stack in Status bits
5..0 shifts left by two, the cause code goes into Cause bits 6..2, and `EPC`
takes the faulting address. If the faulting instruction was in a branch delay
slot, `EPC` points at the **branch** and Cause bit 31 (`BD`) is set, so the
handler re-executes the branch rather than landing in a slot with no branch in
front of it.

`RFE` pops the mode stack by shifting Status bits 5..0 right by two. The "old"
pair keeps its value.

Causes the console can raise: interrupt (0), address error on load (4), address
error on store (5), syscall (8), break (9), reserved instruction (10),
coprocessor unusable (11), arithmetic overflow (12).

## Things that look simple and are not

- **`ADDIU` does not mean unsigned.** The `u` means "does not trap". Its
  immediate is still sign-extended. `ADDI`, `ADD` and `SUB` trap on signed
  overflow and must not write their target when they do.
- **Divide does not trap.** Divide-by-zero and the `INT_MIN / -1` overflow both
  produce fixed values, and unchecked code depends on which:
  - `DIV` by zero: `HI` = dividend, `LO` = `0xFFFFFFFF` if the dividend is
    non-negative, `1` if it is negative.
  - `DIV` of `0x80000000` by `-1`: `HI` = 0, `LO` = `0x80000000`.
  - `DIVU` by zero: `HI` = dividend, `LO` = `0xFFFFFFFF`.
- **Status `Isc` (bit 16) isolates the cache.** Stores made while it is set go
  to the I-cache, not to memory. With no cache modelled, the store must be
  **dropped**. Writing RAM anyway corrupts memory during the BIOS's boot-time
  cache scrub, and the damage surfaces much later.
- **Variable shifts use only the low five bits** of `rs`. In Rust a shift of 32
  is a panic in debug and nonsense in release, so the mask is not optional.
- **Unaligned access traps.** `LH`/`LHU`/`SH` on an odd address and
  `LW`/`SW`/`LWC2`/`SWC2` on a non-multiple-of-four raise an address error with
  `BadVaddr` set. `LWL`/`LWR`/`SWL`/`SWR` are the sanctioned way around it and
  never trap.

## Settled by the hardware suite

Recorded here as facts, with what settled them. See [`../TESTS.md`](../TESTS.md).

- **Coprocessor usability is the Status CU bit alone**, not "is it fitted". With
  CU*n* set, an instruction for an absent coprocessor (COP1, COP3, and the COP0
  load/store forms `LWC0`/`SWC0`) is accepted and does nothing observable. Only
  with the bit clear does it raise coprocessor-unusable. Settled by `cpu/cop`,
  which five cases of turned on this.
- **An unrecognised COP0 sub-opcode does not trap.** Settled by `cpu/cop`'s
  `testCop0InvalidOpcode`.

## Closed by the reference set

**Load-delay write conflict: settled, and the implementation was right.** If
instruction N is a load into `$r` and N+1 also writes `$r` explicitly, the
explicit write wins and the loaded value is never architecturally visible. This
was previously carried as an open question resolved by reasoning; `docs/ref/`
states it from the other direction, so it is now documented behaviour.
`explicit_write_in_the_delay_slot_beats_the_load` in `tests/cpu_semantics.rs` is
the regression guard.

The **related** rule that is *not* implemented is a second load to the same
register cancelling the first. See the divergence list below.

## Known divergences from the reference set

From the read in `../ref/06-conformance-notes.md`. Not yet fixed, listed so they
are not rediscovered, roughly in order of how much they matter.

| | What | Consequence |
|---|---|---|
| A1 | An interrupt is taken *instead of* a pending GTE command rather than after it, so the command is dropped | Broken geometry in Crash Bandicoot 1-3, Spyro. **Latent** while GTE commands are no-ops; the dispatch bug is in `cpu.rs` and should be fixed alongside the GTE, or it will be misdiagnosed as a GTE bug |
| A2 | A second load to the same register does not cancel the first | Silently wrong register values. Cheap to fix and worth a unit test before it becomes hard to attribute |
| B1 | `MFC0` of a nonexistent COP0 register (r0, r1, r2, r4, r10) returns 0 instead of raising Reserved Instruction | None known |
| B2 | `Cause.CE` is never written, so a handler cannot tell which coprocessor was refused | The BIOS `atof`/`strtod` failure path |
| B3 | `Cause.BT` and `TAR` (cop0r6) are not set on a delay-slot exception | None known. Cheap, and `next_pc` already holds the target |
| B5 | `SWC2` bypasses the cache-isolation check, so it writes RAM while `Isc` is set | Invariant leak rather than a real bug |
| B6, B7 | COP0 command decoding is stricter than hardware; `LWC0`/`SWC0` are always Coprocessor Unusable | Both marked uncertain in the reference. **Do not chase without a hardware test** |

**B4 is fixed**: an exception now commits the pending load before the handler
runs, so the handler starts with an empty load-delay slot. It was unreachable
until interrupts could actually fire. See `../notes/TIMING.md` and
`an_exception_commits_the_pending_load_before_the_handler_runs`.

## Open questions

These are unsettled. Each names what would settle it.

1. **The I-cache.** Not modelled at all, only isolated-store dropping. Software
   that writes code and jumps into it without a cache flush behaves differently
   on hardware. UltraRust found exactly this to be a shared boot-blocker on N64,
   so it is worth pricing early rather than discovering late.
2. **Scratchpad through KSEG1.** On hardware the scratchpad is the data cache
   and so is not reachable uncached. The bus currently serves it through every
   segment. Serving an access that hardware would fault makes a real bug look
   like working code.
3. **Instruction cycle costs.** Every instruction is one cycle, and
   multiply/divide do not stall `MFHI`/`MFLO`. The scheduler itself now exists
   and runs off a real master clock, so this is the remaining axis rather than
   the whole gap. See [`TIMING.md`](TIMING.md), which also explains why
   `cpu/access-time` cannot pass until there is an I-cache.
4. **COP0 register reads on unassigned indices** return zero here for r16 to
   r31. Deterministic, which save states require; the documented model is "the
   last value read from a valid COP0 register". The r0/r1/r2/r4/r10 case should
   raise Reserved Instruction instead, which is divergence B1 above.
5. **`PRID`** is set to `0x00000002`. Confirmed by the reference set.
