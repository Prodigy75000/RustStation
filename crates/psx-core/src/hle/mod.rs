// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A high-level emulated BIOS: what a player without a BIOS file boots with.
//!
//! Written from psx-spx, "Kernel (BIOS)", and nothing else: no BIOS image and
//! no other emulator's kernel was read. It is a kernel, not an intro: there is
//! no Sony logo and no shell, the disc's executable starts at once.
//!
//! ## Shape
//!
//! The ROM ([`rom`]) is mostly empty. It holds a reset vector and a window of
//! two-instruction stubs, one per kernel function, each a jump to itself.
//! When the CPU reaches one, [`Psx::step`] hands control here instead, and
//! the function runs in Rust against guest registers and memory, then
//! returns by setting the PC. A function that has to wait (`WaitEvent`,
//! `_card_wait`) just does not move the PC: the CPU runs the stub's jump,
//! comes back, and asks again, with interrupts free to arrive in between.
//!
//! Everything the kernel knows lives in guest RAM, as on the console: the
//! function tables, events, threads, the exception chains, the heap, open
//! files and the pad buffers. None of it is in Rust. So a save state needs
//! nothing new, and a game that reads or patches kernel memory finds it
//! where psx-spx says it is.
//!
//! A few routines have to be guest code, because the game can call into them
//! or they call into the game: the exception vector and handler, the chain
//! walk that calls each interrupt handler in turn, `ReturnFromException`,
//! and a loop that calls event callbacks. Those are assembled by [`asm`] and
//! written into RAM at boot.
//!
//! ## Games patch the kernel
//!
//! psx-spx lists the patches commercial games make to kernel code through
//! `GetC0Table` and `GetB0Table`, at fixed offsets from the exception
//! handler and from `ChangeClearPAD`. The handler here is laid out so each
//! one lands harmlessly or does what the game meant: its first sixteen words
//! are the prologue those patches expect, and the words they overwrite or
//! call into are where they look for them. See [`exception_handler`].

pub mod asm;
mod card;
mod files;
pub mod iso;
mod libc;

use crate::Psx;
use asm::*;

/// Found at [`SIGNATURE_AT`] in the ROM, which is where a real BIOS keeps its
/// kernel version string. How [`Psx`] knows to run the kernel here.
pub const SIGNATURE: &[u8] = b"RustStation HLE BIOS";
const SIGNATURE_AT: usize = 0x108;

pub fn is_hle(bios: &[u8]) -> bool {
    bios.get(SIGNATURE_AT..SIGNATURE_AT + SIGNATURE.len()) == Some(SIGNATURE)
}

/// The stub window, physical. A real BIOS keeps character sets here, which
/// nothing executes, so a real BIOS never trips the check in `step`.
pub(crate) const TRAP_PHYS: u32 = 0x1FC7_0000;
pub(crate) const TRAP_PHYS_END: u32 = 0x1FC7_8000;
const TRAP_KSEG1: u32 = 0xBFC7_0000;

fn trap(id: u32) -> u32 {
    TRAP_KSEG1 + id * 8
}

/// Stub numbers. The A, B and C functions take their own numbers; the rest
/// are the kernel's own entry points. Keep these stable: a save state holds
/// return addresses into the stubs.
const FN_A: u32 = 0x000;
const FN_B: u32 = 0x100;
const FN_C: u32 = 0x200;
const T_BOOT: u32 = 0x300;
const T_CHAINS_DONE: u32 = 0x301;
const T_EXE_RETURNED: u32 = 0x302;
const T_HANG: u32 = 0x303;
const T_ZERO: u32 = 0x304;
const T_SYSCALL_TEST: u32 = 0x305;
const T_SYSCALL: u32 = 0x306;
const T_VBLANK_TEST: u32 = 0x307;
const T_VBLANK: u32 = 0x308;
/// Three each, for root counters 0 to 2.
const T_TIMER_TEST: u32 = 0x309;
const T_TIMER: u32 = 0x30C;
const T_PADCARD_TEST: u32 = 0x30F;
const T_PADCARD: u32 = 0x310;
const T_DEFINT_TEST: u32 = 0x311;
const T_DEFINT: u32 = 0x312;
const T_BAD_A: u32 = 0x313;
const T_BAD_B: u32 = 0x314;
const T_BAD_C: u32 = 0x315;
const T_EXEC_RETURNED: u32 = 0x316;
const T_SET_PAD_OUTPUT: u32 = 0x317;
const T_PAD_ENABLE: u32 = 0x318;
const T_PAD_DISABLE: u32 = 0x319;
const T_LOADEXEC_RETURNED: u32 = 0x31A;
const T_TAKE_HOOK: u32 = 0x31B;
const T_UNRESOLVED: u32 = 0x31C;
const TRAP_COUNT: u32 = 0x320;

// ---- RAM layout (physical) ---------------------------------------------

/// psx-spx "BIOS RAM Map" puts the A table at 200h. The B and C tables and
/// the dispatchers are where a real kernel keeps them, measured: games write
/// through null pointers into this low memory, and Tony Hawk's Pro Skater 2
/// stores a 1 at 520h, which on the console is a spare copy of the B gate
/// and here, when the B table started at 500h, was OpenEvent.
const A_TABLE: u32 = 0x0200;
const C_TABLE: u32 = 0x0674;
const B_TABLE: u32 = 0x0874;
const B_COUNT: u32 = 0x100;
const C_COUNT: u32 = 0x80;
const A_COUNT: u32 = 0xC0;
/// The A, B and C dispatchers.
const DISPATCH: [u32; 3] = [0x05C4, 0x05E0, 0x0600];
/// C(06h). At 0C80h so the four words at 80h, and their garbage copy at 0,
/// are the ones psx-spx quotes, which a couple of games read by accident.
const EXC_HANDLER: u32 = 0x0C80;
/// The "early card IRQ handler" the exception handler calls, which one
/// patch writes into 28h bytes in.
const EARLY_CARD: u32 = 0x0E00;
/// The rest of the guest code.
const CODE: u32 = 0x0F00;
/// ChangeClearPAD, B(5Bh), and the 2000h bytes after it that patches reach.
const PAD_FUNCS: u32 = 0x2000;
const VARS: u32 = 0x4000;
const FCB_BASE: u32 = 0x5000;
const FCB_COUNT: u32 = 16;
const FCB_SIZE: u32 = 0x2C;
const DCB_BASE: u32 = 0x5300;
const DCB_COUNT: u32 = 10;
const DCB_SIZE: u32 = 0x50;
const NAMES: u32 = 0x5700;
/// The exception stack, 1000h bytes, growing down.
const EXC_STACK_TOP: u32 = 0x8000_7000;
/// alloc_kernel_memory's 8 KB.
const KMEM: u32 = 0xE000;
const KMEM_SIZE: u32 = 0x2000;

fn kseg0(phys: u32) -> u32 {
    0x8000_0000 | phys
}
fn kseg1(phys: u32) -> u32 {
    0xA000_0000 | phys
}

// ---- kernel variables, offsets into VARS ---------------------------------

const V_HEAP_START: u32 = 0x00;
const V_HEAP_END: u32 = 0x04;
const V_RAND: u32 = 0x08;
const V_HOOK: u32 = 0x0C;
/// Four words: the ChangeClearRCnt flags for root counters 0 to 2 and vblank.
const V_RCNT_CLEAR: u32 = 0x10;
const V_PAD_CLEAR: u32 = 0x20;
const V_AUTOACK: u32 = 0x24;
const V_PAD_BUF1: u32 = 0x28;
const V_PAD_SIZ1: u32 = 0x2C;
const V_PAD_BUF2: u32 = 0x30;
const V_PAD_SIZ2: u32 = 0x34;
const V_PAD_STARTED: u32 = 0x38;
const V_PAD_ENABLE: u32 = 0x3C;
const V_PAD_BUTTONS: u32 = 0x40;
const V_PAD_OUT1: u32 = 0x44;
const V_PAD_OUT2: u32 = 0x48;
const V_ERRNO: u32 = 0x4C;
const V_CARD_FIND: u32 = 0x50;
const V_CARD_AUTO: u32 = 0x54;
const V_CARD_CHAN: u32 = 0x58;
const V_CARD_STARTED: u32 = 0x5C;
const V_CARD_IGNORE_NEW: u32 = 0x60;
/// Per slot, 10h bytes each: operation, sector, address, status.
const V_CARD_OPS: u32 = 0x64;
const V_CD_LBA: u32 = 0x84;
const V_CD_MODE: u32 = 0x88;
/// num_TCB, num_EvCB, stacktop: what psx-spx calls boot_cnf_values, which
/// one game finds by decoding GetConf's first two instructions.
const V_CONF: u32 = 0x90;
const V_INT5: u32 = 0x9C;
const V_SEARCH_DEV: u32 = 0xA0;
const V_SEARCH_NEXT: u32 = 0xA4;
const V_STRTOK: u32 = 0xA8;
const V_PAD_INIT2: u32 = 0xAC;
const V_BOOT_STACK: u32 = 0xB0;
/// Whether malloc has written the heap's first block header yet.
const V_HEAP_READY: u32 = 0xB4;
/// ResetEntryInt's structure, 30h bytes.
const V_DEFAULT_HOOK: u32 = 0xC0;
/// Eight queued (class, spec) events for the next vblank, and a count.
const V_PENDING_COUNT: u32 = 0xF0;
const V_PENDING: u32 = 0x100;
const PENDING_MAX: u32 = 8;
const V_CURDIR: u32 = 0x140;
const V_PATTERN: u32 = 0x1C0;
const V_STRTOK_BUF: u32 = 0x200;
const V_PAD_INTERNAL: u32 = 0x300;
const V_BOOT_HEADER: u32 = 0x380;
const V_BOOT_NAME: u32 = 0x3C0;
/// The default interrupt chain elements, 10h bytes each.
const V_ELEMENTS: u32 = 0x400;

fn var(off: u32) -> u32 {
    kseg1(VARS + off)
}

