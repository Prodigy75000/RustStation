// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! Multi-disc games: `.m3u` playlists and the libretro disk-control interface.
//!
//! A playlist is one disc image per line, resolved against the playlist's own
//! directory; blank lines and `#` comments are skipped. Frontends that
//! synthesise one write exactly that, every disc staged flat beside the
//! playlist and listed by bare name.
//!
//! A swap is the frontend opening the lid, choosing an image and closing it.
//! Some frontends do all three in one instant, with no frame between, so
//! the drive has to make the change visible by itself. It does: the shell-open
//! status bit stays latched until software next reads the status, which is how
//! a game waiting on "insert disc 2" sees that something happened. See
//! `Cdrom::open_lid`.
//!
//! Which disc is in the drive is frontend state, like the disc image itself,
//! and is not in a save state. The lid is.

use std::ffi::{c_char, c_uint, CStr};
use std::path::{Path, PathBuf};

use psx_core::disc::Disc;

use crate::GameInfo;

/// Is this content path a playlist?
pub fn is_playlist(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".m3u")
}

/// The discs a playlist names, in order, each resolved against the directory
/// the playlist is in unless it is already absolute.
pub fn parse_playlist(text: &str, base: &Path) -> Vec<PathBuf> {
    text.lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let p = Path::new(l);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            }
        })
        .collect()
}

/// The images a game was loaded with, and the drive's view of them.
pub struct Discs {
    pub paths: Vec<PathBuf>,
    /// The image in the drive; `paths.len()` means none.
    pub index: usize,
    pub ejected: bool,
}

impl Discs {
    /// The images for a content path: the playlist's entries, or just itself.
    pub fn for_content(path: &str) -> Result<Discs, String> {
        let paths = if is_playlist(path) {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read playlist {path}: {e}"))?;
            let base = Path::new(path).parent().unwrap_or(Path::new(""));
            let paths = parse_playlist(&text, base);
            if paths.is_empty() {
                return Err(format!("playlist {path} names no discs"));
            }
            paths
        } else {
            vec![PathBuf::from(path)]
        };
        Ok(Discs {
            paths,
            index: 0,
            ejected: false,
        })
    }

    /// Open the image at `index`, or nothing for the empty slot past the end.
    pub fn open(&self, index: usize) -> Result<Option<Disc>, String> {
        match self.paths.get(index) {
            None => Ok(None),
            Some(p) if p.as_os_str().is_empty() => Ok(None),
            Some(p) => Disc::open(p).map(Some).map_err(|e| e.to_string()),
        }
    }
}

pub(crate) static mut DISCS: Option<Discs> = None;

/// A frontend-requested first disc, from `set_initial_image`, applied at load
/// if the path still matches.
pub(crate) static mut INITIAL: Option<(usize, String)> = None;

unsafe fn discs() -> Option<&'static mut Discs> {
    (*std::ptr::addr_of_mut!(DISCS)).as_mut()
}

// ---------------------------------------------------------------------------
// retro_disk_control_ext_callback
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct DiskControlExt {
    pub set_eject_state: unsafe extern "C" fn(bool) -> bool,
    pub get_eject_state: unsafe extern "C" fn() -> bool,
    pub get_image_index: unsafe extern "C" fn() -> c_uint,
    pub set_image_index: unsafe extern "C" fn(c_uint) -> bool,
    pub get_num_images: unsafe extern "C" fn() -> c_uint,
    pub replace_image_index: unsafe extern "C" fn(c_uint, *const GameInfo) -> bool,
    pub add_image_index: unsafe extern "C" fn() -> bool,
    pub set_initial_image: unsafe extern "C" fn(c_uint, *const c_char) -> bool,
    pub get_image_path: unsafe extern "C" fn(c_uint, *mut c_char, usize) -> bool,
    pub get_image_label: unsafe extern "C" fn(c_uint, *mut c_char, usize) -> bool,
}

pub static CALLBACKS: DiskControlExt = DiskControlExt {
    set_eject_state,
    get_eject_state,
    get_image_index,
    set_image_index,
    get_num_images,
    replace_image_index,
    add_image_index,
    set_initial_image,
    get_image_path,
    get_image_label,
};

unsafe extern "C" fn set_eject_state(ejected: bool) -> bool {
    let Some(d) = discs() else {
        return false;
    };
    if let Some(psx) = crate::psx_mut() {
        if ejected {
            psx.bus.cdrom.open_lid();
        } else {
            psx.bus.cdrom.close_lid();
        }
    }
    d.ejected = ejected;
    info!("disc lid {}", if ejected { "opened" } else { "closed" });
    true
}

