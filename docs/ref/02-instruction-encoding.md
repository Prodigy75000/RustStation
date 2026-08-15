# Instruction encoding, complete decode tables

Every table below is complete: all 64 primary slots, all 64 SPECIAL slots, all 32
REGIMM slots, all 32 COPn sub-op slots. Reserved slots are listed explicitly, so a
decoder written against this file has no silent holes.

## 1. Instruction word layout

```
  31..26 |25..21|20..16|15..11|10..6 |  5..0  |
   6bit  | 5bit | 5bit | 5bit | 5bit |  6bit  |
```

| Bits | Field | Used by |
|---|---|---|
| 31..26 | `op`: primary opcode | everything |
| 25..21 | `rs`: source register / COPn sub-opcode | R-type, I-type, COPn |
| 20..16 | `rt`: source-or-destination register / REGIMM sub-op / BCnF-BCnT selector | R-type, I-type, REGIMM, COPn |
| 15..11 | `rd`: destination register / coprocessor register number | R-type, MFCn/CFCn/MTCn/CTCn |
| 10..6 | `shamt` / `sa`: 5-bit shift amount | shift-immediate only |
| 5..0 | `funct`: secondary opcode / COPn cofun | SPECIAL, COPn command |
| 15..0 | `imm16` | I-type, branches, loads/stores |
| 25..0 | `target` (imm26) | J, JAL |
| 25..0 | `imm25` | COPn command (bit 25 = 1) |
| 25..6 | `comment20` | SYSCALL, BREAK |

Per-format summary `[DOC]` (`N/A` = should be zero, **ignored** by hardware, does
not trap if nonzero):

```
  31..26 |25..21|20..16|15..11|10..6 |  5..0  |
  -------+------+------+------+------+--------+------------
  000000 | N/A  | rt   | rd   | imm5 | 0000xx | shift-imm
  000000 | rs   | rt   | rd   | N/A  | 0001xx | shift-reg
  000000 | rs   | N/A  | N/A  | N/A  | 001000 | jr
  000000 | rs   | N/A  | rd   | N/A  | 001001 | jalr
  000000 | <-----comment20bit------> | 00110x | sys/brk
  000000 | N/A  | N/A  | rd   | N/A  | 0100x0 | mfhi/mflo
  000000 | rs   | N/A  | N/A  | N/A  | 0100x1 | mthi/mtlo
  000000 | rs   | rt   | N/A  | N/A  | 0110xx | mul/div
  000000 | rs   | rt   | rd   | N/A  | 10xxxx | alu-reg
  000001 | rs   | 00000| <--immediate16bit--> | bltz
  000001 | rs   | 00001| <--immediate16bit--> | bgez
  000001 | rs   | 10000| <--immediate16bit--> | bltzal
  000001 | rs   | 10001| <--immediate16bit--> | bgezal
  000001 | rs   | xxxx0| <--immediate16bit--> | bltz  ;\undocumented aliases
  000001 | rs   | xxxx1| <--immediate16bit--> | bgez  ;/(see §4)
  00001x | <---------immediate26bit---------> | j/jal
  00010x | rs   | rt   | <--immediate16bit--> | beq/bne
  00011x | rs   | N/A  | <--immediate16bit--> | blez/bgtz
  001xxx | rs   | rt   | <--immediate16bit--> | alu-imm
  001111 | N/A  | rt   | <--immediate16bit--> | lui-imm
  100xxx | rs   | rt   | <--immediate16bit--> | load  rt,[rs+imm]
  101xxx | rs   | rt   | <--immediate16bit--> | store rt,[rs+imm]
  x1xxxx | <------coprocessor specific------> | coprocessor (see §5)
```

## 2. The reserved-slot rule `[DOC]`

> "All opcodes that are marked as 'N/A' in the Primary and Secondary opcode tables
> are causing a Reserved Instruction Exception (excode=0Ah). The unused operand bits
> (eg. Bit21-25 for LUI opcode) should be usually zero, but do not necessarily
> trigger exceptions if set to nonzero values."

Two consequences for a decoder:

- **Do not validate unused operand fields.** A nonzero `rs` on `LUI`, a nonzero `rt`
  on `BLEZ`, a nonzero `sa` on `SLLV`: all ignored, none trap.
