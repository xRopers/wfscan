//! `.toc` files: the directory of one `.cache` file.
//!
//! Layout, little-endian:
//! - 8-byte header: magic `4E C6 67 18`, then a u32 version (20).
//! - 96-byte entries to the end of the file:
//!   `i64 offset, i64 timestamp, i32 compressed size, i32 size, i32 reserved,
//!   i32 parent directory, char name[64]` (NUL-padded).
//! - An entry with offset -1 is a directory. Directories are numbered from 1 in the order
//!   they appear; 0 is the root, which has no entry. `parent directory` is such a number.
//! - A file with timestamp 0 is a stale copy: deleted or replaced, with its bytes possibly
//!   still in the cache. Timestamps are Windows FILETIMEs.
//!
//! Sources: the field order comes from LotusLib's description of the entry; the header,
//! entry size, directory numbering and timestamps were checked against a September 2026
//! Warframe install.

pub const MAGIC: [u8; 4] = [0x4e, 0xc6, 0x67, 0x18];
pub const VERSION: u32 = 20;

const HEADER_LEN: usize = 8;
const ENTRY_LEN: usize = 96;
const NAME_LEN: usize = 64;

/// One file in a `.cache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// Full internal path, starting with `/`.
    pub path: String,
    /// Where the entry's bytes start in the `.cache` file.
    pub offset: u64,
    pub compressed_size: u32,
    pub size: u32,
    /// Windows FILETIME; 0 for a stale entry.
    pub timestamp: i64,
    /// Deleted or replaced by a newer entry.
    pub stale: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toc {
    /// Files in the order the TOC lists them.
    pub files: Vec<TocEntry>,
    /// Directories, not counting the root.
    pub dir_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TocError {
    #[error("not a TOC file (bad magic)")]
    BadMagic,
    #[error("TOC version {0} is not supported (this build reads version {known})", known = VERSION)]
    Version(u32),
    #[error("truncated: {0} bytes left over after the last whole entry")]
    Truncated(usize),
    #[error("entry {index}: {reason}")]
    Entry { index: usize, reason: String },
}

struct RawEntry<'a> {
    offset: i64,
    timestamp: i64,
    compressed_size: i32,
    size: i32,
    parent: i32,
    name: &'a [u8],
}

fn raw_entry(e: &[u8; ENTRY_LEN]) -> RawEntry<'_> {
    let i64_at = |o: usize| i64::from_le_bytes(e[o..o + 8].try_into().unwrap());
    let i32_at = |o: usize| i32::from_le_bytes(e[o..o + 4].try_into().unwrap());
    let name = &e[32..32 + NAME_LEN];
    let len = name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    RawEntry {
        offset: i64_at(0),
        timestamp: i64_at(8),
        compressed_size: i32_at(16),
        size: i32_at(20),
        parent: i32_at(28),
        name: &name[..len],
    }
}

