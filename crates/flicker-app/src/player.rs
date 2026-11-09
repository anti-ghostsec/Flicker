//! Video player: decodes on a background thread, shows frames scaled to fit,
//! draws an optional subtitle overlay, and can loop over a set of ranges (the
//! live preview of a cut). Transport controls live in the app.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use flicker_core::{Engine, Frame, PreviewRequest, TimeRange};
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

/// Frames buffered ahead of the playhead. Small, so seeking stays snappy.
const BUFFER: usize = 4;
/// A frame this close before a range's start still counts as inside it
/// (decoders land a hair early), so looping can't bounce forever.
const SLACK: Duration = Duration::from_millis(80);

enum Msg {
    Frame(Frame),
    End,
    Error(String),
}

/// A running decoder thread. Dropping it stops decoding: the thread's next
/// send fails, it exits, and the ffmpeg process is killed.
struct Decoder {
    rx: Receiver<Msg>,
}

impl Decoder {
    fn spawn(engine: Arc<dyn Engine>, req: PreviewRequest, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel(BUFFER);
        thread::spawn(move || {
            let mut source = match engine.open_preview(&req) {
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
        Self { rx }
    }
}

pub struct Player {
    engine: Arc<dyn Engine>,
    video: PathBuf,
    subtitle: Option<PathBuf>,
    show_subtitles: bool,
    duration: Option<Duration>,
    fps: f64,

    texture: Option<TextureHandle>,
    /// Timestamp of the frame on screen (or where we'll seek to).
    position: Duration,
    playing: bool,
    decoder: Option<Decoder>,
    needs_decoder: bool,
    /// Wall-clock instant matching `clock_pts`, set when the first frame arrives
    /// so ffmpeg's startup delay doesn't make us skip frames.
    clock: Option<(Instant, Duration)>,
    /// Next frame, decoded but not due yet.
    queued: Option<Frame>,
    /// When set, playback only visits these (sorted) ranges, jumping from the
    /// end of one to the start of the next and looping back to the first.
    play_ranges: Option<Vec<TimeRange>>,
    /// A frame was played since the decoder last (re)started, so reaching the
    /// end means `position` is the real last frame.
    shown_this_run: bool,
    error: Option<String>,
}

impl Player {
    pub fn new(
        engine: Arc<dyn Engine>,
        video: PathBuf,
        duration: Option<Duration>,
        source_fps: Option<f64>,
    ) -> Self {
        Self {
            engine,
            video,
            subtitle: None,
            show_subtitles: true,
            duration,
            fps: PreviewRequest::preview_fps(source_fps),
            texture: None,
            position: Duration::ZERO,
            // Developer aid (see app.rs `dev`): start playing immediately.
            playing: std::env::var_os("FLICKER_AUTOPLAY").is_some(),
            decoder: None,
            needs_decoder: true,
            clock: None,
            queued: None,
            play_ranges: None,
            shown_this_run: false,
            error: None,
        }
    }

    pub fn set_subtitle(&mut self, path: Option<PathBuf>) {
        if self.subtitle != path {
            self.subtitle = path;
            self.restart();
        }
    }

    pub fn has_subtitle(&self) -> bool {
        self.subtitle.is_some()
    }

    pub fn show_subtitles(&self) -> bool {
        self.show_subtitles
    }

    pub fn set_show_subtitles(&mut self, show: bool) {
        if self.show_subtitles != show {
            self.show_subtitles = show;
            self.restart();
        }
    }

    /// Restrict playback to `ranges` (looping), or `None` for the whole video.
    pub fn set_play_ranges(&mut self, ranges: Option<Vec<TimeRange>>) {
        let ranges = ranges.filter(|r| !r.is_empty());
        if self.play_ranges == ranges {
            return;
        }
        self.play_ranges = ranges;
        if self.playing && self.jump_target(self.position).is_some() {
            self.restart_at_range();
        }
    }

    /// Stop playback (e.g. while an edit is running).
    pub fn pause(&mut self) {
        if self.playing {
            self.playing = false;
            self.restart();
        }
    }

    /// Timestamp of the frame on screen.
    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn seek(&mut self, to: Duration) {
        self.position = self.duration.map_or(to, |d| to.min(d));
        self.restart();
    }

    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
        if self.playing {
            let at_end = self.duration.is_some_and(|d| self.position + Duration::from_millis(100) >= d);
            if at_end && self.play_ranges.is_none() {
                // At the end: play again from the start.
                self.position = Duration::ZERO;
            }
            if self.jump_target(self.position).is_some() {
                self.restart_at_range();
                return;
            }
        }
        self.restart();
    }

    /// Playback just ran off the end at `position`. If the file claimed to be
    /// longer than its last frame, shrink to fit so the timeline ends where
    /// the picture does.
    fn learn_end(&mut self) {
        let frame = Duration::from_secs_f64(1.0 / self.fps);
        let end = self.position + frame;
        if self.duration.is_some_and(|d| d > end + frame) {
            self.duration = Some(end);
        }
    }

    /// Where playback should continue if `t` is outside the play ranges:
    /// the next range's start, wrapping to the first. `None` = keep going.
    fn jump_target(&self, t: Duration) -> Option<Duration> {
        let ranges = self.play_ranges.as_ref()?;
        if ranges.iter().any(|r| t + SLACK >= r.start && t < r.end) {
            return None;
        }
        let next = ranges.iter().find(|r| r.start > t).unwrap_or(&ranges[0]);
        Some(next.start)
    }

    fn restart_at_range(&mut self) {
        if let Some(t) = self.jump_target(self.position) {
            self.position = t;
        }
        self.restart();
    }

    /// Throw away the current decoder and start a new one at `position`.
    fn restart(&mut self) {
        self.decoder = None;
        self.queued = None;
        self.clock = None;
        self.error = None;
        self.shown_this_run = false;
        self.needs_decoder = true;
    }

    fn ensure_decoder(&mut self, ctx: &egui::Context) {
        if !self.needs_decoder {
            return;
        }
        self.needs_decoder = false;
        let mut req = PreviewRequest::new(self.video.clone());
        req.start = self.position;
        req.fps = self.fps;
        req.single_frame = !self.playing;
        req.subtitle = self.subtitle.clone().filter(|_| self.show_subtitles);
        self.decoder = Some(Decoder::spawn(self.engine.clone(), req, ctx.clone()));
    }

    fn show_frame(&mut self, ctx: &egui::Context, frame: Frame) {
        let size = [frame.width as usize, frame.height as usize];
        let image = ColorImage::from_rgb(size, &frame.rgb);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
        self.position = frame.pts;
    }

    /// Pull due frames from the decoder and schedule the next repaint.
    fn pump(&mut self, ctx: &egui::Context) {
        self.ensure_decoder(ctx);
        loop {
            let frame = match self.queued.take() {
                Some(f) => f,
                None => {
                    let Some(dec) = &self.decoder else { return };
                    match dec.rx.try_recv() {
                        Ok(Msg::Frame(f)) => f,
                        Ok(Msg::End) => {
                            self.decoder = None;
                            if self.playing {
                                if self.shown_this_run {
                                    self.learn_end();
                                }
                                // Loop: back to the start (of the first play range).
                                self.position = self.play_ranges.as_ref().map_or(Duration::ZERO, |r| r[0].start);
                                self.restart();
                                ctx.request_repaint();
                            }
                            return;
                        }
                        Err(TryRecvError::Disconnected) => {
                            self.decoder = None;
                            self.playing = false;
                            return;
                        }
                        Ok(Msg::Error(e)) => {
                            self.error = Some(e);
                            self.decoder = None;
                            self.playing = false;
                            return;
                        }
                        Err(TryRecvError::Empty) => return,
                    }
                }
            };

            if !self.playing {
                self.show_frame(ctx, frame);
                continue;
            }
            if let Some(t) = self.jump_target(frame.pts) {
                // Left the play ranges: continue at the next one.
                self.position = t;
                self.restart();
                ctx.request_repaint();
                return;
            }
            let (start, start_pts) = *self.clock.get_or_insert((Instant::now(), frame.pts));
            let now = start_pts + start.elapsed();
            if frame.pts <= now {
                // Due (or late): show it, then see if a newer one is also due.
                self.show_frame(ctx, frame);
                self.shown_this_run = true;
            } else {
                ctx.request_repaint_after(frame.pts - now);
                self.queued = Some(frame);
                return;
            }
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
        match &self.texture {
            Some(tex) => {
                let [w, h] = tex.size().map(|v| v as f32);
                let scale = (rect.width() / w).min(rect.height() / h);
                let picture = egui::Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(tex.id(), picture, uv, Color32::WHITE);
            }
            None => {
                let msg = match &self.error {
                    Some(e) => last_line(e),
                    None => "Loading…",
                };
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), Color32::GRAY);
            }
        }
    }
}

/// ffmpeg puts the useful part of an error at the end of its log.
pub fn last_line(s: &str) -> &str {
    s.lines().rfind(|l| !l.trim().is_empty()).unwrap_or(s)
}
