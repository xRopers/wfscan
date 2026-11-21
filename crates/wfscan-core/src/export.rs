//! The export driver: run the right exporter over each selected file, in parallel, and
//! record how every one of them ended.
//!
//! - One bad file never stops a run. An exporter's error, or a panic inside it, becomes a
//!   `failed` record for that file.
//! - A file nothing can export is `unsupported`, with what is missing as the reason.
//!   With [`ExportOptions::raw_unsupported`] its decompressed parts are written as they
//!   are (`<path>.H`, `.B`, `.F`, as `wfscan raw` names them) by the `raw` pseudo-exporter;
//!   it stays `unsupported`, so the coverage report still shows what has no reader, but
//!   its record lists the files.
//! - A file whose earlier export still stands is `skipped`: same contents, same exporter
//!   version, outputs still on disk. "Earlier" is this manifest's own record, or the
//!   record in the manifest given as `previous` (`--changed-since`) for files that
//!   haven't changed between the two, nor have the other files that export read
//!   ([`ExportInput::with_file`], recorded as the record's `uses`).
//!
//! The cache is only read. Outputs go to a temporary name and are renamed into place.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Mutex;
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

/// What an exporter gets to work with: one file's header and H data, its B and F parts on
/// request, and any other file of the cache through [`ExportInput::with_file`].
pub struct ExportInput<'a> {
    pub game: Game,
    pub category: Category,
    pub package: &'a str,
    pub path: &'a str,
    pub header: &'a CommonHeader,
    /// The whole H part, common header included.
    pub h: &'a [u8],
    cache: &'a Cache,
    source: &'a Package,
    parts: [Option<&'a TocEntry>; 3],
    oodle: Option<&'a dyn OodleDecoder>,
    /// Other files read through this input (and inputs it opened), `Package:/path`.
    uses: &'a Mutex<Vec<String>>,
}

impl ExportInput<'_> {
    /// `name` as a full internal path: names starting with `/` are already, anything else
    /// is relative to this file's folder (how models name their materials).
    pub fn resolve(&self, name: &str) -> String {
        if name.starts_with('/') {
            return name.to_string();
        }
        let folder = self.path.rsplit_once('/').map_or("", |(folder, _)| folder);
        format!("{folder}/{name}")
    }

    /// The package holding a live file at `path`: this file's own package if it has one,
    /// else the first by name.
    pub fn find_file(&self, path: &str) -> Option<&str> {
        let has = |p: &Package| p.part(Part::H).is_some_and(|h| h.find(path).is_some());
        if has(self.source) {
            return Some(self.source.name.as_str());
        }
        self.cache.packages.iter().find(|p| has(p)).map(|p| p.name.as_str())
    }

    /// Open another file of the cache (an internal path, see [`ExportInput::resolve`]) and
    /// hand it to `use_it` as an input of its own. The file is recorded in the export's
    /// `uses`, so a later `--changed-since` run exports this one again if it changes.
    /// A file that isn't there, or has no common header, is a `Failed` error naming it.
    pub fn with_file<T>(&self, path: &str, use_it: impl FnOnce(&ExportInput) -> T) -> Result<T, ExportError> {
        let package = self.find_file(path).ok_or_else(|| ExportError::failed(format!("{path} is not in the cache")))?;
        {
            let mut uses = self.uses.lock().unwrap_or_else(|e| e.into_inner());
            let tag = format!("{package}:{path}");
            if !uses.contains(&tag) {
                uses.push(tag);
            }
        }
        open_input(self.cache, package, path, self.oodle, self.uses, use_it).map_err(|e| ExportError::failed(format!("{path}: {}", cause(&e))))
    }

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

    /// Hand each output to `emit` as soon as it is made, so that an export of many large
    /// files (a video's frames) never holds them all at once. The driver writes each one
    /// as it arrives. By default, everything [`Exporter::export`] returns.
    fn export_each(&self, input: &ExportInput, emit: &mut dyn FnMut(Output) -> Result<(), ExportError>) -> Result<(), ExportError> {
        for output in self.export(input)? {
            emit(output)?;
        }
        Ok(())
    }
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
    /// Only these entries of the manifest, by position in its file list.
    pub files: Option<std::collections::HashSet<usize>>,
    /// Run the exporters and record the results, but write no files.
    pub dry_run: bool,
    /// Export again even where an earlier export still stands.
    pub force: bool,
    /// Write the decompressed parts of files that would be `unsupported`.
    pub raw_unsupported: bool,
}

impl ExportOptions {
    /// Whether these options pick `file`, entry `index` of the manifest, of kind `kind`.
    pub fn selects(&self, index: usize, file: &FileEntry, kind: Option<Kind>) -> bool {
        !file.stale
            && self.package.as_ref().is_none_or(|p| *p == file.package)
            && self.path.as_ref().is_none_or(|p| file.path.starts_with(p.as_str()))
            && self.files.as_ref().is_none_or(|only| only.contains(&index))
            && (self.kinds.is_empty() || kind.is_some_and(|k| self.kinds.contains(&k)))
    }