/// Element slots in V_ELEMENTS.
const E_CDROM_DMA: u32 = 0;
const E_CDROM_IO: u32 = 1;
const E_SYSCALL: u32 = 2;
const E_VBLANK: u32 = 3;
const E_TIMER2: u32 = 4;
const E_TIMER1: u32 = 5;
const E_TIMER0: u32 = 6;
const E_PADCARD: u32 = 7;
const E_DEFINT: u32 = 8;

fn element(n: u32) -> u32 {
    var(V_ELEMENTS + n * 0x10)
}

// ---- events -------------------------------------------------------------

const EV_FREE: u32 = 0;
const EV_DISABLED: u32 = 0x1000;
const EV_BUSY: u32 = 0x2000;
const EV_READY: u32 = 0x4000;
const MODE_CALLBACK: u32 = 0x1000;
const MODE_READY: u32 = 0x2000;
const EVCB_SIZE: u32 = 0x1C;
const TCB_SIZE: u32 = 0xC0;

/// Event classes and specs the kernel delivers (psx-spx "BIOS Event Summary").
const CLASS_CDROM: u32 = 0xF000_0003;
const CLASS_EXCEPTION: u32 = 0xF000_0010;
const CLASS_HWCARD: u32 = 0xF000_0011;
const CLASS_SWCARD: u32 = 0xF400_0001;
const SPEC_IO_END: u32 = 0x0004;
const SPEC_COMPLETE: u32 = 0x0020;
const SPEC_TIMEOUT: u32 = 0x0100;
const SPEC_NEW: u32 = 0x2000;
const SPEC_ERROR: u32 = 0x8000;

/// I/O ports the kernel touches.
const GP0: u32 = 0x1F80_1810;
const GP1: u32 = 0x1F80_1814;

// ---- the ROM --------------------------------------------------------------

/// The HLE BIOS image: 512 KB, handed to [`Psx::new`] like a real one.
pub fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; crate::bus::BIOS_SIZE];
    let mut put = |at: usize, w: u32| rom[at..at + 4].copy_from_slice(&w.to_le_bytes());

    // Reset: straight into the boot stub.
    let mut a = Asm::new(0xBFC0_0000);
    a.j(trap(T_BOOT));
    a.nop();
    for (i, w) in a.finish().into_iter().enumerate() {
        put(i * 4, w);
    }
    // Kernel date, psx-spx "BIOS ROM Header": the kernel whose behaviour
    // this follows, from 1995-12-04.
    put(0x100, 0x1995_1204);
    // Every stub is a jump to itself.
    for id in 0..TRAP_COUNT {
        let at = trap(id);
        let mut a = Asm::new(at);
        a.j(at);
        a.nop();
        let off = (at - 0xBFC0_0000) as usize;
        for (i, w) in a.finish().into_iter().enumerate() {
            put(off + i * 4, w);
        }
    }
    rom[SIGNATURE_AT..SIGNATURE_AT + SIGNATURE.len()].copy_from_slice(SIGNATURE);
    let version = b"RustStation HLE BIOS\0";
    rom[0x7FF32..0x7FF32 + version.len()].copy_from_slice(version);
    rom
}

// ---- guest access -----------------------------------------------------------

fn reg(p: &Psx, r: u32) -> u32 {
    p.cpu.reg(r)
}
fn set(p: &mut Psx, r: u32, v: u32) {
    p.cpu.force_reg(r, v);
}
fn rd8(p: &mut Psx, a: u32) -> u8 {
    p.bus.load8(a)
}
fn rd16(p: &mut Psx, a: u32) -> u16 {
    p.bus.load16(a & !1)
}
fn rd32(p: &mut Psx, a: u32) -> u32 {
    p.bus.load32(a & !3)
}
fn wr8(p: &mut Psx, a: u32, v: u8) {
    p.bus.store8(a, v);
}
fn wr16(p: &mut Psx, a: u32, v: u16) {
    p.bus.store16(a & !1, v);
}
fn wr32(p: &mut Psx, a: u32, v: u32) {
    p.bus.store32(a & !3, v);
}

/// Argument `n`: the first four in registers, the rest on the stack above
/// the four words the caller reserves for them.
fn arg(p: &mut Psx, n: u32) -> u32 {
    if n < 4 {
        reg(p, A0 + n)
    } else {
        let sp = reg(p, SP);
        rd32(p, sp + 0x10 + (n - 4) * 4)
    }
}

fn jump(p: &mut Psx, to: u32) {
    p.cpu.set_pc(to);
}

/// Return to the caller with `v` in $v0.
fn ret(p: &mut Psx, v: u32) {
    set(p, V0, v);
    let ra = reg(p, RA);
    jump(p, ra);
}

/// Return to the caller leaving $v0 alone.
fn ret_void(p: &mut Psx) {
    let ra = reg(p, RA);
    jump(p, ra);
}

/// A NUL-terminated guest string, up to `max` bytes.
fn string(p: &mut Psx, mut a: u32, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < max {
        let c = rd8(p, a);
        if c == 0 {
            break;
        }
        out.push(c);
        a = a.wrapping_add(1);
    }
    out
}

fn put_string(p: &mut Psx, a: u32, s: &[u8]) {
    for (i, &c) in s.iter().enumerate() {
        wr8(p, a + i as u32, c);
    }
    wr8(p, a + s.len() as u32, 0);
}

/// A message on the kernel's TTY, which is the host's log of what the game
/// printed. Never read by emulated code.
fn log(p: &mut Psx, msg: &str) {
    p.tty.extend_from_slice(b"[hle] ");
    p.tty.extend_from_slice(msg.as_bytes());
    p.tty.push(b'\n');
}

// ---- entry from Psx::step ---------------------------------------------------

/// The CPU is at stub `id`.
pub(crate) fn call(p: &mut Psx, id: u32) {
    // A load issued in the delay slot of the call is the function's to see.
    p.cpu.settle();
    if tracing() {
        let name = match id {
            _ if id < FN_B => format!("A({:02X}h)", id),
            _ if id < FN_C => format!("B({:02X}h)", id - FN_B),
            _ if id < T_BOOT => format!("C({:02X}h)", id - FN_C),
            _ => format!("stub {id:#X}"),
        };
        let (a0, a1, a2, a3, ra) = (reg(p, A0), reg(p, A1), reg(p, A2), reg(p, A3), reg(p, RA));
        eprintln!("hle: {name} {a0:08X} {a1:08X} {a2:08X} {a3:08X} ra={ra:08X}");
    }
    match id {
        T_BOOT => boot(p),
        T_CHAINS_DONE => chains_done(p),
        T_UNRESOLVED => {
            // A(40h) through the table, as a function, then the hook.
            let f = rd32(p, kseg1(A_TABLE + 0x40 * 4));
            set(p, RA, trap(T_TAKE_HOOK));
            jump(p, f);
        }
        T_TAKE_HOOK => {
            let hook = rd32(p, var(V_HOOK));
            longjmp_from(p, hook, 1);
        }
        T_EXE_RETURNED => {
            log(
                p,
                "the boot executable returned; halting as the console does",
            );
            jump(p, trap(T_HANG));
        }
        T_LOADEXEC_RETURNED => files::loadexec_returned(p),
        T_EXEC_RETURNED => files::exec_returned(p),
        T_HANG => {}
        T_ZERO => ret(p, 0),
        T_SYSCALL_TEST => {
            let tcb = current_tcb(p);
            let cause = rd32(p, tcb + 0x98);
            ret(p, u32::from((cause >> 2) & 0x1F == 8));
        }
        T_SYSCALL => syscall(p),
        T_VBLANK_TEST => irq_test(p, 1 << 0),
        T_VBLANK => timer_irq(p, 3),
        _ if (T_TIMER_TEST..T_TIMER_TEST + 3).contains(&id) => {
            irq_test(p, 1 << (4 + id - T_TIMER_TEST))
        }
        _ if (T_TIMER..T_TIMER + 3).contains(&id) => timer_irq(p, id - T_TIMER),
        T_PADCARD_TEST => irq_test(p, 1 << 0),
        T_PADCARD => pad_card_irq(p),
        T_DEFINT_TEST => {
            let pending = p.bus.irq.stat() & p.bus.irq.mask();
            ret(p, u32::from(pending != 0));
        }
        T_DEFINT => default_irq(p),
        T_BAD_A | T_BAD_B | T_BAD_C => {
            let t = ["A", "B", "C"][(id - T_BAD_A) as usize];
            let n = reg(p, T1);
            log(p, &format!("{t}({n:02X}h) does not exist"));
            ret(p, 0);
        }
        T_SET_PAD_OUTPUT => {
            // SetPadOutput(src1, blah1, src2, blah2): only the sources count.
            let (a, b) = (arg(p, 0), arg(p, 2));
            wr32(p, var(V_PAD_OUT1), a);
            wr32(p, var(V_PAD_OUT2), b);
            ret(p, 0);
        }
        T_PAD_ENABLE | T_PAD_DISABLE => {
            wr32(p, var(V_PAD_ENABLE), u32::from(id == T_PAD_ENABLE));
            ret(p, 0);
        }
        _ if id < FN_B => a_function(p, id - FN_A),
        _ if id < FN_C => b_function(p, id - FN_B),
        _ if id < T_BOOT => c_function(p, id - FN_C),
        _ => {
            log(p, &format!("stub {id:#X} has no function"));
            ret(p, 0);
        }
    }
}

/// `RSTA_HLE_TRACE=1`: every kernel call on stderr, for finding what a
/// game asked of the kernel. Host side only. Under a real BIOS the call
/// gates print the same lines, so a game's calls on the two can be diffed.
pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RSTA_HLE_TRACE").is_some())
}

fn not_implemented(p: &mut Psx, what: &str) {
    log(p, &format!("{what} is not implemented; returning 0"));
    ret(p, 0);
}

/// A function the console hangs in, printing what it can first.
fn system_error(p: &mut Psx, what: &str) {
    log(p, &format!("{what}: system error, halted"));
    jump(p, trap(T_HANG));
}

