//! The window itself, driven with egui_kittest (no GPU): clicks go through the real
//! widgets and are checked through the accessibility tree.

mod common;

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use wfscan_core::{Kind, Status};
use wfscan_fixtures::{mdl, snd, tex};
use wfscan_gui::App;
use wfscan_core::Part;
use wfscan_gui::app::{SortKey, Tab, ViewMode};
use wfscan_gui::tree::Row;

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size([1500.0, 900.0]).build_ui_state(|ui, app: &mut App| app.show(ui), App::new())
}

/// Finish the running job (if any) and let the window catch up.
fn settle(h: &mut Harness<'static, App>) {
    let app = h.state_mut();
    app.jobs.wait(&mut app.session);
    h.run_steps(3);
}

/// Step until `label` appears (previews load on their own thread).
fn wait_for(h: &mut Harness<'static, App>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.query_by_label_contains(label).is_none() {
        assert!(Instant::now() < deadline, "'{label}' never appeared");
        std::thread::sleep(Duration::from_millis(10));
        h.step();
    }
}

fn open(h: &mut Harness<'static, App>, input: &common::Input) {
    h.state_mut().open_cache(input.cache.clone());
    settle(h);
}

fn select(h: &mut Harness<'static, App>, path: &str) {
    let index = h.state().session.files().iter().position(|f| f.path == path).unwrap();
    h.state_mut().select(index);
    h.run_steps(2);
}

#[test]
fn welcome_screen() {
    let mut h = harness();
    h.run_steps(2);
    h.get_by_label("Open a cache folder…");
    h.get_by_label_contains("Cache.Windows folder");
}

#[test]
fn open_shows_the_files_and_filters_them() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    assert_eq!(h.state().shown().len(), common::FILES);
    h.get_by_label(&format!("{0} of {0} files", common::FILES));
    h.get_by_label(tex::SPLIT);
    h.get_by_label(mdl::RIGGED);

    h.state_mut().filter.text = "crate".into();
    h.run_steps(2);
    h.get_by_label(&format!("3 of {} files", common::FILES));
    assert!(h.query_by_label(tex::SPLIT).is_none());
    h.get_by_label("Export 3 shown…");

    h.state_mut().filter = wfscan_gui::session::Filter { kind: Some(Kind::Texture), ..Default::default() };
    h.run_steps(2);
    assert_eq!(h.state().shown().len(), 13);
}

#[test]
fn clicking_a_texture_shows_it() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    h.get_by_label(tex::SPLIT).click();
    h.run_steps(2);
    let selected = h.state().selected.expect("the row is selected");
    assert_eq!(h.state().session.files()[selected].path, tex::SPLIT);
    h.get_by_label("Split_d.png");
    h.get_by_label("texture, type 163");
    wait_for(&mut h, "16×16 Bgra8, 5 mips");

    // A cube map says so; a format wfscan doesn't know says that.
    select(&mut h, tex::CUBE);
    wait_for(&mut h, "cube map (first face shown)");
    select(&mut h, tex::NEW_FORMAT);
    wait_for(&mut h, "pixel format 32 is not known");
}

#[test]
fn a_model_and_a_file_without_a_reader() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    select(&mut h, mdl::RIGGED);
    h.get_by_label("Hero_skel.fbx");
    wait_for(&mut h, "4 vertices, 2 triangles, 2 meshes, 3 bones");
    h.get_by_label("Attributes");
    h.get_by_label_contains("Drag to turn");

    select(&mut h, mdl::OTHER_TYPE);
    wait_for(&mut h, "no reader for model type 290");
    select(&mut h, mdl::COLLISION);
    wait_for(&mut h, "collision data only");
}

#[test]
fn a_sound_shows_its_waveform_and_a_play_button() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    select(&mut h, snd::TONE);
    h.get_by_label("Tone.wav");
    wait_for(&mut h, "PCM, mono, 8000 Hz, 0:00.5");
    h.get_by_label("waveform");
    h.get_by_label("Play");
    h.get_by_label("0:00.0 / 0:00.5");
    assert!(!h.state().is_playing());

    // Sounds that can't be decoded say why instead.
    select(&mut h, snd::VORBIS);
    wait_for(&mut h, "Vorbis, mono, 8000 Hz, 0:00.1");
    h.get_by_label_contains("no Vorbis header set for 1 channels at 8000 Hz");
    assert!(h.query_by_label("Play").is_none());
    select(&mut h, snd::ADPCM);
    wait_for(&mut h, "ADPCM sounds have no reader yet");
}

