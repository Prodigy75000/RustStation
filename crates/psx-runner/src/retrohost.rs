// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! A minimal libretro frontend, for testing our own shim.
//!
//! Everything else in this repo drives `psx_core` directly, which means the
//! libretro layer, the thing that actually ships, is the one part nothing
//! exercises. This loads the built shared object the way a real frontend does,
//! through `dlopen`, and drives the C ABI: environment negotiation, content
//! loading, a held pad, and frames out.
//!
//! What that catches, and unit tests on the shim's tables cannot: an entry
//! point that is missing or misnamed, a pixel format the frontend refuses, a
//! `need_fullpath` that leaves content unopenable, a BIOS the core cannot find
//! in the system directory, and a video callback whose geometry disagrees with
//! what it declared.
//!
//! ```text
//! retrohost <core.so|core.dll> <system-dir> [--content <path>]
//!           [--frames N] [--hold start] [--out frame.png]
//! ```
//!
//! `system-dir` is what the core is told is the frontend's system directory,
//! and it must hold a BIOS under one of the canonical names (`scph1001.bin`
//! and friends). That is not a detail: a dump named after its release is
//! invisible to a core looking for `scph1001.bin`, and the failure is a core
//! that loads and then refuses content.

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;

// ---------------------------------------------------------------------------
// dlopen, without a crate for it
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod dl {
    use std::ffi::{c_char, c_void, CString};

    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    pub unsafe fn open(path: &str) -> Result<*mut c_void, String> {
        let c = CString::new(path).map_err(|e| e.to_string())?;
        let h = LoadLibraryA(c.as_ptr());
        if h.is_null() {
            return Err(format!("LoadLibraryA failed for {path}"));
        }
        Ok(h)
    }

    pub unsafe fn sym(handle: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let p = GetProcAddress(handle, c.as_ptr());
        if p.is_null() {
            return Err(format!("missing symbol {name}"));
        }
        Ok(p)
    }
}

#[cfg(not(windows))]
mod dl {
    use std::ffi::{c_char, c_void, CStr, CString};

    const RTLD_NOW: i32 = 2;

