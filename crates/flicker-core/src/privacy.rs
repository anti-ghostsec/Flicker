//! Saving, and what Flicker writes into a saved file besides the picture and
//! sound.
//!
//! Timeline exports are one ffmpeg pass straight into the chosen file, with
//! [`output_args`] making it private as it's written; nothing else is
//! written. Single-file saves ([`crate::Engine::save`]) end with a quick
//! stream-copy pass ([`scrub_args`]) instead. Either way the file has:
//!
//! * no tags copied from the source (title, comment, phone make/model, GPS
//!   location, dates…), no chapters and no data tracks (some cameras keep
//!   GPS in one). Track languages are kept.
//! * no encoder name or version and no creation time (`-fflags +bitexact`),
//!   unless the user turns "Write encoder name and dates into saved files" on.
//! * x264's version string, which sits in the H.264 stream itself (an SEI
//!   message), is removed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::media::{MediaInfo, StreamKind};
use crate::{Error, Result};

/// Write the current video to a file the user keeps.
#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Write the encoder name and the date (off by default).
    pub keep_tags: bool,
}

impl SaveRequest {
    pub fn new(input: PathBuf, output: PathBuf) -> Self {
        Self { input, output, keep_tags: false }
    }

    pub fn validate(&self) -> Result<()> {
        if !self.input.is_file() {
            return Err(Error::MissingFile(self.input.clone()));
        }
        if crate::subtitles::same_file(&self.input, &self.output) {
            return Err(Error::OutputIsInput);
        }
        Ok(())
    }

    /// True when the output needs re-encoding (WebM: VP9 + Opus).
    pub fn is_webm(&self) -> bool {
        ext(&self.output) == "webm"
    }

    /// True when the output's container differs from the input's.
    pub fn changes_container(&self) -> bool {
        ext(&self.input) != ext(&self.output)
    }
}

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// MP4-style containers, which want `-movflags +faststart`.
pub fn is_mp4_family(p: &Path) -> bool {
    matches!(ext(p).as_str(), "mp4" | "m4v" | "mov")
}

/// Output options for every encode, working files included: encoders leave
/// their name and version out of the streams (ffmpeg's AAC encoder would
/// otherwise write "Lavc…" into the sound itself).
pub fn encoder_flags() -> Vec<OsString> {
    ["-flags:v", "+bitexact", "-flags:a", "+bitexact"].map(OsString::from).to_vec()
}

/// Output options for a render that encodes (Append, WebM saves).
///
/// * `keep_tags`: write the usual encoder/date tags (ffmpeg's default).
/// * `drop_source_tags`: leave out tags copied from the inputs.
/// * `h264`: the video is encoded with x264, which writes its version and
///   settings into the stream itself (an SEI message); remove them.
pub fn output_args(keep_tags: bool, drop_source_tags: bool, h264: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    if drop_source_tags {
        a.extend(["-map_metadata", "-1", "-map_chapters", "-1"].map(OsString::from));
    }
    if keep_tags {
        return a;
    }
    // bitexact: the muxers leave out "encoder=Lavf…" and creation dates,
    // every encoder (subtitles too) leaves its version out of what it writes.
    a.extend(["-fflags", "+bitexact", "-flags", "+bitexact"].map(OsString::from));
    // Even so, ffmpeg tags each encoded track "Lavc libx264" unless the
    // track already has an encoder tag; an empty one would be dropped, so
    // it's a single space.
    a.extend(["-metadata:s", "encoder= "].map(OsString::from));
    if h264 {
        // NAL type 6 = SEI, where x264 puts "x264 - core NNN - options…".
        a.extend(["-bsf:v", "filter_units=remove_types=6"].map(OsString::from));
    }
    a
}

