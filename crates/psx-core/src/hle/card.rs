// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Memory cards, psx-spx "BIOS Memory Card Functions" and "Memory Card Data
//! Format".
//!
//! Two levels, as in the kernel. `_card_read`, `_card_write` and
//! `_card_info` queue one sector's work per slot, done by the vblank
//! handler, and report through the hardware-level card events (class
//! F0000011h) or, for `_card_info` and `_card_load`, the software-level ones
//! (F4000001h): 4 done, 100h no card, 2000h a new card, 8000h an error.
//!
//! The "bu" device works on the card's directory: open, create, read,
//! write, find, erase, rename and format, on the card image directly.

use super::files::*;
use super::*;
use crate::memcard::{formatted, FLAG_NEW, SECTOR};

const OP_NONE: u32 = 0;
const OP_READ: u32 = 1;
const OP_WRITE: u32 = 2;
const OP_INFO: u32 = 3;
const OP_LOAD: u32 = 4;
const OP_WRITE_TEST: u32 = 5;

/// `_card_status` values.
const ST_READY: u32 = 0x01;
const ST_READING: u32 = 0x02;
const ST_WRITING: u32 = 0x04;
const ST_INFO: u32 = 0x08;
const ST_TIMEOUT: u32 = 0x11;
const ST_ERROR: u32 = 0x21;

const BLOCK: usize = 0x2000;

fn ops(slot: u32) -> u32 {
    var(V_CARD_OPS + (slot & 1) * 0x10)
}

fn slot_of(port: u32) -> usize {
    ((port >> 4) & 1) as usize
}

pub(super) fn reset(p: &mut Psx) {
    for slot in 0..2 {
        wr32(p, ops(slot), OP_NONE);
        wr32(p, ops(slot) + 0xC, ST_READY);
    }
}

pub(super) fn bu_init(p: &mut Psx) {
    reset(p);
    ret(p, 0);
}

pub(super) fn software_event(p: &mut Psx, spec: u32) {
    let ra = reg(p, RA);
    set(p, V0, 0);
    deliver_then(p, &[(CLASS_SWCARD, spec)], ra);
}

fn queue(p: &mut Psx, port: u32, op: u32, sector: u32, addr: u32, status: u32) {
    let o = ops(slot_of(port) as u32);
    wr32(p, o, op);
    wr32(p, o + 4, sector);
    wr32(p, o + 8, addr);
    wr32(p, o + 0xC, status);
}

pub(super) fn card_info(p: &mut Psx) {
    let port = arg(p, 0);
    queue(p, port, OP_INFO, 0, 0, ST_INFO);
    ret(p, 1);
}

pub(super) fn card_load(p: &mut Psx) {
    let port = arg(p, 0);
    queue(p, port, OP_LOAD, 0, 0, ST_INFO);
    ret(p, 1);
}

pub(super) fn card_write_test(p: &mut Psx) {
    let port = arg(p, 0);
    queue(p, port, OP_WRITE_TEST, 0x3F, 0, ST_WRITING);
    ret(p, 1);
}

/// `_card_read` and `_card_write`. Sector 400h is accepted, as psx-spx
/// says the kernel's does, and fails when it is reached.
pub(super) fn card_io(p: &mut Psx, write: bool) {
    let (port, sector, addr) = (arg(p, 0), arg(p, 1), arg(p, 2));
    if sector > 0x400 {
        return ret(p, 0);
    }
    let (op, st) = if write {
        (OP_WRITE, ST_WRITING)
    } else {
        (OP_READ, ST_READING)
    };
    queue(p, port, op, sector, addr, st);
    ret(p, 1);
}

pub(super) fn card_status(p: &mut Psx) {
    let slot = arg(p, 0);
    let v = rd32(p, ops(slot) + 0xC);
    ret(p, v);
}

pub(super) fn card_wait(p: &mut Psx) {
    let slot = arg(p, 0);
    let v = rd32(p, ops(slot) + 0xC);
    if !matches!(v, ST_READING | ST_WRITING | ST_INFO) {
        ret(p, v);
    }
}

