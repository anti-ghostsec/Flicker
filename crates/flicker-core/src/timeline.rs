//! Clips in a row: videos and pictures one after another, each with its own
//! in/out points, volume and fades, joined by cuts, cross-fades or dips to
//! black. **Edit > Append** renders the current video plus the appended file
//! through one ffmpeg filter graph; clips of a different size are fitted
//! with black bars onto the first clip's size and frame rate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Error, Result};

/// Clips can't be split into pieces shorter than this.
pub const MIN_CLIP: Duration = Duration::from_millis(100);
/// How long a picture stays on screen when it's added.
pub const DEFAULT_IMAGE_LEN: Duration = Duration::from_secs(4);
/// Sample rate every clip's sound is converted to before mixing.
const SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    Video,
    /// A still picture, shown for the clip's length.
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TransitionKind {
    /// The two clips blend into each other.
    #[default]
    Crossfade,
    /// The first clip fades to black, then the second fades in.
    DipToBlack,
}

impl TransitionKind {
    fn xfade_name(self) -> &'static str {
        match self {
            Self::Crossfade => "fade",
            Self::DipToBlack => "fadeblack",
        }
    }
}

/// How a clip starts after the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Zero means a plain cut. The clips overlap by this much.
    pub duration: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub path: PathBuf,
    pub kind: ClipKind,
    pub has_audio: bool,
    /// Length of the source video (`None` for pictures or when unknown).
    pub source_len: Option<Duration>,
    /// Source size and frame rate, used to pick the project settings.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Part of the source that's used. For pictures `in_point` is zero and
    /// `out_point` is how long the picture is shown.
    pub in_point: Duration,
    pub out_point: Duration,
    /// 1.0 = unchanged, 0.0 = silent, up to 2.0.
    pub volume: f32,
    pub muted: bool,
    pub fade_in: Duration,
    pub fade_out: Duration,
    /// Transition from the previous clip (ignored on the first clip).
    pub transition: Transition,
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
            in_point: Duration::ZERO,
            out_point: len,
            volume: 1.0,
            muted: false,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
            transition: Transition::default(),
        }
    }

    pub fn image(path: PathBuf) -> Self {
        Self {
            kind: ClipKind::Image,
            source_len: None,
            ..Self::video(path, DEFAULT_IMAGE_LEN, false)
        }
    }

    /// How long the clip plays.
    pub fn len(&self) -> Duration {
        self.out_point.saturating_sub(self.in_point)
    }

    pub fn is_empty(&self) -> bool {
        self.len() < MIN_CLIP
    }

    pub fn name(&self) -> String {
        self.path.file_name().unwrap_or(self.path.as_os_str()).to_string_lossy().into_owned()
    }

    /// Set the in/out points, kept inside the source and at least
    /// [`MIN_CLIP`] apart.
    pub fn set_range(&mut self, in_point: Duration, out_point: Duration) {
        match self.kind {
            ClipKind::Image => {
                self.in_point = Duration::ZERO;
                self.out_point = out_point.saturating_sub(in_point).max(MIN_CLIP);
            }
            ClipKind::Video => {
                let end = self.source_len.unwrap_or(Duration::MAX);
                let out_point = out_point.min(end).max(MIN_CLIP);
                self.out_point = out_point;
                self.in_point = in_point.min(out_point - MIN_CLIP);
            }
        }
        let len = self.len();
        self.fade_in = self.fade_in.min(len);
        self.fade_out = self.fade_out.min(len);
    }

    fn audible(&self) -> bool {
        self.has_audio && !self.muted && self.volume > 0.0
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

}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub clips: Vec<Clip>,
}

impl Timeline {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// The transition that will really be used before clip `i`: none for the
    /// first clip, and never more than half of either neighbour, so a clip
    /// is never eaten up by the transitions on both its ends.
    pub fn effective_transition(&self, i: usize) -> Transition {
        let Some(clip) = self.clips.get(i) else { return Transition::default() };
        if i == 0 {
            return Transition { duration: Duration::ZERO, ..clip.transition };
        }
        let limit = self.clips[i - 1].len().min(clip.len()) / 2;
        Transition { duration: clip.transition.duration.min(limit), ..clip.transition }
    }

