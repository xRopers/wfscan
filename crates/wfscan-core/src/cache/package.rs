//! A cache folder as packages.
//!
//! `Cache.Windows` holds files named `<part>.<package>.toc` and `.cache`. A package
//! (`Misc`, `Texture`, `Misc_en`, ...) has up to three parts sharing internal paths:
//! `H` (headers: the common header and each file's own header), `B` (body data) and `F`
//! (full-resolution or streamed data). `H` is always there; `B` and `F` are optional.
//!
//! Source: LotusLib's README; the part and package names are those of a September 2026
//! Warframe install.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use super::block::{self, BlockCounts, OodleDecoder};
use super::toc::{self, Toc, TocEntry};
use crate::error::{Error, Result, io_err};

/// One of a package's three cache files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Part {
    H,
    B,
    F,
}

impl Part {
    pub const ALL: [Part; 3] = [Part::H, Part::B, Part::F];

    pub fn letter(self) -> char {
        match self {
            Part::H => 'H',
            Part::B => 'B',
            Part::F => 'F',
        }
    }

    fn from_letter(c: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| c.len() == 1 && c.starts_with(p.letter()))
    }
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.letter())
    }
}

/// Which game a cache is from, which decides how its entries are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Game {
    Warframe,
    /// Warframe before the September 2020 "Ensmallening": no block headers, LZF only.
    WarframePreEnsmallening,
    Soulframe,
    Unknown,
}

impl Game {
    /// Whether [`PartFile::read`] knows this game's compression. An unknown game is tried
    /// as current Warframe.
    pub fn readable(self) -> bool {
        self != Game::WarframePreEnsmallening
    }
}

impl fmt::Display for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Game::Warframe => "Warframe",
            Game::WarframePreEnsmallening => "Warframe before the Ensmallening",
            Game::Soulframe => "Soulframe",
            Game::Unknown => "an unknown game",
        })
    }
}

/// What kind of package this is, from the start of its name. File types are numbered
/// per category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    AnimRetarget,
    CharacterCodes,
    Emblem,
    Font,
    LightMap,
    Misc,
    Shader,
    ShaderDictionary,
    ShaderPermutation,
    Texture,
    VideoTexture,
    Unknown,
}

impl Category {
    pub fn of(package: &str) -> Self {
        // Longer names first: "ShaderPermutationDx11" also starts with "Shader".
        const PREFIXES: [(&str, Category); 11] = [
            ("AnimRetarget", Category::AnimRetarget),
            ("CharacterCodes", Category::CharacterCodes),
            ("Emblem", Category::Emblem),
            ("Font", Category::Font),
            ("LightMap", Category::LightMap),
            ("Misc", Category::Misc),
            ("ShaderDictionary", Category::ShaderDictionary),
            ("ShaderPermutation", Category::ShaderPermutation),
            ("Shader", Category::Shader),
            ("Texture", Category::Texture),
            ("VideoTexture", Category::VideoTexture),
        ];
        PREFIXES.iter().find(|(p, _)| package.starts_with(p)).map_or(Category::Unknown, |&(_, c)| c)
    }

