//! Texture export, checked by reading the results back with texscan: on the synthetic
//! `textures` fixture, where every pixel is known, and on a real cache, where each mip
//! must look like the one above it.
#![cfg(feature = "png")]

use std::path::{Path, PathBuf};

use texscan_core::formats::dds;
use texscan_core::{Image, Subresource, TextureInfo, decode, decode_png};
use wfscan_core::{
    Cache, ExportOptions, ExportRecord, ExportStats, Exporters, FormatOptions, IndexOptions, Kind, Manifest, OodleDecoder, Status,
    TextureOutput, export, index,
};
use wfscan_fixtures::{self as fx, tex};

struct Setup {
    tmp: tempfile::TempDir,
    manifest: Manifest,
    stats: ExportStats,
}

impl Setup {
    fn out(&self, internal: &str) -> PathBuf {
        internal.split('/').filter(|p| !p.is_empty()).fold(self.tmp.path().join("out"), |dir, part| dir.join(part))
    }

    fn record(&self, path: &str) -> &ExportRecord {
        self.manifest.files.iter().find(|f| f.path == path).unwrap().export.as_ref().unwrap()
    }
}

fn exported(output: TextureOutput) -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::textures().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, index_stats) = index(&cache, None, &IndexOptions::default()).unwrap();
    assert_eq!(index_stats.failed, 0);
    let exporters = Exporters::builtin(&FormatOptions { texture: output, ..Default::default() });
    let stats = export(&cache, &mut manifest, None, &exporters, None, &tmp.path().join("out"), &ExportOptions::default()).unwrap();
    Setup { tmp, manifest, stats }
}

/// Every pixel of `image` is `expected`, give or take `tolerance` per channel.
fn assert_solid(image: &Image, expected: [u8; 4], tolerance: u8, what: &str) {
    for pixel in image.rgba.as_chunks::<4>().0 {
        let close = pixel.iter().zip(expected).all(|(&got, want)| got.abs_diff(want) <= tolerance);
        assert!(close, "{what}: pixel {pixel:?}, expected {expected:?}");
    }
}

/// What a fixture image decodes to: the colour it was filled with, as far as the pixel
/// format keeps it.
fn decoded_colour(format: &str, element: u32, mip: u32) -> ([u8; 4], u8) {
    let [r, g, b, a] = fx::texture_colour(element, mip);
    match format {
        "B8G8R8A8_UNORM" => ([r, g, b, a], 0),
        "BC1_UNORM" => ([r, g, b, 255], 8), // 5:6:5
        // texscan shows single-channel formats as it finds them: alpha only, or grey.
        "A8_UNORM" => ([0, 0, 0, r], 0),
        "R16_UNORM" => ([r, r, r, 255], 0),
        other => panic!("no expectation for {other}"),
    }
}

