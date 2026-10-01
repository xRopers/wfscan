//! The export driver, `diff` and `report`, on synthetic caches with stand-in exporters.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use wfscan_core::{
    Cache, Category, Error, ExportError, ExportInput, ExportOptions, ExportStats, Exporter, Exporters, IndexOptions, Kind, Manifest,
    OodleDecoder, Output, Part, Status, diff, export, index, report,
};
use wfscan_fixtures::{self as fx, Fixture, FxPackage};

struct Flip;

impl OodleDecoder for Flip {
    fn decode(&self, comp: &[u8], out: &mut [u8]) -> Result<(), String> {
        fx::flip_decode(comp, out)
    }
}

/// A "model" exporter: writes the H body as `<path>.txt` and the B part as `<path>.bin`.
/// Paths containing `Fail`, `Variant` or `Panic` end the other ways an export can.
struct Models {
    version: u32,
    runs: AtomicU32,
}

impl Models {
    fn new(version: u32) -> Self {
        Self { version, runs: AtomicU32::new(0) }
    }

    fn runs(&self) -> u32 {
        self.runs.load(Ordering::Relaxed)
    }
}

impl Exporter for &'static Models {
    fn name(&self) -> &'static str {
        "model"
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn kind(&self) -> Kind {
        Kind::Model
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        assert_eq!((input.category, input.type_id()), (Category::Misc, 269));
        if input.path.contains("Fail") {
            return Err(ExportError::failed("vertex count 12 is not a multiple of 3"));
        }
        if input.path.contains("Variant") {
            return Err(ExportError::unsupported("no reader for this model layout"));
        }
        if input.path.contains("Panic") {
            panic!("index out of bounds");
        }
        if input.path.contains("Empty") {
            return Ok(vec![]);
        }
        if input.path.contains("Escape") {
            return Ok(vec![Output { path: "/../outside.txt".into(), data: vec![1] }]);
        }
        assert!(input.has(Part::B) && !input.has(Part::F));
        assert!(input.read(Part::F).is_err());
        Ok(vec![
            Output { path: format!("{}.txt", input.path), data: input.h_body().to_vec() },
            Output { path: format!("{}.bin", input.path), data: input.read(Part::B)? },
        ])
    }
}

fn models(version: u32) -> &'static Models {
    Box::leak(Box::new(Models::new(version)))
}

const MODEL_NAMES: [&str; 7] = ["A", "B", "Fail", "Variant", "Panic", "Empty", "Escape"];

/// Seven models that end every way an export can, a texture and a file of unknown type.
fn mixed() -> Fixture {
    let mut files: Vec<_> =
        MODEL_NAMES.iter().enumerate().map(|(i, n)| fx::simple_file(&format!("/Lotus/Models/{n}.fbx"), 269, i as u32 * 2)).collect();
    files.push(fx::simple_file("/Lotus/Scripts/Thing.lua", 22, 50));
    let texture = FxPackage { name: "Texture".into(), parts: "HB", files: vec![fx::simple_file("/Lotus/Textures/T.png", 163, 60)] };
    Fixture { name: "mixed", packages: vec![FxPackage { name: "Misc".into(), parts: "HB", files }, texture] }
}

struct Setup {
    tmp: tempfile::TempDir,
    cache: Cache,
    manifest: Manifest,
}

fn setup(fixture: &Fixture) -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fixture.write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (manifest, _) = index(&cache, Some(&Flip), &IndexOptions::default()).unwrap();
    Setup { tmp, cache, manifest }
}

impl Setup {
    fn out(&self) -> std::path::PathBuf {
        self.tmp.path().join("out")
    }

    fn export(&mut self, exporters: &Exporters, previous: Option<&Manifest>, opts: &ExportOptions) -> ExportStats {
        let out = self.out();
        export(&self.cache, &mut self.manifest, previous, exporters, Some(&Flip), &out, opts).unwrap()
    }

    fn record(&self, path: &str) -> &wfscan_core::ExportRecord {
        self.manifest.files.iter().find(|f| f.path == path && !f.stale).unwrap().export.as_ref().unwrap()
    }
}

fn counts(s: &ExportStats) -> (u64, u64, u64, u64, u64) {
    (s.selected, s.ok, s.failed, s.unsupported, s.skipped)
}

