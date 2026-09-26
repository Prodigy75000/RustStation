# HLE kernel: booting without a BIOS file

Written from psx-spx "Kernel (BIOS)" (every section: memory map, function
summary, file, CD-ROM, memory card, interrupt, event, thread, timer, joypad,
GPU, heap, string and misc functions, control blocks, and "BIOS Patches"),
"CDROM File Formats" (the EXE header and SYSTEM.CNF), and "Memory Card Data
Format". Where those are silent, from measurement: a real BIOS run on this
emulator as a black box, and what games ask of the kernel. No BIOS image was
disassembled and no other emulator's kernel was read.

Code: `crates/psx-core/src/hle/`. The libretro core uses it when the
frontend's system directory has no BIOS file and there is a disc to boot.

## Shape

`hle::rom()` is a 512 KB image handed to `Psx::new` like a real BIOS. It holds
a reset vector, the kernel date at BFC00100h, the string "RustStation HLE
BIOS" at BFC00108h (where a real kernel keeps its version string, and how
`Psx` tells the two apart), and at BFC70000h a window of stubs, eight bytes
each, one per kernel function: `j self; nop`.

When the CPU reaches a stub, `Psx::step` hands it to `hle::call`, which does
the function in Rust against the guest registers and memory and returns by
setting the PC. A function that must wait (WaitEvent while the event is busy,
`_card_wait`, gpu_sync) leaves the PC alone: the CPU runs the stub's jump,
comes back, and the function is asked again, with interrupts free to arrive in
between. The window is where a real BIOS keeps character sets, which nothing
executes, so the extra compare in `step` never fires under a real BIOS.

**Everything the kernel knows is in guest RAM**, where psx-spx puts it: the
A/B/C tables, the table of tables at 100h, ExCBs, the PCB, TCBs, EvCBs, FCBs
and DCBs, the heap's block headers, pad buffer pointers, queued card work. The
Rust side keeps nothing between calls. So save states need no new field, and a
game that reads or patches kernel memory finds it where it expects.

Guest code is needed where the game calls into the kernel's code or the kernel
calls into the game's: the exception vector and handler, the walk over the
interrupt chains, ReturnFromException, the ChangeTh syscall, GetConf's first
two instructions (see below), and a loop that calls event callbacks. These are
assembled by `hle/asm.rs` and written into RAM at boot.

Event callbacks from Rust: the list goes on the guest stack in a frame the
callback loop walks (`run_calls`), with $s0 and $s1 saved in the frame. It
nests, so a callback may deliver events with callbacks of their own.

## RAM layout (physical)

| Range | What |
|---|---|
| 0000h-000Fh | Garbage copy of the vector, first word 3 (psx-spx: R-Types and Fade to Black read it) |
| 0060h-006Bh | RAM size 2, 0, FFh |
| 0080h | Exception vector: `lui k0,0; addiu k0,0C80h; jr k0; nop`, the words psx-spx quotes |
| 00A0h, B0h, C0h | Call gates, to the dispatchers |
| 0100h-0157h | Table of tables |
| 0180h-01FFh | SYSTEM.CNF's command line argument |
| 0200h-04FFh | A table (C0h entries) |
| 0510h-053Fh | Spare copies of the three gates |
| 05C4h, 05E0h, 0600h | A, B, C dispatchers |
| 0674h-0873h | C table (80h entries) |
| 0874h-0C73h | B table (100h entries) |
| 0C80h | C(06h), the exception handler |
| 0E00h | The early card handler: returns, with room behind it |
| 0F00h | Register save, chain walk, ReturnFromException, callback loop, GetConf, ChangeTh's syscall |
| 2000h-3FFFh | ChangeClearPAD, B(5Bh), and the 2000h bytes pad patches reach |
| 4000h-4FFFh | Kernel variables |
| 5000h, 5300h, 5700h | FCBs, DCBs, device names |
| 6000h-6FFFh | Exception stack, 1000h bytes |
| E000h-FFFFh | alloc_kernel_memory's 8 KB: ExCBs, PCB, TCBs, EvCBs |

