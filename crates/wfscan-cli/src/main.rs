use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;
use wfscan_core::output::{internal_path_under, write_atomic};
use wfscan_core::{
    Cache, Classifier, ExportOptions, Exporters, FileEntry, FormatOptions, IndexOptions, Kind, Manifest, Oodle, OodleDecoder,
    PackageInfo, Part, Status, TextureOutput, diff, export, index, package_info, report,
};

#[derive(Parser)]
#[command(name = "wfscan", version, about = "Index and export Warframe's Cache.Windows packages")]
struct Cli {
    /// Print machine-readable JSON on stdout instead of text
    #[arg(long, global = true)]
    json: bool,

    /// Worker threads [default: one per CPU]
    #[arg(short = 'j', long, global = true, value_name = "N")]
    threads: Option<usize>,

    /// Your own Oodle library (oo2core_*.dll or liboo2core*.so) [default: $WFSCAN_OODLE_DLL].
    /// wfscan doesn't include Oodle, and Warframe doesn't ship it as a file; a copy from
    /// another game or SDK works
    #[arg(long, global = true, value_name = "PATH")]
    oodle_dll: Option<PathBuf>,

    /// Read the cache even if another program (the game or its launcher) has it open for
    /// writing
    #[arg(long, global = true)]
    allow_in_use: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summary of a cache folder: the game, its packages, file counts and sizes
    Info {
        /// The game's Cache.Windows folder
        cache: PathBuf,
    },
    /// Record every file of a cache (package, path, parts, sizes, type) and optionally
    /// write the manifest
    Index {
        /// The game's Cache.Windows folder
        cache: PathBuf,
        /// Write the manifest here
        #[arg(short, long, value_name = "MANIFEST")]
        output: Option<PathBuf>,
        /// Also read every B and F part and record a CRC-32 of each part's contents.
        /// Reads the whole cache, which doubles as a check that all of it decodes
        #[arg(long)]
        hash: bool,
        /// Keep each header's source paths and attribute text in the manifest, not just
        /// its type and hash (much larger)
        #[arg(long)]
        full_headers: bool,
        /// Leave out stale entries (deleted or replaced files the cache still lists)
        #[arg(long)]
        no_stale: bool,
    },
    /// List files from a manifest
    List {
        /// Manifest from `wfscan index -o`
        #[arg(short, long)]
        manifest: PathBuf,
        /// Only this package
        #[arg(long)]
        package: Option<String>,
        /// Only paths starting with this
        #[arg(long, value_name = "PREFIX")]
        path: Option<String>,
        /// Only files of this type number
        #[arg(long = "type", value_name = "N")]
        type_id: Option<u32>,
        /// Only files of this kind (model, texture, material, audio, level, level-static,
        /// landscape, shader, icon)
        #[arg(long)]
        kind: Option<Kind>,
        /// Only files whose export ended this way (ok, failed, unsupported, skipped)
        #[arg(long, value_parser = parse_status)]
        status: Option<Status>,
        /// Only files that `index` couldn't read
        #[arg(long)]
        failed: bool,
        /// Only stale entries
        #[arg(long)]
        stale: bool,
    },
    /// Write the decompressed, otherwise untouched H, B and F data of a file, or of every
    /// file under a folder, as `<dir>/<package>/<path>.<H|B|F>`
    Raw {
        /// The game's Cache.Windows folder
        cache: PathBuf,
        /// Internal path of a file, or of a folder (ending in `/`)
        #[arg(long)]
        path: String,
        /// Only this package [default: every package that has the path]
        #[arg(long)]
        package: Option<String>,
        /// Output directory
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
    },
    /// Export files to standard formats and record how each one ended in the manifest.
    /// Files whose earlier export still stands are skipped
    Export {
        /// The game's Cache.Windows folder
        cache: PathBuf,
        /// Manifest from `wfscan index -o`; updated with the results
        #[arg(short, long)]
        manifest: PathBuf,
        /// Output directory
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
        /// Only this package
        #[arg(long)]
        package: Option<String>,
        /// Only paths starting with this
        #[arg(long, value_name = "PREFIX")]
        path: Option<String>,
        /// Only these kinds, comma separated (model, texture, material, audio, level,
        /// level-static, landscape, shader, icon)
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<Kind>,
        /// What textures become: dds (exact: the game's data with every mip, face and
        /// layer) or png (the top mip decoded to 8-bit RGBA, one file per face, layer or
        /// slice; HDR textures are clamped)
        #[arg(long, default_value_t, value_name = "FORMAT")]
        texture_format: TextureOutput,
        /// An older manifest, from before a game update: files unchanged since, and
        /// exported then, are skipped
        #[arg(long, value_name = "MANIFEST")]
        changed_since: Option<PathBuf>,
        /// Run the exporters but write nothing, not even the manifest
        #[arg(long)]
        dry_run: bool,
        /// Export again even where an earlier export still stands
        #[arg(long)]
        force: bool,
    },
    /// What changed between two manifests, such as before and after a game update
    Diff {
        old: PathBuf,
        new: PathBuf,
        /// List every file, not just the totals per type
        #[arg(long)]
        list: bool,
    },
    /// Coverage per file type: how many files, whether wfscan knows the type, and how
    /// reads and exports ended
    Report {
        /// Manifest from `wfscan index -o`
        #[arg(short, long)]
        manifest: PathBuf,
        /// How many kinds of failure to show
        #[arg(long, default_value_t = 20, value_name = "N")]
        errors: usize,
        /// Only types no line of the type table covers
        #[arg(long)]
        unknown: bool,
    },
}

