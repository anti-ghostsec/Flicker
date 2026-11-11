//! The project: video tracks (V1 at the bottom, V2 drawn over it) and audio
//! tracks (A1, A2…) holding clips at positions on a shared time line.
//!
//! Every edit here is a cheap change to this model; nothing is rendered until
//! the preview or an export asks for it (see [`crate::render`]). Clips on one
//! track never overlap, except where a transition joins two neighbours: the
//! later clip then starts before the earlier one ends, and the overlap is
//! the transition.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Clips can't be trimmed or split into pieces shorter than this.
pub const MIN_CLIP: Duration = Duration::from_millis(100);
/// How long a picture stays on screen when it's added.
pub const DEFAULT_IMAGE_LEN: Duration = Duration::from_secs(4);
/// Two clip edges this close count as touching.
pub const EPS: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    /// A video file: picture, and sound if it has any.
    Video,
    /// A still picture, shown for the clip's length.
    Image,
    /// A sound file (music, voice).
    Audio,
    /// Words drawn over the picture (see [`TextStyle`]); no file.
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKind {
    /// The two clips blend into each other.
    #[default]
    Crossfade,
    /// The first clip fades to black, then the second fades in.
    DipToBlack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub path: PathBuf,
    pub kind: ClipKind,
    #[serde(default)]
    pub has_audio: bool,
    /// Length of the source (`None` for pictures or when unknown).
    #[serde(default, with = "opt_secs")]
    pub source_len: Option<Duration>,
    /// Source size and frame rate, used to pick the project's frame size.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<f64>,
    /// Where the clip starts on the timeline.
    #[serde(default, with = "secs")]
    pub start: Duration,
    /// Part of the source that's used. For pictures `in_point` is zero and
    /// `out_point` is how long the picture is shown.
    #[serde(default, with = "secs")]
    pub in_point: Duration,
    #[serde(with = "secs")]
    pub out_point: Duration,
    /// 1.0 = unchanged, 0.0 = silent, up to 2.0.
    #[serde(default = "one")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default, with = "secs")]
    pub fade_in: Duration,
    #[serde(default, with = "secs")]
    pub fade_out: Duration,
    /// How this clip blends with the one before it where they overlap.
    #[serde(default)]
    pub transition: TransitionKind,
    /// Playback speed: 2.0 plays twice as fast (and takes half as long on
    /// the timeline). Videos and sound only.
    #[serde(default = "one64", skip_serializing_if = "is_one64")]
    pub speed: f64,
    /// 1.0 = solid, 0.0 = invisible.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f32,
    /// Turned and mirrored.
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
    /// Edges cut off the source picture.
    #[serde(default, skip_serializing_if = "Crop::is_none")]
    pub crop: Crop,
    /// Motion: throughout the clip, as it comes in and as it goes.
    #[serde(default, skip_serializing_if = "Effects::is_none")]
    pub effects: Effects,
    /// What a text clip says and how it looks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextStyle>,
}

fn one() -> f32 {
    1.0
}

fn one64() -> f64 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_one64(v: &f64) -> bool {
    *v == 1.0
}

/// Slowest and fastest clip speeds.
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

/// Quarter turns clockwise, then mirroring, as seen on screen. Kept in one
/// form only: mirrored both ways is a half turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Transform {
    /// 0, 90, 180 or 270 degrees clockwise.
    #[serde(default)]
    pub rotate: u16,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    fn normalized(mut self) -> Self {
        self.rotate %= 360;
        self.rotate -= self.rotate % 90;
        if self.flip_h && self.flip_v {
            self.flip_h = false;
            self.flip_v = false;
            self.rotate = (self.rotate + 180) % 360;
        }
        self
    }

    /// Turn what's on screen a quarter turn clockwise.
    pub fn rotated_cw(self) -> Self {
        // A turn of a mirrored picture is the turned picture mirrored the
        // other way.
        Self { rotate: self.rotate + 90, flip_h: self.flip_v, flip_v: self.flip_h }.normalized()
    }

    pub fn rotated_ccw(self) -> Self {
        self.rotated_cw().rotated_cw().rotated_cw()
    }

    pub fn rotated_180(self) -> Self {
        self.rotated_cw().rotated_cw()
    }

    /// Mirror what's on screen left to right.
    pub fn flipped_h(self) -> Self {
        Self { flip_h: !self.flip_h, ..self }.normalized()
    }

    /// Mirror what's on screen top to bottom.
    pub fn flipped_v(self) -> Self {
        Self { flip_v: !self.flip_v, ..self }.normalized()
    }

    /// Width and height trade places.
    pub fn swaps_sides(&self) -> bool {
        self.rotate % 180 == 90
    }
}

/// Fractions of the source picture cut off each edge (0.0 to 0.45 each).
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Crop {
    #[serde(default)]
    pub left: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub bottom: f32,
}

impl Crop {
    pub const MAX: f32 = 0.45;

    pub fn is_none(&self) -> bool {
        self.clamped() == Self::default()
    }

    pub fn clamped(&self) -> Self {
        let c = |v: f32| if v.is_finite() { v.clamp(0.0, Self::MAX) } else { 0.0 };
        Self { left: c(self.left), right: c(self.right), top: c(self.top), bottom: c(self.bottom) }
    }
}

/// Movement for the whole length of a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Slowly closer (Ken Burns).
    ZoomIn,
    /// Slowly further away.
    ZoomOut,
    /// Slides slowly to the left (zoomed in a little, so no edge shows).
    PanLeft,
    PanRight,
    /// Beats a little bigger and back, once a second.
    Pulse,
    /// Hops up and down.
    Bounce,
    /// Drifts gently up and down.
    Float,
    /// Shakes like a hand-held camera in a quake.
    Shake,
    /// Rocks from side to side.
    Wobble,
    /// Turns round, once every four seconds.
    Spin,
}