#[test]
fn every_file_gets_a_status() {
    let fixture = mixed();
    let mut s = setup(&fixture);
    let exporter = models(1);
    let stats = s.export(&Exporters::default().with(exporter), None, &ExportOptions::default());
    assert_eq!(counts(&stats), (9, 2, 4, 3, 0));
    assert_eq!(exporter.runs(), 7);

    let a = s.record("/Lotus/Models/A.fbx");
    assert_eq!((a.status, a.exporter.as_deref(), &a.reason), (Status::Ok, Some("model@1"), &None));
    assert_eq!(a.outputs, ["/Lotus/Models/A.fbx.txt", "/Lotus/Models/A.fbx.bin"]);
    let fx_a = &fixture.packages[0].files[0];
    let out = s.out().join("Lotus").join("Models");
    assert_eq!(std::fs::read(out.join("A.fbx.bin")).unwrap(), fx_a.parts[1].as_ref().unwrap().data);
    let h = &fx_a.parts[0].as_ref().unwrap().data;
    assert!(h.ends_with(&std::fs::read(out.join("A.fbx.txt")).unwrap()));
    assert_eq!((stats.outputs, stats.bytes), (4, 2 * (200 + 800)));

    let reason = |path: &str| {
        let r = s.record(path);
        (r.status, r.reason.clone().unwrap())
    };
    assert_eq!(reason("/Lotus/Models/Fail.fbx"), (Status::Failed, "vertex count 12 is not a multiple of 3".into()));
    assert_eq!(reason("/Lotus/Models/Variant.fbx"), (Status::Unsupported, "no reader for this model layout".into()));
    assert_eq!(reason("/Lotus/Models/Panic.fbx"), (Status::Failed, "the exporter panicked: index out of bounds".into()));
    assert_eq!(reason("/Lotus/Models/Empty.fbx"), (Status::Failed, "the exporter produced no files".into()));
    assert_eq!(reason("/Lotus/Models/Escape.fbx").0, Status::Failed);
    assert!(!s.tmp.path().join("outside.txt").exists());
    assert_eq!(reason("/Lotus/Scripts/Thing.lua"), (Status::Unsupported, "no reader for type 22".into()));
    assert_eq!(reason("/Lotus/Textures/T.png"), (Status::Unsupported, "no texture exporter yet".into()));
    assert_eq!(s.record("/Lotus/Models/Variant.fbx").exporter.as_deref(), Some("model@1"));

    // Nothing was left half-written.
    let names: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
}

#[test]
fn a_second_run_skips_what_still_stands() {
    let mut s = setup(&mixed());
    let exporter = models(1);
    let exporters = Exporters::default().with(exporter);
    s.export(&exporters, None, &ExportOptions::default());

    let stats = s.export(&exporters, None, &ExportOptions::default());
    assert_eq!(counts(&stats), (9, 0, 4, 3, 2), "ok files are skipped, failures are tried again");
    assert_eq!(exporter.runs(), 7 + 5);
    assert_eq!(s.record("/Lotus/Models/A.fbx").outputs.len(), 2, "a skipped record keeps its outputs");

    // A third run still skips: a skipped record counts as an earlier export.
    assert_eq!(s.export(&exporters, None, &ExportOptions::default()).skipped, 2);

    // A missing output, a new exporter version, or --force each mean exporting again.
    std::fs::remove_file(s.out().join("Lotus").join("Models").join("A.fbx.bin")).unwrap();
    let stats = s.export(&exporters, None, &ExportOptions::default());
    assert_eq!((stats.ok, stats.skipped), (1, 1));
    assert_eq!(s.record("/Lotus/Models/A.fbx").status, Status::Ok);

    let stats = s.export(&Exporters::default().with(models(2)), None, &ExportOptions::default());
    assert_eq!((stats.ok, stats.skipped), (2, 0));
    assert_eq!(s.record("/Lotus/Models/B.fbx").exporter.as_deref(), Some("model@2"));

    let forced = ExportOptions { force: true, ..Default::default() };
    let stats = s.export(&Exporters::default().with(models(2)), None, &forced);
    assert_eq!((stats.ok, stats.skipped), (2, 0));
}