/// The final, copy-only pass of every save: `input` (the same container as
/// `output`) is copied stream by stream into `output` without source tags,
/// and, unless `keep_tags`, without encoder names or dates. `info` describes
/// `input`.
pub fn scrub_args(input: &Path, output: &Path, info: &MediaInfo, keep_tags: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-y", "-hide_banner", "-nostdin", "-v", "error"].map(OsString::from).to_vec();
    a.extend(crate::input::args(input));
    // Picture, sound and subtitles only: data tracks can hold GPS logs.
    a.extend(["-map", "0:v?", "-map", "0:a?", "-map", "0:s?", "-c", "copy"].map(OsString::from));
    a.extend(["-map_metadata", "-1", "-map_chapters", "-1"].map(OsString::from));

    // Output streams come in map order: video, audio, subtitles.
    let mut n = 0;
    for kind in [StreamKind::Video, StreamKind::Audio, StreamKind::Subtitle] {
        for (k, s) in info.streams.iter().filter(|s| s.kind == kind).enumerate() {
            if let Some(lang) = s.language.as_deref().filter(|l| !l.is_empty() && *l != "und") {
                a.push(format!("-metadata:s:{n}").into());
                a.push(format!("language={lang}").into());
            }
            if kind == StreamKind::Video && s.codec == "h264" && !keep_tags {
                a.push(format!("-bsf:v:{k}").into());
                a.push("filter_units=remove_types=6".into());
            }
            n += 1;
        }
    }

    if keep_tags {
        a.extend(["-metadata", "creation_time=now"].map(OsString::from));
    } else {
        a.extend(["-fflags", "+bitexact"].map(OsString::from));
    }
    if is_mp4_family(output) {
        a.extend(["-movflags", "+faststart"].map(OsString::from));
    }
    a.push(output.into());
    a
}

/// Re-encode `input` as WebM (VP9 + Opus) into `output`. Text subtitles
/// become WebVTT; image subtitles are left out (WebM can't hold them).
pub fn webm_args(input: &Path, output: &Path, info: &MediaInfo, keep_tags: bool) -> Vec<OsString> {
    let mut a = crate::edit::base_args();
    a.extend(crate::input::args(input));
    a.extend(["-map", "0:v:0", "-map", "0:a?"].map(OsString::from));
    let subs: Vec<&str> =
        info.streams.iter().filter(|s| s.kind == StreamKind::Subtitle).map(|s| s.codec.as_str()).collect();
    if !subs.is_empty() && subs.iter().all(|c| crate::edit::is_text_subtitle(c)) {
        a.extend(["-map", "0:s", "-c:s", "webvtt"].map(OsString::from));
    }
    a.extend(["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-row-mt", "1"].map(OsString::from));
    a.extend(["-deadline", "good", "-cpu-used", "4", "-pix_fmt", "yuv420p"].map(OsString::from));
    a.extend(["-c:a", "libopus", "-b:a", "128k"].map(OsString::from));
    a.extend(output_args(keep_tags, true, false));
    a.push(output.into());
    a
}

/// Script Info fields that change how subtitles look; every other one (Title,
/// Original Script, Original Translation, Script Updated By, Update
/// Details, Audio/Video File…) is about who made the file, and where.
const ASS_LOOK_FIELDS: &[&str] = &[
    "scripttype",
    "playresx",
    "playresy",
    "layoutresx",
    "layoutresy",
    "wrapstyle",
    "scaledborderandshadow",
    "ycbcr matrix",
    "collisions",
    "playdepth",
    "timer",
    "kerning",
];