    /// Where each clip starts in the finished video.
    pub fn starts(&self) -> Vec<Duration> {
        let mut out = Vec::with_capacity(self.clips.len());
        let mut t = Duration::ZERO;
        for (i, c) in self.clips.iter().enumerate() {
            t = t.saturating_sub(self.effective_transition(i).duration);
            out.push(t);
            t += c.len();
        }
        out
    }

    /// Length of the finished video.
    pub fn duration(&self) -> Duration {
        match (self.starts().last(), self.clips.last()) {
            (Some(s), Some(c)) => *s + c.len(),
            _ => Duration::ZERO,
        }
    }

    /// The clip playing at `t` and how far into it `t` is. During a
    /// transition this is the incoming clip.
    pub fn locate(&self, t: Duration) -> Option<(usize, Duration)> {
        let starts = self.starts();
        let i = starts.iter().rposition(|s| *s <= t)?;
        let offset = (t - starts[i]).min(self.clips[i].len());
        Some((i, offset))
    }

    /// Split the clip under `t` in two. Returns the index of the second half.
    pub fn split_at(&mut self, t: Duration) -> Option<usize> {
        let (i, offset) = self.locate(t)?;
        let clip = &self.clips[i];
        if offset < MIN_CLIP || clip.len().saturating_sub(offset) < MIN_CLIP {
            return None;
        }
        let mut left = clip.clone();
        let mut right = clip.clone();
        let cut = clip.in_point + offset;
        match clip.kind {
            ClipKind::Video => {
                left.out_point = cut;
                right.in_point = cut;
            }
            ClipKind::Image => {
                left.out_point = offset;
                right.out_point = clip.len() - offset;
            }
        }
        left.fade_out = Duration::ZERO;
        left.fade_in = left.fade_in.min(left.len());
        right.fade_in = Duration::ZERO;
        right.fade_out = right.fade_out.min(right.len());
        right.transition = Transition::default();
        self.clips[i] = left;
        self.clips.insert(i + 1, right);
        Some(i + 1)
    }

    pub fn remove(&mut self, i: usize) -> Option<Clip> {
        (i < self.clips.len()).then(|| self.clips.remove(i))
    }

    /// Move clip `from` so it ends up at index `to`.
    pub fn move_clip(&mut self, from: usize, to: usize) -> bool {
        if from >= self.clips.len() || to >= self.clips.len() || from == to {
            return false;
        }
        let c = self.clips.remove(from);
        self.clips.insert(to, c);
        true
    }

    /// Project settings taken from the first video (or picture) with a known
    /// size; 1280x720 at 30 fps otherwise.
    pub fn suggested_canvas(&self) -> Canvas {
        let first = self
            .clips
            .iter()
            .filter(|c| c.width.is_some() && c.height.is_some())
            .min_by_key(|c| c.kind != ClipKind::Video);
        match first {
            Some(c) => Canvas {
                width: c.width.unwrap_or(1280),
                height: c.height.unwrap_or(720),
                fps: self.clips.iter().find_map(|c| c.fps).unwrap_or(30.0),
            }
            .normalized(),
            None => Canvas::default(),
        }
    }

}

/// Which encoders a render uses. The container comes from the output's
/// extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// H.264 video, AAC sound: plays everywhere (MP4, MOV or MKV).
    #[default]
    Mp4,
    /// VP9 video, Opus sound.
    WebM,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::WebM => "webm",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" | "mov" | "mkv" => Some(Self::Mp4),
            "webm" => Some(Self::WebM),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub timeline: Timeline,
    pub canvas: Canvas,
    pub output: PathBuf,
    pub format: ExportFormat,
    /// Encoder quality: lower is better and bigger.
    pub crf: u8,
    /// x264 speed preset (MP4 only).
    pub preset: String,
    /// Write encoder/date tags (off: see [`crate::privacy`]).
    pub keep_tags: bool,
}

