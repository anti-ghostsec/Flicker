//! The timeline panel under the player: a time ruler, video tracks (V2 over
//! V1) and audio tracks, clips drawn as named blocks. Click or drag the
//! ruler to scrub; drag a clip to move it (on its track or to another track
//! of the same kind), drag its edges to trim; right-click a clip or a join
//! for more. Edges snap to the playhead and to other clips (hold Shift to
//! place them freely).

use std::time::Duration;

use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind};
use flicker_core::media::format_timestamp;
use flicker_core::timeline::{Anim, Crop, Motion, TextStyle, MAX_SPEED, MIN_SPEED};
use flicker_core::{Clip, ClipKind, ClipRef, Edge, Timeline, TrackKind, TrackRef, TransitionKind};

use crate::player::Player;
use crate::project::Project;
use crate::theme::{self, Theme};

const HEADER_W: f32 = 64.0;
const RULER_H: f32 = 22.0;
const VIDEO_H: f32 = 42.0;
const AUDIO_H: f32 = 30.0;
const SCROLLBAR_H: f32 = 14.0;
/// How close (in pixels) the pointer must be to grab a clip's edge.
const EDGE_GRAB: f32 = 6.0;
/// How close (in pixels) edges snap.
const SNAP: f32 = 8.0;
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 600.0;
/// Transition lengths offered in the menus.
const TRANSITION_LENGTHS: [f32; 3] = [0.5, 1.0, 2.0];
/// Speeds offered in the menu.
const SPEEDS: [f64; 9] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];
/// Entrance and exit lengths offered in the menu.
const ANIM_LENGTHS: [f32; 4] = [0.3, 0.6, 1.0, 2.0];

#[derive(Clone, Copy, Debug)]
enum Drag {
    Scrub,
    /// Moving a clip; `grab` is how far into the clip it was picked up.
    Move { from: ClipRef, grab: Duration, to: Option<(TrackRef, Duration)> },
    /// Trimming an edge. The edge follows the pointer from where it was
    /// pressed (`press_x`), starting from where the edge was then (`orig`,
    /// in seconds): nothing is computed from the previous frame.
    Trim { clip: ClipRef, edge: Edge, press_x: f32, orig: f64 },
    Scrollbar { grab: f32 },
}

/// What a right-click was on.
#[derive(Clone, Copy, Debug)]
enum Target {
    Clip(ClipRef),
    /// The join between a clip and the one before it.
    Join(ClipRef),
    Track(TrackRef),
}

/// A change to one clip's settings.
type ClipChange = Box<dyn FnOnce(&mut Clip)>;

/// Things the menus and the properties strip ask for, done after drawing.
enum Action {
    /// Change a clip: as one undo step, or (`live`) as part of a slider
    /// being dragged.
    Clip { c: ClipRef, label: &'static str, live: bool, f: ClipChange },
    Speed(ClipRef, f64),
    Copy(ClipRef),
    Cut(ClipRef),
    /// Paste on this track (or the copied clip's own), at the playhead.
    Paste(Option<TrackRef>),
    Duplicate(ClipRef),
    DetachAudio(ClipRef),
    /// Open the text window for a clip, or for a new one at a place.
    EditText(ClipRef),
    NewText(Option<(TrackRef, Duration)>),
    Split(ClipRef),
    Delete(ClipRef),
    Ripple(ClipRef),
    ToggleMute(ClipRef),
    Transition(ClipRef, Option<(TransitionKind, Duration)>),
    RemoveTrack(TrackRef),
    ToggleTrackMute(TrackRef),
    ToggleTrackHidden(TrackRef),
}

pub struct TimelineView {
    /// Pixels per second.
    pub zoom: f32,
    /// Seconds at the left edge.
    scroll: f32,
    /// Pixels the tracks are scrolled down (when they don't all fit).
    vscroll: f32,
    pub selected: Option<ClipRef>,
    drag: Option<Drag>,
    target: Option<Target>,
    /// Track rows as drawn last frame, for drops from the file manager.
    rows: Vec<(TrackRef, Rect)>,
    tracks_rect: Rect,
    /// The visible width of the tracks, in pixels.
    width: f32,
    /// Zoom to fit this length once the panel's width is known.
    fit: Option<Duration>,
    /// Where a trim may snap to, taken when the drag starts (the playhead
    /// follows the edge while trimming, so it can't be one of them).
    trim_snaps: Vec<f32>,
    /// Show this time in the player once it has the latest timeline.
    follow: Option<Duration>,
    /// The copied clip.
    clipboard: Option<(Clip, TrackRef)>,
    /// The text window, when open.
    text: Option<TextEditor>,
    /// The time under the pointer when the context menu opened.
    menu_at: Duration,
}

/// The window for writing a text clip.
struct TextEditor {
    /// The clip being changed, or where a new one goes.
    target: TextTarget,
    style: TextStyle,
    focus: bool,
}

enum TextTarget {
    Clip(ClipRef),
    New(TrackRef, Duration),
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            zoom: 40.0,
            scroll: 0.0,
            vscroll: 0.0,
            selected: None,
            drag: None,
            target: None,
            rows: Vec::new(),
            tracks_rect: Rect::NOTHING,
            width: 600.0,
            fit: None,
            trim_snaps: Vec::new(),
            follow: None,
            clipboard: None,
            text: None,
            menu_at: Duration::ZERO,
        }
    }
}

fn secs(d: Duration) -> f32 {
    d.as_secs_f32()
}

fn dur(s: f32) -> Duration {
    Duration::from_secs_f32(s.max(0.0))
}

/// Top to bottom: the highest video track first, then A1, A2…
fn display_order(t: &Timeline) -> Vec<TrackRef> {
    let mut v: Vec<TrackRef> = (0..t.video.len()).rev().map(TrackRef::video).collect();
    v.extend((0..t.audio.len()).map(TrackRef::audio));
    v
}

fn row_height(r: TrackRef) -> f32 {
    match r.kind {
        TrackKind::Video => VIDEO_H,
        TrackKind::Audio => AUDIO_H,
    }
}

impl TimelineView {
    pub fn zoom_in(&mut self, playhead: Duration) {
        self.zoom_around(1.5, secs(playhead), None);
    }

    pub fn zoom_out(&mut self, playhead: Duration) {
        self.zoom_around(1.0 / 1.5, secs(playhead), None);
    }

    /// Show the whole timeline.
    pub fn zoom_fit(&mut self, total: Duration) {
        self.fit = Some(total);
    }

    fn do_fit(&mut self, total: Duration) {
        let total = secs(total).max(5.0);
        self.zoom = ((self.width - 20.0) / total).clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll = 0.0;
    }

    /// Zoom by `factor`, keeping time `t` where it is on screen (at `x`
    /// pixels from the left; the middle when `None`).
    fn zoom_around(&mut self, factor: f32, t: f32, x: Option<f32>) {
        let x = x.unwrap_or_else(|| ((t - self.scroll) * self.zoom).clamp(0.0, self.width));
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.scroll = (t - x / self.zoom).max(0.0);
    }

    /// Where a file dropped at `pos` goes: the track under it and the time.
    pub fn drop_target(&self, pos: Pos2) -> Option<(TrackRef, Duration)> {
        let (r, _) = self.rows.iter().find(|(_, rect)| rect.contains(pos))?;
        Some((*r, dur(self.scroll + (pos.x - self.tracks_rect.left()) / self.zoom)))
    }

    /// Keep the selection pointing at a clip that exists.
    pub fn fix_selection(&mut self, t: &Timeline) {
        if self.selected.is_some_and(|c| t.clip(c).is_none()) {
            self.selected = None;
        }
    }