#[test]
fn dds_files_hold_every_image_in_the_right_place() {
    let s = exported(TextureOutput::Dds);
    assert_eq!((s.stats.selected, s.stats.ok, s.stats.unsupported, s.stats.failed), (13, 11, 1, 1));

    /// path, output, pixel format, width, height, mips, faces, array size, depth
    type Expected = (&'static str, &'static str, &'static str, u32, u32, u32, u32, u32, u32);
    let expected: [Expected; 11] = [
        (tex::SPLIT, "/Lotus/Textures/Split_d.dds", "B8G8R8A8_UNORM", 16, 16, 5, 1, 1, 1),
        (tex::ICON, "/Lotus/Interface/Icons/Icon.dds", "B8G8R8A8_UNORM", 8, 8, 4, 1, 1, 1),
        (tex::TALL, "/Lotus/Textures/TallPackMap.dds", "B8G8R8A8_UNORM", 8, 16, 4, 1, 1, 1),
        (tex::BC1, "/Lotus/Textures/Compressed_n.dds", "BC1_UNORM", 16, 16, 5, 1, 1, 1),
        (tex::MASK, "/Lotus/Textures/Mask_mm.dds", "A8_UNORM", 8, 8, 4, 1, 1, 1),
        (tex::HEIGHT, "/Lotus/Textures/Height.dds", "R16_UNORM", 4, 4, 1, 1, 1, 1),
        (tex::ARRAY, "/Lotus/Textures/DetailArray.dds", "B8G8R8A8_UNORM", 8, 8, 4, 1, 3, 1),
        (tex::VOLUME, "/Lotus/Textures/Noise_v.dds", "B8G8R8A8_UNORM", 4, 4, 1, 1, 1, 4),
        (tex::CUBE, "/Lotus/Textures/Sky_cmcross.dds", "B8G8R8A8_UNORM", 4, 4, 3, 6, 1, 1),
        (tex::LIGHT_CUBE, "/Lotus/Levels/Room/0_cross.dds", "B8G8R8A8_UNORM", 8, 8, 4, 6, 1, 1),
        (tex::LIGHT_VOLUME, "/Lotus/Levels/Room/0_fvl.dds", "B8G8R8A8_UNORM", 4, 4, 1, 1, 1, 6),
    ];
    for (path, output, format, width, height, mips, faces, array, depth) in expected {
        let record = s.record(path);
        assert_eq!((record.status, record.exporter.as_deref(), &record.outputs[..]), (Status::Ok, Some("texture-dds@1"), &[output.to_string()][..]), "{path}");
        let file = std::fs::read(s.out(output)).unwrap();
        let info: TextureInfo = dds::parse(&file).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        assert_eq!(info.size, file.len() as u64, "{path}: no bytes left over");
        assert_eq!((info.pixel_format.name, info.width, info.height, info.mips), (format, width, height, mips), "{path}");
        assert_eq!((info.faces, info.array_size, info.depth), (faces, array, depth), "{path}");
        for layer in 0..faces * array {
            for mip in 0..mips {
                for slice in 0..depth {
                    let image = decode(&file, &info, Subresource { layer, mip, slice }).unwrap();
                    assert_eq!((image.width, image.height), ((width >> mip).max(1), (height >> mip).max(1)), "{path}");
                    // Layers and slices are both "elements" of the fixture.
                    let (colour, tolerance) = decoded_colour(format, layer.max(slice), mip);
                    assert_solid(&image, colour, tolerance, &format!("{path} layer {layer} mip {mip} slice {slice}"));
                }
            }
        }
    }

    let unknown = s.record(tex::NEW_FORMAT);
    assert_eq!((unknown.status, unknown.reason.as_deref()), (Status::Unsupported, Some("pixel format 31 is not known")));
    let short = s.record(tex::SHORT);
    assert_eq!(short.status, Status::Failed);
    let reason = short.reason.as_deref().unwrap();
    assert!(reason.contains("needs 84 bytes in B and 256 in F") && reason.contains("B has 80"), "{reason}");
}

#[test]
fn pngs_are_the_top_mip_of_each_element() {
    let s = exported(TextureOutput::Png);
    assert_eq!((s.stats.ok, s.stats.unsupported, s.stats.failed), (11, 1, 1));
    assert_eq!(s.record(tex::SPLIT).exporter.as_deref(), Some("texture-png@1"));

    // path, output stem, element suffixes, width, height
    let none: &[&str] = &[""];
    let faces: &[&str] = &["_px", "_nx", "_py", "_ny", "_pz", "_nz"];
    let expected: [(&str, &str, &[&str], u32, u32); 7] = [
        (tex::SPLIT, "/Lotus/Textures/Split_d", none, 16, 16),
        (tex::TALL, "/Lotus/Textures/TallPackMap", none, 8, 16),
        (tex::BC1, "/Lotus/Textures/Compressed_n", none, 16, 16),
        (tex::ARRAY, "/Lotus/Textures/DetailArray", &["_a0", "_a1", "_a2"], 8, 8),
        (tex::VOLUME, "/Lotus/Textures/Noise_v", &["_z0", "_z1", "_z2", "_z3"], 4, 4),
        (tex::CUBE, "/Lotus/Textures/Sky_cmcross", faces, 4, 4),
        (tex::LIGHT_CUBE, "/Lotus/Levels/Room/0_cross", faces, 8, 8),
    ];
    for (path, stem, suffixes, width, height) in expected {
        let outputs: Vec<String> = suffixes.iter().map(|suffix| format!("{stem}{suffix}.png")).collect();
        assert_eq!(s.record(path).outputs, outputs, "{path}");
        for (element, output) in outputs.iter().enumerate() {
            let image = decode_png(&std::fs::read(s.out(output)).unwrap()).unwrap();
            assert_eq!((image.width, image.height), (width, height), "{output}");
            let tolerance = if path == tex::BC1 { 8 } else { 0 };
            assert_solid(&image, fx::texture_colour(element as u32, 0), tolerance, output);
        }
    }

    // A one-channel mask comes out as a grey picture, not as transparent black.
    let mask = decode_png(&std::fs::read(s.out("/Lotus/Textures/Mask_mm.png")).unwrap()).unwrap();
    let grey = fx::texture_colour(0, 0)[0];
    assert_solid(&mask, [grey, grey, grey, 255], 0, "mask");
}

#[test]
fn switching_formats_exports_again() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::textures().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, _) = index(&cache, None, &IndexOptions::default()).unwrap();
    let out = tmp.path().join("out");
    let mut run = |output: TextureOutput| {
        let exporters = Exporters::builtin(&FormatOptions { texture: output, ..Default::default() });
        let stats = export(&cache, &mut manifest, None, &exporters, None, &out, &ExportOptions::default()).unwrap();
        (stats.ok, stats.skipped)
    };
    assert_eq!(run(TextureOutput::Dds), (11, 0));
    assert_eq!(run(TextureOutput::Dds), (0, 11));
    assert_eq!(run(TextureOutput::Png), (11, 0));
    assert_eq!(run(TextureOutput::Png), (0, 11));
}

