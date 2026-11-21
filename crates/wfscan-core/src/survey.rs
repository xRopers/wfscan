//! A first look at every file of one type, where each new reader starts: how big the type
//! header and the B and F parts are, which header bytes all files share, which header
//! fields give a part's size, and which attribute lines occur.
//!
//! Size fields are found by trying every u32 (little-endian, at any byte offset) of the
//! first and the last bytes of the type header (the last for headers that start with
//! strings, where fields shift from file to file). A field, or two together, "gives" a
//! part's size when `size = k * field + c` (or `a * x + b * y + c`) holds with whole
//! multiples for at least half the files, over at least three different values; the
//! survey says for how many. Half, not all, because one type often has several layouts
//! (models keep everything in B, or split it between B and F).

use std::collections::BTreeMap;

use rayon::prelude::*;
use serde::Serialize;

use crate::cache::block::OodleDecoder;
use crate::cache::header;
use crate::cache::package::{Cache, Part};
use crate::manifest::Manifest;

#[derive(Debug, Clone, Default)]
pub struct SurveyOptions {
    /// The type number.
    pub type_id: u32,
    /// Only this package.
    pub package: Option<String>,
    /// Only paths starting with this.
    pub path: Option<String>,
    /// How many leading type-header bytes to compare and search for size fields.
    pub bytes: usize,
    /// How many sample paths to list.
    pub samples: usize,
}

/// Sizes of one part over the files that have it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SizeStats {
    /// Files with this part.
    pub files: u64,
    pub min: u64,
    pub median: u64,
    pub max: u64,
}

impl SizeStats {
    fn of(mut sizes: Vec<u64>) -> Self {
        if sizes.is_empty() {
            return Self::default();
        }
        sizes.sort_unstable();
        Self { files: sizes.len() as u64, min: sizes[0], median: sizes[sizes.len() / 2], max: sizes[sizes.len() - 1] }
    }
}

/// One u32 of the type header and what it is multiplied by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Term {
    /// Offset into the type header (after the common header); negative counts back from
    /// its end (-4 is the last u32).
    pub offset: i64,
    pub multiple: u64,
}

/// One or two type-header u32s that a part's size follows exactly in every file:
/// `size = sum(multiple * value) + plus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SizeField {
    /// `H` (the type header's own length), `B`, `F` or `B+F` (together, a missing part counting as 0).
    pub part: String,
    pub terms: Vec<Term>,
    pub plus: i64,
    /// Files it holds for: at least half of them.
    pub files: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Survey {
    pub type_id: u32,
    /// Files of the type that matched, and how many of those couldn't be read.
    pub files: u64,
    pub unreadable: u64,
    /// Files per package.
    pub packages: BTreeMap<String, u64>,
    /// The type header (H after the common header), B and F.
    pub h: SizeStats,
    pub b: SizeStats,
    pub f: SizeStats,
    /// The first bytes of the type header as hex, `??` where files differ and `..` past
    /// the shortest one.
    pub common: Vec<String>,
    pub size_fields: Vec<SizeField>,
    /// Attribute keys (text before `=` on a line of the attribute text) and how many
    /// files have each, most common first.
    pub attributes: Vec<(String, u64)>,
    /// Some paths: the smallest files first, which are the easiest to read by hand.
    pub samples: Vec<String>,
}

struct Sample {
    path: String,
    body: Vec<u8>,
    b: Option<u64>,
    f: Option<u64>,
    keys: Vec<String>,
}

