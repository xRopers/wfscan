//! Decoding Bink 2 videos to RGBA frames with the user's own `bink2w64.dll`, loaded at
//! run time (the game ships one in `Warframe\Tools\Bink2`). wfscan never bundles it.
//!
//! The DLL's own exports are used: `BinkOpen(path, 0)`, then per frame `BinkDoFrame`,
//! `BinkCopyToBuffer(..., surface 6 = 32-bit RGBA | copy all)` and `BinkNextFrame`, then
//! `BinkClose`; `BinkGetError` says why an open failed. The handle starts with u32
//! width, height and frame count; they are checked against the file's own header before
//! any frame is decoded, so a library with another layout is refused instead of trusted.
//! Checked with bink2w64.dll 2023.08 ("Bink and Smacker") on the November 2026 cache:
//! an ability video decodes to all its frames with natural colours.
//!
//! `BinkOpen` takes a path, so the video is written to a temporary file first.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::bink::BinkHeader;

/// The environment variable naming the Bink library when none is given.
pub const BINK_DLL_ENV: &str = "WFSCAN_BINK_DLL";

/// Larger than any video the game has; a header past this is refused before decoding.
const MAX_SIDE: u32 = 8192;
const MAX_FRAMES: u32 = 1_000_000;

type Open = unsafe extern "system" fn(*const c_char, u32) -> *mut c_void;
type DoFrame = unsafe extern "system" fn(*mut c_void) -> i32;
type CopyToBuffer = unsafe extern "system" fn(*mut c_void, *mut c_void, i32, u32, u32, u32, u32) -> i32;
type NextFrame = unsafe extern "system" fn(*mut c_void);
type Close = unsafe extern "system" fn(*mut c_void);
type GetError = unsafe extern "system" fn() -> *const c_char;

/// `BinkCopyToBuffer` flags: 32-bit RGBA, and copy the whole frame.
const SURFACE_RGBA: u32 = 6;
const COPY_ALL: u32 = 0x8000_0000;

/// A loaded `bink2w64.dll`.
pub struct BinkLibrary {
    path: PathBuf,
    open: Open,
    do_frame: DoFrame,
    copy: CopyToBuffer,
    next: NextFrame,
    close: Close,
    get_error: GetError,
    // Keeps the functions above valid.
    _library: libloading::Library,
}

impl std::fmt::Debug for BinkLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinkLibrary").field("path", &self.path).finish()
    }
}