fn a_function(p: &mut Psx, n: u32) {
    match n {
        0x00 => files::open(p),
        0x01 => files::lseek(p),
        0x02 => files::read(p),
        0x03 => files::write(p),
        0x04 => files::close(p),
        0x05 => ret(p, 0),
        0x06 | 0x3A => system_error(p, "exit"),
        0x07 => files::isatty(p),
        0x08 => files::getc(p),
        0x09 => files::putc(p),
        0x0A | 0x0C..=0x12 | 0x15..=0x30 | 0x33 | 0x34 | 0x37..=0x39 => libc::call(p, n),
        0x0B | 0x32 => system_error(p, "atof/strtod need the absent FPU"),
        0x13 => setjmp(p),
        0x14 => longjmp(p),
        0x31 | 0x35 | 0x36 => {
            not_implemented(p, &format!("A({n:02X}h), sort/search with a callback"))
        }
        0x3B => ret(p, 0xFFFF_FFFF),
        0x3C => {
            let c = arg(p, 0) as u8;
            libc::tty_putchar(p, c);
            ret(p, c as u32);
        }
        0x3D => ret(p, 0),
        0x3E => libc::puts(p),
        0x3F => libc::printf(p),
        0x40 => {
            let tcb = current_tcb(p);
            let (cause, epc) = (rd32(p, tcb + 0x98), rd32(p, tcb + 0x88));
            let (ra, bad) = (rd32(p, tcb + 8 + 31 * 4), p.cpu.cop0.bad_vaddr);
            let code = (cause >> 2) & 0x1F;
            system_error(p, &format!("unresolved exception {code:#X} at {epc:08X} (ra {ra:08X}, bad address {bad:08X})"));
        }
        0x41 => files::load_test(p),
        0x42 => files::load(p),
        0x43 => files::exec(p),
        0x44 => ret(p, 0),
        0x45 => {
            write_call_vectors(p);
            ret(p, 0);
        }
        0x46 => gpu_dw(p),
        0x47 => gpu_send_dma(p),
        0x48 => {
            let v = arg(p, 0);
            wr32(p, GP1, v);
            ret_void(p);
        }
        0x49 => {
            let v = arg(p, 0);
            wr32(p, GP0, v);
            ret(p, 0);
        }
        0x4A => {
            let (src, n) = (arg(p, 0), arg(p, 1));
            for i in 0..n.min(0x10000) {
                let w = rd32(p, src + i * 4);
                wr32(p, GP0, w);
            }
            ret(p, 0);
        }
        0x4B => {
            let src = arg(p, 0);
            wr32(p, GP1, 0x0400_0002);
            wr32(p, 0x1F80_10F4, 0);
            let dpcr = rd32(p, 0x1F80_10F0);
            wr32(p, 0x1F80_10F0, dpcr | 0x800);
            wr32(p, 0x1F80_10A0, src);
            wr32(p, 0x1F80_10A4, 0);
            wr32(p, 0x1F80_10A8, 0x0100_0401);
            ret_void(p);
        }
        0x4C => {
            wr32(p, 0x1F80_10A8, 0x401);
            wr32(p, GP1, 0x0400_0000);
            wr32(p, GP1, 0x0200_0000);
            wr32(p, GP1, 0x0100_0000);
            ret(p, GP1);
        }
        0x4D => {
            let v = rd32(p, GP1);
            ret(p, v);
        }
        0x4E => gpu_sync(p),
        0x4F | 0x50 | 0x52 | 0x53 | 0x9A | 0x9B => system_error(p, &format!("A({n:02X}h)")),
        0x51 => files::load_exec(p),
        0x54 | 0x71 | 0x56 | 0x72 | 0x90..=0x93 | 0x95..=0x99 | 0x9E | 0xA2 | 0xA3 => ret(p, 0),
        0x55 | 0x70 => card::bu_init(p),
        0x57..=0x5A | 0x73..=0x77 | 0x79..=0x7B | 0x7D | 0x7F | 0x80 | 0x82..=0x8F => ret(p, 0),
        0x5B..=0x6F => ret(p, 0),
        0x78 => files::cd_async_seek(p),
        0x7C => files::cd_async_status(p),
        0x7E => files::cd_async_read(p),
        0x81 => files::cd_async_mode(p),
        0x94 => {
            let (d1, d2) = (arg(p, 0), arg(p, 1));
            let v = rd16(p, var(V_INT5));
            wr8(p, d1, v as u8);
            wr8(p, d2, (v >> 8) as u8);
            ret(p, 0);
        }
        0x9C => set_conf(p),
        0x9D => get_conf(p),
        0x9F => {
            let mb = arg(p, 0);
            wr32(p, 0x1F80_1060, if mb == 8 { 0x0B88 } else { 0x0888 });
            wr32(p, kseg1(0x60), mb);
            ret(p, 0);
        }
        0xA0 => boot(p),
        0xA1 => {
            let (t, code) = (arg(p, 0), arg(p, 1));
            system_error(p, &format!("SystemError({}, {code:X})", t as u8 as char));
        }
        0xA4 => files::cd_get_lbn(p),
        0xA5 => files::cd_read_sector(p),
        0xA6 => ret(p, 0x02),
        0xA7 => card::software_event(p, SPEC_IO_END),
        0xA8 | 0xAE => card::software_event(p, SPEC_ERROR),
        0xA9 => card::software_event(p, SPEC_TIMEOUT),
        0xAA => card::software_event(p, SPEC_NEW),
        0xAB => card::card_info(p),
        0xAC => card::card_load(p),
        0xAD => {
            let f = arg(p, 0);
            wr32(p, var(V_CARD_AUTO), f);
            ret(p, 0);
        }
        0xAF => card::card_write_test(p),
        0xB0 | 0xB1 | 0xB3 => ret(p, 0),
        0xB2 => system_error(p, "_ioabort"),
        0xB4 => get_system_info(p),
        _ => {
            log(p, &format!("A({n:02X}h) does not exist"));
            ret(p, 0);
        }
    }
}

fn b_function(p: &mut Psx, n: u32) {
    match n {
        0x00 => {
            let size = arg(p, 0);
            let v = libc::alloc(p, kseg1(KMEM), kseg1(KMEM + KMEM_SIZE), size);
            ret(p, v);
        }
        0x01 => {
            let buf = arg(p, 0);
            libc::free(p, buf);
            ret(p, 0);
        }
        0x02..=0x06 => timer_function(p, n),
        0x07 => {
            let (class, spec) = (arg(p, 0), arg(p, 1));
            let ra = reg(p, RA);
            deliver_then(p, &[(class, spec)], ra);
        }
        0x08 => open_event(p),
        0x09 => {
            let a0_ = arg(p, 0);
            if let Some(e) = event_addr(p, a0_) {
                wr32(p, e + 4, EV_FREE);
            }
            ret(p, 1);
        }
        0x0A => wait_event(p),
        0x0B => {
            let a0_ = arg(p, 0);
            let r = match event_addr(p, a0_) {
                Some(e) if rd32(p, e + 4) == EV_READY => {
                    wr32(p, e + 4, EV_BUSY);
                    1
                }
                _ => 0,
            };
            ret(p, r);
        }
        0x0C | 0x0D => {
            let a0_ = arg(p, 0);
            if let Some(e) = event_addr(p, a0_) {
                if rd32(p, e + 4) != EV_FREE {
                    wr32(p, e + 4, if n == 0x0C { EV_BUSY } else { EV_DISABLED });
                }
            }
            ret(p, 1);
        }
        0x0E => open_thread(p),
        0x0F => {
            let a0_ = arg(p, 0);
            if let Some(t) = thread_addr(p, a0_) {
                wr32(p, t, 0x1000);
            }
            ret(p, 1);
        }
        0x10 => change_thread(p),
        0x12 => init_pad(p),
        0x13 | 0x4B => {
            start_pad_card(p);
            ret(p, 1);
        }
        0x14 | 0x4C => {
            dequeue(p, 2, element(E_PADCARD));
            wr32(p, var(V_PAD_STARTED), 0);
            ret(p, 1);
        }
        0x15 => pad_init2(p),
        0x16 => {
            let v = pad_dr(p);
            let dest = rd32(p, var(V_PAD_BUTTONS));
            wr32(p, dest, v);
            ret(p, v);
        }
        0x18 => {
            let d = var(V_DEFAULT_HOOK);
            wr32(p, var(V_HOOK), d);
            ret(p, d);
        }
        0x19 => {
            let a = arg(p, 0);
            wr32(p, var(V_HOOK), a);
            ret(p, 0);
        }
        0x1A..=0x1F | 0x21..=0x23 | 0x2A | 0x2B | 0x52 | 0x5A => {
            system_error(p, &format!("B({n:02X}h)"))
        }
        0x20 => {
            let (class, spec) = (arg(p, 0), arg(p, 1));
            for e in events(p) {
                if rd32(p, e + 4) == EV_READY
                    && rd32(p, e + 0xC) == MODE_READY
                    && rd32(p, e) == class
                    && rd32(p, e + 8) == spec
                {
                    wr32(p, e + 4, EV_BUSY);
                }
            }
            ret(p, 0);
        }
        0x32..=0x3B => a_function(p, n - 0x32),
        0x3C..=0x3F => a_function(p, n - 0x3C + 0x3B),
        0x40 => files::cd(p),
        0x41 => card::format(p),
        0x42 => files::firstfile(p),
        0x43 => files::nextfile(p),
        0x44 => card::rename(p),
        0x45 => card::erase(p, false),
        0x46 => card::erase(p, true),
        0x47 | 0x48 => ret(p, 0),
        0x49 => ret_void(p),
        0x4A => {
            let enable = arg(p, 0);
            wr32(p, var(V_PAD_ENABLE), enable);
            wr32(p, var(V_CARD_STARTED), 1);
            ret(p, 1);
        }
        0x4D => card::card_info(p),
        0x4E => card::card_io(p, true),
        0x4F => card::card_io(p, false),
        0x50 => {
            wr32(p, var(V_CARD_IGNORE_NEW), 1);
            ret(p, 0);
        }
        // No character set in this ROM.
        0x51 | 0x53 => ret(p, 0xFFFF_FFFF),
        0x54 => {
            let e = rd32(p, var(V_ERRNO));
            ret(p, e);
        }
        0x55 => files::get_error(p),
        0x56 => ret(p, kseg0(C_TABLE)),
        0x57 => ret(p, kseg0(B_TABLE)),
        0x58 => {
            let c = rd32(p, var(V_CARD_CHAN));
            ret(p, c);
        }
        0x59 => ret(p, 0),
        0x5B => {
            let f = arg(p, 0);
            let old = rd32(p, var(V_PAD_CLEAR));
            wr32(p, var(V_PAD_CLEAR), f);
            ret(p, old);
        }
        0x5C => card::card_status(p),
        0x5D => card::card_wait(p),
        _ => {
            log(p, &format!("B({n:02X}h) does not exist"));
            ret(p, 0);
        }
    }
}