/// Survey the files of `opts.type_id` that `manifest` lists (live ones only). Reads only
/// H parts; B and F sizes come from the manifest.
pub fn survey(cache: &Cache, manifest: &Manifest, oodle: Option<&dyn OodleDecoder>, opts: &SurveyOptions) -> Survey {
    let bytes = opts.bytes.max(4);
    let picked: Vec<_> = manifest
        .files
        .iter()
        .filter(|f| !f.stale && f.header.as_ref().is_some_and(|h| h.type_id == opts.type_id))
        .filter(|f| opts.package.as_ref().is_none_or(|p| *p == f.package))
        .filter(|f| opts.path.as_ref().is_none_or(|p| f.path.starts_with(p.as_str())))
        .collect();
    let mut packages = BTreeMap::new();
    for f in &picked {
        *packages.entry(f.package.clone()).or_insert(0) += 1;
    }
    let read: Vec<Option<Sample>> = picked
        .par_iter()
        .map(|f| {
            let package = cache.package(&f.package)?;
            let entry = package.part(Part::H)?.find(&f.path)?;
            let (h, _) = package.part(Part::H)?.read(entry, oodle).ok()?;
            let header = header::parse(&h).ok()?;
            let keys = header.attributes.lines().filter_map(|l| Some(l.split_once('=')?.0.trim().to_string())).filter(|k| !k.is_empty()).collect();
            let size = |p: Part| f.part(p).map(|e| u64::from(e.size));
            Some(Sample { path: f.path.clone(), body: h[header.len..].to_vec(), b: size(Part::B), f: size(Part::F), keys })
        })
        .collect();
    let unreadable = read.iter().filter(|s| s.is_none()).count() as u64;
    let mut samples: Vec<Sample> = read.into_iter().flatten().collect();

    let mut keys: BTreeMap<String, u64> = BTreeMap::new();
    for s in &samples {
        let mut seen: Vec<&String> = s.keys.iter().collect();
        seen.sort();
        seen.dedup();
        for k in seen {
            *keys.entry(k.clone()).or_insert(0) += 1;
        }
    }
    let mut attributes: Vec<(String, u64)> = keys.into_iter().collect();
    attributes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let shortest = samples.iter().map(|s| s.body.len()).min().unwrap_or(0);
    let common = (0..bytes.min(samples.iter().map(|s| s.body.len()).max().unwrap_or(0)))
        .map(|i| {
            if i >= shortest {
                return "..".to_string();
            }
            let first = samples[0].body[i];
            if samples.iter().all(|s| s.body[i] == first) { format!("{first:02x}") } else { "??".to_string() }
        })
        .collect();

    let size_fields = size_fields(&samples, bytes.min(shortest));
    let stats = |get: &dyn Fn(&Sample) -> Option<u64>| SizeStats::of(samples.iter().filter_map(get).collect());
    let (h, b, f) = (stats(&|s| Some(s.body.len() as u64)), stats(&|s| s.b), stats(&|s| s.f));

    samples.sort_by_key(|s| (s.body.len() as u64 + s.b.unwrap_or(0) + s.f.unwrap_or(0), s.path.clone()));
    Survey {
        type_id: opts.type_id,
        files: picked.len() as u64,
        unreadable,
        packages,
        h,
        b,
        f,
        common,
        size_fields,
        attributes,
        samples: samples.iter().take(opts.samples).map(|s| s.path.clone()).collect(),
    }
}

/// Largest multiple tried; real ones are vertex and index sizes.
const MAX_MULTIPLE: i128 = 1 << 12;
/// Candidate multiples come from this many leading files.
const LEAD: usize = 12;
/// Pairs are only searched among this many fields, and at most this many fits are listed
/// per part, for single fields and for pairs each.
const MAX_PAIR_FIELDS: usize = 300;
const MAX_LISTED: usize = 16;

/// A part's size in one file, if it has the part.
type PartSize<'a> = &'a dyn Fn(&Sample) -> Option<u64>;

