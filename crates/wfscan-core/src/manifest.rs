//! The manifest: a JSON record of every file in a cache, written by `wfscan index` and
//! read by everything else.
//!
//! On disk each file is one line, so a manifest of a few hundred thousand files stays
//! greppable and diffs line by line.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::package::{Category, Game, Part};
use crate::error::{Error, Result, io_err};
use crate::output::write_atomic;
use crate::types::{Kind, kind_of};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// The wfscan version that wrote this.
    pub tool: String,
    pub source: SourceInfo,
    pub packages: Vec<PackageInfo>,
    /// Sorted by package, then path, with stale entries after the live one.
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// The cache folder that was indexed.
    pub dir: String,
    pub game: Game,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub category: Category,
    /// The parts the package has, such as `"HBF"`.
    pub parts: String,
    /// Live files.
    pub files: u64,
    pub stale: u64,
    /// Bytes the live files take in the cache, and decompressed.
    pub compressed_size: u64,
    pub size: u64,
}

/// One internal path in one package: up to three parts, and what its common header says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub package: String,
    pub path: String,
    /// A deleted or replaced copy that the TOC still lists. A stale entry has one part.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<PartEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<PartEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<PartEntry>,
    /// The common header, if the H part has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<HeaderEntry>,
    /// Why a part couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the last `wfscan export` did with this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportRecord>,
}

/// How an export of one file ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Exported; `outputs` lists the files.
    Ok,
    /// The exporter ran and gave up; `reason` says why.
    Failed,
    /// Nothing can export this file (yet); `reason` says what is missing.
    Unsupported,
    /// Unchanged since an earlier export whose outputs are still there.
    Skipped,
}

impl Status {
    pub const ALL: [Status; 4] = [Status::Ok, Status::Failed, Status::Unsupported, Status::Skipped];

    pub fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Failed => "failed",
            Status::Unsupported => "unsupported",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub status: Status,
    /// The exporter and its version, such as `texture@1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exporter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Files written, as internal-style paths under the output folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

impl FileEntry {
    /// Whether `other`, the same path in another manifest, has the same contents as far
    /// as the manifests can tell: the same header type and hash, and for each part the
    /// same sizes and CRC-32, or the same timestamp where either side has no CRC.
    /// Offsets don't count: the launcher moves data around without changing it.
    pub fn same_content(&self, other: &FileEntry) -> bool {
        let header = |f: &FileEntry| f.header.as_ref().map(|h| (h.type_id, h.hash.clone()));
        header(self) == header(other)
            && Part::ALL.iter().all(|&p| match (self.part(p), other.part(p)) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.size == b.size
                        && a.compressed_size == b.compressed_size
                        && match (a.crc32, b.crc32) {
                            (Some(x), Some(y)) => x == y,
                            _ => a.timestamp == b.timestamp,
                        }
                }
                _ => false,
            })
    }

    pub fn part(&self, part: Part) -> Option<&PartEntry> {
        match part {
            Part::H => self.h.as_ref(),
            Part::B => self.b.as_ref(),
            Part::F => self.f.as_ref(),
        }
    }

    pub fn part_mut(&mut self, part: Part) -> &mut Option<PartEntry> {
        match part {
            Part::H => &mut self.h,
            Part::B => &mut self.b,
            Part::F => &mut self.f,
        }
    }

    /// Decompressed bytes over all parts.
    pub fn size(&self) -> u64 {
        Part::ALL.iter().filter_map(|&p| self.part(p)).map(|p| u64::from(p.size)).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartEntry {
    pub offset: u64,
    pub compressed_size: u32,
    pub size: u32,
    /// Windows FILETIME from the TOC.
    pub timestamp: i64,
    /// CRC-32 of the decompressed data (`wfscan index --hash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderEntry {
    /// The file type number; its meaning depends on the game and package category.
    #[serde(rename = "type")]
    pub type_id: u32,
    /// The header's 16-byte hash, in hex.
    pub hash: String,
    /// Paths of the files this one was built from (`wfscan index --full-headers`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// The attribute text (`wfscan index --full-headers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

/// Looks up what a manifest's files are: their package's category, and from that and the
/// header's type number, the kind of asset.
pub struct Classifier<'a> {
    game: Game,
    categories: HashMap<&'a str, Category>,
}

impl Classifier<'_> {
    pub fn category(&self, file: &FileEntry) -> Category {
        self.categories.get(file.package.as_str()).copied().unwrap_or(Category::Unknown)
    }

    pub fn kind(&self, file: &FileEntry) -> Option<Kind> {
        kind_of(self.game, self.category(file), file.header.as_ref()?.type_id)
    }
}