/// An ASS/SSA subtitle file with only what's needed to show it: the
/// Script Info fields that change the look, the styles, the dialogue and
/// any embedded fonts and pictures. Everything else is left out: names and
/// e-mail addresses (Title, Original Script…), comments, `Comment:` lines,
/// file paths (Audio File, Video File, editor sections such as Aegisub's
/// "Project Garbage", `Sound:`/`Movie:` events).
///
/// A subtitle track in Matroska keeps the whole header of its file (before
/// the dialogue) in the track itself, so this is what a private export reads.
/// Text is passed through byte for byte, whatever its encoding; UTF-16
/// files become UTF-8.
pub fn clean_ass(data: &[u8]) -> Vec<u8> {
    let utf8;
    let data = match data {
        [0xff, 0xfe, rest @ ..] => {
            utf8 = utf16(rest, u16::from_le_bytes);
            utf8.as_slice()
        }
        [0xfe, 0xff, rest @ ..] => {
            utf8 = utf16(rest, u16::from_be_bytes);
            utf8.as_slice()
        }
        [0xef, 0xbb, 0xbf, rest @ ..] => rest,
        _ => data,
    };

    #[derive(PartialEq)]
    enum Section {
        Info,
        Styles,
        Events,
        Embedded,
        Other,
    }
    let mut out = Vec::with_capacity(data.len());
    let mut section = Section::Other;
    for raw in data.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw).trim_ascii();
        if line.starts_with(b"[") {
            let name = String::from_utf8_lossy(line).to_ascii_lowercase();
            section = match name.as_str() {
                "[script info]" => Section::Info,
                "[v4+ styles]" | "[v4 styles]" | "[v4 styles+]" => Section::Styles,
                "[events]" => Section::Events,
                "[fonts]" | "[graphics]" => Section::Embedded,
                _ => Section::Other,
            };
            if section != Section::Other {
                if !out.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(line);
                out.push(b'\n');
            }
            continue;
        }
        let key = |l: &[u8]| -> Option<String> {
            let colon = l.iter().position(|&b| b == b':')?;
            Some(String::from_utf8_lossy(&l[..colon]).trim().to_ascii_lowercase())
        };
        let keep = !line.is_empty()
            && match section {
                Section::Info => key(line).is_some_and(|k| ASS_LOOK_FIELDS.contains(&k.as_str())),
                Section::Styles => key(line).is_some_and(|k| k == "format" || k == "style"),
                Section::Events => key(line).is_some_and(|k| k == "format" || k == "dialogue"),
                Section::Embedded => true,
                Section::Other => false,
            };
        if keep {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    out
}

fn utf16(data: &[u8], unit: fn([u8; 2]) -> u16) -> Vec<u8> {
    let units = data.as_chunks::<2>().0.iter().map(|&c| unit(c));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .into_bytes()
}

/// True for an ASS/SSA subtitle file (by its name).
pub fn is_ass(path: &Path) -> bool {
    matches!(ext(path).as_str(), "ass" | "ssa")
}