impl PartialEq for BinkLibrary {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for BinkLibrary {}

impl BinkLibrary {
    /// Load the library at `path`, else at `$WFSCAN_BINK_DLL`.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => std::env::var_os(BINK_DLL_ENV).filter(|v| !v.is_empty()).map(PathBuf::from).ok_or_else(|| {
                format!("no Bink library given (--bink-dll PATH or {BINK_DLL_ENV}); the game has one at Warframe\\Tools\\Bink2\\bink2w64.dll")
            })?,
        };
        if !path.is_file() {
            return Err(format!("Bink library {} not found", path.display()));
        }
        // SAFETY: loading a library runs its initialisation; this is the user's own copy
        // of the Bink DLL, loaded on purpose.
        let library = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("loading {}: {e}", path.display()))?;
        let get = |name: &[u8]| -> Result<*const c_void, String> {
            // SAFETY: only the address is taken here; it is given a type below.
            unsafe { library.get::<*const c_void>(name) }.map(|s| *s).map_err(|_| {
                format!("{} has no {} (is it bink2w64.dll?)", path.display(), String::from_utf8_lossy(&name[..name.len() - 1]))
            })
        };
        // SAFETY: these are the Bink 2 SDK's exports with these signatures; a library
        // laid out differently fails the size check on the first video.
        unsafe {
            Ok(Self {
                open: std::mem::transmute::<*const c_void, Open>(get(b"BinkOpen\0")?),
                do_frame: std::mem::transmute::<*const c_void, DoFrame>(get(b"BinkDoFrame\0")?),
                copy: std::mem::transmute::<*const c_void, CopyToBuffer>(get(b"BinkCopyToBuffer\0")?),
                next: std::mem::transmute::<*const c_void, NextFrame>(get(b"BinkNextFrame\0")?),
                close: std::mem::transmute::<*const c_void, Close>(get(b"BinkClose\0")?),
                get_error: std::mem::transmute::<*const c_void, GetError>(get(b"BinkGetError\0")?),
                path: path.clone(),
                _library: library,
            })
        }
    }

    /// The game's own copy, next to a `Cache.Windows` folder: `..\Tools\Bink2\bink2w64.dll`.
    pub fn next_to_cache(cache_dir: &Path) -> Option<PathBuf> {
        let path = cache_dir.parent()?.join("Tools").join("Bink2").join("bink2w64.dll");
        path.is_file().then_some(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decode the frames of a Bink 2 file, handing each to `frame` as 8-bit RGBA, rows top
    /// to bottom, `width * height * 4` bytes. `frame` returns false to stop early.
    pub fn frames(&self, file: &[u8], header: &BinkHeader, mut frame: impl FnMut(u32, &[u8]) -> Result<bool, String>) -> Result<(), String> {
        if !header.is_bink2() {
            return Err("a Bink 1 video: bink2w64.dll only decodes Bink 2 (FFmpeg reads Bink 1)".into());
        }
        if header.width == 0 || header.height == 0 || header.width > MAX_SIDE || header.height > MAX_SIDE || header.frames == 0 || header.frames > MAX_FRAMES {
            return Err(format!("{}x{} with {} frames is not a size wfscan decodes", header.width, header.height, header.frames));
        }
        let temp = TempFile::new(file).map_err(|e| format!("writing a temporary copy for the Bink library: {e}"))?;
        let name = CString::new(temp.path().to_string_lossy().into_owned()).map_err(|_| "the temporary path has a NUL".to_string())?;
        // SAFETY: the path is a NUL-terminated string that lives through the call.
        let handle = unsafe { (self.open)(name.as_ptr(), 0) };
        if handle.is_null() {
            // SAFETY: BinkGetError returns a NUL-terminated string the library owns.
            let why = unsafe { (self.get_error)() };
            let why = if why.is_null() { "no reason given".into() } else { unsafe { CStr::from_ptr(why) }.to_string_lossy().into_owned() };
            return Err(format!("the Bink library couldn't open the video: {why}"));
        }
        let handle = Handle { handle, close: self.close };
        // SAFETY: an open handle points at the library's BINK struct, which starts with
        // u32 width, height and frame count.
        let opened = unsafe { std::slice::from_raw_parts(handle.handle as *const u32, 3) }.to_vec();
        if opened != [header.width, header.height, header.frames] {
            return Err(format!(
                "the Bink library reports {}x{} with {} frames, the file says {}x{} with {} (an unexpected library version?)",
                opened[0], opened[1], opened[2], header.width, header.height, header.frames
            ));
        }
        let pitch = header.width as usize * 4;
        let mut rgba = vec![0u8; pitch * header.height as usize];
        for index in 0..header.frames {
            // SAFETY: the buffer is exactly pitch x height bytes, as the copy is told.
            let copied = unsafe {
                (self.do_frame)(handle.handle);
                let copied = (self.copy)(handle.handle, rgba.as_mut_ptr().cast(), pitch as i32, header.height, 0, 0, SURFACE_RGBA | COPY_ALL);
                (self.next)(handle.handle);
                copied
            };
            if copied != 0 {
                return Err(format!("the Bink library couldn't copy frame {index} (it returned {copied})"));
            }
            if !frame(index, &rgba)? {
                break;
            }
        }
        Ok(())
    }
}

/// Closes a Bink handle however decoding ends.
struct Handle {
    handle: *mut c_void,
    close: Close,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from BinkOpen and is closed once.
        unsafe { (self.close)(self.handle) }
    }
}

/// A file in the temporary folder that is deleted when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(data: &[u8]) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("wfscan-{}-{}.bk2", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, data)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_reported() {
        let e = BinkLibrary::load(Some(Path::new("no/such/bink2w64.dll"))).unwrap_err();
        assert!(e.contains("not found"), "{e}");
        let cache = std::env::temp_dir().join("wfscan-test-no-game").join("Cache.Windows");
        assert_eq!(BinkLibrary::next_to_cache(&cache), None);
    }

    #[test]
    fn temporary_copies_are_removed() {
        let path = {
            let temp = TempFile::new(b"KB2j").unwrap();
            assert_eq!(std::fs::read(temp.path()).unwrap(), b"KB2j");
            temp.path().to_path_buf()
        };
        assert!(!path.exists());
    }
}
