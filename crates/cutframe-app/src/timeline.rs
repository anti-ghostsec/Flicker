//! The one timeline under the picture: shows the playhead, and in cut mode
//! two handles that mark the part to keep (or remove).

use std::time::Duration;

use cutframe_core::{Selection, TimeRange};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke};

use crate::theme::{self, Theme};

const HEIGHT: f32 = 44.0;
/// Width of a handle. The whole handle is the grab area.
const HANDLE_W: f32 = 14.0;
/// How far handles stick out above and below the bar.
const OVERHANG: f32 = 9.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Start,
    End,
}

/// The cut being set up: where the two handles are and what applying does.
#[derive(Clone, Debug)]
pub struct Cut {
    pub start: Duration,
    pub end: Duration,
    pub selection: Selection,
    dragging: Option<Handle>,
}

impl Cut {
    /// Handles around the middle third of the video, or around the playhead.
    pub fn new(total: Duration, playhead: Duration) -> Self {
        let third = total / 3;
        let (start, end) = if playhead > Duration::ZERO && playhead < total {
            let half = (total / 6).max(Duration::from_millis(500));
            (playhead.saturating_sub(half), (playhead + half).min(total))
        } else {
            (third, third * 2)
        };
        Self { start, end, selection: Selection::Keep, dragging: None }
    }

    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.end)
    }

    /// What the video becomes when the cut is applied.
    pub fn keep(&self, total: Duration) -> Vec<TimeRange> {
        cutframe_core::edit::keep_ranges(&[self.range()], self.selection, total)
    }

    pub fn dragging(&self) -> Option<Handle> {
        self.dragging
    }

    pub fn set(&mut self, handle: Handle, t: Duration) {
        match handle {
            Handle::Start => self.start = t,
            Handle::End => self.end = t,
        }
        // Dragging one handle past the other swaps their roles.
        if self.start > self.end {
            std::mem::swap(&mut self.start, &mut self.end);
            self.dragging = self.dragging.map(|h| match h {
                Handle::Start => Handle::End,
                Handle::End => Handle::Start,
            });
        }
    }
}

#[derive(Default)]
pub struct Output {
    /// Jump the playhead here (click, or end of a drag).
    pub seek: Option<Duration>,
    /// Show this frame now: a handle or the playhead is being dragged.
    pub preview: Option<Duration>,
}

