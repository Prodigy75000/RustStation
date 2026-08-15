# Conformance notes: `crates/psx-core` against this reference

A read of `crates/psx-core/src/cpu.rs` and `crates/psx-core/src/cop0.rs` against the
documents in this directory. **Findings only, nothing here has been changed.**

Everything not listed below was checked and matches. In particular the parts that are
usually wrong are already right: the `pc`/`next_pc` branch-delay model, the
`regs`/`out_regs` load-delay model, `J`/`JAL` taking the top nibble from the delay
slot's PC, `JALR` reading `rs` before writing `rd`, `BLTZAL`/`BGEZAL` linking
unconditionally and comparing the pre-link `$ra`, the REGIMM `(rt & 1Eh) == 10h` link
rule, the exact divide-by-zero and `−2³¹/−1` results, `LWL`/`LWR` bypassing the
pending load via `out_regs`, all four `LWL`/`LWR`/`SWL`/`SWR` merge tables, dropping
stores while `SR.Isc` is set, `SR` mode-stack push and `RFE` copy-down, the
`Cause` write mask of `0300h`, and `PRID = 00000002h`.

---

## A. Game-visible

### A1: Interrupts are taken *instead of* a GTE command, not *after* it

`cpu.rs::step`, the `interrupt_ready()` branch, returns before fetching or executing.
When the pending instruction is a `COP2 imm25`, hardware **executes the GTE command
and then takes the interrupt with `EPC` pointing at it**; the BIOS handler then does
`EPC += 4` because it knows the command already ran.

With the current ordering the command never runs, the BIOS skips it anyway, and the
result is a dropped GTE operation on every interrupt that lands on one.

- Reference: [`01-cpu-overview.md` §4.3](01-cpu-overview.md),
  [`05-cop0-and-exceptions.md` §10.3](05-cop0-and-exceptions.md)
- Symptom: **broken geometry in Crash Bandicoot 1/2/3, Jinx, Spyro the Dragon.**
- Two acceptable fixes: execute the instruction and *then* take the interrupt with
  `EPC` at it; or refuse to take an interrupt when `self.pc` points at a word matching
  `(w & FE000000h) == 4A000000h` and defer by one instruction.
- Latent until `gte.rs` does real work, but the *dispatch* bug is in `cpu.rs` and is
  worth fixing at the same time as the GTE, or it will be misdiagnosed as a GTE bug.

### A2: A second load to the same register does not cancel the first

```rust
// step(): the pending load is applied unconditionally
let (reg, val) = self.load;
self.set_reg(reg as u32, val);
self.load = (0, 0);
```

Nothing cancels an in-flight load when the *new* instruction is itself a load
targeting the same register. Hardware discards the first value entirely, it is never
architecturally visible.

```asm
lw   $1, (a)
lw   $1, (b)
move $2, $1     # hardware: $2 = the value $1 had before BOTH loads
                # here:     $2 = a
```

- Reference: [`04-delay-slots-and-hazards.md` §2.2, §2.4](04-delay-slots-and-hazards.md)
- Fix: in every path that sets `self.load` (`op_lb/lbu/lh/lhu/lw/lwl/lwr`, `MFC0`,
  `MFC2`, `CFC2`), first clear `self.load` if it already targets the same register -
  i.e. cancel it before overwriting, rather than letting it commit.
- **Silent divergence.** Nothing crashes; values are subtly wrong. Worth a unit test
  now, since it will be hard to attribute later.

---

## B. Correctness, not yet game-visible

### B1: `MFC0` of a nonexistent COP0 register returns 0 instead of raising RI

`cop0.rs::read` has `_ => 0`. Hardware splits this three ways:

| Index | Hardware | Current |
|---|---|---|
| r0, r1, r2, r4, r10, r32–63 | **Reserved Instruction (0Ah)** | returns 0 |
| r16–r31 | garbage, **no exception** | returns 0, fine |

`CFC0`/`CTC0` already fall through to `IllegalInstruction` in `op_cop0`, which is
correct (they address r32–63). Only the r0/r1/r2/r4/r10 case needs the RI.

- Reference: [`05-cop0-and-exceptions.md` §1](05-cop0-and-exceptions.md)
- Returning 0 for r16–31 is a deliberate determinism choice and is documented in the
  code; leave it, but note that the documented model is "the last value read from a
  valid COP0 register".

### B2: `Cause.CE` (bits 28–29) is never written

`enter_exception` writes `ExcCode` and `BD` but not `CE`. On a Coprocessor Unusable
exception, hardware sets `CE` to the offending coprocessor number, and a handler that
wants to know *which* coprocessor was refused has no other source.