#[test]
fn the_table_sorts_by_path_and_by_name() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    let paths = |h: &Harness<'static, App>| -> Vec<String> { h.state().shown().iter().map(|&i| h.state().session.files()[i].path.clone()).collect() };
    let sorted = |mut list: Vec<String>, key: fn(&String) -> String| {
        list.sort_by_key(key);
        list
    };
    h.get_by_label("Sort by");
    // By default packages stay together; by path they interleave.
    let by_package = paths(&h);
    h.state_mut().sort = (SortKey::Path, true);
    h.run_steps(2);
    let by_path = paths(&h);
    assert_ne!(by_path, by_package);
    assert_eq!(by_path, sorted(by_package.clone(), |p| p.to_ascii_lowercase()));
    assert_eq!(by_path.first().map(String::as_str), Some(mdl::RIGGED));

    h.state_mut().sort = (SortKey::Path, false);
    h.run_steps(2);
    assert_eq!(paths(&h), by_path.iter().rev().cloned().collect::<Vec<_>>());

    h.state_mut().sort = (SortKey::Name, true);
    h.run_steps(2);
    let names: Vec<String> = paths(&h).iter().map(|p| p.rsplit('/').next().unwrap().to_ascii_lowercase()).collect();
    assert!(names.is_sorted(), "{names:?}");

    // The Path column's heading does the same.
    h.state_mut().sort = (SortKey::Package, true);
    h.run_steps(2);
    h.get_by_label("Path").click();
    h.run_steps(2);
    assert_eq!((h.state().sort, paths(&h)), ((SortKey::Path, true), by_path));
}

/// The tree's lines as text: folders end in `/`.
fn tree_lines(h: &Harness<'static, App>) -> Vec<String> {
    let app = h.state();
    app.tree_rows()
        .iter()
        .map(|row| match *row {
            Row::Folder { folder, depth } => format!("{}{}/", "  ".repeat(depth), app.tree().folders[folder].name),
            Row::File { file, depth } => format!("{}{}", "  ".repeat(depth), app.session.files()[file].path.rsplit('/').next().unwrap()),
        })
        .collect()
}

#[test]
fn the_tree_view_opens_folders_and_follows_the_filter() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    h.get_by_label("Tree").click();
    h.run_steps(2);
    assert_eq!(h.state().view_mode, ViewMode::Tree);
    // Only the packages at first, each saying how much is inside.
    assert_eq!(tree_lines(&h), ["LightMap/", "Misc/", "Texture/"]);
    h.get_by_label("13 files");
    assert!(h.query_by_label("Tone.wav").is_none());

    // Clicking a folder opens it; clicking a file selects it.
    h.get_by_label("Misc").click();
    h.run_steps(2);
    assert_eq!(tree_lines(&h), ["LightMap/", "Misc/", "  Lotus/", "Texture/"]);
    h.get_by_label("Lotus").click();
    h.run_steps(2);
    h.get_by_label("Sounds").click();
    h.run_steps(2);
    let lines = tree_lines(&h);
    let sounds = lines.iter().position(|l| l == "    Sounds/").unwrap();
    assert_eq!(&lines[sounds + 1..sounds + 6], ["      Compressed.wav", "      Old.wav", "      Short.wav", "      SplitStereo.wav", "      Tone.wav"]);
    h.get_by_label("Tone.wav").click();
    h.run_steps(2);
    let selected = h.state().selected.expect("the file is selected");
    assert_eq!(h.state().session.files()[selected].path, snd::TONE);
    wait_for(&mut h, "PCM, mono, 8000 Hz, 0:00.5");

    // Clicking the folder again closes it.
    h.get_by_label("Sounds").click();
    h.run_steps(2);
    assert!(!tree_lines(&h).contains(&"      Tone.wav".to_string()));

    // A filter shows its results unfolded.
    h.state_mut().filter.text = "crate".into();
    h.run_steps(2);
    assert_eq!(tree_lines(&h), ["Misc/", "  Lotus/", "    Objects/", "      Crate/", "        Crate.fbx", "        CrateBroken.fbx", "        CrateCollision.fbx"]);
    assert_eq!(h.query_all_by_label("3 files").count(), 4, "each folder on the way says how much it holds");

    // Without it everything folds again, except down to the selected file.
    h.state_mut().filter.text.clear();
    h.run_steps(2);
    let lines = tree_lines(&h);
    assert!(lines.contains(&"      Tone.wav".to_string()) && !lines.contains(&"      Crate/".to_string()), "{lines:?}");

    // Selecting a file from elsewhere unfolds the way to it.
    select(&mut h, mdl::STATIC);
    assert!(tree_lines(&h).contains(&"        Crate.fbx".to_string()));

    h.state_mut().set_all_folders(true);
    h.run_steps(2);
    assert_eq!(tree_lines(&h).iter().filter(|l| !l.ends_with('/')).count(), common::FILES);
    h.get_by_label("List").click();
    h.run_steps(2);
    assert_eq!(h.state().view_mode, ViewMode::List);
    h.get_by_label(tex::SPLIT);
}

