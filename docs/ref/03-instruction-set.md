# Instruction set, per-instruction semantics

Encodings are in [`02-instruction-encoding.md`](02-instruction-encoding.md). This
file is the semantics: exactly what each instruction computes, and what it can
raise.

## 0. Notation and universal rules

```
$         address of the instruction currently executing
npc       $ + 4, the delay slot's address
imm16     insn[15:0]      imm26 insn[25:0]     sa insn[10:6]
rs        insn[25:21]     rt    insn[20:16]    rd insn[15:11]
sext(x)   sign-extend to 32 bits       zext(x)  zero-extend to 32 bits
R[n]      GPR n; R[0] reads 0, every write to it is discarded
+ - << >> wrapping 32-bit unless stated
```

Rules that apply everywhere and are not repeated per instruction:

- **All arithmetic is 32-bit wrapping** except `ADD`, `ADDI`, `SUB`, which trap.
- **Every branch and jump has one delay slot**, executed taken or not.
- **PC-relative branch target = `$ + 4 + (sext(imm16) << 2)`**: relative to the
  *delay slot*, not to the branch.
- **Effective address for every load/store = `R[rs] + sext(imm16)`**, wrapping. The
  offset is *always* sign-extended, including for the logical-immediate-looking ones.
- **Loads have a one-instruction delay**: see
  [`04-delay-slots-and-hazards.md`](04-delay-slots-and-hazards.md). So do `MFC0`,
  `MFC2`, `CFC2`.
- Any instruction can be preempted by **Int (00h)**; any fetch can raise **AdEL
  (04h)** or **IBE (06h)**. Omitted from the per-instruction lists.
- **If an exception occurs during a load, `rt` is left untouched.** `[DOC]`

### Exception codes referenced below

| Code | Mnemonic | Cause |
|---|---|---|
| 00h | Int | interrupt |
| 04h | AdEL | address error on load or instruction fetch (misaligned, or ≥ `80000000h` in user mode) |
| 05h | AdES | address error on store |
| 06h | IBE | bus error, instruction fetch |
| 07h | DBE | bus error, data access |
| 08h | Sys | `SYSCALL` |
| 09h | Bp | `BREAK`, and COP0 hardware breakpoints |
| 0Ah | RI | reserved instruction |
| 0Bh | CpU | coprocessor unusable |
| 0Ch | Ovf | arithmetic overflow |

---

## 1. ALU, register–register

### ADD / SUB, the trapping pair

```
add rd, rs, rt                       ; SPECIAL 20h
t = (i64)(i32)R[rs] + (i64)(i32)R[rt]
if t outside i32 range:  raise Ovf (0Ch);  R[rd] UNCHANGED
else                     R[rd] = t & FFFFFFFFh

sub rd, rs, rt                       ; SPECIAL 22h
t = (i64)(i32)R[rs] - (i64)(i32)R[rt]
if t outside i32 range:  raise Ovf (0Ch);  R[rd] UNCHANGED
else                     R[rd] = t & FFFFFFFFh
```

Overflow test without widening: `ADD` overflows iff `(~(rs ^ rt) & (res ^ rs)) < 0`;
`SUB` overflows iff `((rs ^ rt) & (res ^ rs)) < 0`.

**On overflow the destination is not written.** `[DOC]` IDT: *"The destination
register rt is not modified when an integer overflow exception occurs."*
Note `sub rd, r0, rt` overflows when `rt = 80000000h`.

Exceptions: **Ovf (0Ch)**.

### ADDU / SUBU / AND / OR / XOR / NOR

```
addu rd,rs,rt  ; 21h   R[rd] = R[rs] + R[rt]              ; wrapping, never traps
subu rd,rs,rt  ; 23h   R[rd] = R[rs] - R[rt]
and  rd,rs,rt  ; 24h   R[rd] = R[rs] & R[rt]
or   rd,rs,rt  ; 25h   R[rd] = R[rs] | R[rt]
xor  rd,rs,rt  ; 26h   R[rd] = R[rs] ^ R[rt]
nor  rd,rs,rt  ; 27h   R[rd] = FFFFFFFFh ^ (R[rs] | R[rt])
```