/// How far a mip is from the 2x2 box average of the mip above it: the mean absolute
/// difference per channel, 0-255. Colour is weighted by alpha, since what lies under
/// transparent pixels is arbitrary. With `hdr`, samples at full brightness are left
/// out: decoding clamped them, and the average of clamped values isn't the clamped
/// average. `None` if nothing was left to compare.
fn mip_distance(larger: &Image, smaller: &Image, hdr: bool) -> Option<f64> {
    let (w, h) = (smaller.width as usize, smaller.height as usize);
    // Premultiplied colour, then alpha itself.
    let at = |image: &Image, x: usize, y: usize, c: usize| {
        let (x, y) = (x.min(image.width as usize - 1), y.min(image.height as usize - 1));
        let pixel = &image.rgba[(y * image.width as usize + x) * 4..][..4];
        if c == 3 { f64::from(pixel[3]) } else { f64::from(pixel[c]) * f64::from(pixel[3]) / 255.0 }
    };
    let (mut total, mut count) = (0.0, 0u32);
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let above = [at(larger, 2 * x, 2 * y, c), at(larger, 2 * x + 1, 2 * y, c), at(larger, 2 * x, 2 * y + 1, c), at(larger, 2 * x + 1, 2 * y + 1, c)];
                let here = at(smaller, x, y, c);
                if hdr && c < 3 && (here >= 255.0 || above.contains(&255.0)) {
                    continue;
                }
                total += (above.iter().sum::<f64>() / 4.0 - here).abs();
                count += 1;
            }
        }
    }
    (count > 0).then(|| total / f64::from(count))
}

fn dds_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            dds_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "dds") {
            into.push(path);
        }
    }
}

