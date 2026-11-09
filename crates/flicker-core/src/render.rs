//! Turning the timeline into an ffmpeg filter graph, for exports and for the
//! live preview.
//!
//! The graph is the same for both: a black background the length of the
//! rendered stretch, with every visible clip laid over it at its place on
//! the timeline (V1 first, so V2 lands on top), and every audible clip's
//! sound delayed to its place and mixed. A render can cover any stretch of
//! the timeline (`from..to`): the preview plays the timeline as a chain of
//! short stretches, so ffmpeg only ever opens the files that play in the
//! next few seconds.
//!
//! Fades and transitions are timed in each clip's own time ("τ", 0 at the
//! clip's first frame), even when the stretch starts halfway through a clip:
//! the clip's timestamps are shifted to τ before the fade filters run, and
//! to the timeline afterwards.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::timeline::{Canvas, Clip, ClipKind, SubtitleExport, Timeline, TrackKind, TransitionKind};
use crate::{Error, Result};

/// Sample rate of all sound before mixing, and of the preview's sound.
pub const SAMPLE_RATE: u32 = 48_000;

/// What to render.
#[derive(Clone, Debug)]
pub struct RenderOpts {
    pub canvas: Canvas,
    /// The stretch of the timeline to render.
    pub from: Duration,
    pub to: Duration,
    pub video: bool,
    pub audio: bool,
    /// Decoder threads per input (`None` = ffmpeg's choice). The preview
    /// uses one to keep memory low.
    pub input_threads: Option<u32>,
}

/// One clip's part in a render.
struct Piece<'a> {
    clip: &'a Clip,
    /// Video track index (0 = V1); upper tracks pad with transparency.
    layer: usize,
    picture: bool,
    sound: bool,
    /// First rendered moment, in the clip's own time.
    tau0: Duration,
    /// How much of the clip is rendered.
    len: Duration,
    /// Where that starts in the render.
    at: Duration,
    /// Overlap with the clip before / after it on its track, and how they blend.
    ov_in: Duration,
    kind_in: TransitionKind,
    ov_out: Duration,
    kind_out: TransitionKind,
}

fn pieces<'a>(t: &'a Timeline, o: &RenderOpts) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    for (r, track) in t.tracks() {
        for (i, clip) in track.clips.iter().enumerate() {
            if clip.is_empty() || clip.end() <= o.from || clip.start >= o.to {
                continue;
            }
            let picture = o.video && r.kind == TrackKind::Video && !track.hidden && clip.kind != ClipKind::Audio;
            let sound = o.audio && !track.muted && clip.audible();
            if !picture && !sound {
                continue;
            }
            let tau0 = o.from.saturating_sub(clip.start);
            let tau1 = clip.len().min(o.to - clip.start);
            if tau1 <= tau0 {
                continue;
            }
            let next = track.clips.get(i + 1);
            out.push(Piece {
                clip,
                layer: if r.kind == TrackKind::Video { r.index } else { 0 },
                picture,
                sound,
                tau0,
                len: tau1 - tau0,
                at: (clip.start + tau0) - o.from,
                ov_in: track.overlap_before(i),
                kind_in: clip.transition,
                ov_out: track.overlap_before(i + 1),
                kind_out: next.map(|n| n.transition).unwrap_or_default(),
            });
        }
    }
    // Pictures are laid on in track order: V1 first, V2 over it.
    out.sort_by_key(|p| p.layer);
    out
}

/// Decoding starts this much before a seek point and the extra is cut off
/// in the graph: some decoders (MP3) give silence for their first frames
/// after a seek, which would be a gap at every preview stretch.
const PREROLL: Duration = Duration::from_millis(500);

fn preroll(p: &Piece) -> Duration {
    match p.clip.kind {
        ClipKind::Image => Duration::ZERO,
        _ => PREROLL.min(p.clip.in_point + p.tau0),
    }
}

fn secs(d: Duration) -> String {
    format!("{:.6}", d.as_secs_f64())
}