/// A file's row in the list (the details pane shows the selected file's path as well,
/// and is drawn first).
fn row<'a>(h: &'a Harness<'static, App>, path: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(path).last().unwrap_or_else(|| panic!("no row for {path}"))
}

/// The paths of the selected files, sorted.
fn selected_paths(h: &Harness<'static, App>) -> Vec<String> {
    let app = h.state();
    let mut paths: Vec<String> = app.selection.iter().map(|&i| app.session.files()[i].path.clone()).collect();
    paths.sort();
    paths
}

#[test]
fn ctrl_and_shift_pick_several_files_and_they_export_together() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    h.state_mut().filter.text = "Sounds".into();
    h.run_steps(2);

    // A plain click picks one file; Ctrl+click adds another, and again takes it out.
    row(&h, snd::TONE).click();
    h.run_steps(2);
    row(&h, snd::SPLIT).click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(selected_paths(&h), [snd::SPLIT, snd::TONE]);
    h.get_by_label("2 selected");
    h.get_by_label("Export 2 selected…");
    row(&h, snd::SPLIT).click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(selected_paths(&h), [snd::TONE]);

    // Shift+click takes every row between the last file clicked (with or without Ctrl)
    // and this one.
    let shown: Vec<String> = h.state().shown().iter().map(|&i| h.state().session.files()[i].path.clone()).collect();
    assert_eq!(shown, [snd::VORBIS, snd::ADPCM, snd::SHORT, snd::SPLIT, snd::TONE]);
    row(&h, snd::ADPCM).click_modifiers(egui::Modifiers::SHIFT);
    h.run_steps(2);
    assert_eq!(selected_paths(&h), [snd::ADPCM, snd::SHORT, snd::SPLIT]);
    row(&h, snd::TONE).click();
    h.run_steps(2);
    row(&h, snd::SHORT).click_modifiers(egui::Modifiers::SHIFT);
    h.run_steps(2);
    assert_eq!(selected_paths(&h), [snd::SHORT, snd::SPLIT, snd::TONE]);
    assert_eq!(h.state().session.files()[h.state().selected.unwrap()].path, snd::SHORT, "the details follow the click");

    // Ctrl+A takes everything shown, Escape nothing; a narrower filter drops what it hides.
    h.key_down_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(2);
    assert_eq!(h.state().selection.len(), shown.len());
    h.state_mut().filter.text = "Tone".into();
    h.run_steps(2);
    assert_eq!(selected_paths(&h), [snd::TONE]);
    h.state_mut().filter.text = "Sounds".into();
    h.run_steps(2);
    h.key_down_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(2);
    h.key_down(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().selection.is_empty());

    // The selection exports on its own.
    row(&h, snd::TONE).click();
    h.run_steps(2);
    row(&h, snd::SPLIT).click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    let out = input.dir.path().join("picked");
    let picked = h.state().selection.clone();
    h.state_mut().start_export(picked, out.clone());
    h.run_steps(1);
    settle(&mut h);
    let (stats, _) = h.state().session.last_export.clone().unwrap();
    assert_eq!((stats.selected, stats.ok), (2, 2));
    assert!(out.join("Lotus").join("Sounds").join("Tone.wav").is_file());
    assert!(!out.join("Lotus").join("Sounds").join("Short.wav").exists());
}

