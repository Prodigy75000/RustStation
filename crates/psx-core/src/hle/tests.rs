// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The kernel, driven the way a game drives it: a program assembled here,
//! sideloaded onto the HLE ROM, calling A, B and C functions through the
//! gates at A0h, B0h and C0h, and leaving its results in RAM.

use super::asm::*;
use super::*;
use crate::exe::Exe;

/// Where test programs load, and where they write their results.
const ORG: u32 = 0x8001_0000;
const OUT: u32 = 0x8003_0000;

/// A test program's builder, with the calling sequence for kernel functions.
struct Prog {
    a: Asm,
}

impl Prog {
    fn new() -> Prog {
        Prog { a: Asm::new(ORG) }
    }

    /// Call A, B or C function `n` with the arguments already in $a0..$a3.
    fn call(&mut self, gate: u32, n: u32) {
        self.a.addiu(T2, ZERO, gate as i32);
        self.a.jalr(T2);
        self.a.addiu(T1, ZERO, n as i32);
    }
    fn a(&mut self, n: u32) {
        self.call(0xA0, n);
    }
    fn b(&mut self, n: u32) {
        self.call(0xB0, n);
    }
    fn c(&mut self, n: u32) {
        self.call(0xC0, n);
    }
    /// $v0 to result word `slot`.
    fn keep(&mut self, slot: u32) {
        self.a.li(T0, OUT + slot * 4);
        self.a.sw(V0, 0, T0);
    }
    fn args(&mut self, args: &[u32]) {
        for (i, &v) in args.iter().enumerate() {
            self.a.li(A0 + i as u32, v);
        }
    }
    /// The end: a loop forever, with a marker that says it got here.
    fn halt(mut self) -> Vec<u32> {
        self.a.li(T0, 0x600D);
        self.a.li(T1, OUT + 0xFC);
        self.a.sw(T0, 0, T1);
        self.a.label("halt");
        self.a.b("halt");
        self.a.nop();
        self.a.finish()
    }
}

fn boot(code: &[u32], cycles: u64) -> Psx {
    let text: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let exe = Exe {
        initial_pc: ORG,
        initial_gp: 0,
        dest: ORG,
        text,
        memfill_start: 0,
        memfill_size: 0,
        sp_base: 0x801F_FF00,
        sp_offset: 0,
    };
    let mut psx = Psx::new(rom()).expect("the HLE ROM is a BIOS-sized image");
    psx.bus.sio.cards[0].connected = true;
    psx.sideload_exe(exe);
    psx.run(cycles);
    assert_eq!(
        result(&mut psx, 0x3F),
        0x600D,
        "the program did not finish:\n{}",
        psx.take_tty()
    );
    psx
}

fn result(psx: &mut Psx, slot: u32) -> u32 {
    psx.bus.load32(OUT + slot * 4)
}

#[test]
fn the_rom_is_recognised_and_every_stub_loops_on_itself() {
    let r = rom();
    assert!(is_hle(&r));
    assert!(!is_hle(&vec![0u8; crate::bus::BIOS_SIZE]));
    for id in [0, FN_B + 0x17, T_BOOT, TRAP_COUNT - 1] {
        let off = (trap(id) - 0xBFC0_0000) as usize;
        let w = u32::from_le_bytes(r[off..off + 4].try_into().unwrap());
        // j to its own address.
        assert_eq!(
            w,
            0x0800_0000 | (trap(id) >> 2 & 0x03FF_FFFF),
            "stub {id:#X}"
        );
    }
}

