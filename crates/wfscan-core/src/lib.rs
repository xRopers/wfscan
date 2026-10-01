//! wfscan-core: read Warframe's `Cache.Windows` packages, index them into a manifest and
//! (later) export their assets. Read-only: nothing here writes to a cache.
//!
//! - [`cache`]: `.toc` and `.cache` files, block decompression, the common header.
//! - [`index`]: walk a cache into a [`Manifest`].
//! - [`manifest`]: the JSON record of every file.

pub mod cache;
pub mod diff;
pub mod error;
pub mod export;
pub mod formats;
pub mod index;
pub mod manifest;
pub mod output;
pub mod report;
pub mod types;

pub use cache::block::{BlockCounts, BlockError, OodleDecoder};
pub use cache::header::{CommonHeader, HeaderError};
pub use cache::oodle::{OODLE_DLL_ENV, Oodle};
pub use cache::package::{Cache, Category, Game, Package, Part, PartFile};
pub use cache::toc::{Toc, TocEntry, TocError};
pub use diff::{Diff, DiffRow, diff};
pub use error::{Error, Result};
pub use formats::FormatOptions;
pub use formats::audio::{AudioOutput, VorbisTables};
pub use formats::texture::TextureOutput;
pub use export::{ExportError, ExportInput, ExportOptions, ExportStats, Exporter, Exporters, Output, export, with_input};
pub use index::{IndexOptions, IndexStats, index, package_info};
pub use manifest::{
    Classifier, ExportRecord, FileEntry, HeaderEntry, MANIFEST_VERSION, Manifest, PackageInfo, PartEntry, SourceInfo, Status,
};
pub use report::{ErrorRow, Report, TypeRow, report};
pub use types::{Kind, kind_of};