fn parse_status(s: &str) -> Result<Status, String> {
    Status::ALL.into_iter().find(|status| status.name() == s).ok_or_else(|| format!("unknown status {s:?} (ok, failed, unsupported or skipped)"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Some(n) = cli.threads {
        rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global()?;
    }
    let oodle = Oodle::load(cli.oodle_dll.as_deref());
    // Open a cache for reading its data, unless something else is writing to it.
    let open = |dir: &Path| -> Result<Cache> {
        let cache = Cache::open(dir)?;
        if !cli.allow_in_use {
            cache.check_not_in_use()?;
        }
        Ok(cache)
    };
    match cli.command {
        Command::Info { cache } => cmd_info(&cache, cli.json),
        Command::Index { cache, output, hash, full_headers, no_stale } => {
            let opts = IndexOptions { hash, full_headers, skip_stale: no_stale };
            cmd_index(&open(&cache)?, output.as_deref(), &opts, oodle, cli.json)
        }
        Command::List { manifest, package, path, type_id, kind, status, failed, stale } => {
            let filter = ListFilter { package, path, type_id, kind, status, failed, stale };
            cmd_list(&manifest, &filter, cli.json)
        }
        Command::Raw { cache, path, package, dir } => cmd_raw(&open(&cache)?, &path, package.as_deref(), &dir, oodle, cli.json),
        Command::Export { cache, manifest, dir, package, path, kinds, texture_format, changed_since, dry_run, force } => {
            let opts = ExportOptions { package, path, kinds, dry_run, force };
            let exporters = Exporters::builtin(&FormatOptions { texture: texture_format });
            let job =
                ExportJob { manifest_path: &manifest, changed_since: changed_since.as_deref(), out_dir: &dir, opts: &opts, exporters: &exporters };
            cmd_export(&open(&cache)?, &job, oodle, cli.json)
        }
        Command::Diff { old, new, list } => cmd_diff(&old, &new, list, cli.json),
        Command::Report { manifest, errors, unknown } => cmd_report(&manifest, errors, unknown, cli.json),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

/// `1234567` as `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// What a command that needs Oodle says when a file needed it and there is none.
fn oodle_hint(needed: u64, load_error: &str) -> String {
    format!("{} files have Oodle blocks and couldn't be read: {load_error}", thousands(needed))
}

#[derive(Serialize)]
struct InfoOut<'a> {
    dir: String,
    game: wfscan_core::Game,
    packages: &'a [PackageInfo],
}

fn package_table(packages: &[PackageInfo]) {
    let width = packages.iter().map(|p| p.name.len()).max().unwrap_or(0).max(7);
    println!("{:width$}  {:5}  {:>9}  {:>7}  {:>13}  {:>13}", "package", "parts", "files", "stale", "in cache", "decompressed");
    for p in packages {
        println!(
            "{:width$}  {:5}  {:>9}  {:>7}  {:>13}  {:>13}",
            p.name,
            p.parts,
            thousands(p.files),
            thousands(p.stale),
            mib(p.compressed_size),
            mib(p.size)
        );
    }
    let sum = |f: fn(&PackageInfo) -> u64| packages.iter().map(f).sum::<u64>();
    println!(
        "{:width$}  {:5}  {:>9}  {:>7}  {:>13}  {:>13}",
        "total",
        "",
        thousands(sum(|p| p.files)),
        thousands(sum(|p| p.stale)),
        mib(sum(|p| p.compressed_size)),
        mib(sum(|p| p.size))
    );
}

fn cmd_info(dir: &Path, json: bool) -> Result<()> {
    let cache = Cache::open(dir)?;
    let packages: Vec<PackageInfo> = cache.packages.iter().map(package_info).collect();
    if json {
        return print_json(&InfoOut { dir: cache.dir.display().to_string(), game: cache.game, packages: &packages });
    }
    println!("{}: {}, {} packages", cache.dir.display(), cache.game, packages.len());
    package_table(&packages);
    Ok(())
}

#[derive(Serialize)]
struct IndexOut<'a> {
    game: wfscan_core::Game,
    stats: wfscan_core::IndexStats,
    manifest: Option<String>,
    packages: &'a [PackageInfo],
}

fn cmd_index(cache: &Cache, output: Option<&Path>, opts: &IndexOptions, oodle: Result<Oodle, String>, json: bool) -> Result<()> {
    let decoder = oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
    let (manifest, stats) = index(cache, decoder, opts)?;
    if let Err(load_error) = &oodle
        && stats.need_oodle > 0
    {
        bail!(oodle_hint(stats.need_oodle, load_error));
    }
    if let Some(path) = output {
        manifest.save(path)?;
    }
    if json {
        return print_json(&IndexOut {
            game: cache.game,
            stats,
            manifest: output.map(|p| p.display().to_string()),
            packages: &manifest.packages,
        });
    }
    println!("{}: {}, {} packages", cache.dir.display(), cache.game, manifest.packages.len());
    println!(
        "{} files ({} stale), {} with a common header, {} failed",
        thousands(stats.files),
        thousands(stats.stale),
        thousands(stats.headers),
        thousands(stats.failed)
    );
    println!(
        "read {} parts, {}, in {:.1} s (blocks: {} Oodle, {} LZF, {} stored)",
        thousands(stats.parts_read),
        mib(stats.bytes_read),
        stats.elapsed.as_secs_f64(),
        thousands(stats.blocks.oodle),
        thousands(stats.blocks.lzf),
        thousands(stats.blocks.stored)
    );
    match output {
        Some(path) => println!("wrote {}", path.display()),
        None => println!("no manifest written (use -o)"),
    }
    Ok(())
}

struct ListFilter {
    package: Option<String>,
    path: Option<String>,
    type_id: Option<u32>,
    kind: Option<Kind>,
    status: Option<Status>,
    failed: bool,
    stale: bool,
}

impl ListFilter {
    fn matches(&self, f: &FileEntry, classifier: &Classifier) -> bool {
        self.package.as_ref().is_none_or(|p| *p == f.package)
            && self.path.as_ref().is_none_or(|p| f.path.starts_with(p.as_str()))
            && self.type_id.is_none_or(|t| f.header.as_ref().is_some_and(|h| h.type_id == t))
            && self.kind.is_none_or(|k| classifier.kind(f) == Some(k))
            && self.status.is_none_or(|s| f.export.as_ref().is_some_and(|e| e.status == s))
            && (!self.failed || f.error.is_some())
            && (!self.stale || f.stale)
    }
}

fn cmd_list(manifest: &Path, filter: &ListFilter, json: bool) -> Result<()> {
    let manifest = Manifest::load(manifest)?;
    let classifier = manifest.classifier();
    let files: Vec<&FileEntry> = manifest.files.iter().filter(|f| filter.matches(f, &classifier)).collect();
    if json {
        return print_json(&files);
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let width = manifest.packages.iter().map(|p| p.name.len()).max().unwrap_or(0);
    for f in &files {
        let parts: String = Part::ALL.iter().map(|&p| if f.part(p).is_some() { p.letter() } else { '-' }).collect();
        let type_id = f.header.as_ref().map_or_else(|| "-".to_string(), |h| h.type_id.to_string());
        let mut notes = String::new();
        if f.stale {
            notes.push_str("  [stale]");
        }
        if let Some(e) = &f.error {
            notes.push_str(&format!("  [failed: {e}]"));
        }
        if let Some(e) = &f.export {
            let reason = e.reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default();
            notes.push_str(&format!("  [{}{reason}]", e.status.name()));
        }
        writeln!(out, "{:width$}  {parts}  {type_id:>5}  {:>11}  {}{notes}", f.package, f.size(), f.path)?;
    }
    out.flush()?;
    eprintln!("{} of {} files", thousands(files.len() as u64), thousands(manifest.files.len() as u64));
    Ok(())
}

#[derive(Serialize)]
struct RawOut {
    package: String,
    path: String,
    part: Part,
    size: usize,
    file: String,
}

fn cmd_raw(cache: &Cache, path: &str, package: Option<&str>, out_dir: &Path, oodle: Result<Oodle, String>, json: bool) -> Result<()> {
    if !cache.game.readable() {
        bail!(wfscan_core::Error::UnsupportedGame(cache.game));
    }
    if let Some(name) = package
        && cache.package(name).is_none()
    {
        bail!("no package named {name} in {}", cache.dir.display());
    }
    let decoder = oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
    let folder = path.ends_with('/');
    let mut written = Vec::new();
    let mut failed = 0u64;
    for pkg in cache.packages.iter().filter(|p| package.is_none_or(|name| p.name == name)) {
        for (part, file) in pkg.parts() {
            let entries: Vec<_> = if folder {
                file.toc.files.iter().filter(|e| !e.stale && e.path.starts_with(path)).collect()
            } else {
                file.find(path).into_iter().collect()
            };
            for entry in entries {
                let target = internal_path_under(&out_dir.join(&pkg.name), &format!("{}.{part}", entry.path))?;
                match file.read(entry, decoder) {
                    Ok((data, _)) => {
                        write_atomic(&target, |w| w.write_all(&data))?;
                        written.push(RawOut {
                            package: pkg.name.clone(),
                            path: entry.path.clone(),
                            part,
                            size: data.len(),
                            file: target.display().to_string(),
                        });
                    }
                    Err(e) => {
                        if let (wfscan_core::Error::Block { source: wfscan_core::BlockError::NoOodle { .. }, .. }, Err(load_error)) =
                            (&e, &oodle)
                        {
                            bail!("{e}: {load_error}");
                        }
                        eprintln!("failed: {} {part}: {e}", pkg.name);
                        failed += 1;
                    }
                }
            }
        }
    }
    if written.is_empty() && failed == 0 {
        let what = if folder { "nothing under" } else { "no file at" };
        let hint = if folder { "" } else { " (end the path with / for a folder)" };
        bail!("{what} {path}{hint}");
    }
    if json {
        return print_json(&written);
    }
    for w in &written {
        println!("{}  {:>11}  {}", w.part, w.size, w.file);
    }
    eprintln!("wrote {} files to {}{}", thousands(written.len() as u64), out_dir.display(), if failed > 0 { format!(", {failed} failed") } else { String::new() });
    Ok(())
}

#[derive(Serialize)]
struct ExportOut {
    stats: wfscan_core::ExportStats,
    dry_run: bool,
    dir: String,
}

/// What `wfscan export` was asked to do.
struct ExportJob<'a> {
    manifest_path: &'a Path,
    changed_since: Option<&'a Path>,
    out_dir: &'a Path,
    opts: &'a ExportOptions,
    exporters: &'a Exporters,
}

fn cmd_export(cache: &Cache, job: &ExportJob, oodle: Result<Oodle, String>, json: bool) -> Result<()> {
    let ExportJob { manifest_path, changed_since, out_dir, opts, exporters } = *job;
    let mut manifest = Manifest::load(manifest_path)?;
    let previous = changed_since.map(Manifest::load).transpose()?;
    let decoder = oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
    let stats = export(cache, &mut manifest, previous.as_ref(), exporters, decoder, out_dir, opts)?;
    if let Err(load_error) = &oodle {
        let need = manifest.files.iter().filter_map(|f| f.export.as_ref()?.reason.as_deref()).filter(|r| r.contains("no Oodle library")).count();
        if need > 0 {
            bail!(oodle_hint(need as u64, load_error));
        }
    }
    if !opts.dry_run {
        manifest.save(manifest_path)?;
    }
    if json {
        return print_json(&ExportOut { stats, dry_run: opts.dry_run, dir: out_dir.display().to_string() });
    }
    println!(
        "{} files selected: {} ok, {} failed, {} unsupported, {} skipped",
        thousands(stats.selected),
        thousands(stats.ok),
        thousands(stats.failed),
        thousands(stats.unsupported),
        thousands(stats.skipped)
    );
    let wrote = if opts.dry_run { "would write" } else { "wrote" };
    println!("{wrote} {} files, {}, to {} in {:.1} s", thousands(stats.outputs), mib(stats.bytes), out_dir.display(), stats.elapsed.as_secs_f64());
    if opts.dry_run {
        println!("dry run: nothing written, manifest unchanged");
    } else {
        println!("updated {} (see `wfscan report`)", manifest_path.display());
    }
    Ok(())
}

fn type_label(type_id: Option<u32>) -> String {
    type_id.map_or_else(|| "-".to_string(), |t| t.to_string())
}

fn kind_label(kind: Option<Kind>, type_id: Option<u32>) -> &'static str {
    match (kind, type_id) {
        (Some(kind), _) => kind.name(),
        (None, Some(_)) => "?",
        (None, None) => "(no header)",
    }
}

#[derive(Serialize)]
struct DiffFile<'a> {
    package: &'a str,
    path: &'a str,
    #[serde(rename = "type")]
    type_id: Option<u32>,
}