fn samples(d: Duration) -> u64 {
    (d.as_secs_f64() * SAMPLE_RATE as f64).round() as u64
}

#[derive(Clone, Copy, PartialEq)]
enum Fade {
    In,
    Out,
}

/// (direction, start, length) in the clip's own time.
type FadeSpec = (Fade, Duration, Duration);

/// Fades the user set plus the ones transitions need. Picture fades also
/// say whether they fade to transparent (`true`) or to black.
fn picture_fades(p: &Piece, upper: bool) -> Vec<(FadeSpec, bool)> {
    let l = p.clip.len();
    let mut f = Vec::new();
    if !p.clip.fade_in.is_zero() {
        f.push(((Fade::In, Duration::ZERO, p.clip.fade_in), upper));
    }
    if !p.clip.fade_out.is_zero() {
        f.push(((Fade::Out, l - p.clip.fade_out, p.clip.fade_out), upper));
    }
    if !p.ov_in.is_zero() {
        match p.kind_in {
            // Fades in over the clip before it.
            TransitionKind::Crossfade => f.push(((Fade::In, Duration::ZERO, p.ov_in), true)),
            // Black for the first half (hidden then), in from black after.
            TransitionKind::DipToBlack => f.push(((Fade::In, p.ov_in / 2, p.ov_in / 2), false)),
        }
    }
    if !p.ov_out.is_zero() && p.kind_out == TransitionKind::DipToBlack {
        f.push(((Fade::Out, l - p.ov_out, p.ov_out / 2), false));
    }
    f
}

fn sound_fades(p: &Piece) -> Vec<FadeSpec> {
    let l = p.clip.len();
    let mut f = Vec::new();
    if !p.clip.fade_in.is_zero() {
        f.push((Fade::In, Duration::ZERO, p.clip.fade_in));
    }
    if !p.clip.fade_out.is_zero() {
        f.push((Fade::Out, l - p.clip.fade_out, p.clip.fade_out));
    }
    if !p.ov_in.is_zero() {
        f.push(match p.kind_in {
            TransitionKind::Crossfade => (Fade::In, Duration::ZERO, p.ov_in),
            TransitionKind::DipToBlack => (Fade::In, p.ov_in / 2, p.ov_in / 2),
        });
    }
    if !p.ov_out.is_zero() {
        f.push(match p.kind_out {
            TransitionKind::Crossfade => (Fade::Out, l - p.ov_out, p.ov_out),
            TransitionKind::DipToBlack => (Fade::Out, l - p.ov_out, p.ov_out / 2),
        });
    }
    f
}

fn fade_name(f: Fade) -> &'static str {
    match f {
        Fade::In => "in",
        Fade::Out => "out",
    }
}