Exceptions: none.

### SLT / SLTU

```
slt  rd,rs,rt  ; 2Ah   R[rd] = ((i32)R[rs] < (i32)R[rt]) ? 1 : 0
sltu rd,rs,rt  ; 2Bh   R[rd] = ((u32)R[rs] < (u32)R[rt]) ? 1 : 0
```

`sltu rd,r0,rt` is the idiomatic "set if `rt` nonzero". Exceptions: none.

---

## 2. ALU, immediate

**The extension rule, which is the single most common source of bugs:**

| Instructions | Immediate |
|---|---|
| `ADDI`, `ADDIU`, `SLTI`, `SLTIU`, **and every load/store offset** | **sign-extended** (`-8000h..+7FFFh`) |
| `ANDI`, `ORI`, `XORI` | **zero-extended** (`0000h..FFFFh`) |
| `LUI` | placed in the upper half; low 16 bits become zero |

### ADDI / ADDIU

```
addi  rt, rs, imm16                  ; 08h
t = (i64)(i32)R[rs] + (i64)(i16)imm16
if t outside i32 range: raise Ovf (0Ch); R[rt] UNCHANGED
else R[rt] = t & FFFFFFFFh

addiu rt, rs, imm16                  ; 09h
R[rt] = R[rs] + sext(imm16)          ; wrapping
```

**`ADDIU` is not "unsigned immediate".** The immediate is *still sign-extended* -
`addiu rt,rs,0FFFFh` subtracts 1. The only difference from `ADDI` is that `ADDIU`
never raises `Ovf`. `[DOC]` IDT: *"The only difference between this instruction and
the ADDI instruction is that ADDIU never causes an overflow exception."*

Exceptions: `ADDI` → **Ovf (0Ch)**; `ADDIU` → none.

### SLTI / SLTIU

```
slti  rt, rs, imm16   ; 0Ah   R[rt] = ((i32)R[rs] < (i32)sext(imm16)) ? 1 : 0
sltiu rt, rs, imm16   ; 0Bh   R[rt] = ((u32)R[rs] < (u32)sext(imm16)) ? 1 : 0
```

**`SLTIU` sign-extends the immediate and then compares *unsigned*.** Confirmed by all
three primary sources. psx-spx spells the effective range out as
`[0..7FFFh] ∪ [FFFF8000h..FFFFFFFFh]`; IDT: *"The 16-bit immediate is sign-extended
… Considering both quantities as unsigned integers, if rs is less than the
sign-extended immediate, the result is set to one."*

So `sltiu rt, rs, -1` tests `rs < FFFFFFFFh`, true for every `rs` except
`FFFFFFFFh`. Zero-extending the immediate is the classic bug.

Exceptions: none.

### ANDI / ORI / XORI

```
andi rt, rs, imm16   ; 0Ch   R[rt] = R[rs] & zext(imm16)   ; upper 16 bits become 0
ori  rt, rs, imm16   ; 0Dh   R[rt] = R[rs] | zext(imm16)
xori rt, rs, imm16   ; 0Eh   R[rt] = R[rs] ^ zext(imm16)
```

Exceptions: none.

### LUI

```
lui rt, imm16                        ; 0Fh
R[rt] = imm16 << 16                  ; low 16 bits ZERO; the rs field is ignored
```

There is no OR with the previous value and no read of `rs`. Bits 25..21 are unused
and do not trap if nonzero. Exceptions: none.

---

## 3. Shifts

**Shifts use `rt` as the value and `rs` (or `sa`) as the amount**: the reverse of
the ALU-register operand order. psx-spx warns explicitly: *"Unlike many other
opcodes, shifts use 'rt' as second (not third) operand."* A decoder that treats
`SLLV` like `ADDU` is silently wrong.