/// The u32s of the first and last `len` type-header bytes that a part's size follows in
/// at least half the files, alone or two together; best fits first.
fn size_fields(samples: &[Sample], len: usize) -> Vec<SizeField> {
    let mut found = Vec::new();
    if samples.len() < 3 || len < 4 {
        return found;
    }
    // Offsets from the end only add something when header lengths differ.
    let fixed = samples.iter().all(|s| s.body.len() == samples[0].body.len());
    let from_start = (0..=len - 4).map(|o| o as i64);
    let from_end = (4..=if fixed { 3 } else { len }).map(|o| -(o as i64));
    // Each offset with at least three different values, and its value per file.
    let fields: Vec<(i64, Vec<i128>)> = from_start
        .chain(from_end)
        .filter_map(|offset| {
            let at = |s: &Sample| if offset >= 0 { offset as usize } else { s.body.len() - offset.unsigned_abs() as usize };
            let values: Vec<i128> = samples.iter().map(|s| i128::from(u32::from_le_bytes(s.body[at(s)..at(s) + 4].try_into().unwrap()))).collect();
            (distinct(values.iter().copied()) >= 3).then_some((offset, values))
        })
        .collect();
    // B+F only says something new when some files have both.
    let both = samples.iter().any(|s| s.b.is_some() && s.f.is_some());
    let parts: [(&str, PartSize); 4] =
        [("H", &|s| Some(s.body.len() as u64)), ("B", &|s| s.b), ("F", &|s| s.f), ("B+F", &|s| Some(s.b.unwrap_or(0) + s.f.unwrap_or(0)))];
    for (name, size) in parts {
        if name == "B+F" && !both {
            continue;
        }
        let Some(sizes) = samples.iter().map(|s| size(s).map(i128::from)).collect::<Option<Vec<i128>>>() else { continue };
        let mut singles: Vec<(SizeField, u64)> = Vec::new();
        let mut best_alone = BTreeMap::new();
        for (offset, values) in &fields {
            let Some((k, c, files)) = fit_single(values, &sizes) else { continue };
            best_alone.insert(*offset, files);
            singles.push((SizeField { part: name.into(), terms: vec![Term { offset: *offset, multiple: k as u64 }], plus: c as i64, files }, files));
        }
        // Pairs, among the fields that don't give the size for every file alone, and only
        // where the pair fits more files than either field does alone.
        let all = samples.len() as u64;
        let rest: Vec<&(i64, Vec<i128>)> =
            fields.iter().filter(|(o, _)| best_alone.get(o) != Some(&all)).take(MAX_PAIR_FIELDS).collect();
        let mut pairs: Vec<(SizeField, u64)> = Vec::new();
        for (i, (o1, v1)) in rest.iter().enumerate() {
            for (o2, v2) in &rest[i + 1..] {
                if (*o1 >= 0) == (*o2 >= 0) && (o1 - o2).abs() < 4 {
                    continue;
                }
                let Some((a, b, c, files)) = fit_pair(v1, v2, &sizes) else { continue };
                let alone = best_alone.get(o1).copied().unwrap_or(0).max(best_alone.get(o2).copied().unwrap_or(0));
                if files > alone {
                    let mut terms = vec![Term { offset: *o1, multiple: a as u64 }, Term { offset: *o2, multiple: b as u64 }];
                    terms.sort_by_key(|t| t.offset);
                    pairs.push((SizeField { part: name.into(), terms, plus: c as i64, files }, files));
                }
            }
        }
        for mut list in [singles, pairs] {
            list.sort_by_key(|(_, files)| std::cmp::Reverse(*files));
            found.extend(list.into_iter().take(MAX_LISTED).map(|(f, _)| f));
        }
    }
    found
}

