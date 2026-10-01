//! End-to-end tests of the `wfscan` binary on the synthetic caches.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use wfscan_fixtures::{self as fx, Fixture};

/// Run wfscan with no Oodle library configured, whatever the environment says.
fn wfscan(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wfscan"))
        .args(args)
        .env_remove("WFSCAN_OODLE_DLL")
        .env_remove("ZSCAN_OODLE_DLL")
        .output()
        .expect("failed to run wfscan")
}

fn ok(args: &[&str]) -> Output {
    let out = wfscan(args);
    assert!(out.status.success(), "wfscan {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn ok_json(args: &[&str]) -> Value {
    serde_json::from_slice(&ok(args).stdout).expect("stdout is not JSON")
}

fn failure(args: &[&str]) -> String {
    let out = wfscan(args);
    assert!(!out.status.success(), "wfscan {args:?} should have failed");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write(fixture: &Fixture) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fixture.write(&dir).unwrap();
    (tmp, dir.to_str().unwrap().to_string())
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn info_summarises_the_packages() {
    let (_tmp, cache) = write(&fx::basic());
    let info = ok_json(&["info", &cache, "--json"]);
    assert_eq!(info["game"], "warframe");
    let packages = info["packages"].as_array().unwrap();
    assert_eq!(packages.iter().map(|p| p["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Emblem", "Misc", "Texture"]);
    assert_eq!((&packages[1]["parts"], &packages[1]["files"], &packages[1]["stale"]), (&Value::from("HBF"), &Value::from(6), &Value::from(1)));

    let text = String::from_utf8(ok(&["info", &cache]).stdout).unwrap();
    assert!(text.contains("Warframe, 3 packages") && text.lines().any(|l| l.starts_with("total")), "{text}");
}

#[test]
fn index_then_list() {
    let fixture = fx::basic().without_oodle();
    let (tmp, cache) = write(&fixture);
    let manifest = tmp.path().join("m.json");
    let manifest = s(&manifest);

    let indexed = ok_json(&["index", &cache, "-o", manifest, "--hash", "--json"]);
    assert_eq!((&indexed["stats"]["files"], &indexed["stats"]["stale"], &indexed["stats"]["failed"]), (&Value::from(7), &Value::from(1), &Value::from(0)));
    assert_eq!(indexed["manifest"], manifest);

    let all = ok_json(&["list", "-m", manifest, "--json"]);
    let all = all.as_array().unwrap();
    let expected = fixture.sorted_files();
    assert_eq!(all.len(), expected.len());
    for (got, (package, want)) in all.iter().zip(&expected) {
        assert_eq!((got["package"].as_str(), got["path"].as_str()), (Some(package.name.as_str()), Some(want.path.as_str())));
        assert_eq!(got["header"]["type"].as_u64(), want.type_id.map(u64::from));
        let b = want.parts[1].as_ref().map(|p| u64::from(crc32fast::hash(&p.data)));
        assert_eq!(got["b"]["crc32"].as_u64(), b, "{}", want.path);
    }

    let count = |args: &[&str]| ok_json(&[&["list", "-m", manifest, "--json"], args].concat()).as_array().unwrap().len();
    assert_eq!(count(&["--type", "22"]), 2);
    assert_eq!(count(&["--type", "22", "--stale"]), 1);
    assert_eq!(count(&["--path", "/Lotus/Scripts/"]), 2);
    assert_eq!(count(&["--package", "Texture"]), 1);
    assert_eq!(count(&["--failed"]), 0);
    assert_eq!(count(&["--path", "/Nope/"]), 0);

    let text = ok(&["list", "-m", manifest, "--path", "/Lotus/Scripts/"]);
    let lines = String::from_utf8(text.stdout).unwrap();
    assert_eq!(lines.lines().count(), 2, "{lines}");
    assert!(lines.contains("HB-") && lines.contains("[stale]") && lines.contains("/Lotus/Scripts/Test.lua"), "{lines}");
    assert!(String::from_utf8_lossy(&text.stderr).contains("2 of 8 files"));
}

#[test]
fn index_without_output_writes_nothing() {
    let (tmp, cache) = write(&fx::basic().without_oodle());
    let text = String::from_utf8(ok(&["index", &cache]).stdout).unwrap();
    assert!(text.contains("7 files (1 stale)") && text.contains("no manifest written"), "{text}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1, "only the cache folder");
}

#[test]
fn index_says_how_to_get_oodle() {
    let (tmp, cache) = write(&fx::basic());
    let manifest = tmp.path().join("m.json");
    let err = failure(&["index", &cache, "-o", s(&manifest), "--hash"]);
    assert!(err.contains("2 files have Oodle blocks"), "{err}");
    if cfg!(feature = "oodle") {
        assert!(err.contains("--oodle-dll") && err.contains("WFSCAN_OODLE_DLL"), "{err}");
    } else {
        assert!(err.contains("built without Oodle"), "{err}");
    }
    assert!(!manifest.exists(), "no manifest for a cache that couldn't be read");

    // Without --hash only H parts are read, and none of those are Oodle here.
    ok(&["index", &cache, "-o", s(&manifest)]);
    assert!(manifest.exists());
}

#[test]
fn raw_writes_a_file_or_a_folder() {
    let fixture = fx::basic().without_oodle();
    let (tmp, cache) = write(&fixture);
    let out = tmp.path().join("raw");
    let skel = "/Lotus/Characters/Tenno/Excalibur/Excalibur_skel.fbx";
    let fx_skel = &fixture.packages[0].files[0];
    assert_eq!(fx_skel.path, skel);

    let written = ok_json(&["raw", &cache, "--path", skel, "-d", s(&out), "--json"]);
    let written = written.as_array().unwrap();
    assert_eq!(written.iter().map(|w| w["part"].as_str().unwrap()).collect::<Vec<_>>(), ["H", "B"]);
    let base = out.join("Misc").join("Lotus").join("Characters").join("Tenno").join("Excalibur");
    assert_eq!(std::fs::read(base.join("Excalibur_skel.fbx.H")).unwrap(), fx_skel.parts[0].as_ref().unwrap().data);
    assert_eq!(std::fs::read(base.join("Excalibur_skel.fbx.B")).unwrap(), fx_skel.parts[1].as_ref().unwrap().data);
    assert_eq!(written[1]["file"], s(&base.join("Excalibur_skel.fbx.B")));

    let folder = tmp.path().join("folder");
    let written = ok_json(&["raw", &cache, "--path", "/Lotus/Scripts/", "-d", s(&folder), "--json"]);
    assert_eq!(written.as_array().unwrap().len(), 2, "the live H and B, not the stale copy");
    let live = fixture.packages[0].files.iter().find(|f| f.path == "/Lotus/Scripts/Test.lua" && !f.stale).unwrap();
    let h = std::fs::read(folder.join("Misc").join("Lotus").join("Scripts").join("Test.lua.H")).unwrap();
    assert_eq!(h, live.parts[0].as_ref().unwrap().data);

    let everything = tmp.path().join("all");
    let written = ok_json(&["raw", &cache, "--path", "/", "-d", s(&everything), "--package", "Texture", "--json"]);
    assert_eq!(written.as_array().unwrap().len(), 2);
}

#[test]
fn raw_errors() {
    let (tmp, cache) = write(&fx::basic());
    let out = tmp.path().join("raw");
    let err = failure(&["raw", &cache, "--path", "/Lotus/Nope.fbx", "-d", s(&out)]);
    assert!(err.contains("no file at /Lotus/Nope.fbx"), "{err}");
    let err = failure(&["raw", &cache, "--path", "/Lotus/", "--package", "Nope", "-d", s(&out)]);
    assert!(err.contains("no package named Nope"), "{err}");
    let err = failure(&["raw", &cache, "--path", "/Lotus/Interface/Icons/Thing.png", "-d", s(&out)]);
    assert!(err.contains("Oodle"), "{err}");
    let err = failure(&["info", s(tmp.path())]);
    assert!(err.contains("no .toc files"), "{err}");
}

/// Index `fixture` into `<tmp>/<name>.json`.
fn indexed(fixture: &Fixture, name: &str) -> (tempfile::TempDir, String, String) {
    let (tmp, cache) = write(fixture);
    let manifest = s(&tmp.path().join(name)).to_string();
    ok(&["index", &cache, "-o", &manifest]);
    (tmp, cache, manifest)
}

#[test]
fn export_records_a_status_for_every_file() {
    let (tmp, cache, manifest) = indexed(&fx::basic().without_oodle(), "m.json");
    let out = tmp.path().join("out");

    // A dry run changes nothing.
    let before = std::fs::read(&manifest).unwrap();
    let dry = ok_json(&["export", &cache, "-m", &manifest, "-d", s(&out), "--dry-run", "--json"]);
    assert_eq!((&dry["dry_run"], &dry["stats"]["selected"]), (&Value::from(true), &Value::from(7)));
    assert_eq!(std::fs::read(&manifest).unwrap(), before);

    assert!(!out.exists());

    // The texture exports; the rest have no exporter yet and say so.
    let exported = ok_json(&["export", &cache, "-m", &manifest, "-d", s(&out), "--json"]);
    let stats = |v: &Value| ["selected", "ok", "failed", "unsupported", "skipped"].map(|k| v["stats"][k].as_u64().unwrap());
    assert_eq!(stats(&exported), [7, 1, 0, 6, 0]);
    let unsupported = ok_json(&["list", "-m", &manifest, "--status", "unsupported", "--json"]);
    assert_eq!(unsupported.as_array().unwrap().len(), 6, "the stale entry is not exported");
    let model = ok_json(&["list", "-m", &manifest, "--kind", "model", "--json"]);
    assert_eq!(model.as_array().unwrap().len(), 1);
    assert_eq!(model[0]["export"]["reason"], "no model exporter yet");
    let done = ok_json(&["list", "-m", &manifest, "--status", "ok", "--json"]);
    assert_eq!(done.as_array().unwrap().len(), 1);
    assert_eq!((&done[0]["export"]["exporter"], &done[0]["export"]["outputs"][0]), (&Value::from("texture-dds@1"), &Value::from("/Lotus/Interface/Icons/Thing.dds")));
    let icons = out.join("Lotus").join("Interface").join("Icons");
    let dds = std::fs::read(icons.join("Thing.dds")).unwrap();
    assert_eq!((&dds[..4], dds.len()), (&b"DDS "[..], 148 + (256 + 64 + 16 + 4 + 1) * 4));

    // A second run has nothing to do; another texture format does.
    let again = ok_json(&["export", &cache, "-m", &manifest, "-d", s(&out), "--json"]);
    assert_eq!(stats(&again), [7, 0, 0, 6, 1]);
    let png = wfscan(&["export", &cache, "-m", &manifest, "-d", s(&out), "--texture-format", "png", "--json"]);
    if cfg!(feature = "png") {
        assert_eq!(stats(&serde_json::from_slice(&png.stdout).unwrap()), [7, 1, 0, 6, 0]);
        assert_eq!(&std::fs::read(icons.join("Thing.png")).unwrap()[1..4], b"PNG");
    } else {
        let reason = ok_json(&["list", "-m", &manifest, "--kind", "texture", "--json"])[0]["export"]["reason"].to_string();
        assert!(reason.contains("built without PNG support"), "{reason}");
    }
    let err = failure(&["export", &cache, "-m", &manifest, "-d", s(&out), "--texture-format", "jpg"]);
    assert!(err.contains("unknown texture format"), "{err}");

    let only = ok_json(&["export", &cache, "-m", &manifest, "-d", s(&out), "--kinds", "texture,model", "--json"]);
    assert_eq!(only["stats"]["selected"], 2);
    let text = String::from_utf8(ok(&["export", &cache, "-m", &manifest, "-d", s(&out), "--path", "/Lotus/Scripts/"]).stdout).unwrap();
    assert!(text.contains("1 files selected: 0 ok, 0 failed, 1 unsupported, 0 skipped") && text.contains("updated"), "{text}");

    let err = failure(&["export", &cache, "-m", &manifest, "-d", s(&out), "--kinds", "mesh"]);
    assert!(err.contains("unknown kind"), "{err}");
}

#[test]
fn report_shows_coverage() {
    let (tmp, cache, manifest) = indexed(&fx::basic().without_oodle(), "m.json");
    ok(&["export", &cache, "-m", &manifest, "-d", s(&tmp.path().join("out"))]);

    let report = ok_json(&["report", "-m", &manifest, "--json"]);
    let types = report["types"].as_array().unwrap();
    let row = |category: &str, type_id: Value| types.iter().find(|t| t["category"] == category && t["type"] == type_id).unwrap();
    let model = row("misc", Value::from(269));
    assert_eq!((&model["kind"], &model["files"], &model["unsupported"]), (&Value::from("model"), &Value::from(1), &Value::from(1)));
    assert_eq!(row("misc", Value::from(22))["kind"], Value::Null);
    assert_eq!(row("misc", Value::Null)["files"], 2, "NoHeader.bin and BodyOnly.bin");
    assert_eq!(row("texture", Value::from(163))["kind"], "texture");

    let unknown = ok_json(&["report", "-m", &manifest, "--unknown", "--json"]);
    let unknown: Vec<u64> = unknown["types"].as_array().unwrap().iter().map(|t| t["type"].as_u64().unwrap()).collect();
    assert_eq!(unknown.len(), 3, "types 1, 5 and 22: {unknown:?}");

    let text = String::from_utf8(ok(&["report", "-m", &manifest]).stdout).unwrap();
    assert!(text.contains("(no header)") && text.contains("2 files of known kinds, 3 files of 3 unknown types, 2 without a header"), "{text}");
}

#[test]
fn export_refuses_an_out_of_date_manifest() {
    let (tmp, cache, manifest) = indexed(&fx::basic().without_oodle(), "m.json");
    fx::basic_updated().without_oodle().write(Path::new(&cache)).unwrap();
    let err = failure(&["export", &cache, "-m", &manifest, "-d", s(&tmp.path().join("out"))]);
    assert!(err.contains("no longer matches the manifest") && err.contains("wfscan index"), "{err}");
}

#[test]
fn diff_between_two_indexes() {
    let (_old_tmp, _, old) = indexed(&fx::basic().without_oodle(), "old.json");
    let (_new_tmp, new_cache, new) = indexed(&fx::basic_updated().without_oodle(), "new.json");

    let d = ok_json(&["diff", &old, &new, "--json"]);
    let paths = |key: &str| d[key].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(paths("added"), ["/Lotus/Characters/New_skel.fbx"]);
    assert_eq!(paths("removed"), ["/EE/NoHeader.bin"]);
    assert_eq!(paths("changed"), ["/Lotus/Scripts/Test.lua"]);
    assert_eq!(d["unchanged"], 5);
    assert_eq!(d["summary"].as_array().unwrap().len(), 3);

    let text = String::from_utf8(ok(&["diff", &old, &new, "--list"]).stdout).unwrap();
    assert!(text.starts_with("1 added, 1 removed, 1 changed, 5 unchanged"), "{text}");
    for line in ["+ Misc  /Lotus/Characters/New_skel.fbx", "- Misc  /EE/NoHeader.bin", "~ Misc  /Lotus/Scripts/Test.lua"] {
        assert!(text.lines().any(|l| l == line), "{line} missing from:\n{text}");
    }
    let same = String::from_utf8(ok(&["diff", &new, &new]).stdout).unwrap();
    assert_eq!(same.trim(), "0 added, 0 removed, 0 changed, 7 unchanged");

    // The old manifest can stand in as the earlier export for the new cache.
    let out = _new_tmp.path().join("out");
    ok(&["export", &new_cache, "-m", &new, "-d", s(&out), "--changed-since", &old]);
}

#[cfg(windows)]
#[test]
fn a_cache_being_written_is_refused() {
    let (tmp, cache) = write(&fx::basic().without_oodle());
    let _writer = std::fs::OpenOptions::new().write(true).open(Path::new(&cache).join("H.Misc.cache")).unwrap();
    let err = failure(&["index", &cache]);
    assert!(err.contains("H.Misc.cache") && err.contains("open for writing"), "{err}");
    let err = failure(&["raw", &cache, "--path", "/Lotus/", "-d", s(&tmp.path().join("raw"))]);
    assert!(err.contains("open for writing"), "{err}");
    ok(&["index", &cache, "--allow-in-use"]);
    ok(&["info", &cache]);
}
