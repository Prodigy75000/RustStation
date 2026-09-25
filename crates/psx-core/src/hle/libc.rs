// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The kernel's C library: strings, memory, numbers, printf and the heap.
//!
//! psx-spx documents several of these as buggy, and says how. Where a game
//! could have come to depend on the bug (memcmp comparing the byte after the
//! mismatch, strstr not backing up, strpbrk's return), it is reproduced.

use super::*;

pub(super) fn call(p: &mut Psx, n: u32) {
    match n {
        0x0A => {
            let c = arg(p, 0) & 0xFF;
            let v = match c as u8 {
                b'0'..=b'9' => c - b'0' as u32,
                b'A'..=b'Z' => c - b'A' as u32 + 10,
                b'a'..=b'z' => c - b'a' as u32 + 10,
                _ => 9_999_999,
            };
            ret(p, v);
        }
        0x0C | 0x0D => {
            let (src, end, base) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if src == 0 {
                return ret(p, 0);
            }
            let (v, stop) = strtol(p, src, base, n == 0x0D, false);
            if end != 0 {
                wr32(p, end, stop);
            }
            ret(p, v);
        }
        0x0E | 0x0F => {
            let v = arg(p, 0) as i32;
            ret(p, v.wrapping_abs() as u32);
        }
        0x10 | 0x11 => {
            let src = arg(p, 0);
            if src == 0 {
                return ret(p, 0);
            }
            let (v, _) = strtol(p, src, 10, true, true);
            ret(p, v);
        }
        0x12 => {
            let (src, dst) = (arg(p, 0), arg(p, 1));
            let (v, stop) = strtol(p, src, 10, true, false);
            wr32(p, dst, v);
            ret(p, stop);
        }
        0x15 => {
            let (dst, src) = (arg(p, 0), arg(p, 1));
            if dst == 0 || src == 0 {
                return ret(p, 0);
            }
            let end = dst + strlen(p, dst);
            copy_string(p, end, src, u32::MAX);
            ret(p, dst);
        }
        0x16 => {
            let (dst, src, max) = (arg(p, 0), arg(p, 1), arg(p, 2) as i32);
            if dst == 0 || src == 0 {
                return ret(p, 0);
            }
            let end = dst + strlen(p, dst);
            let s = string(p, src, max.max(0) as usize);
            put_string(p, end, &s);
            ret(p, dst);
        }
        0x17 | 0x18 => {
            let (a, b) = (arg(p, 0), arg(p, 1));
            let max = if n == 0x18 { arg(p, 2) } else { u32::MAX };
            let v = match (a, b) {
                (0, 0) => 0,
                (0, _) => -1,
                (_, 0) => 1,
                _ => strcmp(p, a, b, max),
            };
            ret(p, v as u32);
        }
        0x19 => {
            let (dst, src) = (arg(p, 0), arg(p, 1));
            if dst == 0 || src == 0 {
                return ret(p, 0);
            }
            copy_string(p, dst, src, u32::MAX);
            ret(p, dst);
        }
        0x1A => {
            let (dst, src, max) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if dst == 0 || src == 0 {
                return ret(p, 0);
            }
            let s = string(p, src, max as usize);
            for i in 0..max.min(0x10_0000) {
                let c = s.get(i as usize).copied().unwrap_or(0);
                wr8(p, dst + i, c);
            }
            ret(p, dst);
        }
        0x1B => {
            let s = arg(p, 0);
            let v = if s == 0 { 0 } else { strlen(p, s) };
            ret(p, v);
        }
        0x1C | 0x1E => {
            let (s, c) = (arg(p, 0), arg(p, 1) as u8);
            let v = if s == 0 { 0 } else { index(p, s, c, false) };
            ret(p, v);
        }
        0x1D | 0x1F => {
            let (s, c) = (arg(p, 0), arg(p, 1) as u8);
            let v = if s == 0 { 0 } else { index(p, s, c, true) };
            ret(p, v);
        }
        0x20 => {
            let (src, list) = (arg(p, 0), arg(p, 1));
            let l = string(p, list, 256);
            let s = string(p, src, 0x10_0000);
            let v = match s.iter().position(|c| l.contains(c)) {
                Some(i) => src + i as u32,
                // As psx-spx has it: src itself unless the string is empty.
                None if s.is_empty() => 0,
                None => src,
            };
            ret(p, v);
        }
        0x21 | 0x22 => {
            let (src, list) = (arg(p, 0), arg(p, 1));
            let l = string(p, list, 256);
            let s = string(p, src, 0x10_0000);
            let want = n == 0x21;
            let v = s
                .iter()
                .position(|c| l.contains(c) != want)
                .unwrap_or(s.len());
            ret(p, v as u32);
        }
        0x23 => strtok(p),
        0x24 => {
            let (s, sub) = (arg(p, 0), arg(p, 1));
            let v = strstr(p, s, sub);
            ret(p, v);
        }
        0x25 | 0x26 => {
            let c = arg(p, 0) & 0xFF;
            let v = match c as u8 {
                b'a'..=b'z' if n == 0x25 => c - 0x20,
                b'A'..=b'Z' if n == 0x26 => c + 0x20,
                _ => c,
            };
            ret(p, v);
        }
        0x27 => {
            let (src, dst, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if src != 0 && (len as i32) >= 0 {
                copy_forward(p, dst, src, len);
            }
            ret(p, src);
        }
        0x28 | 0x2B => {
            let (dst, fill, len) = if n == 0x28 {
                (arg(p, 0), 0, arg(p, 1))
            } else {
                (arg(p, 0), arg(p, 1) as u8, arg(p, 2))
            };
            if dst == 0 || len == 0 || (len as i32) < 0 {
                return ret(p, 0);
            }
            for i in 0..len {
                wr8(p, dst + i, fill);
            }
            ret(p, dst);
        }
        0x29 | 0x2D => {
            let (a, b, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if a == 0 || b == 0 {
                return ret(p, 0);
            }
            // Bug for bug: the difference of the bytes after the first
            // mismatch.
            let mut v = 0;
            for i in 0..len {
                if rd8(p, a + i) != rd8(p, b + i) {
                    v = (rd8(p, a + i + 1) as i32 - rd8(p, b + i + 1) as i32) as u32;
                    break;
                }
            }
            ret(p, v);
        }
        0x2A => {
            let (dst, src, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if dst != 0 && (len as i32) >= 0 {
                copy_forward(p, dst, src, len);
            }
            ret(p, dst);
        }
        0x2C => {
            let (dst, src, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
            if dst != 0 && (len as i32) >= 0 {
                if src < dst && dst < src.wrapping_add(len) {
                    let mut i = len;
                    loop {
                        let c = rd8(p, src + i);
                        wr8(p, dst + i, c);
                        if i == 0 {
                            break;
                        }
                        i -= 1;
                    }
                } else {
                    copy_forward(p, dst, src, len);
                }
            }
            ret(p, dst);
        }
        0x2E => {
            let (src, c, len) = (arg(p, 0), arg(p, 1) as u8, arg(p, 2));
            if src == 0 || (len as i32) < 0 {
                return ret(p, 0);
            }
            let v = (0..len)
                .find(|&i| p.bus.load8(src + i) == c)
                .map_or(0, |i| src + i);
            ret(p, v);
        }
        0x2F => {
            let x = rd32(p, var(V_RAND))
                .wrapping_mul(0x41C6_4E6D)
                .wrapping_add(0x3039);
            wr32(p, var(V_RAND), x);
            ret(p, (x >> 16) & 0x7FFF);
        }
        0x30 => {
            let s = arg(p, 0);
            wr32(p, var(V_RAND), s);
            ret(p, 0);
        }
        0x33 => {
            let size = arg(p, 0);
            let v = malloc(p, size);
            ret(p, v);
        }
        0x34 => {
            let b = arg(p, 0);
            free(p, b);
            ret_void(p);
        }
        0x37 => {
            let size = arg(p, 0).wrapping_mul(arg(p, 1));
            let v = malloc(p, size);
            if v != 0 {
                for i in 0..size {
                    wr8(p, v + i, 0);
                }
            }
            ret(p, v);
        }
        0x38 => {
            let (old, size) = (arg(p, 0), arg(p, 1));
            if old == 0 {
                let v = malloc(p, size);
                return ret(p, v);
            }
            if size == 0 {
                free(p, old);
                return ret(p, 0);
            }
            let v = malloc(p, size);
            if v != 0 {
                copy_forward(p, v, old, size);
                free(p, old);
            }
            ret(p, v);
        }
        0x39 => {
            let (addr, size) = (arg(p, 0), arg(p, 1));
            // Only noted: nothing is written into the heap until the first
            // malloc. Grand Theft Auto 2 gives InitHeap memory it goes on to
            // use itself, and a block header written there at once was
            // later jumped to.
            wr32(p, var(V_HEAP_START), addr);
            wr32(p, var(V_HEAP_END), addr.wrapping_add(size));
            wr32(p, var(V_HEAP_READY), 0);
            ret(p, 0);
        }
        _ => {
            log(p, &format!("A({n:02X}h) routed to libc but not handled"));
            ret(p, 0);
        }
    }
}

fn strlen(p: &mut Psx, s: u32) -> u32 {
    let mut n = 0;
    while n < 0x10_0000 && rd8(p, s + n) != 0 {
        n += 1;
    }
    n
}

fn copy_string(p: &mut Psx, dst: u32, src: u32, max: u32) {
    let mut i = 0;
    while i < max && i < 0x10_0000 {
        let c = rd8(p, src + i);
        wr8(p, dst + i, c);
        if c == 0 {
            break;
        }
        i += 1;
    }
}

fn copy_forward(p: &mut Psx, dst: u32, src: u32, len: u32) {
    for i in 0..len {
        let c = rd8(p, src.wrapping_add(i));
        wr8(p, dst.wrapping_add(i), c);
    }
}

/// Signed-byte difference at the first mismatch, 0 if equal up to the end
/// or `max`.
fn strcmp(p: &mut Psx, a: u32, b: u32, max: u32) -> i32 {
    let mut i = 0;
    while i < max && i < 0x10_0000 {
        let (x, y) = (rd8(p, a + i), rd8(p, b + i));
        if x != y {
            return x as i8 as i32 - y as i8 as i32;
        }
        if x == 0 {
            break;
        }
        i += 1;
    }
    0
}

fn index(p: &mut Psx, s: u32, c: u8, last: bool) -> u32 {
    let mut found = 0;
    let mut i = 0;
    loop {
        let x = rd8(p, s + i);
        if x == c {
            found = s + i;
            if !last {
                return found;
            }
        }
        if x == 0 || i > 0x10_0000 {
            return found;
        }
        i += 1;
    }
}

/// strstr as psx-spx describes it: after a partial match it carries on from
/// where the match broke, not from the next character.
fn strstr(p: &mut Psx, s: u32, sub: u32) -> u32 {
    if s == 0 || sub == 0 {
        return 0;
    }
    let mut at = s;
    for _ in 0..0x10_0000 {
        if rd8(p, at) == 0 {
            return 0;
        }
        let (mut a, mut b) = (at, sub);
        while rd8(p, b) != 0 && rd8(p, a) == rd8(p, b) {
            a += 1;
            b += 1;
        }
        if rd8(p, b) == 0 {
            return at;
        }
        at = if a == at { at + 1 } else { a };
    }
    0
}

/// strtok with the kernel's own 100h-byte buffer, which it copies the
/// string into on the first call.
fn strtok(p: &mut Psx) {
    let (src, list) = (arg(p, 0), arg(p, 1));
    let buf = var(V_STRTOK_BUF);
    if src != 0 {
        let s = string(p, src, 0xFF);
        put_string(p, buf, &s);
        wr32(p, var(V_STRTOK), buf);
    }
    let l = string(p, list, 256);
    let start = rd32(p, var(V_STRTOK));
    if start == 0 || rd8(p, start) == 0 {
        wr32(p, var(V_STRTOK), 0);
        return ret(p, 0);
    }
    let mut at = start;
    loop {
        let c = rd8(p, at);
        if c == 0 {
            wr32(p, var(V_STRTOK), at);
            return ret(p, start);
        }
        if l.contains(&c) {
            wr8(p, at, 0);
            at += 1;
            // Runs of separators are skipped only when the list has one.
            if l.len() == 1 {
                while rd8(p, at) == l[0] {
                    at += 1;
                }
            }
            wr32(p, var(V_STRTOK), at);
            return ret(p, start);
        }
        at += 1;
    }
}

/// strtol, strtoul and atoi. `atoi` takes a leading "0" as octal, where
/// strtol wants "o".
fn strtol(p: &mut Psx, src: u32, base: u32, signed: bool, atoi: bool) -> (u32, u32) {
    let mut at = src;
    while matches!(rd8(p, at), 0x09..=0x0D | 0x20) {
        at += 1;
    }
    let mut neg = false;
    if signed && rd8(p, at) == b'-' {
        neg = true;
        at += 1;
    }
    let mut base = if (2..=36).contains(&base) { base } else { 10 };
    let c0 = rd8(p, at).to_ascii_lowercase();
    let c1 = rd8(p, at + 1).to_ascii_lowercase();
    if c0 == b'0' && c1 == b'x' {
        base = 16;
        at += 2;
    } else if c0 == b'0' && c1 == b'b' {
        base = 2;
        at += 2;
    } else if (c0 == b'o' && !atoi) || (c0 == b'0' && atoi) {
        base = 8;
        at += 1;
    }
    let mut v: u32 = 0;
    loop {
        let c = rd8(p, at).to_ascii_lowercase();
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 10,
            _ => break,
        };
        if d >= base {
            break;
        }
        v = v.wrapping_mul(base).wrapping_add(d);
        at += 1;
    }
    (if neg { v.wrapping_neg() } else { v }, at)
}

// ---- TTY ----------------------------------------------------------------------

pub(super) fn tty_putchar(p: &mut Psx, c: u8) {
    p.tty.push(c);
}

pub(super) fn puts(p: &mut Psx) {
    let s = arg(p, 0);
    if s == 0 {
        p.tty.extend_from_slice(b"<NULL>");
    } else {
        let text = string(p, s, 4096);
        p.tty.extend_from_slice(&text);
    }
    ret(p, 0);
}

/// printf, from psx-spx's list of what the kernel's understands.
pub(super) fn printf(p: &mut Psx) {
    let fmt = arg(p, 0);
    let f = string(p, fmt, 4096);
    let mut argn = 1;
    let mut out = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let c = f[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut plus, mut space, mut alt) =
            (false, false, false, false, false);
        while i < f.len() {
            match f[i] {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        if i < f.len() && f[i] == b'*' {
            let w = arg(p, argn) as i32;
            argn += 1;
            if w < 0 {
                left = true;
            }
            width = w.unsigned_abs() as usize;
            i += 1;
        }
        while i < f.len() && f[i].is_ascii_digit() {
            width = width * 10 + (f[i] - b'0') as usize;
            i += 1;
        }
        // The kernel's printf also takes '-' after the width: the suite's
        // access-time prints "%2-d", and on the console that is the number
        // left-justified in two columns.
        while i < f.len() && f[i] == b'-' {
            left = true;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < f.len() && f[i] == b'.' {
            i += 1;
            let mut pr = 0;
            if i < f.len() && f[i] == b'*' {
                pr = arg(p, argn) as usize;
                argn += 1;
                i += 1;
            }
            while i < f.len() && f[i].is_ascii_digit() {
                pr = pr * 10 + (f[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(pr);
        }
        let mut half = false;
        while i < f.len() && matches!(f[i], b'h' | b'l' | b'L') {
            half = f[i] == b'h';
            i += 1;
        }
        if i >= f.len() {
            break;
        }
        let conv = f[i];
        i += 1;
        let mut body: Vec<u8> = match conv {
            b'%' => b"%".to_vec(),
            b'c' => {
                let v = arg(p, argn) as u8;
                argn += 1;
                vec![v]
            }
            b's' => {
                let s = arg(p, argn);
                argn += 1;
                let mut t = if s == 0 {
                    b"<NULL>".to_vec()
                } else {
                    string(p, s, 4096)
                };
                if let Some(pr) = prec {
                    t.truncate(pr);
                }
                t
            }
            b'd' | b'i' | b'D' => {
                let mut v = arg(p, argn) as i32;
                argn += 1;
                if half {
                    v = v as i16 as i32;
                }
                let mut s = v.unsigned_abs().to_string().into_bytes();
                if v < 0 {
                    s.insert(0, b'-');
                } else if plus {
                    s.insert(0, b'+');
                } else if space {
                    s.insert(0, b' ');
                }
                s
            }
            b'u' | b'U' | b'o' | b'O' | b'x' | b'X' | b'p' => {
                let mut v = arg(p, argn);
                argn += 1;
                if half {
                    v = v as i16 as i32 as u32;
                }
                let mut s = match conv {
                    b'o' | b'O' => format!("{v:o}"),
                    b'x' | b'p' => format!("{v:x}"),
                    b'X' => format!("{v:X}"),
                    _ => format!("{v}"),
                }
                .into_bytes();
                if alt {
                    let prefix: &[u8] = match conv {
                        b'x' | b'p' => b"0x",
                        b'X' => b"0X",
                        b'o' | b'O' if s[0] != b'0' => b"0",
                        _ => b"",
                    };
                    s.splice(0..0, prefix.iter().copied());
                }
                s
            }
            b'n' => {
                let a = arg(p, argn);
                argn += 1;
                if half {
                    wr16(p, a, out.len() as u16);
                } else {
                    wr32(p, a, out.len() as u32);
                }
                Vec::new()
            }
            other => vec![b'%', other],
        };
        if body.len() < width {
            let pad = width - body.len();
            if left {
                body.extend(std::iter::repeat_n(b' ', pad));
            } else if zero && conv != b's' && conv != b'c' {
                let at = usize::from(matches!(body.first(), Some(b'-' | b'+' | b' ')));
                body.splice(at..at, std::iter::repeat_n(b'0', pad));
            } else {
                body.splice(0..0, std::iter::repeat_n(b' ', pad));
            }
        }
        out.extend_from_slice(&body);
    }
    let n = out.len() as u32;
    p.tty.extend_from_slice(&out);
    ret(p, n);
}

// ---- heap ---------------------------------------------------------------------

/// One free block covering `size` bytes. Every block has a word in front of
/// it: its size, with bit 0 set while free, which is what psx-spx's `free`
/// sets.
pub(super) fn init_heap_at(p: &mut Psx, addr: u32, size: u32) {
    if size < 8 {
        return;
    }
    wr32(p, addr, ((size - 4) & !3) | 1);
}

/// malloc on the heap InitHeap named, formatting it on first use.
fn malloc(p: &mut Psx, size: u32) -> u32 {
    let (lo, hi) = (rd32(p, var(V_HEAP_START)), rd32(p, var(V_HEAP_END)));
    if lo == 0 {
        return 0;
    }
    if rd32(p, var(V_HEAP_READY)) == 0 {
        init_heap_at(p, lo, hi.wrapping_sub(lo));
        wr32(p, var(V_HEAP_READY), 1);
    }
    alloc(p, lo, hi, size)
}

/// First fit, joining free neighbours on the way past.
pub(super) fn alloc(p: &mut Psx, lo: u32, hi: u32, size: u32) -> u32 {
    let n = (size.wrapping_add(3)) & !3;
    if lo == 0 || hi <= lo || n > hi - lo {
        return 0;
    }
    let mut at = lo;
    let mut guard = 0;
    while at.wrapping_add(4) <= hi && guard < 0x10_0000 {
        guard += 1;
        let hdr = rd32(p, at);
        let mut len = hdr & !3;
        if at + 4 + len > hi || at + 4 + len <= at && len != 0 {
            return 0;
        }
        if hdr & 1 != 0 {
            loop {
                let next = at + 4 + len;
                if next + 4 > hi {
                    break;
                }
                let nh = rd32(p, next);
                if nh & 1 == 0 || next + 4 + (nh & !3) > hi {
                    break;
                }
                len += 4 + (nh & !3);
            }
            if len >= n {
                if len >= n + 8 {
                    wr32(p, at, n);
                    wr32(p, at + 4 + n, (len - n - 4) | 1);
                } else {
                    wr32(p, at, len);
                }
                return at + 4;
            }
            wr32(p, at, len | 1);
        }
        at += 4 + len;
    }
    0
}

pub(super) fn free(p: &mut Psx, buf: u32) {
    let h = rd32(p, buf.wrapping_sub(4));
    wr32(p, buf.wrapping_sub(4), h | 1);
}