pub fn show(
    ui: &mut egui::Ui,
    theme: Theme,
    total: Duration,
    playhead: Duration,
    mut cut: Option<&mut Cut>,
) -> Output {
    let mut out = Output::default();
    let (outer, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEIGHT), Sense::click_and_drag());
    if total.is_zero() {
        return out;
    }
    // Room around the bar so the handles fit at either end.
    let bar = Rect::from_x_y_ranges(
        outer.x_range().shrink(HANDLE_W / 2.0 + 1.0),
        (outer.top() + OVERHANG + 1.0)..=(outer.bottom() - OVERHANG - 1.0),
    );
    let x_of = |t: Duration| bar.left() + bar.width() * (t.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0);
    let t_of = |x: f32| total.mul_f32(((x - bar.left()) / bar.width()).clamp(0.0, 1.0));

    // Interaction: a press that starts on a handle drags it, anywhere else scrubs.
    let handle_rect = |t: Duration| {
        Rect::from_x_y_ranges((x_of(t) - HANDLE_W / 2.0)..=(x_of(t) + HANDLE_W / 2.0), outer.y_range())
    };
    let handle_at = |pos: Pos2, cut: &Cut| -> Option<Handle> {
        let (s, e) = (handle_rect(cut.start).expand2(egui::vec2(3.0, 0.0)), handle_rect(cut.end).expand2(egui::vec2(3.0, 0.0)));
        match (s.contains(pos), e.contains(pos)) {
            // Overlapping: take the nearer one.
            (true, true) => Some(if (pos.x - x_of(cut.start)).abs() <= (pos.x - x_of(cut.end)).abs() {
                Handle::Start
            } else {
                Handle::End
            }),
            (true, false) => Some(Handle::Start),
            (false, true) => Some(Handle::End),
            (false, false) => None,
        }
    };
    let pointer = resp.interact_pointer_pos();
    let mut hovered = None;
    if let Some(cut) = cut.as_deref_mut() {
        if resp.drag_started() {
            // Where the press began, not where the pointer is now: by the time
            // a drag registers it may have moved off the handle.
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            cut.dragging = origin.and_then(|p| handle_at(p, cut));
        }
        if let (Some(h), Some(p)) = (cut.dragging, pointer) {
            cut.set(h, t_of(p.x));
            let h = cut.dragging.unwrap_or(h);
            out.preview = Some(match h {
                Handle::Start => cut.start,
                Handle::End => cut.end,
            });
        }
        if resp.drag_stopped() {
            if let Some(h) = cut.dragging.take() {
                out.seek = Some(match h {
                    Handle::Start => cut.start,
                    Handle::End => cut.end,
                });
            }
        }
        hovered = resp.hover_pos().and_then(|p| handle_at(p, cut));
        if hovered.is_some() || cut.dragging.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let handle_drag = cut.as_ref().is_some_and(|c| c.dragging.is_some());
    if !handle_drag {
        if let Some(p) = pointer {
            if resp.dragged() {
                out.preview = Some(t_of(p.x));
            }
            if resp.clicked() || resp.drag_stopped() {
                out.seek = Some(t_of(p.x));
            }
        }
    }

    // Drawing.
    let painter = ui.painter_at(outer.expand(2.0));
    let win95 = theme.is_win95();
    let track = if win95 { Color32::from_gray(0xA0) } else { ui.visuals().extreme_bg_color };
    painter.rect_filled(bar, 0.0, track);

    if let Some(cut) = cut.as_deref() {
        let keep = Color32::from_rgb(60, 150, 80);
        let gone = Color32::from_black_alpha(110);
        let span = |a: Duration, b: Duration| Rect::from_x_y_ranges(x_of(a)..=x_of(b), bar.y_range());
        match cut.selection {
            Selection::Keep => {
                painter.rect_filled(span(Duration::ZERO, cut.start), 0.0, gone);
                painter.rect_filled(span(cut.end, total), 0.0, gone);
                painter.rect_filled(span(cut.start, cut.end), 0.0, keep.gamma_multiply(0.55));
            }
            Selection::Remove => {
                painter.rect_filled(span(cut.start, cut.end), 0.0, Color32::from_rgb(170, 50, 50).gamma_multiply(0.7));
            }
        }
    }
    theme.bevel(&painter, bar.expand(2.0), true);

    // Playhead.
    let x = x_of(playhead);
    let head = if win95 { theme::DARK } else { ui.visuals().strong_text_color() };
    painter.line_segment([egui::pos2(x, bar.top() - 3.0), egui::pos2(x, bar.bottom() + 3.0)], Stroke::new(2.0, head));

    if let Some(cut) = cut.as_deref() {
        for (h, t) in [(Handle::Start, cut.start), (Handle::End, cut.end)] {
            let state = if cut.dragging == Some(h) {
                HandleState::Dragged
            } else if hovered == Some(h) && cut.dragging.is_none() {
                HandleState::Hovered
            } else {
                HandleState::Idle
            };
            draw_handle(&painter, theme, handle_rect(t), bar, h, state);
        }
    }

    if cut.is_some() {
        resp.on_hover_text("Drag the handles to choose the part. Click elsewhere to move the playhead.");
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HandleState {
    Idle,
    Hovered,
    Dragged,
}

/// A tall raised grip, like a Windows 95 slider thumb, with ridges to show it
/// can be grabbed and a notch pointing into the selection.
fn draw_handle(painter: &egui::Painter, theme: Theme, rect: Rect, bar: Rect, h: Handle, state: HandleState) {
    let win95 = theme.is_win95();
    let fill = match (win95, state) {
        (true, HandleState::Dragged) => theme::NAVY,
        (true, HandleState::Hovered) => Color32::from_rgb(0xD8, 0xD8, 0xD8),
        (true, HandleState::Idle) => theme::FACE,
        (false, HandleState::Dragged) => Color32::from_rgb(90, 160, 255),
        (false, HandleState::Hovered) => Color32::from_rgb(245, 245, 245),
        (false, HandleState::Idle) => Color32::from_rgb(225, 225, 225),
    };
    painter.rect_filled(rect, 1.0, fill);
    theme.bevel(painter, rect, state == HandleState::Dragged);

    let ink = if state == HandleState::Dragged && win95 { theme::LIGHT } else { Color32::from_gray(70) };
    // Three ridges in the middle.
    let cy = bar.center().y;
    for dy in [-4.0, 0.0, 4.0] {
        let y = cy + dy;
        painter.line_segment([egui::pos2(rect.left() + 4.0, y), egui::pos2(rect.right() - 4.0, y)], Stroke::new(1.0, ink));
    }
    // Notch at the top pointing into the selection.
    let dir = if h == Handle::Start { 1.0 } else { -1.0 };
    let tip = egui::pos2(rect.center().x + dir * 4.0, rect.top() + 6.0);
    let base = rect.center().x - dir * 2.0;
    painter.add(egui::Shape::convex_polygon(
        vec![egui::pos2(base, rect.top() + 2.5), tip, egui::pos2(base, rect.top() + 9.5)],
        ink,
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn new_cut_surrounds_the_playhead() {
        let c = Cut::new(secs(60), secs(30));
        assert!(c.start < secs(30) && c.end > secs(30));
        let c = Cut::new(secs(60), Duration::ZERO);
        assert_eq!((c.start, c.end), (secs(20), secs(40)));
    }

    #[test]
    fn dragging_past_the_other_handle_swaps() {
        let mut c = Cut::new(secs(60), Duration::ZERO);
        c.dragging = Some(Handle::Start);
        c.set(Handle::Start, secs(50));
        assert_eq!((c.start, c.end), (secs(40), secs(50)));
        assert_eq!(c.dragging(), Some(Handle::End), "still holding the same grip");
    }

    #[test]
    fn keep_and_remove_give_the_resulting_video() {
        let mut c = Cut::new(secs(60), Duration::ZERO);
        assert_eq!(c.keep(secs(60)), vec![TimeRange::new(secs(20), secs(40))]);
        c.selection = Selection::Remove;
        assert_eq!(
            c.keep(secs(60)),
            vec![TimeRange::new(secs(0), secs(20)), TimeRange::new(secs(40), secs(60))]
        );
    }
}
