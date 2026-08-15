// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! libretro C ABI front-end for RustStation.
//!
//! **This core cannot yet run a disc.** There is no SPU, no CD-ROM and no
//! controller, so `retro_run` steps the CPU for a frame's worth of cycles and
//! hands back silence. It does now hand back a **real picture**: whatever the
//! GPU has drawn into the displayed part of VRAM.
//!
//! The shim exists ahead of all that for three reasons: the state-transfer
//! surface (`retro_serialize` / `retro_unserialize`) is the one the netplay
//! handshake negotiates against and it is already honest; the
//! RetroAchievements memory interface needs system RAM exposed from day one (a
//! core that never publishes it hangs the achievement runtime on "waiting for
//! core memory map"); and the deploy path to a device is then a script rather
//! than a project.
//!
//! Content is a PSX-EXE for now, since that is what the conformance suites ship as.
//! Disc images land with the CD-ROM subsystem.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::path::PathBuf;
use std::ptr;

use psx_core::sio::button;
use psx_core::disc::Disc;
use psx_core::{exe::Exe, save, Psx};

// ---------------------------------------------------------------------------
// libretro ABI
// ---------------------------------------------------------------------------

const RETRO_API_VERSION: c_uint = 1;

const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;

/// `RETRO_PIXEL_FORMAT_XRGB8888`. It is **1**, not 2 (2 is RGB565), and getting
/// this wrong shows up as a colour-swapped picture rather than an error.
const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;

const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;

const RETRO_REGION_NTSC: c_uint = 0;

#[repr(C)]
pub struct SystemInfo {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
pub struct GameGeometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub aspect_ratio: f32,
}

#[repr(C)]
pub struct SystemTiming {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
pub struct SystemAvInfo {
    pub geometry: GameGeometry,
    pub timing: SystemTiming,
}

#[repr(C)]
pub struct GameInfo {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

type EnvironmentFn = unsafe extern "C" fn(c_uint, *mut c_void) -> bool;
type VideoRefreshFn = unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize);
type AudioSampleFn = unsafe extern "C" fn(i16, i16);
type AudioSampleBatchFn = unsafe extern "C" fn(*const i16, usize) -> usize;
type InputPollFn = unsafe extern "C" fn();
type InputStateFn = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;

// ---------------------------------------------------------------------------
// Core state
// ---------------------------------------------------------------------------

/// NTSC output geometry. The GPU can put out several widths; 640x480 is the
/// largest and the frame is letterboxed into it until there is one to letterbox.
const FB_WIDTH: usize = 640;
const FB_HEIGHT: usize = 480;

/// 33.8688 MHz / 60 Hz. Instruction-accurate rather than cycle-accurate for
/// now, so this is "instructions per frame". The name is the eventual meaning.
const CYCLES_PER_FRAME: u64 = 564_480;

const SAMPLE_RATE: f64 = 44_100.0;
/// Stereo frames per video frame, at the rate above.
const SAMPLES_PER_FRAME: usize = 735;

static mut ENV_CB: Option<EnvironmentFn> = None;
static mut VIDEO_CB: Option<VideoRefreshFn> = None;
static mut AUDIO_CB: Option<AudioSampleFn> = None;
static mut AUDIO_BATCH_CB: Option<AudioSampleBatchFn> = None;
static mut INPUT_POLL_CB: Option<InputPollFn> = None;
static mut INPUT_STATE_CB: Option<InputStateFn> = None;

static mut PSX: Option<Psx> = None;
static mut FRAMEBUFFER: Vec<u32> = Vec::new();
static mut SILENCE: Vec<i16> = Vec::new();

const LIBRARY_NAME: &[u8] = b"RustStation (PlayStation)\0";
const LIBRARY_VERSION: &[u8] = b"0.1.0\0";
const VALID_EXTENSIONS: &[u8] = b"exe|psexe\0";

/// BIOS images to look for in the frontend's system directory, best first.
/// A PlayStation core cannot do anything at all without one.
const BIOS_CANDIDATES: &[&str] = &[
    "scph5501.bin",
    "scph5500.bin",
    "scph5502.bin",
    "scph7001.bin",
    "scph1001.bin",
    "scph1000.bin",
    "psxonpsp660.bin",
    "bios.bin",
];

// The libretro ABI is single-threaded and callback-driven, so the core's state
// is process-global. These accessors go through raw pointers rather than taking
// a reference to the static directly. Same effect, but it keeps the
// `static_mut_refs` lint meaningful instead of blanket-allowed.
#[inline]
unsafe fn psx_ref() -> Option<&'static Psx> {
    (*ptr::addr_of!(PSX)).as_ref()
}

