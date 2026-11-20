//! Stand-in for [`super::decode`] in a build without the `bink` feature: there is no
//! library to load, so videos export as `.bk2` only.

use std::path::{Path, PathBuf};

use super::bink::BinkHeader;

pub const BINK_DLL_ENV: &str = "WFSCAN_BINK_DLL";

#[derive(Debug, PartialEq, Eq)]
pub struct BinkLibrary;

impl BinkLibrary {
    pub fn load(_path: Option<&Path>) -> Result<Self, String> {
        Err("this wfscan was built without Bink support (rebuild with the `bink` feature)".into())
    }

    pub fn next_to_cache(_cache_dir: &Path) -> Option<PathBuf> {
        None
    }

    pub fn path(&self) -> &Path {
        Path::new("")
    }

    pub fn frames(&self, _file: &[u8], _header: &BinkHeader, _frame: impl FnMut(u32, &[u8]) -> Result<bool, String>) -> Result<(), String> {
        Err("this wfscan was built without Bink support".into())
    }
}
