//! Synthetic caches for tests: no game data, built the same way every time.
//!
//! This crate writes the formats on its own, without wfscan-core, so the reader is tested
//! against an independent writer.
//!
//! Oodle can't be produced without the real library, so fixtures stand in for it with
//! "flip" blocks: the Oodle magic byte followed by the data with every bit inverted.
//! [`flip_decode`] undoes that; tests pass it to the reader as its Oodle decoder.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

/// First byte of an Oodle block as the game writes them.
pub const OODLE_MAGIC: u8 = 0x8c;

/// A timestamp for live entries (a FILETIME in 2026).
pub const TIMESTAMP: i64 = 0x01dd_4b7c_2136_2f50;

const TOC_MAGIC: [u8; 4] = [0x4e, 0xc6, 0x67, 0x18];
const TOC_VERSION: u32 = 20;
const NAME_LEN: usize = 64;

/// How a part's data is laid out in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pack {
    /// As is: compressed size equals size.
    Stored,
    /// One LZF block with a block header.
    Lzf,
    /// Blocks of this many bytes, each with a header; LZF, or stored if LZF doesn't help.
    Blocks(usize),
    /// One LZF block without a block header.
    BareLzf,
    /// One stand-in Oodle block with a block header.
    Flip,
}

#[derive(Debug, Clone)]
pub struct FxPart {
    pub data: Vec<u8>,
    pub pack: Pack,
}

#[derive(Debug, Clone)]
pub struct FxFile {
    pub path: String,
    pub stale: bool,
    /// H, B and F.
    pub parts: [Option<FxPart>; 3],
    /// The type in the H part's common header, if it has one.
    pub type_id: Option<u32>,
    /// The TOC timestamp of a live entry; stale entries get 0.
    pub timestamp: i64,
}

impl FxFile {
    /// Whether reading this file needs an Oodle decoder.
    pub fn needs_oodle(&self) -> bool {
        self.parts.iter().flatten().any(|p| p.pack == Pack::Flip)
    }
}

#[derive(Debug, Clone)]
pub struct FxPackage {
    pub name: String,
    /// The parts to write, such as `"HBF"`, whether or not any file uses them.
    pub parts: &'static str,
    pub files: Vec<FxFile>,
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: &'static str,
    pub packages: Vec<FxPackage>,
}

pub const PART_LETTERS: [char; 3] = ['H', 'B', 'F'];

/// Compressible bytes: words from a small vocabulary.
pub fn text(len: usize, seed: u32) -> Vec<u8> {
    const WORDS: [&str; 8] = ["warframe ", "tenno ", "lotus ", "orokin ", "grineer ", "corpus ", "void ", "relay "];
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(WORDS[(state >> 29) as usize].as_bytes());
    }
    out.truncate(len);
    out
}

/// Incompressible bytes.
pub fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = u64::from(seed) | 1 << 40;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// A common header: hash, source paths, attribute text, type.
pub fn common_header(sources: &[&str], attributes: &str, type_id: u32) -> Vec<u8> {
    let mut h = noise(16, type_id.wrapping_add(sources.len() as u32));
    h.extend_from_slice(&(sources.len() as u32).to_le_bytes());
    for s in sources {
        h.extend_from_slice(&(s.len() as u32).to_le_bytes());
        h.extend_from_slice(s.as_bytes());
    }
    h.extend_from_slice(&(attributes.len() as u32).to_le_bytes());
    h.extend_from_slice(attributes.as_bytes());
    if !attributes.is_empty() {
        h.push(0);
    }
    h.extend_from_slice(&type_id.to_le_bytes());
    h
}