    /// The toolbar row and the selected clip's properties.
    pub fn toolbar(&mut self, ui: &mut egui::Ui, theme: Theme, project: &mut Project, player: &mut Player) {
        self.fix_selection(project.timeline());
        let mut action = None;
        ui.horizontal(|ui| {
            let sel = self.selected;
            let at = player.position();
            if tool_button(ui, theme, "✂ Split", true, "Split at the playhead (S)") {
                project.edit("Split", |t| match sel {
                    Some(c) if t.clip(c).is_some_and(|k| k.start < at && at < k.end()) => {
                        t.split(c, at);
                    }
                    _ => {
                        t.split_all(at);
                    }
                });
            }
            if tool_button(ui, theme, "Delete", sel.is_some(), "Remove the clip, leaving a gap (Del)") {
                action = sel.map(Action::Delete);
            }
            if tool_button(ui, theme, "Ripple Delete", sel.is_some(), "Remove the clip and close the gap (Shift+Del)") {
                action = sel.map(Action::Ripple);
            }
            ui.separator();
            if tool_button(ui, theme, "+ Video Track", true, "Add a video track over the others") {
                project.edit("Add Video Track", |t| t.add_track(TrackKind::Video));
            }
            if tool_button(ui, theme, "+ Audio Track", true, "Add an audio track for music or voice") {
                project.edit("Add Audio Track", |t| t.add_track(TrackKind::Audio));
            }
            ui.separator();
            if tool_button(ui, theme, "−", true, "Zoom out (-)") {
                self.zoom_out(at);
            }
            if tool_button(ui, theme, "+", true, "Zoom in (+)") {
                self.zoom_in(at);
            }
            if tool_button(ui, theme, "Fit", true, "Show the whole timeline") {
                self.zoom_fit(project.timeline().duration());
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            match self.selected {
                Some(c) => self.properties(ui, c, project, &mut action),
                None => {
                    ui.label(egui::RichText::new("Select a clip to change its sound, fades and transition.").weak());
                }
            }
        });
        if let Some(a) = action {
            self.apply(a, project, player.position());
        }
    }

    fn properties(&mut self, ui: &mut egui::Ui, c: ClipRef, project: &mut Project, action: &mut Option<Action>) {
        let Some(clip) = project.timeline().clip(c).cloned() else { return };
        let track = project.timeline().track(c.track).cloned();
        ui.label(egui::RichText::new(format!("{}: {}", c.track.name(), clip.name())).strong());
        ui.separator();
        if clip.has_audio {
            let mut vol = clip.volume * 100.0;
            ui.label("Volume");
            let r = ui.add(egui::Slider::new(&mut vol, 0.0..=200.0).step_by(5.0).fixed_decimals(0).suffix("%"));
            if r.changed() {
                project.live("Volume", |t| {
                    if let Some(k) = t.clip_mut(c) {
                        k.volume = vol / 100.0;
                    }
                });
            }
            let mut muted = clip.muted;
            if ui.checkbox(&mut muted, "Mute").changed() {
                *action = Some(Action::ToggleMute(c));
            }
            ui.separator();
        }
        let len = secs(clip.len());
        let mut fade_in = secs(clip.fade_in);
        let mut fade_out = secs(clip.fade_out);
        ui.label("Fade in");
        if seconds(ui, &mut fade_in, 0.0..=len).changed() {
            project.live("Fade In", |t| {
                if let Some(k) = t.clip_mut(c) {
                    k.fade_in = dur(fade_in).min(k.len());
                }
            });
        }
        ui.label("out");
        if seconds(ui, &mut fade_out, 0.0..=len).changed() {
            project.live("Fade Out", |t| {
                if let Some(k) = t.clip_mut(c) {
                    k.fade_out = dur(fade_out).min(k.len());
                }
            });
        }
        if clip.is_still() {
            let mut l = len;
            ui.label("Length");
            if seconds(ui, &mut l, 0.1..=3600.0).changed() {
                project.live("Picture Length", |t| t.trim(c, Edge::End, clip.start + dur(l)));
            }
        }
        if let Some(track) = track.filter(|t| t.joined(c.index)) {
            ui.separator();
            let ov = track.overlap_before(c.index);
            let current = (!ov.is_zero()).then_some(clip.transition);
            ui.label("From previous");
            let name = |k: Option<TransitionKind>| match k {
                None => "Cut",
                Some(TransitionKind::Crossfade) => "Cross-fade",
                Some(TransitionKind::DipToBlack) => "Dip to black",
            };
            let mut choice = current;
            egui::ComboBox::from_id_salt("transition").selected_text(name(current)).show_ui(ui, |ui| {
                for k in [None, Some(TransitionKind::Crossfade), Some(TransitionKind::DipToBlack)] {
                    ui.selectable_value(&mut choice, k, name(k));
                }
            });
            if choice != current {
                let d = if ov.is_zero() { Duration::from_secs(1) } else { ov };
                *action = Some(Action::Transition(c, choice.map(|k| (k, d))));
            }
            if let Some(kind) = current {
                let mut d = secs(ov);
                if seconds(ui, &mut d, 0.1..=10.0).changed() {
                    project.live("Transition Length", |t| t.set_transition(c, Some((kind, dur(d)))));
                }
            }
        }
    }

    fn apply(&mut self, a: Action, project: &mut Project, playhead: Duration) {
        match a {
            Action::Clip { c, label, live, f } => {
                let change = |t: &mut Timeline| {
                    if let Some(k) = t.clip_mut(c) {
                        f(k);
                    }
                };
                if live {
                    project.live(label, change);
                } else {
                    project.edit(label, change);
                }
            }
            Action::Speed(c, speed) => {
                project.edit("Speed", |t| t.set_speed(c, speed));
            }
            Action::Copy(c) => {
                self.clipboard = project.timeline().clip(c).map(|k| (k.clone(), c.track));
            }
            Action::Cut(c) => {
                self.clipboard = project.timeline().clip(c).map(|k| (k.clone(), c.track));
                project.edit("Cut", |t| t.remove(c));
                self.selected = None;
            }
            Action::Paste(track) => {
                let Some((clip, from)) = self.clipboard.clone() else { return };
                let mut clip = clip;
                clip.transition = TransitionKind::default();
                let t = project.timeline();
                let fits = |r: TrackRef| t.track(r).is_some_and(|tr| tr.accepts(&clip));
                let on = [track, self.selected.map(|c| c.track), Some(from)].into_iter().flatten().find(|r| fits(*r));
                let on = on.or_else(|| t.tracks().map(|(r, _)| r).find(|r| fits(*r)));
                if let Some(r) = on {
                    if let Some(c) = project.edit("Paste", |t| t.add_clip(r, clip, playhead)) {
                        self.selected = Some(c);
                    }
                }
            }
            Action::Duplicate(c) => {
                if let Some(d) = project.edit("Duplicate", |t| t.duplicate(c)) {
                    self.selected = Some(d);
                }
            }
            Action::DetachAudio(c) => {
                if let Some(d) = project.edit("Detach Audio", |t| t.detach_audio(c)) {
                    self.selected = Some(d);
                }
            }
            Action::EditText(c) => {
                if let Some(style) = project.timeline().clip(c).and_then(|k| k.text.clone()) {
                    self.text = Some(TextEditor { target: TextTarget::Clip(c), style, focus: true });
                }
            }
            Action::NewText(at) => {
                let (r, time) = at.unwrap_or_else(|| (text_track(project.timeline(), playhead), playhead));
                let style = TextStyle { text: "Title".into(), ..TextStyle::default() };
                self.text = Some(TextEditor { target: TextTarget::New(r, time), style, focus: true });
            }
            Action::Split(c) => {
                if let Some(r) = project.edit("Split", |t| t.split(c, playhead)) {
                    self.selected = Some(r);
                }
            }
            Action::Delete(c) => {
                project.edit("Delete", |t| t.remove(c));
                self.selected = None;
            }
            Action::Ripple(c) => {
                project.edit("Ripple Delete", |t| t.ripple_remove(c));
                self.selected = None;
            }
            Action::ToggleMute(c) => project.edit("Mute", |t| {
                if let Some(k) = t.clip_mut(c) {
                    k.muted = !k.muted;
                }
            }),
            Action::Transition(c, tr) => {
                let label = if tr.is_some() { "Transition" } else { "Cut" };
                project.edit(label, |t| t.set_transition(c, tr));
            }
            Action::RemoveTrack(r) => {
                project.edit("Remove Track", |t| t.remove_track(r));
                self.selected = None;
            }
            Action::ToggleTrackMute(r) => project.edit("Mute Track", |t| {
                if let Some(tr) = t.track_mut(r) {
                    tr.muted = !tr.muted;
                }
            }),
            Action::ToggleTrackHidden(r) => project.edit("Hide Track", |t| {
                if let Some(tr) = t.track_mut(r) {
                    tr.hidden = !tr.hidden;
                }
            }),
        }
    }

    /// Delete / ripple-delete / split from the keyboard.
    pub fn delete_selected(&mut self, project: &mut Project, ripple: bool) {
        if let Some(c) = self.selected {
            self.apply(if ripple { Action::Ripple(c) } else { Action::Delete(c) }, project, Duration::ZERO);
        }
    }

    /// Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+D.
    pub fn copy_selected(&mut self, project: &mut Project, cut: bool) {
        if let Some(c) = self.selected {
            self.apply(if cut { Action::Cut(c) } else { Action::Copy(c) }, project, Duration::ZERO);
        }
    }

    pub fn paste(&mut self, project: &mut Project, at: Duration) {
        self.apply(Action::Paste(None), project, at);
    }

    pub fn can_paste(&self) -> bool {
        self.clipboard.is_some()
    }

    pub fn duplicate_selected(&mut self, project: &mut Project) {
        if let Some(c) = self.selected {
            self.apply(Action::Duplicate(c), project, Duration::ZERO);
        }
    }

    /// Edit > Add Text: a new title at the playhead.
    pub fn add_text(&mut self, project: &mut Project, at: Duration) {
        self.apply(Action::NewText(None), project, at);
    }

    /// Once the player has the timeline as it is now: show the frame the
    /// last drag asked for.
    pub fn after_sync(&mut self, player: &mut Player) {
        if let Some(t) = self.follow.take() {
            player.seek(t);
        }
    }

    /// The text window, if open.
    pub fn windows(&mut self, ctx: &egui::Context, theme: Theme, project: &mut Project) {
        let Some(ed) = &mut self.text else { return };
        let mut done: Option<bool> = None;
        let title = match ed.target {
            TextTarget::Clip(_) => "Edit Text",
            TextTarget::New(..) => "Add Text",
        };
        crate::app::window(ctx, theme, title, |ui| {
            let r = ui.add(egui::TextEdit::multiline(&mut ed.style.text).desired_rows(3).desired_width(320.0).hint_text("Text"));
            if std::mem::take(&mut ed.focus) {
                r.request_focus();
            }
            egui::Grid::new("text-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Size");
                let mut pct = ed.style.size * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 2.0..=30.0).suffix("% of height").fixed_decimals(0)).changed() {
                    ed.style.size = pct / 100.0;
                }
                ui.end_row();
                ui.label("Colour");
                ui.horizontal(|ui| {
                    for (rgb, name) in [([255, 255, 255], "White"), ([255, 214, 0], "Yellow"), ([0, 0, 0], "Black"), ([230, 40, 40], "Red"), ([60, 200, 255], "Blue")] {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
                        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                        let on = ed.style.color == rgb;
                        ui.painter().rect_stroke(rect, 2.0, Stroke::new(if on { 2.0 } else { 1.0 }, ui.visuals().text_color()), StrokeKind::Outside);
                        if resp.on_hover_text(name).clicked() {
                            ed.style.color = rgb;
                        }
                    }
                    ui.color_edit_button_srgb(&mut ed.style.color);
                });
                ui.end_row();
                ui.label("Place");
                ui.horizontal(|ui| {
                    use flicker_core::timeline::TextPlace;
                    ui.radio_value(&mut ed.style.place, TextPlace::Top, "Top");
                    ui.radio_value(&mut ed.style.place, TextPlace::Middle, "Middle");
                    ui.radio_value(&mut ed.style.place, TextPlace::Bottom, "Bottom");
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut ed.style.boxed, "Dark box behind the text");
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ok = !ed.style.text.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("OK").min_size(egui::vec2(72.0, 0.0))).clicked() {
                    done = Some(true);
                }
                if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(72.0, 0.0))).clicked() {
                    done = Some(false);
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = Some(false);
            }
        });
        match done {
            Some(true) => {
                let Some(ed) = self.text.take() else { return };
                match ed.target {
                    TextTarget::Clip(c) => project.edit("Edit Text", |t| {
                        if let Some(k) = t.clip_mut(c) {
                            k.text = Some(ed.style);
                        }
                    }),
                    TextTarget::New(r, at) => {
                        let placed = project.edit("Add Text", |t| {
                            while t.video.len() <= r.index {
                                t.add_track(TrackKind::Video);
                            }
                            t.add_clip(r, Clip::text(ed.style), at)
                        });
                        if let Some(c) = placed {
                            self.selected = Some(c);
                        }
                    }
                }
            }
            Some(false) => self.text = None,
            None => {}
        }
    }

    /// Split the selected clip at the playhead, or every clip under it
    /// when nothing (under it) is selected.
    pub fn split(&mut self, project: &mut Project, at: Duration) {
        let sel = self.selected.filter(|c| project.timeline().clip(*c).is_some_and(|k| k.start < at && at < k.end()));
        match sel {
            Some(c) => self.apply(Action::Split(c), project, at),
            None => {
                project.edit("Split", |t| t.split_all(at));
            }
        }
    }

    /// The tracks: headers, ruler, clips, playhead and scrollbar.
    pub fn tracks(&mut self, ui: &mut egui::Ui, theme: Theme, project: &mut Project, player: &mut Player) {
        self.fix_selection(project.timeline());
        let full = ui.available_rect_before_wrap();
        ui.advance_cursor_after_rect(full);
        let header = Rect::from_min_max(full.min, egui::pos2(full.left() + HEADER_W, full.bottom() - SCROLLBAR_H));
        let ruler = Rect::from_min_max(egui::pos2(header.right(), full.top()), egui::pos2(full.right(), full.top() + RULER_H));
        let area = Rect::from_min_max(egui::pos2(header.right(), ruler.bottom()), egui::pos2(full.right(), full.bottom() - SCROLLBAR_H));
        let scrollbar = Rect::from_min_max(egui::pos2(area.left(), area.bottom()), full.right_bottom());
        self.width = area.width().max(50.0);
        self.tracks_rect = area;
        if let Some(total) = self.fit.take() {
            self.do_fit(total);
        }

        let total = project.timeline().duration();
        let order = display_order(project.timeline());
        let content_h: f32 = order.iter().map(|r| row_height(*r)).sum();
        let max_v = (content_h - area.height()).max(0.0);
        self.vscroll = self.vscroll.clamp(0.0, max_v);
        let mut rows = Vec::new();
        let mut y = area.top() - self.vscroll;
        for r in &order {
            let h = row_height(*r);
            rows.push((*r, Rect::from_min_max(egui::pos2(area.left(), y), egui::pos2(area.right(), y + h))));
            y += h;
        }
        self.rows = rows.iter().filter(|(_, rect)| rect.intersects(area)).copied().collect();

        // Wheel: Ctrl zooms around the pointer; otherwise scroll.
        let hovered = ui.rect_contains_pointer(full);
        if hovered {
            let (delta, zoom, shift) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift));
            let pointer_x = ui.input(|i| i.pointer.hover_pos()).map(|p| (p.x - area.left()).max(0.0));
            if zoom != 1.0 {
                // Ctrl+wheel: zoom around the pointer.
                let x = pointer_x.unwrap_or(self.width / 2.0);
                let t = self.scroll + x / self.zoom;
                self.zoom_around(zoom, t, Some(x));
            } else if delta != egui::Vec2::ZERO {
                let (mut dx, mut dy) = (delta.x, delta.y);
                if shift || max_v == 0.0 {
                    dx += dy;
                    dy = 0.0;
                }
                self.scroll = (self.scroll - dx / self.zoom).max(0.0);
                self.vscroll = (self.vscroll - dy).clamp(0.0, max_v);
            }
        }

        // Follow the playhead while playing.
        let playhead = secs(player.position());
        if player.is_playing() && self.drag.is_none() {
            let x = (playhead - self.scroll) * self.zoom;
            if x > self.width - 10.0 || x < 0.0 {
                self.scroll = (playhead - self.width * 0.1 / self.zoom).max(0.0);
            }
        }

        let x_of = |s: &Self, t: f32| area.left() + (t - s.scroll) * s.zoom;
        let t_of = |s: &Self, x: f32| (s.scroll + (x - area.left()) / s.zoom).max(0.0);

        // ---- Interaction ------------------------------------------------
        let id = egui::Id::unique("timeline");
        let resp = ui.interact(ruler.union(area), id, Sense::click_and_drag());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let hit = |s: &Self, t: &Timeline, pos: Pos2| -> Option<(ClipRef, Option<Edge>)> {
            let (r, row) = s.rows.iter().find(|(_, rect)| rect.contains(pos))?;
            let track = t.track(*r)?;
            for (i, c) in track.clips.iter().enumerate().rev() {
                let (x0, x1) = (x_of(s, secs(c.start)), x_of(s, secs(c.end())));
                if pos.x < x0 - 2.0 || pos.x > x1 + 2.0 || !row.contains(pos) {
                    continue;
                }
                let grab = EDGE_GRAB.min((x1 - x0) / 3.0);
                let edge = if pos.x <= x0 + grab {
                    Some(Edge::Start)
                } else if pos.x >= x1 - grab {
                    Some(Edge::End)
                } else {
                    None
                };
                return Some((ClipRef { track: *r, index: i }, edge));
            }
            None
        };

        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            self.drag = origin.map(|p| {
                if ruler.contains(p) {
                    return Drag::Scrub;
                }
                match hit(self, project.timeline(), p) {
                    Some((c, Some(edge))) => {
                        self.selected = Some(c);
                        player.pause();
                        project.finish();
                        let k = project.timeline().clip(c);
                        let orig = k.map_or(0.0, |k| match edge {
                            Edge::Start => k.start.as_secs_f64(),
                            Edge::End => k.end().as_secs_f64(),
                        });
                        // The playhead sits on (or a frame before) an edge
                        // just trimmed: it isn't a place to snap back to.
                        let near_edge = (playhead as f64 - orig).abs() <= (SNAP / self.zoom) as f64;
                        let ph = if near_edge { 0.0 } else { playhead };
                        self.trim_snaps = snap_points(project.timeline(), Some(c), ph);
                        Drag::Trim { clip: c, edge, press_x: p.x, orig }
                    }
                    Some((c, None)) => {
                        self.selected = Some(c);
                        let start = project.timeline().clip(c).map_or(0.0, |k| secs(k.start));
                        Drag::Move { from: c, grab: dur(t_of(self, p.x) - start), to: None }
                    }
                    None => Drag::Scrub,
                }
            });
        }
        let snap_tol = SNAP / self.zoom;
        let free = ui.input(|i| i.modifiers.shift);
        let mut readout: Option<(f32, String)> = None;
        let mut snapped_at: Option<f32> = None;
        if let (Some(drag), Some(p)) = (self.drag, pointer) {
            match drag {
                Drag::Scrub => player.seek(dur(t_of(self, p.x))),
                Drag::Trim { clip, edge, press_x, orig } => {
                    let snaps: &[f32] = if free { &[] } else { &self.trim_snaps };
                    let (t, snapped) = trim_target(orig, press_x, p.x, self.zoom, snaps, snap_tol);
                    snapped_at = snapped.then_some(secs(t));
                    project.gesture("Trim", |tl| tl.trim(clip, edge, t));
                    if let Some(k) = project.timeline().clip(clip) {
                        let (at, show) = match edge {
                            Edge::Start => (k.start, k.start),
                            Edge::End => (k.end(), k.end().saturating_sub(player.frame_len())),
                        };
                        // Shown once the player has this version.
                        self.follow = Some(show);
                        readout = Some((secs(at), format!("{}   length {}", format_timestamp(at), format_timestamp(k.len()))));
                    }
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                Drag::Move { from, grab, .. } => {
                    let t = project.timeline();
                    if let Some(clip) = t.clip(from) {
                        let len = secs(clip.len());
                        // The track under the pointer, if the clip can go there.
                        let to_track = self
                            .rows
                            .iter()
                            .find(|(r, rect)| rect.y_range().contains(p.y) && r.kind == from.track.kind)
                            .map(|(r, _)| *r)
                            .filter(|r| t.track(*r).is_some_and(|tr| tr.accepts(clip)))
                            .unwrap_or(from.track);
                        let points = snap_points(t, Some(from), playhead);
                        let start = t_of(self, p.x) - secs(grab);
                        let by_start = snap(start, &points, snap_tol);
                        let by_end = snap(start + len, &points, snap_tol) - len;
                        let start = if (by_end - start).abs() > 0.0 && (by_start - start).abs() == 0.0 { by_end } else { by_start };
                        let track = t.track(to_track).expect("track exists");
                        let exclude = (to_track == from.track).then_some(from.index);
                        let landing = track.fit(clip.len(), dur(start), exclude);
                        self.drag = Some(Drag::Move { from, grab, to: Some((to_track, landing)) });
                        readout = Some((secs(landing), format!("{}  {}", to_track.name(), format_timestamp(landing))));
                    }
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                }
                Drag::Scrollbar { .. } => {}
            }
        }
        if resp.drag_stopped() {
            match self.drag.take() {
                Some(Drag::Move { from, to: Some((track, at)), .. }) => {
                    let unchanged = project.timeline().clip(from).is_some_and(|k| k.start == at) && track == from.track;
                    if !unchanged {
                        if let Some(c) = project.edit("Move", |t| t.move_clip(from, track, at)) {
                            self.selected = Some(c);
                        }
                    }
                }
                Some(Drag::Trim { .. }) => project.finish(),
                _ => {}
            }
        }
        if resp.clicked() {
            if let Some(p) = pointer {
                match (ruler.contains(p), hit(self, project.timeline(), p)) {
                    (false, Some((c, _))) => self.selected = Some(c),
                    _ => {
                        self.selected = None;
                        player.seek(dur(t_of(self, p.x)));
                    }
                }
            }
        }
        if resp.secondary_clicked() {
            self.menu_at = pointer.map_or(Duration::ZERO, |p| dur(t_of(self, p.x)));
            self.target = pointer.and_then(|p| {
                let (r, _) = self.rows.iter().find(|(_, rect)| rect.contains(p))?;
                let tol = dur(EDGE_GRAB / self.zoom);
                if let Some(i) = project.timeline().join_near(*r, dur(t_of(self, p.x)), tol) {
                    return Some(Target::Join(ClipRef { track: *r, index: i }));
                }
                match hit(self, project.timeline(), p) {
                    Some((c, _)) => {
                        self.selected = Some(c);
                        Some(Target::Clip(c))
                    }
                    None => Some(Target::Track(*r)),
                }
            });
        }
        if self.drag.is_none() {
            if let Some(p) = resp.hover_pos() {
                if !ruler.contains(p) {
                    match hit(self, project.timeline(), p) {
                        Some((_, Some(_))) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                        Some((_, None)) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                        None => {}
                    }
                }
            }
        }

        let mut action = None;
        let target = self.target;
        let tl = project.timeline().clone();
        let at = player.position();
        let can_paste = self.clipboard.is_some();
        let menu_at = self.menu_at;
        resp.context_menu(|ui| match target {
            Some(Target::Clip(c)) => clip_menu(ui, &tl, c, at, can_paste, &mut action),
            Some(Target::Join(c)) => join_menu(ui, &tl, c, &mut action),
            Some(Target::Track(r)) => track_menu(ui, &tl, r, menu_at, can_paste, &mut action),
            None => {
                ui.close();
            }
        });
        if let Some(a) = action.take() {
            self.apply(a, project, at);
        }

        // ---- Drawing ----------------------------------------------------
        let t = project.timeline();
        let painter = ui.painter_at(full);
        let win95 = theme.is_win95();
        let visuals = ui.visuals().clone();
        let (bg, bg_alt, grid) = if win95 {
            (Color32::from_gray(0x80), Color32::from_gray(0x88), Color32::from_gray(0x70))
        } else if visuals.dark_mode {
            (Color32::from_gray(28), Color32::from_gray(34), Color32::from_gray(50))
        } else {
            (Color32::from_gray(218), Color32::from_gray(228), Color32::from_gray(200))
        };
        painter.rect_filled(area, 0.0, bg);
        let tracks_painter = painter.with_clip_rect(area);
        for (k, (r, row)) in rows.iter().enumerate() {
            tracks_painter.rect_filled(*row, 0.0, if k % 2 == 0 { bg } else { bg_alt });
            tracks_painter.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, grid));
            if let Some(track) = t.track(*r) {
                let moving = match self.drag {
                    Some(Drag::Move { from, to: Some(_), .. }) if from.track == *r => Some(from.index),
                    _ => None,
                };
                for (i, c) in track.clips.iter().enumerate() {
                    let rect = Rect::from_x_y_ranges(x_of(self, secs(c.start))..=x_of(self, secs(c.end())), row.shrink(3.0).y_range());
                    if rect.right() < area.left() || rect.left() > area.right() {
                        continue;
                    }
                    let selected = self.selected == Some(ClipRef { track: *r, index: i });
                    let dim = moving == Some(i) || track.hidden || (track.muted && r.kind == TrackKind::Audio);
                    draw_clip(&tracks_painter, theme, rect, c, *r, selected, dim, track.overlap_before(i), self.zoom);
                }
            }
        }
        // The clip being moved, where it will land.
        if let Some(Drag::Move { from, to: Some((track, at)), .. }) = self.drag {
            if let (Some(c), Some((_, row))) = (t.clip(from), rows.iter().find(|(r, _)| *r == track)) {
                let x0 = x_of(self, secs(at));
                let rect = Rect::from_x_y_ranges(x0..=x0 + secs(c.len()) * self.zoom, row.shrink(3.0).y_range());
                let mut ghost = c.clone();
                ghost.start = at;
                draw_clip(&tracks_painter, theme, rect, &ghost, track, true, false, Duration::ZERO, self.zoom);
            }
        }
        if ui.input(|i| !i.raw.hovered_files.is_empty()) {
            tracks_painter.rect_filled(area, 0.0, Color32::from_black_alpha(90));
            tracks_painter.text(area.center(), egui::Align2::CENTER_CENTER, "Drop on a track to add it there", egui::FontId::proportional(15.0), Color32::WHITE);
        }
        theme.bevel(&painter, area.expand(1.0), true);

        // Ruler.
        let face = if win95 { theme::FACE } else { visuals.panel_fill };
        let ink = visuals.text_color();
        painter.rect_filled(ruler, 0.0, face);
        let ruler_painter = painter.with_clip_rect(ruler);
        let step = tick_step(self.zoom);
        let first = (self.scroll / step).floor() as i64;
        let last = ((self.scroll + self.width / self.zoom) / step).ceil() as i64;
        for k in first..=last {
            let t0 = k as f32 * step;
            let x = x_of(self, t0);
            ruler_painter.line_segment([egui::pos2(x, ruler.bottom() - 8.0), egui::pos2(x, ruler.bottom())], Stroke::new(1.0, ink));
            ruler_painter.text(
                egui::pos2(x + 3.0, ruler.top() + 2.0),
                egui::Align2::LEFT_TOP,
                ruler_label(t0, step),
                egui::FontId::proportional(11.0),
                ink,
            );
            for m in 1..5 {
                let xm = x_of(self, t0 + step * m as f32 / 5.0);
                ruler_painter.line_segment([egui::pos2(xm, ruler.bottom() - 3.0), egui::pos2(xm, ruler.bottom())], Stroke::new(1.0, ink));
            }
        }
        // The end of the timeline.
        if !total.is_zero() {
            let xe = x_of(self, secs(total));
            tracks_painter.line_segment([egui::pos2(xe, area.top()), egui::pos2(xe, area.bottom())], Stroke::new(1.0, Color32::from_black_alpha(120)));
        }
        theme.bevel(&painter, ruler, false);

        // Playhead.
        let xp = x_of(self, playhead);
        if (area.left()..=area.right()).contains(&xp) {
            let red = Color32::from_rgb(0xE0, 0x20, 0x20);
            painter.line_segment([egui::pos2(xp, ruler.top()), egui::pos2(xp, area.bottom())], Stroke::new(1.5, red));
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(xp - 6.0, ruler.top()), egui::pos2(xp + 6.0, ruler.top()), egui::pos2(xp, ruler.top() + 9.0)],
                red,
                Stroke::NONE,
            ));
        }

        // While dragging: where it snapped, and the time.
        if let Some(t0) = snapped_at {
            let x = x_of(self, t0);
            tracks_painter.line_segment([egui::pos2(x, area.top()), egui::pos2(x, area.bottom())], Stroke::new(1.0, Color32::from_rgb(0xFF, 0xD4, 0x00)));
        }
        if let (Some((t0, text)), Some(p)) = (readout, pointer) {
            let x = x_of(self, t0).clamp(area.left() + 4.0, area.right() - 4.0);
            let font = egui::FontId::monospace(11.0);
            let galley = painter.layout_no_wrap(text, font, Color32::WHITE);
            let size = galley.size() + egui::vec2(8.0, 4.0);
            let mut r = Rect::from_min_size(egui::pos2(x + 6.0, (p.y - size.y - 10.0).max(area.top() + 1.0)), size);
            if r.right() > area.right() {
                r = r.translate(egui::vec2(-(size.x + 12.0), 0.0));
            }
            painter.rect_filled(r, 2.0, Color32::from_black_alpha(200));
            painter.galley(r.min + egui::vec2(4.0, 2.0), galley, Color32::WHITE);
        }

        // Track headers.
        painter.rect_filled(Rect::from_min_max(full.min, egui::pos2(header.right(), ruler.bottom())), 0.0, face);
        painter.text(egui::pos2(full.left() + 6.0, full.top() + 4.0), egui::Align2::LEFT_TOP, format_timestamp(player.position()), egui::FontId::monospace(10.0), ink);
        let mut header_ui = ui.new_child(egui::UiBuilder::new().max_rect(header));
        header_ui.set_clip_rect(Rect::from_min_max(egui::pos2(header.left(), area.top()), header.max));
        for (r, row) in &rows {
            let rect = Rect::from_x_y_ranges(header.x_range(), row.y_range());
            header_ui.painter().rect_filled(rect, 0.0, face);
            theme.bevel(header_ui.painter(), rect.shrink(1.0), false);
            header_ui.painter().text(rect.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, r.name(), egui::FontId::proportional(13.0), ink);
            let Some(track) = t.track(*r) else { continue };
            let mut x = rect.right() - 4.0;
            let mut small = |label: &str, on: bool, tip: &str| {
                let b = Rect::from_min_size(egui::pos2(x - 18.0, rect.center().y - 9.0), egui::vec2(18.0, 18.0));
                x -= 20.0;
                let resp = header_ui.put(b, egui::Button::new(egui::RichText::new(label).size(11.0)).selected(on).frame(!win95));
                if win95 {
                    if on {
                        header_ui.painter().rect_filled(b.shrink(2.0), 0.0, Color32::from_gray(0xA0));
                    }
                    theme.bevel(header_ui.painter(), b, on || resp.is_pointer_button_down_on());
                    header_ui.painter().text(b.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.0), ink);
                }
                resp.on_hover_text(tip).clicked()
            };
            if small("M", track.muted, "Mute this track's sound") {
                action = Some(Action::ToggleTrackMute(*r));
            }
            if r.kind == TrackKind::Video && small("H", track.hidden, "Hide this track's picture") {
                action = Some(Action::ToggleTrackHidden(*r));
            }
        }
        if let Some(a) = action.take() {
            self.apply(a, project, at);
        }

        // Scrollbar.
        let extent = (secs(project.timeline().duration()) + 10.0).max(self.scroll + self.width / self.zoom);
        let visible = self.width / self.zoom;
        let bar = scrollbar;
        painter.rect_filled(bar, 0.0, if win95 { Color32::from_gray(0xD8) } else { visuals.extreme_bg_color });
        let thumb_w = (bar.width() * visible / extent).max(20.0).min(bar.width());
        let thumb_x = bar.left() + (bar.width() - thumb_w) * (self.scroll / (extent - visible).max(0.001)).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(egui::pos2(thumb_x, bar.top()), egui::vec2(thumb_w, bar.height()));
        let sresp = ui.interact(bar, id.with("scroll"), Sense::drag());
        if sresp.drag_started() {
            let p = sresp.interact_pointer_pos().unwrap_or(thumb.center());
            let grab = if thumb.contains(p) { p.x - thumb.left() } else { thumb_w / 2.0 };
            self.drag = Some(Drag::Scrollbar { grab });
        }
        if let (Some(Drag::Scrollbar { grab }), Some(p)) = (self.drag, sresp.interact_pointer_pos()) {
            let frac = ((p.x - grab - bar.left()) / (bar.width() - thumb_w).max(1.0)).clamp(0.0, 1.0);
            self.scroll = frac * (extent - visible).max(0.0);
        }
        if sresp.drag_stopped() {
            self.drag = None;
        }
        painter.rect_filled(thumb, 0.0, if win95 { theme::FACE } else { visuals.widgets.inactive.bg_fill });
        theme.bevel(&painter, thumb, false);
    }
}

