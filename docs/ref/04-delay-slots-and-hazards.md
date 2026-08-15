# Delay slots, hazards and pipeline artifacts

Everything here is behaviour a **cycle-inaccurate interpreter still has to get
right**. The R3000A interlocks or forwards most of its five-stage pipeline, but two
artifacts are architecturally visible, the branch delay slot and the load delay
slot, and a handful of secondary rules hang off them.

---

## 1. The branch delay slot

### 1.1 Base semantics `[DOC]`

> "Note that the instruction following the branch will always be executed."
>, psx-spx

- Applies to **all** of `J JAL JR JALR BEQ BNE BLTZ BGEZ BGTZ BLEZ BLTZAL BGEZAL
  BC0F/T BC2F/T`.
- The delay slot executes **whether or not the branch is taken**.
- **`SYSCALL` and `BREAK` are not branches**: *"exception opcodes are immediately
  executed (ie. without executing the following opcode)"*. The same is true of every
  exception: exceptions have no delay slot.
- PC-relative target = `$ + 4 + (sext(imm16) << 2)`: relative to the delay slot.
- `J`/`JAL` target = `(delay_slot_pc & F0000000h) | (imm26 << 2)`. The top nibble
  comes from the **delay slot's** PC, which matters only if the jump is the last word
  of a 256 MB region.

### 1.2 The model that gets it right for free

```
instr  = fetch(pc)
cur_pc = pc
in_ds  = branch_pending_from_the_previous_instruction
pc     = npc
npc    = pc + 4
execute(instr)          ; a branch sets npc = target, and sets branch_pending
```

This single model produces all the nested-branch behaviour in §1.3 without any
special case. Special-casing nested branches is a classic bug source.

### 1.3 A branch sitting in another branch's delay slot

Architecturally undefined, IDT: *"A delay slot may not itself be occupied by a jump
or branch instruction; however, this error is not detected and the results of such
an operation are undefined."*, but deterministic on silicon, and **real games do
it**: **Threads of Fate** and **Shadow Master**, which otherwise lock up and glitch.

Given `A` at `X` (taken, target `T_A`) and `B` at `X+4` (taken, target `T_B`):

| Step | Executes | Notes |
|---|---|---|
| 1 | `A` @ `X` | sets `npc = T_A` |
| 2 | `B` @ `X+4` | A's delay slot. Sets `npc = T_B`. `B`'s own PC-relative target is computed from `X+4`, i.e. `X+8+off*4` |
| 3 | instruction @ `T_A` | executes as **B's delay slot** |
| 4 | instruction @ `T_B` | control continues here |

So the first branch's target instruction is executed **exactly once, as the second
branch's delay slot**, and `T_A` is not otherwise entered. If `B` is not taken you
get the normal `A`, `B`, `T_A`, `T_A+4`, … `[CONS]`

**Corollary for exceptions:** an exception at step 3 gives `EPC = X+4` (the address
of `B`) with `Cause.BD = 1`. Returning re-executes `B`: but *not* `A`: so the flow
after the return is `B`, `T_B`, … and the instruction at `T_A` runs a second time.
This asymmetry is exactly why "branches in branch delay slots" is on every
recompiler's difficulty list.

A jump in a delay slot is the same rule, unconditional. `JAL` in a delay slot still
writes `$ra = its own address + 8`, which is the address of the *outer* branch's
target, not a sensible return address, but that is what the hardware does. `[CONS]`

### 1.4 Link values

| Instruction | Link register | Value | When |
|---|---|---|---|
| `JAL target` | r31 | `address_of_JAL + 8` | always |
| `JALR rd, rs` | `rd` (default r31; any of r0..r31 encodable) | `address_of_JALR + 8` | always |
| `BLTZAL rs, off` | r31 | `address_of_branch + 8` | **always, taken or not** |
| `BGEZAL rs, off` | r31 | `address_of_branch + 8` | **always, taken or not** |

Why `+8`: the return address is the instruction *after* the delay slot.

### 1.5 JALR ordering: `rs` is read before `rd` is written `[DOC]` `[CONS]`