- **The one exception is the REGIMM `rt` field** (§4), which *is* partially decoded.

Decoders ported from later MIPS ISAs trip on primary `14h..17h`, which are
`BEQL/BNEL/BLEZL/BGTZL` on MIPS-II and **Reserved Instruction** here.

## 3. PRIMARY opcode, bits 31..26

Compact grid `[DOC]`:

```
  00h=SPECIAL 08h=ADDI  10h=COP0 18h=N/A   20h=LB   28h=SB   30h=LWC0 38h=SWC0
  01h=BcondZ  09h=ADDIU 11h=COP1 19h=N/A   21h=LH   29h=SH   31h=LWC1 39h=SWC1
  02h=J       0Ah=SLTI  12h=COP2 1Ah=N/A   22h=LWL  2Ah=SWL  32h=LWC2 3Ah=SWC2
  03h=JAL     0Bh=SLTIU 13h=COP3 1Bh=N/A   23h=LW   2Bh=SW   33h=LWC3 3Bh=SWC3
  04h=BEQ     0Ch=ANDI  14h=N/A  1Ch=N/A   24h=LBU  2Ch=N/A  34h=N/A  3Ch=N/A
  05h=BNE     0Dh=ORI   15h=N/A  1Dh=N/A   25h=LHU  2Dh=N/A  35h=N/A  3Dh=N/A
  06h=BLEZ    0Eh=XORI  16h=N/A  1Eh=N/A   26h=LWR  2Eh=SWR  36h=N/A  3Eh=N/A
  07h=BGTZ    0Fh=LUI   17h=N/A  1Fh=N/A   27h=N/A  2Fh=N/A  37h=N/A  3Fh=N/A
```

Expanded. `pc` in the J/JAL rows is the **delay slot's** address (`$+4`); `$` is the
address of the instruction itself.

