# COP0 and the exception model

COP0 is the System Control Coprocessor. On the PlayStation only the
exception/status half is fitted, **there is no TLB**: so the register file is
sparse, and the freed indices carry LSI-specific debug registers that appear in no
IDT manual.

---

## 1. COP0 register file

| Idx | PSX name | Exists? | Access | Standard R3000A meaning | PSX behaviour |
|---|---|---|---|---|---|
| r0 |, | **No** |, | `Index` (TLB) | `MFC0`/`MTC0` → **RI (0Ah)** |
| r1 |, | **No** |, | `Random` (TLB) | **RI (0Ah)** |
| r2 |, | **No** |, | `EntryLo` (TLB) | **RI (0Ah)** |
| **r3** | **BPC** | Yes | R/W | *(unused)* | Breakpoint-on-execute address |
| r4 |, | **No** |, | `Context` (TLB) | **RI (0Ah)** |
| **r5** | **BDA** | Yes | R/W | *(unused)* | Breakpoint-on-data-access address |
| **r6** | **TAR** / JUMPDEST | Yes | **R only** | *(unused)* | Branch/jump target address, §5.3 |
| **r7** | **DCIC** | Yes | R/W | *(unused)* | Debug & Cache Invalidate Control, §5.1 |
| **r8** | **BadVaddr** | Yes | **R only** | `BadVAddr` | Faulting address, **only** for ExcCode 04h/05h |
| **r9** | **BDAM** | Yes | R/W | *(unused)* | Data-access breakpoint mask |
| r10 |, | **No** |, | `EntryHi` (TLB) | **RI (0Ah)** |
| **r11** | **BPCM** | Yes | R/W | *(unused)* | Execute breakpoint mask |
| **r12** | **SR** | Yes | R/W | `Status` | §2 |
| **r13** | **CAUSE** | Yes | **R, except bits 8–9 R/W** | `Cause` | §3 |
| **r14** | **EPC** | Yes | **R only** | `EPC` | Exception return address |
| **r15** | **PRID** | Yes | **R only** | `PRId` | §4 |
| r16–r31 | *(garbage)* | decode holes | R (garbage), writes ignored | *(undefined)* | **No exception.** See below |
| r32–r63 |, | **No** |, | never used on any MIPS | **RI (0Ah)**: this is what makes `CFC0`/`CTC0` illegal |

### Unimplemented indices `[DOC]`

- **r0, r1, r2, r4, r10 and r32–r63** → **Reserved Instruction Exception (0Ah)**, not
  Coprocessor Unusable. Because r32–63 are the "control register" bank, this is what
  makes **`CFC0`/`CTC0` raise RI**.
- **r16–r31 → garbage, no exception.** psx-spx (COP0 Register Summary): reading
  them raises nothing and returns an unpredictable value. Read soon after a valid
  COP0 register, the value usually repeats that register's value; read later it is
  typically `00000020h`, and later still `00000040h` or even `00000100h`. The source
  offers no explanation for the pattern.

  Practical model: return the **last value read from any valid COP0 register**. The
  `20h`/`40h`/`100h` decay is unexplained `[?]`: treat as don't-care. (With
  `[FFFE0130h].bit11 = 0`, reading these reportedly returns the 32-bit opcode about
  to be executed.)

  **Privilege quirk:** the garbage registers can be read *without* an exception even
  in **user mode with COP0 disabled** (`SR.KUc = 1`, `SR.CU0 = 0`); accessing any
  *other* existing COP0 register, or executing `RFE`: in that state raises
  **CpU (0Bh)**.

### Load/store delay on COP0 `[DOC]`

- **`MFC0` has a one-instruction load delay.** psx-spx explicitly rejects the claim
  of a two-instruction delay: on the PSX both COP0 and COP2 reads complete after one.
- **`MTC0` has no store delay**, with one exception: setting `SR.CU2` (bit 30) is
  delayed, and psx-spx puts the lag before COP2 is really enabled at roughly two clock
  cycles.

### The debug registers are real scratch storage `[DOC]`

Games use them as hiding places, so they must be fully readable and writable:

- **Legacy of Kain: Soul Reaver** (and probably others) stores LibCrypt
  copy-protection values in the debug registers; psx-spx notes they serve purely as
  an out-of-the-way place to keep data, not for debugging.