fn distinct(values: impl Iterator<Item = i128>) -> usize {
    let mut v: Vec<i128> = values.collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

/// How many files `size = k * x + c` holds for, if that is at least half of them and
/// covers three different values of `x`.
fn count_fit(x: &[i128], s: &[i128], k: i128, c: i128) -> Option<u64> {
    let fits = || x.iter().zip(s).filter(move |&(&x, &s)| s == k * x + c);
    let files = fits().count();
    (files * 2 >= x.len() && files >= 3 && distinct(fits().map(|(&x, _)| x)) >= 3).then_some(files as u64)
}

/// The best whole `k` (and its `c`) for `size = k * x + c`, tried from pairs of the
/// leading files, with the number of files it holds for.
fn fit_single(x: &[i128], s: &[i128]) -> Option<(i128, i128, u64)> {
    let n = x.len().min(LEAD);
    let mut tried = Vec::new();
    let mut best: Option<(i128, i128, u64)> = None;
    for i in 0..n {
        for j in i + 1..n {
            let (dx, ds) = (x[j] - x[i], s[j] - s[i]);
            if dx == 0 || ds % dx != 0 || !(1..=MAX_MULTIPLE).contains(&(ds / dx)) {
                continue;
            }
            let k = ds / dx;
            let c = s[i] - k * x[i];
            if tried.contains(&(k, c)) {
                continue;
            }
            tried.push((k, c));
            if let Some(files) = count_fit(x, s, k, c)
                && best.is_none_or(|b| files > b.2)
            {
                best = Some((k, c, files));
            }
        }
    }
    best
}

/// The best whole positive `a`, `b` (and `c`) for `size = a * x + b * y + c`, solved from
/// triples of the leading files, with the number of files it holds for.
fn fit_pair(x: &[i128], y: &[i128], s: &[i128]) -> Option<(i128, i128, i128, u64)> {
    let n = x.len().min(LEAD);
    let check = x.len().min(64);
    let mut tried = Vec::new();
    let mut best: Option<(i128, i128, i128, u64)> = None;
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                let (x1, y1, s1) = (x[j] - x[i], y[j] - y[i], s[j] - s[i]);
                let (x2, y2, s2) = (x[k] - x[i], y[k] - y[i], s[k] - s[i]);
                let det = x1 * y2 - x2 * y1;
                if det == 0 {
                    continue;
                }
                let (da, db) = (s1 * y2 - s2 * y1, x1 * s2 - x2 * s1);
                if da % det != 0 || db % det != 0 {
                    continue;
                }
                let (a, b) = (da / det, db / det);
                if !(1..=MAX_MULTIPLE).contains(&a) || !(1..=MAX_MULTIPLE).contains(&b) {
                    continue;
                }
                let c = s[i] - a * x[i] - b * y[i];
                if tried.contains(&(a, b, c)) {
                    continue;
                }
                tried.push((a, b, c));
                let holds = |m: usize| (0..m).filter(|&f| s[f] == a * x[f] + b * y[f] + c).count();
                // A quick look at the leading files before counting them all.
                if holds(check) * 2 < check {
                    continue;
                }
                let files = holds(x.len());
                if files * 2 >= x.len() && best.is_none_or(|b| files as u64 > b.3) {
                    best = Some((a, b, c, files as u64));
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(body: Vec<u8>, b: u64) -> Sample {
        Sample { path: String::new(), body, b: Some(b), f: None, keys: Vec::new() }
    }

    fn single(offset: i64, multiple: u64, plus: i64, files: u64) -> SizeField {
        SizeField { part: "B".into(), terms: vec![Term { offset, multiple }], plus, files }
    }

    #[test]
    fn a_count_field_is_found() {
        // u32 at 4 is a vertex count: B = 24 * count + 16.
        let samples: Vec<Sample> = [3u32, 10, 7, 100]
            .iter()
            .map(|&n| {
                let mut body = vec![0xaa, 0xbb, 0xcc, 0xdd];
                body.extend_from_slice(&n.to_le_bytes());
                body.extend_from_slice(&[1, 2, 3, 4]);
                sample(body, 24 * u64::from(n) + 16)
            })
            .collect();
        assert_eq!(size_fields(&samples, 12), [single(4, 24, 16, 4)], "headers of one length: no offsets from the end");
        assert!(size_fields(&samples[..2], 12).is_empty(), "two files prove nothing");
    }

    #[test]
    fn a_field_that_fits_most_files_is_found_with_its_count() {
        // Five files follow B = 8 * count; two others have a different layout.
        let samples: Vec<Sample> = [(1u32, 8u64), (2, 16), (3, 24), (50, 999), (4, 32), (60, 7), (9, 72)]
            .iter()
            .map(|&(n, b)| sample(n.to_le_bytes().to_vec(), b))
            .collect();
        assert_eq!(size_fields(&samples, 4), [single(0, 8, 0, 5)]);
    }

    #[test]
    fn two_count_fields_are_found_together() {
        // A name first, so the counts are at fixed offsets from the end only:
        // B = 24 * vertices + 2 * indices.
        let samples: Vec<Sample> = [(3u32, 6u32, "a"), (10, 30, "abc"), (7, 9, "ab"), (100, 150, "abcdef"), (4, 12, "")]
            .iter()
            .map(|&(v, i, name)| {
                let mut body = name.as_bytes().to_vec();
                body.extend_from_slice(&v.to_le_bytes());
                body.extend_from_slice(&i.to_le_bytes());
                body.extend_from_slice(&[9, 9, 9, 9]);
                sample(body, 24 * u64::from(v) + 2 * u64::from(i))
            })
            .collect();
        let fields = size_fields(&samples, 12);
        let pair = SizeField { part: "B".into(), terms: vec![Term { offset: -12, multiple: 24 }, Term { offset: -8, multiple: 2 }], plus: 0, files: 5 };
        assert!(fields.contains(&pair), "{fields:?}");
    }
}