| Op | Bits | Mnemonic | Syntax | Operation |
|---|---|---|---|---|
| 00h | 000000 | SPECIAL |, | sub-decode on `funct`: §5 |
| 01h | 000001 | BcondZ (REGIMM) |, | sub-decode on `rt`: §4 |
| 02h | 000010 | J | `j dest` | `npc = (pc & F0000000h) \| (imm26 << 2)` |
| 03h | 000011 | JAL | `jal dest` | `ra = $+8; npc = (pc & F0000000h) \| (imm26 << 2)` |
| 04h | 000100 | BEQ | `beq rs,rt,dest` | `if rs == rt: npc = $+4+(sext(imm16)<<2)` |
| 05h | 000101 | BNE | `bne rs,rt,dest` | `if rs != rt: npc = $+4+(sext(imm16)<<2)` |
| 06h | 000110 | BLEZ | `blez rs,dest` | `if (i32)rs <= 0: branch` |
| 07h | 000111 | BGTZ | `bgtz rs,dest` | `if (i32)rs > 0: branch` |
| 08h | 001000 | ADDI | `addi rt,rs,imm` | `rt = rs + sext(imm16)`, **traps on overflow** |
| 09h | 001001 | ADDIU | `addiu rt,rs,imm` | `rt = rs + sext(imm16)`, wrapping |
| 0Ah | 001010 | SLTI | `slti rt,rs,imm` | `rt = ((i32)rs < (i32)sext(imm16))` |
| 0Bh | 001011 | SLTIU | `sltiu rt,rs,imm` | `rt = ((u32)rs < (u32)sext(imm16))` |
| 0Ch | 001100 | ANDI | `andi rt,rs,imm` | `rt = rs & zext(imm16)` |
| 0Dh | 001101 | ORI | `ori rt,rs,imm` | `rt = rs \| zext(imm16)` |
| 0Eh | 001110 | XORI | `xori rt,rs,imm` | `rt = rs ^ zext(imm16)` |
| 0Fh | 001111 | LUI | `lui rt,imm` | `rt = imm16 << 16` |
| 10h | 010000 | COP0 | §6 | System Control Coprocessor |
| 11h | 010001 | COP1 | §6 | **Absent.** CpU (0Bh), `CE = 1` |
| 12h | 010010 | COP2 | §6 | GTE |
| 13h | 010011 | COP3 | §6 | **Absent.** CpU (0Bh), `CE = 3` |
| 14h–1Fh | 010100–011111 | N/A |, | **RI (0Ah)**: 12 slots. (`BEQL/BNEL/BLEZL/BGTZL/DADDI/DADDIU/LDL/LDR` on later ISAs) |
| 20h | 100000 | LB | `lb rt,imm(rs)` | `rt = sext8([rs+sext(imm16)])` |
| 21h | 100001 | LH | `lh rt,imm(rs)` | `rt = sext16([addr])`; **AdEL if `addr & 1`** |
| 22h | 100010 | LWL | `lwl rt,imm(rs)` | unaligned load, upper part; address force-aligned |
| 23h | 100011 | LW | `lw rt,imm(rs)` | `rt = [addr]`; **AdEL if `addr & 3`** |
| 24h | 100100 | LBU | `lbu rt,imm(rs)` | `rt = zext8([addr])` |
| 25h | 100101 | LHU | `lhu rt,imm(rs)` | `rt = zext16([addr])`; **AdEL if `addr & 1`** |
| 26h | 100110 | LWR | `lwr rt,imm(rs)` | unaligned load, lower part; address force-aligned |
| 27h | 100111 | N/A |, | **RI (0Ah)** (`LWU` on MIPS-III) |
| 28h | 101000 | SB | `sb rt,imm(rs)` | `[addr] = rt & FFh` |
| 29h | 101001 | SH | `sh rt,imm(rs)` | `[addr] = rt & FFFFh`; **AdES if `addr & 1`** |
| 2Ah | 101010 | SWL | `swl rt,imm(rs)` | unaligned store, upper part |
| 2Bh | 101011 | SW | `sw rt,imm(rs)` | `[addr] = rt`; **AdES if `addr & 3`** |
| 2Ch | 101100 | N/A |, | **RI (0Ah)** (`SDL`) |
| 2Dh | 101101 | N/A |, | **RI (0Ah)** (`SDR`) |
| 2Eh | 101110 | SWR | `swr rt,imm(rs)` | unaligned store, lower part |
| 2Fh | 101111 | N/A |, | **RI (0Ah)** (`CACHE` on R4000) |
| 30h | 110000 | LWC0 | `lwc0 rt,imm(rs)` | not implemented, §7 |
| 31h | 110001 | LWC1 | `lwc1 rt,imm(rs)` | COP1 absent, §7 |
| 32h | 110010 | **LWC2** | `lwc2 rt,imm(rs)` | `cop2dat[rt] = [addr]`; **valid**; AdEL if `addr & 3`; CpU if `!SR.CU2` |
| 33h | 110011 | LWC3 | `lwc3 rt,imm(rs)` | COP3 absent, §7 |
| 34h–37h | 110100–110111 | N/A |, | **RI (0Ah)** |
| 38h | 111000 | SWC0 | `swc0 rt,imm(rs)` | not implemented, §7 |
| 39h | 111001 | SWC1 | `swc1 rt,imm(rs)` | COP1 absent, §7 |
| 3Ah | 111010 | **SWC2** | `swc2 rt,imm(rs)` | `[addr] = cop2dat[rt]`; **valid**; AdES if `addr & 3`; CpU if `!SR.CU2` |
| 3Bh | 111011 | SWC3 | `swc3 rt,imm(rs)` | COP3 absent, §7 |
| 3Ch–3Fh | 111100–111111 | N/A |, | **RI (0Ah)** |

## 4. REGIMM / BcondZ, primary `01h`, indexed by `rt` (bits 20..16)

**All 32 `rt` values are valid branches. None of them raises an exception.** A
decoder that only recognises the four canonical patterns and traps on the rest is
wrong.

The hardware decodes `rt` **partially** `[DOC]` `[CONS]`:

| Field | Effect |
|---|---|
| `rt` bit 0 (instruction bit 16) | **alone** selects the condition: `0` → `rs < 0` (BLTZ), `1` → `rs >= 0` (BGEZ) |
| `rt` bits 4..1 (instruction bits 20..17) | link to **r31** iff they equal `1000b`, i.e. iff `(rt & 1Eh) == 10h` |
| all other bits of `rt` | ignored |

