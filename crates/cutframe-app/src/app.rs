use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cutframe_core::media::format_timestamp;
use cutframe_core::{
    Accuracy, EmbedMode, EmbedRequest, Engine, FfmpegCli, MediaInfo, Selection, SubtitleFormat, TimeRange,
    TrimRequest,
};
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};

use crate::common::{dropped_files, has_ext, pick_file, save_file, JobRunner, VIDEO_EXTS};
use crate::player::{last_line, Player};
use crate::session::{Session, Step};
use crate::theme::{self, Theme};
use crate::timeline::{self, Cut};

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

/// How often a dragged handle refreshes the picture (each refresh restarts
/// the decoder).
const PREVIEW_EVERY: Duration = Duration::from_millis(70);

const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / pause"),
    ("← / →", "Back / forward 5 seconds"),
    ("C", "Start or leave cut mode"),
    ("I / O", "Move the start / end handle to the playhead"),
    ("Enter", "Apply the cut"),
    ("Esc", "Cancel the cut"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Ctrl+O / Ctrl+S", "Open / save as"),
];

/// The open video: its edit history and what's on screen now.
struct Video {
    session: Session,
    info: MediaInfo,
    player: Player,
}

impl Video {
    fn total(&self) -> Duration {
        self.player.duration().unwrap_or_default()
    }

    fn name(&self) -> String {
        file_name(self.session.original())
    }
}

/// What a background job is for, so its result can be put to use.
enum Job {
    /// An edit writing a new step.
    Edit { label: String, output: PathBuf, subtitle: Option<PathBuf>, seek_to: Duration },
    /// Saving the current video, then maybe carrying on with what the save
    /// was asked for.
    Save { to: PathBuf, then: Option<Pending> },
}

/// Something waiting for the user to decide about unsaved changes.
#[derive(Clone, Debug)]
enum Pending {
    Quit,
    Open(PathBuf),
}

enum Dialog {
    Unsaved(Pending),
    Shortcuts,
    About,
    Error(String),
}

pub struct CutFrameApp {
    engine: Arc<dyn Engine>,
    theme: Theme,
    video: Option<Video>,
    cut: Option<Cut>,
    accuracy: Accuracy,
    language: String,
    job: JobRunner<Job>,
    dialog: Option<Dialog>,
    /// Status-bar message: text and whether it's an error.
    status: Option<(String, bool)>,
    /// Set once the user agreed to quit with unsaved changes.
    allow_close: bool,
    title: String,
    last_preview: Instant,
    /// Resume playback once the handle drag that paused it ends.
    resume_after_drag: bool,
    dev_screenshot: Option<dev::Screenshot>,
}

