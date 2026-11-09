use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleFormat {
    Srt,
    Ass,
    WebVtt,
}

impl SubtitleFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "srt" => Some(Self::Srt),
            "ass" | "ssa" => Some(Self::Ass),
            "vtt" => Some(Self::WebVtt),
            _ => None,
        }
    }

    pub const EXTENSIONS: &'static [&'static str] = &["srt", "ass", "ssa", "vtt"];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmbedMode {
    /// Add the subtitles as a separate track the viewer can switch on/off.
    /// Fast: video and audio are copied, not re-encoded.
    #[default]
    Soft,
    /// Draw the subtitles into the picture. Works everywhere, but re-encodes
    /// the video and can't be turned off.
    Hard,
}

#[derive(Clone, Debug)]
pub struct EmbedRequest {
    pub video: PathBuf,
    pub subtitle: PathBuf,
    pub output: PathBuf,
    pub mode: EmbedMode,
    /// ISO 639-2 code such as "eng"; soft mode only.
    pub language: Option<String>,
    /// Track title shown by players; soft mode only.
    pub title: Option<String>,
    /// Mark the new track as default so players show it automatically.
    pub make_default: bool,
    /// Keep subtitle tracks already present in the video.
    pub keep_existing: bool,
    /// x264/VP9 quality for hard mode (lower = better, 18-28 is sensible).
    pub crf: u8,
    /// x264 speed preset for hard mode.
    pub preset: String,
}

impl EmbedRequest {
    pub fn new(video: PathBuf, subtitle: PathBuf, output: PathBuf) -> Self {
        Self {
            video,
            subtitle,
            output,
            mode: EmbedMode::Soft,
            language: None,
            title: None,
            make_default: true,
            keep_existing: true,
            crf: 20,
            preset: "medium".into(),
        }
    }

    pub fn validate(&self) -> Result<SubtitleFormat> {
        for p in [&self.video, &self.subtitle] {
            if !p.is_file() {
                return Err(Error::MissingFile(p.clone()));
            }
        }
        let fmt = SubtitleFormat::from_path(&self.subtitle)
            .ok_or_else(|| Error::UnsupportedSubtitle(self.subtitle.clone()))?;
        if same_file(&self.video, &self.output) {
            return Err(Error::OutputIsInput);
        }
        Ok(fmt)
    }
}

/// Suggest `<name>.subbed.<ext>` (soft) or `<name>.burned.<ext>` (hard) next to the video.
pub fn default_output(video: &Path, mode: EmbedMode) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let ext = video
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| OutputContainer::from_ext(e).is_some())
        .unwrap_or_else(|| "mkv".into());
    let tag = match mode {
        EmbedMode::Soft => "subbed",
        EmbedMode::Hard => "burned",
    };
    video.with_file_name(format!("{stem}.{tag}.{ext}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputContainer {
    Mp4,
    Mkv,
    WebM,
}

impl OutputContainer {
    fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "mp4" | "m4v" | "mov" => Some(Self::Mp4),
            "mkv" => Some(Self::Mkv),
            "webm" => Some(Self::WebM),
            _ => None,
        }
    }

    fn from_path(path: &Path) -> Self {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| Self::from_ext(&e.to_ascii_lowercase()))
            .unwrap_or(Self::Mkv)
    }

    /// Text subtitle codec this container can actually hold.
    fn subtitle_codec(self) -> &'static str {
        match self {
            Self::Mp4 => "mov_text",
            Self::WebM => "webvtt",
            Self::Mkv => "copy",
        }
    }
}