    extern "C" {
        fn dlopen(name: *const c_char, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }

    unsafe fn last_error() -> String {
        let e = dlerror();
        if e.is_null() {
            "(no error)".to_string()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }

    pub unsafe fn open(path: &str) -> Result<*mut c_void, String> {
        let c = CString::new(path).map_err(|e| e.to_string())?;
        let h = dlopen(c.as_ptr(), RTLD_NOW);
        if h.is_null() {
            return Err(format!("dlopen {path}: {}", last_error()));
        }
        Ok(h)
    }

    pub unsafe fn sym(handle: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let p = dlsym(handle, c.as_ptr());
        if p.is_null() {
            return Err(format!("missing symbol {name}: {}", last_error()));
        }
        Ok(p)
    }
}

// ---------------------------------------------------------------------------
// The slice of the ABI this needs
// ---------------------------------------------------------------------------

#[repr(C)]
struct SystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}

#[repr(C)]
struct GameGeometry {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
}

#[repr(C)]
struct SystemTiming {
    fps: f64,
    sample_rate: f64,
}

#[repr(C)]
struct SystemAvInfo {
    geometry: GameGeometry,
    timing: SystemTiming,
}

#[repr(C)]
struct GameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
struct InputDescriptor {
    port: c_uint,
    device: c_uint,
    index: c_uint,
    id: c_uint,
    description: *const c_char,
}

const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;
const ENV_SET_INPUT_DESCRIPTORS: c_uint = 11;
const PIXEL_FORMAT_XRGB8888: c_uint = 1;
const ENV_SET_DISK_CONTROL_INTERFACE: c_uint = 13;
const ENV_SET_DISK_CONTROL_EXT_INTERFACE: c_uint = 58;

/// The leading fields of `retro_disk_control_(ext_)callback`, which are all a
/// swap needs.
#[repr(C)]
struct DiskControl {
    set_eject_state: unsafe extern "C" fn(bool) -> bool,
    get_eject_state: unsafe extern "C" fn() -> bool,
    get_image_index: unsafe extern "C" fn() -> c_uint,
    set_image_index: unsafe extern "C" fn(c_uint) -> bool,
    get_num_images: unsafe extern "C" fn() -> c_uint,
}

/// RetroPad ids by name, for `--hold`. The frontend's side of the mapping the
/// core is being tested on, written independently of it on purpose.
const BUTTON_NAMES: [(&str, c_uint); 16] = [
    ("b", 0),
    ("y", 1),
    ("select", 2),
    ("start", 3),
    ("up", 4),
    ("down", 5),
    ("left", 6),
    ("right", 7),
    ("a", 8),
    ("x", 9),
    ("l", 10),
    ("r", 11),
    ("l2", 12),
    ("r2", 13),
    ("l3", 14),
    ("r3", 15),
];

// ---------------------------------------------------------------------------
// Frontend state. Callback-driven and single-threaded, like every frontend.
// ---------------------------------------------------------------------------

static mut SYSTEM_DIR: Option<CString> = None;
static mut HELD: u32 = 0;
static mut FRAME: Vec<u32> = Vec::new();
static mut FRAME_W: usize = 0;
static mut FRAME_H: usize = 0;
static mut FRAMES_SEEN: u64 = 0;
static mut AUDIO_FRAMES: u64 = 0;
static mut DESCRIPTORS: Vec<(c_uint, String)> = Vec::new();
static mut PIXEL_FORMAT_OK: bool = false;
static mut DISK: Option<*const DiskControl> = None;

unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        ENV_GET_SYSTEM_DIRECTORY => {
            let Some(dir) = (*std::ptr::addr_of!(SYSTEM_DIR)).as_ref() else {
                return false;
            };
            *(data as *mut *const c_char) = dir.as_ptr();
            true
        }
        ENV_SET_PIXEL_FORMAT => {
            let fmt = *(data as *const c_uint);
            // A real frontend supports several. Accepting only the one the core
            // is supposed to ask for turns a silent colour-swap into a refusal.
            PIXEL_FORMAT_OK = fmt == PIXEL_FORMAT_XRGB8888;
            PIXEL_FORMAT_OK
        }
        ENV_SET_INPUT_DESCRIPTORS => {
            let mut p = data as *const InputDescriptor;
            let list = &mut *std::ptr::addr_of_mut!(DESCRIPTORS);
            list.clear();
            while !(*p).description.is_null() {
                let text = CStr::from_ptr((*p).description)
                    .to_string_lossy()
                    .into_owned();
                if (*p).port == 0 {
                    list.push(((*p).id, text));
                }
                p = p.add(1);
            }
            true
        }
        ENV_SET_DISK_CONTROL_INTERFACE | ENV_SET_DISK_CONTROL_EXT_INTERFACE => {
            DISK = Some(data as *const DiskControl);
            true
        }
        // Everything else unsupported, which a core must cope with.
        _ => false,
    }
}

unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    FRAMES_SEEN += 1;
    if data.is_null() {
        return; // A duplicated frame. Legal, and means "reuse the last one".
    }
    let (w, h) = (width as usize, height as usize);
    let frame = &mut *std::ptr::addr_of_mut!(FRAME);
    frame.clear();
    frame.reserve(w * h);
    let base = data as *const u8;
    for y in 0..h {
        let row = base.add(y * pitch) as *const u32;
        for x in 0..w {
            frame.push(*row.add(x));
        }
    }
    FRAME_W = w;
    FRAME_H = h;
}

unsafe extern "C" fn audio_sample(_l: i16, _r: i16) {
    AUDIO_FRAMES += 1;
}

unsafe extern "C" fn audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    AUDIO_FRAMES += frames as u64;
    frames
}

unsafe extern "C" fn input_poll() {}

unsafe extern "C" fn input_state(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    // Port 0 only, digital pad only: this is a test harness, not a frontend.
    if port != 0 || device != 1 || id >= 16 {
        return 0;
    }
    i16::from(HELD & (1 << id) != 0)
}

// ---------------------------------------------------------------------------