/// A simple greedy LZF compressor.
pub fn lzf_compress(data: &[u8]) -> Vec<u8> {
    fn literals(out: &mut Vec<u8>, lit: &[u8]) {
        for chunk in lit.chunks(32) {
            out.push(chunk.len() as u8 - 1);
            out.extend_from_slice(chunk);
        }
    }
    const MAX_LEN: usize = 7 + 255 + 2;
    const MAX_DISTANCE: usize = 1 << 13;
    let mut out = Vec::new();
    let mut last = vec![usize::MAX; 1 << 14];
    let (mut i, mut lit_start) = (0, 0);
    while i + 2 < data.len() {
        let key = (usize::from(data[i]) << 6 ^ usize::from(data[i + 1]) << 3 ^ usize::from(data[i + 2])) & 0x3fff;
        let candidate = std::mem::replace(&mut last[key], i);
        if candidate != usize::MAX && i - candidate <= MAX_DISTANCE && data[candidate..candidate + 3] == data[i..i + 3] {
            let max = (data.len() - i).min(MAX_LEN);
            let len = (3..max).find(|&n| data[candidate + n] != data[i + n]).unwrap_or(max);
            literals(&mut out, &data[lit_start..i]);
            let distance = i - candidate - 1;
            let high = (distance >> 8) as u8;
            if len - 2 < 7 {
                out.push(((len - 2) as u8) << 5 | high);
            } else {
                out.push(7 << 5 | high);
                out.push((len - 2 - 7) as u8);
            }
            out.push(distance as u8);
            i += len;
            lit_start = i;
        } else {
            i += 1;
        }
    }
    literals(&mut out, &data[lit_start..]);
    out
}

/// A stand-in Oodle block for `data`.
pub fn flip_encode(data: &[u8]) -> Vec<u8> {
    std::iter::once(OODLE_MAGIC).chain(data.iter().map(|b| !b)).collect()
}

/// Decode a [`flip_encode`] block into exactly `out.len()` bytes.
pub fn flip_decode(comp: &[u8], out: &mut [u8]) -> Result<(), String> {
    if comp.first() != Some(&OODLE_MAGIC) || comp.len() != out.len() + 1 {
        return Err("not a flip block of this size".into());
    }
    out.iter_mut().zip(&comp[1..]).for_each(|(o, c)| *o = !c);
    Ok(())
}

/// The 8-byte header in front of a block.
pub fn block_header(compressed: usize, size: usize) -> [u8; 8] {
    assert!(compressed < 1 << 22 && size < 1 << 24);
    let mut h = [0u8; 8];
    h[..4].copy_from_slice(&(0x8000_0000 | (compressed as u32) << 2).to_be_bytes());
    h[4..].copy_from_slice(&((size as u32) << 5 | 1).to_be_bytes());
    h
}

fn with_header(payload: &[u8], size: usize) -> Vec<u8> {
    let mut out = block_header(payload.len(), size).to_vec();
    out.extend_from_slice(payload);
    out
}

/// A part's bytes as they sit in the cache.
pub fn encode(part: &FxPart) -> Vec<u8> {
    let data = &part.data;
    let encoded = match part.pack {
        Pack::Stored => return data.clone(),
        Pack::Lzf => with_header(&lzf_compress(data), data.len()),
        Pack::BareLzf => lzf_compress(data),
        Pack::Flip => with_header(&flip_encode(data), data.len()),
        Pack::Blocks(size) => data
            .chunks(size)
            .flat_map(|chunk| {
                let lzf = lzf_compress(chunk);
                with_header(if lzf.len() < chunk.len() { &lzf } else { chunk }, chunk.len())
            })
            .collect(),
    };
    // An entry whose two sizes are equal reads as stored.
    assert_ne!(encoded.len(), data.len(), "fixture data must change size when packed");
    encoded
}

fn toc_entry(offset: i64, timestamp: i64, compressed: i32, size: i32, parent: i32, name: &str) -> Vec<u8> {
    assert!(name.len() <= NAME_LEN, "name too long: {name}");
    let mut e = Vec::with_capacity(96);
    e.extend_from_slice(&offset.to_le_bytes());
    e.extend_from_slice(&timestamp.to_le_bytes());
    e.extend_from_slice(&compressed.to_le_bytes());
    e.extend_from_slice(&size.to_le_bytes());
    e.extend_from_slice(&0i32.to_le_bytes());
    e.extend_from_slice(&parent.to_le_bytes());
    let mut padded = [0u8; NAME_LEN];
    padded[..name.len()].copy_from_slice(name.as_bytes());
    e.extend_from_slice(&padded);
    e
}

