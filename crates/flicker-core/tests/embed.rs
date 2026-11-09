//! End-to-end tests against the real ffmpeg. Skipped if ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use std::time::Duration;

use flicker_core::{
    Accuracy, AudioEdit, AudioRequest, CancelToken, EmbedMode, EmbedRequest, Engine, FfmpegCli, MediaInfo,
    SaveRequest, Selection,
    StreamKind, TimeRange, TrimRequest,
};

const SRT: &str = "1\n00:00:00,200 --> 00:00:01,500\nHello, world!\n\n2\n00:00:01,600 --> 00:00:02,800\nSecond line\n";

fn workdir(name: &str) -> Option<PathBuf> {
    if FfmpegCli::locate().version().is_err() {
        eprintln!("ffmpeg not found, skipping");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("flicker-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn make_video(path: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        // A keyframe every second so fast (copy) cuts have somewhere to land.
        .args(["-c:v", "libx264", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not create sample video");
}

fn run(req: &EmbedRequest) -> Vec<f32> {
    let mut seen = Vec::new();
    FfmpegCli::locate()
        .embed_subtitles(req, &mut |p| seen.extend(p.fraction()), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    seen
}

#[test]
fn soft_mp4_adds_tagged_track() {
    let Some(dir) = workdir("soft-mp4") else { return };
    let (video, subs, out) = (dir.join("in.mp4"), dir.join("subs.srt"), dir.join("out.mp4"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let mut req = EmbedRequest::new(video, subs, out.clone());
    req.language = Some("eng".into());
    let progress = run(&req);
    assert_eq!(progress.last().copied(), Some(1.0));

    let info = FfmpegCli::locate().probe(&out).unwrap();
    let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle).unwrap();
    assert_eq!(sub.codec, "mov_text");
    assert_eq!(sub.language.as_deref(), Some("eng"));
    assert_eq!(info.count(StreamKind::Audio), 1);
}

#[test]
fn soft_mkv_keeps_existing_and_appends() {
    let Some(dir) = workdir("soft-mkv") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let first = dir.join("one.mkv");
    run(&EmbedRequest::new(video, subs.clone(), first.clone()));
    let second = dir.join("two.mkv");
    run(&EmbedRequest::new(first, subs, second.clone()));

    let info = FfmpegCli::locate().probe(&second).unwrap();
    assert_eq!(info.count(StreamKind::Subtitle), 2);
}

#[test]
fn hard_burn_with_awkward_filename() {
    let Some(dir) = workdir("hard") else { return };
    let video = dir.join("in.mp4");
    // Quotes, colons, commas and brackets all need escaping in -vf.
    let subs = dir.join("it's [a]: weird, name.srt");
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let mut req = EmbedRequest::new(video, subs, dir.join("burned.mp4"));
    req.mode = EmbedMode::Hard;
    req.preset = "ultrafast".into();
    run(&req);

    let info = FfmpegCli::locate().probe(&req.output).unwrap();
    assert_eq!(info.count(StreamKind::Subtitle), 0);
    assert_eq!(info.first_video().unwrap().codec, "h264");
}

#[test]
fn rejects_output_equal_to_input() {
    let Some(dir) = workdir("same") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();
    let req = EmbedRequest::new(video.clone(), subs, video);
    let err = FfmpegCli::locate()
        .embed_subtitles(&req, &mut |_| {}, &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, flicker_core::Error::OutputIsInput));
}

fn decode_all(req: &flicker_core::PreviewRequest) -> Vec<flicker_core::Frame> {
    let mut src = FfmpegCli::locate().open_preview(req).unwrap();
    let mut frames = Vec::new();
    while let Some(f) = src.next_frame().unwrap() {
        frames.push(f);
    }
    frames
}

#[test]
fn preview_streams_scaled_frames() {
    let Some(dir) = workdir("preview") else { return };
    let video = dir.join("in.mp4");
    make_video(&video);

    let mut req = flicker_core::PreviewRequest::new(video);
    req.max_width = 160;
    req.fps = 10.0;
    let frames = decode_all(&req);
    assert!((59..=61).contains(&frames.len()), "got {} frames", frames.len());
    let f = &frames[0];
    assert_eq!((f.width, f.height), (160, 120));
    assert_eq!(f.rgb.len(), 160 * 120 * 3);
    assert_eq!(frames[10].pts, std::time::Duration::from_secs(1));
}

#[test]
fn preview_after_seek_draws_subtitles() {
    let Some(dir) = workdir("preview-subs") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    // 2.0s is inside the second cue (1.6–2.8s); seeking must keep timing right.
    let mut req = flicker_core::PreviewRequest::new(video);
    req.start = std::time::Duration::from_secs(2);
    req.single_frame = true;
    let plain = decode_all(&req);
    req.subtitle = Some(subs);
    let subbed = decode_all(&req);

    assert_eq!((plain.len(), subbed.len()), (1, 1));
    let changed = plain[0].rgb.iter().zip(&subbed[0].rgb).filter(|(a, b)| a != b).count();
    assert!(changed > 500, "subtitles not visible ({changed} bytes differ)");

    // 1.55s falls in the gap between cues, so nothing should be drawn.
    req.start = std::time::Duration::from_millis(1550);
    let gap = decode_all(&req);
    req.subtitle = None;
    let gap_plain = decode_all(&req);
    assert!(gap[0].rgb == gap_plain[0].rgb, "subtitle drawn outside its cue");
}

fn trim(name: &str, sel: &[(f64, f64)], selection: Selection, accuracy: Accuracy) -> Option<MediaInfo> {
    let dir = workdir(name)?;
    let video = dir.join("in.mp4");
    make_video(&video);
    let ranges: Vec<TimeRange> = sel
        .iter()
        .map(|&(a, b)| TimeRange::new(Duration::from_secs_f64(a), Duration::from_secs_f64(b)))
        .collect();
    let keep = flicker_core::edit::keep_ranges(&ranges, selection, Duration::from_secs(6));
    let mut req = TrimRequest::new(video, dir.join("out.mp4"), keep);
    req.accuracy = accuracy;
    req.preset = "ultrafast".into();
    let mut last = None;
    FfmpegCli::locate()
        .trim(&req, &mut |p| last = p.fraction(), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(last, Some(1.0));
    Some(FfmpegCli::locate().probe(&req.output).unwrap())
}

fn assert_secs(info: &MediaInfo, expected: f64, tolerance: f64) {
    let got = info.duration.unwrap().as_secs_f64();
    assert!((got - expected).abs() <= tolerance, "expected ~{expected}s, got {got}s");
}

#[test]
fn trim_fast_keeps_one_range() {
    let Some(info) = trim("trim-fast", &[(1.0, 3.0)], Selection::Keep, Accuracy::Fast) else { return };
    // Stream copy cuts on packet boundaries, so allow a few frames of slack.
    assert_secs(&info, 2.0, 0.25);
    assert_eq!(info.count(StreamKind::Audio), 1);
}

#[test]
fn trim_fast_joins_several_ranges() {
    let sel = [(1.0, 2.0), (3.0, 5.0)];
    let Some(info) = trim("trim-fast-multi", &sel, Selection::Keep, Accuracy::Fast) else { return };
    assert_secs(&info, 3.0, 0.3);
}

#[test]
fn cut_precise_removes_ranges_frame_accurately() {
    // Off-keyframe cut points: only re-encoding can hit these exactly.
    let sel = [(1.3, 2.1), (4.45, 5.0)];
    let Some(info) = trim("cut-precise", &sel, Selection::Remove, Accuracy::Precise) else { return };
    assert_secs(&info, 6.0 - 0.8 - 0.55, 0.08);
    assert_eq!(info.count(StreamKind::Audio), 1);
    assert_eq!(info.first_video().unwrap().codec, "h264");
}

/// 40s, 30fps, keyframe every 10s: like real camera/phone footage, where
/// fast cuts can't land anywhere they like.
fn make_long_gop_video(path: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=40"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=40"])
        .args(["-c:v", "libx264", "-g", "300", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not create long-GOP video");
}

/// Decode timestamps of the first video stream, in file order.
fn video_dts(path: &Path) -> Vec<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=dts_time"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

fn fast_trim_long_gop(name: &str, ext: &str, sel: &[(f64, f64)]) -> Option<PathBuf> {
    trim_long_gop(name, ext, sel, Accuracy::Fast)
}

fn trim_long_gop(name: &str, ext: &str, sel: &[(f64, f64)], accuracy: Accuracy) -> Option<PathBuf> {
    let dir = workdir(name)?;
    let video = dir.join(format!("in.{ext}"));
    make_long_gop_video(&video);
    let keep = sel
        .iter()
        .map(|&(a, b)| TimeRange::new(Duration::from_secs_f64(a), Duration::from_secs_f64(b)))
        .collect();
    let mut req = TrimRequest::new(video, dir.join(format!("out.{ext}")), keep);
    req.accuracy = accuracy;
    req.preset = "ultrafast".into();
    let mut last = None;
    FfmpegCli::locate()
        .trim(&req, &mut |p| last = p.fraction(), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(last, Some(1.0));
    Some(req.output)
}

fn decoded_frames(path: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// Right length, right number of frames (30fps), starts at zero, and no
/// timestamp jumps backwards by more than a frame (joins may nudge one).
fn assert_clean_timeline(out: &Path, expected_secs: f64) {
    let info = FfmpegCli::locate().probe(out).unwrap();
    assert_secs(&info, expected_secs, 0.25);
    let frames = decoded_frames(out) as f64;
    assert!((frames - expected_secs * 30.0).abs() <= 3.0, "{frames} frames for {expected_secs}s");
    let dts = video_dts(out);
    assert!(dts[0].abs() < 0.2, "video should start at ~0, starts at {}", dts[0]);
    let worst = dts.windows(2).map(|w| w[0] - w[1]).fold(0.0, f64::max);
    assert!(worst < 0.04, "timestamps jump back by {worst}s");
}

#[test]
fn engine_lists_keyframes() {
    let Some(dir) = workdir("keyframes") else { return };
    let video = dir.join("in.mp4");
    make_long_gop_video(&video);
    let keys = FfmpegCli::locate().keyframes(&video).unwrap();
    let secs: Vec<u64> = keys.iter().map(|k| k.as_secs_f64().round() as u64).collect();
    assert_eq!(secs, vec![0, 10, 20, 30]);
}

#[test]
fn fast_trim_mid_gop_starts_on_keyframe() {
    // 14–22 can only start at the 10s keyframe in copy mode: 12s, clean.
    for ext in ["mp4", "mkv"] {
        let name = format!("fast-gop-one-{ext}");
        let Some(out) = fast_trim_long_gop(&name, ext, &[(14.0, 22.0)]) else { return };
        assert_clean_timeline(&out, 12.0);
    }
}

#[test]
fn fast_trim_several_ranges_has_monotonic_timestamps() {
    // Regression: joining parts used to stack lead-in frames onto the
    // previous part's timestamps and scramble the timeline.
    for ext in ["mp4", "mkv"] {
        let name = format!("fast-gop-multi-{ext}");
        let Some(out) = fast_trim_long_gop(&name, ext, &[(4.0, 8.0), (24.0, 30.0)]) else { return };
        // Snapped to 0–8 and 20–30.
        assert_clean_timeline(&out, 18.0);
    }
}

#[test]
fn precise_trim_long_gop_mkv_is_exact() {
    let sel = [(14.0, 22.0), (31.5, 33.0)];
    let Some(out) = trim_long_gop("precise-gop-mkv", "mkv", &sel, Accuracy::Precise) else { return };
    assert_clean_timeline(&out, 9.5);
}

/// Long-GOP video with a mov_text subtitle track (like a video subtitled by
/// the Embed tool) whose cues sit at known times.
fn make_subbed_long_gop_video(dir: &Path) -> PathBuf {
    let plain = dir.join("plain.mp4");
    make_long_gop_video(&plain);
    let srt = dir.join("cues.srt");
    std::fs::write(
        &srt,
        "1\n00:00:05,000 --> 00:00:06,000\nA\n\n2\n00:00:15,000 --> 00:00:16,000\nB\n\n\
         3\n00:00:21,000 --> 00:00:25,000\nC\n\n4\n00:00:26,000 --> 00:00:27,000\nD\n",
    )
    .unwrap();
    let out = dir.join("subbed.mp4");
    let mut req = EmbedRequest::new(plain, srt, out.clone());
    req.language = Some("eng".into());
    FfmpegCli::locate().embed_subtitles(&req, &mut |_| {}, &CancelToken::new()).unwrap();
    out
}

/// Subtitle cues of a file as (start, end, text), read back via SRT.
fn cues(path: &Path) -> Vec<(f64, f64, String)> {
    let srt = path.with_extension("check.srt");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:s:0"])
        .arg(&srt)
        .status()
        .unwrap()
        .success();
    assert!(ok, "no subtitle track in {}", path.display());
    let t = |s: &str| {
        let (hms, ms) = s.trim().split_once(',').unwrap();
        let v: Vec<f64> = hms.split(':').map(|x| x.parse().unwrap()).collect();
        v[0] * 3600.0 + v[1] * 60.0 + v[2] + ms.parse::<f64>().unwrap() / 1000.0
    };
    std::fs::read_to_string(&srt)
        .unwrap()
        .split("\n\n")
        .filter_map(|b| {
            let mut l = b.lines().skip(1);
            let (a, z) = l.next()?.split_once("-->")?;
            Some((t(a), t(z), l.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

fn assert_cues(path: &Path, expected: &[(f64, f64, &str)]) {
    let got = cues(path);
    assert_eq!(got.len(), expected.len(), "cues: {got:?}");
    for ((s, e, t), (xs, xe, xt)) in got.iter().zip(expected) {
        assert!((s - xs).abs() < 0.1 && (e - xe).abs() < 0.1 && t == xt, "cues: {got:?}");
    }
    // The subtitle track must not outlast the video (that stretched the timeline).
    let info = FfmpegCli::locate().probe(path).unwrap();
    let last_end = got.last().map_or(0.0, |c| c.1);
    assert!(last_end <= info.duration.unwrap().as_secs_f64() + 0.05);
}

fn trim_subbed(name: &str, sel: &[(f64, f64)], accuracy: Accuracy) -> Option<PathBuf> {
    let dir = workdir(name)?;
    let video = make_subbed_long_gop_video(&dir);
    let keep = sel
        .iter()
        .map(|&(a, b)| TimeRange::new(Duration::from_secs_f64(a), Duration::from_secs_f64(b)))
        .collect();
    let mut req = TrimRequest::new(video, dir.join("out.mp4"), keep);
    req.accuracy = accuracy;
    req.preset = "ultrafast".into();
    FfmpegCli::locate().trim(&req, &mut |_| {}, &CancelToken::new()).unwrap_or_else(|e| panic!("{e}"));
    Some(req.output)
}

#[test]
fn fast_trim_retimes_subtitles() {
    // 14–22 really starts at the 10s keyframe: B at 5–6, C clipped to 11–12.
    let Some(out) = trim_subbed("subs-fast", &[(14.0, 22.0)], Accuracy::Fast) else { return };
    assert_clean_timeline(&out, 12.0);
    assert_cues(&out, &[(5.0, 6.0, "B"), (11.0, 12.0, "C")]);
    let info = FfmpegCli::locate().probe(&out).unwrap();
    let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle).unwrap();
    assert_eq!(sub.language.as_deref(), Some("eng"), "language tag carried over");
}

#[test]
fn precise_trim_retimes_subtitles() {
    let Some(out) = trim_subbed("subs-precise", &[(14.0, 22.0)], Accuracy::Precise) else { return };
    assert_clean_timeline(&out, 8.0);
    assert_cues(&out, &[(1.0, 2.0, "B"), (7.0, 8.0, "C")]);
}

#[test]
fn precise_cut_keeps_subtitles_across_parts() {
    // Keep 4–8 and 24–30: A → 1–2; C's tail 24–25 → 4–5; D → 6–7.
    let sel = [(4.0, 8.0), (24.0, 30.0)];
    let Some(out) = trim_subbed("subs-precise-multi", &sel, Accuracy::Precise) else { return };
    assert_clean_timeline(&out, 10.0);
    assert_cues(&out, &[(1.0, 2.0, "A"), (4.0, 5.0, "C"), (6.0, 7.0, "D")]);
}

fn tags(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags:stream_tags", "-of", "default"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_lowercase()
}

#[test]
fn saves_carry_no_source_encoder_or_date_tags() {
    let Some(dir) = workdir("tags") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    // A "phone video": title, location and a date in the file.
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=4"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-shortest", "-metadata", "title=Holiday", "-metadata", "location=+48.85+002.35/"])
        .args(["-metadata", "creation_time=2026-01-02T03:04:05Z"])
        .arg(&video)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    std::fs::write(&subs, SRT).unwrap();

    // An edit (a precise cut), then subtitles with a language, then Save As.
    let cut = dir.join("step1.mp4");
    let mut req = TrimRequest::new(video, cut.clone(), vec![TimeRange::new(Duration::from_secs(1), Duration::from_secs(3))]);
    req.accuracy = Accuracy::Precise;
    req.preset = "ultrafast".into();
    FfmpegCli::locate().trim(&req, &mut |_| {}, &CancelToken::new()).unwrap();
    let step = dir.join("step2.mp4");
    let mut req = EmbedRequest::new(cut, subs, step.clone());
    req.language = Some("ger".into());
    run(&req);

    for out in [dir.join("out.mp4"), dir.join("out.mkv")] {
        let save = SaveRequest::new(step.clone(), out.clone());
        FfmpegCli::locate().save(&save, &mut |_| {}, &CancelToken::new()).unwrap_or_else(|e| panic!("{e}"));
        let tags = tags(&out);
        for bad in ["creation_time", "holiday", "location", "lavc", "lavf5", "lavf6"] {
            assert!(!tags.contains(bad), "{bad} in {}: {tags}", out.display());
        }
        // Matroska must name the writing program: with bitexact it's just
        // "Lavf", no version. MP4 names nothing.
        let mp4 = out.extension().unwrap() == "mp4";
        assert!(!mp4 || !tags.contains("encoder"), "{tags}");
        let bytes = std::fs::read(&out).unwrap();
        for bad in [&b"x264"[..], b"Lavc"] {
            assert!(!bytes.windows(4).any(|w| w == bad), "encoder version left in {}", out.display());
        }
        assert!(!mp4 || !bytes.windows(4).any(|w| w == b"Lavf"));
        let info = FfmpegCli::locate().probe(&out).unwrap();
        let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle).expect("subtitle track kept");
        assert_eq!(sub.language.as_deref(), Some("ger"), "track language kept");
        assert!(!dir.join(".out.flicker-part.mkv").exists());
    }
}

#[test]
fn audio_edits_keep_the_picture() {
    let Some(dir) = workdir("audio") else { return };
    let video = dir.join("in.mp4");
    make_video(&video);
    let engine = FfmpegCli::locate();
    let volume = |path: &Path| -> f64 {
        let out = Command::new("ffmpeg")
            .args(["-v", "info", "-nostats", "-i"])
            .arg(path)
            .args(["-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stderr).into_owned();
        let line = log.lines().find(|l| l.contains("mean_volume:")).unwrap_or_else(|| panic!("{log}"));
        line.split("mean_volume:").nth(1).unwrap().trim().trim_end_matches(" dB").parse().unwrap()
    };
    let before = volume(&video);

    let quiet = dir.join("quiet.mp4");
    let req = AudioRequest::new(video.clone(), quiet.clone(), AudioEdit::Volume(0.5));
    engine.edit_audio(&req, &mut |_| {}, &CancelToken::new()).unwrap();
    let after = volume(&quiet);
    assert!((before - after - 6.0).abs() < 1.0, "half volume is about -6 dB: {before} → {after}");
    let info = engine.probe(&quiet).unwrap();
    assert_eq!(info.first_video().unwrap().codec, "h264");

    let faded = dir.join("faded.mkv");
    let fade = AudioEdit::Fade { fade_in: Duration::from_secs(2), fade_out: Duration::from_secs(2) };
    engine.edit_audio(&AudioRequest::new(video.clone(), faded.clone(), fade), &mut |_| {}, &CancelToken::new()).unwrap();
    assert!(volume(&faded) < before - 1.0);

    let muted = dir.join("muted.mp4");
    engine.edit_audio(&AudioRequest::new(video, muted.clone(), AudioEdit::Mute), &mut |_| {}, &CancelToken::new()).unwrap();
    let info = engine.probe(&muted).unwrap();
    assert_eq!(info.count(StreamKind::Audio), 0);
    assert_eq!(info.count(StreamKind::Video), 1);
}
