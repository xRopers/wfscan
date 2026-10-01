//! The wfscan window: the files as a list or as a tree of folders, the selected file's
//! details with its preview (a picture, a model to turn, a sound to play) or its bytes as
//! hex or text, and the Oodle, coverage and log windows. State lives in [`Session`]; this file only draws it
//! and turns clicks into jobs.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{Align, Color32, ColorImage, Key, Layout, RichText, Sense, Ui, ViewportCommand};
use egui_extras::{Column, TableBuilder};
use wfscan_core::formats::model::render::render;
use wfscan_core::{AudioOutput, FormatOptions, Kind, Part, Report, Status, TextureOutput, report};

use crate::jobs::{Jobs, finish};
use crate::player::Player;
use crate::preview::{self, Content, Previewer, clock};
use crate::session::{self, Filter, Level, Session};
use crate::settings::Settings;
use crate::tree::{self, OpenFolders, Row, Tree};
use crate::widgets::{hex_view, human_size, kind_color, status_color, text_view, waveform};

/// What the table is ordered by. Files that compare equal keep the manifest's order
/// (package, then path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Package,
    /// The whole path, whatever the package, ignoring case.
    Path,
    /// The file's own name, ignoring case.
    Name,
    Kind,
    Type,
    Size,
    Status,
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [SortKey::Package, SortKey::Path, SortKey::Name, SortKey::Kind, SortKey::Type, SortKey::Size, SortKey::Status];

    pub fn name(self) -> &'static str {
        match self {
            SortKey::Package => "package, then path",
            SortKey::Path => "path",
            SortKey::Name => "file name",
            SortKey::Kind => "kind",
            SortKey::Type => "type",
            SortKey::Size => "size",
            SortKey::Status => "export status",
        }
    }
}

/// `a` against `b` ignoring ASCII case, without allocating.
fn compare_ignoring_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// How the files are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// One row per file, sortable.
    List,
    /// Packages and folders that open and close.
    Tree,
}

/// What the details pane shows of the selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The picture, model or sound, for kinds that have a viewer.
    Preview,
    Hex,
    Text,
}

/// How far each level of the tree is indented.
const INDENT: f32 = 16.0;

/// What a click in the sound preview asks for.
enum SoundAction {
    /// Play from this point of the sound, 0 to 1.
    Play(f32),
    Stop,
}