impl Motion {
    pub const ALL: [Motion; 10] = [
        Motion::ZoomIn,
        Motion::ZoomOut,
        Motion::PanLeft,
        Motion::PanRight,
        Motion::Pulse,
        Motion::Bounce,
        Motion::Float,
        Motion::Shake,
        Motion::Wobble,
        Motion::Spin,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Motion::ZoomIn => "Zoom In (Ken Burns)",
            Motion::ZoomOut => "Zoom Out",
            Motion::PanLeft => "Pan Left",
            Motion::PanRight => "Pan Right",
            Motion::Pulse => "Pulse",
            Motion::Bounce => "Bounce",
            Motion::Float => "Float",
            Motion::Shake => "Shake",
            Motion::Wobble => "Wobble",
            Motion::Spin => "Spin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// How a clip comes in or goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anim {
    /// From (or off to) that side of the frame.
    Slide(Side),
    /// Grows from nothing (or shrinks to nothing).
    Zoom,
    /// Grows while turning (or shrinks while turning).
    Spin,
}

impl Anim {
    pub const ALL: [Anim; 6] = [
        Anim::Slide(Side::Left),
        Anim::Slide(Side::Right),
        Anim::Slide(Side::Top),
        Anim::Slide(Side::Bottom),
        Anim::Zoom,
        Anim::Spin,
    ];

    /// "Slide from Left" / "Slide to Left"…
    pub fn name(self, entering: bool) -> String {
        let dir = if entering { "from" } else { "to" };
        match self {
            Anim::Slide(s) => format!("Slide {dir} {s:?}"),
            Anim::Zoom if entering => "Zoom In (grow)".into(),
            Anim::Zoom => "Zoom Out (shrink)".into(),
            Anim::Spin if entering => "Spin In".into(),
            Anim::Spin => "Spin Out".into(),
        }
    }
}

/// Default length of an entrance or exit.
pub const ANIM_LEN: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    #[serde(default)]
    pub motion: Option<Motion>,
    #[serde(default)]
    pub enter: Option<Anim>,
    #[serde(default = "anim_len", with = "secs")]
    pub enter_len: Duration,
    #[serde(default)]
    pub exit: Option<Anim>,
    #[serde(default = "anim_len", with = "secs")]
    pub exit_len: Duration,
}

fn anim_len() -> Duration {
    ANIM_LEN
}

impl Default for Effects {
    fn default() -> Self {
        Self { motion: None, enter: None, enter_len: ANIM_LEN, exit: None, exit_len: ANIM_LEN }
    }
}

impl Effects {
    pub fn is_none(&self) -> bool {
        self.motion.is_none() && self.enter.is_none() && self.exit.is_none()
    }
}

/// Where text sits in the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextPlace {
    Top,
    #[default]
    Middle,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub text: String,
    /// Letter height as a share of the frame's height (0.02 to 0.3).
    #[serde(default = "text_size")]
    pub size: f32,
    /// RGB.
    #[serde(default = "white")]
    pub color: [u8; 3],
    #[serde(default)]
    pub place: TextPlace,
    /// A dark box behind the letters.
    #[serde(default)]
    pub boxed: bool,
}

fn text_size() -> f32 {
    0.08
}

fn white() -> [u8; 3] {
    [255, 255, 255]
}

impl Default for TextStyle {
    fn default() -> Self {
        Self { text: String::new(), size: text_size(), color: white(), place: TextPlace::default(), boxed: false }
    }
}

impl Clip {
    pub fn video(path: PathBuf, len: Duration, has_audio: bool) -> Self {
        Self {
            path,
            kind: ClipKind::Video,
            has_audio,
            source_len: Some(len),
            width: None,
            height: None,
            fps: None,
            start: Duration::ZERO,
            in_point: Duration::ZERO,
            out_point: len,
            volume: 1.0,
            muted: false,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
            transition: TransitionKind::default(),
            speed: 1.0,
            opacity: 1.0,
            transform: Transform::default(),
            crop: Crop::default(),
            effects: Effects::default(),
            text: None,
        }
    }

    pub fn image(path: PathBuf) -> Self {
        Self { kind: ClipKind::Image, source_len: None, ..Self::video(path, DEFAULT_IMAGE_LEN, false) }
    }

    pub fn audio(path: PathBuf, len: Duration) -> Self {
        Self { kind: ClipKind::Audio, ..Self::video(path, len, true) }
    }

    /// Words on the picture, shown for [`DEFAULT_IMAGE_LEN`].
    pub fn text(style: TextStyle) -> Self {
        Self { kind: ClipKind::Text, text: Some(style), ..Self::image(PathBuf::new()) }
    }

    /// The clip's speed, kept sensible (always 1 for pictures and text).
    pub fn speed(&self) -> f64 {
        if self.is_still() || !self.speed.is_finite() {
            1.0
        } else {
            self.speed.clamp(MIN_SPEED, MAX_SPEED)
        }
    }

    /// How much of the source plays in `d` of the timeline.
    pub fn source_span(&self, d: Duration) -> Duration {
        match self.speed() {
            1.0 => d,
            s => Duration::from_secs_f64(d.as_secs_f64() * s),
        }
    }

    /// How long `d` of the source takes on the timeline.
    fn timeline_span(&self, d: Duration) -> Duration {
        match self.speed() {
            1.0 => d,
            s => Duration::from_secs_f64(d.as_secs_f64() / s),
        }
    }

    /// How long the clip plays.
    pub fn len(&self) -> Duration {
        self.timeline_span(self.out_point.saturating_sub(self.in_point))
    }

    /// The clip has a file behind it (text clips don't).
    pub fn has_file(&self) -> bool {
        self.kind != ClipKind::Text
    }

    /// It shows a picture (on a video track).
    pub fn has_picture(&self) -> bool {
        self.kind != ClipKind::Audio
    }

    pub fn is_empty(&self) -> bool {
        self.len() < MIN_CLIP
    }

    pub fn end(&self) -> Duration {
        self.start + self.len()
    }

    pub fn name(&self) -> String {
        if let Some(t) = self.text.as_ref().filter(|_| self.kind == ClipKind::Text) {
            let first = t.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("Text");
            return format!("T  {}", first.trim());
        }
        self.path.file_name().unwrap_or(self.path.as_os_str()).to_string_lossy().into_owned()
    }

