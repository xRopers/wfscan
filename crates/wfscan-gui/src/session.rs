//! What the window is working on, and the slow operations on it. No drawing code: tests
//! drive this directly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use wfscan_core::{
    BinkLibrary, Cache, Category, ExportOptions, ExportStats, Exporters, FileEntry, FormatOptions, IndexOptions, IndexStats, Kind, Manifest, Oodle,
    OodleDecoder, Part, Status, export, index,
};
use wfscan_core::output::write_atomic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
}

/// An opened and indexed cache.
pub struct Loaded {
    pub cache: Arc<Cache>,
    pub manifest: Manifest,
    /// Per entry of `manifest.files`: what kind of asset it is and its package's category.
    pub kinds: Vec<Option<Kind>>,
    pub categories: Vec<Category>,
    pub stats: IndexStats,
}

impl Loaded {
    fn new(cache: Arc<Cache>, manifest: Manifest, stats: IndexStats) -> Self {
        let classifier = manifest.classifier();
        let kinds = manifest.files.iter().map(|f| classifier.kind(f)).collect();
        let categories = manifest.files.iter().map(|f| classifier.category(f)).collect();
        drop(classifier);
        Self { cache, manifest, kinds, categories, stats }
    }
}

/// Which files the table shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Part of the path, any case.
    pub text: String,
    pub kind: Option<Kind>,
    pub status: Option<Status>,
    /// Only files whose kind wfscan knows.
    pub known_only: bool,
}

#[derive(Default)]
pub struct Session {
    pub loaded: Option<Loaded>,
    pub log: Vec<LogLine>,
    /// The Oodle library in use, or why there is none.
    pub oodle: Option<Arc<Oodle>>,
    pub oodle_error: Option<String>,
    /// The Bink library in use (for video frames), or why there is none.
    pub bink: Option<Arc<BinkLibrary>>,
    pub bink_error: Option<String>,
    /// Goes up whenever the file list or its statuses change, so views know to refresh.
    pub generation: u64,
    pub last_export: Option<(ExportStats, PathBuf)>,
}

impl Session {
    pub fn info(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Info, text: text.into() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Error, text: text.into() });
    }

    /// Load the Oodle library at `path`, or the one the environment names.
    pub fn set_oodle(&mut self, path: Option<&Path>) {
        match Oodle::load(path) {
            Ok(oodle) => {
                self.oodle = Some(Arc::new(oodle));
                self.oodle_error = None;
            }
            Err(e) => {
                self.oodle = None;
                self.oodle_error = Some(e);
            }
        }
    }

    /// Load the Bink library at `path`, or the one the environment names.
    pub fn set_bink(&mut self, path: Option<&Path>) {
        match BinkLibrary::load(path) {
            Ok(bink) => {
                self.bink = Some(Arc::new(bink));
                self.bink_error = None;
            }
            Err(e) => {
                self.bink = None;
                self.bink_error = Some(e);
            }
        }
    }

    pub fn set_loaded(&mut self, loaded: Loaded) {
        // Without a Bink library set, use the game's own if it is next to the cache.
        if self.bink.is_none()
            && let Some(found) = BinkLibrary::next_to_cache(&loaded.cache.dir)
        {
            self.set_bink(Some(&found));
            if self.bink.is_some() {
                self.info(format!("Bink: using the game's {}", found.display()));
            }
        }
        let s = &loaded.stats;
        self.info(format!(
            "opened {}: {}, {} packages, {} files in {:.1} s",
            loaded.cache.dir.display(),
            loaded.cache.game,
            loaded.manifest.packages.len(),
            s.files,
            s.elapsed.as_secs_f64()
        ));
        if s.need_oodle > 0 {
            self.error(format!("{} files couldn't be read without an Oodle library (Tools > Oodle library)", s.need_oodle));
        } else if s.failed > 0 {
            self.error(format!("{} files couldn't be read", s.failed));
        }
        self.loaded = Some(loaded);
        self.last_export = None;
        self.generation += 1;
    }

    /// Take the results of an export: the manifest with its new records.
    pub fn set_exported(&mut self, manifest: Manifest, stats: ExportStats, dir: PathBuf) {
        let Some(loaded) = &mut self.loaded else { return };
        loaded.manifest = manifest;
        let raw = if stats.raw > 0 { format!(" ({} written raw)", stats.raw) } else { String::new() };
        self.info(format!(
            "exported to {}: {} ok, {} failed, {} unsupported{raw}, {} skipped; {} files, {:.1} MiB in {:.1} s",
            dir.display(),
            stats.ok,
            stats.failed,
            stats.unsupported,
            stats.skipped,
            stats.outputs,
            stats.bytes as f64 / (1024.0 * 1024.0),
            stats.elapsed.as_secs_f64()
        ));
        self.last_export = Some((stats, dir));
        self.generation += 1;
    }

    pub fn close(&mut self) {
        self.loaded = None;
        self.last_export = None;
        self.generation += 1;
    }

    pub fn files(&self) -> &[FileEntry] {
        self.loaded.as_ref().map_or(&[], |l| &l.manifest.files)
    }

    /// The entries `filter` lets through, as positions in [`Session::files`]. Stale
    /// entries are never shown.
    pub fn view(&self, filter: &Filter) -> Vec<usize> {
        let Some(loaded) = &self.loaded else { return Vec::new() };
        let needle = filter.text.trim().to_ascii_lowercase();
        loaded
            .manifest
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.stale)
            .filter(|(i, _)| filter.kind.is_none_or(|k| loaded.kinds[*i] == Some(k)))
            .filter(|(i, _)| !filter.known_only || loaded.kinds[*i].is_some())
            .filter(|(_, f)| filter.status.is_none_or(|s| f.export.as_ref().is_some_and(|e| e.status == s)))
            .filter(|(_, f)| needle.is_empty() || contains_ignoring_case(&f.path, &needle))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Whether `haystack` contains `needle` (already lower case), ignoring ASCII case.