#[test]
fn a_folder_of_the_tree_can_be_picked_or_exported() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    h.get_by_label("Tree").click();
    h.run_steps(2);
    h.get_by_label("Misc").click();
    h.run_steps(2);
    h.get_by_label("Lotus").click();
    h.run_steps(2);

    // Right-click a folder, even a closed one, to export all of it.
    h.get_by_label("Sounds").click_secondary();
    h.run_steps(2);
    h.get_by_label("Export this folder (5 files)…");
    h.get_by_label("Select its files").click();
    h.run_steps(2);
    let sounds: Vec<String> = selected_paths(&h);
    assert_eq!(sounds.len(), 5);
    assert!(sounds.iter().all(|p| p.starts_with("/Lotus/Sounds/")), "{sounds:?}");
    assert!(!tree_lines(&h).contains(&"      Tone.wav".to_string()), "picking a folder doesn't open it");

    // Ctrl+click on a folder whose files are all picked puts them back; plain click opens it.
    h.get_by_label("Sounds").click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert!(h.state().selection.is_empty());
    h.get_by_label("Sounds").click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().selection.len(), 5);
    h.get_by_label("Objects").click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().selection.len(), 5 + 3);

    // A file's own menu offers the whole selection.
    // (Ctrl+click on Tone.wav, already picked, takes it out.)
    h.get_by_label("Sounds").click();
    h.run_steps(2);
    h.get_by_label("Tone.wav").click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    h.get_by_label("Short.wav").click_secondary();
    h.run_steps(2);
    h.get_by_label("Export the 7 selected files…");
    h.get_by_label("Export this file…");
}

#[test]
fn hex_and_text_tabs_show_a_files_parts() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    select(&mut h, snd::TONE);
    wait_for(&mut h, "PCM, mono, 8000 Hz, 0:00.5");
    h.get_by_label("Play");

    // Hex: the H part first, which starts with the common header's hash.
    h.get_by_label("Hex").click();
    h.run_steps(2);
    assert_eq!(h.state().tab, Tab::Hex);
    assert!(h.query_by_label("Play").is_none());
    assert!(h.query_all_by_label_contains("000000  ").next().is_some(), "hex lines are shown");
    h.get_by_label_contains("B (7.8 KiB)").click();
    h.run_steps(2);
    assert_eq!(h.state().raw_part, Part::B);

    // The part on screen, or all of them, can be saved whole.
    h.get_by_label("Save B…");
    h.get_by_label("Save all as one…");
    let file = h.state().session.files().iter().position(|f| f.path == snd::TONE).unwrap();
    let entry = h.state().session.files()[file].clone();
    let (h_size, b_size) = (entry.part(Part::H).unwrap().size as usize, entry.part(Part::B).unwrap().size as usize);
    let dir = input.dir.path().join("saved");
    std::fs::create_dir_all(&dir).unwrap();
    for (parts, name) in [(vec![Part::B], "Tone.wav.B"), (vec![Part::H], "Tone.wav.H"), (vec![Part::H, Part::B, Part::F], "Tone.wav.HBF")] {
        h.state_mut().start_save_parts(file, parts, dir.join(name));
        h.run_steps(1);
        settle(&mut h);
    }
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
    assert_eq!(read("Tone.wav.B").len(), b_size);
    assert_eq!(&read("Tone.wav.B")[..5], &[0xee; 5]);
    assert_eq!(read("Tone.wav.H").len(), h_size);
    assert_eq!(read("Tone.wav.HBF"), [read("Tone.wav.H"), read("Tone.wav.B")].concat(), "the file has no F part: H then B");
    let log = &h.state().session.log.last().unwrap().text;
    assert!(log.contains("Tone.wav.HBF") && log.contains(&format!("B 7.8 KiB at byte {h_size}")), "{log}");

    // The fixture puts five 0xee bytes before the samples.
    assert!(h.query_all_by_label_contains("000000  ee ee ee ee ee ").next().is_some());

    // Text: the header's attribute text is in the H part.
    h.get_by_label("Text").click();
    h.run_steps(2);
    h.state_mut().raw_part = Part::H;
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("CompressionHint=DEFAULT_COMPRESSION").next().is_some());

    // The choice of tab stays for the next file; back to the preview for the player.
    select(&mut h, snd::SPLIT);
    wait_for(&mut h, "PCM, stereo, 8000 Hz, 0:00.3");
    assert_eq!(h.state().tab, Tab::Text);
    h.get_by_label("Preview").click();
    h.run_steps(2);
    h.get_by_label("Play");

    // A sound that can't be played says why, and its bytes are still a tab away.
    select(&mut h, snd::ADPCM);
    wait_for(&mut h, "ADPCM sounds have no reader yet");
    h.get_by_label("Hex").click();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("000000  ").next().is_some());
}