#[derive(Serialize)]
struct DiffOut<'a> {
    added: Vec<DiffFile<'a>>,
    removed: Vec<DiffFile<'a>>,
    changed: Vec<DiffFile<'a>>,
    unchanged: u64,
    summary: &'a [wfscan_core::DiffRow],
}

fn cmd_diff(old: &Path, new: &Path, list: bool, json: bool) -> Result<()> {
    let (old, new) = (Manifest::load(old)?, Manifest::load(new)?);
    let diff = diff(&old, &new);
    fn file(f: &FileEntry) -> DiffFile<'_> {
        DiffFile { package: &f.package, path: &f.path, type_id: f.header.as_ref().map(|h| h.type_id) }
    }
    if json {
        return print_json(&DiffOut {
            added: diff.added.iter().map(|f| file(f)).collect(),
            removed: diff.removed.iter().map(|f| file(f)).collect(),
            changed: diff.changed.iter().map(|(_, new)| file(new)).collect(),
            unchanged: diff.unchanged,
            summary: &diff.summary,
        });
    }
    println!(
        "{} added, {} removed, {} changed, {} unchanged",
        thousands(diff.added.len() as u64),
        thousands(diff.removed.len() as u64),
        thousands(diff.changed.len() as u64),
        thousands(diff.unchanged)
    );
    if !diff.summary.is_empty() {
        println!("{:17}  {:>5}  {:12}  {:>8}  {:>8}  {:>8}", "category", "type", "kind", "added", "removed", "changed");
    }
    for row in &diff.summary {
        println!(
            "{:17}  {:>5}  {:12}  {:>8}  {:>8}  {:>8}",
            row.category.name(),
            type_label(row.type_id),
            kind_label(row.kind, row.type_id),
            thousands(row.added),
            thousands(row.removed),
            thousands(row.changed)
        );
    }
    if list {
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        let lines = diff.added.iter().map(|f| ('+', f)).chain(diff.removed.iter().map(|f| ('-', f))).chain(diff.changed.iter().map(|(_, f)| ('~', f)));
        for (mark, f) in lines {
            writeln!(out, "{mark} {}  {}", f.package, f.path)?;
        }
        out.flush()?;
    }
    Ok(())
}