unsafe extern "C" fn get_eject_state() -> bool {
    discs().is_some_and(|d| d.ejected)
}

unsafe extern "C" fn get_image_index() -> c_uint {
    discs().map_or(0, |d| d.index as c_uint)
}

/// Change the disc in the drive. Only with the lid open, as on the console.
unsafe extern "C" fn set_image_index(index: c_uint) -> bool {
    let Some(d) = discs() else {
        return false;
    };
    let index = index as usize;
    if !d.ejected || index > d.paths.len() {
        return false;
    }
    let disc = match d.open(index) {
        Ok(disc) => disc,
        Err(e) => {
            error!("cannot open disc {}: {e}", index + 1);
            return false;
        }
    };
    if let Some(psx) = crate::psx_mut() {
        psx.bus.cdrom.disc = disc;
    }
    d.index = index;
    info!("disc {} of {} in the drive", index + 1, d.paths.len());
    true
}

unsafe extern "C" fn get_num_images() -> c_uint {
    discs().map_or(0, |d| d.paths.len() as c_uint)
}

/// Replace an image's path, or with no info remove it from the list.
unsafe extern "C" fn replace_image_index(index: c_uint, info: *const GameInfo) -> bool {
    let Some(d) = discs() else {
        return false;
    };
    let index = index as usize;
    if index >= d.paths.len() {
        return false;
    }
    if info.is_null() {
        d.paths.remove(index);
        if d.index > index || d.index > d.paths.len() {
            d.index -= 1;
        }
        return true;
    }
    if (*info).path.is_null() {
        return false;
    }
    d.paths[index] = PathBuf::from(CStr::from_ptr((*info).path).to_string_lossy().into_owned());
    true
}

unsafe extern "C" fn add_image_index() -> bool {
    let Some(d) = discs() else {
        return false;
    };
    d.paths.push(PathBuf::new());
    true
}

unsafe extern "C" fn set_initial_image(index: c_uint, path: *const c_char) -> bool {
    if path.is_null() {
        return false;
    }
    let path = CStr::from_ptr(path).to_string_lossy().into_owned();
    *std::ptr::addr_of_mut!(INITIAL) = Some((index as usize, path));
    true
}

unsafe fn copy_out(text: &str, out: *mut c_char, len: usize) -> bool {
    if out.is_null() || len == 0 {
        return false;
    }
    let bytes = text.as_bytes();
    let n = bytes.len().min(len - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    true
}

unsafe extern "C" fn get_image_path(index: c_uint, out: *mut c_char, len: usize) -> bool {
    let Some(p) = discs().and_then(|d| d.paths.get(index as usize)) else {
        return false;
    };
    !p.as_os_str().is_empty() && copy_out(&p.to_string_lossy(), out, len)
}

/// The file name without its extension, which for a dump set is the title and
/// disc number, e.g. "Final Fantasy VIII (USA) (Disc 2)".
unsafe extern "C" fn get_image_label(index: c_uint, out: *mut c_char, len: usize) -> bool {
    let Some(p) = discs().and_then(|d| d.paths.get(index as usize)) else {
        return false;
    };
    let Some(stem) = p.file_stem() else {
        return false;
    };
    copy_out(&stem.to_string_lossy(), out, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_resolves_beside_itself_and_skips_the_rest() {
        let base = Path::new("cache");
        let text = "\u{feff}# made by the app\r\nFF8 (Disc 1).cue\r\n\r\n  FF8 (Disc 2).cue  \n#FF8 (Disc 3).cue\nsub/FF8 (Disc 4).cue\n";
        let got = parse_playlist(text, base);
        assert_eq!(
            got,
            vec![
                base.join("FF8 (Disc 1).cue"),
                base.join("FF8 (Disc 2).cue"),
                base.join("sub/FF8 (Disc 4).cue"),
            ]
        );
    }

    #[test]
    fn an_absolute_entry_is_kept_as_it_is() {
        let abs = if cfg!(windows) {
            "C:\\games\\mgs1.cue"
        } else {
            "/games/mgs1.cue"
        };
        let got = parse_playlist(abs, Path::new("elsewhere"));
        assert_eq!(got, vec![PathBuf::from(abs)]);
    }

    #[test]
    fn only_m3u_is_a_playlist() {
        assert!(is_playlist("x/current_rom.M3U"));
        assert!(!is_playlist("x/game.cue"));
        assert!(!is_playlist("x/m3u.cue"));
    }
}