```
sll  rd, rt, sa   ; 00h   R[rd] = R[rt] << sa               ; sa = insn[10:6], 0..31
srl  rd, rt, sa   ; 02h   R[rd] = (u32)R[rt] >> sa          ; logical, zero-fill
sra  rd, rt, sa   ; 03h   R[rd] = (i32)R[rt] >> sa          ; arithmetic, sign-fill
sllv rd, rt, rs   ; 04h   R[rd] = R[rt] << (R[rs] & 1Fh)
srlv rd, rt, rs   ; 06h   R[rd] = (u32)R[rt] >> (R[rs] & 1Fh)
srav rd, rt, rs   ; 07h   R[rd] = (i32)R[rt] >> (R[rs] & 1Fh)
```

- The register shift amount is **masked to 5 bits**. A shift of 32 is not
  representable; `R[rs] = 32` shifts by 0 (identity). Bits 5..31 of `rs` are ignored.
- The immediate `sa` is a 5-bit field, so `>= 32` is not encodable at all.
- **Shift-by-zero is a plain move**, not a no-op, when `rd != rt`.
- **The hardware does NOT generate exceptions on shift overflow.** `[DOC]`
- `sll r0,r0,0` = `NOP`. MIPS32's `SSNOP`/`EHB` meanings for `sa = 1`/`sa = 3` do
  **not** exist here.
- No rotate instructions on MIPS-I.

Exceptions: none.

---

## 4. Multiply and divide

HI/LO are not GPRs. Issue takes one cycle; the unit runs in parallel with the
integer pipe; reading HI/LO while busy **stalls the CPU** (interlocked, so the value
read is always correct).

```
mult  rs, rt   ; 18h   t = (i64)(i32)R[rs] * (i64)(i32)R[rt] ; LO = t[31:0], HI = t[63:32]
multu rs, rt   ; 19h   t = (u64)(u32)R[rs] * (u64)(u32)R[rt] ; LO = t[31:0], HI = t[63:32]
```

Timing depends on the magnitude of **`rs` only**, so `mult` operand order is
observable, "small × large" is faster than "large × small":

| | `\|rs\|` ≤ `7FFh` | `\|rs\|` ≤ `FFFFFh` | otherwise |
|---|---|---|---|
| Cycles after issue | 6 | 9 | 13 |

(For `MULTU` the classification is on `rs` directly; for `MULT` on its
sign-magnitude, i.e. `rs ^ (rs >> 31)`.) The generic IDT manual's 12/35 figures are
for a stock R30xx and **do not match the PSX**: use the numbers above. `[?]`

```
div rs, rt                           ; 1Ah  (signed): 36 cycles, fixed
n = (i32)R[rs] ; d = (i32)R[rt]
if d == 0:
    HI = n
    LO = (n >= 0) ? FFFFFFFFh : 00000001h
else if n == 80000000h and d == -1:
    HI = 0 ; LO = 80000000h
else:
    LO = n / d      ; truncation toward zero
    HI = n % d      ; remainder takes the sign of the dividend

divu rs, rt                          ; 1Bh  (unsigned): 36 cycles, fixed
n = (u32)R[rs] ; d = (u32)R[rt]
if d == 0: HI = n ; LO = FFFFFFFFh
else:      LO = n / d ; HI = n % d
```

**Neither divide nor multiply ever raises an exception**, including on divide by
zero and on `80000000h / -1`. The two special cases above are concrete silicon
behaviour, not "undefined", real code that divides by an unchecked zero depends on
exactly this garbage. In Rust both cases must be special-cased before `i32::div`,
which panics on them.

