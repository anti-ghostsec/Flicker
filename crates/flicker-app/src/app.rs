use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use flicker_core::media::{format_duration, format_timestamp};
use flicker_core::{
    project, Clip, ClipKind, ClipRef, Engine, ExportRequest, FfmpegCli, StreamKind, SubtitleExport, SubtitleFormat,
    Timeline, TrackKind, TrackRef,
};

use crate::common::{
    dropped_files, has_ext, pick_file, pick_media, save_file, save_project_file, JobRunner, AUDIO_EXTS, IMAGE_EXTS,
};
use crate::player::{last_line, Player};
use crate::project::Project;
use crate::settings::Settings;
use crate::theme::{self, Theme};
use crate::timeline::TimelineView;

const LANGUAGES: &[(&str, &str)] = &[
    ("", "(none)"),
    ("eng", "English"),
    ("ara", "Arabic"),
    ("spa", "Spanish"),
    ("fre", "French"),
    ("ger", "German"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("rus", "Russian"),
    ("tur", "Turkish"),
    ("hin", "Hindi"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("chi", "Chinese"),
];

const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / pause"),
    ("← / →", "One frame back / forward"),
    ("Shift+← / →", "5 seconds back / forward"),
    ("Home / End", "Start / end of the timeline"),
    ("S", "Split the selected clip (or every clip) at the playhead"),
    ("Del", "Delete the selected clip, leaving a gap"),
    ("Shift+Del", "Ripple delete: delete it and close the gap"),
    ("Esc", "Unselect"),
    ("+ / -", "Zoom the timeline in / out"),
    ("Ctrl+wheel", "Zoom the timeline around the pointer"),
    ("Wheel / Shift+wheel", "Scroll the timeline"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Ctrl+N / Ctrl+O", "New / open project"),
    ("Ctrl+S / Ctrl+Shift+S", "Save project / save project as"),
    ("Ctrl+I", "Add files to the timeline"),
    ("Ctrl+E", "Export video"),
];

/// How long to wait after a drop from the file manager for the pointer to
/// say where it was (X11 doesn't tell during a drag).
const DROP_WAIT: Duration = Duration::from_millis(400);

enum Job {
    Export { to: PathBuf },
}

/// Something waiting for the user to decide about unsaved changes.
#[derive(Clone, Debug)]
enum Pending {
    Quit,
    New,
    Open(PathBuf),
}

enum Dialog {
    Unsaved(Pending),
    Shortcuts,
    About,
    Error(String),
}

/// Files dropped from the file manager, waiting for the pointer position.
struct Drop {
    files: Vec<PathBuf>,
    since: Instant,
}

pub struct FlickerApp {
    engine: Arc<dyn Engine>,
    theme: Theme,
    /// "Write encoder name and dates into saved files".
    write_tags: bool,
    project: Project,
    player: Player,
    view: TimelineView,
    /// The project version the player shows.
    shown_version: Option<u64>,
    job: JobRunner<Job>,
    dialog: Option<Dialog>,
    /// Status-bar message: text and whether it's an error.
    status: Option<(String, bool)>,
    /// Set once the user agreed to quit with unsaved changes.
    allow_close: bool,
    title: String,
    drop: Option<Drop>,
    dev_screenshot: Option<dev::Screenshot>,
}

impl FlickerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let settings = Settings::load();
        let theme = settings.theme;
        theme.apply(&cc.egui_ctx);
        let engine: Arc<dyn Engine> = Arc::new(FfmpegCli::locate());
        let mut app = Self {
            player: Player::new(engine.clone()),
            engine,
            theme,
            write_tags: settings.write_tags,
            project: Project::default(),
            view: TimelineView::default(),
            shown_version: None,
            job: JobRunner::default(),
            dialog: None,
            status: None,
            allow_close: false,
            title: String::new(),
            drop: None,
            dev_screenshot: dev::Screenshot::from_env(),
        };
        let (projects, media): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(|p| project::is_project(p));
        if let Some(p) = projects.into_iter().next() {
            app.open_project(p);
        }
        if !media.is_empty() {
            app.add_files(media, None);
            app.project = Project::new(app.project.timeline().clone(), None);
        }
        dev::apply(&mut app, &cc.egui_ctx);
        app
    }

    // ---- Projects ----------------------------------------------------------

    /// Do `what`, first asking about unsaved changes if there are any.
    fn request(&mut self, what: Pending) {
        if self.project.is_dirty() {
            self.dialog = Some(Dialog::Unsaved(what));
        } else {
            self.proceed(what);
        }
    }

    fn proceed(&mut self, what: Pending) {
        match what {
            Pending::Open(path) => self.open_project(path),
            Pending::New => self.set_project(Project::default()),
            Pending::Quit => self.allow_close = true,
        }
    }

    fn set_project(&mut self, p: Project) {
        self.project = p;
        self.view = TimelineView::default();
        self.player.pause();
        self.player.seek(Duration::ZERO);
        self.status = None;
    }

    fn open_project(&mut self, path: PathBuf) {
        match project::load(&path) {
            Ok(t) => {
                let missing: Vec<String> = t.files().filter(|f| !f.is_file()).map(file_name).collect();
                self.set_project(Project::new(t, Some(path.clone())));
                self.view.zoom_fit(self.project.timeline().duration());
                if missing.is_empty() {
                    self.set_status(format!("Opened {}", file_name(&path)), false);
                } else {
                    self.set_status(format!("Missing: {}", missing.join(", ")), true);
                }
            }
            Err(e) => self.set_status(format!("Can't open {}: {e}", file_name(&path)), true),
        }
    }

    fn menu_open(&mut self) {
        if let Some(p) = pick_file("Flicker project", &[project::EXTENSION]) {
            self.request(Pending::Open(p));
        }
    }

    /// Save the project (asking where if it has no file yet, or `ask`).
    /// True when saved.
    fn save_project(&mut self, ask: bool) -> bool {
        self.project.finish();
        let path = match (&self.project.path, ask) {
            (Some(p), false) => p.clone(),
            _ => match save_project_file(&self.project.suggested_path(project::EXTENSION)) {
                Some(p) => p,
                None => return false,
            },
        };
        match project::save(&path, self.project.timeline()) {
            Ok(()) => {
                self.project.mark_saved(&path);
                self.set_status(format!("Saved {}", path.display()), false);
                true
            }
            Err(e) => {
                self.fail(format!("Can't save {}: {e}", path.display()));
                false
            }
        }
    }

    // ---- Adding files ------------------------------------------------------

    /// A clip for a file: a video, a picture or sound.
    fn clip_for(&self, path: &Path) -> Result<Clip, String> {
        let info = self.engine.probe(path).map_err(|e| e.to_string())?;
        let audio = info.count(StreamKind::Audio) > 0;
        if has_ext(path, IMAGE_EXTS) {
            let mut c = Clip::image(path.to_path_buf());
            if let Some(v) = info.first_video() {
                (c.width, c.height) = (v.width, v.height);
            }
            return Ok(c);
        }
        // Sound files with cover art have a "video" stream too.
        let video = info.first_video().filter(|_| !has_ext(path, AUDIO_EXTS));
        match (video, audio) {
            (Some(v), _) => {
                let len = info.video_length().ok_or("its length is unknown")?;
                let mut c = Clip::video(path.to_path_buf(), len, audio);
                (c.width, c.height, c.fps) = (v.width, v.height, v.frame_rate);
                Ok(c)
            }
            (None, true) => Ok(Clip::audio(path.to_path_buf(), info.duration.ok_or("its length is unknown")?)),
            (None, false) => Err("it has no picture or sound".into()),
        }
    }

    /// Add files to the timeline: at `at` (a track and time, from a drop),
    /// or videos and pictures at the end of V1 and sound at the playhead.
    /// Projects open; subtitle files load for the preview.
    fn add_files(&mut self, files: Vec<PathBuf>, at: Option<(TrackRef, Duration)>) {
        let mut clips = Vec::new();
        let mut errors = Vec::new();
        for f in files {
            if project::is_project(&f) {
                self.request(Pending::Open(f));
            } else if SubtitleFormat::from_path(&f).is_some() {
                self.load_subtitle(f);
            } else {
                match self.clip_for(&f) {
                    Ok(c) => clips.push(c),
                    Err(e) => errors.push(format!("Can't add {}: {e}", file_name(&f))),
                }
            }
        }
        if clips.is_empty() {
            if let Some(e) = errors.into_iter().next() {
                self.set_status(e, true);
            }
            return;
        }
        let was_empty = self.project.timeline().is_empty();
        let label = match clips.len() {
            1 => format!("Add {}", clips[0].name()),
            n => format!("Add {n} Files"),
        };
        let playhead = self.player.position();
        let mut last = None;
        let mut next_at = at;
        self.project.edit(&label, |t| {
            for clip in clips {
                let placed = place(t, clip, next_at, playhead);
                if let Some(c) = placed {
                    // Several files dropped together go one after another.
                    next_at = next_at.and_then(|(r, _)| t.clip(c).map(|k| (r, k.end())));
                    last = Some(c);
                }
            }
        });
        if let Some(c) = last {
            self.view.selected = Some(c);
            let t = self.project.timeline();
            if let Some(k) = t.clip(c) {
                self.set_status(format!("Added {} to {} at {}", k.name(), c.track.name(), format_timestamp(k.start)), false);
            }
        }
        if was_empty {
            self.view.zoom_fit(self.project.timeline().duration());
        }
        if let Some(e) = errors.into_iter().next() {
            self.set_status(e, true);
        }
    }

    fn menu_add(&mut self) {
        let files = pick_media();
        if !files.is_empty() {
            self.add_files(files, None);
        }
    }

    fn load_subtitle(&mut self, path: PathBuf) {
        let name = file_name(&path);
        self.project.edit("Load Subtitles", |t| t.subtitle = Some(path));
        self.player.set_show_subtitles(true);
        self.set_status(format!("Showing {name} on the video."), false);
    }

    /// Dropped files wait for the pointer to say where they landed.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let files = dropped_files(ctx);
        if !files.is_empty() {
            self.drop = Some(Drop { files, since: Instant::now() });
        }
        let Some(d) = &self.drop else { return };
        let moved = ctx.input(|i| i.raw.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_))));
        if moved || d.since.elapsed() >= DROP_WAIT {
            let at = if moved { ctx.input(|i| i.pointer.hover_pos()).and_then(|p| self.view.drop_target(p)) } else { None };
            let files = self.drop.take().map(|d| d.files).unwrap_or_default();
            self.add_files(files, at);
        } else {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    // ---- Export --------------------------------------------------------------

    fn export(&mut self, ctx: &egui::Context) {
        if self.job.is_busy() {
            return;
        }
        if self.project.timeline().is_empty() {
            self.set_status("Add something to the timeline first.", true);
            return;
        }
        self.project.finish();
        if let Some(to) = save_file(&self.project.suggested_path("mp4")) {
            self.export_to(ctx, to);
        }
    }

    fn export_to(&mut self, ctx: &egui::Context, to: PathBuf) {
        if self.project.timeline().files().any(|f| same_file(f, &to)) {
            self.set_status("Choose another name: that file is used in the timeline.", true);
            return;
        }
        let mut req = ExportRequest::new(self.project.timeline().clone(), to.clone());
        req.keep_tags = self.write_tags;
        let label = format!("Exporting {}", file_name(&to));
        self.player.pause();
        let engine = self.engine.clone();
        self.job.start(ctx, label, Job::Export { to }, move |p, c| engine.export_timeline(&req, p, c));
    }

    fn poll_job(&mut self) {
        let Some((job, result)) = self.job.poll() else { return };
        match (job, result) {
            (Job::Export { to }, Ok(())) => self.set_status(format!("Exported {}", to.display()), false),
            (Job::Export { to }, Err(e)) => {
                // The engine deletes a half-written file.
                let gone = if to.exists() { String::new() } else { format!(" The unfinished {} was deleted.", file_name(&to)) };
                if e == "cancelled" {
                    self.set_status(format!("Export cancelled.{gone}"), false);
                } else {
                    self.fail(e.clone());
                    self.set_status(format!("Export failed: {}.{gone}", last_line(&e).trim_end_matches('.')), true);
                }
            }
        }
    }

    fn fail(&mut self, e: String) {
        self.set_status(last_line(&e).to_owned(), true);
        self.dialog = Some(Dialog::Error(e));
    }

    fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        self.status = Some((msg.into(), error));
    }

    fn undo(&mut self) {
        if self.project.undo() {
            self.view.fix_selection(self.project.timeline());
            self.set_status("Undone.", false);
        }
    }

    fn redo(&mut self) {
        if self.project.redo() {
            self.view.fix_selection(self.project.timeline());
            self.set_status("Redone.", false);
        }
    }

    // ---- Input ---------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let shift = |key| KeyboardShortcut::new(Modifiers::SHIFT, key);
        let plain = |key| KeyboardShortcut::new(Modifiers::NONE, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

        // Shifted ones first: a plain shortcut also matches with Shift held.
        if pressed(cmd_shift(Key::S)) {
            self.save_project(true);
        }
        if pressed(cmd(Key::S)) {
            self.save_project(false);
        }
        if pressed(cmd(Key::N)) {
            self.request(Pending::New);
        }
        if pressed(cmd(Key::O)) {
            self.menu_open();
        }
        if pressed(cmd(Key::I)) {
            self.menu_add();
        }
        if pressed(cmd(Key::E)) {
            self.export(ctx);
        }
        if pressed(cmd(Key::Q)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if pressed(cmd_shift(Key::Z)) || pressed(cmd(Key::Y)) {
            self.redo();
        }
        if pressed(cmd(Key::Z)) {
            self.undo();
        }
        let at = self.player.position();
        if pressed(plain(Key::Space)) {
            self.project.finish();
            self.player.toggle_play();
        }
        if pressed(plain(Key::S)) {
            self.view.split(&mut self.project, at);
        }
        if pressed(shift(Key::Delete)) || pressed(shift(Key::Backspace)) {
            self.view.delete_selected(&mut self.project, true);
        }
        if pressed(plain(Key::Delete)) || pressed(plain(Key::Backspace)) {
            self.view.delete_selected(&mut self.project, false);
        }
        if pressed(plain(Key::Escape)) {
            self.view.selected = None;
        }
        if pressed(plain(Key::Plus)) || pressed(plain(Key::Equals)) || pressed(shift(Key::Equals)) {
            self.view.zoom_in(at);
        }
        if pressed(plain(Key::Minus)) {
            self.view.zoom_out(at);
        }
        let five = Duration::from_secs(5);
        let frame = self.player.frame_len();
        if pressed(shift(Key::ArrowLeft)) {
            self.player.seek(at.saturating_sub(five));
        }
        if pressed(shift(Key::ArrowRight)) {
            self.player.seek(at + five);
        }
        if pressed(plain(Key::ArrowLeft)) {
            self.player.pause();
            self.player.seek(at.saturating_sub(frame));
        }
        if pressed(plain(Key::ArrowRight)) {
            self.player.pause();
            self.player.seek(at + frame);
        }
        if pressed(plain(Key::Home)) {
            self.player.seek(Duration::ZERO);
        }
        if pressed(plain(Key::End)) {
            self.player.seek(self.player.duration().saturating_sub(frame));
        }
    }

    // ---- Layout ----------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let busy = self.job.is_busy();
        let has_clips = !self.project.timeline().is_empty();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if item(ui, true, "New Project", "Ctrl+N") {
                    self.request(Pending::New);
                }
                if item(ui, true, "Open Project…", "Ctrl+O") {
                    self.menu_open();
                }
                if item(ui, true, "Save Project", "Ctrl+S") {
                    self.save_project(false);
                }
                if item(ui, true, "Save Project As…", "Ctrl+Shift+S") {
                    self.save_project(true);
                }
                ui.separator();
                if item(ui, true, "Add to Timeline…", "Ctrl+I") {
                    self.menu_add();
                }
                ui.separator();
                if item(ui, has_clips && !busy, "Export Video…", "Ctrl+E") {
                    self.export(&ctx);
                }
                ui.separator();
                ui.menu_button("Preferences", |ui| {
                    let mut tags = self.write_tags;
                    if check_item(ui, &mut tags, "Write encoder name and dates into exported files") {
                        self.write_tags = tags;
                        Settings::update(|s| s.write_tags = tags);
                    }
                });
                ui.separator();
                if item(ui, true, "Exit", "Ctrl+Q") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.menu_button("Edit", |ui| {
                let text = |verb: &str, what: Option<&str>| match what {
                    Some(w) => format!("{verb} {w}"),
                    None => verb.to_owned(),
                };
                let (undo, redo) = (self.project.undo_label().map(str::to_owned), self.project.redo_label().map(str::to_owned));
                if item(ui, undo.is_some(), &text("Undo", undo.as_deref()), "Ctrl+Z") {
                    self.undo();
                }
                if item(ui, redo.is_some(), &text("Redo", redo.as_deref()), "Ctrl+Y") {
                    self.redo();
                }
                ui.separator();
                let sel = self.view.selected.is_some();
                let at = self.player.position();
                if item(ui, has_clips, "Split at Playhead", "S") {
                    self.view.split(&mut self.project, at);
                }
                if item(ui, sel, "Delete", "Del") {
                    self.view.delete_selected(&mut self.project, false);
                }
                if item(ui, sel, "Ripple Delete", "Shift+Del") {
                    self.view.delete_selected(&mut self.project, true);
                }
                ui.separator();
                if item(ui, true, "Add Video Track", "") {
                    self.project.edit("Add Video Track", |t| t.add_track(TrackKind::Video));
                }
                if item(ui, true, "Add Audio Track", "") {
                    self.project.edit("Add Audio Track", |t| t.add_track(TrackKind::Audio));
                }
            });

            ui.menu_button("Subtitles", |ui| {
                if item(ui, true, "Load…", "") {
                    if let Some(p) = pick_file("Subtitles", SubtitleFormat::EXTENSIONS) {
                        self.load_subtitle(p);
                    }
                }
                let loaded = self.project.timeline().subtitle.is_some();
                if loaded {
                    let mut show = self.player.show_subtitles();
                    if check_item(ui, &mut show, "Show on Video") {
                        self.player.set_show_subtitles(show);
                    }
                    if ui.button("Unload").clicked() {
                        self.project.edit("Unload Subtitles", |t| t.subtitle = None);
                    }
                }
                ui.separator();
                ui.add_enabled_ui(loaded, |ui| {
                    ui.menu_button("When Exporting", |ui| {
                        let mut mode = self.project.timeline().subtitle_export;
                        let old = mode;
                        ui.radio_value(&mut mode, SubtitleExport::Off, "Leave Out");
                        ui.radio_value(&mut mode, SubtitleExport::Track, "Add as Track (viewers can turn it off)");
                        ui.radio_value(&mut mode, SubtitleExport::Burn, "Burn into Picture");
                        if mode != old {
                            self.project.edit("Subtitle Export", |t| t.subtitle_export = mode);
                        }
                    });
                    ui.menu_button("Track Language", |ui| {
                        let mut lang = self.project.timeline().subtitle_language.clone();
                        let old = lang.clone();
                        for (code, name) in LANGUAGES {
                            ui.radio_value(&mut lang, (*code).to_owned(), *name);
                        }
                        if lang != old {
                            self.project.edit("Subtitle Language", |t| t.subtitle_language = lang);
                        }
                    });
                });
            });

            ui.menu_button("View", |ui| {
                ui.menu_button("Theme", |ui| {
                    for t in Theme::ALL {
                        if ui.radio(self.theme == t, t.name()).clicked() {
                            self.theme = t;
                            t.apply(&ctx);
                            Settings::update(|s| s.theme = t);
                        }
                    }
                });
                ui.separator();
                let at = self.player.position();
                if item(ui, true, "Zoom In", "+") {
                    self.view.zoom_in(at);
                }
                if item(ui, true, "Zoom Out", "-") {
                    self.view.zoom_out(at);
                }
                if item(ui, true, "Zoom to Fit", "") {
                    self.view.zoom_fit(self.project.timeline().duration());
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard Shortcuts").clicked() {
                    self.dialog = Some(Dialog::Shortcuts);
                }
                if ui.button("About Flicker").clicked() {
                    self.dialog = Some(Dialog::About);
                }
            });
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let theme = self.theme;
            let has = !self.player.duration().is_zero();
            let button = |ui: &mut egui::Ui, icon: Icon, tip: &str| {
                let resp = ui.add_enabled(has, egui::Button::new("").min_size(egui::vec2(32.0, 24.0)));
                theme.bevel(ui.painter(), resp.rect, resp.is_pointer_button_down_on());
                let ink = if has { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                draw_icon(ui.painter(), resp.rect.center(), icon, ink);
                resp.on_hover_text(tip).clicked()
            };
            if button(ui, Icon::Start, "Start (Home)") {
                self.player.seek(Duration::ZERO);
            }
            let icon = if self.player.is_playing() { Icon::Pause } else { Icon::Play };
            if button(ui, icon, "Play / pause (Space)") {
                self.project.finish();
                self.player.toggle_play();
            }
            if button(ui, Icon::End, "End (End)") {
                let end = self.player.duration().saturating_sub(self.player.frame_len());
                self.player.seek(end);
            }
            ui.add_space(6.0);
            ui.monospace(format!("{} / {}", format_timestamp(self.player.position()), format_timestamp(self.player.duration())));
            if let Some(note) = self.player.sound_note() {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("🔇 {note}")).color(ui.visuals().warn_fg_color));
            }
        });
        ui.add_space(2.0);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        ui.horizontal(|ui| {
            let job_running = self.job.progress_ui(ui);
            if !job_running {
                let (text, error) = match (&self.status, self.player.error()) {
                    (Some((t, e)), _) => (t.clone(), *e),
                    (None, Some(e)) => (format!("Preview: {}", last_line(e)), true),
                    (None, None) if self.project.timeline().is_empty() => {
                        ("Add videos, pictures and music with File > Add to Timeline, or drop them on the timeline.".into(), false)
                    }
                    (None, None) => ("Ready".into(), false),
                };
                let color = if error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
                sunken_label(ui, theme, RichText::new(text).color(color), ui.available_width() - 290.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let t = self.project.timeline();
                let c = t.suggested_canvas();
                let edited = if self.project.is_dirty() { " (edited)" } else { "" };
                let summary = format!(
                    "{}x{} · {:.0} fps · {} · {} clips{edited}",
                    c.width,
                    c.height,
                    c.fps,
                    format_duration(t.duration()),
                    t.clip_count()
                );
                sunken_label(ui, theme, RichText::new(summary).small(), 280.0);
            });
        });
    }

    fn video_area(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let inner = rect.shrink(2.0);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        self.player.ui(&mut child);
        ui.advance_cursor_after_rect(rect);
        self.theme.bevel(ui.painter(), rect, true);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let theme = self.theme;
        let mut close = false;
        let mut then: Option<Result<Pending, Pending>> = None; // Ok = discard & go, Err = save first
        match &mut dialog {
            Dialog::Unsaved(what) => {
                let name = self.project.name();
                window(ctx, theme, "Flicker", |ui| {
                    ui.label(format!("The changes to {name} haven't been saved."));
                    ui.label("Save the project first?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            then = Some(Err(what.clone()));
                        }
                        if ui.button("Don't Save").clicked() {
                            then = Some(Ok(what.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::Shortcuts => {
                window(ctx, theme, "Keyboard Shortcuts", |ui| {
                    egui::Grid::new("keys").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        for (k, what) in SHORTCUTS {
                            ui.label(RichText::new(*k).strong());
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("Drag clips to move them, drag their edges to trim, right-click for more.").small());
                    ui.add_space(8.0);
                    close = ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
                });
            }
            Dialog::About => {
                let engine = self.engine.name();
                window(ctx, theme, "About Flicker", |ui| {
                    ui.label(RichText::new(crate::APP_NAME).strong().size(18.0));
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label(format!("Engine: {}", engine.lines().next().unwrap_or_default()));
                    ui.add_space(8.0);
                    close = ui.button("OK").clicked();
                });
            }
            Dialog::Error(e) => {
                let e = e.clone();
                window(ctx, theme, "Error", |ui| {
                    ui.label(RichText::new(last_line(&e)).strong());
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&e).monospace().small()).wrap());
                    });
                    ui.add_space(8.0);
                    close = ui.button("OK").clicked();
                });
            }
        }
        if !close && then.is_none() {
            self.dialog = Some(dialog);
        }
        match then {
            Some(Ok(what)) => {
                self.proceed(what);
                if self.allow_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            // Save first; carry on only if it was saved.
            Some(Err(what)) if self.save_project(false) => {
                self.proceed(what);
                if self.allow_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(Err(_)) | None => {}
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let star = if self.project.is_dirty() { " *" } else { "" };
        let title = format!("{} - {}{star}", crate::APP_NAME, self.project.name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// Hand the player the timeline as it is now.
    fn sync_player(&mut self) {
        if self.shown_version != Some(self.project.version()) {
            self.shown_version = Some(self.project.version());
            self.player.set_timeline(Arc::new(self.project.timeline().clone()));
        }
    }
}

/// Where a new clip goes (see [`FlickerApp::add_files`]).
fn place(t: &mut Timeline, clip: Clip, at: Option<(TrackRef, Duration)>, playhead: Duration) -> Option<ClipRef> {
    match at {
        Some((r, time)) => {
            let target = if t.track(r).is_some_and(|tr| tr.accepts(&clip)) {
                r
            } else if clip.kind == ClipKind::Audio {
                TrackRef::A1
            } else {
                TrackRef::V1
            };
            t.add_clip(target, clip, time)
        }
        None if clip.kind == ClipKind::Audio => {
            // The first audio track with room at the playhead, or a new one.
            let len = clip.len();
            let free = (0..t.audio.len()).find(|&i| t.audio[i].is_free(playhead, playhead + len, None));
            let r = match free {
                Some(i) => TrackRef::audio(i),
                None => t.add_track(TrackKind::Audio),
            };
            t.add_clip(r, clip, playhead)
        }
        None => t.append(TrackRef::V1, clip),
    }
}

impl eframe::App for FlickerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(shot) = &mut self.dev_screenshot {
            shot.tick(&ctx);
        }
        self.poll_job();
        if self.job.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        // Closing with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.project.is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::Unsaved(Pending::Quit));
            } else {
                self.job.cancel();
            }
        }

        self.handle_drops(&ctx);
        self.shortcuts(&ctx);
        self.update_title(&ctx);

        let theme = self.theme;
        let face = if theme.is_win95() { theme::FACE } else { ui.visuals().panel_fill };
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(270.0)
            .size_range(150.0..=700.0)
            .frame(egui::Frame::NONE.fill(face).inner_margin(4))
            .show(ui, |ui| {
                self.view.toolbar(ui, theme, &mut self.project, &mut self.player);
                ui.add_space(2.0);
                self.view.tracks(ui, theme, &mut self.project, &mut self.player);
            });
        egui::Panel::bottom("transport").show(ui, |ui| self.transport(ui));

        // A drag or slider has ended once nothing is held or being typed in.
        if self.project.in_gesture() && !ctx.input(|i| i.pointer.any_down()) && ctx.memory(|m| m.focused().is_none()) {
            self.project.finish();
        }
        self.sync_player();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(face).inner_margin(4))
            .show(ui, |ui| self.video_area(ui));

        self.dialogs(&ctx);
    }

    fn on_exit(&mut self) {
        // An export still running is stopped, and its unfinished file deleted.
        self.job.cancel_and_wait(Duration::from_secs(5));
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Start,
    Play,
    Pause,
    End,
}

/// Transport icons, drawn so they look the same in every theme and font.
fn draw_icon(p: &egui::Painter, c: egui::Pos2, icon: Icon, ink: Color32) {
    let tri = |dir: f32, x: f32| {
        let pts = vec![egui::pos2(x - 4.0 * dir, c.y - 6.0), egui::pos2(x + 4.0 * dir, c.y), egui::pos2(x - 4.0 * dir, c.y + 6.0)];
        egui::Shape::convex_polygon(pts, ink, egui::Stroke::NONE)
    };
    let bar = |x: f32| egui::Rect::from_center_size(egui::pos2(x, c.y), egui::vec2(2.5, 12.0));
    match icon {
        Icon::Play => {
            p.add(tri(1.0, c.x + 1.0));
        }
        Icon::Pause => {
            p.rect_filled(bar(c.x - 3.0), 0.0, ink);
            p.rect_filled(bar(c.x + 3.0), 0.0, ink);
        }
        Icon::Start => {
            p.rect_filled(bar(c.x - 5.0), 0.0, ink);
            p.add(tri(-1.0, c.x + 1.5));
        }
        Icon::End => {
            p.add(tri(1.0, c.x - 1.5));
            p.rect_filled(bar(c.x + 5.0), 0.0, ink);
        }
    }
}

/// A menu entry with its shortcut shown on the right. Returns true when clicked.
fn item(ui: &mut egui::Ui, enabled: bool, text: &str, shortcut: &str) -> bool {
    let mut b = egui::Button::new(text);
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, b).clicked()
}