#[test]
fn filters_select_files() {
    let mut s = setup(&mixed());
    let exporters = Exporters::default().with(models(1));
    let only = |s: &mut Setup, opts: ExportOptions| counts(&s.export(&exporters, None, &opts));

    assert_eq!(only(&mut s, ExportOptions { path: Some("/Lotus/Models/A".into()), ..Default::default() }), (1, 1, 0, 0, 0));
    assert_eq!(s.manifest.files.iter().filter(|f| f.export.is_some()).count(), 1, "unselected files are left alone");
    assert_eq!(only(&mut s, ExportOptions { package: Some("Texture".into()), ..Default::default() }), (1, 0, 0, 1, 0));
    assert_eq!(only(&mut s, ExportOptions { kinds: vec![Kind::Texture], ..Default::default() }), (1, 0, 0, 1, 0));
    assert_eq!(only(&mut s, ExportOptions { kinds: vec![Kind::Model, Kind::Audio], ..Default::default() }), (7, 1, 4, 1, 1));
    assert_eq!(only(&mut s, ExportOptions { path: Some("/Nope".into()), ..Default::default() }), (0, 0, 0, 0, 0));
}

#[test]
fn a_dry_run_writes_no_files() {
    let mut s = setup(&mixed());
    let stats = s.export(&Exporters::default().with(models(1)), None, &ExportOptions { dry_run: true, ..Default::default() });
    assert_eq!(counts(&stats), (9, 2, 4, 3, 0));
    assert_eq!((stats.outputs, stats.bytes), (4, 2000));
    assert!(!s.out().exists());
}

#[test]
fn a_manifest_of_another_cache_is_refused() {
    let mut s = setup(&mixed());
    let other = setup(&fx::basic());
    let out = s.out();
    let err = export(&other.cache, &mut s.manifest, None, &Exporters::default(), Some(&Flip), &out, &ExportOptions::default());
    assert!(matches!(&err, Err(Error::StaleManifest(_))), "{err:?}");
    assert!(err.unwrap_err().to_string().contains("wfscan index"));
    assert!(s.manifest.files.iter().all(|f| f.export.is_none()));
}

/// `basic` exported, then the game updates to `basic_updated`: only what changed is exported.
#[test]
fn changed_since_exports_only_what_changed() {
    // Type 269 files: one in basic, a second one added by the update.
    let mut before = setup(&fx::basic());
    let exporter = models(1);
    let exporters = Exporters::default().with(exporter);
    let out = before.out();
    let stats = before.export(&exporters, None, &ExportOptions::default());
    assert_eq!((stats.ok, exporter.runs()), (1, 1));

    let mut after = setup(&fx::basic_updated());
    let export_after = |after: &mut Setup, previous: Option<&Manifest>| {
        export(&after.cache, &mut after.manifest, previous, &exporters, Some(&Flip), &out, &ExportOptions::default()).unwrap()
    };
    let stats = export_after(&mut after, Some(&before.manifest));
    assert_eq!((stats.ok, stats.skipped, exporter.runs()), (1, 1, 2), "only the new model ran");
    assert_eq!(after.record("/Lotus/Characters/New_skel.fbx").status, Status::Ok);
    let kept = after.record("/Lotus/Characters/Tenno/Excalibur/Excalibur_skel.fbx");
    assert_eq!((kept.status, kept.outputs.len()), (Status::Skipped, 2));

    // Without the old manifest there is nothing to go on, so everything is exported.
    let mut fresh = setup(&fx::basic_updated());
    assert_eq!(export_after(&mut fresh, None).ok, 2);

    // A file that did change is exported again even though its old export succeeded.
    let mut edited = before.manifest.clone();
    let skel = edited.files.iter_mut().find(|f| f.path.ends_with("Excalibur_skel.fbx")).unwrap();
    skel.header.as_mut().unwrap().hash = "00".repeat(16);
    let mut again = setup(&fx::basic_updated());
    assert_eq!(export_after(&mut again, Some(&edited)).ok, 2);
}