/// The vblank handler's card work: each slot's queued operation, and the
/// events that report it.
pub(super) fn run(p: &mut Psx) -> Vec<(u32, u32)> {
    let mut events = Vec::new();
    for slot in 0..2u32 {
        let o = ops(slot);
        let op = rd32(p, o);
        if op == OP_NONE {
            continue;
        }
        let (sector, addr) = (rd32(p, o + 4), rd32(p, o + 8));
        wr32(p, o, OP_NONE);
        wr32(p, var(V_CARD_CHAN), slot << 4);
        let card = &p.bus.sio.cards[slot as usize];
        let connected = card.connected;
        let new = card.flag & FLAG_NEW != 0;
        let ignore_new = rd32(p, var(V_CARD_IGNORE_NEW)) != 0;
        let (class, spec, status) = match op {
            OP_READ | OP_WRITE if !connected => (CLASS_HWCARD, SPEC_TIMEOUT, ST_TIMEOUT),
            OP_READ | OP_WRITE if new && !ignore_new => (CLASS_HWCARD, SPEC_NEW, ST_ERROR),
            OP_READ | OP_WRITE if sector >= 0x400 => (CLASS_HWCARD, SPEC_ERROR, ST_ERROR),
            OP_READ => {
                let at = sector as usize * SECTOR;
                let data = p.bus.sio.cards[slot as usize].data[at..at + SECTOR].to_vec();
                for (i, &b) in data.iter().enumerate() {
                    wr8(p, addr + i as u32, b);
                }
                (CLASS_HWCARD, SPEC_IO_END, ST_READY)
            }
            OP_WRITE => {
                let data: Vec<u8> = (0..SECTOR as u32).map(|i| p.bus.load8(addr + i)).collect();
                let card = &mut p.bus.sio.cards[slot as usize];
                let at = sector as usize * SECTOR;
                card.data[at..at + SECTOR].copy_from_slice(&data);
                card.flag &= !FLAG_NEW;
                card.written = true;
                (CLASS_HWCARD, SPEC_IO_END, ST_READY)
            }
            OP_WRITE_TEST if !connected => {
                events.push((CLASS_SWCARD, SPEC_TIMEOUT));
                (CLASS_HWCARD, SPEC_TIMEOUT, ST_TIMEOUT)
            }
            OP_WRITE_TEST => {
                p.bus.sio.cards[slot as usize].flag &= !FLAG_NEW;
                events.push((CLASS_SWCARD, SPEC_IO_END));
                (CLASS_HWCARD, SPEC_IO_END, ST_READY)
            }
            // _card_info and _card_load.
            _ if !connected => (CLASS_SWCARD, SPEC_TIMEOUT, ST_TIMEOUT),
            _ if new => (CLASS_SWCARD, SPEC_NEW, ST_READY),
            _ => (CLASS_SWCARD, SPEC_IO_END, ST_READY),
        };
        if matches!(op, OP_READ | OP_WRITE) {
            wr32(p, var(V_CARD_IGNORE_NEW), 0);
        }
        wr32(p, o + 0xC, status);
        events.push((class, spec));
    }
    events
}

// ---- the directory -------------------------------------------------------

fn frame(n: usize) -> std::ops::Range<usize> {
    n * SECTOR..(n + 1) * SECTOR
}

fn seal(d: &mut [u8], n: usize) {
    let r = frame(n);
    let x = d[r.start..r.end - 1].iter().fold(0, |a, b| a ^ b);
    d[r.end - 1] = x;
}

fn state(d: &[u8], n: usize) -> u8 {
    d[n * SECTOR]
}

fn entry_name(d: &[u8], n: usize) -> Vec<u8> {
    let at = n * SECTOR + 0x0A;
    d[at..at + 0x15]
        .iter()
        .take_while(|&&c| c != 0)
        .copied()
        .collect()
}

fn next_block(d: &[u8], n: usize) -> Option<usize> {
    let at = n * SECTOR + 8;
    let v = u16::from_le_bytes([d[at], d[at + 1]]);
    (v != 0xFFFF && v < 15).then_some(v as usize + 1)
}

/// The blocks of the file starting at `first`, in order.
fn chain(d: &[u8], first: usize) -> Vec<usize> {
    let mut out = vec![first];
    let mut at = first;
    while let Some(n) = next_block(d, at) {
        if out.contains(&n) || out.len() >= 15 {
            break;
        }
        out.push(n);
        at = n;
    }
    out
}

/// The first directory entry with state `want` whose name matches.
fn find(d: &[u8], name: &[u8], want: u8) -> Option<usize> {
    (1..16).find(|&n| state(d, n) == want && wildcard(&entry_name(d, n), name))
}

/// A copy of the card's contents, if a card is in: the directory, read.
///
/// On the console, a file function reads the card through the kernel's
/// own sector routine, and every sector it moves is reported like a game's
/// `_card_read`, on F0000011h. Games wait on that: Metal Slug X runs
/// firstfile on its "checking memory card" screen and then waits for the
/// low-level event, and hung there until this reported one. Reported from
/// the next vblank, as a sector read would complete.
fn contents(p: &mut Psx, port: u32) -> Option<Vec<u8>> {
    let c = &p.bus.sio.cards[slot_of(port)];
    let d = c.connected.then(|| c.data.clone());
    sector_io_done(p, d.is_some());
    d
}