impl CutFrameApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let theme = Theme::load();
        theme.apply(&cc.egui_ctx);
        let mut app = Self {
            engine: Arc::new(FfmpegCli::locate()),
            theme,
            video: None,
            cut: None,
            accuracy: Accuracy::Precise,
            language: "eng".into(),
            job: JobRunner::default(),
            dialog: None,
            status: None,
            allow_close: false,
            title: String::new(),
            last_preview: Instant::now(),
            resume_after_drag: false,
            dev_screenshot: dev::Screenshot::from_env(),
        };
        for f in files {
            app.open_any(f);
        }
        if let (Some(cut), Some(v)) = (dev::cut_from_env(), &app.video) {
            let mut c = Cut::new(v.total(), Duration::ZERO);
            (c.start, c.end) = cut;
            app.cut = Some(c);
        }
        app
    }

    // ---- Opening and showing videos -------------------------------------

    /// Open a dropped or command-line file: a video, or subtitles for the
    /// current video.
    fn open_any(&mut self, path: PathBuf) {
        if SubtitleFormat::from_path(&path).is_some() {
            self.load_subtitle(path);
        } else if has_ext(&path, VIDEO_EXTS) {
            self.request(Pending::Open(path));
        } else {
            self.set_status(format!("Can't open {}: not a video or subtitle file.", file_name(&path)), true);
        }
    }

    /// Do `what`, first asking about unsaved changes if there are any.
    fn request(&mut self, what: Pending) {
        if self.video.as_ref().is_some_and(|v| v.session.is_dirty()) {
            self.dialog = Some(Dialog::Unsaved(what));
        } else {
            self.proceed(what);
        }
    }

    fn proceed(&mut self, what: Pending) {
        match what {
            Pending::Open(path) => self.open_video(path),
            Pending::Quit => self.allow_close = true,
        }
    }

    fn open_video(&mut self, path: PathBuf) {
        match self.engine.probe(&path) {
            Ok(info) if info.first_video().is_some() => {
                let session = Session::new(path);
                self.video = Some(self.load(session, info, Duration::ZERO));
                self.cut = None;
                self.status = None;
            }
            Ok(_) => self.set_status(format!("{} has no video track.", file_name(&path)), true),
            Err(e) => self.set_status(format!("Can't open {}: {e}", file_name(&path)), true),
        }
    }

    fn load(&self, session: Session, info: MediaInfo, seek: Duration) -> Video {
        let fps = info.first_video().and_then(|v| v.frame_rate);
        let mut player = Player::new(self.engine.clone(), session.current().to_path_buf(), info.video_length(), fps);
        player.set_subtitle(session.subtitle().map(Path::to_path_buf));
        player.seek(seek);
        Video { session, info, player }
    }

    /// Put the session's current step on screen.
    fn show_current(&mut self, seek: Duration) {
        let Some(old) = self.video.take() else { return };
        let show_subs = old.player.show_subtitles();
        let session = old.session;
        match self.engine.probe(session.current()) {
            Ok(info) => {
                let mut v = self.load(session, info, seek);
                v.player.set_show_subtitles(show_subs);
                self.video = Some(v);
            }
            Err(e) => {
                self.set_status(format!("Can't read the edited video: {e}"), true);
                self.video = Some(old_player_back(session, old.info, old.player));
            }
        }
        self.cut = None;
    }

    fn load_subtitle(&mut self, path: PathBuf) {
        let Some(v) = &mut self.video else {
            self.set_status("Open a video before loading subtitles.", true);
            return;
        };
        v.session.set_subtitle(Some(path.clone()));
        v.player.set_subtitle(Some(path.clone()));
        v.player.set_show_subtitles(true);
        self.set_status(format!("Showing {} on the video.", file_name(&path)), false);
    }

    fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        self.status = Some((msg.into(), error));
    }

    // ---- Editing ---------------------------------------------------------

    fn toggle_cut(&mut self) {
        if self.cut.take().is_none() {
            if let Some(v) = &self.video {
                self.cut = Some(Cut::new(v.total(), v.player.position()));
            }
        }
    }

    fn apply_cut(&mut self, ctx: &egui::Context) {
        let (Some(cut), Some(v)) = (&self.cut, &mut self.video) else { return };
        if self.job.is_busy() {
            return;
        }
        let total = v.total();
        let keep = cut.keep(total);
        let whole = [TimeRange::new(Duration::ZERO, total)];
        if keep.is_empty() || keep == whole {
            self.set_status("Move the handles first: this cut wouldn't change anything.", true);
            return;
        }
        let output = match v.session.next_path(&v.session.working_ext()) {
            Ok(p) => p,
            Err(e) => return self.set_status(format!("Can't create a working file: {e}"), true),
        };
        let subtitle = retime_subtitle(&mut v.session, &keep);
        let (label, seek_to) = match cut.selection {
            Selection::Keep => ("Trim", Duration::ZERO),
            // Land on the join so the result can be checked straight away.
            Selection::Remove => ("Cut", cut.start.saturating_sub(Duration::from_secs(1))),
        };

        let mut req = TrimRequest::new(v.session.current().to_path_buf(), output.clone(), keep);
        req.accuracy = self.accuracy;
        req.preset = "fast".into();
        v.player.pause();
        let engine = self.engine.clone();
        let job = Job::Edit { label: label.into(), output, subtitle, seek_to };
        self.job.start(ctx, format!("Applying {}", label.to_lowercase()), job, move |p, c| engine.trim(&req, p, c));
    }

    fn embed_subtitles(&mut self, ctx: &egui::Context, mode: EmbedMode) {
        let Some(v) = &mut self.video else { return };
        if self.job.is_busy() {
            return;
        }
        let subtitle = match v.session.subtitle() {
            Some(s) => s.to_path_buf(),
            None => match pick_file("Subtitles", SubtitleFormat::EXTENSIONS) {
                Some(p) => p,
                None => return,
            },
        };
        let output = match v.session.next_path(&v.session.working_ext()) {
            Ok(p) => p,
            Err(e) => return self.set_status(format!("Can't create a working file: {e}"), true),
        };
        let mut req = EmbedRequest::new(v.session.current().to_path_buf(), subtitle.clone(), output.clone());
        req.mode = mode;
        req.language = Some(self.language.clone()).filter(|l| !l.is_empty());
        req.title = subtitle.file_stem().map(|s| s.to_string_lossy().into_owned());
        req.preset = "fast".into();
        let (label, keep_overlay) = match mode {
            EmbedMode::Soft => ("Add subtitles", Some(subtitle)),
            // Burned in: an overlay on top would show them twice.
            EmbedMode::Hard => ("Burn subtitles", None),
        };
        let seek_to = v.player.position();
        v.player.pause();
        let engine = self.engine.clone();
        let job = Job::Edit { label: label.into(), output, subtitle: keep_overlay, seek_to };
        self.job.start(ctx, label, job, move |p, c| engine.embed_subtitles(&req, p, c));
    }

    fn save_as(&mut self, ctx: &egui::Context, then: Option<Pending>) {
        let Some(v) = &mut self.video else { return };
        if self.job.is_busy() {
            return;
        }
        let Some(to) = save_file(&v.session.suggested_output()) else { return };
        let from = v.session.current().to_path_buf();
        if same_file(&from, &to) {
            v.session.mark_saved();
            self.set_status("Already saved there.", false);
            return;
        }
        let same_container = from.extension().map(|e| e.to_ascii_lowercase()) == to.extension().map(|e| e.to_ascii_lowercase());
        let total = v.total();
        v.player.pause();
        let engine = self.engine.clone();
        let dest = to.clone();
        self.job.start(ctx, "Saving", Job::Save { to, then }, move |p, c| {
            if same_container {
                std::fs::copy(&from, &dest)?;
                Ok(())
            } else {
                // Different container: remux without re-encoding.
                let req = TrimRequest::new(from, dest, vec![TimeRange::new(Duration::ZERO, total)]);
                engine.trim(&req, p, c)
            }
        });
    }

    fn undo(&mut self) {
        if !self.job.is_busy() && self.video.as_mut().is_some_and(|v| v.session.undo()) {
            self.show_current(Duration::ZERO);
            self.set_status("Undone.", false);
        }
    }

    fn redo(&mut self) {
        if !self.job.is_busy() && self.video.as_mut().is_some_and(|v| v.session.redo()) {
            self.show_current(Duration::ZERO);
            self.set_status("Redone.", false);
        }
    }

    fn revert(&mut self) {
        let Some(v) = &mut self.video else { return };
        while v.session.undo() {}
        self.show_current(Duration::ZERO);
        self.set_status("Back to the original. Redo (Ctrl+Y) brings the edits back.", false);
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some((job, result)) = self.job.poll() else { return };
        match (job, result) {
            (Job::Edit { label, output, subtitle, seek_to }, Ok(())) => {
                if let Some(v) = &mut self.video {
                    v.session.push(Step { path: output, label: label.clone(), subtitle });
                }
                self.show_current(seek_to);
                self.set_status(format!("{label} done. Keep editing, or Undo with Ctrl+Z."), false);
            }
            (Job::Edit { output, .. }, Err(e)) => {
                let _ = std::fs::remove_file(output);
                self.fail(e);
            }
            (Job::Save { to, then }, Ok(())) => {
                if let Some(v) = &mut self.video {
                    v.session.mark_saved();
                }
                self.set_status(format!("Saved {}", to.display()), false);
                if let Some(then) = then {
                    self.proceed(then);
                    if self.allow_close {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            (Job::Save { .. }, Err(e)) => self.fail(e),
        }
    }

    fn fail(&mut self, e: String) {
        self.set_status(last_line(&e).to_owned(), true);
        self.dialog = Some(Dialog::Error(e));
    }

    // ---- Input -----------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let plain = |key| KeyboardShortcut::new(Modifiers::NONE, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

        if pressed(cmd(Key::O)) {
            self.menu_open();
        }
        if pressed(cmd(Key::S)) {
            self.save_as(ctx, None);
        }
        if pressed(cmd(Key::Q)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if pressed(cmd(Key::Z)) {
            self.undo();
        }
        if pressed(cmd(Key::Y)) {
            self.redo();
        }
        if self.job.is_busy() {
            return;
        }
        if pressed(plain(Key::C)) {
            self.toggle_cut();
        }
        if pressed(plain(Key::Enter)) {
            self.apply_cut(ctx);
        }
        if pressed(plain(Key::Escape)) {
            self.cut = None;
        }
        let Some(v) = &mut self.video else { return };
        if pressed(plain(Key::Space)) {
            v.player.toggle_play();
        }
        let five = Duration::from_secs(5);
        if pressed(plain(Key::ArrowLeft)) {
            v.player.seek(v.player.position().saturating_sub(five));
        }
        if pressed(plain(Key::ArrowRight)) {
            v.player.seek(v.player.position() + five);
        }
        if let Some(cut) = &mut self.cut {
            let at = v.player.position();
            if pressed(plain(Key::I)) {
                cut.set(timeline::Handle::Start, at);
            }
            if pressed(plain(Key::O)) {
                cut.set(timeline::Handle::End, at);
            }
        }
    }

    fn menu_open(&mut self) {
        if let Some(p) = pick_file("Video", VIDEO_EXTS) {
            self.request(Pending::Open(p));
        }
    }

    // ---- Layout ----------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let has_video = self.video.is_some();
        let busy = self.job.is_busy();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if item(ui, true, "Open…", "Ctrl+O") {
                    self.menu_open();
                }
                if item(ui, has_video && !busy, "Save As…", "Ctrl+S") {
                    self.save_as(&ctx, None);
                }
                let edited = self.video.as_ref().is_some_and(|v| v.session.undo_label().is_some());
                if item(ui, edited && !busy, "Revert to Original", "") {
                    self.revert();
                }
                ui.separator();
                if item(ui, true, "Exit", "Ctrl+Q") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.menu_button("Edit", |ui| {
                let (undo, redo) = match &self.video {
                    Some(v) => (v.session.undo_label().map(str::to_owned), v.session.redo_label().map(str::to_owned)),
                    None => (None, None),
                };
                let text = |verb: &str, what: &Option<String>| match what {
                    Some(w) => format!("{verb} {w}"),
                    None => verb.to_owned(),
                };
                if item(ui, undo.is_some() && !busy, &text("Undo", &undo), "Ctrl+Z") {
                    self.undo();
                }
                if item(ui, redo.is_some() && !busy, &text("Redo", &redo), "Ctrl+Y") {
                    self.redo();
                }
            });

            ui.menu_button("Cut", |ui| {
                let cutting = self.cut.is_some();
                let label = if cutting { "Leave Cut Mode" } else { "Cut Mode" };
                if item(ui, has_video && !busy, label, "C") {
                    self.toggle_cut();
                }
                ui.separator();
                if let Some(cut) = &mut self.cut {
                    ui.radio_value(&mut cut.selection, Selection::Keep, "Keep between handles");
                    ui.radio_value(&mut cut.selection, Selection::Remove, "Remove between handles");
                    ui.separator();
                }
                let at = self.video.as_ref().map(|v| v.player.position());
                if item(ui, cutting, "Start Handle to Playhead", "I") {
                    if let (Some(c), Some(t)) = (&mut self.cut, at) {
                        c.set(timeline::Handle::Start, t);
                    }
                }
                if item(ui, cutting, "End Handle to Playhead", "O") {
                    if let (Some(c), Some(t)) = (&mut self.cut, at) {
                        c.set(timeline::Handle::End, t);
                    }
                }
                ui.separator();
                if item(ui, cutting && !busy, "Apply", "Enter") {
                    self.apply_cut(&ctx);
                }
                if item(ui, cutting, "Cancel", "Esc") {
                    self.cut = None;
                }
                ui.separator();
                ui.menu_button("Accuracy", |ui| {
                    ui.radio_value(&mut self.accuracy, Accuracy::Precise, "Precise: exact frame, re-encodes");
                    ui.radio_value(&mut self.accuracy, Accuracy::Fast, "Fast: lossless, snaps to keyframes");
                });
            });

            ui.menu_button("Subtitles", |ui| {
                if item(ui, has_video, "Load for Preview…", "") {
                    if let Some(p) = pick_file("Subtitles", SubtitleFormat::EXTENSIONS) {
                        self.load_subtitle(p);
                    }
                }
                if let Some(v) = &mut self.video {
                    if v.player.has_subtitle() {
                        let mut show = v.player.show_subtitles();
                        if ui.checkbox(&mut show, "Show on Video").changed() {
                            v.player.set_show_subtitles(show);
                        }
                        if ui.button("Unload").clicked() {
                            v.session.set_subtitle(None);
                            v.player.set_subtitle(None);
                        }
                    }
                }
                ui.separator();
                if item(ui, has_video && !busy, "Add as Track", "") {
                    self.embed_subtitles(&ctx, EmbedMode::Soft);
                }
                if item(ui, has_video && !busy, "Burn into Picture", "") {
                    self.embed_subtitles(&ctx, EmbedMode::Hard);
                }
                ui.menu_button("Track Language", |ui| {
                    for (code, name) in LANGUAGES {
                        ui.radio_value(&mut self.language, (*code).to_owned(), *name);
                    }
                });
            });

            ui.menu_button("View", |ui| {
                ui.menu_button("Theme", |ui| {
                    for t in Theme::ALL {
                        if ui.radio(self.theme == t, t.name()).clicked() {
                            self.theme = t;
                            t.apply(&ctx);
                            t.save();
                        }
                    }
                });
            });

            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard Shortcuts").clicked() {
                    self.dialog = Some(Dialog::Shortcuts);
                }
                if ui.button("About CutFrame").clicked() {
                    self.dialog = Some(Dialog::About);
                }
            });

            // While cutting, the cut's summary and its two actions sit right
            // in the bar.
            if let (Some(cut), Some(v)) = (&self.cut, &self.video) {
                ui.separator();
                let result: Duration = cut.keep(v.total()).iter().map(TimeRange::len).sum();
                let verb = match cut.selection {
                    Selection::Keep => "Keep",
                    Selection::Remove => "Remove",
                };
                ui.label(format!(
                    "{verb} {} – {}  →  {}",
                    format_timestamp(cut.start),
                    format_timestamp(cut.end),
                    format_timestamp(result)
                ));
                let theme = self.theme;
                let button = |ui: &mut egui::Ui, enabled: bool, text: &str| {
                    let resp = ui.add_enabled(enabled, egui::Button::new(text).min_size(egui::vec2(72.0, 20.0)));
                    theme.bevel(ui.painter(), resp.rect, resp.is_pointer_button_down_on());
                    resp.clicked()
                };
                if button(ui, !busy, "✔ Apply") {
                    self.apply_cut(&ctx);
                }
                if button(ui, true, "✖ Cancel") {
                    self.cut = None;
                }
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        let Some(v) = &mut self.video else { return };
        let total = v.total();
        let busy = self.job.is_busy();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let icon = if v.player.is_playing() { "⏸" } else { "▶" };
            let play = ui.add_enabled(!busy, egui::Button::new(RichText::new(icon).size(15.0)).min_size(egui::vec2(34.0, 26.0)));
            if play.on_hover_text("Play / pause (Space)").clicked() {
                v.player.toggle_play();
            }
            ui.monospace(format!("{} / {}", format_timestamp(v.player.position()), format_timestamp(total)));
            ui.add_space(4.0);
            ui.add_enabled_ui(!busy, |ui| {
                let out = timeline::show(ui, theme, total, v.player.position(), self.cut.as_mut());
                let dragging_handle = self.cut.as_ref().is_some_and(|c| c.dragging().is_some());
                if dragging_handle && v.player.is_playing() {
                    self.resume_after_drag = true;
                    v.player.pause();
                }
                if let Some(t) = out.preview {
                    if self.last_preview.elapsed() >= PREVIEW_EVERY {
                        self.last_preview = Instant::now();
                        v.player.seek(t);
                    } else {
                        ui.ctx().request_repaint_after(PREVIEW_EVERY);
                    }
                }
                if let Some(t) = out.seek {
                    v.player.seek(t);
                    if std::mem::take(&mut self.resume_after_drag) {
                        v.player.toggle_play();
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        ui.horizontal(|ui| {
            let job_running = self.job.progress_ui(ui);
            if !job_running {
                let (text, error) = match (&self.status, &self.video) {
                    (Some((t, e)), _) => (t.clone(), *e),
                    (None, Some(_)) if self.cut.is_some() => {
                        ("Drag the handles. Space plays the result in a loop. Enter applies.".into(), false)
                    }
                    (None, Some(_)) => ("Ready".into(), false),
                    (None, None) => ("Open a video with File > Open, or drop one on the window.".into(), false),
                };
                let color = if error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
                sunken_label(ui, theme, RichText::new(text).color(color), ui.available_width() - 260.0);
            }
            if let Some(v) = &self.video {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = if v.session.is_dirty() { " (edited)" } else { "" };
                    sunken_label(ui, theme, RichText::new(format!("{}{dirty}", v.info.summary())).small(), 250.0);
                });
            }
        });
    }

    fn video_area(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        match &mut self.video {
            Some(v) => {
                let inner = rect.shrink(2.0);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                v.player.ui(&mut child);
                ui.advance_cursor_after_rect(rect);
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "File > Open…  or drop a video here",
                    egui::FontId::proportional(16.0),
                    Color32::GRAY,
                );
                ui.advance_cursor_after_rect(rect);
            }
        }
        self.theme.bevel(ui.painter(), rect, true);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &self.dialog else { return };
        let theme = self.theme;
        let mut close = false;
        let mut then: Option<Result<Pending, Pending>> = None; // Ok = discard & go, Err = save first
        match dialog {
            Dialog::Unsaved(what) => {
                let name = self.video.as_ref().map(Video::name).unwrap_or_default();
                window(ctx, theme, "CutFrame", |ui| {
                    ui.label(format!("The edits to {name} haven't been saved."));
                    ui.label("Save them as a new file first?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save As…").clicked() {
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
                    ui.add_space(8.0);
                    close = ui.button("OK").clicked();
                });
            }
            Dialog::About => {
                let engine = self.engine.name();
                window(ctx, theme, "About CutFrame", |ui| {
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
        if close {
            self.dialog = None;
        }
        match then {
            Some(Ok(what)) => {
                self.dialog = None;
                if let Some(v) = &mut self.video {
                    v.session.mark_saved(); // discarding: nothing left to ask about
                }
                self.proceed(what);
                if self.allow_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(Err(what)) => {
                self.dialog = None;
                self.save_as(ctx, Some(what));
            }
            None => {}
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.video {
            Some(v) => {
                let star = if v.session.is_dirty() { " *" } else { "" };
                format!("{} - {}{star}", crate::APP_NAME, v.name())
            }
            None => crate::APP_NAME.to_owned(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for CutFrameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(shot) = &mut self.dev_screenshot {
            shot.tick(&ctx);
        }
        self.poll_job(&ctx);
        if self.job.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        // Closing with unsaved edits asks first (the working files are temporary).
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.video.as_ref().is_some_and(|v| v.session.is_dirty()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::Unsaved(Pending::Quit));
            } else {
                self.job.cancel();
            }
        }

        if !self.job.is_busy() {
            for path in dropped_files(&ctx) {
                self.open_any(path);
            }
        }
        self.shortcuts(&ctx);
        if let Some(v) = &mut self.video {
            let total = v.total();
            v.player.set_play_ranges(self.cut.as_ref().map(|c| c.keep(total)));
        }
        self.update_title(&ctx);

        let theme = self.theme;
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu_bar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.video.is_some() {
            egui::Panel::bottom("transport").show(ui, |ui| self.transport(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(if theme.is_win95() { theme::FACE } else { ui.visuals().panel_fill }).inner_margin(4))
            .show(ui, |ui| self.video_area(ui));

        self.dialogs(&ctx);
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

/// Keep showing the previous video when the new step can't be read.
fn old_player_back(session: Session, info: MediaInfo, player: Player) -> Video {
    Video { session, info, player }
}

/// Retime the preview subtitles to match an edit keeping `keep`, writing the
/// result next to the step files. `None` if there are none or they can't be read.
fn retime_subtitle(session: &mut Session, keep: &[TimeRange]) -> Option<PathBuf> {
    let src = session.subtitle()?.to_path_buf();
    let text = std::fs::read_to_string(&src).ok()?;
    let (out_text, ext) = match SubtitleFormat::from_path(&src)? {
        SubtitleFormat::Ass => (cutframe_core::retime::retime_ass(&text, keep), "ass"),
        SubtitleFormat::Srt => (cutframe_core::retime::retime_srt(&text, keep), "srt"),
        SubtitleFormat::WebVtt => return None,
    };
    let path = session.next_path(ext).ok()?;
    std::fs::write(&path, out_text).ok()?;
    Some(path)
}

/// Developer aids for checking the UI without a screen grabber:
/// `CUTFRAME_SCREENSHOT=out.ppm CUTFRAME_SCREENSHOT_AFTER=2.5 cutframe …`
/// saves the window after the given number of seconds. Add `CUTFRAME_AUTOPLAY=1`
/// to start playing, and `CUTFRAME_CUT=2,4` to open cut mode with the handles
/// at 2s and 4s.
mod dev {
    use std::time::{Duration, Instant};

    use eframe::egui;

    pub fn cut_from_env() -> Option<(Duration, Duration)> {
        let s = std::env::var("CUTFRAME_CUT").ok()?;
        let (a, b) = s.split_once(',')?;
        Some((Duration::from_secs_f64(a.trim().parse().ok()?), Duration::from_secs_f64(b.trim().parse().ok()?)))
    }

    pub struct Screenshot {
        path: String,
        at: Instant,
        requested: bool,
    }

    impl Screenshot {
        pub fn from_env() -> Option<Self> {
            let path = std::env::var("CUTFRAME_SCREENSHOT").ok()?;
            let after = std::env::var("CUTFRAME_SCREENSHOT_AFTER")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(2.0);
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |img| {
                    let [w, h] = img.size;
                    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                    out.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
                    let _ = std::fs::write(&path, out);
                },
            )));
        }
    }
}