#[test]
fn an_event_is_ready_once_delivered_and_testing_it_consumes_that() {
    let mut p = Prog::new();
    p.args(&[0xF300_0001, 4, MODE_READY, 0]);
    p.b(0x08);
    p.a.mov(S0, V0);
    p.keep(0);
    p.a.mov(A0, S0);
    p.b(0x0C);
    // Not delivered yet.
    p.a.mov(A0, S0);
    p.b(0x0B);
    p.keep(1);
    p.args(&[0xF300_0001, 4]);
    p.b(0x07);
    p.a.mov(A0, S0);
    p.b(0x0B);
    p.keep(2);
    p.a.mov(A0, S0);
    p.b(0x0B);
    p.keep(3);
    // A spec that does not match marks nothing.
    p.args(&[0xF300_0001, 8]);
    p.b(0x07);
    p.a.mov(A0, S0);
    p.b(0x0B);
    p.keep(4);
    let mut psx = boot(&p.halt(), 200_000);
    // The kernel keeps five events for itself, so the game's first is 5.
    assert_eq!(result(&mut psx, 0), 0xF100_0005);
    assert_eq!([1, 2, 3, 4].map(|s| result(&mut psx, s)), [0, 1, 0, 0]);
}

#[test]
fn a_callback_event_runs_its_function_each_delivery_and_is_never_ready() {
    let mut p = Prog::new();
    p.a.b("main");
    p.a.nop();
    // The callback: count calls in result word 10.
    p.a.label("callback");
    p.a.li(T0, OUT + 40);
    p.a.lw(T1, 0, T0);
    p.a.nop();
    p.a.addiu(T1, T1, 1);
    p.a.jr(RA);
    p.a.sw(T1, 0, T0);
    p.a.label("main");
    let callback = p.a.addr_of("callback");
    p.args(&[0xF300_0002, 0x20, MODE_CALLBACK, callback]);
    p.b(0x08);
    p.a.mov(S0, V0);
    p.a.mov(A0, S0);
    p.b(0x0C);
    for _ in 0..3 {
        p.args(&[0xF300_0002, 0x20]);
        p.b(0x07);
    }
    p.a.mov(A0, S0);
    p.b(0x0B);
    p.keep(1);
    // $s0 survives the callbacks, as the calling convention says it must.
    p.a.mov(V0, S0);
    p.keep(2);
    let mut psx = boot(&p.halt(), 200_000);
    assert_eq!(result(&mut psx, 10), 3);
    assert_eq!(result(&mut psx, 1), 0);
    assert_eq!(result(&mut psx, 2), 0xF100_0005);
}

#[test]
fn the_heap_hands_back_a_freed_block_and_refuses_what_it_does_not_have() {
    let mut p = Prog::new();
    p.args(&[0x8010_0000, 0x100]);
    p.a(0x39);
    p.args(&[0x10]);
    p.a(0x33);
    p.a.mov(S0, V0);
    p.keep(0);
    p.args(&[0x10]);
    p.a(0x33);
    p.keep(1);
    p.a.mov(A0, S0);
    p.a(0x34);
    p.args(&[0x8]);
    p.a(0x33);
    p.keep(2);
    p.args(&[0x1000]);
    p.a(0x33);
    p.keep(3);
    let mut psx = boot(&p.halt(), 200_000);
    let (a, b, c, big) = (
        result(&mut psx, 0),
        result(&mut psx, 1),
        result(&mut psx, 2),
        result(&mut psx, 3),
    );
    assert_eq!(a, 0x8010_0004, "the first block is just past its header");
    assert_eq!(b, a + 0x14, "the second after the first and its own header");
    assert_eq!(c, a, "the freed block is reused");
    assert_eq!(big, 0, "more than the heap holds");
}