#[test]
fn diff_finds_what_an_update_did() {
    let old = setup(&fx::basic()).manifest;
    let new = setup(&fx::basic_updated()).manifest;
    let d = diff(&old, &new);
    let paths = |files: &[&wfscan_core::FileEntry]| files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
    assert_eq!(paths(&d.added), ["/Lotus/Characters/New_skel.fbx"]);
    assert_eq!(paths(&d.removed), ["/EE/NoHeader.bin"]);
    assert_eq!(d.changed.iter().map(|(_, n)| n.path.as_str()).collect::<Vec<_>>(), ["/Lotus/Scripts/Test.lua"]);
    assert_eq!(d.unchanged, 5, "offsets moved, contents didn't");

    let row = |type_id: Option<u32>| d.summary.iter().find(|r| r.type_id == type_id).map(|r| (r.kind, r.added, r.removed, r.changed));
    assert_eq!(row(Some(269)), Some((Some(Kind::Model), 1, 0, 0)));
    assert_eq!(row(Some(22)), Some((None, 0, 0, 1)));
    assert_eq!(row(None), Some((None, 0, 1, 0)));
    assert_eq!(d.summary.len(), 3);

    let same = diff(&old, &old);
    assert!(same.added.is_empty() && same.removed.is_empty() && same.changed.is_empty() && same.summary.is_empty());
    assert_eq!(same.unchanged, 7);
}

#[test]
fn report_groups_by_type_and_failure() {
    let mut s = setup(&mixed());
    let before = report(&s.manifest);
    let models_row = before.types.iter().find(|t| t.type_id == Some(269)).unwrap();
    assert_eq!((models_row.category, models_row.kind, models_row.files, models_row.not_exported()), (Category::Misc, Some(Kind::Model), 7, 7));
    assert!(before.errors.is_empty());

    s.export(&Exporters::default().with(models(1)), None, &ExportOptions::default());
    let after = report(&s.manifest);
    let row = |category: Category, type_id: u32| after.types.iter().find(|t| t.category == category && t.type_id == Some(type_id)).unwrap();
    let m = row(Category::Misc, 269);
    assert_eq!((m.ok, m.failed, m.unsupported, m.skipped, m.not_exported()), (2, 4, 1, 0, 0));
    let header = |name: &str| fx::common_header(&[&format!("/Lotus/Models/{name}.fbx")], "", 269).len() as u64;
    assert_eq!(m.size, MODEL_NAMES.iter().map(|n| header(n) + 200 + 800).sum::<u64>(), "H and B sizes add up");
    let lua = row(Category::Misc, 22);
    assert_eq!((lua.kind, lua.files, lua.unsupported), (None, 1, 1));
    assert_eq!(row(Category::Texture, 163).kind, Some(Kind::Texture));
    assert_eq!(after.types.iter().map(|t| t.category).collect::<Vec<_>>(), [Category::Misc, Category::Misc, Category::Texture]);

    assert_eq!(after.errors.len(), 4, "{:#?}", after.errors);
    let fail = after.errors.iter().find(|e| e.message.starts_with("vertex count")).unwrap();
    assert_eq!((fail.message.as_str(), fail.files, fail.sample.as_str()), ("vertex count # is not a multiple of #", 1, "/Lotus/Models/Fail.fbx"));
}

/// The real cache: every file gets a status, and with no exporters yet they are all
/// `unsupported`. Needs `WFSCAN_CACHE_DIR` and `WFSCAN_OODLE_DLL`; skipped otherwise.
#[test]
fn real_cache_exports_without_exporters() {
    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let Ok(oodle) = wfscan_core::Oodle::load(None) else {
        eprintln!("skipped: no Oodle library");
        return;
    };
    let cache = Cache::open(Path::new(&dir)).unwrap();
    let (mut manifest, _) = index(&cache, Some(&oodle), &IndexOptions::default()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let stats = export(&cache, &mut manifest, None, &Exporters::default(), Some(&oodle), tmp.path(), &ExportOptions::default()).unwrap();
    eprintln!("{stats:?}");
    assert_eq!(stats.selected, stats.unsupported);
    let r = report(&manifest);
    let known: u64 = r.types.iter().filter(|t| t.kind.is_some()).map(|t| t.files).sum();
    eprintln!("{known} of {} files are of known kinds", stats.selected);
    assert!(known * 2 > stats.selected, "most files should be of a known kind");
}
