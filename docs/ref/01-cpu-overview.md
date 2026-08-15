# The R3000A CPU, overview and execution model

## 1. What the part actually is

The PlayStation CPU is an **LSI CoreWare CW33300** core (LSI LR33300/LR33310
family), packaged by Sony as `CXD8530BQ` / `CXD8530CQ` (early) or `CXD8606CQ`
(later). It is **MIPS-I / R3000A instruction-set compatible**, 32-bit,
little-endian, and differs from a stock R3000A in two ways that matter:

- **No TLB.** COP0 registers r0, r1, r2, r4 and r10 (Index, Random, EntryLo,
  Context, EntryHi) do not exist. The TLB instructions raise Reserved Instruction.
- **LSI-specific COP0 debug registers** occupy some of the freed indices: BPC (r3),
  BDA (r5), TAR/JUMPDEST (r6), DCIC (r7), BDAM (r9), BPCM (r11). These appear in
  *no* IDT manual.

Clock is 33.8688 MHz (= 44100 × 300 × 256 / 100). Attached coprocessors:

| Coprocessor | Present? | Notes |
|---|---|---|
| COP0 | Yes | System control: exceptions, status, debug. No TLB half. |
| COP1 | **No** | No FPU. Any COP1 access → Coprocessor Unusable, `Cause.CE = 1`. This is why the BIOS's `atof` (A(0Bh)) and `strtod` (A(32h)) lock the machine. |
| COP2 | Yes | The GTE (Geometry Transformation Engine). Requires `SR.CU2`. |
| COP3 | **No** | → Coprocessor Unusable, `Cause.CE = 3`. |

## 2. Register file

32 general-purpose 32-bit registers, plus HI, LO and PC.

| Reg | ABI name | Convention |
|---|---|---|
| r0 | `zero` | **Hardwired to 0.** Every write is discarded, loads, link registers, `MFHI r0`, all of it. |
| r1 | `at` | Assembler temporary |
| r2–r3 | `v0`–`v1` | Function return values |
| r4–r7 | `a0`–`a3` | First four arguments |
| r8–r15 | `t0`–`t7` | Caller-saved temporaries |
| r16–r23 | `s0`–`s7` | Callee-saved |
| r24–r25 | `t8`–`t9` | Caller-saved temporaries |
| r26–r27 | `k0`–`k1` | **Reserved for the kernel / exception handlers.** Clobbered without warning by the BIOS handler. |
| r28 | `gp` | Global pointer |
| r29 | `sp` | Stack pointer |
| r30 | `fp` / `s8` | Frame pointer |
| r31 | `ra` | Return address, written by JAL, JALR (default `rd`), BLTZAL, BGEZAL |

The ABI names are a **software convention only**; the hardware treats r1–r31
identically. Nothing in the CPU enforces them.

**HI / LO** hold multiply and divide results. They are not GPRs: they are only
reachable through `MFHI` / `MFLO` / `MTHI` / `MTLO`, and they are **interlocked on
read**: see [`04-delay-slots-and-hazards.md` §4](04-delay-slots-and-hazards.md).

**PC** is not software-visible except through the link registers and COP0 `EPC`.

## 3. Reset state

| Item | Value at reset |
|---|---|
| PC | **`BFC00000h`**: the uncached (KSEG1) view of the BIOS ROM. BEV-independent. |
| `SR.BEV` (bit 22) | **1**: the first exception vectors into ROM, not into RAM nothing has written yet `[DOC]` |
| `SR.TS` (bit 21) | Set by reset on IDT no-TLB parts; **PSX value unverified** `[?]` |
| `SR.IsC` (bit 16) | **Not initialised by reset** `[DOC]` |
| GPRs, HI, LO | **Undefined.** Zero is the correct deterministic choice for an emulator, determinism is a save-state requirement. |
| `Cause`, `EPC`, `BadVaddr` | Undefined |
| `PRID` (cop0r15) | `00000002h` on `CXD8606CQ`; `00000001h` on `CXD8530BQ`/`CXD8530CQ` `[DOC]`. Return `00000002h`. |

### COP0 state at game entry (i.e. after the BIOS has run) `[DOC]`

Useful when sideloading an EXE without running the BIOS:

```
cop0r3  bpc      = 00000000h
cop0r5  bda      = 00000000h
cop0r6  jumpdest = <varies>
cop0r7  dcic     = 00000000h
cop0r8  badvaddr = FFFFFFFFh    ; the kernel deliberately faults at FFFFFFFFh during boot
cop0r9  bdam     = 00000000h
cop0r11 bpcm     = 00000000h
cop0r12 sr       = 40000000h    ; only CU2 set: GTE enabled, IEc=0, all IM clear
cop0r13 cause    = 00000020h    ; ExcCode = 08h (Syscall): the last exception seen
cop0r14 epc      = <RetadrFromIrq>
cop0r15 prid     = 00000002h (or 00000001h)
```

## 4. The execution model

Two pipeline artifacts are architecturally visible and **must** be modelled even in
an interpreter with no timing model:

1. **The branch delay slot.** The instruction *after* a branch or jump always
   executes, before the branch takes effect, taken or not taken.