#[test]
fn memcmp_and_strstr_keep_the_bugs_psx_spx_documents() {
    let mut p = Prog::new();
    let data = 0x8004_0000u32;
    // "abcz" and "abex": the first mismatch is c/e, and the kernel's memcmp
    // answers with the bytes after it, z - x, which is 2 where c - e is -2.
    p.args(&[data, data + 8, 4]);
    p.a(0x2D);
    p.keep(0);
    // strstr("aaab", "aab") finds nothing, as psx-spx's example says.
    p.args(&[data + 16, data + 24]);
    p.a(0x24);
    p.keep(1);
    // strstr("xaab", "aab") does.
    p.args(&[data + 32, data + 24]);
    p.a(0x24);
    p.keep(2);
    p.args(&[data + 16]);
    p.a(0x1B);
    p.keep(3);
    let code = p.halt();
    let text: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut psx = Psx::new(rom()).unwrap();
    for (at, s) in [
        (0, &b"abcz"[..]),
        (8, b"abex"),
        (16, b"aaab\0"),
        (24, b"aab\0"),
        (32, b"xaab\0"),
    ] {
        let o = (data & 0x1F_FFFF) as usize + at;
        psx.bus.ram[o..o + s.len()].copy_from_slice(s);
    }
    psx.sideload_exe(Exe {
        initial_pc: ORG,
        initial_gp: 0,
        dest: ORG,
        text,
        memfill_start: 0,
        memfill_size: 0,
        sp_base: 0x801F_FF00,
        sp_offset: 0,
    });
    psx.run(200_000);
    assert_eq!(result(&mut psx, 0x3F), 0x600D);
    assert_eq!(result(&mut psx, 0) as i32, 2);
    assert_eq!(result(&mut psx, 1), 0);
    assert_eq!(result(&mut psx, 2), data + 33);
    assert_eq!(result(&mut psx, 3), 4);
}

#[test]
fn setjmp_returns_twice_and_longjmp_brings_back_the_saved_registers() {
    let mut p = Prog::new();
    let buf = 0x8004_0000;
    p.a.li(S0, 0x1234);
    p.args(&[buf]);
    p.a(0x13);
    // First time here with 0, second with 7.
    p.a.bne(V0, ZERO, "second");
    p.a.nop();
    p.keep(0);
    p.a.li(S0, 0x9999);
    p.args(&[buf, 7]);
    p.a(0x14);
    p.a.label("second");
    p.keep(1);
    p.a.mov(V0, S0);
    p.keep(2);
    let mut psx = boot(&p.halt(), 200_000);
    assert_eq!(result(&mut psx, 0), 0);
    assert_eq!(result(&mut psx, 1), 7);
    assert_eq!(result(&mut psx, 2), 0x1234);
}

/// The whole interrupt path: a vblank raises the exception, the handler at
/// C(06h) saves every register into the current thread, the chain walk calls
/// a handler the program put in with SysEnqIntRP, and that handler's
/// ReturnFromException puts every register back. Meanwhile the main loop
/// counts with registers the handler clobbers.
#[test]
fn an_interrupt_reaches_a_queued_handler_and_the_interrupted_code_never_notices() {
    let mut p = Prog::new();
    let element = 0x8004_0000;
    p.a.b("main");
    p.a.nop();
    // Test: is vblank pending?
    p.a.label("test");
    p.a.li(T0, 0x1F80_1070);
    p.a.lw(V0, 0, T0);
    p.a.nop();
    p.a.jr(RA);
    p.a.andi(V0, V0, 1);
    // Handler: count, clobber, acknowledge, return from the exception.
    p.a.label("handler");
    p.a.li(T0, OUT + 40);
    p.a.lw(T1, 0, T0);
    p.a.nop();
    p.a.addiu(T1, T1, 1);
    p.a.sw(T1, 0, T0);
    for r in [S0, S1, S2, A0, A1, V1, AT, GP, FP] {
        p.a.li(r, 0xDEAD_0000 | r);
    }
    p.a.li(T0, 0x1F80_1070);
    p.a.addiu(T1, ZERO, -2);
    p.a.sw(T1, 0, T0);
    p.b(0x17);
    p.a.label("main");
    let (test, handler) = (p.a.addr_of("test"), p.a.addr_of("handler"));
    p.a.li(T0, element);
    p.a.li(T1, handler);
    p.a.sw(T1, 4, T0);
    p.a.li(T1, test);
    p.a.sw(T1, 8, T0);
    p.args(&[0, element]);
    p.c(0x02);
    // Vblank on, then interrupts on: ExitCriticalSection.
    p.a.li(T0, 0x1F80_1074);
    p.a.addiu(T1, ZERO, 1);
    p.a.sw(T1, 0, T0);
    p.a.addiu(A0, ZERO, 2);
    p.a.syscall();
    // Count to a million in registers the handler clobbers, checking the
    // others each time round.
    p.a.li(S0, 0);
    p.a.li(S1, 1_000_000);
    p.a.li(S2, 0x5A5A_5A5A);
    p.a.li(GP, 0x1357_9BDF);
    p.a.label("loop");
    p.a.addiu(S0, S0, 1);
    p.a.bne(S0, S1, "loop");
    p.a.nop();
    p.a.mov(V0, S0);
    p.keep(0);
    p.a.mov(V0, S2);
    p.keep(1);
    p.a.mov(V0, GP);
    p.keep(2);
    p.a.li(T0, OUT + 40);
    p.a.lw(V0, 0, T0);
    p.a.nop();
    p.keep(3);
    let mut psx = boot(&p.halt(), 4_000_000);
    assert_eq!(result(&mut psx, 0), 1_000_000);
    assert_eq!(result(&mut psx, 1), 0x5A5A_5A5A);
    assert_eq!(result(&mut psx, 2), 0x1357_9BDF);
    // A million loops of three instructions is five and a quarter frames of
    // 571 212 cycles, plus what the handler costs.
    let n = result(&mut psx, 3);
    assert!((5..=6).contains(&n), "{n} vblanks handled");
}