/// A menu entry with a check mark in front when on, like Windows menus
/// (checkboxes don't show inside menus). Returns true when toggled.
fn check_item(ui: &mut egui::Ui, on: &mut bool, text: &str) -> bool {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    // The mark is always laid out, just invisible when off, so the text
    // doesn't move. PLACEHOLDER takes the button's own (hover) colour.
    let mark = if *on { Color32::PLACEHOLDER } else { Color32::TRANSPARENT };
    job.append("✔  ", 0.0, egui::TextFormat::simple(font.clone(), mark));
    job.append(text, 0.0, egui::TextFormat::simple(font, Color32::PLACEHOLDER));
    let clicked = ui.add(egui::Button::new(job)).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// A sunken status-bar field.
fn sunken_label(ui: &mut egui::Ui, theme: Theme, text: RichText, width: f32) {
    let width = width.max(80.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
    theme.bevel(ui.painter(), rect, true);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 2.0))));
    child.add(egui::Label::new(text).truncate());
}

/// A modal dialog. In the Windows 95 theme it gets the navy title bar.
fn window(ctx: &egui::Context, theme: Theme, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::same(3));
    egui::Modal::new(egui::Id::unique("dialog")).frame(frame).show(ctx, |ui| {
        ui.set_min_width(320.0);
        let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
        let (fill, text) = if theme.is_win95() {
            (theme::NAVY, theme::LIGHT)
        } else {
            (ui.visuals().selection.bg_fill, ui.visuals().strong_text_color())
        };
        ui.painter().rect_filled(bar, 0.0, fill);
        ui.painter().text(
            bar.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(13.0),
            text,
        );
        egui::Frame::NONE.inner_margin(egui::Margin::same(10)).show(ui, body);
        if theme.is_win95() {
            theme.bevel(ui.painter(), ui.min_rect().expand(3.0), false);
        }
    });
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned()
}