/// The `.toc` and `.cache` bytes for one part of a package.
pub fn build_part(package: &FxPackage, part: usize) -> (Vec<u8>, Vec<u8>) {
    let mut toc = TOC_MAGIC.to_vec();
    toc.extend_from_slice(&TOC_VERSION.to_le_bytes());
    let mut cache = Vec::new();
    // Directory path -> its number; the root is 0.
    let mut dirs: BTreeMap<String, i32> = BTreeMap::from([(String::new(), 0)]);
    for file in &package.files {
        let Some(data) = &file.parts[part] else { continue };
        let (dir, name) = file.path.rsplit_once('/').expect("paths start with /");
        let mut parent = 0;
        let mut so_far = String::new();
        for component in dir.split('/').filter(|c| !c.is_empty()) {
            so_far = format!("{so_far}/{component}");
            let next = dirs.len() as i32;
            parent = *dirs.entry(so_far.clone()).or_insert_with(|| {
                toc.extend(toc_entry(-1, -1, -1, -1, parent, component));
                next
            });
        }
        let encoded = encode(data);
        // A gap before each entry, so nothing can rely on entries being back to back.
        cache.extend_from_slice(b"\xee\xee\xee");
        let timestamp = if file.stale { 0 } else { file.timestamp };
        toc.extend(toc_entry(cache.len() as i64, timestamp, encoded.len() as i32, data.data.len() as i32, parent, name));
        cache.extend_from_slice(&encoded);
    }
    (toc, cache)
}

impl Fixture {
    /// Write the cache folder: `<part>.<package>.toc` and `.cache` files in `dir`.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for package in &self.packages {
            for (i, letter) in PART_LETTERS.into_iter().enumerate() {
                if !package.parts.contains(letter) {
                    assert!(package.files.iter().all(|f| f.parts[i].is_none()), "{} has data for a part it doesn't list", package.name);
                    continue;
                }
                let (toc, cache) = build_part(package, i);
                std::fs::write(dir.join(format!("{letter}.{}.toc", package.name)), toc)?;
                std::fs::write(dir.join(format!("{letter}.{}.cache", package.name)), cache)?;
            }
        }
        Ok(())
    }

    /// The same cache with LZF in place of the stand-in Oodle blocks, for readers that
    /// have no Oodle decoder.
    pub fn without_oodle(mut self) -> Self {
        for part in self.packages.iter_mut().flat_map(|p| &mut p.files).flat_map(|f| &mut f.parts).flatten() {
            if part.pack == Pack::Flip {
                part.pack = Pack::Lzf;
            }
        }
        self
    }

    /// Files in the order a manifest lists them: by package, then path, stale last.
    pub fn sorted_files(&self) -> Vec<(&FxPackage, &FxFile)> {
        let mut packages: Vec<&FxPackage> = self.packages.iter().collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        packages
            .into_iter()
            .flat_map(|p| {
                let mut files: Vec<&FxFile> = p.files.iter().collect();
                files.sort_by(|a, b| a.path.cmp(&b.path).then(a.stale.cmp(&b.stale)));
                files.into_iter().map(move |f| (p, f))
            })
            .collect()
    }

    /// What a reader should find, for checking by hand or from another tool.
    pub fn expected(&self) -> Value {
        let files: Vec<Value> = self
            .sorted_files()
            .into_iter()
            .map(|(package, file)| {
                let mut parts = serde_json::Map::new();
                for (letter, part) in PART_LETTERS.into_iter().zip(&file.parts) {
                    if let Some(part) = part {
                        parts.insert(
                            letter.to_string(),
                            json!({ "size": part.data.len(), "compressed_size": encode(part).len(), "crc32": crc32fast::hash(&part.data) }),
                        );
                    }
                }
                json!({ "package": package.name, "path": file.path, "stale": file.stale, "type": file.type_id, "parts": parts })
            })
            .collect();
        json!({ "name": self.name, "files": files })
    }
}

fn part(data: Vec<u8>, pack: Pack) -> Option<FxPart> {
    Some(FxPart { data, pack })
}

/// A header followed by `body`.
fn headed(path: &str, attributes: &str, type_id: u32, body: Vec<u8>) -> Vec<u8> {
    let mut h = common_header(&[path], attributes, type_id);
    h.extend(body);
    h
}

