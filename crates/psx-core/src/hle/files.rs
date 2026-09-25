// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! The kernel's devices and files, psx-spx "BIOS File Functions", "BIOS File
//! Execute and Flush Cache" and "BIOS CDROM Functions".
//!
//! "cdrom:" reads the disc image through [`iso`] directly, so a read costs
//! no emulated time and does not touch the drive: the drive is the game's,
//! from its own first command. "bu" is the memory card, in [`card`]. "tty"
//! is the host's log.

use super::*;

/// psx-spx "File Error Numbers".
pub(super) const E_NOT_FOUND: u32 = 0x02;
pub(super) const E_BAD_FD: u32 = 0x09;
pub(super) const E_GENERAL: u32 = 0x10;
pub(super) const E_EXISTS: u32 = 0x11;
pub(super) const E_UNKNOWN_DEVICE: u32 = 0x13;
pub(super) const E_ALIGN: u32 = 0x16;
pub(super) const E_NO_HANDLE: u32 = 0x18;
pub(super) const E_CARD_FULL: u32 = 0x1C;

// FCB fields.
pub(super) const F_STATUS: u32 = 0x00;
pub(super) const F_PORT: u32 = 0x04;
pub(super) const F_POS: u32 = 0x10;
pub(super) const F_FLAGS: u32 = 0x14;
pub(super) const F_ERROR: u32 = 0x18;
pub(super) const F_DCB: u32 = 0x1C;
pub(super) const F_SIZE: u32 = 0x20;
pub(super) const F_START: u32 = 0x24;
pub(super) const F_NUMBER: u32 = 0x28;

pub(super) const DEV_CDROM: u32 = 0;
pub(super) const DEV_CARD: u32 = 1;
pub(super) const DEV_TTY: u32 = 2;

pub(super) fn fcb(fd: u32) -> u32 {
    kseg1(FCB_BASE + fd * FCB_SIZE)
}

fn dcb(dev: u32) -> u32 {
    kseg1(DCB_BASE + dev * DCB_SIZE)
}

pub(super) fn errno(p: &mut Psx, e: u32) {
    wr32(p, var(V_ERRNO), e);
}

/// The three devices, and std in and out on the TTY.
pub(super) fn init_devices(p: &mut Psx) {
    wr32(p, kseg1(0x140), kseg1(FCB_BASE));
    wr32(p, kseg1(0x144), FCB_COUNT * FCB_SIZE);
    wr32(p, kseg1(0x150), kseg1(DCB_BASE));
    wr32(p, kseg1(0x154), DCB_COUNT * DCB_SIZE);
    let names: [(&[u8], &[u8], u32, u32); 3] = [
        (b"cdrom", b"CD-ROM", 0x14, 0x800),
        (b"bu", b"MEMORY CARD", 0x14, 0x80),
        (b"tty", b"CONSOLE", 0x01, 1),
    ];
    let mut at = kseg1(NAMES);
    for (i, (short, long, flags, sector)) in names.into_iter().enumerate() {
        let d = dcb(i as u32);
        put_string(p, at, short);
        wr32(p, d, at);
        at += short.len() as u32 + 1;
        put_string(p, at, long);
        wr32(p, d + 0xC, at);
        at += long.len() as u32 + 1;
        wr32(p, d + 4, flags);
        wr32(p, d + 8, sector);
    }
    for fd in 0..FCB_COUNT {
        wr32(p, fcb(fd) + F_NUMBER, fd);
    }
    for (fd, mode) in [(0, 1), (1, 2)] {
        wr32(p, fcb(fd) + F_STATUS, mode);
        wr32(p, fcb(fd) + F_DCB, dcb(DEV_TTY));
        wr32(p, fcb(fd) + F_FLAGS, 1);
    }
}

/// "bu10:NAME" into (device, port, rest). The port is hex after the name.
pub(super) fn split_device(path: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let colon = path.iter().position(|&c| c == b':')?;
    let (dev, rest) = (&path[..colon], &path[colon + 1..]);
    // The name is the letters, the port the digits after: "bu10", "cdrom0".
    let letters = dev.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    let (name, port) = dev.split_at(letters);
    let port = u32::from_str_radix(std::str::from_utf8(port).ok()?, 16).unwrap_or(0);
    let d = match name {
        b"cdrom" => DEV_CDROM,
        b"bu" => DEV_CARD,
        b"tty" => DEV_TTY,
        _ => return None,
    };
    Some((d, port, rest.to_vec()))
}