pub struct App {
    pub session: Session,
    pub jobs: Jobs,
    /// The context of the last frame drawn (for windows, repaints and viewport commands).
    ctx: egui::Context,
    preview: Previewer,
    player: Player,
    /// The file whose sound is playing.
    playing: Option<usize>,
    /// The selected file, as a position in the session's file list.
    pub selected: Option<usize>,
    scroll_to_selected: bool,
    pub filter: Filter,
    /// The sort column, and whether it ascends.
    pub sort: (SortKey, bool),
    pub view_mode: ViewMode,
    /// The tree of the files shown, what it was built from, which of its folders are
    /// open, and the lines that makes.
    tree: Tree,
    tree_key: Option<(Filter, u64)>,
    pub open: OpenFolders,
    rows: Vec<Row>,
    rows_stale: bool,
    pub tab: Tab,
    /// The part the hex and text tabs show.
    pub raw_part: Part,
    /// The files shown, and what they were worked out from.
    view: Vec<usize>,
    view_key: Option<(Filter, (SortKey, bool), u64)>,
    pub texture_format: TextureOutput,
    pub audio_format: AudioOutput,
    pub show_oodle: bool,
    pub show_coverage: bool,
    show_log: bool,
    /// The coverage report, and the session generation it was made for.
    coverage: Option<(u64, Report)>,
    title: String,
    settings: Settings,
    /// Where settings are saved; `None` (as in tests) keeps them for this run only.
    settings_path: Option<PathBuf>,
    /// The Oodle library path being edited in the Oodle window.
    pub oodle_dll_text: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            session: Session::default(),
            jobs: Jobs::default(),
            ctx: egui::Context::default(),
            preview: Previewer::default(),
            player: Player::default(),
            playing: None,
            selected: None,
            scroll_to_selected: false,
            filter: Filter::default(),
            sort: (SortKey::Package, true),
            view_mode: ViewMode::List,
            tree: Tree::default(),
            tree_key: None,
            open: OpenFolders::default(),
            rows: Vec::new(),
            rows_stale: false,
            tab: Tab::Preview,
            raw_part: Part::H,
            view: Vec::new(),
            view_key: None,
            texture_format: TextureOutput::Dds,
            audio_format: AudioOutput::Native,
            show_oodle: false,
            show_coverage: false,
            show_log: false,
            coverage: None,
            title: String::new(),
            settings: Settings::default(),
            settings_path: None,
            oodle_dll_text: String::new(),
        }
    }

    /// Use the settings saved at `path` (and save changes there). An `oodle_dll` given
    /// here, from the command line, wins over the saved one for this run.
    pub fn load_settings(&mut self, path: Option<PathBuf>, oodle_dll: Option<PathBuf>) {
        self.settings = path.as_deref().map(Settings::load).unwrap_or_default();
        self.settings_path = path;
        let dll = oodle_dll.or_else(|| self.settings.oodle_dll.clone());
        self.oodle_dll_text = dll.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        self.session.set_oodle(dll.as_deref());
    }

    fn save_settings(&mut self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.session.error(format!("Saving settings: {e:#}"));
        }
    }

    /// Use the Oodle library typed in the Oodle window (empty: `WFSCAN_OODLE_DLL`), and
    /// remember it. A cache that was opened without one is opened again.
    pub fn apply_oodle_dll(&mut self) {
        let text = self.oodle_dll_text.trim();
        let dll = (!text.is_empty()).then(|| PathBuf::from(text));
        self.session.set_oodle(dll.as_deref());
        match self.session.oodle_error.clone() {
            None => self.session.info(match &dll {
                Some(p) => format!("Oodle: using {}", p.display()),
                None => "Oodle: using WFSCAN_OODLE_DLL".to_string(),
            }),
            Some(e) => self.session.error(format!("Oodle: {e}")),
        }
        self.settings.oodle_dll = dll;
        self.save_settings();
        let reopen = self.session.loaded.as_ref().filter(|l| l.stats.need_oodle > 0).map(|l| l.cache.dir.clone());
        if let (Some(dir), true) = (reopen, self.session.oodle.is_some()) {
            self.open_cache(dir);
        }
    }

    // ----- actions -------------------------------------------------------------------

    pub fn open_cache(&mut self, dir: PathBuf) {
        self.stop();
        self.selected = None;
        self.preview.clear();
        self.settings.cache_dir = Some(dir.clone());
        self.save_settings();
        let oodle = self.session.oodle.clone();
        self.jobs.start("Opening and indexing", move || finish("Open", session::open_cache(&dir, oodle), Session::set_loaded));
    }

    fn pick_and_open(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose the game's Cache.Windows folder");
        if let Some(dir) = &self.settings.cache_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_cache(dir);
        }
    }

    pub fn close(&mut self) {
        self.stop();
        self.session.close();
        self.selected = None;
        self.preview.clear();
    }

    /// Export the files at these positions of the file list into `dir`.
    pub fn start_export(&mut self, files: HashSet<usize>, dir: PathBuf) {
        let Some(loaded) = &self.session.loaded else { return };
        let (cache, manifest) = (loaded.cache.clone(), loaded.manifest.clone());
        let oodle = self.session.oodle.clone();
        let formats = FormatOptions { texture: self.texture_format, audio: self.audio_format, vorbis: None };
        self.settings.export_dir = Some(dir.clone());
        self.save_settings();
        let label = format!("Exporting {} files", files.len());
        self.jobs.start(label, move || {
            let result = session::run_export(&cache, manifest, files, &dir, &formats, oodle);
            finish("Export", result, move |s, (manifest, stats)| s.set_exported(manifest, stats, dir))
        });
    }

    fn pick_and_export(&mut self, files: HashSet<usize>) {
        let mut dialog = rfd::FileDialog::new().set_title("Export into this folder");
        if let Some(dir) = &self.settings.export_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.start_export(files, dir);
        }
    }

    pub fn select(&mut self, file: usize) {
        self.selected = Some(file);
        self.scroll_to_selected = true;
    }

    /// Play the selected file's sound from `fraction` (0 to 1) of the way through.
    pub fn play(&mut self, fraction: f32) {
        let (Some(file), Some(Ok(loaded))) = (self.selected, &self.preview.current) else { return };
        let Some(Ok(Content::Audio { pcm, .. })) = &loaded.content else { return };
        let from = (f64::from(fraction.clamp(0.0, 1.0)) * pcm.frames() as f64) as u64;
        match self.player.play(pcm.clone(), from) {
            Ok(()) => self.playing = Some(file),
            Err(e) => self.session.error(format!("Play: {e}")),
        }
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.playing = None;
    }

    pub fn is_playing(&self) -> bool {
        self.playing.is_some() && self.player.playing()
    }

    /// The files the table shows, in order.
    pub fn shown(&self) -> &[usize] {
        &self.view
    }

    /// The lines of the tree view.
    pub fn tree_rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Open or close one folder of the tree.
    pub fn toggle_folder(&mut self, folder: usize) {
        self.open.toggle(&self.tree.folders[folder]);
        self.rows_stale = true;
    }

    pub fn set_all_folders(&mut self, open: bool) {
        self.open.set_all(open);
        self.rows_stale = true;
    }

    /// Keep the tree and its lines in step with the files shown and the open folders.
    fn refresh_tree(&mut self) {
        if self.view_mode != ViewMode::Tree {
            return;
        }
        let key = (self.filter.clone(), self.session.generation);
        if self.tree_key.as_ref() != Some(&key) {
            self.tree = Tree::build(self.session.files(), &self.view);
            // A search is no use with its results folded away.
            let filtered = self.filter != Filter::default();
            if filtered != self.open.by_default {
                self.open.set_all(filtered);
            }
            self.tree_key = Some(key);
            self.rows_stale = true;
            self.scroll_to_selected = true;
        }
        if let (true, Some(file)) = (self.scroll_to_selected, self.selected) {
            for folder in self.tree.ancestors(file) {
                self.open.set(&self.tree.folders[folder], true);
            }
            self.rows_stale = true;
        }
        if self.rows_stale {
            self.rows = self.tree.rows(|folder| self.open.is_open(folder));
            self.rows_stale = false;
        }
    }

    fn refresh_view(&mut self) {
        let key = (self.filter.clone(), self.sort, self.session.generation);
        if self.view_key.as_ref() == Some(&key) {
            return;
        }
        let mut view = self.session.view(&self.filter);
        if let Some(loaded) = &self.session.loaded {
            let files = &loaded.manifest.files;
            let (sort, ascending) = self.sort;
            let type_of = |i: usize| files[i].header.as_ref().map(|h| h.type_id);
            match sort {
                SortKey::Package => {}
                SortKey::Path => view.sort_by(|&a, &b| compare_ignoring_case(&files[a].path, &files[b].path)),
                SortKey::Name => view.sort_by(|&a, &b| compare_ignoring_case(tree::file_name(&files[a].path), tree::file_name(&files[b].path))),
                SortKey::Kind => view.sort_by_key(|&i| (loaded.kinds[i].is_none(), loaded.kinds[i], type_of(i))),
                SortKey::Type => view.sort_by_key(|&i| (loaded.categories[i], type_of(i))),
                SortKey::Size => view.sort_by_key(|&i| files[i].size()),
                SortKey::Status => view.sort_by_key(|&i| files[i].export.as_ref().map(|e| e.status as u8)),
            }
            if !ascending {
                view.reverse();
            }
        }
        self.view = view;
        self.view_key = Some(key);
    }

    // ----- drawing -------------------------------------------------------------------

    /// Draw one frame.
    pub fn show(&mut self, ui: &mut Ui) {
        if self.ctx != *ui.ctx() {
            self.ctx = ui.ctx().clone();
            let waker = self.ctx.clone();
            self.jobs.set_waker(move || waker.request_repaint());
        }
        self.preview.poll();
        if self.jobs.poll(&mut self.session) {
            self.preview.clear();
        }
        if self.selected.is_some_and(|i| i >= self.session.files().len()) {
            self.selected = None;
        }
        // A sound stops when it ends or another file is selected.
        if self.playing.is_some() && (!self.player.playing() || self.playing != self.selected) {
            self.stop();
        }
        self.refresh_view();
        self.refresh_tree();
        self.handle_input(ui);
        self.update_title();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| self.menu_bar(ui));
            ui.add_space(2.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.selected.is_some() {
            egui::Panel::right("details").resizable(true).default_size(520.0).min_size(340.0).show(ui, |ui| self.details(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        self.oodle_window();
        self.coverage_window();
        self.log_window();
        if self.jobs.busy() || self.preview.loading() {
            // Keep the elapsed time moving.
            self.ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.is_playing() {
            // And the play position.
            self.ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn handle_input(&mut self, ui: &Ui) {
        let ctx = ui.ctx().clone();
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| p.is_dir()));
        if let Some(dir) = dropped
            && !self.jobs.busy()
        {
            self.open_cache(dir);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::O)) && !self.jobs.busy() {
            self.pick_and_open();
        }
        // Space plays and stops the selected sound when nothing else has focus.
        if ctx.memory(|m| m.focused().is_none()) && ctx.input(|i| i.key_pressed(Key::Space)) {
            if self.is_playing() {
                self.stop();
            } else {
                self.play(0.0);
            }
        }
        // Arrow keys move through the files on screen when nothing else has focus.
        let step = ctx.input(|i| i.key_pressed(Key::ArrowDown) as i64 - i.key_pressed(Key::ArrowUp) as i64);
        if step != 0 && ctx.memory(|m| m.focused().is_none()) {
            let files: Vec<usize> = match self.view_mode {
                ViewMode::List => self.view.clone(),
                ViewMode::Tree => self.rows.iter().filter_map(|row| if let Row::File { file, .. } = row { Some(*file) } else { None }).collect(),
            };
            if !files.is_empty() {
                let position = self.selected.and_then(|file| files.iter().position(|&i| i == file));
                let next = position.map_or(0, |p| (p as i64 + step).clamp(0, files.len() as i64 - 1) as usize);
                self.select(files[next]);
            }
        }
    }

    fn update_title(&mut self) {
        let title = match &self.session.loaded {
            Some(loaded) => format!("{} - wfscan", loaded.cache.dir.display()),
            None => "wfscan".to_string(),
        };
        if title != self.title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        let open = self.session.loaded.is_some();
        ui.menu_button("File", |ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open cache folder…").shortcut_text("Ctrl+O")).clicked() {
                ui.close();
                self.pick_and_open();
            }
            if ui.add_enabled(open && !busy, egui::Button::new("Close")).clicked() {
                ui.close();
                self.close();
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                self.ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        });
        ui.menu_button("Export", |ui| {
            let shown = self.view.len();
            if ui.add_enabled(open && !busy && shown > 0, egui::Button::new(format!("Export the {shown} files shown…"))).clicked() {
                ui.close();
                self.pick_and_export(self.view.iter().copied().collect());
            }
            if ui.add_enabled(self.selected.is_some() && !busy, egui::Button::new("Export the selected file…")).clicked() {
                ui.close();
                self.pick_and_export(self.selected.into_iter().collect());
            }
            ui.separator();
            ui.label("Textures as");
            ui.radio_value(&mut self.texture_format, TextureOutput::Dds, "DDS (exact, every mip)");
            ui.radio_value(&mut self.texture_format, TextureOutput::Png, "PNG (top mip, 8-bit)");
            ui.separator();
            ui.label("Sounds as");
            ui.radio_value(&mut self.audio_format, AudioOutput::Native, "Ogg Vorbis, Opus or WAV (exact)");
            ui.radio_value(&mut self.audio_format, AudioOutput::Wav, "WAV (decoded, larger)");
        });
        ui.menu_button("Tools", |ui| {
            if ui.button("Oodle library…").clicked() {
                ui.close();
                self.show_oodle = true;
            }
            if ui.add_enabled(open, egui::Button::new("Coverage…")).on_hover_text("How many files of each type there are, and how their exports ended").clicked() {
                ui.close();
                self.show_coverage = true;
            }
            if ui.button("Log…").clicked() {
                ui.close();
                self.show_log = true;
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open cache folder…")).clicked() {
                self.pick_and_open();
            }
            if self.session.loaded.is_none() {
                return;
            }
            ui.separator();
            let mode = self.view_mode;
            ui.selectable_value(&mut self.view_mode, ViewMode::List, "List").on_hover_text("One row per file, sortable");
            ui.selectable_value(&mut self.view_mode, ViewMode::Tree, "Tree").on_hover_text("Packages and folders");
            if self.view_mode != mode {
                self.scroll_to_selected = true;
            }
            if self.view_mode == ViewMode::Tree {
                if ui.small_button("⊞").on_hover_text("Open every folder").clicked() {
                    self.set_all_folders(true);
                }
                if ui.small_button("⊟").on_hover_text("Close every folder").clicked() {
                    self.set_all_folders(false);
                }
            }
            ui.separator();
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.filter.text).hint_text("part of the path").desired_width(260.0));
            if !self.filter.text.is_empty() && ui.small_button("×").on_hover_text("Clear the filter").clicked() {
                self.filter.text.clear();
            }
            let kind_name = self.filter.kind.map_or("any kind", Kind::name);
            egui::ComboBox::from_id_salt("kind").selected_text(kind_name).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter.kind, None, "any kind");
                for kind in Kind::ALL {
                    ui.selectable_value(&mut self.filter.kind, Some(kind), kind.name());
                }
            });
            let status_name = self.filter.status.map_or("any status", Status::name);
            egui::ComboBox::from_id_salt("status").selected_text(status_name).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter.status, None, "any status");
                for status in Status::ALL {
                    ui.selectable_value(&mut self.filter.status, Some(status), status.name());
                }
            });
            ui.checkbox(&mut self.filter.known_only, "known kinds only").on_hover_text("Hide files of types wfscan has no name for");
            ui.separator();
            // The tree has its own order: folders, then files, by name.
            ui.add_enabled_ui(self.view_mode == ViewMode::List, |ui| {
                ui.label("Sort by");
                egui::ComboBox::from_id_salt("sort").selected_text(self.sort.0.name()).show_ui(ui, |ui| {
                    for key in SortKey::ALL {
                        if ui.selectable_label(self.sort.0 == key, key.name()).clicked() {
                            self.sort = (key, true);
                        }
                    }
                });
                let (arrow, hint) = if self.sort.1 { ("⏶", "Ascending: click for descending") } else { ("⏷", "Descending: click for ascending") };
                if ui.button(arrow).on_hover_text(hint).clicked() {
                    self.sort.1 = !self.sort.1;
                }
            });
            ui.separator();
            let shown = self.view.len();
            if ui.add_enabled(!busy && shown > 0, egui::Button::new(format!("Export {shown} shown…"))).clicked() {
                self.pick_and_export(self.view.iter().copied().collect());
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = self.jobs.current() {
                ui.spinner();
                ui.label(format!("{}… {:.1} s", job.label, job.started.elapsed().as_secs_f64()));
            } else if let Some(line) = self.session.log.last() {
                let text = match line.level {
                    Level::Info => RichText::new(&line.text),
                    Level::Error => RichText::new(&line.text).color(ui.visuals().error_fg_color),
                };
                if ui.add(egui::Label::new(text).sense(Sense::click()).truncate()).on_hover_text("Show the log").clicked() {
                    self.show_log = true;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.session.loaded.is_some() {
                    let total = self.session.files().iter().filter(|f| !f.stale).count();
                    ui.label(format!("{} of {} files", self.view.len(), total));
                }
                match (&self.session.oodle, &self.session.oodle_error) {
                    (Some(_), _) => ui.weak("Oodle ready"),
                    (None, _) => ui.colored_label(ui.visuals().warn_fg_color, "no Oodle library"),
                };
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.session.loaded.is_none() {
            self.welcome(ui);
        } else if self.view_mode == ViewMode::Tree {
            self.tree_table(ui);
        } else {
            self.table(ui);
        }
    }

    /// The files by folder: a line per open folder and per file inside one.
    fn tree_table(&mut self, ui: &mut Ui) {
        ui.style_mut().interaction.selectable_labels = false;
        let Some(loaded) = &self.session.loaded else { return };
        let files = &loaded.manifest.files;
        let (tree, rows, open) = (&self.tree, &self.rows, &self.open);
        let mut clicked_file: Option<usize> = None;
        let mut clicked_folder: Option<usize> = None;

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(260.0).clip(true))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(40.0))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(80.0));
        if self.scroll_to_selected {
            if let Some(row) = self.selected.and_then(|file| rows.iter().position(|row| matches!(row, Row::File { file: f, .. } if *f == file))) {
                table = table.scroll_to_row(row, None);
            }
            self.scroll_to_selected = false;
        }
        table
            .header(22.0, |mut header| {
                for label in ["Name", "Kind", "Type", "Size", "Export"] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                let row_height = body.ui_mut().text_style_height(&egui::TextStyle::Body) + 4.0;
                body.rows(row_height, rows.len(), |mut row| match rows[row.index()] {
                    Row::Folder { folder, depth } => {
                        let folder_info = &tree.folders[folder];
                        row.col(|ui| {
                            ui.add_space(depth as f32 * INDENT);
                            ui.label(if open.is_open(folder_info) { "⏷" } else { "⏵" });
                            ui.strong(&folder_info.name);
                            ui.weak(if folder_info.total == 1 { "1 file".to_string() } else { format!("{} files", folder_info.total) });
                        });
                        for _ in 0..4 {
                            row.col(|_| {});
                        }
                        if row.response().clicked() {
                            clicked_folder = Some(folder);
                        }
                    }
                    Row::File { file: index, depth } => {
                        let file = &files[index];
                        row.set_selected(self.selected == Some(index));
                        row.col(|ui| {
                            ui.add_space(depth as f32 * INDENT + INDENT);
                            ui.add(egui::Label::new(tree::file_name(&file.path)).extend());
                        });
                        row.col(|ui| {
                            match loaded.kinds[index] {
                                Some(kind) => ui.colored_label(kind_color(kind), kind.name()),
                                None => ui.weak("–"),
                            };
                        });
                        row.col(|ui| {
                            match &file.header {
                                Some(header) => ui.label(header.type_id.to_string()),
                                None => ui.weak("–"),
                            };
                        });
                        row.col(|ui| {
                            ui.label(human_size(file.size()));
                        });
                        row.col(|ui| {
                            if file.error.is_some() {
                                ui.colored_label(ui.visuals().error_fg_color, "unreadable");
                            } else if let Some(export) = &file.export {
                                ui.colored_label(status_color(export.status, ui), export.status.name());
                            }
                        });
                        if row.response().clicked() {
                            clicked_file = Some(index);
                        }
                    }
                });
            });
        if let Some(index) = clicked_file {
            self.selected = Some(index);
        }
        if let Some(folder) = clicked_folder {
            self.toggle_folder(folder);
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.heading("wfscan");
            ui.label("Browse and export the files in Warframe's cache.");
            ui.add_space(12.0);
            if ui.add_enabled(!busy, egui::Button::new("Open a cache folder…")).clicked() {
                self.pick_and_open();
            }
            ui.weak("Choose the game's Cache.Windows folder, or drop it on this window.");
            ui.add_space(12.0);
            if self.session.oodle.is_none() {
                ui.colored_label(ui.visuals().warn_fg_color, "No Oodle library is set. Almost every file needs one to be read.");
                if ui.button("Set the Oodle library…").clicked() {
                    self.show_oodle = true;
                }
            }
        });
    }

    fn table(&mut self, ui: &mut Ui) {
        // Selectable labels would take clicks meant for the row.
        ui.style_mut().interaction.selectable_labels = false;
        let Some(loaded) = &self.session.loaded else { return };
        let files = &loaded.manifest.files;
        let view = &self.view;
        let mut clicked: Option<usize> = None;
        let mut sort = self.sort;

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(40.0))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::remainder().at_least(200.0).clip(true));
        if self.scroll_to_selected {
            if let Some(row) = self.selected.and_then(|file| view.iter().position(|&i| i == file)) {
                table = table.scroll_to_row(row, None);
            }
            self.scroll_to_selected = false;
        }
        let headers = [
            ("Package", SortKey::Package),
            ("Kind", SortKey::Kind),
            ("Type", SortKey::Type),
            ("Size", SortKey::Size),
            ("Export", SortKey::Status),
            ("Path", SortKey::Path),
        ];
        table
            .header(22.0, |mut header| {
                for (label, key) in headers {
                    header.col(|ui| {
                        let arrow = match sort {
                            (k, true) if k == key => " ⏶",
                            (k, false) if k == key => " ⏷",
                            _ => "",
                        };
                        if ui.add(egui::Button::new(RichText::new(format!("{label}{arrow}")).strong()).frame(false)).clicked() {
                            sort = if sort.0 == key { (key, !sort.1) } else { (key, true) };
                        }
                    });
                }
            })
            .body(|mut body| {
                let row_height = body.ui_mut().text_style_height(&egui::TextStyle::Body) + 4.0;
                body.rows(row_height, view.len(), |mut row| {
                    let index = view[row.index()];
                    let file = &files[index];
                    row.set_selected(self.selected == Some(index));
                    row.col(|ui| {
                        ui.label(&file.package);
                    });
                    row.col(|ui| {
                        match loaded.kinds[index] {
                            Some(kind) => ui.colored_label(kind_color(kind), kind.name()),
                            None => ui.weak("–"),
                        };
                    });
                    row.col(|ui| {
                        match &file.header {
                            Some(header) => ui.label(header.type_id.to_string()),
                            None => ui.weak("–"),
                        };
                    });
                    row.col(|ui| {
                        ui.label(human_size(file.size()));
                    });
                    row.col(|ui| {
                        if file.error.is_some() {
                            ui.colored_label(ui.visuals().error_fg_color, "unreadable");
                        } else if let Some(export) = &file.export {
                            ui.colored_label(status_color(export.status, ui), export.status.name());
                        }
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&file.path).extend());
                    });
                    if row.response().clicked() {
                        clicked = Some(index);
                    }
                });
            });
        self.sort = sort;
        if let Some(index) = clicked {
            self.selected = Some(index);
        }
    }

    fn details(&mut self, ui: &mut Ui) {
        let (Some(index), Some(loaded)) = (self.selected, &self.session.loaded) else { return };
        let file = &loaded.manifest.files[index];
        let kind = loaded.kinds[index];
        let name = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
        let mut export_this = false;
        let mut close = false;

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(&name);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.small_button("×").on_hover_text("Close the details").clicked();
            });
        });
        ui.add(egui::Label::new(RichText::new(&file.path).weak()).wrap());
        ui.add_space(4.0);
        egui::Grid::new("file-info").num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
            ui.label("Package");
            ui.label(format!("{} ({})", file.package, loaded.categories[index]));
            ui.end_row();
            ui.label("Kind");
            match (kind, &file.header) {
                (Some(kind), Some(header)) => ui.colored_label(kind_color(kind), format!("{kind}, type {}", header.type_id)),
                (None, Some(header)) => ui.label(format!("type {} (no reader)", header.type_id)),
                _ => ui.weak("no common header"),
            };
            ui.end_row();
            ui.label("Parts");
            let parts: Vec<String> = Part::ALL.iter().filter_map(|&p| Some(format!("{p} {}", human_size(u64::from(file.part(p)?.size))))).collect();
            ui.label(parts.join(", "));
            ui.end_row();
            if let Some(error) = &file.error {
                ui.label("Read");
                ui.add(egui::Label::new(RichText::new(error).color(ui.visuals().error_fg_color)).wrap());
                ui.end_row();
            }
            if let Some(export) = &file.export {
                ui.label("Export");
                let mut text = export.status.name().to_string();
                if let Some(reason) = &export.reason {
                    text += &format!(": {reason}");
                }
                if !export.outputs.is_empty() {
                    text += &format!(" ({})", export.outputs.join(", "));
                }
                ui.add(egui::Label::new(RichText::new(text).color(status_color(export.status, ui))).wrap());
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            export_this = ui.add_enabled(!self.jobs.busy(), egui::Button::new("Export this file…")).clicked();
        });
        ui.separator();

        let key = preview::Key { package: file.package.clone(), path: file.path.clone(), generation: self.session.generation };
        let ctx = ui.ctx().clone();
        self.preview.request(key.clone(), loaded.cache.clone(), self.session.oodle.clone(), kind, move || ctx.request_repaint());
        let mut sound = None;
        if self.preview.key.as_ref() != Some(&key) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading…");
            });
        } else {
            let position = if self.playing == Some(index) { self.player.position() } else { None };
            sound = preview_pane(ui, &mut self.preview, index, position, &mut self.tab, &mut self.raw_part);
        }
        match sound {
            Some(SoundAction::Play(from)) => self.play(from),
            Some(SoundAction::Stop) => self.stop(),
            None => {}
        }

        if close {
            self.selected = None;
        }
        if export_this {
            self.pick_and_export(HashSet::from([index]));
        }
    }

    fn oodle_window(&mut self) {
        let mut open = self.show_oodle;
        let mut apply = false;
        egui::Window::new("Oodle library").open(&mut open).resizable(false).show(&self.ctx.clone(), |ui| {
            ui.label("Warframe compresses its cache with Oodle, which wfscan doesn't include and the game doesn't ship as a file.");
            ui.label("Point at an oo2core_*.dll from another game or SDK:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.oodle_dll_text).hint_text("path to oo2core_9_win64.dll").desired_width(420.0));
                if ui.button("Browse…").clicked()
                    && let Some(path) = rfd::FileDialog::new().set_title("Choose an oo2core library").pick_file()
                {
                    self.oodle_dll_text = path.display().to_string();
                }
            });
            apply = ui.add_enabled(!self.jobs.busy(), egui::Button::new("Apply")).clicked();
            ui.separator();
            match &self.session.oodle_error {
                None => ui.label("Oodle is ready."),
                Some(error) => ui.add(egui::Label::new(RichText::new(error).color(ui.visuals().error_fg_color)).wrap()),
            };
        });
        self.show_oodle = open;
        if apply {
            self.apply_oodle_dll();
        }
    }

    fn coverage_window(&mut self) {
        if !self.show_coverage {
            return;
        }
        let Some(loaded) = &self.session.loaded else { return };
        if self.coverage.as_ref().is_none_or(|(generation, _)| *generation != self.session.generation) {
            self.coverage = Some((self.session.generation, report(&loaded.manifest)));
        }
        let Some((_, coverage)) = &self.coverage else { return };
        let mut open = self.show_coverage;
        egui::Window::new("Coverage").open(&mut open).default_size([760.0, 520.0]).show(&self.ctx.clone(), |ui| {
            let known: u64 = coverage.types.iter().filter(|t| t.kind.is_some()).map(|t| t.files).sum();
            let total: u64 = coverage.types.iter().map(|t| t.files).sum();
            ui.label(format!("{known} of {total} files are of a kind wfscan knows."));
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("coverage").striped(true).num_columns(8).spacing([14.0, 3.0]).show(ui, |ui| {
                    for heading in ["Category", "Type", "Kind", "Files", "Size", "ok", "failed", "unsupported"] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for row in &coverage.types {
                        ui.label(row.category.name());
                        ui.label(row.type_id.map_or_else(|| "–".to_string(), |t| t.to_string()));
                        match row.kind {
                            Some(kind) => ui.colored_label(kind_color(kind), kind.name()),
                            None => ui.weak(if row.type_id.is_some() { "?" } else { "(no header)" }),
                        };
                        ui.label(row.files.to_string());
                        ui.label(human_size(row.size));
                        ui.label(row.ok.to_string());
                        if row.failed > 0 {
                            ui.colored_label(ui.visuals().error_fg_color, row.failed.to_string());
                        } else {
                            ui.label("0");
                        }
                        ui.label(row.unsupported.to_string());
                        ui.end_row();
                    }
                });
                if !coverage.errors.is_empty() {
                    ui.separator();
                    ui.strong("Failures, most common first");
                    for error in coverage.errors.iter().take(40) {
                        let kind = error.kind.map_or("?", Kind::name);
                        ui.add(egui::Label::new(format!("{} × {} {kind}: {}  (e.g. {})", error.files, error.category.name(), error.message, error.sample)).wrap());
                    }
                }
            });
        });
        self.show_coverage = open;
    }

    fn log_window(&mut self) {
        let mut open = self.show_log;
        egui::Window::new("Log").open(&mut open).default_size([640.0, 320.0]).show(&self.ctx.clone(), |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                for line in &self.session.log {
                    let text = match line.level {
                        Level::Info => RichText::new(&line.text),
                        Level::Error => RichText::new(&line.text).color(ui.visuals().error_fg_color),
                    };
                    ui.add(egui::Label::new(text).wrap());
                }
            });
        });
        self.show_log = open;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// The loaded preview: what the file is, its attribute text, and under the tabs a