/// A small cache with one of everything the reader has to handle: all three parts, every
/// block kind, a file without a header, a stale copy, a body-only file, a 64-byte name,
/// a package without a B part and an empty package.
pub fn basic() -> Fixture {
    let skel = "/Lotus/Characters/Tenno/Excalibur/Excalibur_skel.fbx";
    let lua = "/Lotus/Scripts/Test.lua";
    let level = "/Lotus/Levels/Big.level";
    let long_name = format!("/Lotus/{}", "n".repeat(NAME_LEN));
    let icon = "/Lotus/Interface/Icons/Thing.png";

    let mut level_body = text(1500, 7);
    level_body.extend(noise(700, 8));
    level_body.extend(text(900, 9));

    let misc = FxPackage {
        name: "Misc".into(),
        parts: "HBF",
        files: vec![
            FxFile {
                path: skel.into(),
                stale: false,
                parts: [part(headed(skel, "", 269, text(300, 1)), Pack::Lzf), part(text(5000, 2), Pack::Blocks(1024)), None],
                type_id: Some(269),
                timestamp: TIMESTAMP,
            },
            FxFile {
                path: lua.into(),
                stale: false,
                parts: [part(headed(lua, "Version=3\n", 22, vec![1, 2, 3, 4]), Pack::Stored), part(text(900, 3), Pack::Flip), None],
                type_id: Some(22),
                timestamp: TIMESTAMP,
            },
            FxFile {
                path: lua.into(),
                stale: true,
                parts: [part(headed(lua, "Version=2\n", 22, vec![9, 9]), Pack::Stored), None, None],
                type_id: Some(22),
                timestamp: TIMESTAMP,
            },
            FxFile {
                path: level.into(),
                stale: false,
                parts: [
                    part(headed(level, "", 5, text(200, 4)), Pack::Lzf),
                    part(level_body, Pack::Blocks(512)),
                    part(noise(2000, 5), Pack::Stored),
                ],
                type_id: Some(5),
                timestamp: TIMESTAMP,
            },
            FxFile { path: "/EE/NoHeader.bin".into(), stale: false, parts: [part(noise(40, 6), Pack::Stored), None, None], type_id: None, timestamp: TIMESTAMP },
            FxFile { path: "/Lotus/BodyOnly.bin".into(), stale: false, parts: [None, part(text(600, 10), Pack::BareLzf), None], type_id: None, timestamp: TIMESTAMP },
            FxFile {
                path: long_name.clone(),
                stale: false,
                parts: [part(headed(&long_name, "", 1, vec![]), Pack::Stored), None, None],
                type_id: Some(1),
                timestamp: TIMESTAMP,
            },
        ],
    };
    // An icon: a 16x16 texture with all five mips in F.
    let icon_texture = texture(10, (16, 16), 16, 0, 5, 1);
    let texture = FxPackage {
        name: "Texture".into(),
        parts: "HF",
        files: vec![FxFile {
            path: icon.into(),
            stale: false,
            parts: [part(headed(icon, "", 163, icon_texture.header), Pack::Stored), None, part(icon_texture.f, Pack::Flip)],
            type_id: Some(163),
            timestamp: TIMESTAMP,
        }],
    };
    let emblem = FxPackage { name: "Emblem".into(), parts: "HB", files: vec![] };
    Fixture { name: "basic", packages: vec![misc, texture, emblem] }
}

/// A file of the given type with a small H part and a B part, both LZF.
pub fn simple_file(path: &str, type_id: u32, seed: u32) -> FxFile {
    FxFile {
        path: path.into(),
        stale: false,
        parts: [part(headed(path, "", type_id, text(200, seed)), Pack::Lzf), part(text(800, seed + 1), Pack::Lzf), None],
        type_id: Some(type_id),
        timestamp: TIMESTAMP,
    }
}

/// [`basic`] after a game update: `Test.lua` has new contents and a new timestamp,
/// `NoHeader.bin` is gone, `New_skel.fbx` is new. Everything else is the same data,
/// though at other offsets.
pub fn basic_updated() -> Fixture {
    let mut fixture = basic();
    fixture.name = "basic-updated";
    let misc = &mut fixture.packages[0];
    misc.files.retain(|f| f.path != "/EE/NoHeader.bin");
    let lua = misc.files.iter_mut().find(|f| f.path == "/Lotus/Scripts/Test.lua" && !f.stale).expect("basic has Test.lua");
    lua.parts[0] = part(headed(&lua.path, "Version=4\n", 22, vec![1, 2, 3, 4, 5]), Pack::Stored);
    lua.timestamp = TIMESTAMP + 10_000_000;
    misc.files.insert(0, simple_file("/Lotus/Characters/New_skel.fbx", 269, 20));
    fixture
}