**The gates, the dispatchers and the B and C tables are where a real kernel
has them**, measured, and the gates and dispatchers are its instruction
words (a test holds them to that). psx-spx only fixes the A table at 200h,
and they started out at 500h. Tony Hawk's Pro Skater 2 writes a 1 through a
null pointer to 520h, which on the console is a spare copy of the B gate and
here was B(08h), OpenEvent: the game's next OpenEvent jumped to address 1.
Low RAM is where a game's stray writes land, so its layout is compatibility.

## Boot

1. The I/O registers as a real BIOS leaves them (next section).
2. SYSTEM.CNF from the disc's root: BOOT, TCB, EVENT and STACK, all hex, with
   psx-spx's defaults (PSX.EXE, 4, 10h, 801FFF00h). The line after the file
   name goes to 180h.
3. Kernel memory cleared, vectors, tables, guest code, devices; control blocks
   allocated from kernel memory; the default chain elements queued: CdromDma,
   CdromIo, Syscall at priority 0; vblank and the three root counters at 1;
   DefInt at 3. Five EvCBs taken for the kernel's own CD-ROM events, so a
   game's first event handle is F1000005h, as on the console.
4. The executable loaded with the header's BSS cleared, SP and FP at STACK,
   GP from the header, $a0=1, $a1=0, and a return address that halts: psx-spx
   says returning from the boot executable is a SystemError.

There is no intro and no shell. With no disc there is nothing to boot, and the
libretro core refuses to start rather than show a black screen.

## What the console leaves that psx-spx does not say

**Measured.** The SCPH-1001 BIOS (v2.2) run to Crash Bandicoot's entry point,
8003E018h, every register read there by a scratch test (not committed, as it
needs the BIOS file):

| Register | Value at entry |
|---|---|
| COP0 SR | 40000000h: GTE on, interrupts off |
| I_MASK | 000Ch: CD-ROM and DMA |
| CD-ROM interrupt enable | 1Fh, all five |
| DPCR | 00009099h: MDEC in, MDEC out, CD-ROM |
| DICR | GPU and CD-ROM enabled, master enable |
| SPUCNT | C085h, main volume 3FFFh, reverb volume 5EBCh, reverb base E128h |
| Memory control 1F801000h-20h, 1F801060h | The delays and sizes a BIOS writes; RAM size B88h |
| Root counters | Mode 0, target 0 |
| Cache control (FFFE0130h) | 0001E988h: I-cache on, scratchpad on |

Two of these mattered at once. **libcd's CdInit takes the CD-ROM controller's
interrupts as already enabled**: without the 1Fh its first command timed out
("CD timeout: CD_cw:(CdlNop) Sync=NoIntr"). And the drive's power-on
shell-open status bit is cleared, as the BIOS's own status read would.

**Inferred from games.** Which kernel handlers acknowledge their interrupt by
themselves: the three root counters' and the pad handler's, and not vblank's,
which leaves vblank to the pad handler behind it in the chains. Crash
Bandicoot runs timer 2 through kernel events, and Psy-Q's libetc gave up on
it every frame while nothing cleared it ("intr timeout(0040:004d)"). With
vblank's handler clearing it too, the pad handler never saw a vblank, and a
program using the kernel's pads without libetc never saw a button. libetc
itself takes interrupts through the exception hook (setjmp, then
HookEntryInt) and turns off both vblank acknowledges, ChangeClearPAD(0) and
ChangeClearRCnt(3, 0), so it gets vblank either way.

**The pad buffer is status, ID, then data.** The 5Ah a pad sends after its ID
is not stored. Storing it put every button a byte late: on a phone,
Crash Bandicoot ran on HLE and took no input at all.

**Unresolved exceptions go through A(40h).** An exception no chain element
takes delivers F0000010h, 1000h, then calls whatever the A table holds at
40h, SystemErrorUnresolvedException, and then goes out through the hook. The
kernel's own A(40h) halts, as psx-spx says. The CPU suite's cop test puts its
own function in that table entry to catch the coprocessor-unusable faults it
causes on purpose (it steps EPC past the fault), and passes on a real BIOS,
which is how the order was found: the test's code was read out of RAM, not
the BIOS's.

**InitHeap writes nothing.** It notes the heap's address and size, and the
first malloc writes the first block header. Grand Theft Auto 2 gives InitHeap
eight megabytes starting in memory it keeps using for itself; a header
written there at once was later jumped to.