- Cheat devices use COP0 execute breakpoints as boot hooks (`BPC = BFC06xxxh` on
  older firmware, `BPC = 80030000h` on later; the Xplorer has a cheat code type built
  on them).

---

## 2. cop0r12, SR (Status Register)

| Bit | Name | Meaning | PSX status |
|---|---|---|---|
| 0 | **IEc** | Current Interrupt Enable (0 = disable) | **Used.** The global IRQ gate |
| 1 | **KUc** | Current Kernel/User Mode (**0 = Kernel, 1 = User**) | Used; PSX code effectively always runs kernel mode |
| 2 | **IEp** | Previous Interrupt Enable | Used |
| 3 | **KUp** | Previous Kernel/User Mode | Used |
| 4 | **IEo** | Old Interrupt Enable | Used. **Left unchanged by RFE** |
| 5 | **KUo** | Old Kernel/User Mode | Used. Left unchanged by RFE |
| 6–7 |, | Not used | Read 0 |
| 8 | **Im0** | Mask for **Sw0** (`Cause` bit 8) | Writable, functional |
| 9 | **Im1** | Mask for **Sw1** (`Cause` bit 9) | Writable, functional |
| **10** | **Im2** | Mask for hardware Int0, on PSX **the one and only external IRQ line** | **The important one.** Must be 1 for any PSX IRQ |
| 11–15 | Im3–Im7 | Masks for hardware Int1–Int5 | Writable bits, but `Cause` bits 11–15 are **always 0** on PSX → no effect |
| **16** | **Isc** | **Isolate Cache** (0 = no, 1 = isolate) | **Used heavily by the kernel** for cache flushing. See below |
| 17 | **Swc** | Swapped cache mode | **No observable effect on PSX**: see below |
| 18 | **PZ** | Cache parity bits written as 0 and not checked | Writable, inert |
| 19 | **CM** | **Cache Miss**: result of the last isolated load: set if the cache really held data for that address | Hardware-updated status bit |
| 20 | **PE** | Cache parity error; does **not** raise an exception | Effectively inert |
| 21 | **TS** | TLB Shutdown | No TLB. On IDT no-TLB parts this is **set by reset** and is **read-only**. `[?]` PSX value unverified |
| **22** | **BEV** | Boot Exception Vectors (0 = RAM/KSEG0, 1 = ROM/KSEG1) | **Set to 1 by reset**; the BIOS clears it. §7 |
| 23–24 |, | Not used | Read 0 |
| 25 | **RE** | Reverse endianness in user mode only | psx-spx questions whether the bit exists on the PSX at all `[?]`: treat as writable and inert; do **not** implement byte swapping |
| 26–27 |, | Not used | Read 0 |
| 28 | **CU0** | COP0 enable (0 = kernel mode only, 1 = kernel **and** user) | Used |
| 29 | **CU1** | COP1 enable | No COP1 → CpU (0Bh, `CE = 1`) regardless |
| **30** | **CU2** | COP2 enable, **the GTE** | **Critical.** Must be 1 to use the GTE. Takes ~2 cycles to take effect |
| 31 | **CU3** | COP3 enable | No COP3 → CpU (0Bh, `CE = 3`) |

**Writability:** the whole register is `MTC0`-writable except **TS (bit 21)**, which
IDT calls read-only. Bits 6–7, 23–24 and 26–27 read back 0 regardless.

### Isc (bit 16): isolate cache

When set, **all loads and stores are directed to the cache instead of main memory**.
Which cache and which part depends on the BIU/cache-config register at `FFFE0130h`.

**Minimum viable emulation: while `SR.Isc = 1`, discard every store, never touch
RAM or I/O.** The BIOS `FlushCache` stores zeroes across `0000h..0FFFh`; letting
those through wipes the bottom 4 KB of kernel RAM and the console never boots. This
is the most common first-boot bug in a new PSX emulator.

Note `SR.Isc` is **not initialised by reset** `[DOC]`.

### Swc (bit 17): sources conflict

- **nocash:** gives the generic MIPS meaning, the instruction and data caches trade
  roles, and notes the PSX kernel does not use the bit.
- **consoledev fork, hardware-tested:** reports no observable effect on the PSX;
  `IsC` together with `SwC` behaves exactly like `IsC` alone, and the kernel never
  sets it. It also reports that setting bit 17 does not make the scratchpad
  executable: instruction fetches from it still raise a bus error.