fn cmd_report(manifest: &Path, max_errors: usize, unknown_only: bool, json: bool) -> Result<()> {
    let manifest = Manifest::load(manifest)?;
    let mut report = report(&manifest);
    if unknown_only {
        report.types.retain(|t| t.kind.is_none() && t.type_id.is_some());
        report.errors.retain(|e| e.kind.is_none() && e.type_id.is_some());
    }
    if json {
        return print_json(&report);
    }
    println!(
        "{:17}  {:>5}  {:12}  {:>9}  {:>13}  {:>7}  {:>9}  {:>7}  {:>9}  {:>9}  sample",
        "category", "type", "kind", "files", "size", "unread", "ok", "failed", "unsupp.", "skipped"
    );
    for t in &report.types {
        println!(
            "{:17}  {:>5}  {:12}  {:>9}  {:>13}  {:>7}  {:>9}  {:>7}  {:>9}  {:>9}  {}",
            t.category.name(),
            type_label(t.type_id),
            kind_label(t.kind, t.type_id),
            thousands(t.files),
            mib(t.size),
            thousands(t.unreadable),
            thousands(t.ok),
            thousands(t.failed),
            thousands(t.unsupported),
            thousands(t.skipped),
            t.sample
        );
    }
    let files = |rows: &mut dyn Iterator<Item = &wfscan_core::TypeRow>| rows.map(|t| t.files).sum::<u64>();
    let known = files(&mut report.types.iter().filter(|t| t.kind.is_some()));
    let unknown_types = report.types.iter().filter(|t| t.kind.is_none() && t.type_id.is_some()).count();
    let unknown = files(&mut report.types.iter().filter(|t| t.kind.is_none() && t.type_id.is_some()));
    let headerless = files(&mut report.types.iter().filter(|t| t.type_id.is_none()));
    println!(
        "{} files of known kinds, {} files of {} unknown types, {} without a header",
        thousands(known),
        thousands(unknown),
        thousands(unknown_types as u64),
        thousands(headerless)
    );
    if !report.errors.is_empty() {
        println!("failures ({} kinds, most common first):", thousands(report.errors.len() as u64));
        for e in report.errors.iter().take(max_errors) {
            println!(
                "{:>9}  {} {} {}: {}  (e.g. {})",
                thousands(e.files),
                e.category.name(),
                type_label(e.type_id),
                kind_label(e.kind, e.type_id),
                e.message,
                e.sample
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