## Games patch the kernel

psx-spx's "BIOS Patches" lists what commercial games write into kernel code,
found through GetC0Table and GetB0Table at fixed offsets. The handler here is
laid out so each lands harmlessly or does what the game meant:

- **The exception handler's first words.** elo2, Ridge Racer and Pandemonium
  II overwrite words 0 to 13 with a prologue that loads the current TCB and
  saves $at, $v0, $v1, $ra. The handler here starts with that same work, so
  the patch changes nothing that matters. Metal Gear Solid checks words 10 to
  15 against the newer kernel's and, finding them, writes its reordering of
  the same stores.
- **The early card handler** at C(06h)+70h: its address as a lui/addiu pair,
  read by Metal Gear Solid and elo2 to find the handler and patch 28h bytes
  into it; there is room there. Breath of Fire III and Ace Combat 2 write nops
  over the call to uninstall it.
- **The lightgun slot**, four free words at C(06h)+80h, which Sporting Clays
  and Dragon Quest Monsters fill with a call to their own routine. It runs
  there, with every register saved and the exception stack.
- **ChangeClearPAD's neighbourhood.** Games call B(5Bh)+7A0h (SetPadOutput,
  Resident Evil 2's rumble), +884h and +894h (pad enable and disable), so
  those are real entry points. Resident Evil 2 and Sporting Clays turn the pad
  handler's acknowledge off with nine nops at +62Ch, and the pad handler looks
  for them. Everything else there is filler that is never run.
- **GetConf's first two instructions** are `lui`/`lw` of boot_cnf_values + 8,
  because Spec Ops finds that structure by decoding them.

## Devices

- **cdrom:** reads the disc image directly through `hle/iso.rs` (ISO 9660: the
  volume descriptor at 16, directories, names with or without ";1"). A read
  costs no emulated time and never touches the drive, which stays the game's
  from its own first command. Whole sectors: a read may run to the end of the
  file's last sector.
- **bu00: and bu10:** work on the card image's directory directly: create
  (bit 9 of the mode, blocks in bits 16 to 31), open, read and write in 80h
  steps through the block chain, firstfile/nextfile with "?" and "*", erase,
  undelete, rename, format. Asynchronous (bit 15) completes at once, returns 0
  ("accepted", which Metal Slug X loops on), and is reported from the next
  vblank, fd and F4000001h, 4.
- **Card file functions report sector work** on F0000011h, 4 (100h with no
  card) from the next vblank, as the console's file functions do by going
  through its sector routine. Metal Slug X waits for it after firstfile.
- **Low-level card functions**: `_card_read`, `_card_write`, `_card_info`,
  `_card_load` queue one operation per slot, done by the pad and card handler
  on the next vblank. Reads and writes report on F0000011h, info and load on
  F4000001h: 4 done, 100h no card, 2000h new card, 8000h error. The new-card
  flag fails reads and writes until `_new_card`, and a write clears it.
- **tty:** the host's log (`shot --tty`), with "[hle]" notes from the kernel.
- **Pads** are read on vblank by the pad and card handler straight from the
  pad model, 42h and up to 20h bytes after the ID, with SetPadOutput's bytes
  sent. No SIO traffic, so `sio: 0 bytes exchanged` under HLE is expected when
  a game uses the kernel's pad functions.

## Not done

- qsort, lsearch and bsearch (callbacks to a game's compare function): logged
  and return 0. No game on hand calls them yet.
- The character sets. Krom2RawAdd answers -1; a Japanese game drawing text
  with the kernel's font gets nothing.
- The CardSpecificIrq element: card work is done on vblank without SIO
  interrupts, so nothing needs it.
- TTY input: getchar and gets return at once.
- The I-cache's contents. FlushCache and LoadExec invalidate its tags, which
  is all the timing model keeps; code in the cache that RAM has since
  overwritten is not served stale.
- What several functions return where psx-spx does not say (InitPAD2,
  `_bu_init`, SysEnqIntRP): 1 or 0 as seemed natural.

## Save states

A save state made on a real BIOS carries that BIOS's kernel in RAM, whose
tables point into its ROM; loaded on the HLE ROM they point at nothing. The
same goes the other way. A state belongs to the kernel it was made on.