/// Developer aids for checking the UI without a screen grabber:
/// `FLICKER_SCREENSHOT=out.ppm FLICKER_SCREENSHOT_AFTER=2.5 flicker …` saves
/// the window after the given number of seconds. `FLICKER_AUTOPLAY=1`
/// starts playing, `FLICKER_SEEK=3.5` moves the playhead,
/// `FLICKER_SELECT=V1:2` selects a clip, `FLICKER_ZOOM=60` sets pixels per
/// second, `FLICKER_DIALOG=shortcuts|about|unsaved` opens a dialog and
/// `FLICKER_EXPORT=out.mp4` starts exporting there.
mod dev {
    use std::time::{Duration, Instant};

    use eframe::egui;
    use flicker_core::{ClipRef, TrackRef};

    use super::{Dialog, FlickerApp, Pending};

    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|s| !s.is_empty())
    }

    pub fn apply(app: &mut FlickerApp, ctx: &egui::Context) {
        if let Some(out) = var("FLICKER_EXPORT") {
            app.export_to(ctx, out.into());
        }
        if let Some(z) = var("FLICKER_ZOOM").and_then(|z| z.parse().ok()) {
            app.view.zoom = z;
        }
        if let Some(sel) = var("FLICKER_SELECT") {
            app.view.selected = parse_clip(&sel);
        }
        app.sync_player();
        if let Some(t) = var("FLICKER_SEEK").and_then(|s| s.parse::<f64>().ok()) {
            app.player.seek(Duration::from_secs_f64(t));
        }
        if var("FLICKER_AUTOPLAY").is_some() {
            app.player.toggle_play();
        }
        match var("FLICKER_DIALOG").as_deref() {
            Some("shortcuts") => app.dialog = Some(Dialog::Shortcuts),
            Some("about") => app.dialog = Some(Dialog::About),
            Some("unsaved") => app.dialog = Some(Dialog::Unsaved(Pending::Quit)),
            _ => {}
        }
    }

    /// "V1:2" = the third clip on V1.
    fn parse_clip(s: &str) -> Option<ClipRef> {
        let (track, index) = s.split_once(':')?;
        let n: usize = track.get(1..)?.parse().ok()?;
        let track = match track.chars().next()? {
            'V' | 'v' => TrackRef::video(n.checked_sub(1)?),
            _ => TrackRef::audio(n.checked_sub(1)?),
        };
        Some(ClipRef { track, index: index.parse().ok()? })
    }

    pub struct Screenshot {
        path: String,
        at: Instant,
        requested: bool,
    }

    impl Screenshot {
        pub fn from_env() -> Option<Self> {
            let path = var("FLICKER_SCREENSHOT")?;
            let after = var("FLICKER_SCREENSHOT_AFTER").and_then(|s| s.parse::<f64>().ok()).unwrap_or(2.0);
            Some(Self { path, at: Instant::now() + Duration::from_secs_f64(after), requested: false })
        }

        pub fn tick(&mut self, ctx: &egui::Context) {
            if self.requested {
                return;
            }
            let now = Instant::now();
            if now < self.at {
                ctx.request_repaint_after(self.at - now);
                return;
            }
            self.requested = true;
            let path = self.path.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                let [w, h] = img.size;
                let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                out.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
                let _ = std::fs::write(&path, out);
            })));
        }
    }
}