```
is_bgez = rt & 1
is_link = (rt & 0x1E) == 0x10          # equivalently ((rt >> 1) & 0xF) == 8
taken   = ((i32)rs < 0) ^ is_bgez
if is_link: r31 = $+8                  # ALWAYS, taken or not
if taken:   npc = $+4 + (sext(imm16) << 2)
```

Two further rules `[DOC]`:

- **The link happens whether or not the branch is taken.**
- **If `rs` is `r31`, the comparison uses `$ra`'s value *before* linking.** Read
  `rs` into a temp first.

| `rt` | Instruction | Links? |
|---|---|---|
| 00h `00000` | **BLTZ** | no |
| 01h `00001` | **BGEZ** | no |
| 02h, 04h, 06h, 08h, 0Ah, 0Ch, 0Eh | BLTZ (alias) | no |
| 03h, 05h, 07h, 09h, 0Bh, 0Dh, 0Fh | BGEZ (alias) | no |
| **10h `10000`** | **BLTZAL** | **yes** |
| **11h `10001`** | **BGEZAL** | **yes** |
| 12h, 14h, 16h, 18h, 1Ah, 1Ch, 1Eh | BLTZ (alias) | **no**: bits 19..17 nonzero cancels the link |
| 13h, 15h, 17h, 19h, 1Bh, 1Dh, 1Fh | BGEZ (alias) | **no** |

> Source note: the alias rows exist only in nocash's original PSX-SPX (`000001 | rs |
> xxxx0 | imm16 | bltz ;\undocumented dupes … (when bit17-19=nonzero)`); the
> consoledev fork lists only the four canonical values, which would wrongly imply the
> other 28 are undefined. Use the nocash rule. `[?]` No published hardware test; two
> independent emulators implement exactly `(rt & 0x1E) == 0x10`.

## 5. SPECIAL, primary `00h`, indexed by `funct` (bits 5..0)

Compact grid `[DOC]`:

```
  00h=SLL   08h=JR      10h=MFHI 18h=MULT  20h=ADD  28h=N/A  30h=N/A  38h=N/A
  01h=N/A   09h=JALR    11h=MTHI 19h=MULTU 21h=ADDU 29h=N/A  31h=N/A  39h=N/A
  02h=SRL   0Ah=N/A     12h=MFLO 1Ah=DIV   22h=SUB  2Ah=SLT  32h=N/A  3Ah=N/A
  03h=SRA   0Bh=N/A     13h=MTLO 1Bh=DIVU  23h=SUBU 2Bh=SLTU 33h=N/A  3Bh=N/A
  04h=SLLV  0Ch=SYSCALL 14h=N/A  1Ch=N/A   24h=AND  2Ch=N/A  34h=N/A  3Ch=N/A
  05h=N/A   0Dh=BREAK   15h=N/A  1Dh=N/A   25h=OR   2Dh=N/A  35h=N/A  3Dh=N/A
  06h=SRLV  0Eh=N/A     16h=N/A  1Eh=N/A   26h=XOR  2Eh=N/A  36h=N/A  3Eh=N/A
  07h=SRAV  0Fh=N/A     17h=N/A  1Fh=N/A   27h=NOR  2Fh=N/A  37h=N/A  3Fh=N/A