    /// True when its sound is heard (given its track isn't muted).
    pub fn audible(&self) -> bool {
        self.has_audio && !self.muted && self.volume > 0.0
    }

    /// Can be stretched to any length (pictures and text).
    pub fn is_still(&self) -> bool {
        matches!(self.kind, ClipKind::Image | ClipKind::Text)
    }

    fn clamp_fades(&mut self) {
        let len = self.len();
        self.fade_in = self.fade_in.min(len);
        self.fade_out = self.fade_out.min(len);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub kind: TrackKind,
    /// Sorted by start.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// No sound from this track.
    #[serde(default)]
    pub muted: bool,
    /// No picture from this (video) track.
    #[serde(default)]
    pub hidden: bool,
}

impl Track {
    pub fn new(kind: TrackKind) -> Self {
        Self { kind, clips: Vec::new(), muted: false, hidden: false }
    }

    pub fn end(&self) -> Duration {
        self.clips.iter().map(Clip::end).max().unwrap_or_default()
    }

    /// Whether this track can hold `clip`: videos and pictures go on video
    /// tracks; sound files, and videos with sound (for their sound), go on
    /// audio tracks.
    pub fn accepts(&self, clip: &Clip) -> bool {
        match self.kind {
            TrackKind::Video => clip.has_picture(),
            TrackKind::Audio => clip.kind == ClipKind::Audio || (clip.kind == ClipKind::Video && clip.has_audio),
        }
    }

    /// How much clip `i` overlaps the clip before it (its transition).
    pub fn overlap_before(&self, i: usize) -> Duration {
        if i == 0 || i >= self.clips.len() {
            return Duration::ZERO;
        }
        let (a, b) = (&self.clips[i - 1], &self.clips[i]);
        a.end().saturating_sub(b.start).min(a.len()).min(b.len())
    }

    /// Clip `i` touches or overlaps the one before it.
    pub fn joined(&self, i: usize) -> bool {
        i > 0 && i < self.clips.len() && self.clips[i].start <= self.clips[i - 1].end() + EPS
    }

    /// The clip playing at `t` (the later one inside a transition).
    pub fn clip_at(&self, t: Duration) -> Option<usize> {
        self.clips.iter().rposition(|c| c.start <= t && t < c.end())
    }

    /// No clip (other than `exclude`) overlaps `from..to`.
    pub fn is_free(&self, from: Duration, to: Duration, exclude: Option<usize>) -> bool {
        self.clips
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != exclude)
            .all(|(_, c)| !(c.start + EPS < to && from + EPS < c.end()))
    }

    /// The free start nearest to `desired` for a clip of length `len`.
    pub fn fit(&self, len: Duration, desired: Duration, exclude: Option<usize>) -> Duration {
        let mut candidates = vec![desired];
        for (i, c) in self.clips.iter().enumerate() {
            if Some(i) == exclude {
                continue;
            }
            candidates.push(c.end());
            if let Some(s) = c.start.checked_sub(len) {
                candidates.push(s);
            }
        }
        let dist = |s: &Duration| s.abs_diff(desired);
        candidates
            .into_iter()
            .filter(|s| self.is_free(*s, *s + len, exclude))
            .min_by_key(dist)
            .unwrap_or_else(|| self.end())
    }