**Implement `SwC` as a writable, inert bit.** The generic MIPS meaning does not apply
because the PSX's d-cache is wired as the scratchpad.

### SR write timing `[DOC]` `[?]`

nocash only, the fork dropped this paragraph, and it is single-sourced. nocash
PSX-SPX (COP0 Status Register) reports that raising `SR.bit0` (`IEc`) from 0 to 1
with `MTC0` cannot trigger an IRQ until the next instruction has executed; raising
any of `SR.bit8..15` from 0 to 1 can trigger one at once, provided `SR.bit0` is
already set; and `RFE` raising `SR.bit0` from 0 to 1 also triggers at once.

So an `MTC0` that sets `IEc` gives one instruction of grace; one that sets an `IM`
bit does not; `RFE` does not.

---

## 3. cop0r13, CAUSE

**Read-only except bits 8–9.** `MTC0` to `Cause` must mask to `00000300h`;
everything else is ignored. A naive full-width write produces phantom exceptions -
software cannot set `ExcCode` or `IP2`.

| Bit | Name | Meaning | `MTC0`-writable? |
|---|---|---|---|
| 0–1 |, | Not used | No |
| **2–6** | **ExcCode** | 5-bit exception code (§6). Positioned so `Cause & 7Ch` indexes a word table with no shift | No |
| 7 |, | Not used | No |
| **8** | **IP0 / Sw0** | Software interrupt 0, a real R/W **latch** | **Yes** |
| **9** | **IP1 / Sw1** | Software interrupt 1, a real R/W **latch** | **Yes** |
| **10** | **IP2 / Int0** | **The PSX hardware interrupt line.** Set live whenever `(I_STAT & I_MASK) != 0`. **Not a latch**: clears automatically when the condition goes false | No (hardware-driven) |
| 11–15 | IP3–IP7 | Hardware IRQ 1–5 | **Always 0 on PSX** |
| 16–27 |, | Not used | No |
| **28–29** | **CE** | Coprocessor number, for a CpU exception | No |
| 30 | **BT** | **Branch Taken**: meaningful only when `BD = 1`; set if the branch is/was to be taken (or is an unconditional jump) | No |
| **31** | **BD** | **Branch Delay**: set when `EPC` points at the *branch* rather than at the faulting delay-slot instruction | No |

**`Cause.IP` is live state, not exception state.** The IDT R30xx manual (Ch. 3)
stresses that these bits reflect the interrupt inputs as they are now, not as they
were when the exception was taken. So
`IP2` is still set while the handler reads `Cause`, and stays set until the handler
acknowledges `I_STAT`. A handler that `RFE`s before acking re-enters immediately.

**`CE` (bits 28–29).** `[?]` The fork and IDT describe it as set only on CpU
exceptions, from the offending instruction's coprocessor number. nocash describes it
more literally as a copy of opcode bits 26..27, which are the coprocessor number
only when the opcode is a COP instruction, and that hints it may latch
unconditionally. Untested. **Standard practice: write
`CE = (opcode >> 26) & 3` only on CpU.**

**`BT` (bit 30).** nocash lists it as undocumented: with `BD = 1` it holds the branch
condition, 0 meaning not taken. The fork gives it the `BT` name and the same
substance. IDT documents bit 30 as
reserved-zero on stock R3000A. No known software depends on it.

---

## 4. cop0r15, PRID

| Bits | Field |
|---|---|
| 0–7 | Revision |
| 8–15 | Implementation |
| 16–31 | Not used (zero) |

| CPU | PRID |
|---|---|
| `CXD8530BQ` / `CXD8530CQ` | `00000001h` `[DOC]` (nocash only) |
| `CXD8606CQ` | `00000002h` `[DOC]` |

**Return `00000002h`.** Nothing in software reads it meaningfully.

Sanity contrast: a real IDT R3051 returns `00000230h`. That the PSX returns 1 or 2
confirms the core is not an IDT part.

---

## 5. The debug / breakpoint block

Provenance `[DOC]`: psx-spx (COP0 Debug Registers) points out that stock R30xx parts
such as IDT's R3041 and R3051 have no equivalent registers, and that the
documentation for them is LSI's L64360 datasheet (ch. 14) and LR33300/LR33310
datasheet (ch. 4). Neither LSI document is freely reachable, which is why several
details below are `[?]`.