#[test]
fn export_marks_the_rows() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    let files = h.state().shown().iter().copied().collect();
    let out = input.dir.path().join("out");
    h.state_mut().start_export(files, out.clone());
    h.run_steps(1);
    settle(&mut h);
    let (stats, _) = h.state().session.last_export.clone().unwrap();
    // Textures, models and the three PCM sounds; the Vorbis one has no tables here.
    assert_eq!((stats.selected, stats.ok), (common::FILES as u64, 11 + 3 + 3));
    assert!(out.join("Lotus").join("Sounds").join("Tone.wav").is_file());
    assert!(out.join("Lotus").join("Objects").join("Crate").join("Crate.glb").is_file());
    assert!(out.join("Lotus").join("Textures").join("Split_d.dds").is_file());

    h.state_mut().filter.status = Some(Status::Failed);
    h.run_steps(2);
    h.get_by_label(&format!("3 of {} files", common::FILES));
    h.get_by_label(mdl::SHORT);

    // The coverage window sums it up.
    h.state_mut().show_coverage = true;
    h.run_steps(2);
    h.get_by_label(&format!("{0} of {0} files are of a kind wfscan knows.", common::FILES));
    h.get_by_label("Failures, most common first");
}

#[test]
fn unsupported_files_can_go_out_as_raw_parts() {
    let input = common::input();
    let mut h = harness();
    open(&mut h, &input);
    // The menu (the table's column is called Export too, and comes later).
    h.query_all_by_label("Export").next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Unsupported files as raw parts").click();
    h.run_steps(2);
    assert!(h.state().raw_unsupported);

    let file = h.state().session.files().iter().position(|f| f.path == mdl::OTHER_TYPE).unwrap();
    let out = input.dir.path().join("raw");
    h.state_mut().start_export([file].into(), out.clone());
    h.run_steps(1);
    settle(&mut h);
    let (stats, _) = h.state().session.last_export.clone().unwrap();
    assert_eq!((stats.unsupported, stats.raw), (1, 1));
    assert!(out.join("Lotus").join("Fx").join("Web").join("WebPatchMesh.H").is_file());
    assert!(h.state().session.log.last().unwrap().text.contains("1 unsupported (1 written raw)"));
}

#[test]
fn the_bink_window_says_where_videos_get_their_library() {
    let input = common::input();
    let mut h = harness();
    // Before a cache is open, it says one will be looked for.
    h.state_mut().show_bink = true;
    h.run_steps(2);
    if std::env::var_os("WFSCAN_BINK_DLL").is_none() {
        h.get_by_label_contains("open a cache to find the game's own");
    }
    // A fixture cache has no game folder around it.
    open(&mut h, &input);
    h.run_steps(2);
    if std::env::var_os("WFSCAN_BINK_DLL").is_none() {
        h.get_by_label_contains("wasn't found next to this cache");
        assert!(h.state().session.bink.is_none());
    }
    // A wrong path is reported, in the window and the log.
    h.state_mut().bink_dll_text = "no/such/bink2w64.dll".into();
    h.run_steps(1);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("not found").next().is_some());
    assert!(h.state().session.log.last().unwrap().text.starts_with("Bink: "));
}

#[test]
fn the_oodle_window_reports_a_missing_library() {
    let mut h = harness();
    h.state_mut().show_oodle = true;
    h.run_steps(2);
    h.state_mut().oodle_dll_text = "no/such/oo2core_9_win64.dll".into();
    h.run_steps(1);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("not found").next().is_some(), "the window shows the error");
    assert!(h.state().session.log.last().unwrap().text.contains("not found"));
}
