//! The player: plays the timeline as it is right now, with sound, from the
//! playhead. There's no render step: every change to the timeline restarts
//! decoding at the same spot (playing on if it was playing), and while
//! paused the frame under the playhead is decoded on its own (the newest
//! request wins, recent frames are kept for instant scrubbing back).

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use flicker_core::{Canvas, Engine, Frame, Timeline, TimelinePreview};

use crate::sound::{self, Playback};

/// Frames buffered ahead of the playhead. Small, so restarting stays snappy.
const BUFFER: usize = 4;
/// Largest preview frame. The picture is scaled up to fill the window.
const PREVIEW_W: u32 = 960;
const PREVIEW_H: u32 = 540;
const PREVIEW_FPS: f64 = 30.0;
/// While playing, wait this long after the last edit before restarting, so
/// dragging a slider doesn't restart ffmpeg on every step.
const EDIT_SETTLE: Duration = Duration::from_millis(120);
/// Scrubbed frames kept.
const CACHE: usize = 16;

enum Msg {
    Frame(Frame),
    End,
    Error(String),
}

/// A decoder thread. Dropping it stops decoding: the thread's next send
/// fails, it exits, and ffmpeg is killed.
struct Decoder {
    rx: Receiver<Msg>,
    /// For a single-frame decode: what it's for (timeline revision, time).
    target: Option<(u64, Duration)>,
    started: Instant,
    /// Its first frame has been reported to the sync log.
    logged: bool,
}

impl Decoder {
    fn spawn(engine: Arc<dyn Engine>, req: TimelinePreview, ctx: egui::Context, target: Option<(u64, Duration)>) -> Self {
        let (tx, rx) = mpsc::sync_channel(BUFFER);
        thread::spawn(move || {
            let mut source = match engine.open_timeline_frames(&req) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                    ctx.request_repaint();
                    return;
                }
            };
            loop {
                let msg = match source.next_frame() {
                    Ok(Some(f)) => Msg::Frame(f),
                    Ok(None) => Msg::End,
                    Err(e) => Msg::Error(e.to_string()),
                };
                let last = !matches!(msg, Msg::Frame(_));
                if tx.send(msg).is_err() || last {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });
        Self { rx, target, started: Instant::now(), logged: false }
    }
}

/// What the picture's clock follows while playing.
enum Clock {
    /// The sound card (the normal case).
    Sound(Playback),
    /// No sound: the wall clock, from when the first frame arrived.
    Wall(Option<(Instant, Duration)>),
}

pub struct Player {
    engine: Arc<dyn Engine>,
    timeline: Arc<Timeline>,
    /// Bumped on every change, to tell cached frames apart.
    revision: u64,
    canvas: Canvas,
    duration: Duration,
    show_subtitles: bool,

    texture: Option<TextureHandle>,
    /// The playhead.
    position: Duration,
    playing: bool,
    /// Restart decoding (at `position`) once `restart_at` has passed.
    restart: bool,
    restart_at: Instant,
    decoder: Option<Decoder>,
    clock: Option<Clock>,
    /// Where the current playback started.
    play_from: Duration,
    /// Next frame, decoded but not due yet.
    queued: Option<Frame>,
    /// The picture has ended; waiting for the sound to finish.
    picture_done: bool,
    /// What's on screen: (revision, time).
    shown: Option<(u64, Duration)>,
    cache: VecDeque<(u64, Duration, Frame)>,
    error: Option<String>,
    /// Why there's no sound, for the status bar.
    sound_note: Option<String>,
    sync_log: Option<std::fs::File>,
}