```
mfhi rd   ; 10h   R[rd] = HI    ; stalls while mul/div busy; NOT load-delayed
mflo rd   ; 12h   R[rd] = LO    ; stalls while mul/div busy; NOT load-delayed
mthi rs   ; 11h   HI = R[rs]    ; source is rs, not rd; not interlocked
mtlo rs   ; 13h   LO = R[rs]
```

`MTHI`/`MTLO` exist essentially only so an exception handler can restore HI/LO -
multiply results are written as soon as they are available and cannot be inhibited
by an exception. See [`04-delay-slots-and-hazards.md` §4.4](04-delay-slots-and-hazards.md)
for the three HI/LO hazards, none of which needs modelling in practice.

Exceptions: none, for all of the above.

---

## 5. Jumps and branches

### J / JAL

```
j   target26                         ; 02h
npc = (npc_before & F0000000h) | (imm26 << 2)

jal target26                         ; 03h
R[31] = $ + 8                        ; the address AFTER the delay slot
npc   = (npc_before & F0000000h) | (imm26 << 2)
```

**The top nibble comes from the delay slot's PC, not the jump's PC.** IDT is
explicit: *"combined with the high-order bits of the address of the delay slot"*,
with the update at `T+1`. This only differs when the jump itself sits at
`?FFFFFFCh`: rare, but real. In the canonical interpreter shape (`pc` already
advanced to the delay slot before `execute`) this falls out naturally as
`pc & F0000000h`.

`JAL` writes the link at issue time, so the **delay-slot instruction sees the new
`$ra`**.

Exceptions: none from the jump itself; a bad target faults on the subsequent fetch.

### JR / JALR

```
jr rs                                ; SPECIAL 08h
npc = R[rs]

jalr rd, rs                          ; SPECIAL 09h
tmp   = R[rs]                        ; READ rs FIRST
R[rd] = $ + 8                        ; rd defaults to 31 in asm, but is a real field
npc   = tmp
```

- **`rd` is a real 5-bit field at insn[15:11].** Decode it; do not hardcode 31.
- **`rs` is sampled before `rd` is written.** IDT pseudocode uses an explicit temp
  (`T: temp ← GPR[rs]; GPR[rd] ← PC+8`), and `jalr r31,r31` jumping correctly on its
  first execution proves it.
- Writing the link to `r0` (`jalr r0, rs`) is discarded, `r0` is hardwired.
- **A misaligned target raises no exception at the jump.** `[DOC]` The `AdEL (04h)`
  fires on the *next instruction fetch*, with `EPC = BadVaddr = the bad target` and
  `Cause.BD = 0`. Since `EPC` is itself misaligned, returning re-faults, effectively
  fatal. Do not attribute the fault to the jump.
  `[?]` Whether the delay-slot instruction executes before the fault is unsettled:
  IDT says it does not; a strict precise-exception reading says it does. No hardware
  test found; no game known to depend on it.

Exceptions: AdEL (deferred to the target fetch).

### BEQ / BNE / BLEZ / BGTZ

```
beq  rs, rt, off16   ; 04h   if R[rs] == R[rt]:      npc = $+4+(sext(imm16)<<2)
bne  rs, rt, off16   ; 05h   if R[rs] != R[rt]:      npc = $+4+(sext(imm16)<<2)
blez rs,     off16   ; 06h   if (i32)R[rs] <= 0:     npc = $+4+(sext(imm16)<<2)
bgtz rs,     off16   ; 07h   if (i32)R[rs] >  0:     npc = $+4+(sext(imm16)<<2)
```

`BEQ`/`BNE` are bitwise comparisons, no sign involved. `BLEZ`/`BGTZ` are **signed**
against zero and never link; their `rt` field is unused and does not trap if
nonzero. Exceptions: none.

### BLTZ / BGEZ / BLTZAL / BGEZAL (REGIMM, primary `01h`)

All four share primary opcode `01h` and are distinguished by the `rt` field. The
decode is partial, see [`02-instruction-encoding.md` §4](02-instruction-encoding.md)
for the full 32-entry table and the undocumented aliases.