/// A path on the disc, from the root, with the current directory applied.
fn cd_path(p: &mut Psx, rest: &[u8]) -> String {
    let rest = String::from_utf8_lossy(rest).into_owned();
    if rest.starts_with('\\') || rest.starts_with('/') {
        rest
    } else {
        let cur = String::from_utf8_lossy(&string(p, var(V_CURDIR), 0x7F)).into_owned();
        format!("{cur}\\{rest}")
    }
}

fn free_fcb(p: &mut Psx) -> Option<u32> {
    (2..FCB_COUNT).find(|&fd| p.bus.load32(fcb(fd) + F_STATUS) == 0)
}

fn valid_fd(p: &mut Psx, fd: u32) -> Option<u32> {
    if fd < FCB_COUNT && rd32(p, fcb(fd) + F_STATUS) != 0 {
        Some(fcb(fd))
    } else {
        None
    }
}

fn device_of(p: &mut Psx, f: u32) -> u32 {
    let d = rd32(p, f + F_DCB);
    (0..3).find(|&i| dcb(i) == d).unwrap_or(DEV_TTY)
}

pub(super) fn open(p: &mut Psx) {
    let (name, mode) = (arg(p, 0), arg(p, 1));
    let path = string(p, name, 0x80);
    let Some((dev, port, rest)) = split_device(&path) else {
        errno(p, E_UNKNOWN_DEVICE);
        return ret(p, 0xFFFF_FFFF);
    };
    let Some(fd) = free_fcb(p) else {
        errno(p, E_NO_HANDLE);
        return ret(p, 0xFFFF_FFFF);
    };
    let f = fcb(fd);
    match dev {
        DEV_CDROM => {
            let full = cd_path(p, &rest);
            let found = p.bus.cdrom.disc.as_mut().and_then(|d| iso::find(d, &full));
            let Some(e) = found.filter(|e| !e.dir) else {
                errno(p, E_NOT_FOUND);
                return ret(p, 0xFFFF_FFFF);
            };
            setup_fcb(p, f, mode, 0, DEV_CDROM, e.size, e.lba);
            ret(p, fd);
        }
        DEV_CARD => card::open(p, fd, port, &rest, mode),
        _ => {
            setup_fcb(p, f, mode.max(1), port, DEV_TTY, 0, 0);
            ret(p, fd);
        }
    }
}

pub(super) fn setup_fcb(
    p: &mut Psx,
    f: u32,
    mode: u32,
    port: u32,
    dev: u32,
    size: u32,
    start: u32,
) {
    wr32(p, f + F_STATUS, mode.max(1));
    wr32(p, f + F_PORT, port);
    wr32(p, f + F_POS, 0);
    let flags = rd32(p, dcb(dev) + 4);
    wr32(p, f + F_FLAGS, flags);
    wr32(p, f + F_ERROR, 0);
    wr32(p, f + F_DCB, dcb(dev));
    wr32(p, f + F_SIZE, size);
    wr32(p, f + F_START, start);
}

pub(super) fn lseek(p: &mut Psx) {
    let (fd, off, how) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let Some(f) = valid_fd(p, fd) else {
        errno(p, E_BAD_FD);
        return ret(p, 0xFFFF_FFFF);
    };
    let pos = rd32(p, f + F_POS);
    let size = rd32(p, f + F_SIZE);
    let new = match how {
        0 => off,
        1 => pos.wrapping_add(off),
        2 => size.wrapping_add(off),
        _ => {
            errno(p, E_ALIGN);
            return ret(p, 0xFFFF_FFFF);
        }
    };
    wr32(p, f + F_POS, new);
    ret(p, new);
}