### 5.1 cop0r7, DCIC

The consoledev fork uses LSI's datasheet mnemonics; nocash uses functional names
derived from black-box testing. Both are given.

| Bit | LSI name | Meaning | R/W |
|---|---|---|---|
| 0 | **DB**: Debug | Set by hardware on **any** break | R/W |
| 1 | **PC**: Program Counter | Set on a **BPC (execute)** break | R/W |
| 2 | **DA**: Data Address | Set on a **BDA (data)** break | R/W |
| 3 | **R**: Read Reference | Set on a **BDA data-read** break | R/W |
| 4 | **W**: Write Reference | Set on a **BDA data-write** break | R/W |
| 5 | **T**: Trace | Set on an **any-jump / trace** break | R/W |
| 6–11 |, | Not used, always zero | R |
| 12–13 | Jump Redirection (0 = disable, 1–3 = enable) | §5.2 | R/W |
| 14–15 | **Unknown** `[?]` | Unknown in both sources | R/W |
| 16–22 |, | Not used, always zero | R |
| 23 | **DE**: Debug Enable | Master enable for bits 24–31 (nocash names it a top-level master enable) | R/W |
| 24 | **PCE**: Program Counter Breakpoint Enable | uses BPC + BPCM | R/W |
| 25 | **DAE**: Data Address Breakpoint Enable | uses BDA + BDAM | R/W |
| 26 | **DR**: Data Read Enable | break on read, when bit 25 set | R/W |
| 27 | **DW**: Data Write Enable | break on write, when bit 25 set | R/W |
| 28 | **TE**: Trace Enable | break on any branch/jump/call | R/W |
| 29 | **KD**: Kernel Debug Enable | break in kernel mode (nocash read it as the enable for bit 28 and/or for execute breaks at ≥ `80000000h`) | R/W |
| 30 | **UD**: User Debug Enable | break in user mode (nocash read it as the enable for bits 24–27) | R/W |
| 31 | **TR**: Trap Enable | **0 = only set the status bits; 1 = jump to the debug vector** | R/W |

`[?]` The gating hierarchy of DE/KD/UD/TR is the least certain part of this file.
nocash's functional guesses are consistent with the LSI names once you notice that on
the PSX "kernel mode" ≈ "address ≥ `80000000h`" in practice. **Use the LSI naming**;
it is datasheet-backed and explains nocash's observations.

### Break conditions

```
exec_break: ((PC   ^ BPC) & BPCM) == 0
data_break: ((addr ^ BDA) & BDAM) == 0
```

Recommended gating (the mode gate is the least-confirmed part):

```
mode_ok    = (SR.KUc == 0) ? DCIC.KD : DCIC.UD
exec_break = DCIC.DE && DCIC.PCE && mode_ok && ((PC ^ BPC) & BPCM) == 0
data_break = DCIC.DE && DCIC.DAE && mode_ok
             && ((is_load && DCIC.DR) || (is_store && DCIC.DW))
             && ((addr ^ BDA) & BDAM) == 0
trace_break= DCIC.DE && DCIC.TE  && mode_ok && instruction_is_branch_or_jump
```

On a hit: set the matching DCIC status bit(s) **plus bit 0 (DB)**, then

- if **`TR` (bit 31) = 1** → take the break exception: vector **`80000040h`**,
  `ExcCode = 09h`, `EPC` as usual;
- if **`TR` = 0** → **only the status bits are set; no exception.**

**Trap:** `BPCM = 0` or `BDAM = 0` makes the mask term zero, so `(x ^ reg) & 0 == 0`
is **always true**: a zero mask matches *every* address. The enables are the only
thing preventing a break storm, so the enable gating must be exact, or games that
stash junk in these registers (§1, Soul Reaver) will break.

### The break vector, and how to tell it from the BREAK opcode `[DOC]`

psx-spx (COP0 Debug Registers): a breakpoint match vectors to **`80000040h`**, not to
the normal `80000080h`. `Cause.ExcCode` is set to `09h`, the same code the `BREAK`
opcode produces, and `EPC` holds the return address as for any exception. The
handler must disable breakpoints early; in particular an enabled trace break has to
be turned off before the code at `80000040h` jumps on to the real handler, or the
jump itself will trip it.

