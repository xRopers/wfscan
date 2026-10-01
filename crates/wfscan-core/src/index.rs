//! Walking a cache into a [`Manifest`].
//!
//! Every internal path of every package becomes one [`FileEntry`] joining its H, B and F
//! parts. The H part is read for its common header; B and F are only read when hashing.
//! A file that can't be read gets an `error` and the walk carries on.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::cache::block::{BlockCounts, BlockError, OodleDecoder};
use crate::cache::header;
use crate::cache::package::{Cache, Package, Part};
use crate::cache::toc::TocEntry;
use crate::error::{Error, Result};
use crate::manifest::{FileEntry, HeaderEntry, MANIFEST_VERSION, Manifest, PackageInfo, PartEntry, SourceInfo};

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptions {
    /// Read every part and record the CRC-32 of its contents. Reads the whole cache.
    pub hash: bool,
    /// Keep each header's source paths and attribute text, not just its type and hash.
    pub full_headers: bool,
    /// Leave out stale entries.
    pub skip_stale: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct IndexStats {
    /// Live files.
    pub files: u64,
    pub stale: u64,
    /// Files whose H part has a common header.
    pub headers: u64,
    /// Files with a part that couldn't be read.
    pub failed: u64,
    /// Of those, the ones that only need an Oodle library.
    pub need_oodle: u64,
    /// Parts read, and their decompressed bytes.
    pub parts_read: u64,
    pub bytes_read: u64,
    pub blocks: BlockCounts,
    #[serde(serialize_with = "seconds")]
    pub elapsed: Duration,
}

fn seconds<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

impl std::ops::AddAssign for IndexStats {
    fn add_assign(&mut self, o: Self) {
        self.files += o.files;
        self.stale += o.stale;
        self.headers += o.headers;
        self.failed += o.failed;
        self.need_oodle += o.need_oodle;
        self.parts_read += o.parts_read;
        self.bytes_read += o.bytes_read;
        self.blocks += o.blocks;
    }
}

/// One manifest entry to be: a path and the TOC entries of its parts.
struct Row<'a> {
    stale: bool,
    parts: [Option<&'a TocEntry>; 3],
}

impl Row<'_> {
    fn path(&self) -> &str {
        &self.parts.iter().flatten().next().expect("a row has a part").path
    }
}

/// A package's rows: each live path once, with whichever parts have it, then each stale
/// entry on its own.
fn rows(package: &Package, skip_stale: bool) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    for (i, part) in Part::ALL.into_iter().enumerate() {
        let Some(file) = package.part(part) else { continue };
        for entry in &file.toc.files {
            let mut parts = [None; 3];
            parts[i] = Some(entry);
            if entry.stale {
                if !skip_stale {
                    rows.push(Row { stale: true, parts });
                }
                continue;
            }
            let find = |p: Part| package.part(p).and_then(|f| f.find(&entry.path));
            if Part::ALL[..i].iter().any(|&earlier| find(earlier).is_some()) {
                continue; // already a row, from the first part that has this path
            }
            for (j, &later) in Part::ALL.iter().enumerate().skip(i + 1) {
                parts[j] = find(later);
            }
            rows.push(Row { stale: false, parts });
        }
    }
    rows.sort_by(|a, b| a.path().cmp(b.path()).then(a.stale.cmp(&b.stale)));
    rows
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn index_row(package: &Package, row: &Row, oodle: Option<&dyn OodleDecoder>, opts: &IndexOptions) -> (FileEntry, IndexStats) {
    let mut entry = FileEntry {
        package: package.name.clone(),
        path: row.path().to_string(),
        stale: row.stale,
        h: None,
        b: None,
        f: None,
        header: None,
        error: None,
        export: None,
    };
    let mut stats = IndexStats { files: u64::from(!row.stale), stale: u64::from(row.stale), ..Default::default() };
    let mut need_oodle = false;
    for part in Part::ALL {
        let Some(toc) = row.parts[part as usize] else { continue };
        let mut record =
            PartEntry { offset: toc.offset, compressed_size: toc.compressed_size, size: toc.size, timestamp: toc.timestamp, crc32: None };
        if part == Part::H || opts.hash {
            let file = package.part(part).expect("a row's part exists");
            match file.read(toc, oodle) {
                Ok((data, blocks)) => {
                    stats.parts_read += 1;
                    stats.bytes_read += data.len() as u64;
                    stats.blocks += blocks;
                    if opts.hash {
                        record.crc32 = Some(crc32fast::hash(&data));
                    }
                    if part == Part::H
                        && let Ok(h) = header::parse(&data)
                    {
                        stats.headers += 1;
                        entry.header = Some(HeaderEntry {
                            type_id: h.type_id,
                            hash: hex(&h.hash),
                            sources: if opts.full_headers { h.sources } else { Vec::new() },
                            attributes: opts.full_headers.then_some(h.attributes),
                        });
                    }
                }
                Err(e) => {
                    need_oodle |= matches!(e, Error::Block { source: BlockError::NoOodle { .. }, .. });
                    // The path is already in the entry; keep the part and the cause.
                    let cause = match &e {
                        Error::Block { source, .. } => source.to_string(),
                        other => other.to_string(),
                    };
                    entry.error.get_or_insert(format!("{part}: {cause}"));
                }
            }
        }
        *entry.part_mut(part) = Some(record);
    }
    stats.failed = u64::from(entry.error.is_some());
    stats.need_oodle = u64::from(need_oodle);
    (entry, stats)
}

/// Summarise one package from its TOCs.
pub fn package_info(package: &Package) -> PackageInfo {
    let mut info = PackageInfo {
        name: package.name.clone(),
        category: package.category,
        parts: package.parts().map(|(p, _)| p.letter()).collect(),
        files: 0,
        stale: 0,
        compressed_size: 0,
        size: 0,
    };
    for (part, file) in package.parts() {
        for entry in &file.toc.files {
            if entry.stale {
                info.stale += 1;
                continue;
            }
            // Count a path once, under the first part that has it.
            let earlier = Part::ALL[..part as usize].iter().any(|&p| package.part(p).is_some_and(|f| f.find(&entry.path).is_some()));
            info.files += u64::from(!earlier);
            info.compressed_size += u64::from(entry.compressed_size);
            info.size += u64::from(entry.size);
        }
    }
    info
}

/// Index every package of `cache`. Without an Oodle decoder, files with Oodle blocks are
/// recorded with an error and counted in [`IndexStats::need_oodle`].
pub fn index(cache: &Cache, oodle: Option<&dyn OodleDecoder>, opts: &IndexOptions) -> Result<(Manifest, IndexStats)> {
    if !cache.game.readable() {
        return Err(Error::UnsupportedGame(cache.game));
    }
    let start = Instant::now();
    let mut stats = IndexStats::default();
    let mut files = Vec::new();
    for package in &cache.packages {
        let indexed: Vec<(FileEntry, IndexStats)> =
            rows(package, opts.skip_stale).par_iter().map(|row| index_row(package, row, oodle, opts)).collect();
        files.reserve(indexed.len());
        for (entry, s) in indexed {
            stats += s;
            files.push(entry);
        }
    }
    stats.elapsed = start.elapsed();
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        tool: env!("CARGO_PKG_VERSION").to_string(),
        source: SourceInfo { dir: cache.dir.display().to_string(), game: cache.game },
        packages: cache.packages.iter().map(package_info).collect(),
        files,
    };
    Ok((manifest, stats))
}
