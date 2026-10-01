//! Reading and indexing the synthetic caches from wfscan-fixtures, and, when
//! `WFSCAN_CACHE_DIR` names a real `Cache.Windows`, the real thing.

use std::path::Path;

use wfscan_core::{
    BlockCounts, Cache, Category, Error, Game, IndexOptions, Manifest, OodleDecoder, Part, PartFile, TocEntry, index,
};
use wfscan_fixtures::{self as fx, Fixture, FxFile, FxPackage, FxPart, Pack};

/// The fixtures' stand-in for Oodle.
struct Flip;

impl OodleDecoder for Flip {
    fn decode(&self, comp: &[u8], out: &mut [u8]) -> Result<(), String> {
        fx::flip_decode(comp, out)
    }
}

fn open(fixture: &Fixture) -> (tempfile::TempDir, Cache) {
    let tmp = tempfile::tempdir().unwrap();
    fixture.write(tmp.path()).unwrap();
    let cache = Cache::open(tmp.path()).unwrap();
    (tmp, cache)
}

/// The TOC entry a fixture file's part should have produced.
fn toc_entry<'a>(file: &'a PartFile, fx_file: &FxFile) -> &'a TocEntry {
    if fx_file.stale {
        file.toc.files.iter().find(|e| e.stale && e.path == fx_file.path).expect("stale entry listed")
    } else {
        file.find(&fx_file.path).expect("live entry found")
    }
}

#[test]
fn opens_packages_and_detects_the_game() {
    let (_tmp, cache) = open(&fx::basic());
    assert_eq!(cache.game, Game::Warframe);
    let seen: Vec<(&str, Category, String)> =
        cache.packages.iter().map(|p| (p.name.as_str(), p.category, p.parts().map(|(part, _)| part.letter()).collect())).collect();
    assert_eq!(
        seen,
        [
            ("Emblem", Category::Emblem, "HB".to_string()),
            ("Misc", Category::Misc, "HBF".to_string()),
            ("Texture", Category::Texture, "HF".to_string())
        ]
    );
    let misc = wfscan_core::package_info(cache.package("Misc").unwrap());
    assert_eq!((misc.files, misc.stale), (6, 1));
    assert!(cache.package("Nope").is_none());
}

#[test]
fn every_part_reads_back() {
    let fixture = fx::basic();
    let (_tmp, cache) = open(&fixture);
    let mut total = BlockCounts::default();
    for (package, fx_file) in fixture.sorted_files() {
        for (part, fx_part) in Part::ALL.into_iter().zip(&fx_file.parts) {
            let Some(fx_part) = fx_part else { continue };
            let file = cache.package(&package.name).unwrap().part(part).unwrap();
            let entry = toc_entry(file, fx_file);
            assert_eq!(entry.compressed_size as usize, fx::encode(fx_part).len(), "{} {part}", fx_file.path);
            let (data, counts) = file.read(entry, Some(&Flip)).unwrap();
            assert_eq!(data, fx_part.data, "{} {part}", fx_file.path);
            total += counts;
        }
    }
    assert!(total.oodle == 2 && total.lzf > 5 && total.stored > 5, "every block kind is covered: {total:?}");
}