/// picture, a model to turn around or a sound to play, or one of the file's parts as hex
/// or text. `playing` is the frame the file's sound has reached, if it is playing.
fn preview_pane(ui: &mut Ui, preview: &mut Previewer, id: usize, playing: Option<u64>, tab: &mut Tab, raw_part: &mut Part) -> Option<SoundAction> {
    let Previewer { current, texture, drawn, view, .. } = preview;
    let loaded = match current {
        Some(Ok(loaded)) => loaded,
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            return None;
        }
        None => return None,
    };
    let mut action = None;
    ui.label(&loaded.summary);
    if !loaded.attributes.trim().is_empty() {
        egui::CollapsingHeader::new("Attributes").id_salt(("attributes", id)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("attributes-text", id)).max_height(160.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&loaded.attributes).monospace()).wrap());
            });
        });
    }
    ui.add_space(4.0);
    // A file with no viewer shows its bytes under Preview too.
    let shown = if loaded.content.is_none() && *tab == Tab::Preview { Tab::Hex } else { *tab };
    ui.horizontal(|ui| {
        for (value, label) in [(Tab::Preview, "Preview"), (Tab::Hex, "Hex"), (Tab::Text, "Text")] {
            ui.selectable_value(tab, value, label);
        }
        if shown != Tab::Preview {
            ui.separator();
            for raw in &loaded.raw {
                ui.selectable_value(raw_part, raw.part, format!("{} ({})", raw.part, human_size(raw.size as u64)));
            }
        }
    });
    let content = match (shown, &loaded.content) {
        (Tab::Preview, Some(content)) => content,
        _ => {
            if *tab == Tab::Preview {
                ui.weak("No viewer for this kind of file yet: these are its bytes.");
            }
            let raw = loaded.raw.iter().find(|raw| raw.part == *raw_part).or(loaded.raw.first())?;
            if raw.truncated() {
                ui.weak(format!("Showing the first {} of {}.", human_size(raw.bytes.len() as u64), human_size(raw.size as u64)));
            }
            match shown {
                Tab::Text => text_view(ui, ("text", id, raw.part), &raw.text),
                _ => hex_view(ui, ("hex", id, raw.part), &raw.bytes),
            }
            return None;
        }
    };
    match content {
        Err(e) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("can't show this file: {e}"));
        }
        Ok(Content::Texture(image)) => {
            let handle = texture.get_or_insert_with(|| ui.ctx().load_texture(format!("texture-{id}"), image.clone(), Default::default()));
            let size = handle.size_vec2();
            // Fit the pane; small textures are enlarged (up to 8×) with sharp pixels.
            let available = ui.available_size();
            let scale = (available.x / size.x).min(available.y / size.y).clamp(0.01, 8.0);
            ui.add(egui::Image::new(&*handle).texture_options(egui::TextureOptions::NEAREST).fit_to_exact_size(size * scale));
        }
        Ok(Content::Model(model)) => {
            ui.weak("Drag to turn, scroll to zoom. Shaded by shape only: no textures.");
            let side = ui.available_width().min(ui.available_height()).clamp(160.0, 720.0);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), Sense::drag());
            if response.dragged() {
                let delta = response.drag_delta();
                view.yaw += delta.x * 0.01;
                view.pitch = (view.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                view.zoom = (view.zoom * (scroll * 0.002).exp()).clamp(0.2, 20.0);
            }
            let pixels = [side as usize, side as usize];
            if drawn.as_ref() != Some(&(*view, pixels)) || texture.is_none() {
                let rgba = render(model, pixels[0] as u32, pixels[1] as u32, view);
                let image = ColorImage::from_rgba_unmultiplied(pixels, &rgba);
                *texture = Some(ui.ctx().load_texture(format!("model-{id}"), image, Default::default()));
                *drawn = Some((*view, pixels));
            }
            ui.painter().rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
            if let Some(handle) = texture {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(handle.id(), rect, uv, Color32::WHITE);
            }
        }
        Ok(Content::Audio { pcm, peaks }) => {
            let fraction = playing.map(|frame| frame as f32 / pcm.frames().max(1) as f32);
            if let Some(at) = waveform(ui, peaks, fraction, 120.0) {
                action = Some(SoundAction::Play(at));
            }
            ui.horizontal(|ui| {
                if playing.is_some() {
                    if ui.button("Stop").clicked() {
                        action = Some(SoundAction::Stop);
                    }
                } else if ui.button("Play").clicked() {
                    action = Some(SoundAction::Play(0.0));
                }
                let now = playing.map_or(0.0, |frame| frame as f64 / f64::from(pcm.sample_rate.max(1)));
                ui.monospace(format!("{} / {}", clock(now), clock(pcm.seconds())));
            });
            ui.weak("Click the waveform to play from there. Space plays and stops.");
        }
    }
    action
}