impl ExportRequest {
    pub fn new(timeline: Timeline, output: PathBuf) -> Self {
        let format = ExportFormat::from_path(&output).unwrap_or_default();
        Self {
            canvas: timeline.suggested_canvas(),
            timeline,
            output,
            format,
            crf: 20,
            preset: "medium".into(),
            keep_tags: false,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.timeline.clips.iter().all(Clip::is_empty) {
            return Err(Error::NothingToKeep);
        }
        for c in &self.timeline.clips {
            if !c.path.is_file() {
                return Err(Error::MissingFile(c.path.clone()));
            }
            if crate::subtitles::same_file(&c.path, &self.output) {
                return Err(Error::OutputIsInput);
            }
        }
        Ok(())
    }
}

fn secs(d: Duration) -> String {
    format!("{:.6}", d.as_secs_f64())
}

/// ffmpeg inputs and a `-filter_complex` graph that renders `timeline`
/// onto `canvas`, ending in the labels `[vout]` (and `[aout]` with audio).
pub(crate) fn render_graph(timeline: &Timeline, canvas: Canvas, audio: bool) -> (Vec<OsString>, String) {
    let canvas = canvas.normalized();
    let (w, h, fps) = (canvas.width, canvas.height, canvas.fps);
    let clips: Vec<(usize, &Clip)> =
        timeline.clips.iter().enumerate().filter(|(_, c)| !c.is_empty()).collect();
    let mut inputs: Vec<OsString> = Vec::new();
    let mut graph: Vec<String> = Vec::new();

    for (n, (_, c)) in clips.iter().enumerate() {
        let len = secs(c.len());
        match c.kind {
            ClipKind::Video => {
                if !c.in_point.is_zero() {
                    inputs.push("-ss".into());
                    inputs.push(secs(c.in_point).into());
                }
                inputs.push("-t".into());
                inputs.push(len.clone().into());
            }
            ClipKind::Image => {
                inputs.extend(["-loop", "1", "-framerate"].map(OsString::from));
                inputs.push(format!("{fps}").into());
                inputs.push("-t".into());
                inputs.push(len.clone().into());
            }
        }
        inputs.push("-i".into());
        inputs.push(c.path.clone().into());

        let mut v = format!(
            "[{n}:v:0]scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p,\
             trim=duration={len},setpts=PTS-STARTPTS,fps={fps}"
        );
        if !c.fade_in.is_zero() {
            v += &format!(",fade=t=in:st=0:d={}", secs(c.fade_in));
        }
        if !c.fade_out.is_zero() {
            v += &format!(",fade=t=out:st={}:d={}", secs(c.len().saturating_sub(c.fade_out)), secs(c.fade_out));
        }
        graph.push(format!("{v}[v{n}]"));

        if audio {
            let format = format!("aformat=sample_fmts=fltp:sample_rates={SAMPLE_RATE}:channel_layouts=stereo");
            let mut a = if c.audible() {
                format!(
                    "[{n}:a:0]aresample={SAMPLE_RATE},{format},asetpts=PTS-STARTPTS,volume={:.3},\
                     apad=whole_dur={len},atrim=duration={len}",
                    c.volume
                )
            } else {
                format!("anullsrc=r={SAMPLE_RATE}:cl=stereo,{format},atrim=duration={len}")
            };
            if c.audible() && !c.fade_in.is_zero() {
                a += &format!(",afade=t=in:st=0:d={}", secs(c.fade_in));
            }
            if c.audible() && !c.fade_out.is_zero() {
                a += &format!(",afade=t=out:st={}:d={}", secs(c.len().saturating_sub(c.fade_out)), secs(c.fade_out));
            }
            graph.push(format!("{a}[a{n}]"));
        }
    }

    // Join the clips one by one: concat for cuts, xfade/acrossfade for
    // transitions. setpts and concat forget the frame rate, which xfade
    // needs on both inputs, so every chain and join ends with fps=.
    let filtered = Timeline { clips: clips.iter().map(|(_, c)| (*c).clone()).collect() };
    let mut cur_len = filtered.clips.first().map(Clip::len).unwrap_or_default();
    for n in 1..filtered.clips.len() {
        let (pv, pa) = if n == 1 { ("v0".to_string(), "a0".to_string()) } else { (format!("vj{}", n - 1), format!("aj{}", n - 1)) };
        let t = filtered.effective_transition(n);
        if t.duration.is_zero() {
            if audio {
                graph.push(format!("[{pv}][{pa}][v{n}][a{n}]concat=n=2:v=1:a=1[vc{n}][aj{n}]"));
            } else {
                graph.push(format!("[{pv}][v{n}]concat=n=2:v=1:a=0[vc{n}]"));
            }
            graph.push(format!("[vc{n}]fps={fps}[vj{n}]"));
        } else {
            let offset = cur_len.saturating_sub(t.duration);
            graph.push(format!(
                "[{pv}][v{n}]xfade=transition={}:duration={}:offset={},fps={fps}[vj{n}]",
                t.kind.xfade_name(),
                secs(t.duration),
                secs(offset)
            ));
            if audio {
                graph.push(format!("[{pa}][a{n}]acrossfade=d={}[aj{n}]", secs(t.duration)));
            }
        }
        cur_len = cur_len.saturating_sub(t.duration) + filtered.clips[n].len();
    }
    let last = filtered.clips.len().saturating_sub(1);
    let (lv, la) = if last == 0 { ("v0".to_string(), "a0".to_string()) } else { (format!("vj{last}"), format!("aj{last}")) };
    graph.push(format!("[{lv}]null[vout]"));
    if audio {
        graph.push(format!("[{la}]anull[aout]"));
    }
    (inputs, graph.join(";"))
}

fn strs(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

/// Full ffmpeg argument list for an export.
pub fn export_args(req: &ExportRequest) -> Vec<OsString> {
    let (inputs, graph) = render_graph(&req.timeline, req.canvas, true);
    let mut a = strs(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    a.extend(inputs);
    a.push("-filter_complex".into());
    a.push(graph.into());
    a.extend(strs(&["-map", "[vout]", "-map", "[aout]"]));
    let crf = req.crf.to_string();
    match req.format {
        ExportFormat::Mp4 => {
            a.extend(strs(&["-c:v", "libx264", "-crf", &crf, "-preset", &req.preset, "-pix_fmt", "yuv420p"]));
            a.extend(strs(&["-c:a", "aac", "-b:a", "192k"]));
        }
        ExportFormat::WebM => {
            a.extend(strs(&["-c:v", "libvpx-vp9", "-crf", &crf, "-b:v", "0", "-row-mt", "1"]));
            a.extend(strs(&["-deadline", "good", "-cpu-used", "4", "-pix_fmt", "yuv420p"]));
            a.extend(strs(&["-c:a", "libopus", "-b:a", "128k"]));
        }
    }
    a.extend(crate::privacy::output_args(req.keep_tags, true, req.format == ExportFormat::Mp4));
    // The container follows the file name (working files may be MKV or MOV).
    if crate::privacy::is_mp4_family(&req.output) {
        a.extend(strs(&["-movflags", "+faststart"]));
    }
    a.push(req.output.clone().into());
    a
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

    fn three() -> Timeline {
        Timeline {
            clips: vec![
                Clip::video("a.mp4".into(), s(10), true),
                Clip::image("b.png".into()),
                Clip::video("c.mp4".into(), s(6), false),
            ],
        }
    }

    #[test]
    fn durations_and_starts() {
        let mut t = three();
        assert_eq!(t.duration(), s(20));
        assert_eq!(t.starts(), vec![s(0), s(10), s(14)]);
        t.clips[1].transition.duration = s(1);
        // Never more than half the shorter neighbour.
        t.clips[2].transition.duration = s(30);
        assert_eq!(t.effective_transition(2).duration, s(2));
        assert_eq!(t.starts(), vec![s(0), s(9), s(11)]);
        assert_eq!(t.duration(), s(17));
        // The first clip never has a transition.
        t.clips[0].transition.duration = s(1);
        assert_eq!(t.effective_transition(0).duration, s(0));
    }

    #[test]
    fn locate_finds_clip_and_offset() {
        let t = three();
        assert_eq!(t.locate(s(0)), Some((0, s(0))));
        assert_eq!(t.locate(ms(10_500)), Some((1, ms(500))));
        assert_eq!(t.locate(s(19)), Some((2, s(5))));
    }

    #[test]
    fn split_video_and_image() {
        let mut t = three();
        t.clips[0].in_point = s(2); // plays 2..10
        t.clips[0].fade_in = s(1);
        t.clips[0].fade_out = s(1);
        assert_eq!(t.split_at(s(3)), Some(1));
        assert_eq!(t.clips.len(), 4);
        assert_eq!((t.clips[0].in_point, t.clips[0].out_point), (s(2), s(5)));
        assert_eq!((t.clips[1].in_point, t.clips[1].out_point), (s(5), s(10)));
        assert_eq!((t.clips[0].fade_in, t.clips[0].fade_out), (s(1), s(0)));
        assert_eq!((t.clips[1].fade_in, t.clips[1].fade_out), (s(0), s(1)));
        assert_eq!(t.duration(), s(18), "splitting keeps the length");

        // The picture now starts at 8s; split it 1s in.
        assert_eq!(t.split_at(s(9)), Some(3));
        assert_eq!(t.clips[2].len(), s(1));
        assert_eq!(t.clips[3].len(), s(3));
        assert_eq!(t.clips[3].kind, ClipKind::Image);

        // Too close to an edge.
        assert_eq!(t.split_at(ms(8_050)), None);
    }

    #[test]
    fn move_and_remove() {
        let mut t = three();
        assert!(t.move_clip(2, 0));
        assert_eq!(t.clips[0].path, PathBuf::from("c.mp4"));
        assert!(!t.move_clip(0, 9));
        assert_eq!(t.remove(1).unwrap().path, PathBuf::from("a.mp4"));
        assert_eq!(t.clips.len(), 2);
    }

    #[test]
    fn set_range_stays_inside_source() {
        let mut c = Clip::video("a.mp4".into(), s(10), true);
        c.set_range(s(8), s(20));
        assert_eq!((c.in_point, c.out_point), (s(8), s(10)));
        c.set_range(s(10), s(10));
        assert_eq!(c.len(), MIN_CLIP);
        let mut i = Clip::image("b.png".into());
        i.set_range(Duration::ZERO, s(7));
        assert_eq!(i.len(), s(7));
    }

    #[test]
    fn canvas_follows_first_video() {
        let mut t = three();
        assert_eq!(t.suggested_canvas(), Canvas::default());
        t.clips[1].width = Some(1001);
        t.clips[1].height = Some(500);
        t.clips[2].width = Some(641);
        t.clips[2].height = Some(361);
        t.clips[2].fps = Some(25.0);
        assert_eq!(t.suggested_canvas(), Canvas { width: 640, height: 360, fps: 25.0 });
    }

    #[test]
    fn graph_uses_concat_and_xfade() {
        let mut t = three();
        t.clips[0].volume = 0.5;
        t.clips[1].fade_in = s(1);
        t.clips[2].transition = Transition { kind: TransitionKind::DipToBlack, duration: s(1) };
        let (inputs, g) = render_graph(&t, Canvas::default(), true);
        let inputs: Vec<String> = inputs.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let inputs = inputs.join(" ");
        assert!(inputs.contains("-t 10.000000 -i a.mp4"), "{inputs}");
        assert!(inputs.contains("-loop 1 -framerate 30 -t 4.000000 -i b.png"), "{inputs}");
        assert!(g.contains("[0:a:0]aresample=48000"), "{g}");
        assert!(g.contains("volume=0.500"), "{g}");
        // Pictures and clips without sound get silence.
        assert!(g.contains("anullsrc=r=48000:cl=stereo,aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,atrim=duration=4.000000[a1]"), "{g}");
        assert!(g.contains("[v1]fade=t=in") || g.contains(",fade=t=in:st=0:d=1.000000[v1]"), "{g}");
        assert!(g.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1"), "{g}");
        assert!(g.contains("[vj1][v2]xfade=transition=fadeblack:duration=1.000000:offset=13.000000"), "{g}");
        assert!(g.contains("[aj1][a2]acrossfade=d=1.000000[aj2]"), "{g}");
        assert!(g.ends_with("[vj2]null[vout];[aj2]anull[aout]"), "{g}");
    }

    #[test]
    fn muted_clip_is_silent_and_single_clip_works() {
        let mut t = Timeline { clips: vec![Clip::video("a.mp4".into(), s(3), true)] };
        t.clips[0].muted = true;
        let (_, g) = render_graph(&t, Canvas::default(), true);
        assert!(!g.contains("[0:a:0]"), "{g}");
        assert!(g.ends_with("[v0]null[vout];[a0]anull[aout]"), "{g}");
        let (_, g) = render_graph(&t, Canvas::default(), false);
        assert!(!g.contains("anullsrc"), "{g}");
    }

    #[test]
    fn export_args_strip_tags() {
        let t = three();
        let mut req = ExportRequest::new(t, "out.webm".into());
        assert_eq!(req.format, ExportFormat::WebM);
        let a: Vec<String> = export_args(&req).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let a = a.join(" ");
        assert!(a.contains("libvpx-vp9") && a.contains("libopus"), "{a}");
        assert!(a.contains("-map_metadata -1") && a.contains("+bitexact"), "{a}");
        req.format = ExportFormat::Mp4;
        req.keep_tags = true;
        let a: Vec<String> = export_args(&req).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let a = a.join(" ");
        assert!(a.contains("libx264") && !a.contains("bitexact"), "{a}");
    }
}