#[test]
fn change_thread_runs_another_thread_and_comes_back_with_one() {
    let mut p = Prog::new();
    p.a.b("main");
    p.a.nop();
    // The other thread: mark, then switch back to thread 0.
    p.a.label("other");
    p.a.li(T0, OUT + 40);
    p.a.li(T1, 0x7777);
    p.a.sw(T1, 0, T0);
    p.args(&[0xFF00_0000]);
    p.b(0x10);
    p.a.label("stuck");
    p.a.b("stuck");
    p.a.nop();
    p.a.label("main");
    let other = p.a.addr_of("other");
    p.args(&[other, 0x8018_0000, 0]);
    p.b(0x0E);
    p.keep(0);
    p.a.li(S0, 0x4242);
    p.a.mov(A0, V0);
    p.b(0x10);
    p.keep(1);
    p.a.mov(V0, S0);
    p.keep(2);
    let mut psx = boot(&p.halt(), 200_000);
    assert_eq!(result(&mut psx, 0), 0xFF00_0001);
    assert_eq!(result(&mut psx, 10), 0x7777, "the other thread ran");
    assert_eq!(
        result(&mut psx, 1),
        1,
        "ChangeTh returns 1 to the thread it left"
    );
    assert_eq!(
        result(&mut psx, 2),
        0x4242,
        "and that thread's registers are its own"
    );
}

