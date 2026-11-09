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
}

fn one() -> f32 {
    1.0
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
        }
    }

    pub fn image(path: PathBuf) -> Self {
        Self { kind: ClipKind::Image, source_len: None, ..Self::video(path, DEFAULT_IMAGE_LEN, false) }
    }

    pub fn audio(path: PathBuf, len: Duration) -> Self {
        Self { kind: ClipKind::Audio, ..Self::video(path, len, true) }
    }

    /// How long the clip plays.
    pub fn len(&self) -> Duration {
        self.out_point.saturating_sub(self.in_point)
    }

    pub fn is_empty(&self) -> bool {
        self.len() < MIN_CLIP
    }

    pub fn end(&self) -> Duration {
        self.start + self.len()
    }

    pub fn name(&self) -> String {
        self.path.file_name().unwrap_or(self.path.as_os_str()).to_string_lossy().into_owned()
    }

    /// True when its sound is heard (given its track isn't muted).
    pub fn audible(&self) -> bool {
        self.has_audio && !self.muted && self.volume > 0.0
    }

    /// Can be stretched to any length (pictures).
    pub fn is_still(&self) -> bool {
        self.kind == ClipKind::Image
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
            TrackKind::Video => matches!(clip.kind, ClipKind::Video | ClipKind::Image),
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
                    lower = lower.max(start.saturating_sub(clip.in_point));
                }
                let upper = end.saturating_sub(MIN_CLIP).max(lower);
                let new = t.clamp(lower, upper);
                let clip = &mut track.clips[c.index];
                if clip.is_still() {
                    clip.out_point = end - new;
                } else if new < start {
                    clip.in_point -= start - new;
                } else {
                    clip.in_point += new - start;
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
                    upper = upper.min(start + src.saturating_sub(clip.in_point));
                }
                let lower = start + MIN_CLIP;
                let new = t.clamp(lower, upper.max(lower));
                let clip = &mut track.clips[c.index];
                clip.out_point = clip.in_point + (new - start);
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
            left.out_point = clip.in_point + offset;
            right.in_point = clip.in_point + offset;
        }
        right.start = t;
        left.fade_out = Duration::ZERO;
        right.fade_in = Duration::ZERO;
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
            Some(c) => Canvas {
                width: c.width.unwrap_or(1280),
                height: c.height.unwrap_or(720),
                fps: clips().find_map(|c| c.fps).unwrap_or(30.0),
            }
            .normalized(),
            None => Canvas::default(),
        }
    }

    /// Every file the project uses.
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.tracks().flat_map(|(_, t)| t.clips.iter().map(|c| c.path.as_path())).chain(self.subtitle.as_deref())
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
}