2. **The load delay slot.** A load's result is not readable by the very next
   instruction; that instruction still sees the *old* register value.

Everything else about the five-stage pipeline (IF/RD/ALU/MEM/WB) is invisible to
software, because the R3000A interlocks or forwards it.

### 4.1 Canonical interpreter shape

Maintain `pc` (the instruction to fetch) and `npc` (what `pc` becomes). A branch
rewrites `npc`, never `pc`. This one model reproduces *all* nested-branch behaviour
for free; special-casing it is a classic source of bugs.

```
step():
    # 1. Interrupts are level-triggered and checked at the instruction boundary.
    #    See 05-cop0-and-exceptions.md §6 for the exact condition,
    #    and §4.3 below for the GTE-command exception to this ordering.
    if interrupt_ready():
        take_exception(Int, at = pc, in_delay_slot = branch_pending)
        return

    cur_pc  = pc
    if cur_pc & 3: take_exception(AdEL, BadVaddr = cur_pc); return

    instr   = fetch(cur_pc)

    # 2. Advance the branch-delay machinery BEFORE executing: a branch taken by
    #    *this* instruction must flag the *next* one, not itself.
    in_delay_slot = branch_pending
    branch_pending = false
    pc  = npc
    npc = pc + 4

    # 3. The load issued by the previous instruction lands NOW, before this
    #    instruction executes, so this instruction still reads the OLD value,
    #    and an explicit write here beats the arriving load.
    commit_pending_load()

    execute(instr)
```

`cur_pc` is what `EPC` is taken from. `in_delay_slot` is what `Cause.BD` is taken
from.

### 4.2 Two-register-file trick

The standard way to get the load delay exactly right without a real pipeline:

- `regs`: the file as the **current** instruction sees it (reads come from here).
- `out_regs`: the file as it will be **after** this instruction (writes go here).

At the top of each instruction, apply the pending load to `out_regs`; at the bottom,
`regs = out_regs`. An explicit register write by the instruction therefore
overwrites the arriving load, which is the documented behaviour.

Two cancellation rules must be added on top of this, see
[`04-delay-slots-and-hazards.md` §2.4–2.5](04-delay-slots-and-hazards.md).

### 4.3 Where the interrupt check goes, the GTE exception `[DOC]` `[HW]`

If an interrupt occurs **on** a `COP2 imm25` (GTE command) instruction, the GTE
command **is executed anyway**, and `EPC` nevertheless points *at* that
instruction. The BIOS handler compensates:

```
if (Cause & 7Ch) == 00h                    ; ExcCode = Int
   if (mem32[EPC] & FE000000h) == 4A000000h ; the opcode is a cop2cmd
      EPC = EPC + 4                         ; skip it, it already ran
```

An emulator that checks for interrupts *before* executing and then skips the
instruction will have the BIOS skip a GTE command that never ran. **Crash Bandicoot
1/2/3, Jinx and Spyro the Dragon render broken geometry** when this is wrong. Two
workable strategies:

- Execute the GTE command, then take the interrupt with `EPC` pointing at it
  (what the hardware does), or
- Refuse to take an interrupt when `pc` points at a `cop2 imm25` and defer it by one
  instruction (what Mednafen does to sidestep the pipeline nuance).

Note the BIOS fixup cannot work when `Cause.BD` is set, so GTE commands in branch
delay slots are a hazard in real code too. Old BIOS revisions implement the fixup
incompletely (they examine a copy of cop0r13 in r2 without ever moving cop0r13 into
r2, so they examine garbage).

## 5. Addressing and endianness

- **Little-endian.** Byte at `base+0` is bits 7:0 of the word at `base`.
- All instructions are 32-bit and must be word-aligned; a fetch at `PC & 3 != 0`
  raises `AdEL` with `BadVaddr = PC`.
- Effective address for every load/store is `rs + sext(imm16)`, wrapping.
- Segments (detail is out of scope here; see the memory-map reference when written):
  `KUSEG 00000000h`, `KSEG0 80000000h` (cached), `KSEG1 A0000000h` (uncached),
  `KSEG2 C0000000h`. In **user mode** (`SR.KUc = 1`) any access at or above
  `80000000h` raises `AdEL`/`AdES`. PSX software effectively always runs in kernel
  mode.

## 6. Timing, at a glance

RustStation has no timing model yet. These are the numbers it will eventually need;
none of them change *what* an instruction computes, only *when*.

| Operation | Cycles |
|---|---|
| Most instructions | 1 |
| `MULT`/`MULTU` | 1 issue + **6 / 9 / 13** depending on the magnitude of **`rs`** (not `rt`) |
| `DIV`/`DIVU` | 1 issue + **36**, fixed |
| `LW` from scratchpad | 1 |
| `LW` from on-die I/O | 5 |
| `LW` from main RAM | 7 |
| `LW` from BIOS ROM | 27–33 (programmable) |

`MFHI`/`MFLO` **stall** until an in-flight multiply/divide completes; `MFC2`/`CFC2`
and a new GTE command stall until the current GTE command completes. Missing these
two stalls is the usual cause of "the game runs too fast".