#[test]
fn a_memory_card_file_is_created_written_read_back_and_found() {
    let mut p = Prog::new();
    let name = 0x8004_0000u32;
    let pattern = 0x8004_0020u32;
    let src = 0x8004_0100u32;
    let dst = 0x8004_0200u32;
    let dir = 0x8004_0300u32;
    // open("bu00:BASLUS-00000TEST", create, one block)
    p.args(&[name, 0x0001_0202]);
    p.a(0x00);
    p.a.mov(S0, V0);
    p.keep(0);
    p.args(&[0, src, 0x80]);
    p.a.mov(A0, S0);
    p.a(0x03);
    p.keep(1);
    p.a.mov(A0, S0);
    p.a(0x04);
    p.args(&[name, 1]);
    p.a(0x00);
    p.a.mov(S0, V0);
    p.args(&[0, dst, 0x80]);
    p.a.mov(A0, S0);
    p.a(0x02);
    p.keep(2);
    p.a.mov(A0, S0);
    p.a(0x04);
    p.args(&[pattern, dir]);
    p.b(0x42);
    p.keep(3);
    // A second create of the same name fails.
    p.args(&[name, 0x0001_0202]);
    p.a(0x00);
    p.keep(4);
    let code = p.halt();
    let text: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut psx = Psx::new(rom()).unwrap();
    psx.bus.sio.cards[0].connected = true;
    let put = |psx: &mut Psx, at: u32, s: &[u8]| {
        let o = (at & 0x1F_FFFF) as usize;
        psx.bus.ram[o..o + s.len()].copy_from_slice(s);
    };
    put(&mut psx, name, b"bu00:BASLUS-00000TEST\0");
    put(&mut psx, pattern, b"bu00:BASLUS*\0");
    let data: Vec<u8> = (0..0x80u8).map(|i| i.wrapping_mul(7)).collect();
    put(&mut psx, src, &data);
    psx.sideload_exe(Exe {
        initial_pc: ORG,
        initial_gp: 0,
        dest: ORG,
        text,
        memfill_start: 0,
        memfill_size: 0,
        sp_base: 0x801F_FF00,
        sp_offset: 0,
    });
    psx.run(400_000);
    assert_eq!(result(&mut psx, 0x3F), 0x600D, "{}", psx.take_tty());
    assert_eq!(
        result(&mut psx, 0),
        2,
        "the first free handle after std in and out"
    );
    assert_eq!(result(&mut psx, 1), 0x80);
    assert_eq!(result(&mut psx, 2), 0x80);
    let o = (dst & 0x1F_FFFF) as usize;
    assert_eq!(&psx.bus.ram[o..o + 0x80], &data[..]);
    assert_eq!(result(&mut psx, 3), dir);
    let d = (dir & 0x1F_FFFF) as usize;
    assert_eq!(&psx.bus.ram[d..d + 16], b"BASLUS-00000TEST");
    assert_eq!(result(&mut psx, 4), 0xFFFF_FFFF);
    // And on the card: block 1 in use, its data in the block.
    let card = &psx.bus.sio.cards[0];
    assert_eq!(card.data[0x80], 0x51);
    assert_eq!(&card.data[0x2000..0x2080], &data[..]);
    assert!(card.written);
}

/// The call gates and the A, B and C dispatchers are the words a real kernel
/// leaves at the same addresses (the SCPH-1001 BIOS, read at a game's entry
/// point), so the B table sits at 874h and the C table at 674h, and 500h to
/// 53Fh is spare, as on the console: Tony Hawk's Pro Skater 2 writes a 1 to
/// 520h through a null pointer.
#[test]
fn the_call_gates_and_dispatchers_are_the_consoles() {
    let mut psx = boot(&Prog::new().halt(), 100_000);
    let words = |psx: &mut Psx, at: u32, n: u32| {
        (0..n)
            .map(|i| psx.bus.load32(0x8000_0000 + at + i * 4))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        words(&mut psx, 0xA0, 12),
        [
            0x3C08_0000,
            0x2508_05C4,
            0x0100_0008,
            0,
            0x3C08_0000,
            0x2508_05E0,
            0x0100_0008,
            0,
            0x3C08_0000,
            0x2508_0600,
            0x0100_0008,
            0,
        ]
    );
    assert_eq!(
        words(&mut psx, 0x5C4, 7),
        [
            0x2408_0200,
            0x0009_4880,
            0x0109_4020,
            0x8D08_0000,
            0,
            0x0100_0008,
            0
        ]
    );
    assert_eq!(
        words(&mut psx, 0x5E0, 8),
        [
            0x3C08_0000,
            0x2508_0874,
            0x0009_4880,
            0x0109_4020,
            0x8D08_0000,
            0,
            0x0100_0008,
            0
        ]
    );
    assert_eq!(
        words(&mut psx, 0x600, 8),
        [
            0x3C08_0000,
            0x2508_0674,
            0x0009_4880,
            0x0109_4020,
            0x8D08_0000,
            0,
            0x0100_0008,
            0
        ]
    );
}

/// InitHeap only notes where the heap is. Grand Theft Auto 2 hands it memory
/// it keeps using, and crashed on a block header written there at once.
#[test]
fn init_heap_writes_nothing_until_the_first_malloc() {
    let mut p = Prog::new();
    let heap = 0x8010_0000;
    p.a.li(T0, heap);
    p.a.li(T1, 0x1234_5678);
    p.a.sw(T1, 0, T0);
    p.args(&[heap, 0x1000]);
    p.a(0x39);
    p.a.li(T0, heap);
    p.a.lw(V0, 0, T0);
    p.a.nop();
    p.keep(0);
    p.args(&[0x10]);
    p.a(0x33);
    p.keep(1);
    let mut psx = boot(&p.halt(), 200_000);
    assert_eq!(result(&mut psx, 0), 0x1234_5678);
    assert_eq!(result(&mut psx, 1), heap + 4);
}

