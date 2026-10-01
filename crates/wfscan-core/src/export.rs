//! The export driver: run the right exporter over each selected file, in parallel, and
//! record how every one of them ended.
//!
//! - One bad file never stops a run. An exporter's error, or a panic inside it, becomes a
//!   `failed` record for that file.
//! - A file nothing can export is `unsupported`, with what is missing as the reason.
//! - A file whose earlier export still stands is `skipped`: same contents, same exporter
//!   version, outputs still on disk. "Earlier" is this manifest's own record, or the
//!   record in the manifest given as `previous` (`--changed-since`) for files that
//!   haven't changed between the two.
//!
//! The cache is only read. Outputs go to a temporary name and are renamed into place.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::cache::block::OodleDecoder;
use crate::cache::header::{self, CommonHeader};
use crate::cache::package::{Cache, Category, Game, Package, Part};
use crate::cache::toc::TocEntry;
use crate::error::{Error, Result};
use crate::index::package_info;
use crate::manifest::{ExportRecord, FileEntry, Manifest, Status};
use crate::output::{internal_path_under, write_atomic};
use crate::types::Kind;

/// Why an exporter produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// The file is of a variant the exporter doesn't handle (yet).
    Unsupported(String),
    /// The file should have exported and didn't.
    Failed(String),
}

impl ExportError {
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed(reason.into())
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported(reason.into())
    }
}

/// One file an exporter wants written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Where it goes under the output folder, as an internal-style path: usually the
    /// source's own path with a new extension, such as `/Lotus/A/B.png`.
    pub path: String,
    pub data: Vec<u8>,
}

/// What an exporter gets to work with: one file's header and H data, and its B and F
/// parts on request.
pub struct ExportInput<'a> {
    pub game: Game,
    pub category: Category,
    pub package: &'a str,
    pub path: &'a str,
    pub header: &'a CommonHeader,
    /// The whole H part, common header included.
    pub h: &'a [u8],
    source: &'a Package,
    parts: [Option<&'a TocEntry>; 3],
    oodle: Option<&'a dyn OodleDecoder>,
}

impl ExportInput<'_> {
    pub fn type_id(&self) -> u32 {
        self.header.type_id
    }

    /// The H part after the common header: the type's own header.
    pub fn h_body(&self) -> &[u8] {
        &self.h[self.header.len..]
    }

    pub fn has(&self, part: Part) -> bool {
        self.parts[part as usize].is_some()
    }

    /// The decompressed B or F part.
    pub fn read(&self, part: Part) -> Result<Vec<u8>, ExportError> {
        let entry = self.parts[part as usize].ok_or_else(|| ExportError::failed(format!("the file has no {part} part")))?;
        let file = self.source.part(part).expect("a listed part exists");
        file.read(entry, self.oodle).map(|(data, _)| data).map_err(|e| ExportError::failed(format!("{part}: {}", cause(&e))))
    }
}