IDT's pseudocode uses an explicit temp: `T: temp ← GPR[rs] ; GPR[rd] ← PC + 8 ;
T+1: PC ← temp`.

The confirming observation is in psx-spx: `jalr r31,r31` *does* jump correctly the
first time (it only misbehaves if an IRQ forces a second execution): which is only
possible if `rs` was sampled before `rd` was clobbered.

Same rule for `BLTZAL`/`BGEZAL` when `rs == r31`: **the comparison uses `$ra`'s
value before linking.** `[DOC]`

Writing the link to `r0` (`jalr r0, rs`) is discarded.

### 1.6 Exceptions in a branch delay slot, COP0 side effects

| COP0 field | Behaviour |
|---|---|
| `EPC` (cop0r14) | address of the **branch**, i.e. `faulting_pc − 4` `[DOC]` |
| `Cause.BD` (bit 31) | set `[DOC]` |
| `Cause.BT` (bit 30) | set if the branch is/was to be taken (or is an unconditional jump) `[DOC]` |
| `TAR` (cop0r6) | updated to the branch/jump **destination address** when `BD = 1` and `BT = 1` `[DOC]` |

Do **not** be clever and point `EPC` at the delay slot. IDT: *"if the CPU returned
from the exception to the branch delay instruction itself the branch would not be
taken and the exception would have broken the interrupted program."*

> "Interrupts should always return to EPC+0, no matter of the BD flag. That way, if
> BD=1, the branch gets executed again, that's required because EPC stores only the
> current program counter, but not additionally the branch destination address."
>, psx-spx `[DOC]`

Consequence for a handler that wants to *inspect* the faulting instruction: with
`BD = 1` the faulting opcode is at `EPC + 4`.

For **non-interrupt** exceptions the handler must decide: with `BD = 0` it may return
to `EPC+0` (retry) or `EPC+4` (skip the offending opcode).

`TAR` (cop0r6) is a PSX/LSI-specific register absent from generic MIPS docs, and it
is the only way a handler could recover the branch target. Sony's handler does not
use it, and no known game reads it.

### 1.7 JR/JALR to a misaligned address

```
EPC = BadVaddr = the bad target
Cause.BD = 0            ; the fault happens at the target's fetch, which is not a delay slot
ExcCode  = 04h (AdEL)
```

> "jumps to faulty memory locations are executed without exception, but will trigger
> address errors and bus errors at the target location, ie. EPC (and BadAddr, in case
> of address errors) point to the faulty address, not to the opcode that has jumped
> to that address.", psx-spx `[DOC]`

Since `EPC` is itself misaligned, returning re-faults, effectively fatal.

`[?]` **Open question:** is the delay-slot instruction executed before the fault?
IDT says it is not (*"the instruction in the delay slot is not executed"*), and also
disagrees with psx-spx about `EPC`. A strict precise-exception reading says it
*does* complete, since the fault is detected at the target's instruction fetch. No
hardware test found; no game known to depend on it.

---

## 2. The load delay slot

### 2.1 Base semantics `[DOC]`

> "The loaded data is NOT available to the next opcode, ie. the target register isn't
> updated until the next opcode has completed. So, if the next opcode tries to read
> from the load destination register, then it would (usually) receive the OLD value
> of that register (unless an IRQ occurs between the load and next opcode…)."
>, psx-spx

> "In R30xx family processors all loads are implemented with a delay of one
> instruction. The instruction immediately following a load may not use the
> destination register of the load instruction… The hardware does not enforce this
> restriction nor detect a failure to follow it.", IDT R30xx

Carrying a load delay: **`LB LBU LH LHU LW LWL LWR LWC2`**, plus the coprocessor
moves **`MFC0 MFC2 CFC2`**.

**Exactly one instruction slot.** psx-spx kills the "two slots after coprocessor
reads" rumour: *"that seems to be nonsense (the PSX does finish both COP0 and COP2
reads after ONE opcode)."*

`MFHI`/`MFLO` are **not** load-delayed, they interlock instead (§4).

### 2.2 The precise commit model

The rule is not "the load happens at the end of the delay slot". The observable rule
is:

> The loaded value lands in the target register **after** the delay-slot instruction
> has *read* its source operands, but **before** the delay-slot instruction *writes*
> its own destination.

Reference shape, note the **two cancellation lines**, which are the part naive
implementations omit:

```
state: load_delay      = (reg, value)   ; committed at the START of this instruction
       next_load_delay = (reg, value)   ; set BY this instruction