fn sector_io_done(p: &mut Psx, connected: bool) {
    deliver_later(
        p,
        CLASS_HWCARD,
        if connected { SPEC_IO_END } else { SPEC_TIMEOUT },
    );
}

fn store(p: &mut Psx, port: u32, d: Vec<u8>) {
    let c = &mut p.bus.sio.cards[slot_of(port)];
    c.data = d;
    c.written = true;
}

pub(super) fn open(p: &mut Psx, fd: u32, port: u32, name: &[u8], mode: u32) {
    let Some(mut d) = contents(p, port) else {
        errno(p, E_GENERAL);
        return ret(p, 0xFFFF_FFFF);
    };
    let first = if mode & 0x200 != 0 {
        if find(&d, name, 0x51).is_some() {
            errno(p, E_EXISTS);
            return ret(p, 0xFFFF_FFFF);
        }
        let blocks = ((mode >> 16) as usize).clamp(1, 15);
        let free: Vec<usize> = (1..16)
            .filter(|&n| state(&d, n) & 0xF0 == 0xA0)
            .take(blocks)
            .collect();
        if free.len() < blocks {
            errno(p, E_CARD_FULL);
            return ret(p, 0xFFFF_FFFF);
        }
        for (i, &n) in free.iter().enumerate() {
            let r = frame(n);
            d[r.clone()].fill(0);
            d[r.start] = match (i, blocks) {
                (0, _) => 0x51,
                (i, b) if i == b - 1 => 0x53,
                _ => 0x52,
            };
            let next = free.get(i + 1).map_or(0xFFFF, |&m| m as u16 - 1);
            d[r.start + 8..r.start + 10].copy_from_slice(&next.to_le_bytes());
            if i == 0 {
                d[r.start + 4..r.start + 8]
                    .copy_from_slice(&((blocks * BLOCK) as u32).to_le_bytes());
                let len = name.len().min(0x14);
                d[r.start + 0x0A..r.start + 0x0A + len].copy_from_slice(&name[..len]);
            }
            seal(&mut d, n);
        }
        let first = free[0];
        store(p, port, d.clone());
        first
    } else {
        match find(&d, name, 0x51) {
            Some(n) => n,
            None => {
                errno(p, E_NOT_FOUND);
                return ret(p, 0xFFFF_FFFF);
            }
        }
    };
    let at = first * SECTOR + 4;
    let size = u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
    setup_fcb(p, fcb(fd), mode, port, DEV_CARD, size, first as u32);
    ret(p, fd);
}

/// read or write on an open card file: whole sectors, from a sector
/// boundary, through the file's block chain.
pub(super) fn read_write(p: &mut Psx, fd: u32, addr: u32, len: u32, write: bool) {
    let f = fcb(fd);
    let (mode, port, pos, size, first) = (
        rd32(p, f + F_STATUS),
        rd32(p, f + F_PORT),
        rd32(p, f + F_POS),
        rd32(p, f + F_SIZE),
        rd32(p, f + F_START),
    );
    if !pos.is_multiple_of(0x80) || !len.is_multiple_of(0x80) || pos.saturating_add(len) > size {
        wr32(p, f + F_ERROR, E_ALIGN);
        errno(p, E_ALIGN);
        return ret(p, 0xFFFF_FFFF);
    }
    if !p.bus.sio.cards[slot_of(port)].connected {
        wr32(p, f + F_ERROR, E_GENERAL);
        errno(p, E_GENERAL);
        return ret(p, 0xFFFF_FFFF);
    }
    let slot = slot_of(port);
    sector_io_done(p, true);
    let blocks = chain(&p.bus.sio.cards[slot].data, first as usize);
    for k in 0..len / 0x80 {
        let off = (pos + k * 0x80) as usize;
        let Some(&b) = blocks.get(off / BLOCK) else {
            break;
        };
        let at = b * BLOCK + off % BLOCK;
        let ram = addr + k * 0x80;
        if write {
            let data: Vec<u8> = (0..0x80).map(|i| p.bus.load8(ram + i)).collect();
            let card = &mut p.bus.sio.cards[slot];
            card.data[at..at + 0x80].copy_from_slice(&data);
            card.written = true;
        } else {
            let data = p.bus.sio.cards[slot].data[at..at + 0x80].to_vec();
            for (i, &c) in data.iter().enumerate() {
                wr8(p, ram + i as u32, c);
            }
        }
    }
    wr32(p, f + F_POS, pos + len);
    if mode & 0x8000 != 0 {
        // Asynchronous: done already, but reported as the kernel reports
        // it, from the next vblank, and answered with 0, "accepted", not
        // a byte count. Metal Slug X loops on read until it returns 0, and
        // with a save on the card hung on "checking memory card".
        deliver_later(p, fd, SPEC_IO_END);
        deliver_later(p, CLASS_SWCARD, SPEC_IO_END);
        return ret(p, 0);
    }
    ret(p, len);
}