/// Where a save is written before its last pass: a hidden file next to the
/// output (not in /tmp, which is often in RAM).
pub fn staging_path(output: &Path) -> PathBuf {
    let stem = output.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let ext = output.extension().map(|e| e.to_string_lossy()).unwrap_or("mkv".into());
    output.with_file_name(format!(".{stem}.flicker-part.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::StreamInfo;

    fn s(a: &[OsString]) -> String {
        a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    fn stream(index: usize, kind: StreamKind, codec: &str, language: Option<&str>) -> StreamInfo {
        StreamInfo {
            index,
            kind,
            codec: codec.into(),
            language: language.map(Into::into),
            width: None,
            height: None,
            frame_rate: None,
            end: None,
        }
    }

    fn info() -> MediaInfo {
        // The subtitle comes first in the file, but is written last.
        MediaInfo {
            format: "mov,mp4".into(),
            duration: None,
            streams: vec![
                stream(0, StreamKind::Subtitle, "mov_text", Some("ger")),
                stream(1, StreamKind::Video, "h264", Some("und")),
                stream(2, StreamKind::Audio, "aac", Some("eng")),
                stream(3, StreamKind::Other, "bin_data", None),
            ],
        }
    }

    #[test]
    fn encoding_strips_by_default() {
        let a = s(&output_args(false, true, true));
        assert!(a.contains("-map_metadata -1"), "{a}");
        assert!(a.contains("-fflags +bitexact"), "{a}");
        assert!(a.contains("filter_units=remove_types=6"), "{a}");
        assert!(output_args(true, false, true).is_empty());
        assert_eq!(s(&output_args(true, true, false)), "-map_metadata -1 -map_chapters -1");
    }

    #[test]
    fn scrub_copies_without_tags_but_keeps_languages() {
        let a = s(&scrub_args(Path::new("/v/step1.mp4"), Path::new("/v/x.mp4"), &info(), false));
        assert!(a.contains("-map 0:v? -map 0:a? -map 0:s? -c copy"), "{a}");
        assert!(a.contains("-map_metadata -1 -map_chapters -1"), "{a}");
        // Video is output 0, audio 1, the subtitle 2; "und" isn't written.
        assert!(a.contains("-metadata:s:1 language=eng"), "{a}");
        assert!(a.contains("-metadata:s:2 language=ger"), "{a}");
        assert!(!a.contains("-metadata:s:0"), "{a}");
        assert!(a.contains("-bsf:v:0 filter_units=remove_types=6"), "{a}");
        assert!(a.contains("-fflags +bitexact"), "{a}");
        assert!(!a.contains("creation_time"), "{a}");
        assert!(a.ends_with("-movflags +faststart /v/x.mp4"), "{a}");
    }

    #[test]
    fn scrub_with_tags_dates_the_file() {
        let a = s(&scrub_args(Path::new("/v/a.mkv"), Path::new("/v/b.mkv"), &info(), true));
        assert!(a.contains("-map_metadata -1"), "source tags are never copied: {a}");
        assert!(a.contains("-metadata creation_time=now"), "{a}");
        assert!(!a.contains("bitexact") && !a.contains("filter_units"), "{a}");
        assert!(!a.contains("movflags"), "{a}");
    }

    #[test]
    fn ass_header_keeps_the_look_and_loses_the_author() {
        let ass = "\u{feff}; Script generated by Aegisub, /home/jane/subs\r\n\
            [Script Info]\r\n\
            Title: Jane's holiday\r\n\
            Original Script: jane@example.com\r\n\
            Script Updated By: Jane Doe\r\n\
            ScriptType: v4.00+\r\n\
            PlayResX: 1920\r\n\
            PlayResY: 1080\r\n\
            ; a note\r\n\
            \r\n\
            [Aegisub Project Garbage]\r\n\
            Audio File: /home/jane/Videos/raw.mkv\r\n\
            Video File: /home/jane/Videos/raw.mkv\r\n\
            \r\n\
            [V4+ Styles]\r\n\
            Format: Name, Fontname, Fontsize\r\n\
            Style: Default,Arial,48\r\n\
            \r\n\
            [Events]\r\n\
            Format: Layer, Start, End, Style, Text\r\n\
            Comment: 0,0:00:00.00,0:00:01.00,Default,ask jane@example.com\r\n\
            Sound: 0,0:00:00.00,0:00:01.00,Default,/home/jane/a.wav\r\n\
            Dialogue: 0,0:00:00.50,0:00:02.00,Default,Grüße, {\\i1}hi{\\i0}\r\n";
        let out = String::from_utf8(clean_ass(ass.as_bytes())).unwrap();
        assert_eq!(
            out,
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n\
             [V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,48\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Text\n\
             Dialogue: 0,0:00:00.50,0:00:02.00,Default,Grüße, {\\i1}hi{\\i0}\n"
        );
        // UTF-16 files come out as UTF-8, just as clean.
        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(ass.trim_start_matches('\u{feff}').encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(String::from_utf8(clean_ass(&utf16)).unwrap(), out);
        // Text that isn't UTF-8 is kept as it is.
        let latin1 = b"[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,Gr\xfc\xdfe\n";
        assert_eq!(clean_ass(latin1), latin1);
        assert!(is_ass(Path::new("a.SSA")) && !is_ass(Path::new("a.srt")));
    }

    #[test]
    fn webm_reencodes_and_converts_text_subtitles() {
        let a = s(&webm_args(Path::new("/v/a.mp4"), Path::new("/v/b.webm"), &info(), false));
        assert!(a.contains("libvpx-vp9") && a.contains("libopus"), "{a}");
        assert!(a.contains("-map 0:s -c:s webvtt"), "{a}");
        assert!(a.contains("+bitexact") && !a.contains("filter_units"), "{a}");
        let req = SaveRequest::new("/v/a.mp4".into(), "/v/b.WebM".into());
        assert!(req.is_webm() && req.changes_container());
        assert!(!SaveRequest::new("/v/a.MP4".into(), "/v/b.mp4".into()).changes_container());
        assert_eq!(staging_path(Path::new("/v/x.webm")), PathBuf::from("/v/.x.flicker-part.webm"));
    }
}