/// A texture as the game stores it: the header that follows the common header, and the
/// mips split between B (smallest) and F (largest).
#[derive(Debug, Clone)]
pub struct FxTexture {
    pub header: Vec<u8>,
    pub b: Vec<u8>,
    pub f: Vec<u8>,
}

/// The colour every pixel of one image of a fixture texture has, as R, G, B, A: red
/// tells the mip, green the element (face, layer or slice).
pub fn texture_colour(element: u32, mip: u32) -> [u8; 4] {
    [(40 + mip * 24) as u8, (16 + element * 32) as u8, 200, 255]
}

/// One image filled with `rgba`, in pixel format `format` (10 BGRA8, 12 A8 holding the
/// red value, 30 R16 holding red in its high byte, 1 BC1).
fn solid_image(format: u8, width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let [r, g, b, a] = rgba;
    let pixels = (width * height) as usize;
    match format {
        10 => [b, g, r, a].repeat(pixels),
        12 => vec![r; pixels],
        30 => [0, r].repeat(pixels),
        1 => {
            // Both endpoints the same 5:6:5 colour, every pixel index 0.
            let c = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
            let [lo, hi] = c.to_le_bytes();
            let blocks = (width.div_ceil(4) * height.div_ceil(4)) as usize;
            [lo, hi, lo, hi, 0, 0, 0, 0].repeat(blocks)
        }
        other => panic!("fixture textures can't be made in format {other}"),
    }
}

/// Build a texture of solid-colour images ([`texture_colour`]). `authored` is the size in
/// the header, `stored` the larger dimension of the top mip actually stored; the
/// smallest `b_mips` mips go to B and the `f_mips` above them to F, each mip holding all
/// `elements`.
pub fn texture(format: u8, authored: (u16, u16), stored: u16, b_mips: u8, f_mips: u8, elements: u32) -> FxTexture {
    let larger = u32::from(authored.0.max(authored.1));
    let top = (u32::from(authored.0) * u32::from(stored) / larger, u32::from(authored.1) * u32::from(stored) / larger);
    let size = |mip: u32| ((top.0 >> mip).max(1), (top.1 >> mip).max(1));
    let mips = u32::from(b_mips) + u32::from(f_mips);
    // Smallest mip first, and within a mip one element after another.
    let part = |range: std::ops::Range<u32>| -> Vec<u8> {
        range
            .rev()
            .flat_map(|mip| (0..elements).flat_map(move |element| solid_image(format, size(mip).0, size(mip).1, texture_colour(element, mip))))
            .collect()
    };
    let b_size = if b_mips > 0 { size(u32::from(f_mips)) } else { (0, 0) };

    let mut header = vec![b_mips, f_mips, 1, format];
    let offsets = u32::from(f_mips.saturating_sub(1));
    header.extend_from_slice(&offsets.to_le_bytes());
    header.extend((0..offsets).flat_map(|i| (1000 * (i + 1)).to_le_bytes()));
    for value in [authored.0, authored.1, b_size.0 as u16, b_size.1 as u16, stored, 0] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    // The fields after the sizes, which nothing reads: flags, counts, tint floats.
    header.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 5, 0, 0, 0]);
    header.extend(std::iter::repeat_n(1.0f32.to_le_bytes(), 4).flatten());
    FxTexture { header, b: part(u32::from(f_mips)..mips), f: part(0..u32::from(f_mips)) }
}

/// A texture file: header in H (stored), mips in B and F (LZF) where there are any.
pub fn texture_file(path: &str, type_id: u32, texture: FxTexture) -> FxFile {
    let data = |bytes: Vec<u8>| (!bytes.is_empty()).then_some(FxPart { pack: if bytes.len() < 32 { Pack::Stored } else { Pack::Lzf }, data: bytes });
    FxFile {
        path: path.into(),
        stale: false,
        parts: [part(headed(path, "", type_id, texture.header), Pack::Stored), data(texture.b), data(texture.f)],
        type_id: Some(type_id),
        timestamp: TIMESTAMP,
    }
}