#[inline]
unsafe fn psx_mut() -> Option<&'static mut Psx> {
    (*ptr::addr_of_mut!(PSX)).as_mut()
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn retro_api_version() -> c_uint {
    RETRO_API_VERSION
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_environment(cb: EnvironmentFn) {
    ENV_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_video_refresh(cb: VideoRefreshFn) {
    VIDEO_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_audio_sample(cb: AudioSampleFn) {
    AUDIO_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_audio_sample_batch(cb: AudioSampleBatchFn) {
    AUDIO_BATCH_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_input_poll(cb: InputPollFn) {
    INPUT_POLL_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_set_input_state(cb: InputStateFn) {
    INPUT_STATE_CB = Some(cb);
}

#[no_mangle]
pub unsafe extern "C" fn retro_init() {
    FRAMEBUFFER = vec![0u32; FB_WIDTH * FB_HEIGHT];
    SILENCE = vec![0i16; SAMPLES_PER_FRAME * 2];
}

#[no_mangle]
pub unsafe extern "C" fn retro_deinit() {
    PSX = None;
    FRAMEBUFFER = Vec::new();
    SILENCE = Vec::new();
}

#[no_mangle]
pub unsafe extern "C" fn retro_get_system_info(info: *mut SystemInfo) {
    if info.is_null() {
        return;
    }
    (*info).library_name = LIBRARY_NAME.as_ptr() as *const c_char;
    (*info).library_version = LIBRARY_VERSION.as_ptr() as *const c_char;
    (*info).valid_extensions = VALID_EXTENSIONS.as_ptr() as *const c_char;
    // The EXE is handed over as a buffer; nothing needs a path on disc yet.
    (*info).need_fullpath = false;
    (*info).block_extract = false;
}

#[no_mangle]
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut SystemAvInfo) {
    if info.is_null() {
        return;
    }
    (*info).geometry = GameGeometry {
        base_width: FB_WIDTH as c_uint,
        base_height: FB_HEIGHT as c_uint,
        max_width: FB_WIDTH as c_uint,
        max_height: FB_HEIGHT as c_uint,
        aspect_ratio: 4.0 / 3.0,
    };
    (*info).timing = SystemTiming {
        fps: 60.0,
        sample_rate: SAMPLE_RATE,
    };
}

#[no_mangle]
pub extern "C" fn retro_set_controller_port_device(port: c_uint, device: c_uint) {
    // Only the digital pad exists, so the one thing worth honouring is a port
    // being emptied: a game that polls an absent controller must time out
    // rather than read a pad that answers with nothing held.
    if let Some(psx) = unsafe { psx_mut() } {
        if let Some(pad) = psx.bus.sio.pads.get_mut(port as usize) {
            pad.connected = device != 0;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro_reset() {
    if let Some(psx) = psx_mut() {
        psx.reset();
    }
}

/// RetroPad device and button ids, and the pad bit each one drives.
///
/// The mapping is the conventional PlayStation one: RetroPad B/A/Y/X are
/// Cross/Circle/Square/Triangle. It is spelled out rather than derived, because
/// the two layouts are rotated relative to each other and every core that gets
/// this wrong gets it wrong in the same confidently-symmetrical way.
const RETRO_DEVICE_JOYPAD: c_uint = 1;
const PAD_MAP: [(c_uint, u16); 16] = [
    (0, button::SELECT),
    (3, button::START),
    (4, button::UP),
    (5, button::DOWN),
    (6, button::LEFT),
    (7, button::RIGHT),
    (8, button::CROSS),    // RETRO_DEVICE_ID_JOYPAD_A
    (1, button::CIRCLE),   // B
    (2, button::SQUARE),   // Y
    (9, button::TRIANGLE), // X
    (10, button::L1),
    (11, button::R1),
    (12, button::L2),
    (13, button::R2),
    (14, button::L3),
    (15, button::R3),
];

/// Read both ports from the frontend into the emulated pads.
unsafe fn poll_pads(psx: &mut Psx) {
    let Some(state) = INPUT_STATE_CB else { return };
    for port in 0..2u32 {
        let mut held = 0u16;
        for (id, bit) in PAD_MAP {
            if state(port, RETRO_DEVICE_JOYPAD, 0, id) != 0 {
                held |= 1 << bit;
            }
        }
        psx.bus.sio.pads[port as usize].buttons = held;
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro_run() {
    if let Some(poll) = INPUT_POLL_CB {
        poll();
    }

    if let Some(psx) = psx_mut() {
        poll_pads(psx);
        psx.run(CYCLES_PER_FRAME);

        // The GPU picks the resolution, and software changes it mid-game, so
        // the geometry is read per frame rather than fixed at load. It can only
        // shrink from the maximum declared in `retro_get_system_av_info`.
        let width = psx.bus.gpu.display_width() as usize;
        let height = psx.bus.gpu.display_height() as usize;
        let fb = &mut *ptr::addr_of_mut!(FRAMEBUFFER);
        psx.bus.gpu.framebuffer(fb);

        if let Some(video) = VIDEO_CB {
            video(
                fb.as_ptr() as *const c_void,
                width as c_uint,
                height as c_uint,
                width * 4,
            );
        }
    }

    // No SPU yet: silence. It must still be *emitted*, because a frontend starved of
    // audio stalls its own frame pacing.
    if let Some(batch) = AUDIO_BATCH_CB {
        batch((*ptr::addr_of!(SILENCE)).as_ptr(), SAMPLES_PER_FRAME);
    }
}

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro_load_game(info: *const GameInfo) -> bool {
    let Some(bios) = load_bios() else {
        eprintln!(
            "[RustStation] no BIOS found in the frontend's system directory. \
             Expected one of: {}",
            BIOS_CANDIDATES.join(", ")
        );
        return false;
    };

    let mut psx = match Psx::new(bios) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[RustStation] {e}");
            return false;
        }
    };

    // A null info is "boot with no content", the BIOS menu. Valid on its own.
    if !info.is_null() {
        // A disc is loaded by path, not from the buffer the frontend may have
        // read for us: a cue sheet names other files beside it, and a disc is
        // too large to want in memory anyway.
        let path = if (*info).path.is_null() {
            None
        } else {
            CStr::from_ptr((*info).path).to_str().ok()
        };
        let is_disc = path.is_some_and(|p| {
            let p = p.to_ascii_lowercase();
            p.ends_with(".cue") || p.ends_with(".bin") || p.ends_with(".iso") || p.ends_with(".img")
        });

        if is_disc {
            match Disc::open(std::path::Path::new(path.unwrap())) {
                Ok(d) => psx.bus.cdrom.disc = Some(d),
                Err(e) => {
                    eprintln!("[RustStation] {e}");
                    return false;
                }
            }
        } else if !(*info).data.is_null() && (*info).size > 0 {
            let image = std::slice::from_raw_parts((*info).data as *const u8, (*info).size);
            match Exe::parse(image) {
                Ok(exe) => psx.sideload_exe(exe),
                Err(e) => {
                    eprintln!("[RustStation] {e}");
                    return false;
                }
            }
        }
    }

    let mut fmt = RETRO_PIXEL_FORMAT_XRGB8888;
    if let Some(env) = ENV_CB {
        if !env(
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            &mut fmt as *mut c_uint as *mut c_void,
        ) {
            eprintln!("[RustStation] frontend refused XRGB8888");
            return false;
        }
    }

    PSX = Some(psx);
    true
}

#[no_mangle]
pub unsafe extern "C" fn retro_load_game_special(
    _game_type: c_uint,
    _info: *const GameInfo,
    _num: usize,
) -> bool {
    false
}

#[no_mangle]
pub unsafe extern "C" fn retro_unload_game() {
    PSX = None;
}

#[no_mangle]
pub extern "C" fn retro_get_region() -> c_uint {
    RETRO_REGION_NTSC
}

/// Ask the frontend where its system directory is and look for a BIOS in it.
unsafe fn load_bios() -> Option<Vec<u8>> {
    let env = ENV_CB?;
    let mut dir: *const c_char = ptr::null();
    if !env(
        RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY,
        &mut dir as *mut *const c_char as *mut c_void,
    ) || dir.is_null()
    {
        return None;
    }

    let base = PathBuf::from(CStr::from_ptr(dir).to_string_lossy().into_owned());
    for name in BIOS_CANDIDATES {
        // Case varies between dumps; try both, since Linux and Android will not
        // do it for us.
        for candidate in [base.join(name), base.join(name.to_uppercase())] {
            if let Ok(image) = std::fs::read(&candidate) {
                if image.len() == psx_core::bus::BIOS_SIZE {
                    return Some(image);
                }
                eprintln!(
                    "[RustStation] ignoring {}: {} bytes, expected {}",
                    candidate.display(),
                    image.len(),
                    psx_core::bus::BIOS_SIZE
                );
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Save states
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn retro_serialize_size() -> usize {
    // Constant for a given format version, and equal on every platform. The
    // netplay handshake compares it across peers.
    Psx::state_size()
}

#[no_mangle]
pub unsafe extern "C" fn retro_serialize(data: *mut c_void, size: usize) -> bool {
    let Some(psx) = psx_ref() else {
        return false;
    };
    if data.is_null() || size < Psx::state_size() {
        return false;
    }
    let state = psx.save_state();
    ptr::copy_nonoverlapping(state.as_ptr(), data as *mut u8, state.len());
    true
}

#[no_mangle]
pub unsafe extern "C" fn retro_unserialize(data: *const c_void, size: usize) -> bool {
    let Some(psx) = psx_mut() else {
        return false;
    };
    if data.is_null() {
        return false;
    }
    let bytes = std::slice::from_raw_parts(data as *const u8, size);
    psx.load_state(bytes)
}

// ---------------------------------------------------------------------------
// Memory interface (RetroAchievements)
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro_get_memory_data(id: c_uint) -> *mut c_void {
    match id {
        RETRO_MEMORY_SYSTEM_RAM => match psx_mut() {
            Some(psx) => psx.bus.ram.as_mut_ptr() as *mut c_void,
            None => ptr::null_mut(),
        },
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro_get_memory_size(id: c_uint) -> usize {
    match id {
        RETRO_MEMORY_SYSTEM_RAM => psx_core::bus::RAM_SIZE,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Unimplemented ABI surface
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn retro_cheat_reset() {}

#[no_mangle]
pub unsafe extern "C" fn retro_cheat_set(_index: c_uint, _enabled: bool, _code: *const c_char) {}

/// The netplay handshake's state-identity token, as a C string. Not part of the
/// libretro ABI. The Trophy Hub host looks it up by symbol.
#[no_mangle]
pub unsafe extern "C" fn ruststation_state_token() -> *const c_char {
    // Leaked once, deliberately: the pointer has to outlive the call and the
    // string is a handful of bytes for the lifetime of the process.
    static mut TOKEN: *const c_char = ptr::null();
    if TOKEN.is_null() {
        let s = format!(
            "{}:{}:{}",
            psx_core::CORE_ID,
            save::FORMAT_VERSION,
            Psx::state_size()
        );
        TOKEN = CString::new(s).expect("no interior NUL").into_raw();
    }
    TOKEN
}