/// Where a trimmed edge goes: where it was when pressed (`orig` seconds),
/// moved as far as the pointer has moved since (`press_x` → `x` pixels),
/// snapped to the nearest of `snaps` within `tolerance` seconds. Also says
/// whether it snapped.
fn trim_target(orig: f64, press_x: f32, x: f32, zoom: f32, snaps: &[f32], tolerance: f32) -> (Duration, bool) {
    let raw = (orig + (x - press_x) as f64 / zoom as f64).max(0.0);
    let near = snaps
        .iter()
        .map(|&p| p as f64)
        .filter(|p| (p - raw).abs() <= tolerance as f64)
        .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()));
    match near {
        Some(p) => (Duration::from_secs_f64(p.max(0.0)), true),
        None => (Duration::from_secs_f64(raw), false),
    }
}

/// Where edges snap to: the playhead, the start, and every clip's edges.
fn snap_points(t: &Timeline, exclude: Option<ClipRef>, playhead: f32) -> Vec<f32> {
    let mut v = vec![0.0, playhead];
    for (r, track) in t.tracks() {
        for (i, c) in track.clips.iter().enumerate() {
            if Some(ClipRef { track: r, index: i }) != exclude {
                v.push(secs(c.start));
                v.push(secs(c.end()));
            }
        }
    }
    v
}