The same section notes that the `BREAK` **opcode** reports the same `ExcCode`
(`09h`) but goes to the **normal** handler at `80000080h`, not to `80000040h`.

Same `ExcCode`, different vectors. Getting this backwards is an easy implementation
mistake.

### 5.2 DCIC bits 12–13, "jump redirection" `[DOC]`

nocash PSX-SPX (COP0 Debug Registers) reports, from black-box testing, that with
either bit set the CPU appears to watch for a load into a register `rx`, followed by
one or more instructions that leave `rx` alone, followed by a jump or call through
`rx`. On detecting that pattern it sets `PC` to the word stored at address
`00000000h`, and records nothing useful in COP0; in particular `EPC` does not get the
return address. nocash judges the feature practically unusable.

Nothing sets bits 12–13 in practice, and it is safe to ignore them.

### 5.3 cop0r6, TAR / JUMPDEST, sources conflict

- **nocash (older, black-box):** describes it as an odd register of no apparent use.
  Some exceptions make the CPU record a jump destination in it, after which the
  register is locked. Reset and hardware interrupts (`Cause` bit 10) unlock it;
  `SYSCALL`/`BREAK` and software interrupts do not.
- **consoledev fork (newer, mechanistic):** when an exception hits the delay slot of
  a jump or branch (`Cause` bit 31 set) and the branch is taken or unconditional
  (`Cause` bit 30 set), `TAR` is loaded with that jump or branch's destination.

These are not really contradictory: the fork's rule *produces* the sticky,
random-looking behaviour nocash observed, and explains the lock/unlock pattern
(interrupts land at arbitrary points; syscall/break never occur in a delay slot in
normal kernel code). **Implement the fork's rule.** Read-only in both accounts. No
known game reads it.

### 5.4 cop0r8, BadVaddr `[DOC]`

psx-spx (COP0 Register Summary): `BadVaddr` is written **only** by address errors
(ExcCode `04h` and `05h`); every other exception, bus errors included, leaves it as
it was.

The IDT R30xx manual (Ch. 3) is stronger: after any other exception the register's
contents are undefined, and a bus error specifically does not set it.

---

## 6. Exception codes

| Code | Mnemonic | Meaning | Reachable on PSX? |
|---|---|---|---|
| **00h** | **Int** | External interrupt, or a software interrupt via `Cause` bits 8–9 | **Yes, the common one** |
| 01h | Mod | TLB modification | **No** |
| 02h | TLBL | TLB miss on load / fetch | **No** |
| 03h | TLBS | TLB miss on store | **No** |
| **04h** | **AdEL** | Address error on load **or instruction fetch** | **Yes**: sets `BadVaddr` |
| **05h** | **AdES** | Address error on store | **Yes**: sets `BadVaddr` |
| **06h** | **IBE** | Bus error on instruction fetch | Yes in principle; unused memory regions and I/O gaps. Rare; no known game triggers it |
| **07h** | **DBE** | Bus error on data access | Yes in principle. IDT notes R30xx parts effectively cannot take a bus error on a *store* (the write buffer makes it imprecise), so realistically loads only |
| **08h** | **Sys** | `SYSCALL` | **Yes**: every BIOS A/B/C call routes through it |
| **09h** | **Bp** | `BREAK`, **and** COP0 hardware breakpoints (different vectors, same code, §5.1) | **Yes** |
| **0Ah** | **RI** | Reserved instruction. Also raised by touching cop0 r0/r1/r2/r4/r10/r32–63 and by TLBR/TLBWI/TLBWR/TLBP | **Yes** |
| **0Bh** | **CpU** | Coprocessor unusable; `CE` holds the coprocessor number | **Yes** |
| **0Ch** | **Ovf** | Arithmetic overflow, `ADD`/`ADDI`/`SUB` only | **Yes** |
| 0Dh–1Fh |, | Reserved (Tr, FPE, … on later ISAs) | **No.** And since `Cause` is read-only except bits 8–9, they can never appear at all |

### Exception priority (highest first) `[DOC]`

