use std::io;
use std::path::{Path, PathBuf};

use crate::cache::block::BlockError;
use crate::cache::package::Game;
use crate::cache::toc::TocError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },

    #[error("{}: {source}", path.display())]
    Toc { path: PathBuf, source: TocError },

    #[error("{}: no .toc files here; point at the game's Cache.Windows folder", .0.display())]
    NoPackages(PathBuf),

    #[error("this cache is from {0}, which wfscan can't read yet")]
    UnsupportedGame(Game),

    #[error("{path} in {}: the entry lies outside the cache file", cache.display())]
    OutOfRange { path: String, cache: PathBuf },

    #[error("{path}: {source}")]
    Block { path: String, source: BlockError },

    #[error("{0}")]
    NotFound(String),

    #[error("the cache no longer matches the manifest ({0}); run `wfscan index` again")]
    StaleManifest(String),

    #[error("{} is open for writing in another program (the game or its launcher?); close it first", .0.display())]
    InUse(PathBuf),

    #[error("unsafe internal path {0:?}")]
    BadPath(String),

    #[error("invalid manifest: {0}")]
    ManifestParse(#[from] serde_json::Error),

    #[error("manifest version {found} is not supported (this build reads version {expected})")]
    ManifestVersion { found: u32, expected: u32 },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_path_buf(), source }
}
