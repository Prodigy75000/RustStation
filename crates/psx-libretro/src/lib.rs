// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! libretro C ABI front-end for RustStation.
//!
//! Content is a **disc image** (a cue sheet, or a raw image) or a PSX-EXE, and
//! either arrives as a path: `need_fullpath` is set because a cue sheet names
//! other files beside it and a frontend that reads content into a buffer for us
//! would hand over the sheet's text and nothing it points at.
//!
//! Both pads are read every frame from the frontend's RetroPad. **There is no
//! audio**: `retro_run` emits silence, which it must still do, because a
//! frontend starved of audio stalls its own frame pacing.
//!
//! Two surfaces here exist ahead of anything needing them, on purpose. The
//! state transfer (`retro_serialize` / `retro_unserialize`) is what the netplay
//! handshake negotiates against, and the RetroAchievements memory interface
//! needs system RAM published from day one: a core that never publishes it
//! hangs the achievement runtime on "waiting for core memory map".

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
const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;

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
/// What the frontend will offer as content. Disc images first: they are the
/// point, and a core that does not list them cannot be handed one however well
/// it would cope. `exe` and `psexe` stay for the conformance suites, which ship
/// as PSX-EXEs.
const VALID_EXTENSIONS: &[u8] = b"cue|bin|img|iso|exe|psexe\0";

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
    // **A cue sheet names other files beside it**, so the core has to be given
    // a path and open them itself. A frontend that reads the content into a
    // buffer for us hands over the cue sheet's text and nothing it points at.
    // The same flag then means a PSX-EXE arrives as a path too, which
    // `retro_load_game` reads for itself.
    (*info).need_fullpath = true;
    // A disc image must not be unpacked into memory behind our back either.
    (*info).block_extract = true;
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

/// Indexed by `RETRO_DEVICE_ID_JOYPAD_*`, which run 0..16 with no gaps, so the
/// id is the index and there is no pair to get out of step.
///
/// The ids are **not** in the order the button names suggest: `B` is 0, `Y` is
/// 1 and `SELECT` is 2, so a table written from the names alone puts Select on
/// the south face button and Square on Select. That is what this one did.
const PAD_MAP: [u16; 16] = [
    button::CROSS,    //  0 B, the south face button
    button::SQUARE,   //  1 Y, west
    button::SELECT,   //  2 SELECT
    button::START,    //  3 START
    button::UP,       //  4
    button::DOWN,     //  5
    button::LEFT,     //  6
    button::RIGHT,    //  7
    button::CIRCLE,   //  8 A, east
    button::TRIANGLE, //  9 X, north
    button::L1,       // 10 L
    button::R1,       // 11 R
    button::L2,       // 12
    button::R2,       // 13
    button::L3,       // 14
    button::R3,       // 15
];

/// Read both ports from the frontend into the emulated pads.
unsafe fn poll_pads(psx: &mut Psx) {
    let Some(state) = INPUT_STATE_CB else { return };
    for port in 0..2u32 {
        let mut held = 0u16;
        for (id, bit) in PAD_MAP.iter().enumerate() {
            if state(port, RETRO_DEVICE_JOYPAD, 0, id as c_uint) != 0 {
                held |= 1 << bit;
            }
        }
        psx.bus.sio.pads[port as usize].buttons = held;
    }
}

/// Tell the frontend what each button does, so it can label its remapper and
/// its on-screen overlay. Purely cosmetic, and the first thing anyone looks at
/// when the pad feels wrong, so it has to agree with `PAD_MAP` exactly.
const INPUT_DESCRIPTORS: [(c_uint, c_uint, &[u8]); 16] = [
    (0, 0, b"Cross\0"),
    (0, 1, b"Square\0"),
    (0, 2, b"Select\0"),
    (0, 3, b"Start\0"),
    (0, 4, b"D-Pad Up\0"),
    (0, 5, b"D-Pad Down\0"),
    (0, 6, b"D-Pad Left\0"),
    (0, 7, b"D-Pad Right\0"),
    (0, 8, b"Circle\0"),
    (0, 9, b"Triangle\0"),
    (0, 10, b"L1\0"),
    (0, 11, b"R1\0"),
    (0, 12, b"L2\0"),
    (0, 13, b"R2\0"),
    (0, 14, b"L3\0"),
    (0, 15, b"R3\0"),
];