```
is_bgez = rt & 1                     ; bit 16 alone selects the condition
is_link = (rt & 1Eh) == 10h          ; exactly rt = 10h and 11h
cond    = ((i32)R[rs] < 0) ^ is_bgez ; EVALUATE THE CONDITION FIRST
if is_link: R[31] = $ + 8            ; ALWAYS, taken or not
if cond:    npc = $+4+(sext(imm16)<<2)
```

Two facts worth stating twice:

- **`BLTZAL`/`BGEZAL` write `$ra` even when the branch is not taken.** `[DOC]`
- **If `rs` is `r31`, the comparison uses `$ra`'s value *before* linking.** `[DOC]`

Exceptions: none.

### Branches in delay slots

Architecturally undefined (IDT: *"A delay slot may not itself be occupied by a jump
or branch instruction; however, this error is not detected and the results of such
an operation are undefined"*): but deterministic on silicon, and **real games do
it**. See [`04-delay-slots-and-hazards.md` §1.3](04-delay-slots-and-hazards.md).

---

## 6. Loads and stores

`addr = R[rs] + sext(imm16)` throughout.

```
lb  rt, imm(rs)  ; 20h   R[rt] = sext8 (read8 (addr))   ; no alignment requirement
lbu rt, imm(rs)  ; 24h   R[rt] = zext8 (read8 (addr))
lh  rt, imm(rs)  ; 21h   if addr & 1: AdEL(04h), BadVaddr=addr, rt untouched
                 ;       R[rt] = sext16(read16(addr))
lhu rt, imm(rs)  ; 25h   if addr & 1: AdEL(04h)
                 ;       R[rt] = zext16(read16(addr))
lw  rt, imm(rs)  ; 23h   if addr & 3: AdEL(04h), BadVaddr=addr
                 ;       R[rt] = read32(addr)

sb  rt, imm(rs)  ; 28h   write8 (addr, R[rt] & FFh)     ; no alignment requirement
sh  rt, imm(rs)  ; 29h   if addr & 1: AdES(05h) ; write16(addr, R[rt] & FFFFh)
sw  rt, imm(rs)  ; 2Bh   if addr & 3: AdES(05h) ; write32(addr, R[rt])
```

All five loads carry the one-instruction load delay. Exceptions: `AdEL`/`AdES` as
shown, plus `AdEL`/`AdES` for any access at or above `80000000h` while
`SR.KUc = 1`, plus `DBE (07h)`.

**Narrow stores put the whole register on the bus.** `[DOC]` `[HW]` *"During an
8-bit or 16-bit store, all 32 bits of the GPR are placed on the bus."* Some 32-bit
I/O registers therefore behave as if a full 32-bit store had happened using the
register's entire value, the CD-audio soundscope in the SCPH-7xxx shells relies on
this (it uses `sh` on DMA registers and hangs otherwise).

**Stores while `SR.IsC` (cop0r12 bit 16) is set must not reach RAM or I/O.** The
BIOS `FlushCache` stores zeroes across `0000h..0FFFh` with the cache isolated; an
emulator that lets those through wipes the bottom 4 KB of kernel RAM and never
boots. This is the single most common first-boot bug in a new PSX emulator.

### LWL / LWR / SWL / SWR, unaligned access

These **force-align the address internally** and therefore **can never raise an
alignment exception**. `[DOC]` The transferred data is not sign- or zero-expanded;
untouched bytes of both `rt` and memory are preserved.

```
n    = addr & 3
base = addr & ~3
W    = the 32-bit little-endian word at base
R    = the current value of rt  (for LWL/LWR: the BYPASSED value, see §04 2.6)
```

**LWL**: `mask = 00FFFFFFh >> (8*n)`, `rt = (R & mask) | (W << (24 - 8*n))`

| `n` | bytes read | new `rt` |
|---|---|---|
| 0 | `[base+0]` | `(R & 00FFFFFFh) \| (W << 24)` |
| 1 | `[base+0..1]` | `(R & 0000FFFFh) \| (W << 16)` |
| 2 | `[base+0..2]` | `(R & 000000FFh) \| (W << 8)` |
| 3 | `[base+0..3]` | `W` |

**LWR**: `mask = FFFFFF00h << (24 - 8*n)`, `rt = (R & mask) | (W >> (8*n))`

| `n` | bytes read | new `rt` |
|---|---|---|
| 0 | `[base+0..3]` | `W` |
| 1 | `[base+1..3]` | `(R & FF000000h) \| (W >> 8)` |
| 2 | `[base+2..3]` | `(R & FFFF0000h) \| (W >> 16)` |
| 3 | `[base+3]` | `(R & FFFFFF00h) \| (W >> 24)` |

**SWL**: resulting memory word at `base`:

| `n` | bytes written | resulting word | bus transaction |
|---|---|---|---|
| 0 | `[base+0]` | `(W & FFFFFF00h) \| (rt >> 24)` | 8-bit write at `base` |
| 1 | `[base+0..1]` | `(W & FFFF0000h) \| (rt >> 16)` | 16-bit write at `base` |
| 2 | `[base+0..2]` | `(W & FF000000h) \| (rt >> 8)` | **24-bit** write at `base` |
| 3 | `[base+0..3]` | `rt` | 32-bit write at `base` |

**SWR**: resulting memory word at `base`:

| `n` | bytes written | resulting word | bus transaction |
|---|---|---|---|
| 0 | `[base+0..3]` | `rt` | 32-bit write at `base` |
| 1 | `[base+1..3]` | `(W & 000000FFh) \| (rt << 8)` | **24-bit** write at `base+1` |
| 2 | `[base+2..3]` | `(W & 0000FFFFh) \| (rt << 16)` | 16-bit write at `base+2` |
| 3 | `[base+3]` | `(W & 00FFFFFFh) \| (rt << 24)` | 8-bit write at `base+3` |

The 24-bit rows are real: *"The CPU has four separate byte-access signals, so, within
a 32bit location, it can transfer all fragments of Rt at once (including for odd
24bit amounts)."* `[DOC]` The read-modify-write formulation above is a faithful
*emulation* for RAM, but it is **not what the hardware does**: real silicon asserts
byte enables. If SWL/SWR ever lands on a write-only or side-effecting I/O register,
the RMW model is wrong. psx-spx: *"Results on unaligned I/O port writes (via SWL/SWR
opcodes) are unknown."* `[?]`