```
  Reset  At any time (highest)
  AdEL   Memory (load instruction)          ;\
  AdES   Memory (store instruction)         ; memory, data
  DBE    Memory (load or store)             ;/
  MOD    ALU (data TLB)                     ;\
  TLBL   ALU (DTLB miss)                    ; none such on PSX
  TLBS   ALU (DTLB miss)                    ;/
  Ovf    ALU
  Int    ALU
  Sys    RD (instruction decode)            ;\
  Bp     RD (instruction decode)            ;
  RI     RD (instruction decode)            ;
  CpU    RD (instruction decode)            ;/
  TLBL   I-fetch (ITLB miss)                ; none such
  AdEL   IVA (instruction virtual address)  ;\ memory, opcode fetch
  IBE    RD (end of I-fetch, lowest)        ;/
```

Two counter-intuitive consequences: a **data** address error from the *previous*
instruction outranks an **instruction-fetch** address error from the current one; and
`Int` outranks `Sys`/`Bp`/`RI`/`CpU` of the same instruction.

---

## 7. Exception entry

```
on_exception(excode, faulting_pc, in_delay_slot, branch_taken, branch_target):

    # 1. EPC / BD / BT / TAR
    if in_delay_slot:
        EPC      = faulting_pc - 4        # the address of the BRANCH
        Cause.BD = 1                      # bit 31
        Cause.BT = branch_taken           # bit 30
        if branch_taken: TAR = branch_target   # cop0r6
    else:
        EPC      = faulting_pc
        Cause.BD = 0
        Cause.BT = 0

    # 2. Push the SR mode stack, 3 entries deep, 2 bits wide.
    #    KUc/IEc -> KUp/IEp -> KUo/IEo ; the previous "old" pair is DISCARDED.
    SR = (SR & ~0x3F) | ((SR << 2) & 0x3F)
    #    => KUc = 0 (kernel), IEc = 0 (interrupts disabled)

    # 3. Cause
    Cause.ExcCode = excode                # bits 6..2
    if excode == CpU: Cause.CE = (opcode >> 26) & 3    # bits 29..28

    # 4. BadVaddr, ONLY for AdEL (04h) / AdES (05h)
    if excode in (AdEL, AdES): BadVaddr = bad_address

    # 5. Vector
    PC = vector(excode, SR.BEV)
```

The IDT R30xx manual (Ch. 4) gives the same sequence in four steps: `EPC` is set to
the restart address; the current user-mode and interrupt-enable bits are pushed
onto the 3-entry stack in `SR`, leaving the CPU in kernel mode with interrupts
disabled; `Cause` is filled in with the reason, and `BadVaddr` too for address
errors; control passes to the exception vector.

### Vectors

| Exception | BEV = 0 | BEV = 1 |
|---|---|---|
| **Reset** | **`BFC00000h`** | **`BFC00000h`** (BEV-independent; reset also *forces* BEV = 1) |
| UTLB miss | `80000000h` | `BFC00100h` | 
| **COP0 hardware breakpoint** | **`80000040h`** | `BFC00140h` `[?]` |
| **General**: all interrupts, all other exceptions, **including the `BREAK` opcode** | **`80000080h`** | `BFC00180h` |

- The UTLB-miss pair is a stock-R3000A entry; on PSX it is **unreachable**.
- The `80000040h` break vector is **PSX/LSI-specific** and appears in no IDT manual;
  it is directly hardware-confirmed. `[?]` The BEV = 1 form `BFC00140h` is a table
  entry only, no source states it was tested, and the BIOS contains no BEV = 1
  vectors, so nothing exercises it.
- **The PSX uses only the BEV = 0 vectors.** Apart from the reset vector, the BIOS
  ROM contains none of the BEV = 1 vectors (psx-spx, COP0 Exception Vectors). `[DOC]`
- The actual handler at `80000080h` is four opcodes (`LUI+ADDIU+JMP+NOP` on retail
  BIOS, `LUI+ORI+JMP+NOP` on some debug BIOSes) that jumps to the real kernel
  handler; the same four are mirrored at physical `00000080h`.
- Cache note `[DOC]` `[?]`: psx-spx observes that rewriting the vectors through
  their KSEG0 addresses (`800000xxh`) appeared to reach the instruction cache without
  a flush, though only in some of its tests and not reliably, while rewriting the
  KUSEG mirror (`000000xxh`) appeared to need a cache flush.

---

## 8. RFE

```
rfe                                  ; COP0 command 10h ; word 42000010h
SR.bit0 = SR.bit2 ; SR.bit1 = SR.bit3      ; IEp -> IEc, KUp -> KUc
SR.bit2 = SR.bit4 ; SR.bit3 = SR.bit5      ; IEo -> IEp, KUo -> KUp
; bits 4-5 (IEo/KUo) LEFT UNCHANGED
; equivalently: SR = (SR & ~0x0F) | ((SR >> 2) & 0x0F)
```