    /// The category as the game spells it in package names.
    pub fn name(self) -> &'static str {
        match self {
            Category::AnimRetarget => "AnimRetarget",
            Category::CharacterCodes => "CharacterCodes",
            Category::Emblem => "Emblem",
            Category::Font => "Font",
            Category::LightMap => "LightMap",
            Category::Misc => "Misc",
            Category::Shader => "Shader",
            Category::ShaderDictionary => "ShaderDictionary",
            Category::ShaderPermutation => "ShaderPermutation",
            Category::Texture => "Texture",
            Category::VideoTexture => "VideoTexture",
            Category::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One `.toc` + `.cache` pair.
pub struct PartFile {
    pub toc_path: PathBuf,
    pub cache_path: PathBuf,
    pub toc: Toc,
    /// `None` if the `.cache` file is missing or empty.
    cache: Option<Mmap>,
    /// Live entries, as indexes into `toc.files`, sorted by path.
    by_path: OnceLock<Vec<u32>>,
}

impl PartFile {
    fn open(toc_path: &Path) -> Result<Self> {
        let toc_bytes = std::fs::read(toc_path).map_err(io_err(toc_path))?;
        let toc = toc::parse(&toc_bytes).map_err(|source| Error::Toc { path: toc_path.to_path_buf(), source })?;
        let cache_path = toc_path.with_extension("cache");
        let cache = match File::open(&cache_path) {
            Ok(file) if file.metadata().map_err(io_err(&cache_path))?.len() > 0 => {
                // SAFETY: the map is read-only. If another process truncates the file
                // while it is mapped, reads fault; wfscan is not meant to run while the
                // game or its launcher is writing the cache.
                Some(unsafe { Mmap::map(&file) }.map_err(io_err(&cache_path))?)
            }
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_err(&cache_path)(e)),
        };
        Ok(Self { toc_path: toc_path.to_path_buf(), cache_path, toc, cache, by_path: OnceLock::new() })
    }

    /// Size of the `.cache` file.
    pub fn cache_len(&self) -> u64 {
        self.cache.as_ref().map_or(0, |m| m.len() as u64)
    }

    /// The live entry at `path`.
    pub fn find(&self, path: &str) -> Option<&TocEntry> {
        let files = &self.toc.files;
        let sorted = self.by_path.get_or_init(|| {
            let mut live: Vec<u32> = (0..files.len() as u32).filter(|&i| !files[i as usize].stale).collect();
            live.sort_unstable_by(|&a, &b| files[a as usize].path.cmp(&files[b as usize].path));
            live
        });
        let i = sorted.binary_search_by(|&i| files[i as usize].path.as_str().cmp(path)).ok()?;
        Some(&files[sorted[i] as usize])
    }

    /// An entry's bytes as they sit in the cache, still compressed.
    pub fn compressed(&self, entry: &TocEntry) -> Result<&[u8]> {
        let cache = self.cache.as_deref().unwrap_or_default();
        usize::try_from(entry.offset)
            .ok()
            .and_then(|start| cache.get(start..start.checked_add(entry.compressed_size as usize)?))
            .ok_or_else(|| Error::OutOfRange { path: entry.path.clone(), cache: self.cache_path.clone() })
    }

    /// An entry's contents.
    pub fn read(&self, entry: &TocEntry, oodle: Option<&dyn OodleDecoder>) -> Result<(Vec<u8>, BlockCounts)> {
        block::decompress(self.compressed(entry)?, entry.size as usize, oodle)
            .map_err(|source| Error::Block { path: entry.path.clone(), source })
    }
}

pub struct Package {
    pub name: String,
    pub category: Category,
    parts: [Option<PartFile>; 3],
}

impl Package {
    pub fn part(&self, part: Part) -> Option<&PartFile> {
        self.parts[part as usize].as_ref()
    }

    /// The parts this package has, in H, B, F order.
    pub fn parts(&self) -> impl Iterator<Item = (Part, &PartFile)> {
        Part::ALL.into_iter().filter_map(|p| Some((p, self.part(p)?)))
    }
}

/// An opened cache folder.
pub struct Cache {
    pub dir: PathBuf,
    pub game: Game,
    /// Sorted by name.
    pub packages: Vec<Package>,
}

impl Cache {
    /// Read every TOC in `dir` and map the cache files.
    pub fn open(dir: &Path) -> Result<Self> {
        let mut packages: Vec<Package> = Vec::new();
        let mut names: Vec<PathBuf> =
            std::fs::read_dir(dir).map_err(io_err(dir))?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>().map_err(io_err(dir))?;
        names.sort();
        for path in names {
            let Some((part, name)) = split_toc_name(&path) else { continue };
            let file = PartFile::open(&path)?;
            let i = match packages.iter().position(|p| p.name == name) {
                Some(i) => i,
                None => {
                    packages.push(Package { category: Category::of(&name), name, parts: [None, None, None] });
                    packages.len() - 1
                }
            };
            packages[i].parts[part as usize] = Some(file);
        }
        if packages.is_empty() {
            return Err(Error::NoPackages(dir.to_path_buf()));
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        let game = guess_game(&packages);
        Ok(Self { dir: dir.to_path_buf(), game, packages })
    }

    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Refuse a cache that another program has open for writing: the game or its launcher
    /// may be changing it under us. Only Windows can tell; elsewhere this always passes.
    pub fn check_not_in_use(&self) -> Result<()> {
        let files = self.packages.iter().flat_map(|p| p.parts()).flat_map(|(_, f)| [&f.toc_path, &f.cache_path]);
        match files.into_iter().find(|path| open_for_writing_elsewhere(path)) {
            Some(path) => Err(Error::InUse(path.clone())),
            None => Ok(()),
        }
    }
}

/// Whether some other handle has `path` open with write access: opening it while denying
/// writers then fails with a sharing violation.
#[cfg(windows)]
fn open_for_writing_elsewhere(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let opened = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path);
    matches!(opened, Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION))
}