/// An exception nothing handles goes through A(40h) in the A table. The CPU
/// suite's cop test puts its own function there, which steps past the fault;
/// this does the same with a BREAK and counts.
#[test]
fn an_unhandled_exception_calls_whatever_the_a_table_holds_for_40h() {
    let mut p = Prog::new();
    p.a.b("main");
    p.a.nop();
    p.a.label("a40");
    // EPC += 4 in the current TCB, and count.
    p.a.addiu(T0, ZERO, 0x108);
    p.a.lw(T0, 0, T0);
    p.a.nop();
    p.a.lw(T0, 0, T0);
    p.a.nop();
    p.a.lw(T1, 0x88, T0);
    p.a.nop();
    p.a.addiu(T1, T1, 4);
    p.a.sw(T1, 0x88, T0);
    p.a.li(T0, OUT + 40);
    p.a.lw(T1, 0, T0);
    p.a.nop();
    p.a.addiu(T1, T1, 1);
    p.a.jr(RA);
    p.a.sw(T1, 0, T0);
    p.a.label("main");
    let a40 = p.a.addr_of("a40");
    p.a.li(T0, a40);
    p.a.sw(T0, 0x300, ZERO);
    p.a.li(S0, 0x0BAD);
    p.a.word(0x0000_000D);
    p.a.word(0x0000_000D);
    p.a.mov(V0, S0);
    p.keep(0);
    let mut psx = boot(&p.halt(), 200_000);
    assert_eq!(result(&mut psx, 10), 2, "both breaks reached A(40h)");
    assert_eq!(
        result(&mut psx, 0),
        0x0BAD,
        "and came back with the registers intact"
    );
}

/// InitPAD2's buffers, as psx-spx lays them out: status, the pad's ID, then
/// its data, filled on vblank. The 5Ah the pad sends after its ID is not
/// data; kept, it put every button a byte late, and Crash Bandicoot took no
/// input at all.
#[test]
fn the_pad_buffer_is_status_id_then_buttons() {
    let mut p = Prog::new();
    let (b1, b2) = (0x8004_0000u32, 0x8004_0040u32);
    p.args(&[b1, 0x22, b2, 0x22]);
    p.b(0x12);
    p.b(0x13);
    p.a.addiu(A0, ZERO, 2);
    p.a.syscall();
    // Two frames and a bit.
    p.a.li(S0, 700_000);
    p.a.label("wait");
    p.a.addiu(S0, S0, -1);
    p.a.bne(S0, ZERO, "wait");
    p.a.nop();
    let code = p.halt();
    let text: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut psx = Psx::new(rom()).unwrap();
    psx.bus.sio.pads[0].connected = true;
    psx.bus.sio.pads[0].buttons = 1 << crate::sio::button::DOWN | 1 << crate::sio::button::CROSS;
    psx.sideload_exe(Exe {
        initial_pc: ORG,
        initial_gp: 0,
        dest: ORG,
        text,
        memfill_start: 0,
        memfill_size: 0,
        sp_base: 0x801F_FF00,
        sp_offset: 0,
    });
    psx.run(3_000_000);
    assert_eq!(result(&mut psx, 0x3F), 0x600D);
    let o = (b1 & 0x1F_FFFF) as usize;
    // Held buttons read as 0: Down is bit 6 of the first byte, Cross bit 6
    // of the second.
    assert_eq!(&psx.bus.ram[o..o + 4], &[0x00, 0x41, 0xBF, 0xBF]);
    let o2 = (b2 & 0x1F_FFFF) as usize;
    assert_eq!(psx.bus.ram[o2], 0xFF, "no pad in port 2");
}
