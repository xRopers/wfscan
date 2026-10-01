// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use wfscan_gui::App;

fn main() -> eframe::Result {
    // `wfscan-gui [--oodle-dll PATH] [CACHE FOLDER]`: the folder opens straight away.
    let mut args = std::env::args_os().skip(1);
    let (mut cache, mut oodle_dll) = (None, None);
    while let Some(a) = args.next() {
        if a == "--oodle-dll" {
            oodle_dll = args.next().map(PathBuf::from);
        } else {
            cache = Some(PathBuf::from(a));
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("wfscan").with_inner_size([1360.0, 840.0]).with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "wfscan",
        options,
        Box::new(move |_cc| {
            let mut app = App::new();
            app.load_settings(wfscan_gui::settings::Settings::default_path(), oodle_dll);
            if let Some(dir) = cache {
                app.open_cache(dir);
            }
            Ok(Box::new(app))
        }),
    )
}