```

| funct | Mnemonic | Syntax | Operation |
|---|---|---|---|
| 00h | SLL | `sll rd,rt,sa` | `rd = rt << sa`. `sll r0,r0,0` (word `00000000h`) is **NOP** |
| 01h | N/A |, | **RI (0Ah)** |
| 02h | SRL | `srl rd,rt,sa` | `rd = (u32)rt >> sa` |
| 03h | SRA | `sra rd,rt,sa` | `rd = (i32)rt >> sa` |
| 04h | SLLV | `sllv rd,rt,rs` | `rd = rt << (rs & 1Fh)` |
| 05h | N/A |, | **RI (0Ah)** |
| 06h | SRLV | `srlv rd,rt,rs` | `rd = (u32)rt >> (rs & 1Fh)` |
| 07h | SRAV | `srav rd,rt,rs` | `rd = (i32)rt >> (rs & 1Fh)` |
| 08h | JR | `jr rs` | `npc = rs` |
| 09h | JALR | `jalr rd,rs` | `tmp = rs; rd = $+8; npc = tmp` (rd defaults to r31 in asm, but **any rd is encodable**) |
| 0Ah | N/A |, | **RI (0Ah)** (`MOVZ`) |
| 0Bh | N/A |, | **RI (0Ah)** (`MOVN`) |
| 0Ch | SYSCALL | `syscall imm20` | **Sys (08h)**, executed immediately (no delay slot) |
| 0Dh | BREAK | `break imm20` | **Bp (09h)**, executed immediately |
| 0Eh | N/A |, | **RI (0Ah)** |
| 0Fh | N/A |, | **RI (0Ah)** (`SYNC`) |
| 10h | MFHI | `mfhi rd` | `rd = HI`; **stalls** while mul/div busy |
| 11h | MTHI | `mthi rs` | `HI = rs` |
| 12h | MFLO | `mflo rd` | `rd = LO`; **stalls** while mul/div busy |
| 13h | MTLO | `mtlo rs` | `LO = rs` |
| 14h–17h | N/A |, | **RI (0Ah)** (`DSLLV/DSRLV/DSRAV`) |
| 18h | MULT | `mult rs,rt` | `HI:LO = (i64)rs * (i64)rt` |
| 19h | MULTU | `multu rs,rt` | `HI:LO = (u64)rs * (u64)rt` |
| 1Ah | DIV | `div rs,rt` | signed; **never traps**: see the garbage table below |
| 1Bh | DIVU | `divu rs,rt` | unsigned; never traps |
| 1Ch–1Fh | N/A |, | **RI (0Ah)** (`DMULT/DMULTU/DDIV/DDIVU`) |
| 20h | ADD | `add rd,rs,rt` | `rd = rs + rt`, **traps on overflow, rd unchanged** |
| 21h | ADDU | `addu rd,rs,rt` | wrapping |
| 22h | SUB | `sub rd,rs,rt` | `rd = rs - rt`, **traps on overflow** |
| 23h | SUBU | `subu rd,rs,rt` | wrapping |
| 24h | AND | `and rd,rs,rt` | `rd = rs & rt` |
| 25h | OR | `or rd,rs,rt` | `rd = rs \| rt` |
| 26h | XOR | `xor rd,rs,rt` | `rd = rs ^ rt` |
| 27h | NOR | `nor rd,rs,rt` | `rd = ~(rs \| rt)` |
| 28h, 29h | N/A |, | **RI (0Ah)** |
| 2Ah | SLT | `slt rd,rs,rt` | signed compare |
| 2Bh | SLTU | `sltu rd,rs,rt` | unsigned compare |
| 2Ch–2Fh | N/A |, | **RI (0Ah)** (`DADD/DADDU/DSUB/DSUBU`) |
| 30h–3Fh | N/A |, | **RI (0Ah)**: 16 slots (`TGE/TGEU/TLT/TLTU/TEQ/TNE`, `DSLL/DSRL/DSRA/DSLL32/DSRL32/DSRA32`) |

### Divide-by-zero and divide-overflow results, no exception `[DOC]`

| Opcode | `rs` | `rt` | HI (remainder) | LO (quotient) |
|---|---|---|---|---|
| `DIVU` | any | `0` | `rs` | `FFFFFFFFh` |
| `DIV` | `+0 .. +7FFFFFFFh` | `0` | `rs` | `FFFFFFFFh` (−1) |
| `DIV` | `80000000h .. FFFFFFFFh` | `0` | `rs` | `00000001h` (+1) |
| `DIV` | `80000000h` | `FFFFFFFFh` (−1) | `0` | `80000000h` |

> "For `divu`, the result is more or less correct (as close to infinite as possible).
> For `div`, the results are total garbage (about furthest away from the desired
> result as possible).", psx-spx

`MULT`/`MULTU`/`DIV`/`DIVU` **never** raise an exception under any circumstances.

### JALR caveat `[DOC]`

Assembly syntax varies between vendors (IDT79R3041 writes `jalr rs,rd`, MIPS32
writes `jalr rd,rs`): same encoding either way.

`jalr r31,r31` is encodable and is not trapped, but it destroys the target address.
That is normally harmless, *except* if an IRQ lands between the JALR and its delay
slot: `BD` is set, `EPC` points back at the JALR, and the re-execution jumps to the
already-clobbered register.

## 6. COPn, primary `10h`..`13h`, indexed by bits 25..21

`n` = bits 27..26. **Bit 25 selects move/branch (0) versus coprocessor command (1).**

```
  0100nn |0|0000| rt   | rd   | N/A  | 000000 | MFCn rt,rd_dat  ; rt = dat[rd]
  0100nn |0|0010| rt   | rd   | N/A  | 000000 | CFCn rt,rd_cnt  ; rt = cnt[rd]
  0100nn |0|0100| rt   | rd   | N/A  | 000000 | MTCn rt,rd_dat  ; dat[rd] = rt
  0100nn |0|0110| rt   | rd   | N/A  | 000000 | CTCn rt,rd_cnt  ; cnt[rd] = rt
  0100nn |0|1000|00000 | <--immediate16bit--> | BCnF target ; jump if false
  0100nn |0|1000|00001 | <--immediate16bit--> | BCnT target ; jump if true
  0100nn |1| <--------immediate25bit--------> | COPn imm25
  1100nn | rs   | rt   | <--immediate16bit--> | LWCn rt_dat,[rs+imm]
  1110nn | rs   | rt   | <--immediate16bit--> | SWCn rt_dat,[rs+imm]