per instruction:
  1. regs[load_delay.reg] = load_delay.value        ; commit the pending load
     load_delay = next_load_delay ; next_load_delay = none
  2. execute: read operands from regs
  3. on a normal register write (rd):
        regs[rd] = value
        if load_delay.reg == rd: load_delay = none       ; *** CANCEL ***
  4. on a load targeting rt:
        if load_delay.reg == rt: load_delay = none       ; *** CANCEL ***
        next_load_delay = (rt, value)
  regs[0] is forced to 0 after every write.
```

The two-register-file formulation (`regs` for reads, `out_regs` for writes,
`regs = out_regs` at the end of each instruction) is equivalent for rule 3 but **does
not on its own implement rule 4**: see §2.4.

### 2.3 What the delay-slot instruction sees

The **old** value.

```asm
lw   $1, 0($0)
move $2, $1     # $2 = OLD $1
move $3, $1     # $3 = the loaded value
```

This is used as a real anti-emulator check in the wild: the Xenogears "Agemo" patch
loads a register, reads it in the delay slot, and branches on whether it got the new
or the old value to tell hardware from ePSXe. **Skullmonkeys** also requires correct
load-delay handling.

### 2.4 Two loads targeting the same register, back to back

```asm
lw   $1, (a)
lw   $1, (b)
move $2, $1     # $2 = the value $1 had BEFORE BOTH loads
nop
                # $1 = b
```

**The first load's value is discarded and is never architecturally visible.**
`[CONS]`

This is *not* what a naive "each load independently has a one-instruction delay"
model predicts, that model gives `$2 == a`. It is the `WriteRegDelayed`
cancellation in §2.2 rule 4. Two independent hardware-driven emulators implement it
identically; `[?]` no published hardware test writeup was found.

Getting this wrong is a **silent divergence**: nothing crashes, values are just
subtly wrong.

### 2.5 The delay-slot instruction writes the same register

```asm
lw    $1, 0($0)
addiu $1, $0, 42
                  # $1 == 42, unconditionally, whatever the LW fetched
```

**The delay-slot instruction wins and the loaded value is discarded entirely**: not
merely overwritten one instruction later. `[DOC]`

### 2.6 Summary table

| Sequence | What the second instruction sees | Final register value |
|---|---|---|
| `lw r1,(a)` ; `or r2,r1,r0` | old `r1` | `r1 = a` (visible from the next instruction) |
| `lw r1,(a)` ; `addiu r1,r0,42` | (writes `r1`) | **`r1 = 42`**, `a` lost |
| `lw r1,(a)` ; `lw r1,(b)` |, | **`r1 = b`**, `a` lost and never visible |
| `lw r1,(a)` ; `sw r1,(x)` | old `r1` → **the old value is stored** | `r1 = a` |
| `lw r1,(a)` ; `lw r2,0(r1)` | old `r1` used as the base address |, |
| `lw r1,(a)` ; `lwl r1,(b)` | **the pending value `a`** (bypass, §2.7) | merged |
| `lw r1,(a)` ; `beq r1,r2,X` | old `r1` used for the compare | `r1 = a` |
| `lw r1,(a)` ; `mfc0 r1,$12` |, | `r1 = SR`, `a` lost `[CONS]` |

The `sw` row deserves emphasis: **a store in a load delay slot writes the OLD value
of `rt`**, because it reads `rt` as a source before the pending load commits.

### 2.7 LWL/LWR chained pairs, the second *does* see the first's result

**Yes, there is dedicated bypass hardware for exactly this.** `[DOC]`

> "The contents of general register rt are internally bypassed within the processor
> so that no NOP is needed between an immediately preceding load instruction which
> specifies register rt and a following LWL (or LWR) instruction which also specifies
> register rt.", IDT R30xx, Appendix A

> "One exception to the load delay is that Load Word Right and Load Word Left may
> specify a destination register that is the same register used as the destination of
> an immediately preceding load.", IDT R30xx

Precise rule `[CONS]`:

- When `LWL`/`LWR` needs the current value of `rt` to merge into, it takes it from
  the **pending load value** if a load targeting `rt` is in flight; otherwise from
  the register file.
- The bypass applies to **any** preceding load into `rt`, not only to LWL/LWR, so
  `lw r1,(x) ; lwl r1,(y)` merges into `x`'s value.
- When the bypass fires, the in-flight load is **consumed**, not committed and then
  overwritten.
- `LWL`/`LWR` still impose a normal load delay on the *following* instruction. IDT:
  *"the last instruction of the pair will still have the delay slot."*
- **`SWL`/`SWR` do not get this treatment**: `rt` is a source there, subject to the
  ordinary load delay.

**Zen Nihon Joshi Pro Wrestling, Joou Densetsu: Yume no Taikousen** has `lwr` in a
branch delay slot and `lwl` in a load delay slot. `[HW]`

### 2.8 Load delay and branch delay compose independently

Nothing special happens to a pending load when a branch is involved. `[DOC]`

```asm
lw   $1, 0($4)
beq  $1, $0, X     # compares the OLD $1
nop                # this is the LOAD delay slot; $1 updates here
```

and the pending load **survives the branch**:

```asm
lw   $1, 0($4)
bne  $2, $3, TGT
TGT:  or $5, $1, $0   # the instruction at the branch target is the load delay slot;
                      # it sees OLD $1, and $1 updates after it
