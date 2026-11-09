//! Trimming and cutting: turn a video into a list of kept time ranges.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Error, Result};

/// Ranges shorter than this are dropped (they'd produce empty segments).
const MIN_RANGE: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Duration,
    pub end: Duration,
}

impl TimeRange {
    pub fn new(a: Duration, b: Duration) -> Self {
        Self { start: a.min(b), end: a.max(b) }
    }

    pub fn len(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() < MIN_RANGE
    }

    pub fn contains(&self, t: Duration) -> bool {
        self.start <= t && t < self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Selection {
    /// Keep only the selected ranges (trim).
    #[default]
    Keep,
    /// Remove the selected ranges and keep everything else (cut).
    Remove,
}

/// Turn the user's selection into sorted, non-overlapping ranges to keep.
pub fn keep_ranges(selected: &[TimeRange], selection: Selection, duration: Duration) -> Vec<TimeRange> {
    let mut ranges: Vec<TimeRange> = selected
        .iter()
        .map(|r| TimeRange::new(r.start.min(duration), r.end.min(duration)))
        .filter(|r| !r.is_empty())
        .collect();
    ranges.sort_by_key(|r| r.start);

    let mut merged: Vec<TimeRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }

    match selection {
        Selection::Keep => merged,
        Selection::Remove => {
            let mut keep = Vec::with_capacity(merged.len() + 1);
            let mut cursor = Duration::ZERO;
            for r in &merged {
                keep.push(TimeRange::new(cursor, r.start));
                cursor = r.end;
            }
            keep.push(TimeRange::new(cursor, duration));
            keep.retain(|r| !r.is_empty());
            keep
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Accuracy {
    /// Copy streams without re-encoding. Near-instant and lossless, but cut
    /// points snap to the nearest earlier keyframe.
    #[default]
    Fast,
    /// Re-encode so cuts land on the exact frame.
    Precise,
}

#[derive(Clone, Debug)]
pub struct TrimRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Ranges to keep, in output order. Build with [`keep_ranges`].
    pub keep: Vec<TimeRange>,
    pub accuracy: Accuracy,
    /// x264 quality for precise mode.
    pub crf: u8,
    pub preset: String,
}

impl TrimRequest {
    pub fn new(input: PathBuf, output: PathBuf, keep: Vec<TimeRange>) -> Self {
        Self { input, output, keep, accuracy: Accuracy::Fast, crf: 18, preset: "medium".into() }
    }

    pub fn output_duration(&self) -> Duration {
        self.keep.iter().map(TimeRange::len).sum()
    }

    pub fn validate(&self) -> Result<()> {
        if !self.input.is_file() {
            return Err(Error::MissingFile(self.input.clone()));
        }
        if crate::subtitles::same_file(&self.input, &self.output) {
            return Err(Error::OutputIsInput);
        }
        if self.keep.iter().all(TimeRange::is_empty) {
            return Err(Error::NothingToKeep);
        }
        Ok(())
    }
}

/// Suggest `<name>.trimmed.<ext>` next to the input.
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let ext = input.extension().map(|e| e.to_string_lossy()).unwrap_or("mkv".into());
    input.with_file_name(format!("{stem}.trimmed.{ext}"))
}

/// Where Fast mode can really cut: stream copy must start each part on a
/// keyframe, so every start moves back to the keyframe at or before it.
/// Parts that then overlap (gaps shorter than the keyframe spacing) merge.
///
/// `keyframes` must be sorted. With no keyframe info, ranges are unchanged.
pub fn snap_to_keyframes(keep: &[TimeRange], keyframes: &[Duration]) -> Vec<TimeRange> {
    let mut out: Vec<TimeRange> = Vec::with_capacity(keep.len());
    for r in keep {
        let i = keyframes.partition_point(|k| *k <= r.start);
        let start = if i == 0 { r.start.min(keyframes.first().copied().unwrap_or(r.start)) } else { keyframes[i - 1] };
        let snapped = TimeRange::new(start, r.end);
        match out.last_mut() {
            Some(last) if snapped.start <= last.end => last.end = last.end.max(snapped.end),
            _ => out.push(snapped),
        }
    }
    out
}

fn strs(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

pub(crate) fn base_args() -> Vec<OsString> {
    strs(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"])
}

fn finish_args(a: &mut Vec<OsString>, output: &Path) {
    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov") {
        a.extend(strs(&["-movflags", "+faststart"]));
    }
    a.push(output.into());
}

/// Where the segment muxer should split the input so that every kept part
/// starts its own piece. Starts sit just *before* their keyframe (the muxer
/// splits at the first keyframe at or after each time, and rounding must not
/// push it to the next one); ends split at the next keyframe after them and
/// are trimmed exactly with `outpoint` when joining.
pub(crate) fn split_times(parts: &[TimeRange], keyframes: &[Duration]) -> Vec<Duration> {
    let mut times = Vec::with_capacity(parts.len() * 2);
    for p in parts {
        if !p.start.is_zero() {
            let i = keyframes.partition_point(|k| *k < p.start);
            let prev = if i == 0 { Duration::ZERO } else { keyframes[i - 1] };
            let margin = Duration::from_millis(50).min((p.start - prev) / 2);
            times.push(p.start - margin);
        }
        times.push(p.end);
    }
    times.sort();
    times.dedup();
    times
}

/// Fast mode, step 1: copy the input (up to `until`) into pieces split at
/// `times`, each with timestamps starting at zero, listed in `list` (CSV).
pub(crate) fn split_args(
    input: &Path,
    until: Duration,
    times: &[Duration],
    list: &Path,
    pattern: &Path,
) -> Vec<OsString> {
    let mut a = base_args();
    a.push("-i".into());
    a.push(input.into());
    a.push("-t".into());
    a.push(format!("{:.6}", until.as_secs_f64()).into());
    // Subtitles are retimed separately (see `retime`), not cut here.
    a.extend(strs(&["-map", "0:v", "-map", "0:a?", "-c", "copy"]));
    a.extend(strs(&["-f", "segment", "-reset_timestamps", "1", "-avoid_negative_ts", "make_zero"]));
    if !times.is_empty() {
        let joined: Vec<String> = times.iter().map(|t| format!("{:.6}", t.as_secs_f64())).collect();
        a.push("-segment_times".into());
        a.push(joined.join(",").into());
    }
    a.extend(strs(&["-segment_list_type", "csv", "-segment_list"]));
    a.push(list.into());
    a.push(pattern.into());
    a
}

/// One piece written by the segment muxer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Piece {
    pub file: PathBuf,
    /// Where the piece starts in the source.
    pub start: Duration,
}

/// Parse the segment muxer's CSV list (`name,start,end` per line). Names are
/// relative to the list's directory.
pub(crate) fn parse_pieces(csv: &str, dir: &Path) -> Vec<Piece> {
    csv.lines()
        .filter_map(|l| {
            let mut cols = l.split(',');
            let name = cols.next()?.trim();
            let start: f64 = cols.next()?.trim().parse().ok()?;
            (!name.is_empty() && start.is_finite() && start >= 0.0)
                .then(|| Piece { file: dir.join(name), start: Duration::from_secs_f64(start) })
        })
        .collect()
}

/// For each kept part, the piece that starts it and how long to play from it:
/// the last piece starting no later than the part (plus slack, since piece
/// start times include the codec delay).
pub(crate) fn pick_pieces(parts: &[TimeRange], pieces: &[Piece]) -> Option<Vec<(PathBuf, Duration)>> {
    const SLACK: Duration = Duration::from_millis(250);
    parts
        .iter()
        .map(|p| {
            let piece = pieces.iter().rev().find(|x| x.start <= p.start + SLACK)?;
            Some((piece.file.clone(), p.len()))
        })
        .collect()
}

/// Concat-demuxer list playing each piece from its start for the given time.
pub(crate) fn concat_list(parts: &[(PathBuf, Duration)]) -> String {
    let mut s = String::from("ffconcat version 1.0\n");
    for (p, len) in parts {
        let path = p.to_string_lossy().replace('\\', "/").replace('\'', r"'\''");
        s += &format!("file '{path}'\noutpoint {:.6}\n", len.as_secs_f64());
    }
    s
}

/// Fast mode, joining: stream-copy the parts listed in `list` into `output`.
/// `total` is the output length.
pub(crate) fn concat_args(list: &Path, output: &Path, total: Duration) -> Vec<OsString> {
    let mut a = base_args();
    a.extend(strs(&["-f", "concat", "-safe", "0", "-i"]));
    a.push(list.into());
    a.extend(strs(&["-map", "0", "-c", "copy"]));
    a.push("-t".into());
    a.push(format!("{:.6}", total.as_secs_f64()).into());
    finish_args(&mut a, output);
    a
}

pub(crate) fn is_text_subtitle(codec: &str) -> bool {
    matches!(codec, "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "mov_text" | "text")
}

/// Subtitle encoder for each text track, given the output container. Image
/// subtitles (PGS, DVD) are skipped: they can't be re-encoded or retimed.
pub(crate) fn subtitle_codec_args(subs: &[String], output: &Path) -> Vec<OsString> {
    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut a = Vec::new();
    for (i, codec) in subs.iter().enumerate() {
        if !is_text_subtitle(codec) {
            continue;
        }
        let target = match ext.as_str() {
            "mp4" | "m4v" | "mov" => "mov_text",
            "webm" => "webvtt",
            _ => match codec.as_str() {
                "ass" | "ssa" => "ass",
                "webvtt" => "webvtt",
                _ => "srt",
            },
        };
        a.push(format!("-c:s:{i}").into());
        a.push(target.into());
    }
    a
}

/// Final step when subtitles exist: copy video/audio from `av`, add each
/// retimed subtitle file (`(index in source, codec, file)`), and carry over
/// each track's language/title from the source.
pub(crate) fn mux_subtitles_args(
    av: &Path,
    subs: &[(usize, String, PathBuf)],
    source: &Path,
    output: &Path,
) -> Vec<OsString> {
    let mut a = base_args();
    a.push("-i".into());
    a.push(av.into());
    for (_, _, file) in subs {
        a.push("-i".into());
        a.push(file.into());
    }
    let source_input = subs.len() + 1;
    a.push("-i".into());
    a.push(source.into());
    a.extend(strs(&["-map", "0:v", "-map", "0:a?"]));
    for k in 0..subs.len() {
        a.push("-map".into());
        a.push(format!("{}:0", k + 1).into());
    }
    a.extend(strs(&["-c", "copy"]));
    let codecs: Vec<String> = subs.iter().map(|(_, c, _)| c.clone()).collect();
    a.extend(subtitle_codec_args(&codecs, output));
    for (k, (i, _, _)) in subs.iter().enumerate() {
        a.push(format!("-map_metadata:s:s:{k}").into());
        a.push(format!("{source_input}:s:s:{i}").into());
    }
    finish_args(&mut a, output);
    a
}

/// Precise mode: one accurately-seeked input per range, joined with the
/// concat filter and re-encoded. `has_audio` says whether the input has audio.
/// Writes video/audio to `output`; subtitles are added afterwards (see `retime`).
pub(crate) fn precise_args(req: &TrimRequest, has_audio: bool, output: &Path) -> Vec<OsString> {
    let mut a = base_args();
    let mut pads = String::new();
    for (i, r) in req.keep.iter().enumerate() {
        a.push("-ss".into());
        a.push(format!("{:.3}", r.start.as_secs_f64()).into());
        a.push("-t".into());
        a.push(format!("{:.3}", r.len().as_secs_f64()).into());
        a.push("-i".into());
        a.push(req.input.clone().into());
        pads += &format!("[{i}:v:0]");
        if has_audio {
            pads += &format!("[{i}:a:0]");
        }
    }
    let n = req.keep.len();
    let (audio, out) = if has_audio { (1, "[v][a]") } else { (0, "[v]") };
    a.push("-filter_complex".into());
    a.push(format!("{pads}concat=n={n}:v=1:a={audio}{out}").into());
    a.extend(strs(&["-map", "[v]"]));
    if has_audio {
        a.extend(strs(&["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]));
    }
    let crf = req.crf.to_string();
    a.extend(strs(&["-c:v", "libx264", "-crf", &crf, "-preset", &req.preset]));
    a.extend(strs(&["-pix_fmt", "yuv420p"]));
    finish_args(&mut a, output);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }
    fn r(a: f64, b: f64) -> TimeRange {
        TimeRange::new(s(a), s(b))
    }

    #[test]
    fn keep_merges_sorts_and_clamps() {
        let sel = [r(5.0, 8.0), r(1.0, 3.0), r(2.5, 4.0), r(9.0, 20.0)];
        assert_eq!(
            keep_ranges(&sel, Selection::Keep, s(10.0)),
            vec![r(1.0, 4.0), r(5.0, 8.0), r(9.0, 10.0)]
        );
    }

    #[test]
    fn remove_inverts_selection() {
        let sel = [r(2.0, 3.0), r(0.0, 1.0)];
        assert_eq!(keep_ranges(&sel, Selection::Remove, s(5.0)), vec![r(1.0, 2.0), r(3.0, 5.0)]);
        // Removing everything leaves nothing.
        assert!(keep_ranges(&[r(0.0, 5.0)], Selection::Remove, s(5.0)).is_empty());
    }

    #[test]
    fn snapping_moves_starts_back_to_keyframes() {
        let kf: Vec<Duration> = [0.0, 10.0, 20.0, 30.0].map(s).to_vec();
        // Start on a keyframe stays put; others move back.
        assert_eq!(snap_to_keyframes(&[r(10.0, 12.0)], &kf), vec![r(10.0, 12.0)]);
        assert_eq!(snap_to_keyframes(&[r(14.0, 22.0)], &kf), vec![r(10.0, 22.0)]);
        // Second part would start inside the first: merged, never duplicated.
        assert_eq!(snap_to_keyframes(&[r(1.0, 8.0), r(9.0, 15.0)], &kf), vec![r(0.0, 15.0)]);
        assert_eq!(
            snap_to_keyframes(&[r(4.0, 8.0), r(24.0, 30.0)], &kf),
            vec![r(0.0, 8.0), r(20.0, 30.0)]
        );
        // No keyframe info: unchanged.
        assert_eq!(snap_to_keyframes(&[r(4.0, 8.0)], &[]), vec![r(4.0, 8.0)]);
    }

    #[test]
    fn concat_list_escapes_quotes_and_sets_outpoints() {
        assert_eq!(
            concat_list(&[("/tmp/it's/0.mp4".into(), s(8.0)), ("/tmp/1.mp4".into(), s(2.5))]),
            "ffconcat version 1.0\nfile '/tmp/it'\\''s/0.mp4'\noutpoint 8.000000\n\
             file '/tmp/1.mp4'\noutpoint 2.500000\n"
        );
    }

    #[test]
    fn split_times_sit_just_before_starts() {
        let kf: Vec<Duration> = [0.0, 10.0, 20.0, 30.0].map(s).to_vec();
        let parts = [r(0.0, 8.0), r(20.0, 30.0)];
        assert_eq!(split_times(&parts, &kf), vec![s(8.0), s(19.95), s(30.0)]);
        // Keyframes closer than 100ms: margin shrinks to half the gap.
        let kf: Vec<Duration> = [0.0, 1.0, 1.04].map(s).to_vec();
        assert_eq!(split_times(&[r(1.04, 2.0)], &kf), vec![s(1.02), s(2.0)]);
    }

    #[test]
    fn pieces_are_parsed_and_matched_to_parts() {
        let csv = "p000.mkv,0.000000,10.067000\np001.mkv,10.067000,20.067000\n\
                   p002.mkv,20.067000,30.067000\n";
        let pieces = parse_pieces(csv, Path::new("/t"));
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[2], Piece { file: "/t/p002.mkv".into(), start: s(20.067) });
        assert_eq!(
            pick_pieces(&[r(0.0, 8.0), r(20.0, 30.0)], &pieces).unwrap(),
            vec![("/t/p000.mkv".into(), s(8.0)), ("/t/p002.mkv".into(), s(10.0))]
        );
    }

    #[test]
    fn text_subtitles_are_reencoded_for_the_container() {
        let subs = ["mov_text".to_string(), "hdmv_pgs_subtitle".into(), "ass".into()];
        let joined = |out: &str| {
            subtitle_codec_args(&subs, Path::new(out))
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        // Bitmap track 1 is left to "-c copy".
        assert_eq!(joined("o.mp4"), "-c:s:0 mov_text -c:s:2 mov_text");
        assert_eq!(joined("o.mkv"), "-c:s:0 srt -c:s:2 ass");
        assert_eq!(joined("o.webm"), "-c:s:0 webvtt -c:s:2 webvtt");
    }

    #[test]
    fn precise_args_join_ranges() {
        let mut req =
            TrimRequest::new("in.mp4".into(), "o.mp4".into(), vec![r(1.0, 2.0), r(4.0, 6.0)]);
        req.accuracy = Accuracy::Precise;
        let joined = |audio| {
            precise_args(&req, audio, Path::new("o.mp4"))
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let with_audio = joined(true);
        assert!(with_audio.contains("-ss 4.000 -t 2.000 -i in.mp4"), "{with_audio}");
        assert!(with_audio.contains("[0:v:0][0:a:0][1:v:0][1:a:0]concat=n=2:v=1:a=1[v][a]"));
        assert!(joined(false).contains("[0:v:0][1:v:0]concat=n=2:v=1:a=0[v]"));
    }
}