Canonical PSX (little-endian) unaligned word load, note the pair order is the
reverse of the big-endian examples in the IDT manual:

```asm
lwl  r2, 3(t0)     ; no nop needed between these two
lwr  r2, 0(t0)     ; the second sees the first's partial result
nop                ; the load delay is HERE
and  r2, r2, 0FFFFh
```

Degenerate case: if `addr` is already word-aligned, `LWL @ addr+3` and
`LWR @ addr+0` each load the whole word, so the pair duplicates effort but is
harmless. (psx-spx speculates otherwise, *"Uhhhhhhhm, OR is that NOT allowed…"* -
the tables above settle it.)

Exceptions: `AdEL`/`AdES` only for user-mode/KUSEG violations; `DBE`. **Never for
misalignment.**

---

## 7. Coprocessor instructions

Coprocessor-unusable rule: if `SR.CU<n>` (cop0r12 bits 28..31) is clear, any COP*n*
instruction raises **CpU (0Bh)** with `Cause.CE = n`. COP0 is special, usable in
kernel mode regardless of `CU0`; `CU0 = 1` additionally permits user mode.

### MFC0 / MTC0

```
mfc0 rt, rd     ; 010000 00000 rt rd 00000 000000
R[rt] = cop0[rd]                     ; ONE-INSTRUCTION LOAD DELAY

mtc0 rt, rd     ; 010000 00100 rt rd 00000 000000
cop0[rd] = R[rt]                     ; no store delay (one exception, below)
```