#[test]
fn index_matches_the_fixture() {
    let fixture = fx::basic();
    let (_tmp, cache) = open(&fixture);
    let opts = IndexOptions { hash: true, full_headers: true, ..Default::default() };
    let (manifest, stats) = index(&cache, Some(&Flip), &opts).unwrap();

    let expected = fixture.sorted_files();
    assert_eq!(manifest.files.len(), expected.len());
    for (got, (package, want)) in manifest.files.iter().zip(&expected) {
        assert_eq!((&got.package, &got.path, got.stale), (&package.name, &want.path, want.stale));
        assert_eq!(got.error, None, "{}", got.path);
        assert_eq!(got.header.as_ref().map(|h| h.type_id), want.type_id, "{}", got.path);
        for (part, fx_part) in Part::ALL.into_iter().zip(&want.parts) {
            let sizes = got.part(part).map(|p| (p.size as usize, p.compressed_size as usize, p.crc32));
            let want_sizes = fx_part.as_ref().map(|p| (p.data.len(), fx::encode(p).len(), Some(crc32fast::hash(&p.data))));
            assert_eq!(sizes, want_sizes, "{} {part}", got.path);
        }
    }
    let lua = manifest.files.iter().find(|f| f.path == "/Lotus/Scripts/Test.lua" && !f.stale).unwrap();
    let header = lua.header.as_ref().unwrap();
    assert_eq!((header.sources.as_slice(), header.attributes.as_deref()), (&["/Lotus/Scripts/Test.lua".to_string()][..], Some("Version=3\n")));
    assert_eq!(header.hash.len(), 32);

    assert_eq!((stats.files, stats.stale, stats.failed, stats.need_oodle), (7, 1, 0, 0));
    assert_eq!(stats.headers, expected.iter().filter(|(_, f)| f.type_id.is_some()).count() as u64);
    assert_eq!(stats.parts_read, expected.iter().map(|(_, f)| f.parts.iter().flatten().count() as u64).sum::<u64>());
    assert_eq!(manifest.source.game, Game::Warframe);
    assert_eq!(manifest.packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Emblem", "Misc", "Texture"]);

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("manifest.json");
    manifest.save(&path).unwrap();
    assert_eq!(Manifest::load(&path).unwrap(), manifest);
}

#[test]
fn index_defaults_read_only_headers() {
    let (_tmp, cache) = open(&fx::basic());
    let (manifest, stats) = index(&cache, Some(&Flip), &IndexOptions::default()).unwrap();
    assert!(manifest.files.iter().all(|f| Part::ALL.iter().all(|&p| f.part(p).is_none_or(|e| e.crc32.is_none()))));
    assert!(manifest.files.iter().flat_map(|f| &f.header).all(|h| h.sources.is_empty() && h.attributes.is_none()));
    assert_eq!(stats.parts_read, manifest.files.iter().filter(|f| f.h.is_some()).count() as u64);
    assert_eq!(stats.failed, 0, "the Oodle parts are B and F, which aren't read");

    let opts = IndexOptions { skip_stale: true, ..Default::default() };
    let (manifest, stats) = index(&cache, Some(&Flip), &opts).unwrap();
    assert_eq!((stats.stale, manifest.files.iter().filter(|f| f.stale).count()), (0, 0));
    assert_eq!(manifest.files.len(), 7);
}

#[test]
fn files_needing_oodle_fail_soft_without_it() {
    let fixture = fx::basic();
    let (_tmp, cache) = open(&fixture);
    let opts = IndexOptions { hash: true, ..Default::default() };
    let (manifest, stats) = index(&cache, None, &opts).unwrap();
    let need = fixture.sorted_files().iter().filter(|(_, f)| f.needs_oodle()).count() as u64;
    assert_eq!((need, stats.failed, stats.need_oodle), (2, 2, 2));
    for (got, (_, want)) in manifest.files.iter().zip(fixture.sorted_files()) {
        assert_eq!(got.error.is_some(), want.needs_oodle(), "{}", got.path);
        if let Some(e) = &got.error {
            assert!(e.contains("Oodle"), "{e}");
            assert!(got.header.is_some(), "the H part was still read");
        }
    }
}

#[test]
fn a_damaged_cache_fails_soft_per_file() {
    let fixture = fx::basic().without_oodle();
    let tmp = tempfile::tempdir().unwrap();
    fixture.write(tmp.path()).unwrap();

    // Cut B.Misc.cache short and scribble over the start of the level's H data.
    let b_cache = tmp.path().join("B.Misc.cache");
    let b = std::fs::read(&b_cache).unwrap();
    std::fs::write(&b_cache, &b[..b.len() / 2]).unwrap();
    let cache = Cache::open(tmp.path()).unwrap();
    let h = cache.package("Misc").unwrap().part(Part::H).unwrap();
    let level = h.find("/Lotus/Levels/Big.level").unwrap().offset as usize;
    drop(cache);
    let h_cache = tmp.path().join("H.Misc.cache");
    let mut bytes = std::fs::read(&h_cache).unwrap();
    bytes[level + 8..level + 12].copy_from_slice(&[0x20, 0xff, 0xff, 0xff]);
    std::fs::write(&h_cache, bytes).unwrap();

    let cache = Cache::open(tmp.path()).unwrap();
    let (manifest, stats) = index(&cache, None, &IndexOptions { hash: true, ..Default::default() }).unwrap();
    assert!(stats.failed >= 2 && stats.failed < stats.files, "{stats:?}");
    let error = |path: &str| manifest.files.iter().find(|f| f.path == path && !f.stale).unwrap().error.clone();
    let level_error = error("/Lotus/Levels/Big.level").unwrap();
    assert!(level_error.starts_with("H: block at 0x0: LZF"), "{level_error}");
    let body_error = error("/Lotus/BodyOnly.bin").unwrap();
    assert!(body_error.starts_with("B: ") && body_error.contains("outside the cache file"), "{body_error}");
    assert_eq!(error("/EE/NoHeader.bin"), None);
    assert_eq!(error("/Lotus/Interface/Icons/Thing.png"), None);
}

#[test]
fn bad_folders_are_clear_errors() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(Cache::open(tmp.path()), Err(Error::NoPackages(_))));
    assert!(matches!(Cache::open(&tmp.path().join("missing")), Err(Error::Io { .. })));

    fx::basic().write(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("H.Misc.toc"), b"this is not a table of contents").unwrap();
    let err = Cache::open(tmp.path()).err().unwrap();
    assert!(matches!(err, Error::Toc { .. }) && err.to_string().contains("H.Misc.toc"), "{err}");
}