    /// `manifest` cut down to the files these options pick, as a run leaves it: for a
    /// coverage report of that run alone (a dry run's included).
    pub fn selection(&self, manifest: &Manifest) -> Manifest {
        let classifier = manifest.classifier();
        let files = manifest.files.iter().enumerate().filter(|&(i, f)| self.selects(i, f, classifier.kind(f))).map(|(_, f)| f.clone()).collect();
        Manifest { version: manifest.version, tool: manifest.tool.clone(), source: manifest.source.clone(), packages: manifest.packages.clone(), files }
    }
}

/// The name and version recorded for files written as raw parts.
pub const RAW_EXPORTER: &str = "raw@1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExportStats {
    /// Files the options selected; the four statuses add up to this.
    pub selected: u64,
    pub ok: u64,
    pub failed: u64,
    pub unsupported: u64,
    pub skipped: u64,
    /// Of the unsupported files, those written as raw parts.
    pub raw: u64,
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
    ExportRecord { status, exporter, reason: Some(reason.into()), outputs: Vec::new(), uses: Vec::new() }
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
        let (record, bytes) = self.export_with_reader(file, category, kind, earlier);
        if record.status == Status::Unsupported && self.opts.raw_unsupported {
            return self.export_raw(file, record.reason.unwrap_or_default());
        }
        (record, bytes)
    }

    /// The parts of `file` in the cache, checked against what the manifest says.
    fn find_parts<'p>(&self, file: &FileEntry, package: &'p Package) -> Result<[Option<&'p TocEntry>; 3], String> {
        let mut parts = [None; 3];
        for part in Part::ALL {
            let Some(listed) = file.part(part) else { continue };
            match package.part(part).and_then(|f| f.find(&file.path)) {
                Some(e) if (e.offset, e.compressed_size, e.size) == (listed.offset, listed.compressed_size, listed.size) => {
                    parts[part as usize] = Some(e);
                }
                _ => return Err(format!("{part}: the cache no longer matches the manifest")),
            }
        }
        Ok(parts)
    }

    fn write(&self, outputs: &[Output]) -> Result<u64, String> {
        let mut bytes = 0;
        for output in outputs {
            let target = internal_path_under(self.out_dir, &output.path).map_err(|e| e.to_string())?;
            if !self.opts.dry_run {
                write_atomic(&target, |w| w.write_all(&output.data)).map_err(|e| e.to_string())?;
            }
            bytes += output.data.len() as u64;
        }
        Ok(bytes)
    }

    /// Write an unsupported file's decompressed parts as they are. The record keeps the
    /// reason nothing could read it; a part that can't be read makes it `failed`.
    fn export_raw(&self, file: &FileEntry, reason: String) -> (ExportRecord, u64) {
        let tag = Some(RAW_EXPORTER.to_string());
        let failed = |why: String| (record(Status::Failed, tag.clone(), why), 0);
        let package = self.cache.package(&file.package).expect("the manifest's packages were checked against the cache");
        let parts = match self.find_parts(file, package) {
            Ok(parts) => parts,
            Err(e) => return failed(e),
        };
        let mut outputs = Vec::new();
        for part in Part::ALL {
            let Some(entry) = parts[part as usize] else { continue };
            match package.part(part).expect("a listed part exists").read(entry, self.oodle) {
                Ok((data, _)) => outputs.push(Output { path: format!("{}.{part}", file.path), data }),
                Err(e) => return failed(format!("{part}: {}", cause(&e))),
            }
        }
        if outputs.is_empty() {
            return failed("the file has no parts".into());
        }
        let bytes = match self.write(&outputs) {
            Ok(bytes) => bytes,
            Err(e) => return failed(e),
        };
        let outputs = outputs.into_iter().map(|o| o.path).collect();
        (ExportRecord { status: Status::Unsupported, exporter: tag, reason: Some(reason), outputs, uses: Vec::new() }, bytes)
    }

    /// Export one file with the exporter for its kind.
    fn export_with_reader(&self, file: &FileEntry, category: Category, kind: Option<Kind>, earlier: Option<&ExportRecord>) -> (ExportRecord, u64) {
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
            return (
                ExportRecord { status: Status::Skipped, exporter: tag, reason: None, outputs: earlier.outputs.clone(), uses: earlier.uses.clone() },
                0,
            );
        }

        let failed = |reason: String| (record(Status::Failed, tag.clone(), reason), 0);
        let package = self.cache.package(&file.package).expect("the manifest's packages were checked against the cache");
        let parts = match self.find_parts(file, package) {
            Ok(parts) => parts,
            Err(e) => return failed(e),
        };
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
        let uses = Mutex::new(Vec::new());
        let input = ExportInput {
            game: self.cache.game,
            category,
            package: &file.package,
            path: &file.path,
            header: &header,
            h: &h,
            cache: self.cache,
            source: package,
            parts,
            oodle: self.oodle,
            uses: &uses,
        };
        // Each output is written as soon as the exporter hands it over.
        let (mut paths, mut bytes) = (Vec::new(), 0u64);
        let mut emit = |output: Output| -> Result<(), ExportError> {
            bytes += self.write(std::slice::from_ref(&output)).map_err(ExportError::Failed)?;
            paths.push(output.path);
            Ok(())
        };
        let result = catch_unwind(AssertUnwindSafe(|| exporter.export_each(&input, &mut emit)));
        let mut uses = uses.into_inner().unwrap_or_else(|e| e.into_inner());
        uses.sort();
        match result {
            Ok(Ok(())) if paths.is_empty() => failed("the exporter produced no files".into()),
            Ok(Ok(())) => (ExportRecord { status: Status::Ok, exporter: tag, reason: None, outputs: paths, uses }, bytes),
            Ok(Err(ExportError::Unsupported(reason))) => (record(Status::Unsupported, tag, reason), 0),
            Ok(Err(ExportError::Failed(reason))) => failed(reason),
            Err(panic) => failed(panic_message(panic)),
        }
    }
}