fn c_function(p: &mut Psx, n: u32) {
    match n {
        0x00 => {
            let prio = arg(p, 0);
            enqueue_timers(p, prio);
            ret(p, 0);
        }
        0x01 => {
            let prio = arg(p, 0);
            enqueue(p, prio, element(E_SYSCALL));
            ret(p, 0);
        }
        0x02 => {
            let (prio, s) = (arg(p, 0), arg(p, 1));
            enqueue(p, prio, s);
            ret(p, 0);
        }
        0x03 => {
            let (prio, s) = (arg(p, 0), arg(p, 1));
            dequeue(p, prio, s);
            ret(p, 0);
        }
        0x04 => {
            let slot = events(p)
                .iter()
                .position(|&e| p.bus.load32(e + 4) == EV_FREE)
                .map_or(0xFFFF_FFFF, |i| i as u32);
            ret(p, slot);
        }
        0x05 => {
            let slot = threads(p)
                .iter()
                .position(|&t| p.bus.load32(t) != 0x4000)
                .map_or(0xFFFF_FFFF, |i| i as u32);
            ret(p, slot);
        }
        0x07 => {
            write_exception_vector(p);
            ret(p, 0);
        }
        0x08 => {
            let (addr, size) = (arg(p, 0), arg(p, 1));
            libc::init_heap_at(p, addr, size);
            ret(p, 0);
        }
        0x09 | 0x0E..=0x12 | 0x14..=0x18 | 0x1B => ret(p, 0),
        0x0A => {
            let (t, flag) = (arg(p, 0), arg(p, 1));
            if t < 4 {
                let old = rd32(p, var(V_RCNT_CLEAR + t * 4));
                wr32(p, var(V_RCNT_CLEAR + t * 4), flag);
                ret(p, old);
            } else {
                ret(p, 0);
            }
        }
        0x0B => system_error(p, "C(0Bh)"),
        0x0C => {
            let prio = arg(p, 0);
            enqueue(p, prio, element(E_DEFINT));
            ret(p, 0);
        }
        0x0D => {
            let (irq, flag) = (arg(p, 0), arg(p, 1));
            let mut m = rd32(p, var(V_AUTOACK));
            if irq < 11 {
                if flag != 0 {
                    m |= 1 << irq;
                } else {
                    m &= !(1 << irq);
                }
            }
            wr32(p, var(V_AUTOACK), m);
            ret(p, 0);
        }
        0x13 => ret(p, 0),
        0x19 => system_error(p, "_ioabort"),
        0x1A => {
            let m = arg(p, 0);
            wr32(p, var(V_CARD_FIND), m);
            ret(p, 0);
        }
        0x1C => ret(p, 0),
        0x1D => {
            let m = rd32(p, var(V_CARD_FIND));
            ret(p, m);
        }
        _ => {
            log(p, &format!("C({n:02X}h) does not exist"));
            ret(p, 0);
        }
    }
}

// ---- boot -----------------------------------------------------------------

/// SYSTEM.CNF's settings, psx-spx "CDROM File Playstation EXE and SYSTEM.CNF".
struct BootConf {
    exe: String,
    arg: Vec<u8>,
    tcbs: u32,
    events: u32,
    stack: u32,
}

impl BootConf {
    fn parse(text: &str) -> BootConf {
        let mut c = BootConf {
            exe: "cdrom:\\PSX.EXE;1".into(),
            arg: Vec::new(),
            tcbs: 4,
            events: 16,
            stack: 0x801F_FF00,
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let hex =
                |v: &str| u32::from_str_radix(v.split_whitespace().next().unwrap_or(""), 16).ok();
            match key.trim().to_ascii_uppercase().as_str() {
                "BOOT" => {
                    let mut parts = value.splitn(2, char::is_whitespace);
                    c.exe = parts.next().unwrap_or("").to_string();
                    c.arg = parts.next().unwrap_or("").trim().as_bytes().to_vec();
                    c.arg.truncate(0x7F);
                }
                "TCB" => c.tcbs = hex(value).unwrap_or(c.tcbs).clamp(1, 16),
                "EVENT" => c.events = hex(value).unwrap_or(c.events).clamp(1, 64),
                "STACK" => c.stack = hex(value).unwrap_or(c.stack),
                _ => {}
            }
        }
        c
    }
}

/// Everything the console does between reset and the game's first
/// instruction, minus the intro: set up the kernel, then load and run the
/// executable SYSTEM.CNF names.
fn boot(p: &mut Psx) {
    // Exceptions to 80000080h from now on, interrupts off until the game.
    p.cpu.cop0.sr = 0;
    p.bus.irq.set_mask(0);
    p.bus.irq.ack(0);
    init_hardware(p);

    let conf = {
        let text = match p.bus.cdrom.disc.as_mut() {
            Some(disc) => iso::find(disc, "SYSTEM.CNF;1")
                .filter(|e| !e.dir)
                .and_then(|e| iso::read(disc, e.lba, 0, e.size.min(2048))),
            None => None,
        };
        BootConf::parse(&String::from_utf8_lossy(&text.unwrap_or_default()))
    };

    // A sideloaded executable (a test program, a homebrew EXE) instead of
    // the disc's, on a kernel set up as for a SYSTEM.CNF with the defaults.
    if let Some(exe) = p.pending_exe.take() {
        init_kernel(p, 4, 16, 0x801F_FF00);
        set(p, SP, 0x801F_FF00);
        set(p, FP, 0x801F_FF00);
        p.apply_exe(&exe);
        set(p, A0, 1);
        set(p, A1, 0);
        set(p, RA, trap(T_EXE_RETURNED));
        p.pending_exe = Some(exe);
        p.exe_loaded = true;
        return;
    }

    init_kernel(p, conf.tcbs, conf.events, conf.stack);

    // The command line argument, and the boot file's name, which LoadExec
    // reloads when an executable it ran returns.
    for i in 0..0x80u32 {
        let c = conf.arg.get(i as usize).copied().unwrap_or(0);
        wr8(p, kseg1(0x180 + i), c);
    }
    let name = conf.exe.as_bytes().to_vec();
    put_string(p, var(V_BOOT_NAME), &name[..name.len().min(0x3F)]);

    let has_disc = p.bus.cdrom.disc.is_some();
    if !has_disc {
        log(p, "no disc: nothing to boot");
        jump(p, trap(T_HANG));
        return;
    }
    // As LoadExec with SYSTEM.CNF's stack, and the boot file never returns
    // anywhere but a halt.
    if !files::load_and_exec(p, &conf.exe, conf.stack, 0, trap(T_EXE_RETURNED)) {
        let msg = format!("cannot load {}", conf.exe);
        log(p, &msg);
        jump(p, trap(T_HANG));
    }
}

/// The I/O registers as a real BIOS leaves them at the game's first
/// instruction.
///
/// Measured, not documented: the SCPH-1001 BIOS (v2.2) run on this emulator
/// to Crash Bandicoot's entry point, and every register read there. What the
/// games need from it is mostly the interrupt mask: libcd's CdInit expects
/// the CD-ROM and DMA interrupts already on, and timed out without them.
fn init_hardware(p: &mut Psx) {
    let words: [(u32, u32); 12] = [
        (0x1F80_1000, 0x1F00_0000),
        (0x1F80_1004, 0x1F80_2000),
        (0x1F80_1008, 0x0013_243F),
        (0x1F80_100C, 0x0000_3022),
        (0x1F80_1010, 0x0013_243F),
        (0x1F80_1014, 0x2009_31E1),
        (0x1F80_1018, 0x0002_0943),
        (0x1F80_101C, 0x0007_0777),
        (0x1F80_1020, 0x0000_132C),
        (0x1F80_1060, 0x0000_0B88),
        // DMA: MDEC in and out, and CD-ROM on; GPU and CD-ROM interrupts.
        (0x1F80_10F0, 0x0000_9099),
        (0x1F80_10F4, 0x008C_0000),
    ];
    for (a, v) in words {
        wr32(p, a, v);
    }
    for t in 0..3 {
        wr16(p, 0x1F80_1104 + t * 0x10, 0);
        wr16(p, 0x1F80_1108 + t * 0x10, 0);
    }
    // The SPU on with CD audio, at the volumes the intro left.
    let spu: [(u32, u16); 8] = [
        (0x1F80_1D80, 0x3FFF),
        (0x1F80_1D82, 0x3FFF),
        (0x1F80_1D84, 0x5EBC),
        (0x1F80_1D86, 0x5EBC),
        (0x1F80_1DA2, 0xE128),
        (0x1F80_1DAC, 0x0004),
        (0x1F80_1DB0, 0),
        (0x1F80_1DAA, 0xC085),
    ];
    for (a, v) in spu {
        wr16(p, a, v);
    }
    // The CD-ROM controller with all five of its interrupts enabled, which
    // libcd takes for granted (its first command timed out without), and
    // the drive's status read, which clears the shell-open bit from power-on.
    wr8(p, 0x1F80_1800, 1);
    wr8(p, 0x1F80_1802, 0x1F);
    wr8(p, 0x1F80_1803, 0x1F);
    p.bus.cdrom.as_after_boot();
    p.bus.irq.set_mask(0x000C);
    // GTE usable, interrupts off: the game turns them on.
    p.cpu.cop0.sr = 0x4000_0000;
}

