//! The timeline panel under the player: a time ruler, video tracks (V2 over
//! V1) and audio tracks, clips drawn as named blocks. Click or drag the
//! ruler to scrub; drag a clip to move it (on its track or to another track
//! of the same kind), drag its edges to trim; right-click a clip or a join
//! for more. Edges snap to the playhead and to other clips.

use std::time::Duration;

use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind};
use flicker_core::media::format_timestamp;
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

#[derive(Clone, Copy, Debug)]
enum Drag {
    Scrub,
    /// Moving a clip; `grab` is how far into the clip it was picked up.
    Move { from: ClipRef, grab: Duration, to: Option<(TrackRef, Duration)> },
    Trim { clip: ClipRef, edge: Edge },
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

/// Things the menus and the properties strip ask for, done after drawing.
enum Action {
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
        if clip.kind == ClipKind::Image {
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
                        Drag::Trim { clip: c, edge }
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
        if let (Some(drag), Some(p)) = (self.drag, pointer) {
            match drag {
                Drag::Scrub => player.seek(dur(t_of(self, p.x))),
                Drag::Trim { clip, edge } => {
                    let t = snap(t_of(self, p.x), &snap_points(project.timeline(), Some(clip), playhead), snap_tol);
                    project.live("Trim", |tl| tl.trim(clip, edge, dur(t)));
                    if let Some(k) = project.timeline().clip(clip) {
                        let show = match edge {
                            Edge::Start => k.start,
                            Edge::End => k.end().saturating_sub(player.frame_len()),
                        };
                        player.seek(show);
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
        resp.context_menu(|ui| match target {
            Some(Target::Clip(c)) => clip_menu(ui, &tl, c, at, &mut action),
            Some(Target::Join(c)) => join_menu(ui, &tl, c, &mut action),
            Some(Target::Track(r)) => track_menu(ui, &tl, r, &mut action),
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
    if c.muted {
        name += "  (muted)";
    } else if c.has_audio && (c.volume - 1.0).abs() > 0.004 {
        name += &format!("  {:.0}%", c.volume * 100.0);
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

fn clip_menu(ui: &mut egui::Ui, t: &Timeline, c: ClipRef, at: Duration, action: &mut Option<Action>) {
    let Some(clip) = t.clip(c) else { return };
    let under = clip.start < at && at < clip.end();
    if ui.add_enabled(under, egui::Button::new("Split at Playhead").shortcut_text("S")).clicked() {
        *action = Some(Action::Split(c));
    }
    if ui.add(egui::Button::new("Delete").shortcut_text("Del")).clicked() {
        *action = Some(Action::Delete(c));
    }
    if ui.add(egui::Button::new("Ripple Delete").shortcut_text("Shift+Del")).clicked() {
        *action = Some(Action::Ripple(c));
    }
    if clip.has_audio {
        ui.separator();
        if ui.button(if clip.muted { "Unmute" } else { "Mute" }).clicked() {
            *action = Some(Action::ToggleMute(c));
        }
    }
    if t.track(c.track).is_some_and(|tr| tr.joined(c.index)) {
        ui.separator();
        ui.menu_button("Transition from Previous", |ui| join_menu(ui, t, c, action));
    }
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

fn track_menu(ui: &mut egui::Ui, t: &Timeline, r: TrackRef, action: &mut Option<Action>) {
    let Some(track) = t.track(r) else { return };
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
