//! Look and feel. "Windows 95" matches the classic desktop: grey 3D bevels,
//! navy highlights, square corners. The choice is remembered between runs.

use eframe::egui::{self, Color32, CornerRadius, Painter, Rect, Shadow, Stroke};

pub const FACE: Color32 = Color32::from_rgb(0xC0, 0xC0, 0xC0);
pub const SHADOW: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const DARK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const LIGHT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const NAVY: Color32 = Color32::from_rgb(0x00, 0x00, 0x80);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Win95,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Win95, Theme::Light, Theme::Dark];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Win95 => "Windows 95",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Theme::Win95 => "win95",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn is_win95(self) -> bool {
        self == Theme::Win95
    }

    pub fn apply(self, ctx: &egui::Context) {
        let visuals = match self {
            Theme::Win95 => win95_visuals(),
            Theme::Light => egui::Visuals::light(),
            Theme::Dark => egui::Visuals::dark(),
        };
        // Pin egui's light/dark slot so following the system theme can't
        // swap ours out.
        let slot = if visuals.dark_mode { egui::Theme::Dark } else { egui::Theme::Light };
        ctx.set_theme(slot);
        ctx.set_visuals_of(slot, visuals);
        ctx.style_mut_of(slot, |s| {
            s.spacing.button_padding = egui::vec2(8.0, 3.0);
            s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        });
    }

    /// Saved choice, defaulting to Windows 95.
    pub fn load() -> Self {
        let saved = config_file().and_then(|f| std::fs::read_to_string(f).ok());
        Self::ALL
            .into_iter()
            .find(|t| saved.as_deref().map(str::trim) == Some(t.key()))
            .unwrap_or(Theme::Win95)
    }

    pub fn save(self) {
        if let Some(f) = config_file() {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(f, self.key());
        }
    }

    /// Draw a raised (buttons, bars) or sunken (fields, the video well) edge
    /// in the Windows 95 style. No-op for other themes, which draw a thin
    /// outline instead.
    pub fn bevel(self, painter: &Painter, rect: Rect, sunken: bool) {
        if !self.is_win95() {
            let stroke = Stroke::new(1.0, painter.ctx().global_style().visuals.widgets.noninteractive.bg_stroke.color);
            painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
            return;
        }
        let (outer_tl, outer_br, inner_tl, inner_br) =
            if sunken { (SHADOW, LIGHT, DARK, FACE) } else { (LIGHT, DARK, FACE, SHADOW) };
        edge(painter, rect, outer_tl, outer_br);
        edge(painter, rect.shrink(1.0), inner_tl, inner_br);
    }
}

/// Top/left lines in `tl`, bottom/right in `br`, one pixel inside `rect`.
fn edge(painter: &Painter, rect: Rect, tl: Color32, br: Color32) {
    let r = rect.shrink(0.5);
    let (a, b, c, d) = (r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom());
    painter.line_segment([d, a], Stroke::new(1.0, tl));
    painter.line_segment([a, b], Stroke::new(1.0, tl));
    painter.line_segment([b, c], Stroke::new(1.0, br));
    painter.line_segment([c, d], Stroke::new(1.0, br));
}

fn win95_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    let square = CornerRadius::ZERO;
    v.panel_fill = FACE;
    v.window_fill = FACE;
    v.faint_bg_color = FACE;
    v.extreme_bg_color = LIGHT;
    v.text_edit_bg_color = Some(LIGHT);
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.window_stroke = Stroke::new(1.0, DARK);
    v.window_shadow = Shadow { offset: [2, 2], blur: 0, spread: 0, color: SHADOW };
    v.popup_shadow = Shadow { offset: [2, 2], blur: 0, spread: 0, color: SHADOW };
    v.selection.bg_fill = NAVY;
    v.selection.stroke = Stroke::new(1.0, LIGHT);
    v.hyperlink_color = NAVY;
    v.warn_fg_color = Color32::from_rgb(0x80, 0x60, 0x00);
    v.error_fg_color = Color32::from_rgb(0xA0, 0x00, 0x00);
    v.striped = false;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = square;
        wv.expansion = 0.0;
        wv.bg_fill = FACE;
        wv.weak_bg_fill = FACE;
        wv.fg_stroke = Stroke::new(1.0, DARK);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, SHADOW);
    w.inactive.bg_stroke = Stroke::new(1.0, SHADOW);
    // Menus highlight in navy with white text, like the real thing.
    for wv in [&mut w.hovered, &mut w.active, &mut w.open] {
        wv.bg_fill = NAVY;
        wv.weak_bg_fill = NAVY;
        wv.bg_stroke = Stroke::new(1.0, NAVY);
        wv.fg_stroke = Stroke::new(1.0, LIGHT);
    }
    v
}

fn config_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("cutframe").join("theme"))
}