fn snap(t: f32, points: &[f32], tolerance: f32) -> f32 {
    points
        .iter()
        .copied()
        .filter(|p| (p - t).abs() <= tolerance)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
        .unwrap_or(t)
}

/// Seconds between labelled ticks: at least ~70 pixels apart.
fn tick_step(zoom: f32) -> f32 {
    const STEPS: [f32; 13] = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    STEPS.iter().copied().find(|s| s * zoom >= 70.0).unwrap_or(1800.0)
}

fn ruler_label(t: f32, step: f32) -> String {
    let total = t.round() as u64;
    let (m, s) = (total / 60, total % 60);
    if step < 1.0 {
        let frac = t - t.floor();
        format!("{}:{:02}.{}", (t as u64) / 60, (t as u64) % 60, ((frac * 10.0).round() as u64).min(9))
    } else {
        format!("{m}:{s:02}")
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_clip(p: &egui::Painter, theme: Theme, rect: Rect, c: &Clip, track: TrackRef, selected: bool, dim: bool, overlap: Duration, zoom: f32) {
    let base = match (c.kind, track.kind) {
        (ClipKind::Text, _) => Color32::from_rgb(0xA8, 0x5A, 0x14),
        (ClipKind::Image, _) => Color32::from_rgb(0x7A, 0x4E, 0x9C),
        (_, TrackKind::Audio) | (ClipKind::Audio, _) => Color32::from_rgb(0x2E, 0x7D, 0x3C),
        (ClipKind::Video, _) => Color32::from_rgb(0x1F, 0x5F, 0x9A),
    };
    let fill = if dim { base.gamma_multiply(0.45) } else { base };
    p.rect_filled(rect, 2.0, fill);
    // Fades: shaded corners under a ramp.
    let shade = Color32::from_black_alpha(90);
    let fi = secs(c.fade_in) * zoom;
    if fi > 1.0 {
        p.add(egui::Shape::convex_polygon(vec![rect.left_top(), egui::pos2(rect.left() + fi, rect.top()), rect.left_bottom()], shade, Stroke::NONE));
        p.line_segment([rect.left_bottom(), egui::pos2(rect.left() + fi, rect.top())], Stroke::new(1.0, Color32::WHITE));
    }
    let fo = secs(c.fade_out) * zoom;
    if fo > 1.0 {
        p.add(egui::Shape::convex_polygon(vec![egui::pos2(rect.right() - fo, rect.top()), rect.right_top(), rect.right_bottom()], shade, Stroke::NONE));
        p.line_segment([egui::pos2(rect.right() - fo, rect.top()), rect.right_bottom()], Stroke::new(1.0, Color32::WHITE));
    }
    // A transition from the clip before: crossed lines over the overlap
    // for a cross-fade, a V (down to black and up) for a dip.
    let ov = secs(overlap) * zoom;
    if ov > 1.0 {
        let o = Rect::from_x_y_ranges(rect.left()..=rect.left() + ov, rect.y_range());
        p.rect_filled(o, 0.0, Color32::from_white_alpha(40));
        let s = Stroke::new(1.0, Color32::WHITE);
        match c.transition {
            TransitionKind::Crossfade => {
                p.line_segment([o.left_top(), o.right_bottom()], s);
                p.line_segment([o.left_bottom(), o.right_top()], s);
            }
            TransitionKind::DipToBlack => {
                p.line_segment([o.left_top(), o.center_bottom()], s);
                p.line_segment([o.center_bottom(), o.right_top()], s);
            }
        }
    }
    let mut name = c.name();
    if track.kind == TrackKind::Audio && c.kind == ClipKind::Video {
        name += "  (sound only)";
    }
    if c.muted {
        name += "  (muted)";
    } else if c.has_audio && (c.volume - 1.0).abs() > 0.004 {
        name += &format!("  {:.0}%", c.volume * 100.0);
    }
    if c.speed() != 1.0 {
        name += &format!("  {}×", c.speed());
    }
    if track.kind == TrackKind::Video {
        if !c.transform.is_identity() {
            name += "  ⟲";
        }
        if !c.effects.is_none() {
            name += "  fx";
        }
        if c.opacity < 1.0 {
            name += &format!("  {:.0}% opaque", c.opacity * 100.0);
        }
    }
    let text_rect = rect.shrink2(egui::vec2(4.0, 2.0));
    if text_rect.width() > 8.0 {
        let tp = p.with_clip_rect(text_rect.intersect(p.clip_rect()));
        tp.text(text_rect.left_top(), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(12.0), Color32::WHITE);
        if track.kind == TrackKind::Video && rect.height() > 30.0 {
            tp.text(text_rect.left_bottom(), egui::Align2::LEFT_BOTTOM, format_timestamp(c.len()), egui::FontId::proportional(10.0), Color32::from_white_alpha(200));
        }
    }
    let border = if selected { Stroke::new(2.0, Color32::from_rgb(0xFF, 0xD4, 0x00)) } else { Stroke::new(1.0, Color32::from_black_alpha(160)) };
    p.rect_stroke(rect, 2.0, border, StrokeKind::Inside);
    let _ = theme;
}

fn clip_menu(ui: &mut egui::Ui, t: &Timeline, c: ClipRef, at: Duration, can_paste: bool, action: &mut Option<Action>) {
    let Some(clip) = t.clip(c) else { return };
    let set = |label: &'static str, live: bool, f: ClipChange| Action::Clip { c, label, live, f };
    let under = clip.start < at && at < clip.end();
    if ui.add_enabled(under, egui::Button::new("Split at Playhead").shortcut_text("S")).clicked() {
        *action = Some(Action::Split(c));
    }
    if ui.add(egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
        *action = Some(Action::Cut(c));
    }
    if ui.add(egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
        *action = Some(Action::Copy(c));
    }
    if ui.add_enabled(can_paste, egui::Button::new("Paste at Playhead").shortcut_text("Ctrl+V")).clicked() {
        *action = Some(Action::Paste(Some(c.track)));
    }
    if ui.add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() {
        *action = Some(Action::Duplicate(c));
    }
    if ui.add(egui::Button::new("Delete").shortcut_text("Del")).clicked() {
        *action = Some(Action::Delete(c));
    }
    if ui.add(egui::Button::new("Ripple Delete").shortcut_text("Shift+Del")).clicked() {
        *action = Some(Action::Ripple(c));
    }
    if clip.kind == ClipKind::Text {
        ui.separator();
        if ui.button("Edit Text…").clicked() {
            *action = Some(Action::EditText(c));
        }
    }

    // Sound and speed.
    let sound = clip.has_audio;
    if sound || !clip.is_still() {
        ui.separator();
    }
    if sound {
        ui.menu_button("Volume", |ui| {
            let mut vol = clip.volume * 100.0;
            let r = ui.add(egui::Slider::new(&mut vol, 0.0..=200.0).step_by(5.0).fixed_decimals(0).suffix("%"));
            if r.changed() {
                *action = Some(set("Volume", true, Box::new(move |k| k.volume = vol / 100.0)));
            }
            for v in [0.0f32, 50.0, 100.0, 150.0, 200.0] {
                if ui.radio((clip.volume * 100.0 - v).abs() < 0.5, format!("{v:.0}%")).clicked() {
                    *action = Some(set("Volume", false, Box::new(move |k| k.volume = v / 100.0)));
                }
            }
        });
        if ui.button(if clip.muted { "Unmute" } else { "Mute" }).clicked() {
            *action = Some(Action::ToggleMute(c));
        }
    }
    if !clip.is_still() {
        ui.menu_button("Speed", |ui| {
            for sp in SPEEDS {
                let label = if sp == 1.0 { "Normal (1×)".to_string() } else { format!("{sp}×") };
                if ui.radio((clip.speed() - sp).abs() < 1e-6, label).clicked() {
                    *action = Some(Action::Speed(c, sp));
                }
            }
            ui.separator();
            let mut sp = clip.speed();
            let r = ui.add(egui::DragValue::new(&mut sp).range(MIN_SPEED..=MAX_SPEED).speed(0.01).max_decimals(2).suffix("×"));
            if r.changed() {
                *action = Some(Action::Speed(c, sp));
            }
        });
    }
    let detachable = c.track.kind == TrackKind::Video && clip.kind == ClipKind::Video && clip.has_audio && !clip.muted;
    if detachable && ui.button("Detach Audio").on_hover_text("Put the sound on an audio track of its own").clicked() {
        *action = Some(Action::DetachAudio(c));
    }

    // The picture.
    if c.track.kind == TrackKind::Video && clip.has_picture() {
        ui.separator();
        if clip.has_file() {
            ui.menu_button("Flip / Rotate", |ui| {
                let tr = clip.transform;
                let mut item = |ui: &mut egui::Ui, text: &str, label: &'static str, f: fn(flicker_core::timeline::Transform) -> flicker_core::timeline::Transform| {
                    if ui.button(text).clicked() {
                        *action = Some(set(label, false, Box::new(move |k| k.transform = f(k.transform))));
                    }
                };
                item(ui, "Flip Horizontally", "Flip Horizontally", |t| t.flipped_h());
                item(ui, "Flip Vertically", "Flip Vertically", |t| t.flipped_v());
                ui.separator();
                item(ui, "Rotate 90° Right", "Rotate", |t| t.rotated_cw());
                item(ui, "Rotate 90° Left", "Rotate", |t| t.rotated_ccw());
                item(ui, "Rotate 180°", "Rotate", |t| t.rotated_180());
                ui.separator();
                if ui.add_enabled(!tr.is_identity(), egui::Button::new("Reset")).clicked() {
                    *action = Some(set("Reset Rotation", false, Box::new(|k| k.transform = Default::default())));
                }
            });
        }
        effects_menu(ui, clip, &set, action);
        ui.menu_button("Opacity", |ui| {
            let mut o = clip.opacity * 100.0;
            if ui.add(egui::Slider::new(&mut o, 0.0..=100.0).fixed_decimals(0).suffix("%")).changed() {
                *action = Some(set("Opacity", true, Box::new(move |k| k.opacity = o / 100.0)));
            }
        });
        if clip.has_file() {
            ui.menu_button("Crop", |ui| {
                let cr = clip.crop;
                let mut side = |ui: &mut egui::Ui, name: &str, v: f32, put: fn(&mut Crop, f32)| {
                    let mut pct = v * 100.0;
                    ui.horizontal(|ui| {
                        ui.add_sized([48.0, 18.0], egui::Label::new(name));
                        if ui.add(egui::Slider::new(&mut pct, 0.0..=Crop::MAX * 100.0).fixed_decimals(0).suffix("%")).changed() {
                            *action = Some(set("Crop", true, Box::new(move |k| put(&mut k.crop, pct / 100.0))));
                        }
                    });
                };
                side(ui, "Left", cr.left, |c, v| c.left = v);
                side(ui, "Right", cr.right, |c, v| c.right = v);
                side(ui, "Top", cr.top, |c, v| c.top = v);
                side(ui, "Bottom", cr.bottom, |c, v| c.bottom = v);
                if ui.add_enabled(!cr.is_none(), egui::Button::new("Reset")).clicked() {
                    *action = Some(set("Reset Crop", false, Box::new(|k| k.crop = Crop::default())));
                }
            });
        }
    }
    if t.track(c.track).is_some_and(|tr| tr.joined(c.index)) {
        ui.separator();
        ui.menu_button("Transition from Previous", |ui| join_menu(ui, t, c, action));
    }
}

/// Effects ▸ Motion / Entrance / Exit.
fn effects_menu(ui: &mut egui::Ui, clip: &Clip, set: &dyn Fn(&'static str, bool, ClipChange) -> Action, action: &mut Option<Action>) {
    let fx = clip.effects;
    ui.menu_button("Effects", |ui| {
        ui.menu_button("Motion", |ui| {
            if ui.radio(fx.motion.is_none(), "None").clicked() {
                *action = Some(set("Motion", false, Box::new(|k| k.effects.motion = None)));
            }
            for m in Motion::ALL {
                if ui.radio(fx.motion == Some(m), m.name()).clicked() {
                    *action = Some(set("Motion", false, Box::new(move |k| k.effects.motion = Some(m))));
                }
            }
        });
        for entering in [true, false] {
            let (title, current, fade) = if entering {
                ("Entrance", fx.enter, clip.fade_in)
            } else {
                ("Exit", fx.exit, clip.fade_out)
            };
            let len = if entering { fx.enter_len } else { fx.exit_len };
            ui.menu_button(title, |ui| {
                let put = move |k: &mut Clip, a: Option<Anim>, fade: bool| {
                    let d = if fade { len.min(k.len()) } else { Duration::ZERO };
                    if entering {
                        k.effects.enter = a;
                        k.fade_in = d;
                    } else {
                        k.effects.exit = a;
                        k.fade_out = d;
                    }
                };
                if ui.radio(current.is_none() && fade.is_zero(), "None").clicked() {
                    *action = Some(set(title, false, Box::new(move |k| put(k, None, false))));
                }
                if ui.radio(current.is_none() && !fade.is_zero(), if entering { "Fade In" } else { "Fade Out" }).clicked() {
                    *action = Some(set(title, false, Box::new(move |k| put(k, None, true))));
                }
                for a in Anim::ALL {
                    if ui.radio(current == Some(a), a.name(entering)).clicked() {
                        *action = Some(set(title, false, Box::new(move |k| put(k, Some(a), false))));
                    }
                }
                ui.separator();
                ui.label(egui::RichText::new("Length").weak());
                for l in ANIM_LENGTHS {
                    let d = Duration::from_secs_f32(l);
                    if ui.radio((secs(len) - l).abs() < 0.01, format!("{l} s")).clicked() {
                        *action = Some(set(title, false, Box::new(move |k| {
                            if entering {
                                k.effects.enter_len = d;
                                if !k.fade_in.is_zero() {
                                    k.fade_in = d.min(k.len());
                                }
                            } else {
                                k.effects.exit_len = d;
                                if !k.fade_out.is_zero() {
                                    k.fade_out = d.min(k.len());
                                }
                            }
                        })));
                    }
                }
            });
        }
        ui.separator();
        if ui.add_enabled(!fx.is_none(), egui::Button::new("Remove Effects")).clicked() {
            *action = Some(set("Remove Effects", false, Box::new(|k| k.effects = Default::default())));
        }
    });
}

/// The video track a new title goes on: the highest one with room for it
/// at `at`, or a new one (added when the title is placed).
fn text_track(t: &Timeline, at: Duration) -> TrackRef {
    let len = flicker_core::timeline::DEFAULT_IMAGE_LEN;
    (1..t.video.len())
        .rev()
        .find(|&i| t.video[i].is_free(at, at + len, None))
        .map(TrackRef::video)
        .unwrap_or(TrackRef::video(t.video.len()))
}

fn join_menu(ui: &mut egui::Ui, t: &Timeline, c: ClipRef, action: &mut Option<Action>) {
    let Some(track) = t.track(c.track) else { return };
    let ov = track.overlap_before(c.index);
    let kind = track.clips.get(c.index).map(|k| k.transition).unwrap_or_default();
    ui.label(egui::RichText::new("Join").weak());
    if ui.radio(ov.is_zero(), "Cut").clicked() {
        *action = Some(Action::Transition(c, None));
    }
    for (k, name) in [(TransitionKind::Crossfade, "Cross-fade"), (TransitionKind::DipToBlack, "Dip to black")] {
        for len in TRANSITION_LENGTHS {
            let d = Duration::from_secs_f32(len);
            let on = !ov.is_zero() && kind == k && (secs(ov) - len).abs() < 0.01;
            if ui.radio(on, format!("{name} {len} s")).clicked() {
                *action = Some(Action::Transition(c, Some((k, d))));
            }
        }
    }
}

fn track_menu(ui: &mut egui::Ui, t: &Timeline, r: TrackRef, at: Duration, can_paste: bool, action: &mut Option<Action>) {
    let Some(track) = t.track(r) else { return };
    if ui.add_enabled(can_paste, egui::Button::new("Paste at Playhead").shortcut_text("Ctrl+V")).clicked() {
        *action = Some(Action::Paste(Some(r)));
    }
    if r.kind == TrackKind::Video && ui.button("Add Text Here…").clicked() {
        *action = Some(Action::NewText(Some((r, at))));
    }
    ui.separator();
    if ui.button(if track.muted { "Unmute Track" } else { "Mute Track" }).clicked() {
        *action = Some(Action::ToggleTrackMute(r));
    }
    if r.kind == TrackKind::Video && ui.button(if track.hidden { "Show Track" } else { "Hide Track" }).clicked() {
        *action = Some(Action::ToggleTrackHidden(r));
    }
    let removable = track.clips.is_empty()
        && match r.kind {
            TrackKind::Video => t.video.len() > 1,
            TrackKind::Audio => t.audio.len() > 1,
        };
    if ui.add_enabled(removable, egui::Button::new(format!("Remove {}", r.name()))).clicked() {
        *action = Some(Action::RemoveTrack(r));
    }
}

/// A seconds field.
fn seconds(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) -> egui::Response {
    ui.add(egui::DragValue::new(value).range(range).speed(0.05).max_decimals(2).suffix(" s"))
}

/// A toolbar button, with Windows 95 bevels in that theme.
fn tool_button(ui: &mut egui::Ui, theme: Theme, text: &str, enabled: bool, tip: &str) -> bool {
    let resp = ui.add_enabled(enabled, egui::Button::new(text).min_size(egui::vec2(24.0, 20.0)));
    theme.bevel(ui.painter(), resp.rect, resp.is_pointer_button_down_on());
    resp.on_hover_text(tip).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_picks_the_nearest_point_within_reach() {
        let pts = [0.0, 4.0, 10.0, 10.3];
        assert_eq!(snap(10.2, &pts, 0.25), 10.3);
        assert_eq!(snap(9.9, &pts, 0.25), 10.0);
        assert_eq!(snap(7.0, &pts, 0.25), 7.0);
    }

    #[test]
    fn a_trimmed_edge_follows_the_pointer_from_where_it_was_pressed() {
        let ms = Duration::from_millis;
        // Pressed 5 px inside the edge at 4s (40 px/s): no jump when the
        // drag starts, wherever on the handle it was grabbed.
        assert_eq!(trim_target(4.0, 165.0, 165.0, 40.0, &[], 0.2), (ms(4_000), false));
        // Each pixel is exactly 1/40 s from the press, every frame.
        assert_eq!(trim_target(4.0, 165.0, 175.0, 40.0, &[], 0.2), (ms(4_250), false));
        assert_eq!(trim_target(4.0, 165.0, 164.0, 40.0, &[], 0.2), (ms(3_975), false));
        // Snapping pulls only within reach, and lets go past it.
        let snaps = [5.0, 9.0];
        assert_eq!(trim_target(4.0, 165.0, 203.0, 40.0, &snaps, 0.2), (ms(5_000), true));
        assert_eq!(trim_target(4.0, 165.0, 215.0, 40.0, &snaps, 0.2), (ms(5_250), false));
        // Never before 0.
        assert_eq!(trim_target(1.0, 100.0, 0.0, 40.0, &[], 0.2), (Duration::ZERO, false));
    }

    #[test]
    fn trimming_never_snaps_to_its_own_trail() {
        // The playhead follows the dragged edge, so it isn't a snap point
        // during a trim: only the points taken when the drag began are.
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("a.mp4".into(), Duration::from_secs(10), false)).unwrap();
        let c = ClipRef { track: TrackRef::V1, index: 0 };
        let snaps = snap_points(&t, Some(c), 0.0);
        assert_eq!(snaps, vec![0.0, 0.0], "only the start and the playhead at the press");
        let mut p = Project::new(t, None);
        let mut ends = Vec::new();
        // Drag the end left a pixel at a time: it moves every frame,
        // evenly, and never sticks to where it was the frame before.
        for k in 0..40 {
            let (to, _) = trim_target(10.0, 400.0, 400.0 - k as f32, 40.0, &snaps, 8.0 / 40.0);
            p.gesture("Trim", |tl| tl.trim(c, Edge::End, to));
            ends.push(p.timeline().clip(c).unwrap().end());
        }
        for w in ends.windows(2) {
            assert_eq!(w[0] - w[1], Duration::from_millis(25), "{ends:?}");
        }
    }

    #[test]
    fn ticks_stay_readable() {
        assert_eq!(tick_step(40.0), 2.0);
        assert_eq!(tick_step(400.0), 0.25);
        assert_eq!(tick_step(1.0), 120.0);
        assert_eq!(ruler_label(75.0, 5.0), "1:15");
        assert_eq!(ruler_label(1.5, 0.5), "0:01.5");
    }

    #[test]
    fn tracks_are_listed_top_to_bottom() {
        let mut t = Timeline::default();
        t.add_track(TrackKind::Video);
        t.add_track(TrackKind::Audio);
        let names: Vec<String> = display_order(&t).into_iter().map(TrackRef::name).collect();
        assert_eq!(names, ["V2", "V1", "A1", "A2"]);
    }
}