impl Player {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        let sync_log = std::env::var_os("FLICKER_SYNC_LOG").and_then(|p| std::fs::File::create(p).ok());
        Self {
            engine,
            timeline: Arc::new(Timeline::default()),
            revision: 0,
            canvas: Canvas::default().fit_within(PREVIEW_W, PREVIEW_H, PREVIEW_FPS),
            duration: Duration::ZERO,
            show_subtitles: true,
            texture: None,
            position: Duration::ZERO,
            playing: false,
            restart: true,
            restart_at: Instant::now(),
            decoder: None,
            clock: None,
            play_from: Duration::ZERO,
            queued: None,
            picture_done: false,
            shown: None,
            cache: VecDeque::new(),
            error: None,
            sound_note: None,
            sync_log,
        }
    }

    /// Show this version of the timeline from now on.
    pub fn set_timeline(&mut self, timeline: Arc<Timeline>) {
        if *timeline == *self.timeline {
            return;
        }
        self.canvas = timeline.suggested_canvas().fit_within(PREVIEW_W, PREVIEW_H, PREVIEW_FPS);
        self.duration = timeline.duration();
        self.timeline = timeline;
        self.revision += 1;
        self.cache.clear();
        self.error = None;
        if self.position > self.duration {
            self.position = self.duration;
        }
        if self.duration.is_zero() {
            self.playing = false;
        }
        let settle = if self.playing { EDIT_SETTLE } else { Duration::ZERO };
        self.schedule(settle);
    }

    pub fn show_subtitles(&self) -> bool {
        self.show_subtitles
    }

    pub fn set_show_subtitles(&mut self, show: bool) {
        if self.show_subtitles != show {
            self.show_subtitles = show;
            self.revision += 1;
            self.cache.clear();
            self.schedule(Duration::ZERO);
        }
    }

    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Why playback is silent, if it is.
    pub fn sound_note(&self) -> Option<&str> {
        self.sound_note.as_deref()
    }

    pub fn frame_len(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.canvas.fps)
    }

    pub fn seek(&mut self, to: Duration) {
        let to = to.min(self.duration);
        if to == self.position && !self.restart && (self.playing || self.shown == Some((self.revision, to))) {
            return;
        }
        self.position = to;
        self.schedule(Duration::ZERO);
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.playing = false;
            self.schedule(Duration::ZERO);
        }
    }

    pub fn toggle_play(&mut self) {
        if self.duration.is_zero() {
            return;
        }
        self.playing = !self.playing;
        if self.playing && self.position + self.frame_len() >= self.duration {
            // At the end: play again from the start.
            self.position = Duration::ZERO;
        }
        self.schedule(Duration::ZERO);
    }

    /// Restart decoding at `position` after `after`.
    fn schedule(&mut self, after: Duration) {
        if !self.playing || after.is_zero() {
            self.stop_decoding();
        }
        self.restart = true;
        self.restart_at = Instant::now() + after;
    }

    fn stop_decoding(&mut self) {
        // A single-frame decode in flight is left to finish: its frame is
        // close to what's wanted and shows sooner than a new one would.
        if self.decoder.as_ref().is_some_and(|d| d.target.is_none()) {
            self.decoder = None;
        }
        self.clock = None;
        self.queued = None;
        self.picture_done = false;
    }

    fn request(&self, single: bool) -> TimelinePreview {
        TimelinePreview {
            timeline: self.timeline.clone(),
            canvas: self.canvas,
            start: self.position,
            subtitles: self.show_subtitles,
            single_frame: single,
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if now < self.restart_at {
            ctx.request_repaint_after(self.restart_at - now);
            return;
        }
        if self.playing {
            self.stop_decoding();
            self.decoder = None;
            self.decoder = Some(Decoder::spawn(self.engine.clone(), self.request(false), ctx.clone(), None));
            self.play_from = self.position;
            let audible = self.timeline.tracks().any(|(_, t)| !t.muted && t.clips.iter().any(|c| c.audible()));
            self.clock = Some(if audible {
                Clock::Sound(Playback::start(self.engine.clone(), self.request(false)))
            } else {
                Clock::Wall(None)
            });
            self.restart = false;
            return;
        }
        // Paused: one frame. A decode already in flight finishes first.
        let target = (self.revision, self.position);
        if self.shown == Some(target) {
            self.restart = false;
            return;
        }
        if let Some(i) = self.cached(target) {
            let frame = self.cache[i].2.clone();
            self.show(ctx, frame, target);
            self.restart = false;
            return;
        }
        if self.decoder.is_none() {
            self.decoder = Some(Decoder::spawn(self.engine.clone(), self.request(true), ctx.clone(), Some(target)));
            self.restart = false;
        }
    }

    fn cached(&self, (rev, t): (u64, Duration)) -> Option<usize> {
        let half = self.frame_len() / 2;
        self.cache.iter().position(|(r, ct, _)| *r == rev && ct.abs_diff(t) <= half)
    }

    fn show(&mut self, ctx: &egui::Context, frame: Frame, at: (u64, Duration)) {
        let size = [frame.width as usize, frame.height as usize];
        let image = ColorImage::from_rgb(size, &frame.rgb);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
        self.shown = Some(at);
    }

    /// The playback clock: where the timeline is now. `None` = not yet.
    fn now(&mut self, first_pts: Duration) -> Option<Duration> {
        match self.clock.as_mut()? {
            Clock::Sound(p) => match p.state() {
                sound::State::Starting => None,
                sound::State::Playing(..) => {
                    self.sound_note = None;
                    p.clock().map(|d| self.play_from + d)
                }
                sound::State::Finished => Some(self.duration),
                sound::State::Unavailable(why) => {
                    // Carry on silently, from where the sound got to.
                    self.sound_note = Some(why);
                    self.clock = Some(Clock::Wall(Some((Instant::now(), first_pts))));
                    Some(first_pts)
                }
            },
            Clock::Wall(origin) => {
                let (start, pts) = *origin.get_or_insert((Instant::now(), first_pts));
                Some(pts + start.elapsed())
            }
        }
    }

    fn sound_finished(&self) -> bool {
        match &self.clock {
            Some(Clock::Sound(p)) => !matches!(p.state(), sound::State::Playing(..) | sound::State::Starting),
            _ => true,
        }
    }

    /// Pull due frames from the decoder and schedule the next repaint.
    fn pump(&mut self, ctx: &egui::Context) {
        if self.restart {
            self.start(ctx);
        }
        loop {
            let frame = match self.queued.take() {
                Some(f) => f,
                None => {
                    let Some(dec) = &self.decoder else { break };
                    match dec.rx.try_recv() {
                        Ok(Msg::Frame(f)) => f,
                        Ok(Msg::End) | Err(TryRecvError::Disconnected) => {
                            let single = dec.target.is_some();
                            self.decoder = None;
                            if single {
                                if self.restart {
                                    self.start(ctx);
                                }
                            } else {
                                self.picture_done = true;
                            }
                            break;
                        }
                        Ok(Msg::Error(e)) => {
                            self.error = Some(e);
                            self.decoder = None;
                            self.playing = false;
                            self.clock = None;
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
            };

            if let (Some(log), Some(d)) = (&mut self.sync_log, &mut self.decoder) {
                // How long the first frame took to arrive (FLICKER_SYNC_LOG).
                if !d.logged {
                    use std::io::Write;
                    d.logged = true;
                    let kind = if d.target.is_some() { "frame" } else { "play" };
                    let _ = writeln!(log, "# {kind} {:.0} ms", d.started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            if let Some(target) = self.decoder.as_ref().and_then(|d| d.target) {
                // A scrubbed frame.
                self.cache.push_back((target.0, target.1, frame.clone()));
                if self.cache.len() > CACHE {
                    self.cache.pop_front();
                }
                self.show(ctx, frame, target);
                continue;
            }
            if !self.playing {
                continue;
            }
            if let Some(Clock::Sound(p)) = &self.clock {
                // The first picture is here: let the sound start.
                p.go();
            }
            let Some(now) = self.now(frame.pts) else {
                // Waiting for the sound to start.
                self.queued = Some(frame);
                ctx.request_repaint_after(Duration::from_millis(5));
                return;
            };
            if frame.pts <= now {
                // Due (or late): show it, then see if a newer one is due too.
                if let Some(log) = &mut self.sync_log {
                    use std::io::Write;
                    let _ = writeln!(log, "{:.4} {:.4}", now.as_secs_f64(), frame.pts.as_secs_f64());
                }
                self.position = frame.pts;
                let at = (self.revision, frame.pts);
                self.show(ctx, frame, at);
            } else {
                ctx.request_repaint_after((frame.pts - now).min(Duration::from_millis(20)));
                self.queued = Some(frame);
                return;
            }
        }
        if self.playing && self.picture_done && self.queued.is_none() {
            if self.sound_finished() {
                self.playing = false;
                self.clock = None;
                self.picture_done = false;
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
        }
        if self.playing && self.decoder.is_some() {
            // Frames arrive with a repaint; this keeps the clock moving.
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    /// Decode as needed and draw the picture, scaled to fit `ui`'s whole
    /// available area on a black background.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);

        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        let empty = self.timeline.is_empty();
        match (&self.texture, empty) {
            (Some(tex), false) => {
                let [w, h] = tex.size().map(|v| v as f32);
                let scale = (rect.width() / w).min(rect.height() / h);
                let picture = egui::Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(tex.id(), picture, uv, Color32::WHITE);
            }
            _ => {
                let msg = match (&self.error, empty) {
                    (Some(e), _) => last_line(e),
                    (None, true) => "File > Add to Timeline…  or drop videos, pictures and music here",
                    (None, false) => "Loading…",
                };
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(15.0), Color32::GRAY);
            }
        }
    }
}

/// ffmpeg puts the useful part of an error at the end of its log.
pub fn last_line(s: &str) -> &str {
    s.lines().rfind(|l| !l.trim().is_empty()).unwrap_or(s)
}