```

| Bits 25..21 | Mnemonic | Notes |
|---|---|---|
| `00000` 00h | MFCn | `rt = cop_n.data[rd]`. **1-instruction load delay** |
| `00001` 01h |, | Undefined on R3000/PSX `[?]` |
| `00010` 02h | CFCn | `rt = cop_n.ctrl[rd]` (= `cop_n` reg 32+rd). **1-instruction load delay** |
| `00011` 03h |, | Undefined `[?]` |
| `00100` 04h | MTCn | `cop_n.data[rd] = rt` |
| `00101` 05h |, | Undefined `[?]` |
| `00110` 06h | CTCn | `cop_n.ctrl[rd] = rt` |
| `00111` 07h |, | Undefined `[?]` |
| `01000` 08h | BCnF / BCnT | `rt = 00000` → branch-if-false, `rt = 00001` → branch-if-true. Target = `$+4+(sext(imm16)<<2)` |
| `01001`–`01111` 09h–0Fh |, | Undefined `[?]` |
| `1xxxx` 10h–1Fh | COPn imm25 | Execute coprocessor command `imm25` |

**Per-coprocessor:**

- **COP0 (10h).** `MFC0`/`MTC0` valid. **`CFC0`/`CTC0` address cop0r32..63, which do
  not exist → Reserved Instruction (0Ah)**, *not* Coprocessor Unusable. Same for
  `MFC0` of cop0 r0, r1, r2, r4, r10. Registers r16..r31 return garbage with **no**
  exception, and can be read even in user mode with COP0 disabled. Any *other* COP0
  register access (or `RFE`) with `SR.KUc = 1` and `SR.CU0 = 0` → CpU (0Bh).
- **COP1 (11h), COP3 (13h).** Absent. `SR.CU1`/`SR.CU3` are writable, but no hardware
  exists; with the CU bit clear → CpU (0Bh) with `CE` = 1 or 3. `[?]` What happens
  with the CU bit forced set is undocumented, treating it as a no-op is safe.
- **COP2 (12h) = GTE.** All forms valid. CpU (0Bh, `CE = 2`) if `SR.CU2` (bit 30) is
  clear. Note `SR.CU2` takes ~2 clock cycles to take effect after `MTC0`. `BC2F`
  jumps always and `BC2T` never, the flag reads as permanently false `[DOC]`.

### COP0 cofun (command) encodings, `010000 1 0000 ... funct`

| cofun | Mnemonic | PSX behaviour |
|---|---|---|
| 01h | TLBR | **RI (0Ah)**: no TLB |
| 02h | TLBWI | **RI (0Ah)** |
| 06h | TLBWR | **RI (0Ah)** |
| 08h | TLBP | **RI (0Ah)** |
| **10h** | **RFE** | Return from exception: `SR.2-3 → SR.0-1`, `SR.4-5 → SR.2-3`; bits 4-5 unchanged. **Does not jump.** Works in user mode only if `SR.CU0` is set |
| 00h, 03h–05h, 07h, 09h–0Fh, 11h–1Fh |, | Unused; **execute with no exception**, no known effect. These work even in user mode with COP0 disabled `[DOC]` (nocash only) |
| 20h–1FFFFFFh | mirrors | "the upper 16 bit of the 25 bit command number are ignored", mirror `00h..1Fh` `[DOC]` (nocash only) |

> `[?]` The mirror note implies `RFE` should be matched on the low **5** bits
> (`imm25 & 1Fh == 10h`), whereas masking **6** bits (`instr & 3Fh == 10h`) is the
> common choice. Untested; the 6-bit mask is the safe/common one.

### COP2 `imm25` command layout (for reference; GTE semantics are out of scope) `[DOC]`

```
  31-25  must be 0100101b for "COP2 imm25"
  24-20  fake GTE command number (00h..1Fh): IGNORED by hardware
  19     sf  - shift fraction in IR registers (0 = none, 1 = 12-bit fraction)
  18-17  MVMVA multiply matrix    (0=Rotation, 1=Light, 2=Color, 3=Reserved)
  16-15  MVMVA multiply vector    (0=V0, 1=V1, 2=V2, 3=IR/long)
  14-13  MVMVA translation vector (0=TR, 1=BK, 2=FC/bugged, 3=None)
  12-11  always zero (ignored)
  10     lm  - saturate IR1..IR3  (0 = -8000h..+7FFFh, 1 = 0..+7FFFh)
  9-6    always zero (ignored)
  5-0    REAL GTE command number (00h..3Fh): what the hardware decodes