/// Read one file's H part and hand `use_it` what an exporter would get, without going
/// through a manifest: for previews and tools that look at a single file.
pub fn with_input<T>(
    cache: &Cache,
    package: &str,
    path: &str,
    oodle: Option<&dyn OodleDecoder>,
    use_it: impl FnOnce(&ExportInput) -> T,
) -> Result<T> {
    open_input(cache, package, path, oodle, &Mutex::new(Vec::new()), use_it)
}

fn open_input<T>(
    cache: &Cache,
    package: &str,
    path: &str,
    oodle: Option<&dyn OodleDecoder>,
    uses: &Mutex<Vec<String>>,
    use_it: impl FnOnce(&ExportInput) -> T,
) -> Result<T> {
    let source = cache.package(package).ok_or_else(|| Error::NotFound(format!("no package named {package}")))?;
    let mut parts = [None; 3];
    for part in Part::ALL {
        parts[part as usize] = source.part(part).and_then(|f| f.find(path));
    }
    let h_entry = parts[Part::H as usize].ok_or_else(|| Error::NotFound(format!("{package} has no file at {path}")))?;
    let (h, _) = source.part(Part::H).expect("a found entry's part exists").read(h_entry, oodle)?;
    let header = header::parse(&h).map_err(|e| Error::NotFound(format!("{path} has no common header ({e})")))?;
    let input = ExportInput { game: cache.game, category: source.category, package, path, header: &header, h: &h, cache, source, parts, oodle, uses };
    Ok(use_it(&input))
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
    // This manifest by package and path, only needed to check an older record's `uses`.
    let now: HashMap<(&str, &str), &FileEntry> = if previous.is_some() {
        manifest.files.iter().filter(|f| !f.stale).map(|f| ((f.package.as_str(), f.path.as_str()), f)).collect()
    } else {
        HashMap::new()
    };
    let selected: Vec<(usize, Option<Kind>)> = manifest
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (i, classifier.kind(f)))
        .filter(|&(i, kind)| opts.selects(i, &manifest.files[i], kind))
        .collect();

    let results: Vec<(usize, ExportRecord, u64)> = selected
        .par_iter()
        .map(|&(i, kind)| {
            let file = &manifest.files[i];
            // This manifest's own record, else the older manifest's if the file and every
            // file its export read are unchanged between the two.
            let earlier = file.export.as_ref().or_else(|| {
                let old = before.get(&(file.package.as_str(), file.path.as_str()))?;
                let record = old.export.as_ref()?;
                let unchanged = |package: &str, path: &str| {
                    let (Some(then), Some(now)) = (before.get(&(package, path)), now.get(&(package, path))) else { return false };
                    then.same_content(now)
                };
                let uses_unchanged = record.uses.iter().all(|u| u.split_once(':').is_some_and(|(package, path)| unchanged(package, path)));
                (old.same_content(file) && uses_unchanged).then_some(record)
            });
            let (record, bytes) = run.export_one(file, classifier.category(file), kind, earlier);
            (i, record, bytes)
        })
        .collect();
    drop(classifier);
    drop(now);

    let mut stats = ExportStats { selected: results.len() as u64, ..Default::default() };
    for (i, record, bytes) in results {
        match record.status {
            Status::Ok => {
                stats.ok += 1;
                stats.outputs += record.outputs.len() as u64;
                stats.bytes += bytes;
            }
            Status::Failed => stats.failed += 1,
            Status::Unsupported => {
                stats.unsupported += 1;
                if !record.outputs.is_empty() {
                    stats.raw += 1;
                    stats.outputs += record.outputs.len() as u64;
                    stats.bytes += bytes;
                }
            }
            Status::Skipped => stats.skipped += 1,
        }
        manifest.files[i].export = Some(record);
    }
    stats.elapsed = start.elapsed();
    Ok(stats)
}