- Reference: [`05-cop0-and-exceptions.md` §3, §7](05-cop0-and-exceptions.md)
- Fix: on `CoprocessorError`, set `CE = (opcode >> 26) & 3`: 1 for COP1, 2 for COP2,
  3 for COP3, 0 for the LWC0/SWC0 path.
- The BIOS's `atof`/`strtod` failure path is the only known consumer.

### B3: `Cause.BT` (bit 30) and `TAR` (cop0r6) are not updated on a delay-slot exception

`enter_exception` sets `BD` but leaves `BT` clear and never writes `jump_dest`.
Hardware sets `BT` when the branch is/was to be taken, and latches the branch target
into `TAR`.

- Reference: [`04-delay-slots-and-hazards.md` §1.6](04-delay-slots-and-hazards.md),
  [`05-cop0-and-exceptions.md` §5.3, §7](05-cop0-and-exceptions.md)
- No known game reads either. Cheap to add while the exception path is being touched:
  `next_pc` already holds the branch target at the moment the exception is raised.
- Note this is the newer, mechanistic model of cop0r6; nocash's older "randomly
  memorized jump address" description is the same behaviour observed from software.

### B4: A pending load is not committed when an exception is taken

The interrupt and misaligned-fetch paths in `step()` return before the
`self.load` commit, so the pending load survives into the handler and lands on the
handler's *first* instruction. Hardware commits it at exception entry, and the handler
starts with an empty load-delay slot.

- Reference: [`04-delay-slots-and-hazards.md` §2.9](04-delay-slots-and-hazards.md)
- The register ends up with the right value either way; only the instruction at which
  it becomes visible shifts by one. It matters because the BIOS exception handler
  itself contains load-delay-slot code.
- Also: `exception()` does not clear `self.load`, so a load issued by an instruction
  that then faults could survive. In practice every load path returns before setting
  `self.load` when it faults, so this is currently unreachable, but it is an
  invariant worth asserting rather than relying on.

### B5: `LWC2`/`SWC2` bypass the cache-isolation check

`op_swc2` calls `bus.store32` directly instead of going through `Cpu::store`, so a
`swc2` executed while `SR.Isc` is set writes RAM. The comment on `Cpu::store` says the
check exists "in exactly one place", this is the exception to that.

- Reference: [`05-cop0-and-exceptions.md` §2](05-cop0-and-exceptions.md)
- No real code does `swc2` with the cache isolated; this is an invariant leak, not a
  bug in practice.

### B6: COP0 command decoding is stricter than hardware

`op_cop0` matches `instr.s() == 0x10` (bit 25 set **and** bits 24..21 zero) and then
requires `funct == 0x10`, raising RI otherwise. Hardware:

| cofun | Hardware | Current |
|---|---|---|
| 01h, 02h, 06h, 08h (TLBR/TLBWI/TLBWR/TLBP) | **RI (0Ah)** | RI ✓ |
| 10h (RFE) | RFE | RFE ✓ |
| 00h, 03h–05h, 07h, 09h–0Fh, 11h–1Fh | **execute with no exception**, no effect | RI ✗ |
| 20h–1FFFFFFh | mirrors of 00h–1Fh | RI ✗ |

- Reference: [`02-instruction-encoding.md` §6](02-instruction-encoding.md)
- The mirror rule implies matching `RFE` on the low **5** bits; masking 6 bits (as
  here) is the common and safe choice. `[?]` Both details are nocash-only and
  untested. **Low priority, do not chase this without a hardware test.**

### B7: `LWC0`/`SWC0` are treated as plain Coprocessor Unusable

`0x30 | 0x31 | 0x33` and `0x38 | 0x39 | 0x3B` all raise `CoprocessorError`
unconditionally. For COP1/COP3 that is right. For **COP0** (`LWC0` = 30h,
`SWC0` = 38h) hardware raises CpU only when `SR.CU0 = 0`: **and does so even in
kernel mode**: while with `CU0 = 1` it does something glitchy but exception-free
(`SWC0` stores the *next opcode word* to memory; `LWC0` does a dummy read).

- Reference: [`02-instruction-encoding.md` §7](02-instruction-encoding.md)
- `[?]` nocash's own text hedges on the `CU0 = 1` behaviours. No commercial game is
  known to touch these. **Leave as-is; recorded so it is not rediscovered.**

---

## C. Known-absent, already tracked

Listed for completeness, these are acknowledged in the code's own doc comments and
in `docs/notes/CPU.md`.

