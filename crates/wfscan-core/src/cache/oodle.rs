//! Oodle (cargo feature `oodle`, on by default), decoded by the user's own oo2core
//! library through zscan-core's loader. wfscan never includes the library, and Warframe
//! doesn't ship one as a file, so the user points at a copy from another game or SDK:
//! `--oodle-dll`, `WFSCAN_OODLE_DLL`, or zscan's `ZSCAN_OODLE_DLL`.
//!
//! [`Oodle`] exists in every build, so a build without the feature can say what's missing.

use std::path::Path;

use super::block::OodleDecoder;

/// Environment variable naming the oo2core library, when no path is given.
pub const OODLE_DLL_ENV: &str = "WFSCAN_OODLE_DLL";

/// What to tell a user who hasn't said where their oo2core library is.
#[cfg(feature = "oodle")]
const NO_OODLE_DLL: &str = "no Oodle library given. wfscan doesn't include Oodle and Warframe doesn't ship it as a \
    file: pass --oodle-dll <path to oo2core_*.dll> or set WFSCAN_OODLE_DLL, using a copy from another game or SDK";

/// A loaded oo2core library.
pub struct Oodle {
    #[cfg(feature = "oodle")]
    codec: &'static dyn zscan_core::Codec,
}

impl Oodle {
    /// Load the library at `path`, else the one `WFSCAN_OODLE_DLL` or `ZSCAN_OODLE_DLL` names.
    #[cfg(feature = "oodle")]
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let from_env = || std::env::var_os(OODLE_DLL_ENV).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
        if let Some(path) = path.map(Path::to_path_buf).or_else(from_env) {
            zscan_core::set_oodle_dll(Some(path));
        }
        if zscan_core::plugins::oodle_dll_path().is_none() {
            return Err(NO_OODLE_DLL.into());
        }
        let codec = zscan_core::codec_for(zscan_core::Format::Oodle).map_err(|e| e.reason)?;
        Ok(Self { codec })
    }

    #[cfg(not(feature = "oodle"))]
    pub fn load(_path: Option<&Path>) -> Result<Self, String> {
        Err("this wfscan was built without Oodle support (rebuild with the `oodle` feature)".into())
    }
}

#[cfg(feature = "oodle")]
impl OodleDecoder for Oodle {
    fn decode(&self, comp: &[u8], out: &mut [u8]) -> Result<(), String> {
        use std::cell::RefCell;
        use zscan_core::{DecodeCtx, SizeHint};

        /// Block sizes are 24-bit fields.
        const MAX_BLOCK: usize = 1 << 24;
        thread_local! {
            static CTX: RefCell<DecodeCtx> = RefCell::new(DecodeCtx::new(MAX_BLOCK));
        }
        let decoded = CTX
            .with_borrow_mut(|ctx| self.codec.decode_with_hint(comp, SizeHint::exact(comp.len(), out.len()), ctx))
            .map_err(|e| e.to_string())?;
        out.copy_from_slice(&decoded.data);
        Ok(())
    }
}

#[cfg(not(feature = "oodle"))]
impl OodleDecoder for Oodle {
    fn decode(&self, _comp: &[u8], _out: &mut [u8]) -> Result<(), String> {
        unreachable!("an Oodle can't be loaded in a build without the `oodle` feature")
    }
}