pub(super) fn read(p: &mut Psx) {
    let (fd, dst, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let Some(f) = valid_fd(p, fd) else {
        errno(p, E_BAD_FD);
        return ret(p, 0xFFFF_FFFF);
    };
    match device_of(p, f) {
        DEV_CDROM => {
            let (pos, size, lba) = (
                rd32(p, f + F_POS),
                rd32(p, f + F_SIZE),
                rd32(p, f + F_START),
            );
            // Whole sectors: the last one reads to its end.
            let end = size.div_ceil(0x800) * 0x800;
            if len == 0 || pos >= end {
                wr32(p, f + F_ERROR, E_ALIGN);
                errno(p, E_ALIGN);
                return ret(p, 0xFFFF_FFFF);
            }
            let n = len.min(end - pos);
            let data = p
                .bus
                .cdrom
                .disc
                .as_mut()
                .and_then(|d| iso::read(d, lba, pos, n));
            let Some(data) = data else {
                errno(p, E_GENERAL);
                return ret(p, 0xFFFF_FFFF);
            };
            for (i, &b) in data.iter().enumerate() {
                wr8(p, dst + i as u32, b);
            }
            wr32(p, f + F_POS, pos + n);
            ret(p, n);
        }
        DEV_CARD => card::read_write(p, fd, dst, len, false),
        _ => ret(p, 0),
    }
}

pub(super) fn write(p: &mut Psx) {
    let (fd, src, len) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let Some(f) = valid_fd(p, fd) else {
        errno(p, E_BAD_FD);
        return ret(p, 0xFFFF_FFFF);
    };
    match device_of(p, f) {
        DEV_CARD => card::read_write(p, fd, src, len, true),
        DEV_TTY => {
            let text: Vec<u8> = (0..len.min(0x1_0000))
                .map(|i| p.bus.load8(src + i))
                .collect();
            p.tty.extend_from_slice(&text);
            ret(p, len);
        }
        _ => ret(p, 0),
    }
}

pub(super) fn close(p: &mut Psx) {
    let fd = arg(p, 0);
    let Some(f) = valid_fd(p, fd) else {
        errno(p, E_BAD_FD);
        return ret(p, 0xFFFF_FFFF);
    };
    if fd >= 2 {
        wr32(p, f + F_STATUS, 0);
    }
    ret(p, fd);
}

pub(super) fn isatty(p: &mut Psx) {
    let fd = arg(p, 0);
    let v = match valid_fd(p, fd) {
        Some(f) => (rd32(p, f + F_FLAGS) >> 1) & 1,
        None => 0,
    };
    ret(p, v);
}

pub(super) fn getc(p: &mut Psx) {
    ret(p, 0xFFFF_FFFF);
}

pub(super) fn putc(p: &mut Psx) {
    let (c, fd) = (arg(p, 0) as u8, arg(p, 1));
    if fd == 1 {
        p.tty.push(c);
        ret(p, 1)
    } else {
        ret(p, 0xFFFF_FFFF)
    }
}

pub(super) fn get_error(p: &mut Psx) {
    let fd = arg(p, 0);
    let v = match valid_fd(p, fd) {
        Some(f) => rd32(p, f + F_ERROR),
        None => 0xFFFF_FFFF,
    };
    ret(p, v);
}

/// B(40h) cd: "cdrom:\PATH" becomes where relative names start.
pub(super) fn cd(p: &mut Psx) {
    let a0_ = arg(p, 0);
    let path = string(p, a0_, 0x80);
    let Some((DEV_CDROM, _, rest)) = split_device(&path) else {
        errno(p, E_UNKNOWN_DEVICE);
        return ret(p, 0);
    };
    let full = cd_path(p, &rest);
    let trimmed = full.trim_end_matches(['\\', '/']).to_string();
    put_string(
        p,
        var(V_CURDIR),
        &trimmed.as_bytes()[..trimmed.len().min(0x7F)],
    );
    ret(p, 1);
}

/// Wildcards as psx-spx has them: "?" is any one character, and "*" the
/// rest of the name. A "?" that meets the end of the name also ends the
/// match, successfully.
pub(super) fn wildcard(name: &[u8], pat: &[u8]) -> bool {
    let mut i = 0;
    for &c in pat {
        match c {
            b'*' => return true,
            b'?' => {
                if i >= name.len() {
                    return true;
                }
                i += 1;
            }
            _ => {
                if i >= name.len() || !name[i].eq_ignore_ascii_case(&c) {
                    return false;
                }
                i += 1;
            }
        }
    }
    i == name.len()
}

/// Write a direntry: name, attribute, size, next (unused), first sector.
pub(super) fn put_direntry(p: &mut Psx, d: u32, name: &[u8], attr: u32, size: u32, sector: u32) {
    for i in 0..0x14 {
        let c = if i < 0x13 {
            name.get(i as usize).copied().unwrap_or(0)
        } else {
            0
        };
        wr8(p, d + i, c);
    }
    wr32(p, d + 0x14, attr);
    wr32(p, d + 0x18, size);
    wr32(p, d + 0x1C, 0);
    wr32(p, d + 0x20, sector);
    wr32(p, d + 0x24, 0);
}

pub(super) fn firstfile(p: &mut Psx) {
    let (pat, dir) = (arg(p, 0), arg(p, 1));
    let path = string(p, pat, 0x7F);
    put_string(p, var(V_PATTERN), &path);
    wr32(p, var(V_SEARCH_NEXT), 0);
    search(p, dir);
}

pub(super) fn nextfile(p: &mut Psx) {
    let dir = arg(p, 0);
    search(p, dir);
}

/// Continue the search firstfile started, from the index it reached.
fn search(p: &mut Psx, dir: u32) {
    let path = string(p, var(V_PATTERN), 0x7F);
    let from = rd32(p, var(V_SEARCH_NEXT));
    let Some((dev, port, rest)) = split_device(&path) else {
        return ret(p, 0);
    };
    wr32(p, var(V_SEARCH_DEV), dev);
    if dev == DEV_CARD {
        return card::search(p, port, &rest, from, dir);
    }
    if dev != DEV_CDROM {
        return ret(p, 0);
    }
    let full = cd_path(p, &rest);
    let (folder, pattern) = match full.rfind(['\\', '/']) {
        Some(at) => (full[..at].to_string(), full[at + 1..].to_string()),
        None => (String::new(), full.clone()),
    };
    let listing = match p.bus.cdrom.disc.as_mut() {
        Some(disc) => match iso::find(disc, &folder) {
            Some(d) if d.dir => iso::list(disc, &d),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    let pat = pattern.as_bytes();
    for (i, e) in listing.iter().enumerate().skip(2).skip(from as usize) {
        let name = e.name.as_bytes();
        let bare = e.name.split(';').next().unwrap_or("").as_bytes();
        if wildcard(name, pat) || wildcard(bare, pat) {
            wr32(p, var(V_SEARCH_NEXT), i as u32 - 1);
            put_direntry(p, dir, name, 0, e.size, e.lba);
            return ret(p, dir);
        }
    }
    wr32(p, var(V_SEARCH_NEXT), u32::MAX / 2);
    ret(p, 0);
}

// ---- executables ------------------------------------------------------------

/// The 800h-byte header and body of a PSX-EXE on the disc.
fn read_exe(p: &mut Psx, name: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let (dev, _, rest) = split_device(name)?;
    if dev != DEV_CDROM {
        return None;
    }
    let full = cd_path(p, &rest);
    let disc = p.bus.cdrom.disc.as_mut()?;
    let mut e = iso::find(disc, &full);
    if e.is_none() && !full.contains(';') {
        e = iso::find(disc, &format!("{full};1"));
    }
    let e = e.filter(|e| !e.dir)?;
    let header = iso::read(disc, e.lba, 0, 0x800)?;
    if &header[..8] != crate::exe::MAGIC {
        return None;
    }
    let size = u32::from_le_bytes([header[0x1C], header[0x1D], header[0x1E], header[0x1F]]);
    let body = iso::read(disc, e.lba, 0x800, size.min(e.size.saturating_sub(0x800)))?;
    Some((header, body))
}

/// LoadTest's copy of header bytes 10h to 4Bh.
fn put_header(p: &mut Psx, buf: u32, header: &[u8]) {
    for i in 0..0x3C {
        wr8(p, buf + i, header[0x10 + i as usize]);
    }
}

pub(super) fn load_test(p: &mut Psx) {
    let (name, buf) = (arg(p, 0), arg(p, 1));
    let n = string(p, name, 0x40);
    match read_exe(p, &n) {
        Some((header, _)) => {
            put_header(p, buf, &header);
            let pc = rd32(p, buf);
            ret(p, pc);
        }
        None => ret(p, 0),
    }
}

pub(super) fn load(p: &mut Psx) {
    let (name, buf) = (arg(p, 0), arg(p, 1));
    let n = string(p, name, 0x40);
    match read_exe(p, &n) {
        Some((header, body)) => {
            put_header(p, buf, &header);
            let dest = rd32(p, buf + 8);
            copy_in(p, dest, &body);
            ret(p, 1);
        }
        None => ret(p, 0),
    }
}

fn copy_in(p: &mut Psx, dest: u32, body: &[u8]) {
    let base = crate::bus::mask_region(dest) as usize;
    if base < crate::bus::RAM_SIZE && base + body.len() <= crate::bus::RAM_SIZE {
        p.bus.ram[base..base + body.len()].copy_from_slice(body);
    } else {
        for (i, &b) in body.iter().enumerate() {
            wr8(p, dest + i as u32, b);
        }
    }
}

/// Exec(headerbuf, a0, a1): the caller's registers into the header's spare
/// words, the BSS cleared, the stack set if the header gives one, and in.
/// $s0 carries the header address across, since the executable must keep it.
fn start(p: &mut Psx, buf: u32, a0: u32, a1: u32, ra: u32) {
    let (r, sp, fp, gp, s0) = (reg(p, RA), reg(p, SP), reg(p, FP), reg(p, GP), reg(p, S0));
    wr32(p, buf + 0x28, r);
    wr32(p, buf + 0x2C, sp);
    wr32(p, buf + 0x30, fp);
    wr32(p, buf + 0x34, gp);
    wr32(p, buf + 0x38, s0);
    let (bss, bss_size) = (rd32(p, buf + 0x18), rd32(p, buf + 0x1C));
    if bss_size != 0 {
        for i in 0..bss_size / 4 {
            wr32(p, bss + i * 4, 0);
        }
    }
    let (base, off) = (rd32(p, buf + 0x20), rd32(p, buf + 0x24));
    if base != 0 {
        set(p, SP, base.wrapping_add(off));
        set(p, FP, base.wrapping_add(off));
    }
    let gp = rd32(p, buf + 4);
    set(p, GP, gp);
    set(p, A0, a0);
    set(p, A1, a1);
    set(p, S0, buf);
    set(p, RA, ra);
    let pc = rd32(p, buf);
    jump(p, pc);
}

pub(super) fn exec(p: &mut Psx) {
    let (buf, a0, a1) = (arg(p, 0), arg(p, 1), arg(p, 2));
    start(p, buf, a0, a1, trap(T_EXEC_RETURNED));
}

/// The executable Exec started came back: the caller's registers from the
/// header, and 1.
pub(super) fn exec_returned(p: &mut Psx) {
    let buf = reg(p, S0);
    let (ra, sp, fp, gp, s0) = (
        rd32(p, buf + 0x28),
        rd32(p, buf + 0x2C),
        rd32(p, buf + 0x30),
        rd32(p, buf + 0x34),
        rd32(p, buf + 0x38),
    );
    set(p, SP, sp);
    set(p, FP, fp);
    set(p, GP, gp);
    set(p, S0, s0);
    set(p, RA, ra);
    ret(p, 1);
}

/// Load `name` and run it with the stack at `stack + offset`, returning to
/// `ra`. The header goes where LoadExec keeps it. False if it cannot load.
pub(super) fn load_and_exec(p: &mut Psx, name: &str, stack: u32, offset: u32, ra: u32) -> bool {
    let Some((header, body)) = read_exe(p, name.as_bytes()) else {
        return false;
    };
    let buf = var(V_BOOT_HEADER);
    put_header(p, buf, &header);
    let dest = rd32(p, buf + 8);
    copy_in(p, dest, &body);
    wr32(p, buf + 0x20, stack);
    wr32(p, buf + 0x24, offset);
    wr32(p, var(V_BOOT_STACK), stack);
    start(p, buf, 1, 0, ra);
    true
}

pub(super) fn load_exec(p: &mut Psx) {
    let (name, stack, offset) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let mut n = string(p, name, 0x1F);
    if !n.contains(&b';') {
        n.extend_from_slice(b";1");
    }
    let n = String::from_utf8_lossy(&n).into_owned();
    // Interrupts on, as psx-spx says LoadExec's ExitCriticalSection leaves
    // them. The boot path does not do this: a real BIOS starts the boot
    // executable with them off.
    p.cpu.cop0.sr |= 0x401;
    if !load_and_exec(p, &n, stack, offset, trap(T_LOADEXEC_RETURNED)) {
        log(p, &format!("LoadExec cannot load {n}"));
        loadexec_returned(p);
    }
}

/// On the console, what follows a returning LoadExec crashes (psx-spx, Part
/// 3's bug), so this halts.
pub(super) fn loadexec_returned(p: &mut Psx) {
    log(
        p,
        "a LoadExec'd executable returned; halting as the console does",
    );
    jump(p, trap(T_HANG));
}

// ---- direct CD-ROM access ---------------------------------------------------

pub(super) fn cd_get_lbn(p: &mut Psx) {
    let a0_ = arg(p, 0);
    let path = string(p, a0_, 0x80);
    let lba = split_device(&path)
        .filter(|(d, _, _)| *d == DEV_CDROM)
        .map(|(_, _, rest)| cd_path(p, &rest))
        .and_then(|full| p.bus.cdrom.disc.as_mut().and_then(|d| iso::find(d, &full)))
        .map_or(0xFFFF_FFFF, |e| e.lba);
    ret(p, lba);
}

pub(super) fn cd_read_sector(p: &mut Psx) {
    let (count, sector, buf) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let data = p
        .bus
        .cdrom
        .disc
        .as_mut()
        .and_then(|d| iso::read(d, sector, 0, count.min(0x400) * 0x800));
    match data {
        Some(d) => {
            copy_in(p, buf, &d);
            ret(p, count);
        }
        None => ret(p, 0xFFFF_FFFF),
    }
}

fn cd_event_then_return(p: &mut Psx, v: u32) {
    set(p, V0, v);
    let ra = reg(p, RA);
    deliver_then(p, &[(CLASS_CDROM, SPEC_COMPLETE)], ra);
}

pub(super) fn cd_async_seek(p: &mut Psx) {
    let src = arg(p, 0);
    let msf = [rd8(p, src), rd8(p, src + 1), rd8(p, src + 2)];
    let lba = crate::disc::msf_bcd_to_lba(msf);
    wr32(p, var(V_CD_LBA), lba);
    cd_event_then_return(p, 1);
}

pub(super) fn cd_async_status(p: &mut Psx) {
    let dst = arg(p, 0);
    wr8(p, dst, 0x02);
    cd_event_then_return(p, 1);
}

pub(super) fn cd_async_mode(p: &mut Psx) {
    let mode = arg(p, 0);
    wr32(p, var(V_CD_MODE), mode);
    cd_event_then_return(p, 1);
}

pub(super) fn cd_async_read(p: &mut Psx) {
    let (count, dst, mode) = (arg(p, 0), arg(p, 1), arg(p, 2));
    let lba = rd32(p, var(V_CD_LBA));
    let (from, len) = if mode & 0x10 != 0 {
        (24, 0x918)
    } else if mode & 0x20 != 0 {
        (12, 0x924)
    } else {
        (0, 0x800)
    };
    let mut at = dst;
    for s in 0..count.min(0x400) {
        let bytes = match p.bus.cdrom.disc.as_mut() {
            Some(disc) if len == 0x800 => iso::user_data(disc, lba + s).map(|d| d.to_vec()),
            Some(disc) => {
                let mut raw = [0u8; crate::disc::RAW_SECTOR];
                disc.read_sector(lba + s, &mut raw)
                    .then(|| raw[from..from + len].to_vec())
            }
            None => None,
        };
        let Some(bytes) = bytes else {
            return cd_event_then_return(p, 0);
        };
        copy_in(p, at, &bytes);
        at += len as u32;
    }
    wr32(p, var(V_CD_LBA), lba + count);
    cd_event_then_return(p, 1);
}
