//! Videos, Flash movies, compiled scripts and sound banks: copied out after a check.

use std::collections::HashMap;

use wfscan_core::{BinkLibrary, Cache, ExportOptions, Exporters, FormatOptions, IndexOptions, Kind, Manifest, Status, VideoOutput, export, index};
use wfscan_fixtures::{self as fx, pt};

fn exported() -> (tempfile::TempDir, Manifest, wfscan_core::ExportStats) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::passthrough().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, _) = index(&cache, None, &IndexOptions::default()).unwrap();
    let exporters = Exporters::builtin(&FormatOptions::default());
    let stats = export(&cache, &mut manifest, None, &exporters, None, &tmp.path().join("out"), &ExportOptions::default()).unwrap();
    (tmp, manifest, stats)
}

/// The fixture file's part (0 H, 1 B, 2 F) as written.
fn fixture_part(path: &str, part: usize) -> Vec<u8> {
    let fixture = fx::passthrough();
    let file = fixture.packages.iter().flat_map(|p| &p.files).find(|f| f.path == path).unwrap();
    file.parts[part].as_ref().unwrap().data.clone()
}

#[test]
fn the_type_table_knows_them() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::passthrough().write(&dir).unwrap();
    let (manifest, _) = index(&Cache::open(&dir).unwrap(), None, &IndexOptions::default()).unwrap();
    let classifier = manifest.classifier();
    let kinds: HashMap<&str, Option<Kind>> = manifest.files.iter().map(|f| (f.path.as_str(), classifier.kind(f))).collect();
    assert_eq!(kinds[pt::VIDEO], Some(Kind::Video));
    assert_eq!(kinds[pt::VIDEO_STREAMED], Some(Kind::Video), "VideoTextureStreaming is the same category");
    assert_eq!(kinds[pt::FLASH], Some(Kind::Flash));
    assert_eq!(kinds[pt::FLASH_OTHER], Some(Kind::Flash));
    assert_eq!(kinds[pt::SCRIPT], Some(Kind::Script));
    assert_eq!(kinds[pt::SCRIPT_BROKEN], Some(Kind::Script));
    assert_eq!(kinds[pt::BANK], Some(Kind::SoundBank));
}

#[test]
fn each_kind_is_copied_out_or_says_why_not() {
    let (tmp, manifest, stats) = exported();
    assert_eq!((stats.selected, stats.ok, stats.failed, stats.unsupported), (10, 5, 2, 3), "{stats:?}");
    let record = |path: &str| manifest.files.iter().find(|f| f.path == path).unwrap().export.clone().unwrap();
    let out = |internal: &str| std::fs::read(tmp.path().join("out").join(internal.trim_start_matches('/'))).unwrap();

    // Videos: F as it is, whichever Bink version; a cut-short one fails with both sizes.
    let video = record(pt::VIDEO);
    assert_eq!((video.status, video.exporter.as_deref(), video.outputs.clone()), (Status::Ok, Some("video-bk2@1"), vec![pt::VIDEO.to_string()]));
    assert_eq!(out(pt::VIDEO), fixture_part(pt::VIDEO, 2));
    let streamed = record(pt::VIDEO_STREAMED);
    assert_eq!(streamed.outputs, ["/Lotus/Interface/AbilityVideos/Power1_s.bik"], "a Bink 1 file is named .bik");
    assert_eq!(out(&streamed.outputs[0]), fixture_part(pt::VIDEO_STREAMED, 2));
    let short = record(pt::VIDEO_SHORT);
    assert_eq!((short.status, short.reason.as_deref()), (Status::Failed, Some("F: the Bink header says 600 bytes, the part holds 500")));

    // Flash: B as it is; without the magic it isn't a movie this exporter knows.
    assert_eq!(out(pt::FLASH), fixture_part(pt::FLASH, 1));
    let other = record(pt::FLASH_OTHER);
    assert_eq!((other.status, other.reason.as_deref()), (Status::Unsupported, Some("B doesn't start with FWS, CWS or ZWS")));

    // Scripts: B as .luac, after checking the string table.
    let script = record(pt::SCRIPT);
    assert_eq!(script.outputs, ["/EE/Interface/AnchorMgr.luac"]);
    assert_eq!(out(&script.outputs[0]), fixture_part(pt::SCRIPT, 1));
    let broken = record(pt::SCRIPT_BROKEN);
    assert_eq!(broken.status, Status::Failed);
    assert!(broken.reason.as_deref().unwrap().contains("string 0 runs off the end"), "{broken:?}");

    // Sound banks: the attribute text.
    let bank = record(pt::BANK);
    assert_eq!(bank.outputs, [format!("{}.txt", pt::BANK)]);
    assert_eq!(String::from_utf8(out(&bank.outputs[0])).unwrap(), fx::BANK_TEXT);
    let empty = record(pt::BANK_EMPTY);
    assert_eq!((empty.status, empty.reason.as_deref()), (Status::Unsupported, Some("the sound bank has no attribute text")));
    let not_a_bank = record(pt::NOT_A_BANK);
    assert_eq!((not_a_bank.status, not_a_bank.reason.as_deref()), (Status::Unsupported, Some("has data in B or F, so it isn't a plain sound bank")));
}