fn one_file_cache(path: &str, pack: Pack) -> Fixture {
    let mut data = fx::common_header(&[path], "", 3);
    data.extend(fx::text(400, 1));
    let file = FxFile { path: path.into(), stale: false, parts: [Some(FxPart { data, pack }), None, None], type_id: Some(3), timestamp: fx::TIMESTAMP };
    Fixture { name: "one", packages: vec![FxPackage { name: "Misc".into(), parts: "H", files: vec![file] }] }
}

#[test]
fn other_games_and_eras_are_told_apart() {
    let (_tmp, soulframe) = open(&one_file_cache("/SF/Thing.fbx", Pack::Lzf));
    assert_eq!(soulframe.game, Game::Soulframe);
    assert!(index(&soulframe, None, &IndexOptions::default()).is_ok(), "Soulframe caches read like Warframe's");

    let (_tmp, unknown) = open(&one_file_cache("/D2/Thing.fbx", Pack::Lzf));
    assert_eq!(unknown.game, Game::Unknown);

    let (_tmp, old) = open(&one_file_cache("/Lotus/Thing.fbx", Pack::BareLzf));
    assert_eq!(old.game, Game::WarframePreEnsmallening);
    assert!(matches!(index(&old, None, &IndexOptions::default()), Err(Error::UnsupportedGame(Game::WarframePreEnsmallening))));
}

/// The whole real cache indexes without a single failure. Needs `WFSCAN_CACHE_DIR` and an
/// Oodle library (`WFSCAN_OODLE_DLL`); skipped otherwise. Add `WFSCAN_FULL=1` to decode
/// every B and F part too (minutes, not seconds).
#[test]
fn real_cache_indexes_cleanly() {
    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let oodle = match wfscan_core::Oodle::load(None) {
        Ok(oodle) => oodle,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let cache = Cache::open(Path::new(&dir)).unwrap();
    let opts = IndexOptions { hash: std::env::var_os("WFSCAN_FULL").is_some(), ..Default::default() };
    let (manifest, stats) = index(&cache, Some(&oodle), &opts).unwrap();
    eprintln!("{}: {stats:?}", cache.game);
    let failed: Vec<_> = manifest.files.iter().filter(|f| f.error.is_some()).take(10).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(stats.files > 100_000 && stats.headers * 10 > stats.files * 9, "{stats:?}");
}