#[repr(C)]
struct InputDescriptor {
    port: c_uint,
    device: c_uint,
    index: c_uint,
    id: c_uint,
    description: *const c_char,
}

unsafe fn publish_input_descriptors() {
    let Some(env) = ENV_CB else { return };
    // Both ports, then the terminating all-zero entry the frontend scans for.
    let mut descs: Vec<InputDescriptor> = Vec::with_capacity(INPUT_DESCRIPTORS.len() * 2 + 1);
    for port in 0..2u32 {
        for (_, id, text) in INPUT_DESCRIPTORS {
            descs.push(InputDescriptor {
                port,
                device: RETRO_DEVICE_JOYPAD,
                index: 0,
                id,
                description: text.as_ptr() as *const c_char,
            });
        }
    }
    descs.push(InputDescriptor {
        port: 0,
        device: 0,
        index: 0,
        id: 0,
        description: ptr::null(),
    });
    env(
        RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS,
        descs.as_ptr() as *mut c_void,
    );
    // The frontend copies what it needs during the call, but it is entitled to
    // keep the pointer, and the strings are 'static either way. Leaking the
    // array once is cheaper than owning it for the process lifetime.
    std::mem::forget(descs);
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
    if !info.is_null() && !load_content(&mut psx, &*info) {
        return false;
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

    publish_input_descriptors();
    PSX = Some(psx);
    true
}

/// Put the frontend's content into the machine: a disc in the drive, or a
/// PSX-EXE sideloaded at the shell hand-over.
///
/// Both arrive as a path, because `need_fullpath` is set for the cue sheet's
/// sake. The buffer is still honoured when a frontend supplies one anyway,
/// since that costs one branch and saves a confusing failure.
unsafe fn load_content(psx: &mut Psx, info: &GameInfo) -> bool {
    let path = if info.path.is_null() {
        None
    } else {
        CStr::from_ptr(info.path).to_str().ok()
    };

    let is_disc = path.is_some_and(|p| {
        let p = p.to_ascii_lowercase();
        [".cue", ".bin", ".iso", ".img"]
            .iter()
            .any(|ext| p.ends_with(ext))
    });

    if is_disc {
        return match Disc::open(std::path::Path::new(path.expect("checked above"))) {
            Ok(d) => {
                psx.bus.cdrom.disc = Some(d);
                true
            }
            Err(e) => {
                eprintln!("[RustStation] {e}");
                false
            }
        };
    }

    // Otherwise a PSX-EXE, from the buffer if there is one and from the path if
    // not.
    let owned;
    let image: &[u8] = if !info.data.is_null() && info.size > 0 {
        std::slice::from_raw_parts(info.data as *const u8, info.size)
    } else if let Some(p) = path {
        match std::fs::read(p) {
            Ok(bytes) => {
                owned = bytes;
                &owned
            }
            Err(e) => {
                eprintln!("[RustStation] cannot read {p}: {e}");
                return false;
            }
        }
    } else {
        eprintln!("[RustStation] content has neither a path nor data");
        return false;
    };

    match Exe::parse(image) {
        Ok(exe) => {
            psx.sideload_exe(exe);
            true
        }
        Err(e) => {
            eprintln!("[RustStation] {e}");
            false
        }
    }
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

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The name this core would print for a pad bit. Independent of both tables
    /// under test, so it can referee between them.
    fn name_of(bit: u16) -> &'static str {
        match bit {
            button::CROSS => "Cross",
            button::SQUARE => "Square",
            button::CIRCLE => "Circle",
            button::TRIANGLE => "Triangle",
            button::SELECT => "Select",
            button::START => "Start",
            button::UP => "D-Pad Up",
            button::DOWN => "D-Pad Down",
            button::LEFT => "D-Pad Left",
            button::RIGHT => "D-Pad Right",
            button::L1 => "L1",
            button::R1 => "R1",
            button::L2 => "L2",
            button::R2 => "R2",
            button::L3 => "L3",
            button::R3 => "R3",
            other => panic!("no name for pad bit {other}"),
        }
    }

    /// The RetroPad ids are not in the order their names suggest.
    ///
    /// `B` is 0, `Y` is 1 and `SELECT` is 2, so a table written from the names
    /// puts Select on the south face button and Square on Select. This core did
    /// exactly that, and it is asserted here rather than left to the comment
    /// because nothing about the wrong version looks wrong.
    #[test]
    fn the_retropad_ids_map_to_the_conventional_face_buttons() {
        assert_eq!(PAD_MAP[0], button::CROSS, "id 0 is B, the south button");
        assert_eq!(PAD_MAP[1], button::SQUARE, "id 1 is Y, the west button");
        assert_eq!(PAD_MAP[2], button::SELECT, "id 2 is SELECT, not a face button");
        assert_eq!(PAD_MAP[3], button::START);
        assert_eq!(PAD_MAP[8], button::CIRCLE, "id 8 is A, the east button");
        assert_eq!(PAD_MAP[9], button::TRIANGLE, "id 9 is X, the north button");
    }

    /// Every pad bit is driven by exactly one RetroPad id.
    ///
    /// A table of pairs can lose an entry or repeat one and still compile and
    /// still mostly work; a game only shows it when the one button nobody
    /// tested turns out to be the one that opens the menu.
    #[test]
    fn every_pad_bit_is_driven_exactly_once() {
        let mut seen = 0u16;
        for bit in PAD_MAP {
            assert!(bit < 16, "pad bit {bit} is out of range");
            assert_eq!(seen & (1 << bit), 0, "pad bit {bit} is driven twice");
            seen |= 1 << bit;
        }
        assert_eq!(seen, u16::MAX, "some pad bit is not reachable at all");
    }

    /// The labels the frontend shows agree with what the buttons actually do.
    ///
    /// These are two separate tables and nothing but this makes them agree. A
    /// wrong label is not a cosmetic problem: it is the first thing anyone
    /// consults when the pad feels wrong, so it sends the next reader the wrong
    /// way.
    #[test]
    fn the_input_descriptors_agree_with_the_map() {
        assert_eq!(INPUT_DESCRIPTORS.len(), PAD_MAP.len());
        for (index, (_, id, text)) in INPUT_DESCRIPTORS.iter().enumerate() {
            assert_eq!(*id as usize, index, "descriptors are in id order");
            let label = std::str::from_utf8(&text[..text.len() - 1]).expect("utf-8");
            assert_eq!(
                label,
                name_of(PAD_MAP[index]),
                "the label for RetroPad id {id} does not match what it drives"
            );
            assert_eq!(*text.last().expect("non-empty"), 0, "labels are NUL-terminated");
        }
    }

    /// The frontend has to be told a disc is acceptable content, and told to
    /// hand it over as a path.
    ///
    /// Disc loading was implemented and unreachable for exactly this reason:
    /// the extension list said `exe|psexe`, so nothing ever offered the core a
    /// cue sheet to refuse.
    #[test]
    fn a_disc_is_offerable_content_and_arrives_as_a_path() {
        let mut info = SystemInfo {
            library_name: ptr::null(),
            library_version: ptr::null(),
            valid_extensions: ptr::null(),
            need_fullpath: false,
            block_extract: false,
        };
        unsafe { retro_get_system_info(&mut info) };

        let exts = unsafe { CStr::from_ptr(info.valid_extensions) }
            .to_str()
            .expect("utf-8");
        for ext in ["cue", "bin", "exe"] {
            assert!(
                exts.split('|').any(|e| e == ext),
                "{ext} is missing from {exts:?}"
            );
        }
        assert!(info.need_fullpath, "a cue sheet names files beside it");
    }
}