/// An error without the path it names, for a record that already has the path.
fn cause(e: &Error) -> String {
    match e {
        Error::Block { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// Turns one kind of asset into standard files.
pub trait Exporter: Send + Sync {
    /// Short name, such as `texture`.
    fn name(&self) -> &'static str;
    /// Raise this when the output changes, so files exported by an older version are
    /// exported again instead of skipped.
    fn version(&self) -> u32;
    fn kind(&self) -> Kind;
    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError>;
}

/// The exporters a run can use, at most one per kind.
#[derive(Default)]
pub struct Exporters {
    list: Vec<Box<dyn Exporter>>,
}

impl Exporters {
    /// Add an exporter, replacing any other for the same kind.
    pub fn with(mut self, exporter: impl Exporter + 'static) -> Self {
        self.list.retain(|e| e.kind() != exporter.kind());
        self.list.push(Box::new(exporter));
        self
    }

    pub fn for_kind(&self, kind: Kind) -> Option<&dyn Exporter> {
        self.list.iter().find(|e| e.kind() == kind).map(|e| &**e)
    }

    pub fn kinds(&self) -> Vec<Kind> {
        self.list.iter().map(|e| e.kind()).collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Only this package.
    pub package: Option<String>,
    /// Only paths starting with this.
    pub path: Option<String>,
    /// Only these kinds; empty means every file.
    pub kinds: Vec<Kind>,
    /// Run the exporters and record the results, but write no files.
    pub dry_run: bool,
    /// Export again even where an earlier export still stands.
    pub force: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExportStats {
    /// Files the options selected; the four statuses add up to this.
    pub selected: u64,
    pub ok: u64,
    pub failed: u64,
    pub unsupported: u64,
    pub skipped: u64,
    /// Output files written (or, in a dry run, that would have been), and their bytes.
    pub outputs: u64,
    pub bytes: u64,
    #[serde(serialize_with = "seconds")]
    pub elapsed: Duration,
}

fn seconds<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

struct Run<'a> {
    cache: &'a Cache,
    exporters: &'a Exporters,
    oodle: Option<&'a dyn OodleDecoder>,
    out_dir: &'a Path,
    opts: &'a ExportOptions,
}

fn record(status: Status, exporter: Option<String>, reason: impl Into<String>) -> ExportRecord {
    ExportRecord { status, exporter, reason: Some(reason.into()), outputs: Vec::new() }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    let text = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned());
    format!("the exporter panicked: {}", text.as_deref().unwrap_or("(no message)"))
}

impl Run<'_> {
    /// Whether every output of an earlier export is still in the output folder.
    fn outputs_exist(&self, outputs: &[String]) -> bool {
        !outputs.is_empty() && outputs.iter().all(|o| internal_path_under(self.out_dir, o).is_ok_and(|p| p.is_file()))
    }

    /// Export one file. Returns its record and the bytes of output.
    fn export_one(&self, file: &FileEntry, category: Category, kind: Option<Kind>, earlier: Option<&ExportRecord>) -> (ExportRecord, u64) {
        let Some(header_entry) = &file.header else {
            return (record(Status::Unsupported, None, "no common header"), 0);
        };
        let Some(kind) = kind else {
            return (record(Status::Unsupported, None, format!("no reader for type {}", header_entry.type_id)), 0);
        };
        let Some(exporter) = self.exporters.for_kind(kind) else {
            return (record(Status::Unsupported, None, format!("no {kind} exporter yet")), 0);
        };
        let tag = Some(format!("{}@{}", exporter.name(), exporter.version()));

        if !self.opts.force
            && let Some(earlier) = earlier
            && matches!(earlier.status, Status::Ok | Status::Skipped)
            && earlier.exporter == tag
            && self.outputs_exist(&earlier.outputs)
        {
            return (ExportRecord { status: Status::Skipped, exporter: tag, reason: None, outputs: earlier.outputs.clone() }, 0);
        }

        let failed = |reason: String| (record(Status::Failed, tag.clone(), reason), 0);
        let package = self.cache.package(&file.package).expect("the manifest's packages were checked against the cache");
        let mut parts = [None; 3];
        for part in Part::ALL {
            let Some(listed) = file.part(part) else { continue };
            let found = package.part(part).and_then(|f| f.find(&file.path));
            match found {
                Some(e) if (e.offset, e.compressed_size, e.size) == (listed.offset, listed.compressed_size, listed.size) => {
                    parts[part as usize] = Some(e);
                }
                _ => return failed(format!("{part}: the cache no longer matches the manifest")),
            }
        }
        let Some(h_entry) = parts[Part::H as usize] else {
            return failed("the file has no H part".into());
        };
        let h = match package.part(Part::H).expect("a listed part exists").read(h_entry, self.oodle) {
            Ok((data, _)) => data,
            Err(e) => return failed(format!("H: {}", cause(&e))),
        };
        let header = match header::parse(&h) {
            Ok(header) => header,
            Err(e) => return failed(format!("H: common header: {e}")),
        };
        let input = ExportInput {
            game: self.cache.game,
            category,
            package: &file.package,
            path: &file.path,
            header: &header,
            h: &h,
            source: package,
            parts,
            oodle: self.oodle,
        };
        let outputs = match catch_unwind(AssertUnwindSafe(|| exporter.export(&input))) {
            Ok(Ok(outputs)) if outputs.is_empty() => return failed("the exporter produced no files".into()),
            Ok(Ok(outputs)) => outputs,
            Ok(Err(ExportError::Unsupported(reason))) => return (record(Status::Unsupported, tag, reason), 0),
            Ok(Err(ExportError::Failed(reason))) => return failed(reason),
            Err(panic) => return failed(panic_message(panic)),
        };
        let mut bytes = 0;
        for output in &outputs {
            let target = match internal_path_under(self.out_dir, &output.path) {
                Ok(target) => target,
                Err(e) => return failed(e.to_string()),
            };
            if !self.opts.dry_run
                && let Err(e) = write_atomic(&target, |w| w.write_all(&output.data))
            {
                return failed(e.to_string());
            }
            bytes += output.data.len() as u64;
        }
        let outputs = outputs.into_iter().map(|o| o.path).collect();
        (ExportRecord { status: Status::Ok, exporter: tag, reason: None, outputs }, bytes)
    }
}

/// Check that `manifest` still describes `cache`: the same game, and for every package
/// the same file counts and sizes.
pub fn check_manifest(cache: &Cache, manifest: &Manifest) -> Result<()> {
    if manifest.source.game != cache.game {
        return Err(Error::StaleManifest(format!("it is of {}, the cache is {}", manifest.source.game, cache.game)));
    }
    let now: Vec<_> = cache.packages.iter().map(package_info).collect();
    if let Some(changed) = manifest.packages.iter().find(|p| !now.contains(p)) {
        return Err(Error::StaleManifest(format!("package {} differs", changed.name)));
    }
    if now.len() != manifest.packages.len() {
        return Err(Error::StaleManifest("the cache has packages the manifest lacks".into()));
    }
    Ok(())
}

/// Export the files `opts` selects into `out_dir`, and set each one's `export` record in
/// `manifest`. `previous` is an older manifest whose records count as earlier exports for
/// files that haven't changed since.
pub fn export(
    cache: &Cache,
    manifest: &mut Manifest,
    previous: Option<&Manifest>,
    exporters: &Exporters,
    oodle: Option<&dyn OodleDecoder>,
    out_dir: &Path,
    opts: &ExportOptions,
) -> Result<ExportStats> {
    if !cache.game.readable() {
        return Err(Error::UnsupportedGame(cache.game));
    }
    check_manifest(cache, manifest)?;
    let start = Instant::now();
    let run = Run { cache, exporters, oodle, out_dir, opts };

    let classifier = manifest.classifier();
    let before: HashMap<(&str, &str), &FileEntry> = previous
        .map(|p| p.files.iter().filter(|f| !f.stale).map(|f| ((f.package.as_str(), f.path.as_str()), f)).collect())
        .unwrap_or_default();
    let selected: Vec<(usize, Option<Kind>)> = manifest
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.stale)
        .filter(|(_, f)| opts.package.as_ref().is_none_or(|p| *p == f.package))
        .filter(|(_, f)| opts.path.as_ref().is_none_or(|p| f.path.starts_with(p.as_str())))
        .map(|(i, f)| (i, classifier.kind(f)))
        .filter(|(_, kind)| opts.kinds.is_empty() || kind.is_some_and(|k| opts.kinds.contains(&k)))
        .collect();

    let results: Vec<(usize, ExportRecord, u64)> = selected
        .par_iter()
        .map(|&(i, kind)| {
            let file = &manifest.files[i];
            let earlier = file.export.as_ref().or_else(|| {
                let old = before.get(&(file.package.as_str(), file.path.as_str()))?;
                old.same_content(file).then_some(old.export.as_ref()?)
            });
            let (record, bytes) = run.export_one(file, classifier.category(file), kind, earlier);
            (i, record, bytes)
        })
        .collect();
    drop(classifier);

    let mut stats = ExportStats { selected: results.len() as u64, ..Default::default() };
    for (i, record, bytes) in results {
        match record.status {
            Status::Ok => {
                stats.ok += 1;
                stats.outputs += record.outputs.len() as u64;
                stats.bytes += bytes;
            }
            Status::Failed => stats.failed += 1,
            Status::Unsupported => stats.unsupported += 1,
            Status::Skipped => stats.skipped += 1,
        }
        manifest.files[i].export = Some(record);
    }
    stats.elapsed = start.elapsed();
    Ok(stats)
}