```

So the load delay slot can be the branch's delay slot, the fall-through, or the
branch target, whichever instruction actually executes next.

### 2.9 Exception during a load delay slot, the pending load completes `[DOC]`

> "…unless an IRQ occurs between the load and next opcode, in that case the load
> would complete during IRQ handling, and so, the next opcode would receive the NEW
> value.", psx-spx

> "Memory read/write opcodes take a 1-cycle delay… however, if an exception occurs
> between the two opcodes, then the read/write operation may finish, and the second
> opcode would probably receive the NEW value.", psx-spx, *Unpredictable Things*

Exact model `[CONS]`: when an exception is taken at instruction *N*,

- the load issued by *N−1* is **committed** to the register file;
- any load issued by *N* itself is **dropped**: *N* did not complete;
- the handler therefore starts with an **empty** load-delay slot;
- `EPC` points at *N* (or *N−4* with `BD = 1`), so after `RFE`+`jr`, *N* re-executes
  and now reads the **new** value.

This is genuine hardware non-determinism: the same instruction sequence produces two
different results depending on whether an IRQ landed. A cycle-inaccurate interpreter
cannot reproduce *when* it happens, only *what* happens when it does. Nothing is
known to depend on it, but the commit-don't-drop rule matters because the BIOS
exception handler itself contains load-delay-slot code.

### 2.10 Exception raised *by* the load

> "If an exception occurs during a load instruction, the rt register is left
> untouched.", psx-spx `[DOC]`

So an `AdEL`/`DBE` from an `LW` creates no pending load; any *previously* pending
load still commits per §2.9.

---

## 3. R0 and encoding aliases

- **Every write to r0 is discarded**, everywhere: load targets, link registers
  (`jalr r0, rs`), `MFHI r0`, `MFC0 r0, $12`. r0 always reads 0, including during a
  delay slot.
- `NOP` = `SLL r0, r0, 0` = word `00000000h`. `SLL r0, r0, N` for any `N` is also a
  no-op. MIPS32's `SSNOP`/`EHB` meanings for `sa = 1`/`3` do **not** exist here.
- **Shift amounts:** the immediate `sa` is 5 bits (≥ 32 is unencodable); the register
  forms use **`rs & 1Fh`** only. `rs = 32` shifts by 0.
- **Shifts take `rt` as the value and `rs` as the amount**: reversed from the ALU
  operand order.
- **Unused operand fields are not checked** and must not be validated. The single
  exception is the REGIMM `rt` field.

---

## 4. MULT / DIV and the HI/LO hazards

### 4.1 Issue and completion `[DOC]`

> "The mul/div opcodes are starting the multiply/divide operation, starting takes
> only a single clock cycle, however, trying to read the result from the hi/lo
> registers while the mul/div operation is busy will halt the CPU until the mul/div
> has completed.", psx-spx

**HI/LO are interlocked on read**: unlike GPR load delays, which are not. The
multiply unit runs in parallel with the integer pipe. The value read is always
correct; the cost is time.

### 4.2 Cycle counts (PSX-measured, excluding the 1-cycle issue) `[DOC]`

| Operation | `rs` range | Cycles |
|---|---|---|
| `MULTU` | `00000000h..000007FFh` | 6 |
| `MULTU` | `00000800h..000FFFFFh` | 9 |
| `MULTU` | `00100000h..FFFFFFFFh` | 13 |
| `MULT` | `\|rs\|` ≤ `7FFh` | 6 |
| `MULT` | `\|rs\|` ≤ `FFFFFh` | 9 |
| `MULT` | otherwise | 13 |
| `DIV` / `DIVU` | any operands | **36**, fixed |

Timing depends on **`rs` only, not `rt`**: "small × large" is much faster than
"large × small", so operand order is observable. Including the issue cycle the
totals are 7 / 10 / 14 and 37.

`[?]` The generic IDT R30xx manual quotes 12 clocks for multiply and 35 for divide.
Those are stock-R30xx numbers and **do not match the PSX**. Use psx-spx's.

Worked example from psx-spx: after `multu 123h, 12345678h` you may insert up to six
cached ALU opcodes, or one 7-cycle main-RAM read, before `mflo` without an extra
stall.

### 4.3 Divide errors, defined garbage, never an exception

See [`02-instruction-encoding.md` §5](02-instruction-encoding.md) for the table.
`MULT`/`MULTU`/`DIV`/`DIVU` **never** raise an exception under any circumstances.

### 4.4 The three HI/LO hazards

**(a) Reading too soon → interlock, not garbage.** `MFHI`/`MFLO` while the unit is
busy stalls the CPU. Model this as a **time cost**, not a data hazard: record
`muldiv_done_at`, and on `MFHI`/`MFLO` advance the cycle counter to it. A zero-cost
multiply is one of the three usual causes of "the game runs too fast" (with a
missing i-cache and instant DMA).

**(b) Starting a MULT/DIV within two instructions *after* an MFHI/MFLO.** `[DOC]`

> "If either of the two preceding instructions is MFHI or MFLO, the results of those
> instructions are undefined.", IDT, Appendix A

The mechanism (IDT §13): an exception inhibits register writeback for most
instructions, but **not** in the multiply unit, once a multiply or divide starts,
its writes to HI/LO cannot be prevented. So an exception can land just in time to
stop an `mfhi` writeback while still letting a subsequent multiply start and
overwrite the data.

psx-spx knows about it but not why: *"there seems to be a strange rule that one
should not touch the lo/hi registers in the next 2 cycles or so… not yet understood
if/when/how that rule applies…?"* `[?]`

**In practice: do not model it.** It is only observable when an exception lands in
the two-instruction window, no game is known to depend on it, and a plain interpreter
that applies HI/LO writes atomically will never see it.

**(c) Writing HI/LO while a multiply is in flight.** `[DOC]`

> "Instructions that write to the HI and LO registers are not interlocked and
> serialized… If a MTHI operation is executed following a MULT, MULTU, DIV, or DIVU
> instruction, but before any MFLO, MFHI, MTLO, or MTHI instructions, the contents of
> the companion special register LO are undefined.", IDT, Appendix A

- `MTHI`/`MTLO` are **not** interlocked and do not stall for an in-flight multiply.
- A second `MULT`/`DIV` issued before the first completes simply replaces the pending
  result; the first is lost.

Pragmatic model, sufficient for all known software `[CONS]`: compute HI/LO
**immediately** at the `MULT`/`DIV`, record `muldiv_done_at`, and use that timestamp
only to stall `MFHI`/`MFLO`. `MTHI`/`MTLO` then overwrite immediately. Real hardware
would let a late in-flight result clobber your `MTHI` value; `[?]` unverified on PSX
and not known to be relied on.

**(d) Corollary for handlers.** Because HI/LO changes cannot be inhibited by an
exception, a handler that wants to preserve HI/LO must save and restore them, which
is essentially the only reason `MTHI`/`MTLO` exist.

---

## 5. Signed overflow

| Opcode | Traps? | Destination on overflow |
|---|---|---|
| `ADD rd,rs,rt` | yes | **`rd` unchanged** |
| `ADDI rt,rs,imm16` | yes | **`rt` unchanged** |
| `SUB rd,rs,rt` | yes | **`rd` unchanged** |
| `ADDU`, `ADDIU`, `SUBU` | no | written, wrapping mod 2³² |

- `ExcCode = 0Ch (Ovf)`; `Cause.BD`/`BT` set as usual in a delay slot; **`BadVaddr`
  is not touched** (only `04h`/`05h` update it).
- The test is on the 32-bit signed result: carry out of bit 30 ≠ carry out of bit 31.
- `ADDI`'s immediate is sign-extended before the check, so `addi rt, rs, 8000h` is
  `rs + (−32768)`.
- **Exception priority:** `Ovf` sits *below* data address errors and *above* `Int`.
- There is **no** shift overflow trap, and multiply/divide never trap.
- Compilers essentially never emit `ADD`/`SUB`/`ADDI`: the PSX SDK uses the `U`
  forms. Where they appear it is hand-written code or an overflow-check idiom.

---

## 6. Alignment and address errors

| Instruction | Faults if | ExcCode | `BadVaddr` |
|---|---|---|---|
| `LB`, `LBU`, `SB` | never (alignment) |, |, |
| `LH`, `LHU` | `addr & 1` | `04h AdEL` | = addr |
| `SH` | `addr & 1` | `05h AdES` | = addr |
| `LW`, `LWC0..3` | `addr & 3` | `04h AdEL` | = addr |
| `SW`, `SWC0..3` | `addr & 3` | `05h AdES` | = addr |
| `LWL`, `LWR`, `SWL`, `SWR` | **never**: the address is force-aligned |, |, |
| instruction fetch | `PC & 3` | `04h AdEL` | = PC |
| any data access ≥ `80000000h` while `SR.KUc = 1` | always | `AdEL`/`AdES` | = addr |

On a faulting store nothing is written; on a faulting load `rt` is untouched.

**Bus errors** (`06h IBE` on fetch, `07h DBE` on data) come from unmapped or locked
regions, not from misalignment. IDT notes that `DBE` is imprecise for stores
(*"the instruction which generated the store data is not guaranteed to be the one
which recognizes the exception"*) and that R30xx parts effectively cannot take a bus
error on a store at all, because of the write buffer. Most emulators never raise
`IBE`/`DBE` and no known game triggers them.

LWL/LWR/SWL/SWR merge tables are in
[`03-instruction-set.md` §6](03-instruction-set.md).

---

## 7. Ordering, the write queue, and "no timing model"

Out of scope for the CPU-core pass, but listed so the assumptions are explicit.

The CPU has a **4-word pass-through write queue**, enabled in KUSEG and KSEG0 and
disabled in KSEG1/KSEG2. It flushes on a load from a queued address, on **any** KSEG1
access (read or write), and when full. **Loads are allowed to overtake queued
stores** except to the same address, so the order seen on the bus is not program
order. `[DOC]`

> "It is important to realize that the write queue's mechanism is only viable for
> normal memory attached to the main CPU, and that any hardware register state
> machine will get messed up by it.", psx-spx

**An interpreter that executes stores synchronously and in program order does not
reproduce this**, which is the *forgiving* direction and the right default. The
consequences to be aware of:

- the "four dummy writes to flush" idiom becomes a no-op you must tolerate;
- store-then-immediately-read-back of an I/O register works in the emulator when it
  might not on hardware;
- you lose the CPU stall on a full queue during DMA, which is one of the mechanisms
  that keeps CPU and DMA in step.

Other timing hazards worth recording now so they are not rediscovered later:

| Hazard | Rule |
|---|---|
| **`MTC0` side effects** | *"software must assume that any such side effects will be unpredictable on the three instruction periods following the execution of an mtc0."* Enabling/disabling a coprocessor: the next two instructions may or may not trap. Enabling interrupts: the enable won't affect the following two. `[DOC]` |
| **`SR.CU2` enable delay** | ~2 clock cycles before COP2 is actually enabled. `[DOC]` |
| **`SR` write timing** `[DOC]` `[?]` | Changing `SR.IEc` 0→1 via `MTC0` **won't trigger an IRQ until after the next opcode**; changing `SR.IM` bits 0→1 **can trigger immediately** (if `IEc` was already set); `RFE` restoring `IEc` 0→1 also triggers immediately. nocash only, the fork dropped this paragraph. |
| **`IEc` and `IM` in one `MTC0`** | IDT: *"not recommended… may result in side effects such as spurious interrupts."* |
| **COP2 register write delay** | 2–3 clock cycles (3 for `IRGB`), counted in **clock cycles, not opcodes**. |
| **GTE command latency** | Fixed per command (RTPS 15, RTPT 23, NCDT 44, …). `MFC2`/`CFC2`/a new GTE op stalls until completion; `MTC2`/`CTC2` do not. |

---

## 8. Correctness checklist

### Exactly right with **no** cycle model

1. Branch delay slot via the `pc`/`npc` model, including nested branches (§1.3).
2. `JAL`/`JALR`/`B*AL` link = branch address + 8; `rs` sampled before `rd` written;
   `B*AL` links unconditionally; `rs == r31` compares the pre-link value.
3. `Cause.BD` / `Cause.BT` / `EPC − 4` / `TAR` on exceptions in branch delay slots.
4. Load delay with **both** cancellation rules, register overwritten by the
   delay-slot instruction (§2.5), and register re-targeted by a second load /
   `MFC0` / `MFC2` / `CFC2` (§2.4).
5. `LWL`/`LWR` bypass of the pending load; `SWL`/`SWR` **not** bypassed.
6. Pending load **committed**, not dropped, when an exception is taken; the load
   issued by the faulting instruction **is** dropped (§2.9).
7. `ADD`/`ADDI`/`SUB` overflow → `Ovf`, destination unmodified.
8. `AdEL`/`AdES` alignment rules; `LWL/LWR/SWL/SWR` never fault; `BadVaddr` only on
   `04h`/`05h`.
9. Divide by zero and `−2³¹ / −1` producing the exact documented values; mul/div
   never trap.
10. r0 write suppression everywhere, including load targets and link registers.
11. `SLLV/SRLV/SRAV` masking `rs & 1Fh`; `rt` is the shifted value.
12. Reserved Instruction on the `N/A` opcode holes, notably primary `14h..17h`.
13. REGIMM decode: bit 16 → condition, `(rt & 1Eh) == 10h` → link.
14. **Discard all stores while `SR.IsC = 1`** (boot blocker).
15. `SR.SwC` as an inert bit.
16. The GTE-command interrupt quirk ([`01-cpu-overview.md` §4.3](01-cpu-overview.md)).

### Needs at least a coarse cycle counter

17. `MFHI`/`MFLO` stalling until the multiply/divide completes.
18. `MFC2`/`CFC2`/a new GTE op stalling until the current GTE command completes.
19. Load cost by region (1 / 5 / 7 / 27–33) and i-cache miss fills.
20. CPU running in parallel with DMA rather than DMA completing instantly.

### Safe **not** to model, no known dependency

21. The MFHI/MFLO → MULT two-instruction hazard (§4.4b).
22. Late in-flight multiply results clobbering an `MTHI`/`MTLO` (§4.4c).
23. Write-queue reordering of loads ahead of stores (§7): keep the ordered,
    synchronous model.
24. `SR.PZ`, `SR.PE`, `SR.TS`, `SR.RE`.
25. `DCIC` bits 12–13 "jump redirection".

---

## 9. Games known to depend on CPU-core behaviour

Useful as a smoke-test list once a renderer exists. `[HW]` unless noted.

| Behaviour | Games | Symptom if wrong |
|---|---|---|
| Load delay slot | **Skullmonkeys**; the **Xenogears "Agemo"** patch uses it as an emulator-detection check | misbehaviour / detected as an emulator |
| `lwr` in a branch delay slot, `lwl` in a load delay slot | **Zen Nihon Joshi Pro Wrestling** |, |
| Branches in branch delay slots | **Threads of Fate**, **Shadow Master** | lockups and graphical glitches |
| `MFC2`/`CFC2` load delay | **Tekken 2** | severe graphical glitching, broken geometry |
| Interrupt on a GTE command | **Crash Bandicoot 1/2/3**, **Jinx**, **Spyro the Dragon** | broken geometry |
| Software COP0 interrupt bits (`Cause` bits 8–9) | **Jackie Chan Stuntmaster**, **MTV Sports** titles |, |
| COP0 debug registers used as scratch storage | **Legacy of Kain: Soul Reaver** (LibCrypt) | must be freely readable/writable |
| Narrow store putting the full 32-bit GPR on the bus | CD-audio **Soundscope** in SCPH-7xxx shells | hangs waiting for an IRQ that never fires |
| MULT/DIV stall + i-cache + non-instant DMA | **Megatudo 2096**, **NBA Jam Extreme**, **Zero Divide**, **Battle Arena Toshinden** | games run too fast |

Worth knowing because they get misattributed: **Skullmonkeys** also needs GPU
coordinate truncation/sign-extension; **Threads of Fate**'s glitches are *only* the
branch-in-delay-slot issue. "Runs too fast" is almost always missing stalls, not a
decoding bug.