| Missing | Reference | Consequence |
|---|---|---|
| Any timing model, every instruction is 1 cycle | [`04` §4.2](04-delay-slots-and-hazards.md) |, |
| `MULT`/`DIV` do not stall `MFHI`/`MFLO` | [`04` §4.4a](04-delay-slots-and-hazards.md) | one of the three usual causes of "runs too fast" |
| No instruction cache | [`04` §8](04-delay-slots-and-hazards.md) | Formula One 99; self-modifying-code titles |
| No COP0 hardware breakpoints (BPC/BDA/DCIC stored but inert), so the `80000040h` vector is unused | [`05` §5](05-cop0-and-exceptions.md) | none known, but the registers **must** stay freely readable/writable (Soul Reaver LibCrypt) |
| `MFC2`/`CFC2` load delay | [`03` §7](03-instruction-set.md) | **Tekken 2**: `op_cop2` already routes both through `self.load`, so this is ✓ present. Listed only to confirm |
| GTE register write delay (2–3 cycles), GTE command latency and stalls | [`04` §7](04-delay-slots-and-hazards.md) | latent until the GTE is real |
| Write-queue reordering | [`04` §7](04-delay-slots-and-hazards.md) | deliberate; the ordered model is the forgiving one |

---

## D. Open question already flagged in `cpu.rs`

The header comment says:

> "the pending load is applied to `out_regs` *before* the instruction executes, so an
> explicit write by that instruction wins over the arriving load. (That precedence is
> the one part of this taken on reasoning rather than a documented statement -
> `docs/notes/CPU.md` lists it as an open question for the conformance suite to
> settle.)"

**This is documented, and the implementation is correct.** `[DOC]`

> "You might think that since the LW finishes after the load delay slot its fetched
> value will override the one set by the ADDIU. It turns out that it's not the case
> however: after those two instructions $1 will contain 42, no matter what the LW
> fetched."

psx-spx states the same rule from the other direction. See
[`04-delay-slots-and-hazards.md` §2.5](04-delay-slots-and-hazards.md) and the summary
table in §2.6. The open question can be closed.

The *related* rule that is **not** implemented is the second cancellation, a new load
to the same register, which is A2 above.

---

## E. Test coverage

`tests/test-suite/cpu/` currently holds `access-time`, `code-in-io`, `cop` and
`io-access-bitwidth`. Only `cop` exercises the CPU core directly; the other three are
bus and timing tests. **None of the findings above is covered by an existing test.**

Suggested unit tests, all writable today with no bus or timing model:

| # | Test | Catches |
|---|---|---|
| 1 | `lw r1,(a)` ; `lw r1,(b)` ; `move r2,r1` → `r2` is the pre-load value | **A2** |
| 2 | `lw r1,(a)` ; `addiu r1,r0,42` → `r1 == 42` | §2.5 (already correct, regression guard, closes D) |
| 3 | `lw r1,(a)` ; `sw r1,(x)` → the **old** `r1` is stored | §2.6 |
| 4 | `lw r1,(a)` ; `lwl r1,3(b)` ; `lwr r1,0(b)` → merges into `a` | §2.7 (already correct) |
| 5 | Branch in a branch delay slot: `A`, `B`, `[T_A]`, `[T_B]` execution order | §1.3 |
| 6 | `bgezal r31, x` with `r31 < 0`: compares the pre-link value, links anyway | §1.4/1.5 (already correct) |
| 7 | REGIMM `rt = 12h` behaves as `BLTZ` **without** linking; `rt = 1Fh` as `BGEZ` | §4 encoding |
| 8 | Exception in a delay slot: `EPC == branch_addr`, `BD == 1`, `BT` set, `TAR` set | **B3** |
| 9 | `mfc0 r1, $0` raises RI (0Ah) | **B1** |
| 10 | `cop2` command with an interrupt pending: the command executes, `EPC` points at it | **A1** |
| 11 | `addi` overflow leaves `rt` unchanged and raises `Ovf` | §5 (already correct) |
| 12 | `sltiu r1, r0, -1` → 1 (sign-extended immediate, unsigned compare) | §2 extension rule |
| 13 | Primary opcode `14h` raises RI, not a branch | §2 reserved-slot rule |
| 14 | All four `LWL`/`LWR`/`SWL`/`SWR` alignments, both directions | §6 tables |

Beyond unit tests, the standard third-party CPU conformance ROMs (amidog's CPU/COP0
tests, and JaCzekanski's `ps1-tests` CPU set) are **not** currently in
`tests/test-suite/` and would cover most of the above against captured hardware logs.

---

## F. Nothing here is a licence to copy

Every finding above is stated as **hardware behaviour plus the observable symptom**,
with a reference to the document that derives it from psx-spx / nocash / IDT. Where a
claim is marked `[CONS]` in those documents it records that two independent emulators
*behave* a certain way on a documented-ambiguous case, that is evidence about the
silicon, not an implementation to copy.

Implement each fix from the described behaviour and the test that pins it. See the
clean-room note in [`README.md`](README.md).