/// Kernel memory, tables, vectors and devices, as psx-spx lays them out.
fn init_kernel(p: &mut Psx, tcbs: u32, evcbs: u32, stack: u32) {
    for a in 0..0x1_0000u32 / 4 {
        wr32(p, kseg1(a * 4), 0);
    }
    write_exception_vector(p);
    // The garbage copy of the vector at 0, first word smashed, which
    // R-Types and Fade to Black read through null pointers.
    let v = rd32(p, kseg1(0x84));
    wr32(p, kseg1(0x0), 3);
    wr32(p, kseg1(0x4), v);
    let v = rd32(p, kseg1(0x88));
    wr32(p, kseg1(0x8), v);
    wr32(p, kseg1(0x60), 2);
    wr32(p, kseg1(0x64), 0);
    wr32(p, kseg1(0x68), 0xFF);
    write_call_vectors(p);
    write_code(p);
    write_tables(p);

    wr32(p, var(V_CONF), tcbs);
    wr32(p, var(V_CONF + 4), evcbs);
    wr32(p, var(V_CONF + 8), stack);
    wr32(p, var(V_RAND), 0x24040001);
    // Every handler acknowledges by itself. psx-spx does not say so; the
    // games do. Psy-Q's libetc, which takes interrupts through the exception
    // hook, turns off exactly the two that would take vblank from it,
    // ChangeClearPAD(0) and ChangeClearRCnt(3, 0), and leaves the three root
    // counters alone: Crash Bandicoot runs timer 2 through the kernel's
    // events, and libetc gives up on a timer 2 interrupt nobody clears
    // ("intr timeout(0040:004d)").
    for t in 0..4 {
        wr32(p, var(V_RCNT_CLEAR + t * 4), 1);
    }
    wr32(p, var(V_PAD_CLEAR), 1);
    wr32(p, var(V_PAD_ENABLE), 1);
    let exit = var(V_DEFAULT_HOOK);
    wr32(p, exit, kseg0(rfe_addr()));
    wr32(p, exit + 4, EXC_STACK_TOP - 4);
    wr32(p, var(V_HOOK), exit);
    put_string(p, var(V_CURDIR), b"");

    files::init_devices(p);
    card::reset(p);
    configure(p, tcbs, evcbs);
}

/// Allocate the control blocks and put the default handlers in their chains.
fn configure(p: &mut Psx, tcbs: u32, evcbs: u32) {
    libc::init_heap_at(p, kseg1(KMEM), KMEM_SIZE);
    let (lo, hi) = (kseg1(KMEM), kseg1(KMEM + KMEM_SIZE));
    let excb = libc::alloc(p, lo, hi, 4 * 8);
    let pcb = libc::alloc(p, lo, hi, 4);
    let tcb = libc::alloc(p, lo, hi, tcbs * TCB_SIZE);
    let evcb = libc::alloc(p, lo, hi, evcbs * EVCB_SIZE);
    for a in (excb..excb + 32).step_by(4) {
        wr32(p, a, 0);
    }
    for i in 0..tcbs * TCB_SIZE / 4 {
        wr32(p, tcb + i * 4, 0);
    }
    for i in 0..evcbs * EVCB_SIZE / 4 {
        wr32(p, evcb + i * 4, 0);
    }
    for i in 0..tcbs {
        wr32(p, tcb + i * TCB_SIZE, if i == 0 { 0x4000 } else { 0x1000 });
    }
    wr32(p, pcb, tcb);
    let table = [
        (0x100, excb, 32),
        (0x108, pcb, 4),
        (0x110, tcb, tcbs * TCB_SIZE),
        (0x120, evcb, evcbs * EVCB_SIZE),
    ];
    for (at, addr, size) in table {
        wr32(p, kseg1(at), addr);
        wr32(p, kseg1(at + 4), size);
    }

    // Chain elements: next, second function (the handler), first function
    // (the test), unused.
    let elements = [
        (E_CDROM_DMA, T_ZERO, T_ZERO),
        (E_CDROM_IO, T_ZERO, T_ZERO),
        (E_SYSCALL, T_SYSCALL_TEST, T_SYSCALL),
        (E_VBLANK, T_VBLANK_TEST, T_VBLANK),
        (E_TIMER2, T_TIMER_TEST + 2, T_TIMER + 2),
        (E_TIMER1, T_TIMER_TEST + 1, T_TIMER + 1),
        (E_TIMER0, T_TIMER_TEST, T_TIMER),
        (E_PADCARD, T_PADCARD_TEST, T_PADCARD),
        (E_DEFINT, T_DEFINT_TEST, T_DEFINT),
    ];
    for (n, test, handler) in elements {
        let e = element(n);
        wr32(p, e, 0);
        wr32(p, e + 4, trap(handler));
        wr32(p, e + 8, trap(test));
        wr32(p, e + 12, 0);
    }
    // Enqueued in reverse, since each goes to the front: psx-spx's order is
    // CdromDma, CdromIo, Syscall at 0; the root counters at 1; DefInt at 3.
    enqueue(p, 0, element(E_SYSCALL));
    enqueue(p, 0, element(E_CDROM_IO));
    enqueue(p, 0, element(E_CDROM_DMA));
    enqueue_timers(p, 1);
    enqueue(p, 3, element(E_DEFINT));

    // The kernel keeps five events for its own CD-ROM functions, so a game
    // gets its handles from the sixth, as on the console.
    let ev = rd32(p, kseg1(0x120));
    for (i, spec) in [0x10, 0x20, 0x40, 0x80, 0x8000].into_iter().enumerate() {
        let e = ev + i as u32 * EVCB_SIZE;
        if i as u32 >= evcbs {
            break;
        }
        wr32(p, e, CLASS_CDROM);
        wr32(p, e + 4, EV_BUSY);
        wr32(p, e + 8, spec);
        wr32(p, e + 0xC, MODE_READY);
        wr32(p, e + 0x10, 0);
    }
    wr32(p, var(V_PAD_STARTED), 0);
}

fn enqueue_timers(p: &mut Psx, prio: u32) {
    for e in [E_TIMER0, E_TIMER1, E_TIMER2, E_VBLANK] {
        enqueue(p, prio, element(e));
    }
}

fn write_exception_vector(p: &mut Psx) {
    let mut a = Asm::new(0x8000_0080);
    let (hi, lo) = hi_lo(EXC_HANDLER);
    a.lui(K0, hi);
    a.addiu(K0, K0, lo as i32);
    a.jr(K0);
    a.nop();
    for (i, w) in a.finish().into_iter().enumerate() {
        wr32(p, kseg1(0x80 + i as u32 * 4), w);
    }
}

/// The three call gates, and spare copies of them at 510h to 53Fh, where the
/// console keeps the copies init_a0_b0_c0_vectors works from.
fn write_call_vectors(p: &mut Psx) {
    for (i, gate) in [0xA0u32, 0xB0, 0xC0].into_iter().enumerate() {
        let target = DISPATCH[i];
        let mut a = Asm::new(gate);
        a.lui(T0, 0);
        a.addiu(T0, T0, target as i32);
        a.jr(T0);
        a.nop();
        let words = a.finish();
        write_words(p, gate, &words);
        write_words(p, 0x510 + i as u32 * 0x10, &words);
    }
}

fn write_words(p: &mut Psx, at: u32, words: &[u32]) {
    for (i, &w) in words.iter().enumerate() {
        wr32(p, kseg1(at + i as u32 * 4), w);
    }
}

fn save_rest_addr() -> u32 {
    CODE
}
fn walk_addr() -> u32 {
    CODE + 0x100
}
fn rfe_addr() -> u32 {
    CODE + 0x200
}
fn run_calls_addr() -> u32 {
    CODE + 0x300
}
fn get_conf_addr() -> u32 {
    CODE + 0x380
}
fn syscall_stub_addr() -> u32 {
    CODE + 0x3C0
}

/// C(06h), at 0C80h.
///
/// Laid out for the patches psx-spx documents:
///
/// * words 0 to 13 are the prologue elo2, Ridge Racer and Pandemonium II
///   write over it (from their table of the older kernel's missing
///   `mfc0 $v0, cop0r13`), so writing them changes nothing that matters;
/// * words 10 to 15 are what Metal Gear Solid checks before it writes its
///   own version of them, which is the same work reordered;
/// * words 28 and 29 load the early card handler's address, which Metal
///   Gear Solid and elo2 read to find it, and words 28 to 30 are what Breath
///   of Fire III and Ace Combat 2 overwrite with `nop` to uninstall it;
/// * words 32 to 35 are four free words that Sporting Clays and Dragon Quest
///   Monsters fill with a call to their lightgun routine, which then runs
///   here, with every register saved and a stack.
fn exception_handler() -> Vec<u32> {
    let mut a = Asm::new(EXC_HANDLER);
    a.nop();
    a.nop();
    a.addiu(K0, ZERO, 0x100);
    a.lw(K0, 8, K0);
    a.nop();
    a.lw(K0, 0, K0);
    a.nop();
    a.addi(K0, K0, 8);
    a.nop();
    a.nop();
    a.sw(AT, 4, K0);
    a.sw(V0, 8, K0);
    a.sw(V1, 0xC, K0);
    a.sw(RA, 0x7C, K0);
    a.mfc0(V1, C0_EPC);
    a.nop();
    a.jal(save_rest_addr());
    a.nop();
    a.pad_to(0x70);
    let (hi, lo) = hi_lo(EARLY_CARD);
    a.lui(T0, hi);
    a.addiu(T0, T0, lo as i32);
    a.jalr(T0);
    a.nop();
    a.nop();
    a.nop();
    a.nop();
    a.nop();
    a.j(walk_addr());
    a.nop();
    a.finish()
}