/// Every video, Flash movie, script and sound bank in a real cache. Needs
/// `WFSCAN_CACHE_DIR` and `WFSCAN_OODLE_DLL`; skipped otherwise.
#[test]
fn real_passthrough_files_all_check_out() {
    let (Some(dir), Ok(oodle)) = (std::env::var_os("WFSCAN_CACHE_DIR"), wfscan_core::Oodle::load(None)) else {
        eprintln!("skipped: set WFSCAN_CACHE_DIR and WFSCAN_OODLE_DLL");
        return;
    };
    let cache = Cache::open(std::path::Path::new(&dir)).unwrap();
    let (mut manifest, _) = index(&cache, Some(&oodle), &IndexOptions::default()).unwrap();
    let exporters = Exporters::builtin(&FormatOptions::default());
    let out = tempfile::tempdir().unwrap();
    for kind in [Kind::Video, Kind::Flash, Kind::Script, Kind::SoundBank] {
        let opts = ExportOptions { kinds: vec![kind], dry_run: true, ..Default::default() };
        let stats = export(&cache, &mut manifest, None, &exporters, Some(&oodle), out.path(), &opts).unwrap();
        eprintln!("{kind}: {} files, {} ok, {} failed, {} unsupported, {:.1} MiB", stats.selected, stats.ok, stats.failed, stats.unsupported, stats.bytes as f64 / 1048576.0);
        let classifier = manifest.classifier();
        let mut reasons: HashMap<String, (u64, String)> = HashMap::new();
        for f in manifest.files.iter().filter(|f| !f.stale && classifier.kind(f) == Some(kind)) {
            let e = f.export.as_ref().unwrap();
            if e.status != Status::Ok {
                let entry = reasons.entry(e.reason.clone().unwrap_or_default()).or_insert((0, f.path.clone()));
                entry.0 += 1;
            }
        }
        for (reason, (n, sample)) in &reasons {
            eprintln!("  {n} x {reason} (e.g. {sample})");
        }
        assert!(stats.selected > 0 && stats.failed == 0, "{kind}: {stats:?}");
    }
}

/// Videos as PNG frames need the Bink library; without it they are unsupported, and say
/// where the game keeps one.
#[test]
fn video_frames_without_the_bink_library_say_why() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::passthrough().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, _) = index(&cache, None, &IndexOptions::default()).unwrap();
    let exporters = Exporters::builtin(&FormatOptions { video: VideoOutput::Png, ..Default::default() });
    let opts = ExportOptions { kinds: vec![Kind::Video], ..Default::default() };
    let stats = export(&cache, &mut manifest, None, &exporters, None, &tmp.path().join("out"), &opts).unwrap();
    assert_eq!((stats.selected, stats.unsupported, stats.failed), (3, 2, 1), "{stats:?}");
    let video = manifest.files.iter().find(|f| f.path == pt::VIDEO).unwrap().export.clone().unwrap();
    assert_eq!((video.status, video.exporter.as_deref()), (Status::Unsupported, Some("video-png@1")));
    assert!(video.reason.as_deref().unwrap().contains("Tools\\Bink2\\bink2w64.dll"), "{video:?}");
    // A cut-short file still fails on its header, before any library is needed.
    let short = manifest.files.iter().find(|f| f.path == pt::VIDEO_SHORT).unwrap().export.clone().unwrap();
    assert_eq!(short.status, Status::Failed);
}

/// A few real videos decoded to PNG frames with the game's own Bink library: every frame
/// is written, at the size and count the file's header gives. Needs `WFSCAN_CACHE_DIR`,
/// `WFSCAN_OODLE_DLL` and the game's `Tools\Bink2\bink2w64.dll` next to the cache.
#[test]
fn real_videos_decode_to_frames() {
    let (Some(dir), Ok(oodle)) = (std::env::var_os("WFSCAN_CACHE_DIR"), wfscan_core::Oodle::load(None)) else {
        eprintln!("skipped: set WFSCAN_CACHE_DIR and WFSCAN_OODLE_DLL");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let Some(dll) = BinkLibrary::next_to_cache(&dir) else {
        eprintln!("skipped: no Tools\\Bink2\\bink2w64.dll next to the cache");
        return;
    };
    let bink = std::sync::Arc::new(BinkLibrary::load(Some(&dll)).unwrap());
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, _) = index(&cache, Some(&oodle), &IndexOptions::default()).unwrap();
    let exporters = Exporters::builtin(&FormatOptions { video: VideoOutput::Png, bink: Some(bink), ..Default::default() });
    let out = tempfile::tempdir().unwrap();
    let opts = ExportOptions { path: Some("/Lotus/Interface/AbilityVideos/A".into()), kinds: vec![Kind::Video], ..Default::default() };
    let stats = export(&cache, &mut manifest, None, &exporters, Some(&oodle), out.path(), &opts).unwrap();
    eprintln!("{stats:?}");
    assert!(stats.selected >= 2 && stats.ok == stats.selected, "{stats:?}");
    for file in manifest.files.iter().filter(|f| f.export.as_ref().is_some_and(|e| e.status == Status::Ok)) {
        let record = file.export.as_ref().unwrap();
        let folder = wfscan_core::formats::video::frames_folder(&file.path);
        let on_disk = |internal: &str| out.path().join(internal.trim_start_matches('/'));
        let info: serde_json::Value = serde_json::from_slice(&std::fs::read(on_disk(&format!("{folder}/video.json"))).unwrap()).unwrap();
        let frames = info["frames"].as_u64().unwrap() as usize;
        assert_eq!(record.outputs.len(), frames + 1, "{}: one PNG per frame and the JSON", file.path);
        let last = std::fs::read(on_disk(&format!("{folder}/{:05}.png", frames - 1))).unwrap();
        let image = texscan_core_png_size(&last);
        assert_eq!(image, (info["width"].as_u64().unwrap() as u32, info["height"].as_u64().unwrap() as u32), "{}", file.path);
    }
}

/// Width and height from a PNG's IHDR.
fn texscan_core_png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()))
}