macro_rules! entry {
    ($handle:expr, $name:literal, $ty:ty) => {{
        let p = unsafe { dl::sym($handle, $name) }?;
        unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
    }};
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("retrohost: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "usage: retrohost <core.so|core.dll> <system-dir> [--content PATH] \
             [--frames N] [--hold BUTTON] [--out PNG]"
                .to_string(),
        );
    }

    let core_path = args[0].clone();
    let system_dir = args[1].clone();
    let mut content: Option<String> = None;
    let mut frames: u64 = 600;
    let mut out: Option<String> = None;
    let mut hold: u32 = 0;
    let mut mash = false;
    let mut save_at: Option<(u64, String)> = None;
    let mut swaps: Vec<(u64, c_uint)> = Vec::new();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--content" => {
                i += 1;
                content = args.get(i).cloned();
            }
            "--frames" => {
                i += 1;
                frames = args
                    .get(i)
                    .ok_or("--frames needs a number")?
                    .parse()
                    .map_err(|e| format!("--frames: {e}"))?;
            }
            "--out" => {
                i += 1;
                out = args.get(i).cloned();
            }
            "--hold" => {
                i += 1;
                let name = args
                    .get(i)
                    .ok_or("--hold needs a button")?
                    .to_ascii_lowercase();
                let (_, id) = BUTTON_NAMES
                    .iter()
                    .find(|(n, _)| *n == name)
                    .ok_or_else(|| format!("unknown button {name}"))?;
                hold |= 1 << id;
            }
            "--mash" => mash = true,
            "--save-at" => {
                let n = args.get(i + 1).ok_or("--save-at needs FRAME PATH")?;
                let path = args.get(i + 2).ok_or("--save-at needs FRAME PATH")?;
                save_at = Some((
                    n.parse().map_err(|e| format!("--save-at: {e}"))?,
                    path.clone(),
                ));
                i += 2;
            }
            "--swap-at" => {
                // FRAME DISC (1-based): open the lid, change the disc and close
                // it, all before that frame, the way the TrophyHub host does.
                let n = args.get(i + 1).ok_or("--swap-at needs FRAME DISC")?;
                let d = args.get(i + 2).ok_or("--swap-at needs FRAME DISC")?;
                let d: c_uint = d.parse().map_err(|e| format!("--swap-at: {e}"))?;
                swaps.push((
                    n.parse().map_err(|e| format!("--swap-at: {e}"))?,
                    d.max(1) - 1,
                ));
                i += 2;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }

    unsafe {
        SYSTEM_DIR = Some(CString::new(system_dir.clone()).map_err(|e| e.to_string())?);
        HELD = hold;
    }

    let handle = unsafe { dl::open(&core_path) }?;

    let api_version = entry!(
        handle,
        "retro_api_version",
        unsafe extern "C" fn() -> c_uint
    );
    let set_environment = entry!(
        handle,
        "retro_set_environment",
        unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool)
    );
    let set_video = entry!(
        handle,
        "retro_set_video_refresh",
        unsafe extern "C" fn(unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize))
    );
    let set_audio = entry!(
        handle,
        "retro_set_audio_sample",
        unsafe extern "C" fn(unsafe extern "C" fn(i16, i16))
    );
    let set_audio_batch = entry!(
        handle,
        "retro_set_audio_sample_batch",
        unsafe extern "C" fn(unsafe extern "C" fn(*const i16, usize) -> usize)
    );
    let set_input_poll = entry!(
        handle,
        "retro_set_input_poll",
        unsafe extern "C" fn(unsafe extern "C" fn())
    );
    let set_input_state = entry!(
        handle,
        "retro_set_input_state",
        unsafe extern "C" fn(unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16)
    );
    let get_system_info = entry!(
        handle,
        "retro_get_system_info",
        unsafe extern "C" fn(*mut SystemInfo)
    );
    let get_av_info = entry!(
        handle,
        "retro_get_system_av_info",
        unsafe extern "C" fn(*mut SystemAvInfo)
    );
    let init = entry!(handle, "retro_init", unsafe extern "C" fn());
    let load_game = entry!(
        handle,
        "retro_load_game",
        unsafe extern "C" fn(*const GameInfo) -> bool
    );
    let retro_run = entry!(handle, "retro_run", unsafe extern "C" fn());
    let retro_serialize_size = entry!(
        handle,
        "retro_serialize_size",
        unsafe extern "C" fn() -> usize
    );
    let retro_serialize = entry!(
        handle,
        "retro_serialize",
        unsafe extern "C" fn(*mut c_void, usize) -> bool
    );
    let serialize_size = entry!(
        handle,
        "retro_serialize_size",
        unsafe extern "C" fn() -> usize
    );
    let unload = entry!(handle, "retro_unload_game", unsafe extern "C" fn());
    let deinit = entry!(handle, "retro_deinit", unsafe extern "C" fn());

    unsafe {
        if api_version() != 1 {
            return Err(format!("core reports libretro API {}", api_version()));
        }

        let mut info = std::mem::zeroed::<SystemInfo>();
        get_system_info(&mut info);
        let name = CStr::from_ptr(info.library_name).to_string_lossy();
        let version = CStr::from_ptr(info.library_version).to_string_lossy();
        let exts = CStr::from_ptr(info.valid_extensions).to_string_lossy();
        println!("core: {name} {version}");
        println!(
            "      extensions {exts}, need_fullpath {}",
            info.need_fullpath
        );

        set_environment(environment);
        set_video(video_refresh);
        set_audio(audio_sample);
        set_audio_batch(audio_sample_batch);
        set_input_poll(input_poll);
        set_input_state(input_state);

        init();

        // A path, not a buffer: the core asked for `need_fullpath`, and handing
        // it a buffer anyway is how a frontend "supports" cue sheets and then
        // cannot open the tracks beside them.
        let c_path = content
            .as_deref()
            .map(|p| CString::new(p).map_err(|e| e.to_string()))
            .transpose()?;
        let game = c_path.as_ref().map(|p| GameInfo {
            path: p.as_ptr(),
            data: std::ptr::null(),
            size: 0,
            meta: std::ptr::null(),
        });
        let ok = match &game {
            Some(g) => load_game(g),
            None => load_game(std::ptr::null()),
        };
        if !ok {
            return Err(format!(
                "retro_load_game refused. Is there a BIOS in {system_dir}, \
                 named the way the core looks for it?"
            ));
        }
        if !PIXEL_FORMAT_OK {
            return Err("core never negotiated a pixel format this frontend accepts".into());
        }

        let mut av = std::mem::zeroed::<SystemAvInfo>();
        get_av_info(&mut av);
        println!(
            "      declared {}x{} max {}x{}, {:.2} fps, {} Hz, state {} bytes",
            av.geometry.base_width,
            av.geometry.base_height,
            av.geometry.max_width,
            av.geometry.max_height,
            av.timing.fps,
            av.timing.sample_rate as u64,
            serialize_size()
        );

        if let Some(dc) = DISK {
            let dc = &*dc;
            println!(
                "      disc control: {} image(s), disc {} in the drive",
                (dc.get_num_images)(),
                (dc.get_image_index)() + 1
            );
        }
        let descs = &*std::ptr::addr_of!(DESCRIPTORS);
        if descs.is_empty() {
            println!("      no input descriptors published");
        } else {
            let held: Vec<&str> = descs
                .iter()
                .filter(|(id, _)| hold & (1 << id) != 0)
                .map(|(_, text)| text.as_str())
                .collect();
            println!(
                "      {} input descriptors; holding {}",
                descs.len(),
                if held.is_empty() {
                    "nothing".to_string()
                } else {
                    held.join(", ")
                }
            );
        }

        // --mash: a fixed button sequence, one press of six frames every
        // thirty, so a run gets past title screens the way a player would and
        // two runs press exactly the same things. RetroPad names: b is the
        // south button (Cross), a the east (Circle). Start first, then
        // confirm, cancel and the D-pad in a cycle.
        const MASH: [&str; 8] = ["start", "b", "b", "down", "b", "a", "right", "start"];
        let mash_ids: Vec<u32> = MASH
            .iter()
            .map(|n| {
                BUTTON_NAMES
                    .iter()
                    .find(|(m, _)| m == n)
                    .map(|(_, id)| *id)
                    .expect("a name in BUTTON_NAMES")
            })
            .collect();

        // Every retro_run is timed. A frame that takes seconds is how a hang
        // on a phone looks from the frontend: the unload waits behind it, and
        // the next load is told the game is still loaded.
        let (mut slowest, mut slowest_at) = (std::time::Duration::ZERO, 0u64);
        let mut total = std::time::Duration::ZERO;
        for f in 0..frames {
            if mash {
                let press = (f % 30) < 6;
                let id = mash_ids[((f / 30) as usize) % mash_ids.len()];
                HELD = hold | if press { 1 << id } else { 0 };
            }
            for (n, disc) in &swaps {
                if f == *n {
                    let Some(dc) = DISK else {
                        return Err("the core published no disc control".into());
                    };
                    let dc = &*dc;
                    let ok = (dc.set_eject_state)(true)
                        && (dc.set_image_index)(*disc)
                        && (dc.set_eject_state)(false);
                    println!(
                        "frame {f}: swap to disc {} of {}: {}",
                        disc + 1,
                        (dc.get_num_images)(),
                        if ok { "ok" } else { "REFUSED" }
                    );
                }
            }
            // --save-at: the state before frame N runs, so a slow or hung
            // frame can be replayed in the direct harness with its diagnostics.
            if let Some((n, path)) = &save_at {
                if f == *n {
                    let mut buf = vec![0u8; retro_serialize_size()];
                    if !retro_serialize(buf.as_mut_ptr() as *mut c_void, buf.len()) {
                        return Err("retro_serialize refused".into());
                    }
                    std::fs::write(path, &buf).map_err(|e| format!("{path}: {e}"))?;
                    println!(
                        "saved the state before frame {n} to {path} (held {:#06x})",
                        *std::ptr::addr_of!(HELD)
                    );
                }
            }
            let t = std::time::Instant::now();
            retro_run();
            let dt = t.elapsed();
            total += dt;
            if dt > slowest {
                slowest = dt;
                slowest_at = f;
            }
            if dt > std::time::Duration::from_secs(1) {
                println!("SLOW FRAME {f}: {:.1} s", dt.as_secs_f64());
            }
        }
        println!(
            "frame time: slowest {:.1} ms at frame {slowest_at}, mean {:.2} ms",
            slowest.as_secs_f64() * 1e3,
            total.as_secs_f64() * 1e3 / frames.max(1) as f64
        );

        let (w, h) = (FRAME_W, FRAME_H);
        let frame = &*std::ptr::addr_of!(FRAME);
        let lit = frame.iter().filter(|p| **p & 0x00FF_FFFF != 0).count();
        let (seen, audio) = (
            *std::ptr::addr_of!(FRAMES_SEEN),
            *std::ptr::addr_of!(AUDIO_FRAMES),
        );
        println!(
            "ran {frames} frames: {seen} video callbacks, {audio} audio frames, \
             last {w}x{h}, {lit} non-black pixels"
        );

        // A core that declared a size larger than it ever fills would go
        // unnoticed until a frontend allocated a texture for it.
        if w > av.geometry.max_width as usize || h > av.geometry.max_height as usize {
            return Err(format!(
                "core sent a {w}x{h} frame, larger than the {}x{} it declared",
                av.geometry.max_width, av.geometry.max_height
            ));
        }

        if let Some(path) = out {
            write_png(&path, frame, w, h)?;
            println!("wrote {path}");
        }

        unload();
        deinit();
    }
    Ok(())
}

/// The frame as a PNG, so a run can be looked at rather than only counted.
fn write_png(path: &str, frame: &[u32], w: usize, h: usize) -> Result<(), String> {
    if w == 0 || h == 0 || frame.len() < w * h {
        return Err("no frame to write".to_string());
    }
    let file = File::create(Path::new(path)).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for pixel in &frame[..w * h] {
        rgb.push((pixel >> 16) as u8);
        rgb.push((pixel >> 8) as u8);
        rgb.push(*pixel as u8);
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}