/// ffmpeg inputs and a `-filter_complex` graph for a stretch of the
/// timeline, ending in `[vout]` (when `o.video`) and `[aout]` (when
/// `o.audio`). The sound is exactly `to - from` long, sample for sample, so
/// stretches rendered one after another join seamlessly.
pub fn render_graph(t: &Timeline, o: &RenderOpts) -> (Vec<OsString>, String) {
    let canvas = o.canvas.normalized();
    let (w, h, fps) = (canvas.width, canvas.height, canvas.fps);
    let span = o.to.saturating_sub(o.from);
    let pieces = pieces(t, o);
    let mut inputs: Vec<OsString> = Vec::new();
    let mut graph: Vec<String> = Vec::new();
    let mut overlays = Vec::new();
    let mut mixes = Vec::new();

    for (n, p) in pieces.iter().enumerate() {
        if let Some(threads) = o.input_threads {
            inputs.extend(["-threads".into(), threads.to_string().into()]);
        }
        match p.clip.kind {
            // A picture is decoded once and repeated in the graph (looping
            // the input would decode it again for every frame).
            ClipKind::Image => {}
            ClipKind::Video | ClipKind::Audio => {
                let from = p.clip.in_point + p.tau0;
                if !from.is_zero() {
                    inputs.extend(["-ss".into(), secs(from - preroll(p)).into()]);
                }
                inputs.extend(["-t".into(), secs(p.len + preroll(p)).into()]);
            }
        }
        inputs.extend(["-i".into(), p.clip.path.clone().into()]);

        if p.picture {
            let upper = p.layer > 0;
            let fades = picture_fades(p, upper);
            let alpha = upper || fades.iter().any(|(_, a)| *a);
            let (pad, fmt) = if upper { ("black@0", "yuva420p") } else if alpha { ("black", "yuva420p") } else { ("black", "yuv420p") };
            let mut v = format!("[{n}:v:0]");
            if !preroll(p).is_zero() {
                v += &format!("trim=start={},", secs(preroll(p)));
            }
            v += &format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color={pad},setsar=1,format={fmt},"
            );
            if p.clip.kind == ClipKind::Image {
                v += &format!("loop=loop=-1:size=1,setpts=N/({fps})/TB,trim=duration={},", secs(p.len));
            }
            v += &format!("setpts=PTS-STARTPTS+{}/TB", secs(p.tau0));
            for ((dir, st, d), to_alpha) in &fades {
                v += &format!(",fade=t={}:st={}:d={}", fade_name(*dir), secs(*st), secs(*d));
                if *to_alpha {
                    v += ":alpha=1";
                }
            }
            v += &format!(",setpts=PTS-STARTPTS+{}/TB", secs(p.at));
            graph.push(format!("{v}[pv{n}]"));
            // A dip to black shows this clip only from the middle of the dip.
            let hidden_until = match p.kind_in {
                TransitionKind::DipToBlack if !p.ov_in.is_zero() => (p.ov_in / 2).saturating_sub(p.tau0),
                _ => Duration::ZERO,
            };
            overlays.push((n, p.at + hidden_until, p.at + p.len));
        }

        if p.sound {
            let mut a = format!("[{n}:a:0]");
            if !preroll(p).is_zero() {
                a += &format!("atrim=start={},", secs(preroll(p)));
            }
            a += &format!(
                "asetpts=PTS-STARTPTS,aresample={SAMPLE_RATE},\
                 aformat=sample_fmts=fltp:sample_rates={SAMPLE_RATE}:channel_layouts=stereo,\
                 asetpts=PTS-STARTPTS+{}/TB,volume={:.3}",
                secs(p.tau0),
                p.clip.volume.clamp(0.0, crate::audio::MAX_VOLUME)
            );
            for (dir, st, d) in sound_fades(p) {
                a += &format!(",afade=t={}:st={}:d={}", fade_name(dir), secs(st), secs(d));
            }
            a += ",asetpts=PTS-STARTPTS";
            let delay = samples(o.from + p.at).saturating_sub(samples(o.from));
            if delay > 0 {
                a += &format!(",adelay=delays={delay}S:all=1");
            }
            graph.push(format!("{a}[pa{n}]"));
            mixes.push(n);
        }
    }

    if o.video {
        graph.push(format!("color=c=black:s={w}x{h}:r={fps}:d={},format=yuv420p[base]", secs(span.max(Duration::from_millis(1)))));
        let mut last = "base".to_string();
        for (k, (n, from, to)) in overlays.iter().enumerate() {
            graph.push(format!(
                "[{last}][pv{n}]overlay=eof_action=pass:enable='between(t,{},{})'[ov{k}]",
                secs(*from),
                secs(to.saturating_sub(Duration::from_micros(1)))
            ));
            last = format!("ov{k}");
        }
        graph.push(format!("[{last}]null[vout]"));
    }

    if o.audio {
        let total = samples(o.to).saturating_sub(samples(o.from)).max(1);
        let tail = format!("apad,atrim=end_sample={total},asetpts=PTS-STARTPTS[aout]");
        match mixes.len() {
            0 => graph.push(format!("anullsrc=r={SAMPLE_RATE}:cl=stereo,{tail}")),
            1 => graph.push(format!("[pa{}]{tail}", mixes[0])),
            k => {
                let ins: String = mixes.iter().map(|n| format!("[pa{n}]")).collect();
                graph.push(format!("{ins}amix=inputs={k}:normalize=0:dropout_transition=0:duration=longest,{tail}"));
            }
        }
    }
    (inputs, graph.join(";"))
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
    pub output: std::path::PathBuf,
    pub format: ExportFormat,
    /// Encoder quality: lower is better and bigger.
    pub crf: u8,
    /// x264 speed preset (MP4 only).
    pub preset: String,
    /// Write encoder/date tags (off: see [`crate::privacy`]).
    pub keep_tags: bool,
}