fn write_code(p: &mut Psx) {
    write_words(p, EXC_HANDLER, &exception_handler());

    // The early card handler: returns, with room behind it for the patch.
    let mut a = Asm::new(EARLY_CARD);
    a.jr(RA);
    a.nop();
    a.pad_to(0x100);
    write_words(p, EARLY_CARD, &a.finish());

    // A, B, C dispatch: look the number up in the table and jump, word for
    // word as the console does it, with no check on the number.
    for (n, table) in [A_TABLE, B_TABLE, C_TABLE].into_iter().enumerate() {
        let mut a = Asm::new(DISPATCH[n]);
        if n == 0 {
            a.addiu(T0, ZERO, table as i32);
        } else {
            a.lui(T0, 0);
            a.addiu(T0, T0, table as i32);
        }
        a.sll(T1, T1, 2);
        a.add(T0, T0, T1);
        a.lw(T0, 0, T0);
        a.nop();
        a.jr(T0);
        a.nop();
        write_words(p, DISPATCH[n], &a.finish());
    }

    // Save the rest of the registers into the current TCB. $k0 points at
    // its register file (TCB+8), and $at, $v0, $v1, $ra are already there.
    let mut a = Asm::new(save_rest_addr());
    for r in 4..26 {
        a.sw(r, r as i32 * 4, K0);
    }
    for r in 27..31 {
        a.sw(r, r as i32 * 4, K0);
    }
    a.mfhi(T0);
    a.sw(T0, 0x84, K0);
    a.mflo(T0);
    a.sw(T0, 0x88, K0);
    a.mfc0(T0, C0_SR);
    a.nop();
    a.sw(T0, 0x8C, K0);
    a.mfc0(T0, C0_CAUSE);
    a.nop();
    a.sw(T0, 0x90, K0);
    a.mfc0(T0, C0_EPC);
    a.nop();
    a.sw(T0, 0x80, K0);
    a.li(SP, EXC_STACK_TOP);
    a.jr(RA);
    a.nop();
    write_words(p, save_rest_addr(), &a.finish());

    // Walk the four priority chains, calling each element's test and, when
    // it answers non-zero, its handler. A handler that is done calls
    // ReturnFromException and never comes back here.
    let mut a = Asm::new(walk_addr());
    a.mov(S0, ZERO);
    a.addiu(S1, ZERO, 0x100);
    a.lw(S1, 0, S1);
    a.nop();
    a.label("prio");
    a.addu(T0, S1, S0);
    a.lw(S2, 0, T0);
    a.nop();
    a.label("elem");
    a.beq(S2, ZERO, "next_prio");
    a.nop();
    a.lw(T0, 8, S2);
    a.nop();
    a.beq(T0, ZERO, "skip");
    a.nop();
    a.jalr(T0);
    a.nop();
    a.beq(V0, ZERO, "skip");
    a.nop();
    a.lw(T0, 4, S2);
    a.nop();
    a.beq(T0, ZERO, "skip");
    a.nop();
    a.jalr(T0);
    a.mov(A0, V0);
    a.label("skip");
    a.lw(S2, 0, S2);
    a.b("elem");
    a.nop();
    a.label("next_prio");
    a.addiu(S0, S0, 8);
    a.addiu(T0, ZERO, 32);
    a.bne(S0, T0, "prio");
    a.nop();
    a.li(T0, trap(T_CHAINS_DONE));
    a.jr(T0);
    a.nop();
    write_words(p, walk_addr(), &a.finish());

    // ReturnFromException, B(17h): everything back from the current TCB but
    // $k0, which carries the return address, and RFE in the jump's slot.
    let mut a = Asm::new(rfe_addr());
    a.addiu(K0, ZERO, 0x108);
    a.lw(K0, 0, K0);
    a.nop();
    a.lw(K0, 0, K0);
    a.nop();
    a.addiu(K0, K0, 8);
    a.lw(T0, 0x84, K0);
    a.nop();
    a.mthi(T0);
    a.lw(T0, 0x88, K0);
    a.nop();
    a.mtlo(T0);
    a.lw(T0, 0x8C, K0);
    a.nop();
    a.mtc0(T0, C0_SR);
    for r in 1..26 {
        a.lw(r, r as i32 * 4, K0);
    }
    for r in 27..32 {
        a.lw(r, r as i32 * 4, K0);
    }
    a.lw(K0, 0x80, K0);
    a.nop();
    a.jr(K0);
    a.rfe();
    write_words(p, rfe_addr(), &a.finish());

    // Call a list of functions the host left on the stack: see `run_calls`.
    let mut a = Asm::new(run_calls_addr());
    a.label("loop");
    a.lw(T0, 0xC, S0);
    a.nop();
    a.beq(S1, T0, "done");
    a.sll(T1, S1, 2);
    a.addu(T1, T1, S0);
    a.lw(T1, 0x18, T1);
    a.addiu(S1, S1, 1);
    a.jalr(T1);
    a.nop();
    a.b("loop");
    a.nop();
    a.label("done");
    a.lw(RA, 0, S0);
    a.lw(S1, 8, S0);
    a.lw(SP, 0x10, S0);
    a.lw(S0, 4, S0);
    a.jr(RA);
    a.nop();
    write_words(p, run_calls_addr(), &a.finish());

    // GetConf, A(9Dh), starts by loading from boot_cnf_values + 8, because
    // Spec Ops finds that structure by decoding these two instructions.
    let mut a = Asm::new(kseg0(get_conf_addr()));
    let (hi, lo) = hi_lo(var(V_CONF + 8));
    a.lui(T0, hi);
    a.lw(T1, lo as u16 as i16 as i32, T0);
    a.li(T0, trap(FN_A + 0x9D));
    a.jr(T0);
    a.nop();
    write_words(p, get_conf_addr(), &a.finish());

    // ChangeTh's way in: SYS(03h), and back to the caller when this thread
    // is next switched to.
    let mut a = Asm::new(kseg0(syscall_stub_addr()));
    a.syscall();
    a.nop();
    a.jr(RA);
    a.nop();
    write_words(p, syscall_stub_addr(), &a.finish());

    // ChangeClearPAD, B(5Bh), and the 2000h bytes the pad patches reach
    // into. Three of those offsets are functions games call; the rest is
    // filler that is never run.
    let mut words = vec![0x2400_0001u32; 0x2000 / 4];
    let mut stub = |off: u32, id: u32| {
        let mut a = Asm::new(kseg0(PAD_FUNCS + off));
        a.li(T0, trap(id));
        a.jr(T0);
        a.nop();
        for (i, w) in a.finish().into_iter().enumerate() {
            words[off as usize / 4 + i] = w;
        }
    };
    stub(0, FN_B + 0x5B);
    stub(0x7A0, T_SET_PAD_OUTPUT);
    stub(0x884, T_PAD_ENABLE);
    stub(0x894, T_PAD_DISABLE);
    write_words(p, PAD_FUNCS, &words);
}

fn write_tables(p: &mut Psx) {
    for n in 0..A_COUNT {
        wr32(p, kseg1(A_TABLE + n * 4), trap(FN_A + n));
    }
    for n in 0..B_COUNT {
        wr32(p, kseg1(B_TABLE + n * 4), trap(FN_B + n));
    }
    for n in 0..C_COUNT {
        wr32(p, kseg1(C_TABLE + n * 4), trap(FN_C + n));
    }
    // The ones that are guest code.
    wr32(p, kseg1(A_TABLE + 0x9D * 4), kseg0(get_conf_addr()));
    wr32(p, kseg1(B_TABLE + 0x17 * 4), kseg0(rfe_addr()));
    wr32(p, kseg1(B_TABLE + 0x5B * 4), kseg0(PAD_FUNCS));
    wr32(p, kseg1(C_TABLE + 0x06 * 4), EXC_HANDLER);
    // C(80h) and up mirror B; the table stops at 7Fh, like the ROM's.
}

// ---- control blocks -------------------------------------------------------

fn current_tcb(p: &mut Psx) -> u32 {
    let pcb = rd32(p, kseg1(0x108));
    rd32(p, pcb)
}

fn events(p: &mut Psx) -> Vec<u32> {
    let base = rd32(p, kseg1(0x120));
    let n = rd32(p, kseg1(0x124)) / EVCB_SIZE;
    (0..n.min(256)).map(|i| base + i * EVCB_SIZE).collect()
}

fn threads(p: &mut Psx) -> Vec<u32> {
    let base = rd32(p, kseg1(0x110));
    let n = rd32(p, kseg1(0x114)) / TCB_SIZE;
    (0..n.min(64)).map(|i| base + i * TCB_SIZE).collect()
}

fn event_addr(p: &mut Psx, handle: u32) -> Option<u32> {
    let i = handle & 0xFFFF;
    events(p).get(i as usize).copied()
}

fn thread_addr(p: &mut Psx, handle: u32) -> Option<u32> {
    let i = handle & 0xFFFF;
    threads(p).get(i as usize).copied()
}

/// SysEnqIntRP: to the front of the chain.
fn enqueue(p: &mut Psx, prio: u32, s: u32) {
    let excb = rd32(p, kseg1(0x100)) + (prio & 3) * 8;
    // Already queued: leave it, rather than make a loop.
    let mut e = rd32(p, excb);
    let mut guard = 0;
    while e != 0 && guard < 64 {
        if e == s {
            return;
        }
        e = rd32(p, e);
        guard += 1;
    }
    let first = rd32(p, excb);
    wr32(p, s, first);
    wr32(p, excb, s);
}

/// SysDeqIntRP, which here can remove any element, not only the first.
fn dequeue(p: &mut Psx, prio: u32, s: u32) {
    let excb = rd32(p, kseg1(0x100)) + (prio & 3) * 8;
    let mut link = excb;
    let mut guard = 0;
    loop {
        let e = rd32(p, link);
        if e == 0 || guard > 64 {
            return;
        }
        if e == s {
            let next = rd32(p, e);
            wr32(p, link, next);
            return;
        }
        link = e;
        guard += 1;
    }
}

// ---- events -----------------------------------------------------------------

fn open_event(p: &mut Psx) {
    let (class, spec, mode, func) = (arg(p, 0), arg(p, 1), arg(p, 2), arg(p, 3));
    let evs = events(p);
    for (i, &e) in evs.iter().enumerate() {
        if rd32(p, e + 4) == EV_FREE {
            wr32(p, e, class);
            wr32(p, e + 4, EV_DISABLED);
            wr32(p, e + 8, spec);
            wr32(p, e + 0xC, mode);
            wr32(p, e + 0x10, func);
            ret(p, 0xF100_0000 | i as u32);
            return;
        }
    }
    ret(p, 0xFFFF_FFFF);
}

