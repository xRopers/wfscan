//! The session driven through jobs, with no window: open a cache, filter, export.

mod common;

use std::collections::HashSet;

use wfscan_core::{FormatOptions, Kind, Status, TextureOutput};
use wfscan_fixtures::{mdl, tex};
use wfscan_gui::jobs::{Jobs, finish};
use wfscan_gui::session::{self, Filter, Level, Session};

fn opened(input: &common::Input) -> (Session, Jobs) {
    let (mut session, mut jobs) = (Session::default(), Jobs::default());
    let dir = input.cache.clone();
    assert!(jobs.start("Opening", move || finish("Open", session::open_cache(&dir, None), Session::set_loaded)));
    assert!(!jobs.start("Again", || Box::new(|_| ())), "one job at a time");
    jobs.wait(&mut session);
    assert!(!jobs.busy());
    (session, jobs)
}

#[test]
fn open_filter_and_export() {
    let input = common::input();
    let (mut session, mut jobs) = opened(&input);
    let loaded = session.loaded.as_ref().expect("the cache opened");
    assert_eq!((loaded.manifest.files.len(), loaded.stats.failed), (common::FILES, 0));
    assert_eq!(session.generation, 1);
    assert!(session.log[0].text.starts_with("opened ") && session.log[0].level == Level::Info, "{:?}", session.log);

    // Filters: text in any case, kind, known kinds only.
    let count = |filter: Filter| session.view(&filter).len();
    assert_eq!(count(Filter::default()), common::FILES);
    assert_eq!(count(Filter { text: "CRATE".into(), ..Default::default() }), 3);
    assert_eq!(count(Filter { kind: Some(Kind::Texture), ..Default::default() }), 13);
    assert_eq!(count(Filter { kind: Some(Kind::Model), ..Default::default() }), 7);
    assert_eq!(count(Filter { kind: Some(Kind::Audio), ..Default::default() }), 6);
    assert_eq!(count(Filter { known_only: true, ..Default::default() }), common::FILES);
    assert_eq!(count(Filter { status: Some(Status::Ok), ..Default::default() }), 0, "nothing is exported yet");

    // Export the models and one texture.
    let mut files: HashSet<usize> = session.view(&Filter { kind: Some(Kind::Model), ..Default::default() }).into_iter().collect();
    files.insert(session.files().iter().position(|f| f.path == tex::SPLIT).unwrap());
    let out = input.dir.path().join("out");
    let (cache, manifest) = (loaded.cache.clone(), loaded.manifest.clone());
    let dir = out.clone();
    jobs.start("Exporting", move || {
        let result = session::run_export(&cache, manifest, files, &dir, &FormatOptions { texture: TextureOutput::Png, ..Default::default() }, None);
        finish("Export", result, move |s, (manifest, stats)| s.set_exported(manifest, stats, dir))
    });
    jobs.wait(&mut session);

    let (stats, dir) = session.last_export.clone().expect("the export finished");
    assert_eq!((stats.selected, stats.ok, stats.unsupported, stats.failed), (8, 4, 3, 1));
    assert_eq!((dir, session.generation), (out.clone(), 2));
    assert!(out.join("Lotus").join("Characters").join("Hero").join("Hero_skel.glb").is_file());
    assert!(out.join("Lotus").join("Textures").join("Split_d.png").is_file());
    assert_eq!(session.view(&Filter { status: Some(Status::Ok), ..Default::default() }).len(), 4);
    assert_eq!(session.view(&Filter { status: Some(Status::Failed), text: "broken".into(), ..Default::default() }).len(), 1);
    let record = session.files().iter().find(|f| f.path == mdl::RIGGED).unwrap().export.as_ref().unwrap();
    assert_eq!(record.status, Status::Ok);
    assert!(session.files().iter().find(|f| f.path == tex::ICON).unwrap().export.is_none(), "files not chosen are left alone");

    session.close();
    assert!(session.loaded.is_none() && session.files().is_empty() && session.view(&Filter::default()).is_empty());
}

#[test]
fn a_folder_that_is_not_a_cache_is_an_error_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, mut jobs) = (Session::default(), Jobs::default());
    let path = dir.path().to_path_buf();
    jobs.start("Opening", move || finish("Open", session::open_cache(&path, None), Session::set_loaded));
    jobs.wait(&mut session);
    assert!(session.loaded.is_none());
    let last = session.log.last().unwrap();
    assert!(last.level == Level::Error && last.text.contains("no .toc files"), "{last:?}");
}

#[test]
fn a_missing_oodle_library_is_reported() {
    let mut session = Session::default();
    session.set_oodle(Some(std::path::Path::new("no/such/oo2core_9_win64.dll")));
    assert!(session.oodle.is_none());
    assert!(session.oodle_error.as_deref().is_some_and(|e| e.contains("not found")), "{:?}", session.oodle_error);
}