**Is the "old" pair preserved or duplicated? Duplicated.** `RFE` is a *copy-down*,
not a rotate and not a pop-with-fill. After one `RFE`, bits 4–5 still hold what they
held and bits 2–3 now hold the same values. Two consecutive `RFE`s leave the whole
6-bit field as `{KUo, IEo}` replicated three times.

- **`RFE` does not jump to `EPC`.** The handler must copy `EPC` into a register
  (conventionally `k0`) and jump there, with the `RFE` in the `jr`'s delay slot:

  ```asm
  mfc0 k0, $14      ; EPC
  ; push k0 if you expect nested exceptions
  ; ... process Cause, ack the device ...
  jr   k0
  rfe               ; delay slot
  ```

  The IDT R30xx manual (Ch. 4) recommends exactly this pairing, `jr` with `rfe` in
  its delay slot, as the safest way back to user mode.
- **`RFE` re-enabling `IEc` takes effect immediately** for interrupt purposes -
  unlike an `MTC0` that sets `IEc`, which has one instruction of grace (§2). `[?]`
  single-sourced.
- `RFE` in user mode with `CU0 = 0` → **CpU (0Bh)**.

---

## 9. Interrupts

### 9.1 The controller

```
1F801070h  I_STAT  (R = status, W = acknowledge, write 0 to clear a bit)
1F801074h  I_MASK  (R/W, 0 = disabled, 1 = enabled)

  0     IRQ0   VBLANK
  1     IRQ1   GPU (via GP0(1Fh), rarely used)
  2     IRQ2   CDROM
  3     IRQ3   DMA
  4     IRQ4   TMR0
  5     IRQ5   TMR1
  6     IRQ6   TMR2
  7     IRQ7   Controller / memory card, byte received
  8     IRQ8   SIO
  9     IRQ9   SPU
  10    IRQ10  Controller, lightpen (reportedly also PIO)
  11-15 Not used (always zero)
  16-31 Garbage
```

### 9.2 The chain `[DOC]`

psx-spx (Interrupts):

- The request bits in `I_STAT` are **edge-triggered**: a bit is set only when its
  source goes from false to true.
- Whenever `(I_STAT & I_MASK) != 0`, `cop0r13.bit10` is set; the interrupt is taken
  when `cop0r12.bit10` and `cop0r12.bit0` are also set.
- `cop0r13.bit10` is **not a latch**. It clears by itself once
  `(I_STAT & I_MASK) == 0`, so nothing needs acknowledging on the COP0 side.
- The two software interrupt bits, `cop0r13.bit8..9`, are ordinary read/write
  latches.

### 9.3 The exact take-interrupt condition

```
Cause.IP2 (bit 10) = ((I_STAT & I_MASK) != 0)     # combinational, re-evaluated continuously
take_irq = ((Cause[15:8] & SR[15:8]) != 0) && (SR.IEc == 1)
```

Things that trip people up:

- **`KUc` is not part of the condition.** Kernel/user mode is irrelevant to whether an
  interrupt is taken.
- Both `SR.Im2` **and** `SR.IEc` must be set for hardware IRQs; `SR.Im0`/`Im1` +
  `IEc` for the software ones.
- **Ack ordering is mandatory**, because `I_STAT` is edge-triggered. psx-spx
  prescribes clearing the `I_STAT` bit first (for example `I_STAT.bit7 = 0`) and only
  then acknowledging the device (for example `JOY_CTRL.bit4 = 1`). The other order can permanently lose an IRQ source, after
  the device-side ack, a fresh device IRQ within one clock produces no new edge into
  `I_STAT`. Most IRQs (all except 0, 4, 5, 6) need a device-side ack too.
- **Software interrupts** (`Cause` bits 8–9) produce `ExcCode = 00h (Int)`,
  indistinguishable from a hardware IRQ except by inspecting `Cause.IP`. They are
  latches and the handler must clear them manually before `RFE`. **Jackie Chan
  Stuntmaster** and the **MTV Sports** titles use them.
- IDT timing caveat for a cycle-accurate core: the R30xx resynchronises some of its
  interrupt inputs internally, so an interrupt is only recognised on the rising edge
  of the second clock after the input goes active (IDT R30xx manual, Ch. 5).