fn wait_event(p: &mut Psx) {
    let a0_ = arg(p, 0);
    let Some(e) = event_addr(p, a0_) else {
        ret(p, 0);
        return;
    };
    match rd32(p, e + 4) {
        EV_READY => {
            wr32(p, e + 4, EV_BUSY);
            ret(p, 1);
        }
        // Busy: stay on the stub, and ask again.
        EV_BUSY => {}
        _ => ret(p, 0),
    }
}

/// DeliverEvent for each (class, spec): events waiting to be marked ready
/// are, and callbacks are collected, in table order.
fn deliver(p: &mut Psx, class: u32, spec: u32, calls: &mut Vec<u32>) {
    for e in events(p) {
        if rd32(p, e + 4) != EV_BUSY || rd32(p, e) != class || rd32(p, e + 8) != spec {
            continue;
        }
        match rd32(p, e + 0xC) {
            MODE_READY => wr32(p, e + 4, EV_READY),
            MODE_CALLBACK => {
                let f = rd32(p, e + 0x10);
                if f != 0 {
                    calls.push(f);
                }
            }
            _ => {}
        }
    }
}

/// Deliver events, run any callbacks they carry, then continue at `then`.
fn deliver_then(p: &mut Psx, list: &[(u32, u32)], then: u32) {
    let mut calls = Vec::new();
    for &(class, spec) in list {
        deliver(p, class, spec, &mut calls);
    }
    run_calls(p, &calls, then);
}

/// Call `calls` in guest code, one after the other, then go to `then`.
///
/// The list goes on the guest stack in a frame the guest loop at
/// `run_calls_addr` walks, so it nests: a callback can deliver events that
/// have callbacks of their own. `$s0` and `$s1` are the loop's, and saved in
/// the frame.
fn run_calls(p: &mut Psx, calls: &[u32], then: u32) {
    if calls.is_empty() {
        jump(p, then);
        return;
    }
    let sp = reg(p, SP);
    let n = calls.len() as u32;
    let frame = (sp.wrapping_sub(0x18 + n * 4)) & !7;
    let (s0, s1) = (reg(p, S0), reg(p, S1));
    wr32(p, frame, then);
    wr32(p, frame + 4, s0);
    wr32(p, frame + 8, s1);
    wr32(p, frame + 0xC, n);
    wr32(p, frame + 0x10, sp);
    for (i, &f) in calls.iter().enumerate() {
        wr32(p, frame + 0x18 + i as u32 * 4, f);
    }
    set(p, S0, frame);
    set(p, S1, 0);
    set(p, SP, frame - 0x10);
    jump(p, kseg0(run_calls_addr()));
}

// ---- exceptions and interrupts -------------------------------------------

/// A chain element's test: is this interrupt both raised and enabled?
fn irq_test(p: &mut Psx, bits: u16) {
    let pending = p.bus.irq.stat() & p.bus.irq.mask() & bits;
    ret(p, u32::from(pending != 0));
}

fn ack(p: &mut Psx, bits: u16) {
    p.bus.irq.ack(!bits);
}

/// The root counter and vblank handlers: deliver, and when ChangeClearRCnt
/// says so, acknowledge and leave the exception at once.
fn timer_irq(p: &mut Psx, t: u32) {
    let bit = if t == 3 { 1 << 0 } else { 1 << (4 + t) };
    let clear = rd32(p, var(V_RCNT_CLEAR + t * 4)) != 0;
    let then = if clear {
        ack(p, bit);
        kseg0(rfe_addr())
    } else {
        reg(p, RA)
    };
    deliver_then(p, &[(0xF200_0000 | t, 0x0002)], then);
}

/// DefInt: an event per raised interrupt, acknowledged only where
/// SetIrqAutoAck asked.
fn default_irq(p: &mut Psx) {
    const CLASS: [u32; 11] = [1, 2, 3, 4, 5, 6, 6, 8, 0xB, 9, 0xA];
    let pending = p.bus.irq.stat() & p.bus.irq.mask();
    let auto = rd32(p, var(V_AUTOACK)) as u16;
    let mut list = Vec::new();
    for (irq, class) in CLASS.iter().enumerate() {
        if pending & (1 << irq) != 0 {
            list.push((0xF000_0000 | class, 0x1000));
        }
    }
    ack(p, pending & auto);
    let ra = reg(p, RA);
    deliver_then(p, &list, ra);
}

/// The end of the chains: out through the hook HookEntryInt set, by default
/// ReturnFromException.
///
/// An exception no element took (not an interrupt, not a syscall) delivers
/// F0000010h, 1000h, and calls A(40h), SystemErrorUnresolvedException,
/// through the A table, before going out through the hook. The A(40h) here
/// halts, as psx-spx says; but a program can put its own in the table, and
/// the CPU suite's cop test does exactly that to catch the
/// coprocessor-unusable exceptions it causes on purpose: its A(40h) steps
/// EPC past the fault and returns. On a real BIOS it passes that way, which
/// is how the order was found.
fn chains_done(p: &mut Psx) {
    let tcb = current_tcb(p);
    let cause = rd32(p, tcb + 0x98);
    let code = (cause >> 2) & 0x1F;
    let hook = rd32(p, var(V_HOOK));
    if code != 0 {
        deliver_then(p, &[(CLASS_EXCEPTION, 0x1000)], trap(T_UNRESOLVED));
        return;
    }
    if tracing() {
        let (st, mk) = (p.bus.irq.stat(), p.bus.irq.mask());
        let to = rd32(p, hook);
        eprintln!(
            "hle: exception done, istat {st:04X} imask {mk:04X}, hook {hook:08X} -> {to:08X}"
        );
    }
    longjmp_from(p, hook, 1);
}

fn syscall(p: &mut Psx) {
    let tcb = current_tcb(p);
    let epc = rd32(p, tcb + 0x88);
    wr32(p, tcb + 0x88, epc.wrapping_add(4));
    let a0 = rd32(p, tcb + 8 + 4 * 4);
    let sr = rd32(p, tcb + 0x94);
    let rfe = kseg0(rfe_addr());
    match a0 {
        0 => jump(p, rfe),
        1 => {
            wr32(p, tcb + 8 + 2 * 4, u32::from(sr & 0x404 == 0x404));
            wr32(p, tcb + 0x94, sr & !0x404);
            jump(p, rfe);
        }
        2 => {
            wr32(p, tcb + 0x94, sr | 0x404);
            jump(p, rfe);
        }
        3 => {
            let new = rd32(p, tcb + 8 + 5 * 4);
            wr32(p, tcb + 8 + 2 * 4, 1);
            let pcb = rd32(p, kseg1(0x108));
            wr32(p, pcb, new);
            jump(p, rfe);
        }
        _ => deliver_then(p, &[(CLASS_EXCEPTION, 0x4000)], rfe),
    }
}

// ---- threads -----------------------------------------------------------------

fn open_thread(p: &mut Psx) {
    let (pc, sp, gp) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let ts = threads(p);
    for (i, &t) in ts.iter().enumerate() {
        if rd32(p, t) != 0x4000 {
            wr32(p, t, 0x4000);
            wr32(p, t + 4, 0x1000);
            wr32(p, t + 0x88, pc);
            wr32(p, t + 8 + 28 * 4, gp);
            wr32(p, t + 8 + 29 * 4, sp);
            wr32(p, t + 8 + 30 * 4, sp);
            ret(p, 0xFF00_0000 | i as u32);
            return;
        }
    }
    ret(p, 0xFFFF_FFFF);
}

/// ChangeTh: through SYS(03h), so the registers go into the old TCB and
/// come out of the new one by the exception path, as the console does it.
fn change_thread(p: &mut Psx) {
    let a0_ = arg(p, 0);
    let Some(t) = thread_addr(p, a0_) else {
        ret(p, 0);
        return;
    };
    set(p, A0, 3);
    set(p, A1, t);
    jump(p, kseg0(syscall_stub_addr()));
}

// ---- setjmp -----------------------------------------------------------------

fn setjmp(p: &mut Psx) {
    let buf = arg(p, 0);
    let (ra, sp, fp, gp) = (reg(p, RA), reg(p, SP), reg(p, FP), reg(p, GP));
    wr32(p, buf, ra);
    wr32(p, buf + 4, sp);
    wr32(p, buf + 8, fp);
    for i in 0..8 {
        let v = reg(p, S0 + i);
        wr32(p, buf + 0xC + i * 4, v);
    }
    wr32(p, buf + 0x2C, gp);
    ret(p, 0);
}

fn longjmp(p: &mut Psx) {
    let (buf, v) = (arg(p, 0), arg(p, 1));
    longjmp_from(p, buf, v);
}

fn longjmp_from(p: &mut Psx, buf: u32, v: u32) {
    let ra = rd32(p, buf);
    let sp = rd32(p, buf + 4);
    let fp = rd32(p, buf + 8);
    for i in 0..8 {
        let s = rd32(p, buf + 0xC + i * 4);
        set(p, S0 + i, s);
    }
    let gp = rd32(p, buf + 0x2C);
    set(p, SP, sp);
    set(p, FP, fp);
    set(p, GP, gp);
    set(p, RA, ra);
    set(p, V0, v);
    jump(p, ra);
}

// ---- timers --------------------------------------------------------------

fn timer_function(p: &mut Psx, n: u32) {
    let t = arg(p, 0);
    let port = 0x1F80_1100 + t * 0x10;
    match n {
        0x02 => {
            let (reload, flags) = (arg(p, 1), arg(p, 2));
            if t > 2 {
                return ret(p, 0);
            }
            wr16(p, port + 4, 0);
            wr16(p, port + 8, reload as u16);
            let mut mode = if flags & 0x10 == 0 { 0x48 } else { 0x49 };
            if flags & 1 == 0 {
                mode |= 0x100;
            }
            if flags & 0x1000 != 0 {
                mode |= 0x10;
            }
            wr16(p, port + 4, mode);
            ret(p, 1);
        }
        0x03 => {
            if t > 2 {
                return ret(p, 0);
            }
            let v = rd16(p, port) as u32;
            ret(p, v);
        }
        0x04 | 0x05 => {
            let bit = match t {
                0..=2 => 1u16 << (4 + t),
                3 => 1,
                _ => 0,
            };
            let m = p.bus.irq.mask();
            p.bus
                .irq
                .set_mask(if n == 0x04 { m | bit } else { m & !bit });
            ret(p, u32::from(n == 0x05 || t <= 2));
        }
        _ => {
            if t > 2 {
                return ret(p, 0);
            }
            wr16(p, port, 0);
            ret(p, 1);
        }
    }
}