/// Paths of the [`textures`] fixture.
pub mod tex {
    pub const SPLIT: &str = "/Lotus/Textures/Split_d.png";
    pub const ICON: &str = "/Lotus/Interface/Icons/Icon.png";
    pub const TALL: &str = "/Lotus/Textures/TallPackMap";
    pub const BC1: &str = "/Lotus/Textures/Compressed_n.tga";
    pub const MASK: &str = "/Lotus/Textures/Mask_mm.png";
    pub const HEIGHT: &str = "/Lotus/Textures/Height.lchm";
    pub const ARRAY: &str = "/Lotus/Textures/DetailArray";
    pub const VOLUME: &str = "/Lotus/Textures/Noise_v.png";
    pub const CUBE: &str = "/Lotus/Textures/Sky_cmcross.hdr";
    pub const NEW_FORMAT: &str = "/Lotus/Textures/Lut.cube";
    pub const SHORT: &str = "/Lotus/Textures/Short_d.png";
    pub const LIGHT_CUBE: &str = "/Lotus/Levels/Room/0_cross.hdr";
    pub const LIGHT_VOLUME: &str = "/Lotus/Levels/Room/0_fvl.raw";
}

/// One texture of every shape and layout: split between B and F, all in F, stored
/// smaller than authored and not square, block-compressed, one- and two-byte formats, an
/// array, a volume, cube maps, a pixel format wfscan doesn't know and one with data
/// missing.
pub fn textures() -> Fixture {
    let mut short = texture(10, (8, 8), 8, 3, 1, 1);
    short.b.truncate(short.b.len() - 4);
    // Good data under a format code wfscan has no name for.
    let mut lut = texture(10, (4, 4), 4, 1, 0, 4);
    lut.header[3] = 31;
    let files = vec![
        texture_file(tex::SPLIT, 163, texture(10, (16, 16), 16, 3, 2, 1)),
        texture_file(tex::ICON, 163, texture(10, (8, 8), 8, 0, 4, 1)),
        texture_file(tex::TALL, 188, texture(10, (16, 32), 16, 2, 2, 1)),
        texture_file(tex::BC1, 184, texture(1, (16, 16), 16, 4, 1, 1)),
        texture_file(tex::MASK, 163, texture(12, (8, 8), 8, 4, 0, 1)),
        texture_file(tex::HEIGHT, 163, texture(30, (4, 4), 4, 1, 0, 1)),
        texture_file(tex::ARRAY, 166, texture(10, (8, 8), 8, 2, 2, 3)),
        texture_file(tex::VOLUME, 174, texture(10, (4, 4), 4, 1, 0, 4)),
        texture_file(tex::CUBE, 177, texture(10, (4, 4), 4, 3, 0, 6)),
        texture_file(tex::NEW_FORMAT, 174, lut),
        texture_file(tex::SHORT, 163, short),
    ];
    let lightmap = vec![
        texture_file(tex::LIGHT_CUBE, 183, texture(10, (8, 8), 8, 4, 0, 6)),
        texture_file(tex::LIGHT_VOLUME, 176, texture(10, (4, 4), 4, 1, 0, 6)),
    ];
    Fixture {
        name: "textures",
        packages: vec![
            FxPackage { name: "Texture".into(), parts: "HBF", files },
            FxPackage { name: "LightMap".into(), parts: "HB", files: lightmap },
        ],
    }
}

/// Every fixture, for `gen-fixtures`.
pub fn all() -> Vec<Fixture> {
    vec![basic(), basic_updated(), textures()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_round_trips() {
        let data = text(100, 1);
        let mut out = vec![0; 100];
        flip_decode(&flip_encode(&data), &mut out).unwrap();
        assert_eq!(out, data);
        assert!(flip_decode(&flip_encode(&data), &mut [0; 99]).is_err());
    }

    #[test]
    fn lzf_shrinks_text_and_not_noise() {
        assert!(lzf_compress(&text(2000, 1)).len() < 1000);
        assert!(lzf_compress(&noise(2000, 1)).len() >= 2000);
    }

    #[test]
    fn building_is_deterministic() {
        let (a, b) = (basic(), basic());
        for (pa, pb) in a.packages.iter().zip(&b.packages) {
            for part in 0..3 {
                assert_eq!(build_part(pa, part), build_part(pb, part));
            }
        }
        assert_eq!(a.expected(), b.expected());
    }
}