/// Real textures: export a few folders as DDS and check that every mip of every element
/// is a smaller copy of the mip above it. A wrong mip order, or elements read in the
/// wrong order, would pair up unrelated images. Needs `WFSCAN_CACHE_DIR` and
/// `WFSCAN_OODLE_DLL`; skipped otherwise.
#[test]
fn real_textures_have_consistent_mips() {
    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let Ok(oodle) = wfscan_core::Oodle::load(None) else {
        eprintln!("skipped: no Oodle library");
        return;
    };
    let oodle: &dyn OodleDecoder = &oodle;
    let cache = Cache::open(Path::new(&dir)).unwrap();
    let (mut manifest, _) = index(&cache, Some(oodle), &IndexOptions::default()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let exporters = Exporters::builtin(&FormatOptions::default());
    // Characters (split B/F, every BC format), icons (all in F), engine materials (cube
    // maps, volumes), a texture array, and one level's lightmaps.
    let folders = [
        "/Lotus/Characters/Tenno/Excalibur/",
        "/Lotus/Interface/Icons/StoreIcons/Warframes/",
        "/EE/Materials/",
        "/Lotus/Characters/Tenno/Operator/Heads/SharedTextures/",
        "/Lotus/Levels/CorpusShipRemastered/ConLjunction02/",
    ];
    for folder in folders {
        let opts = ExportOptions { path: Some(folder.into()), kinds: vec![Kind::Texture], ..Default::default() };
        let stats = export(&cache, &mut manifest, None, &exporters, Some(oodle), tmp.path(), &opts).unwrap();
        eprintln!("{folder}: {} ok, {} unsupported, {} failed", stats.ok, stats.unsupported, stats.failed);
        assert!(stats.ok > 0 && stats.failed == 0, "{folder}: {stats:?}");
    }

    let mut files = Vec::new();
    dds_files(tmp.path(), &mut files);
    // Mips are not always plain box averages (normals are renormalised, noise is
    // regenerated, some have borders drawn in), so a pair counts as wrong only when far
    // apart, and a few are allowed. On the September 2026 cache the median is 3.8 and
    // 15 of 4,452 pairs are over the line, all small mips of hard-edged graphics.
    const FAR: f64 = 30.0;
    let (mut pairs, mut far, mut shapes, mut distances) = (0u32, Vec::new(), [0u32; 4], Vec::new());
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let info = dds::parse(&bytes).unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
        assert_eq!(info.size, bytes.len() as u64, "{}", file.display());
        shapes[if info.faces == 6 { 1 } else if info.array_size > 1 { 2 } else if info.depth > 1 { 3 } else { 0 }] += 1;
        for layer in 0..info.faces * info.array_size {
            let mut above = decode(&bytes, &info, Subresource { layer, mip: 0, slice: 0 }).unwrap();
            for mip in 1..info.mips {
                let this = decode(&bytes, &info, Subresource { layer, mip, slice: 0 }).unwrap();
                // Below 8 pixels a block-compressed mip is too coarse to compare.
                let hdr = info.pixel_format.name.starts_with("BC6H");
                if this.width >= 8
                    && this.height >= 8
                    && let Some(distance) = mip_distance(&above, &this, hdr)
                {
                    pairs += 1;
                    distances.push(distance);
                    if distance > FAR {
                        far.push(format!("{} {} layer {layer} mip {mip}: {distance:.1}", file.display(), info.pixel_format.name));
                    }
                }
                above = this;
            }
        }
    }
    distances.sort_by(f64::total_cmp);
    let at = |fraction: f64| distances[((distances.len() - 1) as f64 * fraction) as usize];
    eprintln!("{} files (flat {}, cube {}, array {}, volume {}), {pairs} mip pairs, {} far apart", files.len(), shapes[0], shapes[1], shapes[2], shapes[3], far.len());
    eprintln!("distance: median {:.1}, 90% under {:.1}, 99% under {:.1}, largest {:.1}", at(0.5), at(0.9), at(0.99), at(1.0));
    for line in far.iter().take(20) {
        eprintln!("  {line}");
    }
    assert!(shapes.iter().all(|&n| n > 0), "the sample should cover every shape: {shapes:?}");
    assert!(pairs > 1000 && at(0.5) < 5.0 && (far.len() as f64) < f64::from(pairs) * 0.02, "{} of {pairs} mip pairs disagree", far.len());
}