/// Parse a whole `.toc` file. An empty file is an empty TOC.
pub fn parse(data: &[u8]) -> Result<Toc, TocError> {
    if data.is_empty() {
        return Ok(Toc::default());
    }
    if data.len() < HEADER_LEN || data[..4] != MAGIC {
        return Err(TocError::BadMagic);
    }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != VERSION {
        return Err(TocError::Version(version));
    }
    let (body, rest) = data[HEADER_LEN..].as_chunks::<ENTRY_LEN>();
    if !rest.is_empty() {
        return Err(TocError::Truncated(rest.len()));
    }
    let entries: Vec<RawEntry> = body.iter().map(raw_entry).collect();
    let err = |index: usize, reason: String| TocError::Entry { index, reason };

    // Directories first: a parent may be listed after its children.
    let dirs: Vec<(usize, &RawEntry)> = entries.iter().enumerate().filter(|(_, e)| e.offset == -1).collect();
    let parent_of = |index: usize, e: &RawEntry| match usize::try_from(e.parent) {
        Ok(p) if p <= dirs.len() => Ok(p),
        _ => Err(err(index, format!("parent directory {} does not exist", e.parent))),
    };
    // dir_paths[n] is the path of directory n; the root's is empty.
    let mut dir_paths: Vec<Option<String>> = vec![None; dirs.len() + 1];
    dir_paths[0] = Some(String::new());
    for start in 1..=dirs.len() {
        // Walk up to the nearest directory whose path is known, then fill in on the way down.
        let mut chain = Vec::new();
        let mut n = start;
        while dir_paths[n].is_none() {
            let (index, e) = dirs[n - 1];
            if chain.len() > dirs.len() {
                return Err(err(index, "directory is its own ancestor".into()));
            }
            chain.push(n);
            n = parent_of(index, e)?;
        }
        let mut path = dir_paths[n].clone().unwrap();
        for &n in chain.iter().rev() {
            path.push('/');
            path.push_str(&String::from_utf8_lossy(dirs[n - 1].1.name));
            dir_paths[n] = Some(path.clone());
        }
    }

    let mut files = Vec::with_capacity(entries.len() - dirs.len());
    for (index, e) in entries.iter().enumerate().filter(|(_, e)| e.offset != -1) {
        let parent = parent_of(index, e)?;
        let (Ok(offset), Ok(compressed_size), Ok(size)) =
            (u64::try_from(e.offset), u32::try_from(e.compressed_size), u32::try_from(e.size))
        else {
            return Err(err(index, "negative offset or size".into()));
        };
        let dir = dir_paths[parent].as_deref().unwrap();
        files.push(TocEntry {
            path: format!("{dir}/{}", String::from_utf8_lossy(e.name)),
            offset,
            compressed_size,
            size,
            timestamp: e.timestamp,
            stale: e.timestamp == 0,
        });
    }
    Ok(Toc { files, dir_count: dirs.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: i64, timestamp: i64, comp: i32, size: i32, parent: i32, name: &str) -> Vec<u8> {
        let mut e = Vec::with_capacity(ENTRY_LEN);
        e.extend_from_slice(&offset.to_le_bytes());
        e.extend_from_slice(&timestamp.to_le_bytes());
        e.extend_from_slice(&comp.to_le_bytes());
        e.extend_from_slice(&size.to_le_bytes());
        e.extend_from_slice(&0i32.to_le_bytes());
        e.extend_from_slice(&parent.to_le_bytes());
        let mut n = [0u8; NAME_LEN];
        n[..name.len()].copy_from_slice(name.as_bytes());
        e.extend_from_slice(&n);
        e
    }

    fn dir(parent: i32, name: &str) -> Vec<u8> {
        entry(-1, -1, -1, -1, parent, name)
    }

    fn toc(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut t = MAGIC.to_vec();
        t.extend_from_slice(&VERSION.to_le_bytes());
        t.extend(entries.iter().flatten());
        t
    }

    #[test]
    fn files_get_full_paths() {
        let long = "x".repeat(NAME_LEN);
        let t = toc(&[
            dir(0, "Lotus"),
            dir(1, "Scripts"),
            entry(100, 5, 10, 20, 2, "A.lua"),
            entry(0, 5, 3, 3, 0, "root.bin"),
            entry(200, 0, 7, 9, 2, "A.lua"),
            entry(300, 5, 1, 1, 1, &long),
        ]);
        let parsed = parse(&t).unwrap();
        assert_eq!(parsed.dir_count, 2);
        let got: Vec<_> = parsed.files.iter().map(|f| (f.path.as_str(), f.offset, f.compressed_size, f.size, f.stale)).collect();
        let long_path = format!("/Lotus/{long}");
        assert_eq!(
            got,
            [
                ("/Lotus/Scripts/A.lua", 100, 10, 20, false),
                ("/root.bin", 0, 3, 3, false),
                ("/Lotus/Scripts/A.lua", 200, 7, 9, true),
                (long_path.as_str(), 300, 1, 1, false),
            ]
        );
    }

    #[test]
    fn a_parent_may_come_after_its_child() {
        let t = toc(&[dir(2, "Child"), dir(0, "Top"), entry(0, 1, 1, 1, 1, "f")]);
        assert_eq!(parse(&t).unwrap().files[0].path, "/Top/Child/f");
    }

    #[test]
    fn empty_and_header_only() {
        assert_eq!(parse(&[]).unwrap(), Toc::default());
        assert_eq!(parse(&toc(&[])).unwrap(), Toc::default());
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(parse(b"nope"), Err(TocError::BadMagic));
        assert_eq!(parse(b"not a toc file at all"), Err(TocError::BadMagic));
        let mut wrong_version = toc(&[]);
        wrong_version[4] = 99;
        assert_eq!(parse(&wrong_version), Err(TocError::Version(99)));
        let mut cut = toc(&[dir(0, "Lotus")]);
        cut.truncate(cut.len() - 10);
        assert_eq!(parse(&cut), Err(TocError::Truncated(ENTRY_LEN - 10)));
    }

    #[test]
    fn rejects_bad_entries() {
        let bad_parent = toc(&[dir(0, "Lotus"), entry(0, 1, 1, 1, 7, "f")]);
        assert!(matches!(parse(&bad_parent), Err(TocError::Entry { index: 1, .. })));
        let negative_parent = toc(&[entry(0, 1, 1, 1, -3, "f")]);
        assert!(matches!(parse(&negative_parent), Err(TocError::Entry { index: 0, .. })));
        let cycle = toc(&[dir(2, "A"), dir(1, "B")]);
        assert!(matches!(parse(&cycle), Err(TocError::Entry { .. })));
        let negative_size = toc(&[entry(0, 1, -5, 1, 0, "f")]);
        assert!(matches!(parse(&negative_size), Err(TocError::Entry { index: 0, .. })));
    }
}