---

## 10. Edge cases

### 10.1 Exception in a branch delay slot

`EPC = branch_address` (= `faulting_pc − 4`), `Cause.BD = 1`, `Cause.BT =
branch_taken`, `TAR = branch_target` if taken.

**Do not** point `EPC` at the delay slot: returning straight to the delay-slot
instruction would skip the branch, and the exception would then have corrupted the
interrupted program (IDT R30xx manual, Ch. 4).

A handler wanting to inspect the faulting instruction reads `[EPC+4]` when `BD = 1`.

### 10.2 Interrupt in a delay slot re-executes the branch

This is intended and correct `[DOC]`. psx-spx (COP0 Exception Handling): an
interrupt handler should always return to `EPC+0` whatever `BD` says. With `BD = 1`
that re-executes the branch, which is required because `EPC` holds a single address
and the branch destination is not saved anywhere.

So the branch (including `JAL`/`BLTZAL`/`BGEZAL`, which harmlessly rewrite `$ra` with
the same value) and its delay slot both re-execute. The delay-slot instruction was
nullified by the precise-exception machinery and had no visible effect, so this is
safe.

For **non-interrupt** exceptions the handler must handle `BD` itself; with `BD = 0`
it may return to `EPC+0` (retry) or `EPC+4` (skip).

### 10.3 Interrupt on a GTE command, the Crash Bandicoot case

Covered in [`01-cpu-overview.md` §4.3](01-cpu-overview.md), and it is a **CPU-side**
requirement. Summary: the GTE command executes, `EPC` points at it, and the BIOS
handler skips it with `EPC += 4` after testing
`(mem32[EPC] & FE000000h) == 4A000000h`. **Crash Bandicoot 1/2/3, Jinx and Spyro the
Dragon render broken geometry** if this is wrong. The fixup cannot work when
`Cause.BD` is set.

### 10.4 An exception while BD is already set

There is **no shadow copy** of `EPC`/`Cause`/`SR`. A second exception unconditionally
recomputes `Cause.BD`/`BT`, overwrites `EPC`, and pushes the SR stack again.

- A nested exception inside the handler is almost always *not* in a delay slot, so it
  sets `BD = 0` and destroys the outer `BD = 1` information, the outer context is
  unrecoverable unless the handler saved `EPC`/`SR`/`Cause` first.
- The 3-deep SR stack buys exactly **one** free nesting level; after two pushes
  without a save, the original `{KUo, IEo}` is gone. The IDT R30xx manual (Ch. 3)
  presents the third level as a way to survive an exception that arrives before the
  handler for the first one has saved `SR`, and cautions that this only works in
  limited circumstances.
- Entry forces `IEc = 0`, so you cannot get an interrupt inside an interrupt without
  the handler explicitly re-enabling, but `AdEL`/`AdES`/`RI`/`Ovf`/`Sys`/`Bp` in the
  handler are not maskable and will clobber `EPC`.
- There is no "BD is sticky" behaviour and no re-entrancy interlock in hardware.

### 10.5 Pending load across an exception

Covered in [`04-delay-slots-and-hazards.md` §2.9](04-delay-slots-and-hazards.md): the
load issued by the *previous* instruction **commits**; the load issued by the
faulting instruction is **dropped**; the handler starts with an empty load-delay
slot.

### 10.6 Jump to a bad address

`EPC` and `BadVaddr` point at **the faulty target**, not at the jump, and
`Cause.BD = 0`. See [`04-delay-slots-and-hazards.md` §1.7](04-delay-slots-and-hazards.md).

### 10.7 Miscellaneous implementer traps

- **`MTC0` to `Cause` must mask to bits 8–9.**
- **`EPC`, `BadVaddr`, `PRID`, `TAR` are read-only.** (The BIOS init code writes 0 to
  cop0r6 anyway; the write is ignored.)
- **`SR.CU2` then an immediate GTE op**: ~2 cycles before COP2 is really enabled. No known
  game depends on it, so ignoring it is safe.
- **BIOS bug worth knowing:** psx-spx notes that early BIOS versions test a copy of
  cop0r13 in `r2` without ever having moved cop0r13 into `r2`, so they test garbage.
  This is the same defect behind the missing GTE fixup in old BIOSes.
