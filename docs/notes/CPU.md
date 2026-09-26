# MIPS R3000A / LSI CW33300

**Written from:** the reference set in `../ref/` (psx-spx, nocash's original
PSX-SPX, and the IDT R30xx Family Software Reference Manual for generic MIPS-I
semantics), and the hardware logs of the ps1-tests `cpu` suite. The note was
first drafted from general MIPS R3000 knowledge and has since been read against
that set: "Settled by the hardware suite" and "Closed by the reference set"
record what was confirmed, "Known divergences" what the read found wrong, and
"Open questions" what is still unsettled.

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
  to the I-cache, not to memory, so RAM must **not** be written. Writing it
  anyway corrupts memory during the BIOS's boot-time cache scrub, and the
  damage surfaces much later. With the cache control register's tag bit set,
  the store writes an I-cache tag, which is how the BIOS flushes the cache;
  otherwise it would write cached code, which is not kept (see open question 2).
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

**A second plain load to the same register cancels the first: implemented.**
The first value is never architecturally visible.
`a_second_load_cancels_the_first` in `tests/cpu_semantics.rs` pins it, and was
proven to fail without the cancellation.

`LWL`/`LWR` are the deliberate exception: they merge with a pending load, so
the in-flight value has to reach them, and they bypass `Cpu::set_load`. Whether
they should *also* cancel is an open question below.

## Known divergences from the reference set

From the read in `../ref/06-conformance-notes.md`. Not yet fixed, listed so they
are not rediscovered, roughly in order of how much they matter.

| | What | Consequence |
|---|---|---|
| B1 | `MFC0` of a nonexistent COP0 register (r0, r1, r2, r4, r10) returns 0 instead of raising Reserved Instruction | None known |
| B2 | `Cause.CE` is never written, so a handler cannot tell which coprocessor was refused | The BIOS `atof`/`strtod` failure path |
| B3 | `Cause.BT` and `TAR` (cop0r6) are not set on a delay-slot exception | None known. Cheap, and `next_pc` already holds the target |
| B5 | `SWC2` bypasses the cache-isolation check, so it writes RAM while `Isc` is set | Invariant leak rather than a real bug |
| B6, B7 | COP0 command decoding is stricter than hardware; `LWC0`/`SWC0` are always Coprocessor Unusable | Both marked uncertain in the reference. **Do not chase without a hardware test** |

**B4 is fixed**: an exception now commits the pending load before the handler
runs, so the handler starts with an empty load-delay slot. It was unreachable
until interrupts could actually fire. See `../notes/TIMING.md` and
`an_exception_commits_the_pending_load_before_the_handler_runs`.

**A1 is fixed**: an interrupt no longer swallows a pending GTE command. When the
instruction about to run is a `COP2 imm25`, the interrupt is deferred by one
instruction so the command executes first. Taking it the other way round means
the command is skipped on the way in and skipped again by the BIOS handler, so
it never runs, and every interrupt landing on one silently drops a geometry
operation. `an_interrupt_does_not_swallow_a_gte_command` pins it, and was proven
to fail without the check.

## Open questions

These are unsettled. Each names what would settle it.

1. **Does `LWL`/`LWR` cancel a pending load as well as merging with it?** A
   second *plain* load to the same register cancels the first (implemented). An
   `LWL` merging with a pending load clearly consumes its value, but whether it
   additionally discards it is not documented either way. The two differ only in
   what a read inside the `LWL`'s own delay slot sees. This core does not
   cancel, which is the conservative reading.

   `lwl_delay_slot_sees_the_cancelled_load` in `tests/cpu_semantics.rs` records
   the alternative as an `#[ignore]`d test, per the convention in
   `../ref/README.md`. Un-ignore it if hardware says the other thing.

   Worth knowing: `lwl_merges_with_a_pending_load` does **not** distinguish the
   two, because `op_lwl` samples `out_regs` before any cancellation would
   apply. That was checked by patching `LWL` to cancel and watching the test
   stay green, rather than assumed.
2. **The I-cache's contents.** Since 2026-09-26 its tags are modelled
   (`crates/psx-core/src/timing.rs`): which fetches hit, what a miss costs, and
   the tag writes the BIOS flushes it with. The instructions themselves still
   come from RAM. On hardware a line keeps the code it was filled with until it
   is flushed, so software that writes code and jumps into it without a flush
   runs the old code there and the new code here. psx-spx names a game that
   depends on this; stale instruction caches are a known bug class in emulator
   cores generally, so it is worth pricing before it is met.
3. **Scratchpad through KSEG1.** On hardware the scratchpad is the data cache
   and so is not reachable uncached. The bus currently serves it through every
   segment. Serving an access that hardware would fault makes a real bug look
   like working code.
4. **Instruction cycle costs** landed 2026-09-26: I-cache hits and misses,
   what loads cost by region, and the multiplier's and the GTE's waits
   (`crates/psx-core/src/timing.rs`, [`TIMING.md`](TIMING.md)). Not yet: the
   write queue, the load shadow as its own mechanism, and DMA taking the bus.
5. **COP0 register reads on unassigned indices** return zero here for r16 to
   r31. Deterministic, which save states require; the documented model is "the
   last value read from a valid COP0 register". The r0/r1/r2/r4/r10 case should
   raise Reserved Instruction instead, which is divergence B1 above.
6. **`PRID`** is set to `0x00000002`. Confirmed by the reference set.