#[cfg(not(windows))]
fn open_for_writing_elsewhere(_path: &Path) -> bool {
    false
}

/// `H.Misc_en.toc` is part H of package `Misc_en`.
fn split_toc_name(path: &Path) -> Option<(Part, String)> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".toc")?;
    let (part, name) = stem.split_once('.')?;
    (!name.is_empty()).then_some((Part::from_letter(part)?, name.to_string()))
}

/// How many compressed entries to look at when telling the two Warframe eras apart.
const ERA_SAMPLE: usize = 64;

/// Tell the game from its root folders (`/Lotus` or `/SF`), and Warframe's era from
/// whether compressed entries start with a block header.
fn guess_game(packages: &[Package]) -> Game {
    let roots = |root: &str| packages.iter().flat_map(|p| p.part(Part::H)).any(|h| h.toc.files.iter().any(|f| f.path.starts_with(root)));
    if roots("/SF/") {
        return Game::Soulframe;
    }
    if !roots("/Lotus/") {
        return Game::Unknown;
    }
    let Some(h) = packages.iter().find(|p| p.name == "Misc").and_then(|p| p.part(Part::H)) else {
        return Game::Warframe;
    };
    let sample: Vec<bool> = h
        .toc
        .files
        .iter()
        .filter(|f| !f.stale && f.compressed_size != f.size)
        .filter_map(|f| h.compressed(f).ok())
        .take(ERA_SAMPLE)
        .map(block::has_block_header)
        .collect();
    if !sample.is_empty() && sample.iter().all(|&has| !has) { Game::WarframePreEnsmallening } else { Game::Warframe }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_names() {
        assert_eq!(split_toc_name(Path::new("x/H.Misc.toc")), Some((Part::H, "Misc".into())));
        assert_eq!(split_toc_name(Path::new("F.Misc_en.toc")), Some((Part::F, "Misc_en".into())));
        assert_eq!(split_toc_name(Path::new("B.Misc.cache")), None);
        assert_eq!(split_toc_name(Path::new("X.Misc.toc")), None);
        assert_eq!(split_toc_name(Path::new("HB.Misc.toc")), None);
        assert_eq!(split_toc_name(Path::new("H..toc")), None);
        assert_eq!(split_toc_name(Path::new("notes.toc")), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_writer_elsewhere_is_noticed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("H.Misc.cache");
        std::fs::write(&path, b"data").unwrap();
        assert!(!open_for_writing_elsewhere(&path));
        let reader = File::open(&path).unwrap();
        assert!(!open_for_writing_elsewhere(&path), "readers are fine");
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(open_for_writing_elsewhere(&path));
        drop((reader, writer));
        assert!(!open_for_writing_elsewhere(&path));
        assert!(!open_for_writing_elsewhere(&tmp.path().join("missing")));
    }

    #[test]
    fn categories() {
        assert_eq!(Category::of("Misc_en"), Category::Misc);
        assert_eq!(Category::of("ShaderDx11"), Category::Shader);
        assert_eq!(Category::of("ShaderPermutationDx11"), Category::ShaderPermutation);
        assert_eq!(Category::of("ShaderDictionaryDx11"), Category::ShaderDictionary);
        assert_eq!(Category::of("VideoTextureStreaming"), Category::VideoTexture);
        assert_eq!(Category::of("Texture_en"), Category::Texture);
        assert_eq!(Category::of("Whatever"), Category::Unknown);
    }
}
