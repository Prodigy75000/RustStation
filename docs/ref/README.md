# `docs/ref/`: hardware reference

Reference material for the hardware RustStation emulates. These documents describe
**the machine**, not this codebase. Implementation notes, open questions and
decisions live in `docs/notes/`.

## Scope of this set

The current set covers the **CPU core only**: the MIPS R3000A (LSI CW33300) and
its system control coprocessor, COP0.

| File | Covers |
|---|---|
| [`01-cpu-overview.md`](01-cpu-overview.md) | Core identity, register file, the execution model, reset state, and the pseudocode an interpreter should be shaped like |
| [`02-instruction-encoding.md`](02-instruction-encoding.md) | Instruction word layout; complete PRIMARY / SPECIAL / REGIMM / COPn decode tables, all 64 (or 32) entries each, including every reserved slot |
| [`03-instruction-set.md`](03-instruction-set.md) | Per-instruction semantics in pseudocode, with exact sign/zero extension, and the exceptions each instruction can raise |
| [`04-delay-slots-and-hazards.md`](04-delay-slots-and-hazards.md) | Branch delay slots, load delay slots, LWL/LWR bypass, HI/LO interlocks, overflow, alignment. The pipeline artifacts software can see |
| [`05-cop0-and-exceptions.md`](05-cop0-and-exceptions.md) | COP0 register file, full SR / Cause / DCIC bit layouts, the exception model, vectors, RFE, and the interrupt-taken condition |
| [`06-conformance-notes.md`](06-conformance-notes.md) | Where `crates/psx-core` currently diverges from the above, ranked, with the test in `tests/test-suite` that would catch each |

Not covered yet (deliberately out of scope for this pass): memory map, caches and
timing; the GTE (COP2) command set; the interrupt controller's device side;
everything downstream of the CPU (GPU, SPU, CDROM, MDEC, DMA, timers).

## Confidence markers

Claims are marked where it matters:

- **`[DOC]`**: stated in psx-spx, nocash's original PSX-SPX, or an IDT R30xx manual.
- **`[HW]`**: explicitly hardware-verified in a cited source.
- **Unmarked "widely documented behaviour"**: stated in prose where a claim is
  commonly described but appears in none of the primary sources below. Treat it as
  unverified until a hardware test settles it.
- **`[?]`**: genuinely uncertain, or sources disagree. **Do not encode `[?]` claims
  as invariants or assertions.** Where a `[?]` matters, the safe choice is stated.

If you are about to write a test that asserts a `[?]` behaviour, write it as a
`#[ignore]`d test with the open question recorded in `docs/notes/` instead.

## Clean-room note

RustStation is a clean-room implementation. Everything here is derived from
**hardware documentation and hardware test results**: psx-spx, nocash's PSX-SPX,
LSI/IDT datasheets and manuals, not from another emulator's source code.

Third-party documentation is distilled and cited, never reproduced: every statement
taken from psx-spx or an IDT manual is restated here in our own words with a pointer
to the section it came from.

**Do not paste code from another emulator into this repo, and do not read another
emulator's source to resolve an ambiguity.** If a behaviour is unclear, the correct
move is to write a test for `tests/test-suite` and settle it on hardware or against
the existing captured logs.

## Primary sources

- **psx-spx (consoledev fork)**: the maintained, hardware-tested community version.
  <https://psx-spx.consoledev.net/cpuspecifications/>
  Raw markdown: <https://raw.githubusercontent.com/psx-spx/psx-spx.github.io/master/docs/cpuspecifications.md>
- **nocash PSX-SPX (Martin Korth), original**: contains material the fork dropped
  (REGIMM aliases, COP0 cofun mirrors, SR write-timing, LWC0/SWC0 glitches).
  <https://problemkaputt.de/psx-spx.htm>
- **IDT R30xx Family Software Reference Manual**: the R3000A architecture book;
  Appendix A is the authoritative ISA pseudocode, Ch. 3–4 the CP0/exception model,
  Ch. 13 the hazard list. <https://cgi.cse.unsw.edu.au/~cs3231/doc/R3000.pdf>
- **IDT R3051/R3052 RISController Hardware User's Manual**, Ch. 5.
  <https://stuff.mit.edu/afs/sipb/contrib/doc/specs/ic/cpu/mips/r3051.pdf>

The PSX CPU is **not** an IDT part. It is an LSI CoreWare **CW33300** core, R3000A
instruction-set compatible but with **no TLB** and with **LSI-specific COP0 debug
registers**. Where an IDT manual and psx-spx disagree, psx-spx's hardware-tested
statement wins; the IDT manuals are the right source for generic MIPS-I semantics
and for the *reasoning* behind a hazard, not for PSX-specific behaviour. The only
primary documentation for the COP0 debug block is LSI's LR33300/LR33310 datasheet
(ch. 4) and L64360 datasheet (ch. 14), neither of which is freely reachable.

The two spx variants have diverged in both directions, the fork adds
hardware-measured load timing and cache detail, and drops some of nocash's
observations. For encoding tables specifically, cross-check both.