pub fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
}

/// Open a cache folder and index it.
pub fn open_cache(dir: &Path, oodle: Option<Arc<Oodle>>) -> anyhow::Result<Loaded> {
    let cache = Cache::open(dir)?;
    cache.check_not_in_use()?;
    let decoder = oodle.as_deref().map(|o| o as &dyn OodleDecoder);
    let (manifest, stats) = index(&cache, decoder, &IndexOptions::default()).with_context(|| dir.display().to_string())?;
    Ok(Loaded::new(Arc::new(cache), manifest, stats))
}

/// Export the entries at `files` of `manifest` into `out_dir`. Returns the manifest with
/// their export records set.
pub fn run_export(
    cache: &Cache,
    mut manifest: Manifest,
    files: HashSet<usize>,
    out_dir: &Path,
    formats: &FormatOptions,
    raw_unsupported: bool,
    oodle: Option<Arc<Oodle>>,
) -> anyhow::Result<(Manifest, ExportStats)> {
    let decoder = oodle.as_deref().map(|o| o as &dyn OodleDecoder);
    let exporters = Exporters::builtin(formats);
    let opts = ExportOptions { files: Some(files), raw_unsupported, ..Default::default() };
    let stats = export(cache, &mut manifest, None, &exporters, decoder, out_dir, &opts)?;
    Ok((manifest, stats))
}

/// Write a file's parts, decompressed and whole, one after another into `target`: one
/// part to get it alone, or several to join them in the order given. A part the file
/// doesn't have is left out. Returns each part written and its size.
pub fn save_parts(
    cache: &Cache,
    package: &str,
    path: &str,
    parts: &[Part],
    oodle: Option<Arc<Oodle>>,
    target: &Path,
) -> anyhow::Result<Vec<(Part, usize)>> {
    let decoder = oodle.as_deref().map(|o| o as &dyn OodleDecoder);
    let source = cache.package(package).with_context(|| format!("no package named {package}"))?;
    let found: Vec<_> = parts.iter().filter_map(|&part| Some((part, source.part(part)?, source.part(part)?.find(path)?))).collect();
    anyhow::ensure!(!found.is_empty(), "{path} has none of those parts");
    let mut sizes = Vec::new();
    write_atomic(target, |w| {
        for (part, file, entry) in &found {
            let (data, _) = file.read(entry, decoder).map_err(|e| std::io::Error::other(format!("{part}: {e}")))?;
            w.write_all(&data)?;
            sizes.push((*part, data.len()));
        }
        Ok(())
    })?;
    Ok(sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_search() {
        assert!(contains_ignoring_case("/Lotus/Characters/Tenno/Excalibur", "excal"));
        assert!(contains_ignoring_case("/Lotus/A", ""));
        assert!(!contains_ignoring_case("/Lotus/A", "tenno"));
        assert!(!contains_ignoring_case("ab", "abc"));
    }
}