impl Manifest {
    /// Something that tells a file's package category and kind of asset.
    pub fn classifier(&self) -> Classifier<'_> {
        Classifier { game: self.source.game, categories: self.packages.iter().map(|p| (p.name.as_str(), p.category)).collect() }
    }

    /// What kind of asset each file is, in the order of `files`.
    pub fn kinds(&self) -> Vec<Option<Kind>> {
        let classifier = self.classifier();
        self.files.iter().map(|f| classifier.kind(f)).collect()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(io_err(path))?;
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let Version { version } = serde_json::from_slice(&bytes)?;
        if version != MANIFEST_VERSION {
            return Err(Error::ManifestVersion { found: version, expected: MANIFEST_VERSION });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, |w| self.write(w))
    }

    /// The JSON, with one package and one file per line.
    pub fn write(&self, w: &mut dyn Write) -> io::Result<()> {
        fn lines<T: Serialize>(w: &mut dyn Write, items: &[T]) -> io::Result<()> {
            for (i, item) in items.iter().enumerate() {
                let sep = if i + 1 < items.len() { "," } else { "" };
                writeln!(w, "{}{sep}", serde_json::to_string(item)?)?;
            }
            Ok(())
        }
        writeln!(w, "{{")?;
        writeln!(w, "\"version\": {},", self.version)?;
        writeln!(w, "\"tool\": {},", serde_json::to_string(&self.tool)?)?;
        writeln!(w, "\"source\": {},", serde_json::to_string(&self.source)?)?;
        writeln!(w, "\"packages\": [")?;
        lines(w, &self.packages)?;
        writeln!(w, "],")?;
        writeln!(w, "\"files\": [")?;
        lines(w, &self.files)?;
        writeln!(w, "]")?;
        writeln!(w, "}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let part = |offset| PartEntry { offset, compressed_size: 10, size: 20, timestamp: 133_000_000_000_000_000, crc32: None };
        Manifest {
            version: MANIFEST_VERSION,
            tool: "0.1.0".into(),
            source: SourceInfo { dir: "C:\\Games\\Cache.Windows".into(), game: Game::Warframe },
            packages: vec![PackageInfo {
                name: "Misc".into(),
                category: Category::Misc,
                parts: "HB".into(),
                files: 2,
                stale: 1,
                compressed_size: 30,
                size: 60,
            }],
            files: vec![
                FileEntry {
                    package: "Misc".into(),
                    path: "/Lotus/A \"quoted\".fbx".into(),
                    stale: false,
                    h: Some(part(0)),
                    b: Some(PartEntry { crc32: Some(0xdead_beef), ..part(5) }),
                    f: None,
                    header: Some(HeaderEntry {
                        type_id: 269,
                        hash: "00ff".repeat(8),
                        sources: vec!["/Lotus/A.fbx".into()],
                        attributes: Some("line 1\nline 2".into()),
                    }),
                    error: None,
                    export: Some(ExportRecord {
                        status: Status::Ok,
                        exporter: Some("model@1".into()),
                        reason: None,
                        outputs: vec!["/Lotus/A.gltf".into(), "/Lotus/A.bin".into()],
                    }),
                },
                FileEntry {
                    package: "Misc".into(),
                    path: "/Lotus/B.lua".into(),
                    stale: true,
                    h: Some(part(9)),
                    b: None,
                    f: None,
                    header: None,
                    error: Some("no block header at 0x10".into()),
                    export: None,
                },
            ],
        }
    }

    #[test]
    fn same_content_ignores_offsets_and_prefers_crcs() {
        let m = sample();
        let a = &m.files[0];
        assert!(a.same_content(a));
        assert!(!a.same_content(&m.files[1]));

        let mut moved = a.clone();
        moved.h.as_mut().unwrap().offset += 1000;
        moved.export = None;
        assert!(a.same_content(&moved));

        let mut retimed = a.clone();
        retimed.b.as_mut().unwrap().timestamp += 1;
        assert!(a.same_content(&retimed), "both have a CRC for B, so its timestamp doesn't matter");
        retimed.h.as_mut().unwrap().timestamp += 1;
        assert!(!a.same_content(&retimed), "H has no CRC, so its timestamp does");

        let mut edits: Vec<FileEntry> = vec![a.clone(); 5];
        edits[0].b.as_mut().unwrap().crc32 = Some(1);
        edits[1].h.as_mut().unwrap().size += 1;
        edits[2].header.as_mut().unwrap().hash = "ff".repeat(16);
        edits[3].b = None;
        edits[4].header = None;
        for e in &edits {
            assert!(!a.same_content(e), "{e:?}");
        }
    }

    #[test]
    fn kinds_come_from_the_package_category() {
        let mut m = sample();
        assert_eq!(m.kinds(), [Some(Kind::Model), None], "B.lua has no header");
        m.files[0].header.as_mut().unwrap().type_id = 9999;
        assert_eq!(m.kinds(), [None, None]);
        m.files[0].header.as_mut().unwrap().type_id = 269;
        m.packages[0].category = Category::Texture;
        assert_eq!(m.kinds(), [None, None]);
    }

    #[test]
    fn round_trips_with_one_file_per_line() {
        let m = sample();
        let mut text = Vec::new();
        m.write(&mut text).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert_eq!(text.lines().count(), 9 + m.packages.len() + m.files.len(), "{text}");
        assert!(text.lines().any(|l| l.starts_with("{\"package\":\"Misc\",\"path\":\"/Lotus/B.lua\",\"stale\":true")), "{text}");
        assert_eq!(serde_json::from_str::<Manifest>(&text).unwrap(), m);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.json");
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path).unwrap(), m);
    }

    #[test]
    fn empty_lists_are_valid_json() {
        let m = Manifest { packages: vec![], files: vec![], ..sample() };
        let mut text = Vec::new();
        m.write(&mut text).unwrap();
        assert_eq!(serde_json::from_slice::<Manifest>(&text).unwrap(), m);
    }

    #[test]
    fn other_versions_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.json");
        std::fs::write(&path, br#"{"version": 99, "anything": true}"#).unwrap();
        assert!(matches!(Manifest::load(&path), Err(Error::ManifestVersion { found: 99, expected: MANIFEST_VERSION })));
        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(Manifest::load(&path), Err(Error::ManifestParse(_))));
    }
}