Register-by-register behaviour is in
[`05-cop0-and-exceptions.md`](05-cop0-and-exceptions.md). The parts that belong here:

- **`MFC0` of cop0 r0, r1, r2, r4, r10 → Reserved Instruction (0Ah)**, not CpU.
- **`MFC0` of cop0 r16..r31 → garbage, no exception**, and readable even in user
  mode with `CU0 = 0`.
- **`Cause` (r13) is read-only except bits 8–9.** `MTC0` must mask to `00000300h`; a
  full-width write produces phantom exceptions.
- **`EPC` (r14), `BadVaddr` (r8), `PRID` (r15), `TAR` (r6) are read-only.**
- **`MTC0` has no store delay**, except that setting `SR.CU2` (bit 30) takes ~2
  clock cycles to actually enable COP2. `[DOC]`
- psx-spx explicitly debunks the "coprocessor reads take *two* opcodes" rumour:
  *"the PSX does finish both COP0 and COP2 reads after ONE opcode."*

Exceptions: RI (0Ah) for the nonexistent indices; CpU (0Bh) in user mode with
`CU0 = 0` (except for the r16–31 garbage registers).

### CFC0 / CTC0

```
cfc0 rt, rd  /  ctc0 rt, rd          ; would address cop0r32..63
```

**COP0 has no control-register bank.** psx-spx: *"Registers 32..63 (aka 'control
registers') aren't used in any MIPS processors. Trying to read any of these
registers causes a Reserved Instruction Exception (excode=0Ah)."*

Implement both as **RI (0Ah)**. `[?]` The write direction (`CTC0`) is not separately
documented; RI is the consistent reading.

### RFE

```
rfe                                  ; COP0 command 10h ; word 42000010h
SR.bit0 = SR.bit2 ; SR.bit1 = SR.bit3      ; IEp -> IEc, KUp -> KUc
SR.bit2 = SR.bit4 ; SR.bit3 = SR.bit5      ; IEo -> IEp, KUo -> KUp
; bits 4-5 are LEFT UNCHANGED, a copy-down, not a rotate or a pop-with-fill
; equivalently: SR = (SR & ~0Fh) | ((SR >> 2) & 0Fh)
```

**`RFE` does not jump.** The handler must copy `EPC` into a register (conventionally
`k0`) and `jr` to it, with the `RFE` in the `jr`'s delay slot:

```asm
mfc0 k0, $14      ; EPC
; ... process Cause, ack the device ...
jr   k0
rfe               ; in the delay slot
```

Decode: any `COP0 imm25` with `funct == 10h`. Exceptions: CpU (0Bh) in user mode
with `CU0 = 0`.

### TLBR / TLBWI / TLBWR / TLBP

COP0 commands `01h`, `02h`, `06h`, `08h`. **No TLB on the PSX → Reserved
Instruction (0Ah).** `RFE` (`10h`) is the only COP0 command that exists.

### COP1 / COP3, MFC1, CFC1, MTC1, CTC1, COP1, BC1F/T, LWC1, SWC1 (and the `3` forms)

No such coprocessor. With `SR.CU1`/`SR.CU3` clear (the normal state), any of these
raises **CpU (0Bh)** with `Cause.CE` = 1 or 3.

The CU bits *are* writable, so software can in principle set them. `[?]` What the
instructions then do is undocumented; unconditional CpU is correct for all real
software.

### COP2, the GTE

Semantics of the GTE registers and the 63 commands are out of scope for this
document set; what the **CPU** does is:

```
mfc2 rt, rd   ; 010010 00000 ...   R[rt] = cop2.data[rd]   ; 1-instruction load delay
cfc2 rt, rd   ; 010010 00010 ...   R[rt] = cop2.ctrl[rd]   ; 1-instruction load delay
mtc2 rt, rd   ; 010010 00100 ...   cop2.data[rd] = R[rt]   ; reaches the GTE in 2-3 cycles
ctc2 rt, rd   ; 010010 00110 ...   cop2.ctrl[rd] = R[rt]
cop2 imm25    ; 010010 1 <imm25>   execute GTE command imm25[5:0]
lwc2 rt, imm(rs) ; 32h   cop2.data[rt] = read32(addr)   ; AdEL if addr & 3
swc2 rt, imm(rs) ; 3Ah   write32(addr, cop2.data[rt])   ; AdES if addr & 3
bc2f / bc2t      ; the GTE has no condition flag: bc2f jumps always, bc2t never
```

- **`MFC2`/`CFC2` carry the same one-instruction load delay as a memory load.**
  **Tekken 2 requires this**: without it, "severe graphical glitching" / broken
  geometry.
- **`MFC2`/`CFC2` and a new GTE command stall** while a command is in flight;
  `MTC2`/`CTC2` do not stall.
- All forms raise **CpU (0Bh, `CE = 2`)** when `SR.CU2` is clear.
- The interrupt-on-GTE-command quirk is in
  [`01-cpu-overview.md` §4.3](01-cpu-overview.md): it is a **CPU-side** requirement
  and matters even before the GTE itself is implemented.
- psx-spx warns that GTE instructions "should not be used in delay slots of jumps and
  branches, or in event handlers or interrupts", because the BIOS's `EPC += 4`
  fixup cannot work when `Cause.BD` is set.

---

## 8. SYSCALL and BREAK

Both are SPECIAL with a **20-bit code field at bits 25..6** (the whole rs/rt/rd/sa
span).

```
syscall imm20                        ; SPECIAL 0Ch -> Sys (08h)
break   imm20                        ; SPECIAL 0Dh -> Bp  (09h)
```

- **The 20-bit code has zero effect on the CPU.** It is not latched into `Cause` or
  anywhere else. `[DOC]` The handler must load the instruction word itself and mask
  `(word >> 6) & FFFFFh`.
- **`EPC` = the address of the `SYSCALL`/`BREAK` instruction.** If it sits in a
  branch delay slot, `EPC` = the branch's address and `Cause.BD = 1`. So the handler
  reads the opcode at `[EPC]` when `BD = 0` and at `[EPC+4]` when `BD = 1`, and
  returns to `EPC+4` (BD = 0) to skip it.
  `[?]` psx-spx elsewhere says "by examining the opcode bits at `[epc-4]`", which
  contradicts its own definition of `EPC` and IDT's. That phrasing almost certainly
  describes a handler that has already advanced `EPC`. **Implement `EPC` = the
  faulting instruction's own address.**
- **Both take effect immediately, the following instruction is *not* executed.**
  Exceptions have no delay slot.
- **`BREAK` vectors to the normal handler at `80000080h`**, *not* to `80000040h`.
  Only the COP0 hardware breakpoints (BPC/BDA/DCIC matches) use `80000040h`, even
  though both report `ExcCode = 09h`. This is a frequent emulator mistake.

The PSX BIOS builds its A/B/C-function dispatch on top of `SYSCALL`.

---

## 9. Illegal and unimplemented opcodes

```
Primary N/A: 14h-1Fh, 27h, 2Ch, 2Dh, 2Fh, 34h-37h, 3Ch-3Fh
SPECIAL N/A: 01h, 05h, 0Ah, 0Bh, 0Eh, 0Fh, 14h-17h, 1Ch-1Fh,
             28h, 29h, 2Ch-2Fh, 30h-3Fh
  -> Reserved Instruction Exception, ExcCode = 0Ah
```

**Unused operand bits are not checked** and must not be validated, see
[`02-instruction-encoding.md` §2](02-instruction-encoding.md). The single exception
is the REGIMM `rt` field.