/// firstfile2 and nextfile on a card: entries from `from + 1` on.
pub(super) fn search(p: &mut Psx, port: u32, pattern: &[u8], from: u32, dir: u32) {
    let want = if rd32(p, var(V_CARD_FIND)) == 1 {
        0xA1
    } else {
        0x51
    };
    let Some(d) = contents(p, port) else {
        return ret(p, 0);
    };
    for n in (from as usize + 1)..16 {
        if state(&d, n) == want && wildcard(&entry_name(&d, n), pattern) {
            let at = n * SECTOR + 4;
            let size = u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
            wr32(p, var(V_SEARCH_NEXT), n as u32);
            let attr = if want == 0x51 { 0x50 } else { 0xA0 };
            put_direntry(p, dir, &entry_name(&d, n), attr, size, n as u32 * 0x40);
            return ret(p, dir);
        }
    }
    wr32(p, var(V_SEARCH_NEXT), 16);
    ret(p, 0)
}

fn card_path(p: &mut Psx, n: u32) -> Option<(u32, Vec<u8>)> {
    let path = string(p, n, 0x40);
    match split_device(&path) {
        Some((DEV_CARD, port, rest)) => Some((port, rest)),
        _ => None,
    }
}

/// erase, or with `undelete` its reverse, over every block of the file.
pub(super) fn erase(p: &mut Psx, undelete: bool) {
    let a0_ = arg(p, 0);
    let Some((port, name)) = card_path(p, a0_) else {
        errno(p, E_UNKNOWN_DEVICE);
        return ret(p, 0);
    };
    let Some(mut d) = contents(p, port) else {
        errno(p, E_GENERAL);
        return ret(p, 0);
    };
    let (from, to) = if undelete { (0xA1, 0x51) } else { (0x51, 0xA1) };
    let Some(first) = find(&d, &name, from) else {
        errno(p, E_NOT_FOUND);
        return ret(p, 0);
    };
    if undelete && find(&d, &name, 0x51).is_some() {
        errno(p, E_EXISTS);
        return ret(p, 0);
    }
    for n in chain(&d, first) {
        let s = state(&d, n);
        // 51/52/53 in use, A1/A2/A3 the same, deleted.
        d[n * SECTOR] = (s & 0x0F) | (to & 0xF0);
        seal(&mut d, n);
    }
    store(p, port, d);
    ret(p, 1);
}

pub(super) fn rename(p: &mut Psx) {
    let (a, b) = (arg(p, 0), arg(p, 1));
    let (Some((port, old)), Some((port2, new))) = (card_path(p, a), card_path(p, b)) else {
        errno(p, E_UNKNOWN_DEVICE);
        return ret(p, 0);
    };
    if slot_of(port) != slot_of(port2) {
        errno(p, 0x12);
        return ret(p, 0);
    }
    let Some(mut d) = contents(p, port) else {
        errno(p, E_GENERAL);
        return ret(p, 0);
    };
    if find(&d, &new, 0x51).is_some() {
        errno(p, E_EXISTS);
        return ret(p, 0);
    }
    let Some(n) = find(&d, &old, 0x51) else {
        errno(p, E_NOT_FOUND);
        return ret(p, 0);
    };
    let at = n * SECTOR + 0x0A;
    d[at..at + 0x15].fill(0);
    let len = new.len().min(0x14);
    d[at..at + len].copy_from_slice(&new[..len]);
    seal(&mut d, n);
    store(p, port, d);
    ret(p, 1);
}

pub(super) fn format(p: &mut Psx) {
    let a0_ = arg(p, 0);
    let Some((port, _)) = card_path(p, a0_) else {
        errno(p, E_UNKNOWN_DEVICE);
        return ret(p, 0);
    };
    if contents(p, port).is_none() {
        errno(p, E_GENERAL);
        return ret(p, 0);
    }
    store(p, port, formatted());
    ret(p, 1);
}