```

A GTE command instruction is identified by `(instr & FE000000h) == 4A000000h`: the
test the BIOS exception handler uses.

## 7. LWCn / SWCn on the PlayStation

| Op | Mnemonic | Behaviour |
|---|---|---|
| 32h | **LWC2** | Valid. `cop2dat[rt] = [rs+sext(imm16)]`: **GTE data registers 0..31 only**, not control registers. AdEL if `addr & 3`. CpU (`CE=2`) if `!SR.CU2` |
| 3Ah | **SWC2** | Valid. `[rs+sext(imm16)] = cop2dat[rt]`. AdES if `addr & 3`. CpU if `!SR.CU2`. Stalls until any in-flight GTE command completes |
| 31h/39h | LWC1/SWC1 | COP1 absent → CpU (0Bh, `CE = 1`) when `SR.CU1 = 0` |
| 33h/3Bh | LWC3/SWC3 | COP3 absent → CpU (0Bh, `CE = 3`) when `SR.CU3 = 0` |
| 30h/38h | LWC0/SWC0 | Not implemented, and **glitchy rather than clean**: see below |

**LWC0 / SWC0 / BC0F / BC0T** `[DOC]` (nocash; the consoledev fork simplifies this to
"always CpU"):

- With **`SR.CU0 = 1`**: no exception, nothing useful. The branch condition reads as
  always false (`bc0f` always jumps, `bc0t` never); `SWC0` **stores garbage, the
  next opcode word, to memory**; `LWC0` does a dummy memory read without changing
  any cop0 register.
- With **`SR.CU0 = 0`**: Coprocessor Unusable (0Bh): **and this happens even in
  kernel mode**, unlike `MFC0`/`MTC0`/`RFE`, which ignore `CU0 = 0` in kernel mode.

`[?]` Confidence on the `CU0 = 1` behaviours is low (nocash's own text hedges). No
commercial game is known to depend on them; a no-op is safe.

## 8. Assembler pseudo-instructions

Not hardware; listed so a disassembler produces readable output `[DOC]`:

| Pseudo | Real encoding |
|---|---|
| `nop` | `sll r0,r0,0` (`00000000h`) |
| `move rd,rs` | `addu rd,rs,r0` |
| `b dest` | `beq r0,r0,dest` |
| `bal dest` | `bgezal r0,dest` |
| `beqz rs,dest` / `bnez rs,dest` | `beq/bne rs,r0,dest` |
| `li` / `la` | `lui` + `ori` (or a bare `addiu`) |
| `subi rt,rs,imm` / `subiu` | `addi/addiu rt,rs,-imm` |
| `not rd,rs` | `nor rd,rs,r0` |
| `neg rd,rs` | `sub rd,r0,rs` |