// ---- pads ---------------------------------------------------------------------

fn init_pad(p: &mut Psx) {
    let (b1, s1, b2, s2) = (arg(p, 0), arg(p, 1), arg(p, 2), arg(p, 3));
    wr32(p, var(V_PAD_BUF1), b1);
    wr32(p, var(V_PAD_SIZ1), s1);
    wr32(p, var(V_PAD_BUF2), b2);
    wr32(p, var(V_PAD_SIZ2), s2);
    for (b, s) in [(b1, s1), (b2, s2)] {
        for i in 0..s.min(0x22) {
            wr8(p, b + i, 0);
        }
    }
    wr32(p, var(V_PAD_ENABLE), 1);
    ret(p, 1);
}

/// StartPAD2 and StartCARD2: the pad and card handler on vblank.
fn start_pad_card(p: &mut Psx) {
    enqueue(p, 2, element(E_PADCARD));
    wr32(p, var(V_PAD_STARTED), 1);
    let m = p.bus.irq.mask();
    p.bus.irq.set_mask(m | 1);
}

fn pad_init2(p: &mut Psx) {
    let (ty, dest) = (arg(p, 0), arg(p, 1));
    if ty != 0x2000_0000 && ty != 0x2000_0001 {
        return ret(p, 0);
    }
    let b1 = var(V_PAD_INTERNAL);
    let b2 = b1 + 0x22;
    wr32(p, var(V_PAD_BUF1), b1);
    wr32(p, var(V_PAD_SIZ1), 0x22);
    wr32(p, var(V_PAD_BUF2), b2);
    wr32(p, var(V_PAD_SIZ2), 0x22);
    for i in 0..0x44 {
        wr8(p, b1 + i, 0);
    }
    wr32(p, var(V_PAD_ENABLE), 1);
    wr32(p, var(V_PAD_INIT2), 1);
    wr32(p, var(V_PAD_BUTTONS), dest);
    start_pad_card(p);
    ret(p, 2);
}

/// PAD_dr's value: each pad's two button bytes, byte-swapped, FFFFh for
/// anything that is not a digital pad.
fn pad_dr(p: &mut Psx) -> u32 {
    let mut out = 0;
    for (i, v) in [V_PAD_BUF1, V_PAD_BUF2].into_iter().enumerate() {
        let b = rd32(p, var(v));
        let (status, id) = (rd8(p, b), rd8(p, b + 1));
        let half = if status == 0 && id == 0x41 {
            (rd8(p, b + 2) as u32) << 8 | rd8(p, b + 3) as u32
        } else {
            0xFFFF
        };
        out |= half << (16 * i);
    }
    out
}

/// Read both pads the way the kernel's vblank handler does: 42h, and up to
/// 20h bytes after the ID, with what SetPadOutput asked to send.
fn read_pads(p: &mut Psx) {
    for port in 0..2usize {
        let buf = rd32(p, var(if port == 0 { V_PAD_BUF1 } else { V_PAD_BUF2 }));
        if buf == 0 {
            continue;
        }
        let out = rd32(p, var(if port == 0 { V_PAD_OUT1 } else { V_PAD_OUT2 }));
        let mut tx = [0u8; 0x20];
        if out != 0 {
            for (i, b) in tx.iter_mut().enumerate() {
                *b = rd8(p, out + i as u32);
            }
        }
        if !p.bus.sio.pads[port].connected {
            wr8(p, buf, 0xFF);
            continue;
        }
        let pad = &mut p.bus.sio.pads[port];
        let mut rx = Vec::with_capacity(0x22);
        pad.exchange(0, 0x01);
        let (id, _) = pad.exchange(1, 0x42);
        rx.push(id);
        let (b, mut more) = pad.exchange(2, 0);
        rx.push(b);
        let mut step = 3;
        while more && rx.len() < 0x21 {
            let (b, ack) = pad.exchange(step, tx[step as usize - 3]);
            rx.push(b);
            more = ack;
            step += 1;
        }
        wr8(p, buf, 0);
        for (i, &b) in rx.iter().enumerate() {
            wr8(p, buf + 1 + i as u32, b);
        }
    }
    if rd32(p, var(V_PAD_INIT2)) != 0 {
        let dest = rd32(p, var(V_PAD_BUTTONS));
        if dest != 0 {
            let v = pad_dr(p);
            wr32(p, dest, v);
        }
    }
}

/// The pad and card handler, on vblank.
fn pad_card_irq(p: &mut Psx) {
    if rd32(p, var(V_PAD_ENABLE)) != 0 {
        read_pads(p);
    }
    let mut list = card::run(p);
    let n = rd32(p, var(V_PENDING_COUNT)).min(PENDING_MAX);
    for i in 0..n {
        let class = rd32(p, var(V_PENDING + i * 8));
        let spec = rd32(p, var(V_PENDING + i * 8 + 4));
        list.push((class, spec));
    }
    wr32(p, var(V_PENDING_COUNT), 0);
    // Resident Evil 2 and Sporting Clays turn the acknowledge off by writing
    // nine nops over it, 62Ch into ChangeClearPAD.
    let patched_off = (0..9).all(|i| p.bus.load32(kseg1(PAD_FUNCS + 0x62C + i * 4)) == 0);
    let clear = rd32(p, var(V_PAD_CLEAR)) != 0 && !patched_off;
    let then = if clear {
        ack(p, 1);
        kseg0(rfe_addr())
    } else {
        reg(p, RA)
    };
    deliver_then(p, &list, then);
}

/// Queue an event for the next vblank's handler.
fn deliver_later(p: &mut Psx, class: u32, spec: u32) {
    let n = rd32(p, var(V_PENDING_COUNT));
    if n >= PENDING_MAX {
        return;
    }
    wr32(p, var(V_PENDING + n * 8), class);
    wr32(p, var(V_PENDING + n * 8 + 4), spec);
    wr32(p, var(V_PENDING_COUNT), n + 1);
}

// ---- GPU --------------------------------------------------------------------

fn gpu_sync(p: &mut Psx) {
    let stat = rd32(p, GP1);
    let dma = (stat >> 29) & 3 != 0;
    if dma && rd32(p, 0x1F80_10A8) & (1 << 24) != 0 {
        return;
    }
    if stat & (1 << 28) == 0 {
        return;
    }
    if dma {
        wr32(p, GP1, 0x0400_0000);
    }
    ret(p, 0);
}

fn gpu_dw(p: &mut Psx) {
    let (x, y, w, h, src) = (arg(p, 0), arg(p, 1), arg(p, 2), arg(p, 3), arg(p, 4));
    wr32(p, GP0, 0xA000_0000);
    wr32(p, GP0, (y & 0xFFFF) << 16 | (x & 0xFFFF));
    wr32(p, GP0, (h & 0xFFFF) << 16 | (w & 0xFFFF));
    let n = (w & 0xFFFF) * (h & 0xFFFF) / 2;
    for i in 0..n {
        let v = rd32(p, src + i * 4);
        wr32(p, GP0, v);
    }
    ret(p, src + n * 4);
}

fn gpu_send_dma(p: &mut Psx) {
    let (x, y, w, h, src) = (arg(p, 0), arg(p, 1), arg(p, 2), arg(p, 3), arg(p, 4));
    let n = (w & 0xFFFF) * (h & 0xFFFF) / 32;
    wr32(p, GP0, 0xA000_0000);
    wr32(p, GP0, (y & 0xFFFF) << 16 | (x & 0xFFFF));
    wr32(p, GP0, (h & 0xFFFF) << 16 | (w & 0xFFFF));
    wr32(p, GP1, 0x0400_0002);
    let dpcr = rd32(p, 0x1F80_10F0);
    wr32(p, 0x1F80_10F0, dpcr | 0x800);
    wr32(p, 0x1F80_10A0, src);
    wr32(p, 0x1F80_10A4, n << 16 | 0x10);
    wr32(p, 0x1F80_10A8, 0x0100_0201);
    ret(p, GP0);
}

// ---- configuration and system info ------------------------------------------

fn get_conf(p: &mut Psx) {
    let (ev, tcb, stack) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let (nt, ne, st) = (
        rd32(p, var(V_CONF)),
        rd32(p, var(V_CONF + 4)),
        rd32(p, var(V_CONF + 8)),
    );
    wr32(p, ev, ne);
    wr32(p, tcb, nt);
    wr32(p, stack, st);
    ret_void(p);
}

/// SetConf: new control blocks, the current thread carried over as thread 0.
fn set_conf(p: &mut Psx) {
    let (ne, nt, stack) = (arg(p, 0).clamp(1, 64), arg(p, 1).clamp(1, 16), arg(p, 2));
    let old = current_tcb(p);
    let mut saved = [0u32; (TCB_SIZE / 4) as usize];
    for (i, w) in saved.iter_mut().enumerate() {
        *w = rd32(p, old + i as u32 * 4);
    }
    wr32(p, var(V_CONF), nt);
    wr32(p, var(V_CONF + 4), ne);
    wr32(p, var(V_CONF + 8), stack);
    configure(p, nt, ne);
    let tcb = rd32(p, kseg1(0x110));
    for (i, &w) in saved.iter().enumerate() {
        wr32(p, tcb + i as u32 * 4, w);
    }
    wr32(p, tcb, 0x4000);
    ret(p, 0);
}

fn get_system_info(p: &mut Psx) {
    let v = match arg(p, 0) {
        0x00 => 0x1995_1204,
        0x01 => 3,
        0x02 => 0xBFC0_0000 + SIGNATURE_AT as u32,
        0x05 => rd32(p, kseg1(0x60)) << 10,
        0x07 => 0x400,
        0x09 => 0x200,
        0x0C..=0x0E => 1,
        _ => 0,
    };
    ret(p, v);
}

#[cfg(test)]
mod tests;