    /// Put `clip` in start order; returns its index.
    fn insert(&mut self, clip: Clip) -> usize {
        let i = self.clips.partition_point(|c| c.start <= clip.start);
        self.clips.insert(i, clip);
        i
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackRef {
    pub kind: TrackKind,
    /// 0 = V1 / A1.
    pub index: usize,
}

impl TrackRef {
    pub const V1: TrackRef = TrackRef { kind: TrackKind::Video, index: 0 };
    pub const A1: TrackRef = TrackRef { kind: TrackKind::Audio, index: 0 };

    pub fn video(index: usize) -> Self {
        Self { kind: TrackKind::Video, index }
    }

    pub fn audio(index: usize) -> Self {
        Self { kind: TrackKind::Audio, index }
    }

    /// "V1", "A2"…
    pub fn name(self) -> String {
        let letter = match self.kind {
            TrackKind::Video => 'V',
            TrackKind::Audio => 'A',
        };
        format!("{letter}{}", self.index + 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClipRef {
    pub track: TrackRef,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// What happens to the subtitle file at export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleExport {
    /// Only shown in the preview.
    #[default]
    Off,
    /// A track viewers can turn on or off.
    Track,
    /// Drawn into the picture.
    Burn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    /// V1 first; later tracks are drawn over earlier ones.
    pub video: Vec<Track>,
    pub audio: Vec<Track>,
    /// Subtitles timed to the timeline.
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    #[serde(default)]
    pub subtitle_export: SubtitleExport,
    /// ISO 639-2 code for a subtitle track ("" = none).
    #[serde(default = "eng")]
    pub subtitle_language: String,
}

fn eng() -> String {
    "eng".into()
}

impl Default for Timeline {
    /// One video track and one audio track.
    fn default() -> Self {
        Self {
            video: vec![Track::new(TrackKind::Video)],
            audio: vec![Track::new(TrackKind::Audio)],
            subtitle: None,
            subtitle_export: SubtitleExport::Off,
            subtitle_language: eng(),
        }
    }
}

impl Timeline {
    pub fn track(&self, r: TrackRef) -> Option<&Track> {
        match r.kind {
            TrackKind::Video => self.video.get(r.index),
            TrackKind::Audio => self.audio.get(r.index),
        }
    }

    pub fn track_mut(&mut self, r: TrackRef) -> Option<&mut Track> {
        match r.kind {
            TrackKind::Video => self.video.get_mut(r.index),
            TrackKind::Audio => self.audio.get_mut(r.index),
        }
    }

    /// Every track with its reference: video tracks, then audio tracks.
    pub fn tracks(&self) -> impl Iterator<Item = (TrackRef, &Track)> {
        let v = self.video.iter().enumerate().map(|(i, t)| (TrackRef::video(i), t));
        let a = self.audio.iter().enumerate().map(|(i, t)| (TrackRef::audio(i), t));
        v.chain(a)
    }

    pub fn clip(&self, c: ClipRef) -> Option<&Clip> {
        self.track(c.track)?.clips.get(c.index)
    }

    pub fn clip_mut(&mut self, c: ClipRef) -> Option<&mut Clip> {
        self.track_mut(c.track)?.clips.get_mut(c.index)
    }

    pub fn add_track(&mut self, kind: TrackKind) -> TrackRef {
        match kind {
            TrackKind::Video => {
                self.video.push(Track::new(kind));
                TrackRef::video(self.video.len() - 1)
            }
            TrackKind::Audio => {
                self.audio.push(Track::new(kind));
                TrackRef::audio(self.audio.len() - 1)
            }
        }
    }

    /// Remove an empty track (never the last of its kind).
    pub fn remove_track(&mut self, r: TrackRef) -> bool {
        let list = match r.kind {
            TrackKind::Video => &mut self.video,
            TrackKind::Audio => &mut self.audio,
        };
        if list.len() > 1 && list.get(r.index).is_some_and(|t| t.clips.is_empty()) {
            list.remove(r.index);
            true
        } else {
            false
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks().all(|(_, t)| t.clips.is_empty())
    }

    pub fn clip_count(&self) -> usize {
        self.tracks().map(|(_, t)| t.clips.len()).sum()
    }

    /// Length of the finished video: where the last clip on any track ends.
    pub fn duration(&self) -> Duration {
        self.tracks().map(|(_, t)| t.end()).max().unwrap_or_default()
    }

    /// Put `clip` on track `r` as near `at` as it fits.
    pub fn add_clip(&mut self, r: TrackRef, mut clip: Clip, at: Duration) -> Option<ClipRef> {
        let track = self.track_mut(r)?;
        if !track.accepts(&clip) {
            return None;
        }
        clip.start = track.fit(clip.len(), at, None);
        let index = track.insert(clip);
        Some(ClipRef { track: r, index })
    }

    /// Put `clip` at the end of track `r`.
    pub fn append(&mut self, r: TrackRef, clip: Clip) -> Option<ClipRef> {
        let at = self.track(r)?.end();
        self.add_clip(r, clip, at)
    }

    /// Move a clip to `at` on track `to` (a track of the same kind), as near
    /// as it fits. Any transition with its old neighbours goes away.
    pub fn move_clip(&mut self, c: ClipRef, to: TrackRef, at: Duration) -> Option<ClipRef> {
        if c.track.kind != to.kind {
            return None;
        }
        let clip = self.clip(c)?;
        if !self.track(to)?.accepts(clip) {
            return None;
        }
        let mut clip = self.track_mut(c.track)?.clips.remove(c.index);
        let track = self.track_mut(to)?;
        clip.start = track.fit(clip.len(), at, None);
        let index = track.insert(clip);
        Some(ClipRef { track: to, index })
    }

    /// Drag one edge of a clip to `t`. A clip can't grow into its
    /// neighbours, past the ends of its source, or below [`MIN_CLIP`].
    /// Pictures can be any length.
    pub fn trim(&mut self, c: ClipRef, edge: Edge, t: Duration) -> bool {
        let Some(track) = self.track_mut(c.track) else { return false };
        let Some(clip) = track.clips.get(c.index) else { return false };
        let (start, end) = (clip.start, clip.end());
        match edge {
            Edge::Start => {
                let mut lower = match c.index.checked_sub(1).map(|p| track.clips[p].end()) {
                    Some(prev_end) => prev_end.min(start),
                    None => Duration::ZERO,
                };
                if !clip.is_still() {
                    lower = lower.max(start.saturating_sub(clip.timeline_span(clip.in_point)));
                }
                let upper = end.saturating_sub(MIN_CLIP).max(lower);
                let new = t.clamp(lower, upper);
                let clip = &mut track.clips[c.index];
                if clip.is_still() {
                    clip.out_point = end - new;
                } else if new < start {
                    clip.in_point = clip.in_point.saturating_sub(clip.source_span(start - new));
                } else {
                    clip.in_point += clip.source_span(new - start);
                }
                clip.start = new;
                clip.clamp_fades();
            }
            Edge::End => {
                let mut upper = match track.clips.get(c.index + 1) {
                    Some(next) => next.start.max(end),
                    None => Duration::MAX,
                };
                if let (false, Some(src)) = (clip.is_still(), clip.source_len) {
                    upper = upper.min(start + clip.timeline_span(src.saturating_sub(clip.in_point)));
                }
                let lower = start + MIN_CLIP;
                let new = t.clamp(lower, upper.max(lower));
                let clip = &mut track.clips[c.index];
                clip.out_point = clip.in_point + clip.source_span(new - start);
                if let (false, Some(src)) = (clip.is_still(), clip.source_len) {
                    clip.out_point = clip.out_point.min(src.max(clip.in_point));
                }
                clip.clamp_fades();
            }
        }
        true
    }

    /// Split a clip in two at `t`. Returns the second half.
    pub fn split(&mut self, c: ClipRef, t: Duration) -> Option<ClipRef> {
        let track = self.track_mut(c.track)?;
        let clip = track.clips.get(c.index)?;
        let offset = t.checked_sub(clip.start)?;
        if offset < MIN_CLIP || clip.len().saturating_sub(offset) < MIN_CLIP {
            return None;
        }
        let mut left = clip.clone();
        let mut right = clip.clone();
        if clip.is_still() {
            left.out_point = offset;
            right.out_point = clip.len() - offset;
        } else {
            left.out_point = clip.in_point + clip.source_span(offset);
            right.in_point = left.out_point;
        }
        right.start = t;
        left.fade_out = Duration::ZERO;
        right.fade_in = Duration::ZERO;
        // The first half keeps how it comes in, the second how it goes.
        left.effects.exit = None;
        right.effects.enter = None;
        right.transition = TransitionKind::default();
        left.clamp_fades();
        right.clamp_fades();
        track.clips[c.index] = left;
        track.clips.insert(c.index + 1, right);
        Some(ClipRef { track: c.track, index: c.index + 1 })
    }

    /// Split every clip under `t` (on every track). Returns how many were split.
    pub fn split_all(&mut self, t: Duration) -> usize {
        let refs: Vec<ClipRef> = self
            .tracks()
            .flat_map(|(r, track)| {
                track
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.start < t && t < c.end())
                    .map(move |(i, _)| ClipRef { track: r, index: i })
            })
            .collect();
        // Back to front, so earlier indices stay valid.
        refs.into_iter().rev().filter_map(|c| self.split(c, t)).count()
    }

    /// Take a clip out, leaving a gap.
    pub fn remove(&mut self, c: ClipRef) -> Option<Clip> {
        let track = self.track_mut(c.track)?;
        (c.index < track.clips.len()).then(|| track.clips.remove(c.index))
    }

    /// Take a clip out and close the gap: later clips on its track move left.
    pub fn ripple_remove(&mut self, c: ClipRef) -> Option<Clip> {
        let track = self.track_mut(c.track)?;
        if c.index >= track.clips.len() {
            return None;
        }
        let clip = track.clips.remove(c.index);
        let prev_end = c.index.checked_sub(1).map_or(Duration::ZERO, |p| track.clips[p].end());
        if let Some(next) = track.clips.get(c.index) {
            // The space the clip took up, without moving the next clip into
            // the one before.
            let shift = clip.end().min(next.start).saturating_sub(clip.start.max(prev_end));
            let shift = shift.min(next.start.saturating_sub(prev_end));
            for later in &mut track.clips[c.index..] {
                later.start -= shift;
            }
        }
        Some(clip)
    }

    /// The join near `t` on track `r`: the index of the later clip of two
    /// that touch or overlap, if `t` is within `tolerance` of the join.
    pub fn join_near(&self, r: TrackRef, t: Duration, tolerance: Duration) -> Option<usize> {
        let track = self.track(r)?;
        (1..track.clips.len()).filter(|&i| track.joined(i)).find(|&i| {
            let b = &track.clips[i];
            let a_end = track.clips[i - 1].end();
            let (from, to) = (b.start.saturating_sub(tolerance), a_end.max(b.start) + tolerance);
            from <= t && t <= to
        })
    }

    /// Set the transition between clip `c` and the clip before it (they must
    /// touch or overlap). `None` or a zero length makes it a plain cut. The
    /// clip and everything after it on its track move so the two overlap by
    /// the transition's length (at most half of either clip).
    pub fn set_transition(&mut self, c: ClipRef, transition: Option<(TransitionKind, Duration)>) -> bool {
        let Some(track) = self.track_mut(c.track) else { return false };
        if !track.joined(c.index) {
            return false;
        }
        let current = track.overlap_before(c.index);
        let (prev_len, clip_len) = (track.clips[c.index - 1].len(), track.clips[c.index].len());
        // The earlier clip may itself overlap the one before it.
        let prev_free = prev_len.saturating_sub(track.overlap_before(c.index - 1));
        let wanted = match transition {
            Some((kind, d)) if !d.is_zero() => {
                track.clips[c.index].transition = kind;
                d.min(prev_len / 2).min(clip_len / 2).min(prev_free)
            }
            _ => Duration::ZERO,
        };
        let later = &mut track.clips[c.index..];
        if wanted > current {
            for l in later {
                l.start -= wanted - current;
            }
        } else {
            for l in later {
                l.start += current - wanted;
            }
        }
        true
    }

    /// Frame size and rate: the first video (or picture) with a known size;
    /// 1280x720 at 30 fps otherwise.
    pub fn suggested_canvas(&self) -> Canvas {
        let clips = || self.video.iter().flat_map(|t| t.clips.iter());
        let first =
            clips().filter(|c| c.width.is_some() && c.height.is_some()).min_by_key(|c| c.kind != ClipKind::Video);
        match first {
            Some(c) => {
                let (w, h) = (c.width.unwrap_or(1280), c.height.unwrap_or(720));
                let (w, h) = if c.transform.swaps_sides() { (h, w) } else { (w, h) };
                Canvas {
                width: w,
                height: h,
                fps: clips().find_map(|c| c.fps).unwrap_or(30.0),
            }
            .normalized()
            }
            None => Canvas::default(),
        }
    }

    /// Every file the project uses.
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.tracks()
            .flat_map(|(_, t)| t.clips.iter().filter(|c| c.has_file()).map(|c| c.path.as_path()))
            .chain(self.subtitle.as_deref())
    }

    /// Change a clip's speed (videos and sound). Its start stays put; if
    /// it gets longer, the clips after it on its track move along to make
    /// room.
    pub fn set_speed(&mut self, c: ClipRef, speed: f64) -> bool {
        let Some(track) = self.track_mut(c.track) else { return false };
        let Some(clip) = track.clips.get_mut(c.index) else { return false };
        if clip.is_still() || !speed.is_finite() {
            return false;
        }
        let old_end = clip.end();
        clip.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        if clip.len() < MIN_CLIP {
            clip.out_point = clip.in_point + clip.source_span(MIN_CLIP);
        }
        clip.clamp_fades();
        let new_end = clip.end();
        if let Some(next) = track.clips.get(c.index + 1) {
            let push = new_end.saturating_sub(next.start.max(old_end));
            if !push.is_zero() {
                for later in &mut track.clips[c.index + 1..] {
                    later.start += push;
                }
            }
        }
        true
    }

    /// A copy of a clip straight after it (or as near as there's room).
    pub fn duplicate(&mut self, c: ClipRef) -> Option<ClipRef> {
        let mut copy = self.clip(c)?.clone();
        copy.transition = TransitionKind::default();
        let at = copy.end();
        self.add_clip(c.track, copy, at)
    }

    /// Put a video's sound on an audio track of its own, under the video
    /// (which is muted). Returns the new sound clip.
    pub fn detach_audio(&mut self, c: ClipRef) -> Option<ClipRef> {
        if c.track.kind != TrackKind::Video {
            return None;
        }
        let clip = self.clip(c)?;
        if clip.kind != ClipKind::Video || !clip.has_audio || clip.muted {
            return None;
        }
        let mut sound = clip.clone();
        sound.transform = Transform::default();
        sound.crop = Crop::default();
        sound.effects = Effects::default();
        sound.opacity = 1.0;
        sound.transition = TransitionKind::default();
        let (from, to) = (sound.start, sound.end());
        let free = (0..self.audio.len()).find(|&i| self.audio[i].is_free(from, to, None));
        let r = match free {
            Some(i) => TrackRef::audio(i),
            None => self.add_track(TrackKind::Audio),
        };
        let placed = self.add_clip(r, sound, from)?;
        self.clip_mut(c)?.muted = true;
        Some(placed)
    }
}

/// Output frame size and rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

impl Default for Canvas {
    fn default() -> Self {
        Self { width: 1280, height: 720, fps: 30.0 }
    }
}

impl Canvas {
    /// Even sizes, sensible frame rate.
    pub fn normalized(self) -> Self {
        let even = |v: u32| (v.clamp(16, 7680) / 2) * 2;
        let fps = if self.fps.is_finite() { self.fps.clamp(1.0, 120.0) } else { 30.0 };
        Self { width: even(self.width), height: even(self.height), fps }
    }

    /// The same shape, fitted inside `max_w` x `max_h`, at most `max_fps`.
    pub fn fit_within(self, max_w: u32, max_h: u32, max_fps: f64) -> Self {
        let c = self.normalized();
        let scale = (max_w as f64 / c.width as f64).min(max_h as f64 / c.height as f64).min(1.0);
        Self {
            width: (c.width as f64 * scale).round() as u32,
            height: (c.height as f64 * scale).round() as u32,
            fps: c.fps.min(max_fps),
        }
        .normalized()
    }
}

/// Durations as seconds in project files.
mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64((d.as_secs_f64() * 1e6).round() / 1e6)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let v = f64::deserialize(d)?;
        Ok(if v.is_finite() && v > 0.0 { Duration::from_secs_f64(v.min(1e9)) } else { Duration::ZERO })
    }
}

mod opt_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => super::secs::serialize(d, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let v = Option::<f64>::deserialize(d)?;
        Ok(v.filter(|v| v.is_finite() && *v > 0.0).map(|v| Duration::from_secs_f64(v.min(1e9))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// V1: a.mp4 (0-10), b.png (10-14), c.mp4 (14-20). A1: music (2-32).
    fn project() -> Timeline {
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("a.mp4".into(), s(10), true)).unwrap();
        t.append(TrackRef::V1, Clip::image("b.png".into())).unwrap();
        t.append(TrackRef::V1, Clip::video("c.mp4".into(), s(6), false)).unwrap();
        t.add_clip(TrackRef::A1, Clip::audio("m.mp3".into(), s(30)), s(2)).unwrap();
        t
    }

    fn starts(t: &Timeline, r: TrackRef) -> Vec<Duration> {
        t.track(r).unwrap().clips.iter().map(|c| c.start).collect()
    }

    fn v1(i: usize) -> ClipRef {
        ClipRef { track: TrackRef::V1, index: i }
    }

    #[test]
    fn appending_and_duration() {
        let t = project();
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(10), s(14)]);
        assert_eq!(t.duration(), s(32), "the music runs longest");
        assert_eq!(t.track(TrackRef::V1).unwrap().clip_at(ms(10_500)), Some(1));
        // Pictures can't go on audio tracks, sound files can't go on video tracks.
        let mut t2 = t.clone();
        assert!(t2.append(TrackRef::A1, Clip::image("x.png".into())).is_none());
        assert!(t2.append(TrackRef::V1, Clip::audio("x.mp3".into(), s(1))).is_none());
        // A video's sound can go on an audio track.
        assert!(t2.append(TrackRef::A1, Clip::video("v.mp4".into(), s(2), true)).is_some());
        assert!(t2.append(TrackRef::A1, Clip::video("silent.mp4".into(), s(2), false)).is_none());
    }

    #[test]
    fn dropping_onto_a_clip_finds_the_nearest_free_place() {
        let mut t = project();
        // A 3s clip dropped over the picture: the nearest free spot is
        // after c (20s).
        let c = t.add_clip(TrackRef::V1, Clip::video("d.mp4".into(), s(3), false), s(11)).unwrap();
        assert_eq!(t.clip(c).unwrap().start, s(20));
        // In a gap it goes exactly where dropped, in start order.
        let mut t = project();
        t.remove(v1(1));
        let c = t.add_clip(TrackRef::V1, Clip::video("d.mp4".into(), s(2), false), s(11)).unwrap();
        assert_eq!(t.clip(c).unwrap().start, s(11));
        assert_eq!(c.index, 1);
    }

    #[test]
    fn move_within_and_between_tracks() {
        let mut t = project();
        t.add_track(TrackKind::Video);
        // a (10s) to V2 at 3s.
        let c = t.move_clip(v1(0), TrackRef::video(1), s(3)).unwrap();
        assert_eq!(c, ClipRef { track: TrackRef::video(1), index: 0 });
        assert_eq!(t.clip(c).unwrap().start, s(3));
        assert_eq!(starts(&t, TrackRef::V1), vec![s(10), s(14)]);
        // Video tracks to audio tracks: no.
        assert!(t.move_clip(v1(0), TrackRef::A1, s(0)).is_none());
        // Within V1: c (6s) to 0s.
        let c = t.move_clip(v1(1), TrackRef::V1, Duration::ZERO).unwrap();
        assert_eq!((c.index, t.clip(c).unwrap().start), (0, s(0)));
        // Onto the picture: it goes next to it instead.
        let c = t.move_clip(c, TrackRef::V1, s(8)).unwrap();
        assert_eq!(t.clip(c).unwrap().start, s(4), "ends where the picture starts");
    }

    #[test]
    fn trimming_edges() {
        let mut t = project();
        t.clip_mut(v1(0)).unwrap().fade_out = s(3);
        // Shorten a from the left: the in point follows.
        assert!(t.trim(v1(0), Edge::Start, s(2)));
        let a = t.clip(v1(0)).unwrap();
        assert_eq!((a.start, a.in_point, a.out_point), (s(2), s(2), s(10)));
        // Can't reveal more than the source has.
        t.trim(v1(0), Edge::Start, Duration::ZERO);
        assert_eq!(t.clip(v1(0)).unwrap().in_point, Duration::ZERO);
        // The right edge can't grow into the picture or past the source.
        t.trim(v1(0), Edge::End, s(5));
        assert_eq!(t.clip(v1(0)).unwrap().end(), s(5));
        t.trim(v1(0), Edge::End, s(12));
        assert_eq!(t.clip(v1(0)).unwrap().end(), s(10));
        // Never below the minimum; fades shrink to fit.
        t.trim(v1(0), Edge::End, Duration::ZERO);
        let a = t.clip(v1(0)).unwrap();
        assert_eq!((a.len(), a.fade_out), (MIN_CLIP, MIN_CLIP));
        // A picture can be any length: its left edge goes back to a's end.
        t.trim(v1(1), Edge::Start, Duration::ZERO);
        let b = t.clip(v1(1)).unwrap();
        assert_eq!((b.start, b.len(), b.in_point), (MIN_CLIP, s(14) - MIN_CLIP, Duration::ZERO));
    }

    #[test]
    fn split_and_split_all() {
        let mut t = project();
        t.clip_mut(v1(0)).unwrap().fade_in = s(1);
        t.clip_mut(v1(0)).unwrap().fade_out = s(1);
        let r = t.split(v1(0), s(3)).unwrap();
        assert_eq!(r, v1(1));
        let (a, b) = (t.clip(v1(0)).unwrap(), t.clip(v1(1)).unwrap());
        assert_eq!((a.start, a.in_point, a.out_point, a.fade_in, a.fade_out), (s(0), s(0), s(3), s(1), s(0)));
        assert_eq!((b.start, b.in_point, b.out_point, b.fade_in, b.fade_out), (s(3), s(3), s(10), s(0), s(1)));
        assert_eq!(t.duration(), s(32), "splitting keeps the length");
        // Too close to an edge.
        assert!(t.split(v1(1), ms(3_050)).is_none());
        // Razor through everything at 12s: the picture and the music.
        assert_eq!(t.split_all(s(12)), 2);
        let pics: Vec<Duration> = t.video[0].clips.iter().filter(|c| c.is_still()).map(Clip::len).collect();
        assert_eq!(pics, vec![s(2), s(2)]);
        assert_eq!(starts(&t, TrackRef::A1), vec![s(2), s(12)]);
        assert_eq!(t.audio[0].clips[1].in_point, s(10));
    }

    #[test]
    fn delete_and_ripple_delete() {
        let mut t = project();
        let mut lifted = t.clone();
        lifted.remove(v1(1));
        assert_eq!(starts(&lifted, TrackRef::V1), vec![s(0), s(14)], "a gap is left");
        t.ripple_remove(v1(1)).unwrap();
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(10)], "the gap closes");
        assert_eq!(starts(&t, TrackRef::A1), vec![s(2)], "other tracks stay put");
        // A gap before the removed clip stays.
        let mut t = project();
        t.remove(v1(0));
        t.ripple_remove(v1(0)).unwrap();
        assert_eq!(starts(&t, TrackRef::V1), vec![s(10)]);
    }

    #[test]
    fn transitions_overlap_the_clips() {
        let mut t = project();
        assert!(t.set_transition(v1(1), Some((TransitionKind::Crossfade, s(1)))));
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(9), s(13)], "b and everything after moves left");
        assert_eq!(t.video[0].overlap_before(1), s(1));
        assert_eq!(t.join_near(TrackRef::V1, ms(9_500), ms(200)), Some(1));
        // Never more than half the shorter clip (the picture: 2s).
        t.set_transition(v1(2), Some((TransitionKind::DipToBlack, s(30))));
        assert_eq!(t.video[0].overlap_before(2), s(2));
        assert_eq!(t.video[0].clips[2].transition, TransitionKind::DipToBlack);
        // Back to a cut.
        t.set_transition(v1(1), None);
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(10), s(12)]);
        // Clips that don't touch can't have one.
        t.remove(v1(1));
        assert!(!t.set_transition(v1(1), Some((TransitionKind::Crossfade, s(1)))));
    }

    #[test]
    fn canvas_follows_first_video() {
        let mut t = project();
        assert_eq!(t.suggested_canvas(), Canvas::default());
        t.video[0].clips[1].width = Some(1001);
        t.video[0].clips[1].height = Some(500);
        t.video[0].clips[2].width = Some(641);
        t.video[0].clips[2].height = Some(361);
        t.video[0].clips[2].fps = Some(25.0);
        assert_eq!(t.suggested_canvas(), Canvas { width: 640, height: 360, fps: 25.0 });
        let small = Canvas { width: 1920, height: 1080, fps: 60.0 }.fit_within(960, 540, 30.0);
        assert_eq!(small, Canvas { width: 960, height: 540, fps: 30.0 });
    }

    #[test]
    fn project_json_round_trip() {
        let mut t = project();
        t.set_transition(v1(1), Some((TransitionKind::DipToBlack, s(1))));
        t.audio[0].muted = true;
        t.clip_mut(v1(0)).unwrap().volume = 1.5;
        t.subtitle = Some("subs.srt".into());
        t.subtitle_export = SubtitleExport::Burn;
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"start\":9.0"), "{json}");
        let back: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn turning_and_mirroring_compose_as_on_screen() {
        let t = Transform::default();
        assert!(t.is_identity());
        let r = t.rotated_cw();
        assert_eq!((r.rotate, r.swaps_sides()), (90, true));
        assert_eq!(r.rotated_ccw(), t);
        assert_eq!(t.rotated_180().rotated_180(), t);
        assert_eq!(t.rotated_cw().rotated_cw().rotated_cw().rotated_cw(), t);
        // Mirrored both ways is a half turn, kept in one form.
        assert_eq!(t.flipped_h().flipped_v(), Transform { rotate: 180, ..t });
        assert_eq!(t.flipped_h().flipped_h(), t);
        // Mirroring left-right, then turning, is turning then mirroring
        // top-bottom.
        assert_eq!(t.flipped_h().rotated_cw(), Transform { rotate: 90, flip_h: false, flip_v: true });
        assert_eq!(t.rotated_cw().flipped_v(), t.flipped_h().rotated_cw());
    }

    #[test]
    fn crop_is_kept_in_range() {
        let c = Crop { left: -1.0, right: 0.9, top: f32::NAN, bottom: 0.1 }.clamped();
        assert_eq!(c, Crop { left: 0.0, right: Crop::MAX, top: 0.0, bottom: 0.1 });
        assert!(Crop::default().is_none() && Crop { left: -0.5, ..Crop::default() }.is_none());
    }

    #[test]
    fn speed_changes_the_length_and_pushes_later_clips() {
        let mut t = project();
        // a (10s) at double speed: 5s, the picture stays where it is.
        assert!(t.set_speed(v1(0), 2.0));
        let a = t.clip(v1(0)).unwrap();
        assert_eq!((a.len(), a.out_point), (s(5), s(10)));
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(10), s(14)]);
        // At half speed it's 20s long: the picture and c move along.
        t.set_speed(v1(0), 0.5);
        assert_eq!(t.clip(v1(0)).unwrap().len(), s(20));
        assert_eq!(starts(&t, TrackRef::V1), vec![s(0), s(20), s(24)]);
        // Never slower than a quarter; pictures have no speed.
        t.set_speed(v1(0), 0.01);
        assert_eq!(t.clip(v1(0)).unwrap().speed(), MIN_SPEED);
        assert!(!t.set_speed(v1(1), 2.0));
        // Trimming and splitting work in timeline time, the source follows.
        let mut t = project();
        t.set_speed(v1(0), 2.0);
        t.trim(v1(0), Edge::End, s(4));
        assert_eq!(t.clip(v1(0)).unwrap().out_point, s(8));
        t.trim(v1(0), Edge::End, s(9));
        assert_eq!(t.clip(v1(0)).unwrap().end(), s(5), "no more than the source has");
        t.trim(v1(0), Edge::Start, s(1));
        let a = t.clip(v1(0)).unwrap();
        assert_eq!((a.start, a.in_point, a.len()), (s(1), s(2), s(4)));
        let b = t.split(v1(0), s(3)).unwrap();
        assert_eq!((t.clip(v1(0)).unwrap().out_point, t.clip(b).unwrap().in_point), (s(6), s(6)));
        assert_eq!(t.clip(b).unwrap().len(), s(2));
    }

    #[test]
    fn duplicate_and_detach_audio() {
        let mut t = project();
        let c = t.duplicate(v1(1)).unwrap();
        // No room straight after the picture (c is there): after c instead.
        assert_eq!((c.index, t.clip(c).unwrap().start), (3, s(20)));
        assert_eq!(t.clip(c).unwrap().path, PathBuf::from("b.png"));
        // a's sound goes under it on a track of its own (A1 has music).
        t.clip_mut(v1(0)).unwrap().transform = Transform::default().rotated_cw();
        let snd = t.detach_audio(v1(0)).unwrap();
        assert_eq!(snd.track, TrackRef::audio(1));
        let k = t.clip(snd).unwrap();
        assert_eq!((k.start, k.len(), k.kind, k.transform.is_identity()), (s(0), s(10), ClipKind::Video, true));
        assert!(t.clip(v1(0)).unwrap().muted);
        assert!(t.detach_audio(v1(0)).is_none(), "already done");
        assert!(t.detach_audio(v1(2)).is_none(), "c has no sound");
    }

    #[test]
    fn text_clips_have_no_file() {
        let mut t = project();
        let style = TextStyle { text: "\n  Hello there\nsecond".into(), ..TextStyle::default() };
        let c = t.append(TrackRef::V1, Clip::text(style)).unwrap();
        let k = t.clip(c).unwrap();
        assert_eq!((k.name(), k.len(), k.is_still()), ("T  Hello there".to_string(), DEFAULT_IMAGE_LEN, true));
        assert!(t.files().all(|f| !f.as_os_str().is_empty()));
        assert!(t.append(TrackRef::A1, Clip::text(TextStyle::default())).is_none(), "no text on audio tracks");
        t.trim(c, Edge::End, s(30));
        assert_eq!(t.clip(c).unwrap().len(), s(10));
    }

    #[test]
    fn old_projects_open_with_the_new_settings_off() {
        let old = r#"{"video":[{"kind":"video","clips":[{"path":"a.mp4","kind":"video","out_point":3.0}]}],"audio":[]}"#;
        let t: Timeline = serde_json::from_str(old).unwrap();
        let c = &t.video[0].clips[0];
        assert_eq!((c.speed, c.opacity), (1.0, 1.0));
        assert!(c.transform.is_identity() && c.crop.is_none() && c.effects.is_none() && c.text.is_none());
        // And the new settings survive a round trip.
        let mut t = project();
        let k = t.clip_mut(v1(1)).unwrap();
        k.transform = Transform::default().flipped_v();
        k.effects = Effects { motion: Some(Motion::ZoomIn), enter: Some(Anim::Slide(Side::Left)), exit: Some(Anim::Spin), ..Effects::default() };
        k.opacity = 0.5;
        k.crop.left = 0.1;
        t.set_speed(v1(0), 1.5);
        t.append(TrackRef::V1, Clip::text(TextStyle { text: "Hi".into(), boxed: true, ..TextStyle::default() }));
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Timeline>(&json).unwrap(), t);
        assert!(json.contains(r#""enter":{"slide":"left"}"#), "{json}");
    }
}