impl ExportRequest {
    pub fn new(timeline: Timeline, output: std::path::PathBuf) -> Self {
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
        if self.timeline.duration() < crate::timeline::MIN_CLIP {
            return Err(Error::NothingToKeep);
        }
        for f in self.timeline.files() {
            if !f.is_file() {
                return Err(Error::MissingFile(f.to_path_buf()));
            }
            if crate::subtitles::same_file(f, &self.output) {
                return Err(Error::OutputIsInput);
            }
        }
        if self.timeline.subtitle_export != SubtitleExport::Off {
            if let Some(s) = &self.timeline.subtitle {
                if crate::subtitles::SubtitleFormat::from_path(s).is_none() {
                    return Err(Error::UnsupportedSubtitle(s.clone()));
                }
            }
        }
        Ok(())
    }
}

fn strs(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Full ffmpeg argument list for an export.
pub fn export_args(req: &ExportRequest) -> Vec<OsString> {
    let t = &req.timeline;
    let opts = RenderOpts {
        canvas: req.canvas,
        from: Duration::ZERO,
        to: t.duration(),
        video: true,
        audio: true,
        input_threads: None,
    };
    let (inputs, mut graph) = render_graph(t, &opts);
    let subtitle = t.subtitle.as_ref().filter(|_| t.subtitle_export != SubtitleExport::Off);
    if let (Some(sub), SubtitleExport::Burn) = (subtitle, t.subtitle_export) {
        graph = graph.replace(
            "null[vout]",
            &format!("subtitles=filename={}[vout]", crate::subtitles::escape_filter_path(sub)),
        );
    }
    let n_inputs = inputs.iter().filter(|a| *a == "-i").count();
    let mut a = strs(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    a.extend(inputs);
    let soft = subtitle.filter(|_| t.subtitle_export == SubtitleExport::Track);
    if let Some(sub) = soft {
        a.push("-i".into());
        a.push(sub.into());
    }
    a.push("-filter_complex".into());
    a.push(graph.into());
    a.extend(strs(&["-map", "[vout]", "-map", "[aout]"]));
    if let Some(sub) = soft {
        a.push("-map".into());
        a.push(format!("{n_inputs}:s:0").into());
        let codec = match ext(&req.output).as_str() {
            "webm" => "webvtt",
            "mkv" if matches!(ext(sub).as_str(), "ass" | "ssa") => "ass",
            "mkv" => "srt",
            _ => "mov_text",
        };
        a.extend(strs(&["-c:s", codec]));
        if !t.subtitle_language.is_empty() {
            a.push("-metadata:s:s:0".into());
            a.push(format!("language={}", t.subtitle_language).into());
        }
    }
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
    if req.keep_tags {
        a.extend(strs(&["-metadata", "creation_time=now"]));
    }
    if crate::privacy::is_mp4_family(&req.output) {
        a.extend(strs(&["-movflags", "+faststart"]));
    }
    a.push(req.output.clone().into());
    a
}

/// What the live preview decodes.
#[derive(Clone, Debug)]
pub struct PreviewOpts {
    pub canvas: Canvas,
    pub from: Duration,
    pub to: Duration,
    /// Draw the timeline's subtitle file onto the frames.
    pub subtitles: bool,
    /// Just the first frame (scrubbing).
    pub single_frame: bool,
}

/// ffmpeg arguments writing the picture of a stretch of the timeline as PPM
/// images to stdout.
pub fn preview_video_args(t: &Timeline, p: &PreviewOpts) -> Vec<OsString> {
    let opts = RenderOpts { canvas: p.canvas, from: p.from, to: p.to, video: true, audio: false, input_threads: Some(1) };
    let (inputs, mut graph) = render_graph(t, &opts);
    if let Some(sub) = t.subtitle.as_ref().filter(|_| p.subtitles) {
        // libass times cues by the frames' timestamps: put them back on the
        // timeline's clock first.
        graph = graph.replace(
            "null[vout]",
            &format!(
                "setpts=PTS+{}/TB,subtitles=filename={}[vout]",
                secs(p.from),
                crate::subtitles::escape_filter_path(sub)
            ),
        );
    }
    let mut a = strs(&["-hide_banner", "-v", "error", "-nostdin"]);
    a.extend(inputs);
    a.push("-filter_complex".into());
    a.push(graph.into());
    a.extend(strs(&["-map", "[vout]"]));
    if p.single_frame {
        a.extend(strs(&["-frames:v", "1"]));
    }
    a.extend(strs(&["-f", "image2pipe", "-c:v", "ppm", "-pix_fmt", "rgb24", "pipe:1"]));
    a
}

/// ffmpeg arguments writing the sound of a stretch of the timeline to
/// stdout as 48 kHz stereo signed 16-bit PCM.
pub fn preview_audio_args(t: &Timeline, p: &PreviewOpts) -> Vec<OsString> {
    let opts = RenderOpts { canvas: p.canvas, from: p.from, to: p.to, video: false, audio: true, input_threads: Some(1) };
    let (inputs, graph) = render_graph(t, &opts);
    let mut a = strs(&["-hide_banner", "-v", "error", "-nostdin"]);
    a.extend(inputs);
    a.push("-filter_complex".into());
    a.push(graph.into());
    a.extend(strs(&["-map", "[aout]", "-f", "s16le", "-acodec", "pcm_s16le", "-ac", "2"]));
    a.push("-ar".into());
    a.push(SAMPLE_RATE.to_string().into());
    a.push("pipe:1".into());
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{ClipRef, TrackRef};

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn join(a: &[OsString]) -> String {
        a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    /// V1: a.mp4 (0-10, sound), b.png (10-14), c.mp4 (14-20, silent).
    /// A1: music 2-32.
    fn project() -> Timeline {
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("a.mp4".into(), s(10), true)).unwrap();
        t.append(TrackRef::V1, Clip::image("b.png".into())).unwrap();
        t.append(TrackRef::V1, Clip::video("c.mp4".into(), s(6), false)).unwrap();
        t.add_clip(TrackRef::A1, Clip::audio("m.mp3".into(), s(30)), s(2)).unwrap();
        t
    }

    fn whole(t: &Timeline) -> RenderOpts {
        RenderOpts { canvas: Canvas::default(), from: Duration::ZERO, to: t.duration(), video: true, audio: true, input_threads: None }
    }

    #[test]
    fn every_clip_is_laid_over_black_at_its_place() {
        let t = project();
        let (inputs, g) = render_graph(&t, &whole(&t));
        let i = join(&inputs);
        assert!(i.contains("-t 10.000000 -i a.mp4"), "{i}");
        assert!(i.contains("-t 10.000000 -i a.mp4 -i b.png"), "{i}");
        assert!(g.contains("format=yuv420p,loop=loop=-1:size=1,setpts=N/(30)/TB,trim=duration=4.000000,setpts"), "{g}");
        assert!(i.contains("-t 30.000000 -i m.mp3"), "{i}");
        assert!(g.contains("color=c=black:s=1280x720:r=30:d=32.000000"), "{g}");
        assert!(g.contains("setpts=PTS-STARTPTS+10.000000/TB[pv1]"), "{g}");
        assert!(g.contains("[ov0][pv1]overlay=eof_action=pass:enable='between(t,10.000000,13.999999)'[ov1]"), "{g}");
        assert!(g.ends_with("amix=inputs=2:normalize=0:dropout_transition=0:duration=longest,apad,atrim=end_sample=1536000,asetpts=PTS-STARTPTS[aout]"), "{g}");
        // The music starts at 2s: 96000 samples in.
        assert!(g.contains("adelay=delays=96000S:all=1[pa3]"), "{g}");
        // c has no sound, the picture none either: two sounds mixed.
        assert!(!g.contains("[pa2]") && !g.contains("[pa1]"), "{g}");
    }

    #[test]
    fn mixing_follows_volume_mute_and_track_mute() {
        let mut t = project();
        t.video[0].clips[0].volume = 0.5;
        let (_, g) = render_graph(&t, &whole(&t));
        assert!(g.contains("volume=0.500"), "{g}");
        t.audio[0].muted = true;
        let (inputs, g) = render_graph(&t, &whole(&t));
        assert!(!join(&inputs).contains("m.mp3"), "a muted track isn't even opened");
        assert!(g.contains("[pa0]apad,atrim=end_sample=1536000"), "{g}");
        t.video[0].clips[0].muted = true;
        let (_, g) = render_graph(&t, &whole(&t));
        assert!(g.contains("anullsrc=r=48000:cl=stereo,apad"), "{g}");
        // Hidden V1: no pictures, just black.
        t.video[0].hidden = true;
        let (inputs, g) = render_graph(&t, &whole(&t));
        assert!(inputs.is_empty(), "{}", join(&inputs));
        assert!(g.contains("[base]null[vout]"), "{g}");
    }

    #[test]
    fn transitions_and_fades() {
        let mut t = project();
        let v1 = |i| ClipRef { track: TrackRef::V1, index: i };
        // a→b cross-fade 1s (b now 9-13), b→c dip 2s (c now 11-17).
        t.set_transition(v1(1), Some((TransitionKind::Crossfade, s(1))));
        t.set_transition(v1(2), Some((TransitionKind::DipToBlack, s(2))));
        t.clip_mut(v1(0)).unwrap().fade_in = s(1);
        let (_, g) = render_graph(&t, &whole(&t));
        assert!(g.contains("[0:v:0]scale=1280:720") && g.contains("format=yuv420p,setpts=PTS-STARTPTS+0.000000/TB,fade=t=in:st=0.000000:d=1.000000,"), "{g}");
        // b fades in over a (transparency), and out to black for the dip.
        assert!(g.contains("fade=t=in:st=0.000000:d=1.000000:alpha=1,fade=t=out:st=2.000000:d=1.000000,setpts=PTS-STARTPTS+9.000000/TB[pv1]"), "{g}");
        // c is hidden for the first half of the dip, then comes in from black.
        assert!(g.contains("fade=t=in:st=1.000000:d=1.000000,setpts=PTS-STARTPTS+11.000000/TB[pv2]"), "{g}");
        assert!(g.contains("[ov1][pv2]overlay=eof_action=pass:enable='between(t,12.000000,"), "{g}");
        // a's sound fades out under b's (which fades in).
        assert!(g.contains("afade=t=in:st=0.000000:d=1.000000,afade=t=out:st=9.000000:d=1.000000,asetpts"), "{g}");
    }

    #[test]
    fn a_stretch_starts_inside_clips_and_keeps_their_fades() {
        let mut t = project();
        t.video[0].clips[0].fade_out = s(4);
        // 8s..12s: the last 2s of a, the first 2s of the picture, music.
        let o = RenderOpts { from: s(8), to: s(12), ..whole(&t) };
        let (inputs, g) = render_graph(&t, &o);
        let i = join(&inputs);
        assert!(i.contains("-ss 7.500000 -t 2.500000 -i a.mp4"), "{i}");
        assert!(g.contains("[0:v:0]trim=start=0.500000,scale="), "{g}");
        assert!(g.contains("trim=duration=2.000000,setpts=PTS-STARTPTS+0.000000/TB"), "{g}");
        assert!(i.contains("-ss 5.500000 -t 4.500000 -i m.mp3"), "{i}");
        assert!(g.contains("[2:a:0]atrim=start=0.500000,asetpts=PTS-STARTPTS"), "{g}");
        assert!(!i.contains("c.mp4"), "{i}");
        // a's fade-out still runs from 6s in a's time.
        assert!(g.contains("setpts=PTS-STARTPTS+8.000000/TB,fade=t=out:st=6.000000:d=4.000000,setpts=PTS-STARTPTS+0.000000/TB[pv0]"), "{g}");
        assert!(g.contains("color=c=black:s=1280x720:r=30:d=4.000000"), "{g}");
        assert!(g.contains("atrim=end_sample=192000"), "{g}");
    }

    #[test]
    fn upper_tracks_are_transparent_around_the_picture() {
        let mut t = project();
        let v2 = t.add_track(TrackKind::Video);
        t.add_clip(v2, Clip::image("logo.png".into()), s(1)).unwrap();
        let (_, g) = render_graph(&t, &whole(&t));
        assert!(g.contains("color=black@0,setsar=1,format=yuva420p"), "{g}");
        // V2 is laid on last.
        let last_overlay = g.rfind("overlay").unwrap();
        assert!(g[..last_overlay].rfind("[pv").is_some());
        assert!(g.contains("[ov2][pv4]overlay"), "{g}");
    }

    #[test]
    fn export_args_strip_tags_and_add_subtitles() {
        let mut t = project();
        let mut req = ExportRequest::new(t.clone(), "out.webm".into());
        assert_eq!(req.format, ExportFormat::WebM);
        let a = join(&export_args(&req));
        assert!(a.contains("libvpx-vp9") && a.contains("libopus"), "{a}");
        assert!(a.contains("-map_metadata -1") && a.contains("+bitexact"), "{a}");
        req.format = ExportFormat::Mp4;
        req.keep_tags = true;
        let a = join(&export_args(&req));
        assert!(a.contains("libx264") && !a.contains("bitexact") && a.contains("creation_time=now"), "{a}");

        t.subtitle = Some("s.srt".into());
        t.subtitle_export = SubtitleExport::Track;
        let req = ExportRequest::new(t.clone(), "out.mp4".into());
        let a = join(&export_args(&req));
        assert!(a.contains("-i s.srt -filter_complex"), "{a}");
        assert!(a.contains("-map 4:s:0 -c:s mov_text -metadata:s:s:0 language=eng"), "{a}");
        assert!(a.contains("filter_units=remove_types=6"), "{a}");
        t.subtitle_export = SubtitleExport::Burn;
        let req = ExportRequest::new(t, "out.mp4".into());
        let a = join(&export_args(&req));
        assert!(a.contains("subtitles=filename=s.srt[vout]") && !a.contains("-i s.srt"), "{a}");
    }

    #[test]
    fn preview_args() {
        let mut t = project();
        t.subtitle = Some("s.srt".into());
        let p = PreviewOpts { canvas: Canvas { width: 640, height: 360, fps: 25.0 }, from: s(2), to: s(4), subtitles: true, single_frame: true };
        let a = join(&preview_video_args(&t, &p));
        assert!(a.contains("-threads 1 -ss 1.500000 -t 2.500000 -i a.mp4"), "{a}");
        assert!(a.contains("setpts=PTS+2.000000/TB,subtitles=filename=s.srt[vout]"), "{a}");
        assert!(a.contains("-frames:v 1 -f image2pipe"), "{a}");
        assert!(!a.contains("m.mp3"), "no sound inputs for the picture: {a}");
        let a = join(&preview_audio_args(&t, &p));
        assert!(a.contains("-map [aout] -f s16le"), "{a}");
        assert!(!a.contains("b.png") && !a.contains("[vout]"), "{a}");
    }
}