/// Build the ffmpeg argument list (without the program name).
///
/// `existing_subs` is the number of subtitle tracks already in the video; it
/// tells us the index of the track we add so we can tag it.
pub fn ffmpeg_args(req: &EmbedRequest, existing_subs: usize) -> Vec<OsString> {
    let container = OutputContainer::from_path(&req.output);
    let mut a = Args::default();
    a.push_all(["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    a.push("-i");
    a.push(&req.video);

    match req.mode {
        EmbedMode::Soft => {
            a.push("-i");
            a.push(&req.subtitle);
            a.push_all(["-map", "0:v", "-map", "0:a?"]);
            if req.keep_existing {
                a.push_all(["-map", "0:s?"]);
            }
            a.push_all(["-map", "1:0"]);
            if container == OutputContainer::Mkv {
                // Fonts embedded for existing ASS tracks.
                a.push_all(["-map", "0:t?"]);
            }
            a.push_all(["-c", "copy", "-c:s", container.subtitle_codec()]);

            let n = if req.keep_existing { existing_subs } else { 0 };
            if let Some(lang) = req.language.as_deref().filter(|l| !l.is_empty()) {
                a.push(format!("-metadata:s:s:{n}"));
                a.push(format!("language={lang}"));
            }
            if let Some(title) = req.title.as_deref().filter(|t| !t.is_empty()) {
                a.push(format!("-metadata:s:s:{n}"));
                a.push(format!("title={title}"));
            }
            if req.make_default {
                // Clear "default" on the old tracks, then set it on ours.
                a.push_all(["-disposition:s", "0"]);
                a.push(format!("-disposition:s:{n}"));
                a.push("default");
            }
        }
        EmbedMode::Hard => {
            a.push("-vf");
            a.push(format!("subtitles=filename={}", escape_filter_path(&req.subtitle)));
            a.push_all(["-map", "0:v:0", "-map", "0:a?"]);
            if req.keep_existing {
                a.push_all(["-map", "0:s?", "-c:s", container.subtitle_codec()]);
            }
            let crf = req.crf.to_string();
            match container {
                OutputContainer::WebM => {
                    a.push_all(["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", &crf, "-row-mt", "1"]);
                    a.push_all(["-c:a", "libopus"]);
                }
                _ => {
                    a.push_all(["-c:v", "libx264", "-crf", &crf, "-preset", &req.preset]);
                    a.push_all(["-pix_fmt", "yuv420p", "-c:a", "copy"]);
                }
            }
        }
    }

    if container == OutputContainer::Mp4 {
        a.push_all(["-movflags", "+faststart"]);
    }
    a.push(&req.output);
    a.0
}

/// Escape a path for use as `subtitles=filename=<here>` inside `-vf`.
///
/// FFmpeg parses filter graphs twice: once for the option value and once for
/// the graph, so special characters need two levels of escaping.
pub(crate) fn escape_filter_path(path: &Path) -> String {
    // Forward slashes work on Windows too and avoid one class of escaping.
    let raw = path.to_string_lossy().replace('\\', "/");
    let level1 = escape_chars(&raw, &['\\', '\'', ':']);
    escape_chars(&level1, &['\\', '\'', '[', ']', ',', ';'])
}

fn escape_chars(s: &str, special: &[char]) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if special.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(Default)]
struct Args(Vec<OsString>);

impl Args {
    fn push(&mut self, s: impl Into<OsString>) {
        self.0.push(s.into());
    }
    fn push_all<const N: usize>(&mut self, items: [&str; N]) {
        self.0.extend(items.iter().map(OsString::from));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_str(req: &EmbedRequest, n: usize) -> String {
        ffmpeg_args(req, n)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn soft_mp4_uses_mov_text_and_tags_new_track() {
        let mut req = EmbedRequest::new("in.mp4".into(), "s.srt".into(), "out.mp4".into());
        req.language = Some("eng".into());
        let s = args_str(&req, 2);
        assert!(s.contains("-c:s mov_text"), "{s}");
        assert!(s.contains("-metadata:s:s:2 language=eng"), "{s}");
        assert!(s.contains("-disposition:s:2 default"), "{s}");
        assert!(!s.contains("0:t?"), "mp4 can't hold attachments: {s}");
    }

    #[test]
    fn soft_mkv_copies_subtitle() {
        let req = EmbedRequest::new("in.mkv".into(), "s.ass".into(), "out.mkv".into());
        assert!(args_str(&req, 0).contains("-c:s copy"));
    }

    #[test]
    fn hard_mode_reencodes() {
        let mut req = EmbedRequest::new("in.mp4".into(), "s.srt".into(), "out.mp4".into());
        req.mode = EmbedMode::Hard;
        req.keep_existing = false;
        let s = args_str(&req, 0);
        assert!(s.contains("-vf subtitles=filename=s.srt"), "{s}");
        assert!(s.contains("-c:v libx264"), "{s}");
        assert!(!s.contains("-i s.srt"), "{s}");
    }

    #[test]
    fn filter_path_escaping() {
        assert_eq!(escape_filter_path(Path::new("a'b.srt")), r"a\\\'b.srt");
        assert_eq!(escape_filter_path(Path::new("C:\\x,y.srt")), r"C\\:/x\,y.srt");
    }

    #[test]
    fn default_output_names() {
        assert_eq!(
            default_output(Path::new("/v/movie.mp4"), EmbedMode::Soft),
            PathBuf::from("/v/movie.subbed.mp4")
        );
        assert_eq!(
            default_output(Path::new("/v/clip.avi"), EmbedMode::Hard),
            PathBuf::from("/v/clip.burned.mkv")
        );
    }
}
