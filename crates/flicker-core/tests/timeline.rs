//! End-to-end tests of rendering the timeline (export and the live preview)
//! against the real ffmpeg. Skipped if ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use flicker_core::{
    CancelToken, Canvas, Clip, ClipRef, Engine, ExportFormat, ExportRequest, FfmpegCli, StreamKind, SubtitleExport,
    Timeline, TimelinePreview, TrackKind, TrackRef, TransitionKind,
};

fn workdir(name: &str) -> Option<PathBuf> {
    if FfmpegCli::locate().version().is_err() {
        eprintln!("ffmpeg not found, skipping");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("flicker-tl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn ffmpeg(args: &[&str], out: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(args)
        .arg(out)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not create {}", out.display());
}

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// A 4s 320x240 25fps clip with a 440 Hz tone, a 3s 640x360 clip without
/// sound, a red picture and 5s of a 1 kHz tone (MP3).
fn sources(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let (a, b, img, music) = (dir.join("a.mp4"), dir.join("b.mkv"), dir.join("still.png"), dir.join("music.mp3"));
    ffmpeg(
        &["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=4", "-f", "lavfi", "-i",
          "sine=frequency=440:duration=4", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"],
        &a,
    );
    ffmpeg(&["-f", "lavfi", "-i", "smptebars=size=640x360:rate=30:duration=3", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &b);
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red:size=200x300", "-frames:v", "1"], &img);
    // MP3: its decoder gives silence just after a seek, which the preview
    // must hide.
    ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=1000:duration=5:sample_rate=44100", "-c:a", "libmp3lame"], &music);
    (a, b, img, music)
}

fn clip(path: &Path) -> Clip {
    let info = FfmpegCli::locate().probe(path).unwrap();
    let audio = info.count(StreamKind::Audio) > 0;
    match info.first_video() {
        Some(v) => {
            let mut c = Clip::video(path.to_path_buf(), info.video_length().unwrap(), audio);
            (c.width, c.height, c.fps) = (v.width, v.height, v.frame_rate);
            c
        }
        None => Clip::audio(path.to_path_buf(), info.duration.unwrap()),
    }
}

/// The 5s of music (the MP3 claims a little more: encoder padding).
fn music_clip(music: &Path) -> Clip {
    let mut c = clip(music);
    c.out_point = s(5);
    c
}

/// V1: a (0-4), picture (4-6), b (6-9). A1: music at 1s (1-6).
fn timeline(a: &Path, b: &Path, img: &Path, music: &Path) -> Timeline {
    let mut t = Timeline::default();
    t.append(TrackRef::V1, clip(a)).unwrap();
    let mut pic = Clip::image(img.to_path_buf());
    pic.out_point = s(2);
    t.append(TrackRef::V1, pic).unwrap();
    t.append(TrackRef::V1, clip(b)).unwrap();
    t.add_clip(TrackRef::A1, music_clip(music), s(1)).unwrap();
    t
}

fn export(req: &ExportRequest) -> Vec<f32> {
    let mut seen = Vec::new();
    FfmpegCli::locate()
        .export_timeline(req, &mut |p| seen.extend(p.fraction()), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    seen
}

fn tags(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags:stream_tags", "-of", "default"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_lowercase()
}

/// No encoder names, versions or dates. (Matroska/WebM must name the
/// writing program; with bitexact it's just "Lavf", no version.)
fn assert_no_tags(path: &Path) {
    let t = tags(path);
    for bad in ["lavc", "libx264", "libvpx", "creation_time", "lavf6", "lavf5"] {
        assert!(!t.contains(bad), "{bad} in {t}");
    }
}

fn assert_close(a: Duration, b: Duration) {
    let diff = (a.as_secs_f64() - b.as_secs_f64()).abs();
    assert!(diff < 0.15, "{a:?} vs {b:?}");
}

/// Decode `path`'s sound as mono 8 kHz samples.
fn pcm(path: &Path) -> Vec<i16> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
        .output()
        .unwrap();
    out.stdout.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// Loudness (RMS) of `samples` between two times at `rate` per second.
fn rms(samples: &[i16], rate: f64, from: f64, to: f64) -> f64 {
    let part = &samples[(from * rate) as usize..((to * rate) as usize).min(samples.len())];
    (part.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / part.len().max(1) as f64).sqrt()
}

#[test]
fn export_mp4_with_tracks_transitions_and_music_is_private() {
    let Some(dir) = workdir("mp4") else { return };
    let (a, b, img, music) = sources(&dir);
    let mut t = timeline(&a, &b, &img, &music);
    // Split a at 2s and drop the second half, closing the gap: 2 + 2 + 3 = 7s.
    let v1 = |i| ClipRef { track: TrackRef::V1, index: i };
    t.split(v1(0), s(2)).unwrap();
    t.ripple_remove(v1(1)).unwrap();
    t.video[0].clips[0].volume = 0.5;
    t.video[0].clips[0].fade_in = Duration::from_millis(500);
    // Picture → bars: 1s cross-fade; 6s.
    assert!(t.set_transition(v1(2), Some((TransitionKind::Crossfade, s(1)))));
    // A logo on V2 from 0.5s to 1.5s.
    let v2 = t.add_track(TrackKind::Video);
    let mut logo = Clip::image(img.clone());
    logo.out_point = s(1);
    t.add_clip(v2, logo, Duration::from_millis(500)).unwrap();
    assert_eq!(t.duration(), s(6));

    let out = dir.join("out.mp4");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    req.preset = "ultrafast".into();
    // The first video's size and frame rate; the bars are letterboxed into it.
    assert_eq!(req.canvas, Canvas { width: 320, height: 240, fps: 25.0 });
    let progress = export(&req);
    assert_eq!(progress.last().copied(), Some(1.0));

    let info = FfmpegCli::locate().probe(&out).unwrap();
    assert_close(info.duration.unwrap(), s(6));
    let v = info.first_video().unwrap();
    assert_eq!((v.codec.as_str(), v.width, v.height), ("h264", Some(320), Some(240)));
    let audio = info.streams.iter().find(|s| s.kind == StreamKind::Audio).unwrap();
    assert_eq!(audio.codec, "aac");

    // The clip's tone plays from the start, the music from 1s to 6s.
    let p = pcm(&out);
    assert!(rms(&p, 8000.0, 0.6, 0.9) > 500.0, "a's sound");
    assert!(rms(&p, 8000.0, 2.5, 3.5) > 1000.0, "music");

    assert_no_tags(&out);
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.windows(4).any(|w| w == b"x264"), "x264 SEI left in the file");
    assert!(!bytes.windows(4).any(|w| w == b"Lavf"), "muxer name left in the file");
}

#[test]
fn export_webm_with_dip_to_black_and_subtitle_track() {
    let Some(dir) = workdir("webm") else { return };
    let (a, b, img, music) = sources(&dir);
    let mut t = timeline(&a, &b, &img, &music);
    t.set_transition(ClipRef { track: TrackRef::V1, index: 1 }, Some((TransitionKind::DipToBlack, Duration::from_millis(500))));
    t.audio[0].muted = true;
    let subs = dir.join("s.srt");
    std::fs::write(&subs, "1\n00:00:00,500 --> 00:00:02,000\nHello\n").unwrap();
    t.subtitle = Some(subs);
    t.subtitle_export = SubtitleExport::Track;
    let out = dir.join("out.webm");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    assert_eq!(req.format, ExportFormat::WebM);
    req.canvas = Canvas { width: 160, height: 120, fps: 15.0 };
    export(&req);
    let info = FfmpegCli::locate().probe(&out).unwrap();
    assert_close(info.duration.unwrap(), t.duration());
    assert_eq!(info.first_video().unwrap().codec, "vp9");
    assert!(info.streams.iter().any(|s| s.kind == StreamKind::Audio && s.codec == "opus"));
    let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle).expect("subtitle track");
    assert_eq!((sub.codec.as_str(), sub.language.as_deref()), ("webvtt", Some("eng")));
    assert_no_tags(&out);

    // Mid-dip (the join is at 3.75s) the picture is black.
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "3.75", "-i"])
        .arg(&out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap()
        .stdout;
    let mean = frame.iter().map(|&x| x as f64).sum::<f64>() / frame.len() as f64;
    assert!(mean < 30.0, "mean luma {mean}");
}

#[test]
fn cancel_removes_partial_file() {
    let Some(dir) = workdir("cancel") else { return };
    let (a, b, img, music) = sources(&dir);
    let out = dir.join("out.mp4");
    let req = ExportRequest::new(timeline(&a, &b, &img, &music), out.clone());
    let cancel = CancelToken::new();
    cancel.cancel();
    let r = FfmpegCli::locate().export_timeline(&req, &mut |_| {}, &cancel);
    assert!(matches!(r, Err(flicker_core::Error::Cancelled)), "{r:?}");
    assert!(!out.exists());
}

fn preview(t: &Timeline, start: Duration, single: bool) -> TimelinePreview {
    TimelinePreview {
        timeline: Arc::new(t.clone()),
        canvas: Canvas { width: 160, height: 120, fps: 10.0 },
        start,
        subtitles: false,
        single_frame: single,
    }
}

#[test]
fn preview_sound_is_the_mix_with_no_gap_between_stretches() {
    let Some(dir) = workdir("pcm") else { return };
    let (a, b, img, music) = sources(&dir);
    let mut t = timeline(&a, &b, &img, &music);
    // Music again at 7s (7-12s), playing across the preview stretch
    // boundary at 9s (stretches start at 1s: 1-9, 9-12).
    t.add_clip(TrackRef::A1, music_clip(&music), s(7)).unwrap();
    assert_eq!(t.duration(), s(12));
    let mut src = FfmpegCli::locate().open_timeline_sound(&preview(&t, s(1), false)).unwrap();
    let mut bytes = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = src.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        assert_eq!(n % 4, 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    // Exactly 11s of 48 kHz stereo.
    assert_eq!(bytes.len(), 11 * 48_000 * 4);
    let left: Vec<i16> = bytes.as_chunks::<4>().0.iter().map(|f| i16::from_le_bytes([f[0], f[1]])).collect();
    // From 1s: music + a's tone (1-4s), music (4-6s), nothing (6-7s), music (7-12s).
    assert!(rms(&left, 48_000.0, 0.2, 2.8) > 1000.0);
    assert!(rms(&left, 48_000.0, 5.2, 5.8) < 50.0, "{}", rms(&left, 48_000.0, 5.2, 5.8));
    // No gap where the second stretch starts (9s = 8s in).
    for k in 0..300 {
        let from = 6.5 + k as f64 * 0.01;
        assert!(rms(&left, 48_000.0, from, from + 0.01) > 1000.0, "gap at {from}s in");
    }
}

#[test]
fn preview_frames_and_single_frame() {
    let Some(dir) = workdir("frames") else { return };
    let (a, b, img, music) = sources(&dir);
    let t = timeline(&a, &b, &img, &music);
    let engine = FfmpegCli::locate();
    // The red picture plays 4-6s.
    let mut one = engine.open_timeline_frames(&preview(&t, Duration::from_millis(5_000), true)).unwrap();
    let f = one.next_frame().unwrap().unwrap();
    assert_eq!((f.width, f.height), (160, 120));
    let mid = ((60 * 160 + 80) * 3) as usize;
    assert!(f.rgb[mid] > 200 && f.rgb[mid + 1] < 60, "red: {:?}", &f.rgb[mid..mid + 3]);
    assert_eq!(f.pts, s(5));
    assert!(one.next_frame().unwrap().is_none());

    // Playing from 3s: 6s at 10 fps, frames in order, across stretches.
    let mut all = engine.open_timeline_frames(&preview(&t, s(3), false)).unwrap();
    let mut pts = Vec::new();
    while let Some(f) = all.next_frame().unwrap() {
        pts.push(f.pts);
    }
    assert!((59..=61).contains(&pts.len()), "{}", pts.len());
    assert_eq!(pts[0], s(3));
    assert!(pts.windows(2).all(|w| w[0] < w[1]));
}

/// Everything in `dir`, by name.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Every container, with the subtitles as a track or burnt in: one pass
/// that writes just the finished file, with nothing in it that names the
/// encoder, its version or a date. Track languages are kept.
#[test]
fn every_export_writes_one_private_file_and_nothing_else() {
    let Some(dir) = workdir("onefile") else { return };
    let (a, b, img, music) = sources(&dir);
    let subs = dir.join("s.srt");
    std::fs::write(&subs, "1\n00:00:00,500 --> 00:00:02,000\nHello\n").unwrap();
    let out_dir = dir.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    for (name, mode) in [
        ("track.mkv", SubtitleExport::Track),
        ("track.mov", SubtitleExport::Track),
        ("track.mp4", SubtitleExport::Track),
        ("burn.mp4", SubtitleExport::Burn),
        ("burn.webm", SubtitleExport::Burn),
    ] {
        let mut t = timeline(&a, &b, &img, &music);
        t.subtitle = Some(subs.clone());
        t.subtitle_export = mode;
        t.subtitle_language = "ger".into();
        let out = out_dir.join(name);
        let before = listing(&out_dir);
        let mut req = ExportRequest::new(t, out.clone());
        req.preset = "ultrafast".into();
        req.canvas = Canvas { width: 160, height: 120, fps: 15.0 };
        export(&req);

        let mut expected = before.clone();
        expected.push(name.to_owned());
        expected.sort();
        assert_eq!(listing(&out_dir), expected, "{name}: exactly one new file");

        assert_no_tags(&out);
        let bytes = std::fs::read(&out).unwrap();
        for bad in [&b"x264"[..], b"Lavc", b"libx264"] {
            assert!(!bytes.windows(bad.len()).any(|w| w == bad), "{} left in {name}", String::from_utf8_lossy(bad));
        }
        let info = FfmpegCli::locate().probe(&out).unwrap();
        let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle);
        match mode {
            SubtitleExport::Track => assert_eq!(sub.expect("subtitle track").language.as_deref(), Some("ger"), "{name}"),
            _ => assert!(sub.is_none(), "{name}"),
        }
    }
}

/// With "Write encoder name and dates" on, the same single pass dates the
/// file (and still copies no tags from the sources).
#[test]
fn export_with_tags_dates_the_file() {
    let Some(dir) = workdir("withtags") else { return };
    let (a, b, img, music) = sources(&dir);
    let out = dir.join("tagged.mkv");
    let mut req = ExportRequest::new(timeline(&a, &b, &img, &music), out.clone());
    req.preset = "ultrafast".into();
    req.canvas = Canvas { width: 160, height: 120, fps: 15.0 };
    req.keep_tags = true;
    export(&req);
    let t = tags(&out);
    assert!(t.contains("lavf") && t.contains("libx264"), "{t}");
}

#[test]
fn a_cancelled_export_leaves_nothing() {
    let Some(dir) = workdir("cancelmid") else { return };
    let (a, b, img, music) = sources(&dir);
    let out_dir = dir.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let out = out_dir.join("out.mp4");
    let mut t = timeline(&a, &b, &img, &music);
    // Long enough to still be running at the first progress report.
    for _ in 0..6 {
        t.append(TrackRef::V1, clip(&a)).unwrap();
    }
    let req = ExportRequest::new(t, out.clone());
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let mut reports = 0;
    let r = FfmpegCli::locate().export_timeline(
        &req,
        &mut |_| {
            reports += 1;
            token.cancel();
        },
        &cancel,
    );
    assert!(matches!(r, Err(flicker_core::Error::Cancelled)), "{r:?}");
    assert!(reports >= 1, "cancelled while running");
    assert!(listing(&out_dir).is_empty(), "{:?}", listing(&out_dir));

    // A file already there that ffmpeg never got to is left alone.
    std::fs::write(&out, b"old").unwrap();
    let mut req = req;
    req.timeline.video[0].clips[0].path = dir.join("missing.mp4");
    assert!(FfmpegCli::locate().export_timeline(&req, &mut |_| {}, &CancelToken::new()).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
}

/// A project from someone else that names a web address as a clip (and as
/// its subtitles), in a file next to it and in the project itself: opening
/// it, showing a frame, playing its sound and exporting never connect
/// anywhere. The address is a listener on this computer.
#[test]
fn a_project_never_makes_ffmpeg_connect_out() {
    let Some(dir) = workdir("offline") else { return };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    let real = dir.join("real.mp4");
    ffmpeg(
        &["-f", "lavfi", "-i", "color=c=blue:size=160x120:rate=10:duration=2", "-f", "lavfi", "-i",
          "anullsrc=r=44100:cl=stereo", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"],
        &real,
    );
    // A playlist dressed up as a video, next to the project.
    let list = dir.join("list.mp4");
    std::fs::write(&list, format!("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\n{url}/seg.ts\n#EXT-X-ENDLIST\n")).unwrap();

    let mut t = Timeline::default();
    t.append(TrackRef::V1, Clip::video(format!("{url}/x.mp4").into(), s(2), true)).unwrap();
    t.append(TrackRef::V1, Clip::video(list.clone(), s(2), true)).unwrap();
    t.append(TrackRef::V1, clip(&real)).unwrap();
    t.add_clip(TrackRef::A1, Clip::audio(format!("{url}/x.mp3").into(), s(2)), s(0)).unwrap();
    t.subtitle = Some(format!("{url}/s.srt").into());
    let file = dir.join("p.flicker");
    std::fs::write(&file, &*flicker_core::project::to_json(&t)).unwrap();

    let opened = flicker_core::project::load(&file, None).unwrap();
    assert!(opened.was_plain);
    let t = opened.timeline;
    assert_eq!(t.video[0].clips.len(), 3, "no clip is dropped");
    assert_eq!(t.video[0].clips[0].path, dir.join("x.mp4"));
    assert_eq!(t.video[0].clips[2].path, real);

    let engine = FfmpegCli::locate();
    assert!(engine.probe(&list).is_err());
    // The frame the player shows as soon as a project opens, the sound, and
    // an export: each fails on the missing files (or is refused) quietly.
    for at in [0, 2, 4] {
        let shown = engine.open_timeline_frames(&preview(&t, s(at), true)).and_then(|mut f| f.next_frame());
        assert_eq!(shown.is_ok(), at == 4, "at {at}s");
    }
    if let Ok(mut sound) = engine.open_timeline_sound(&preview(&t, s(0), false)) {
        let mut buf = vec![0u8; 1 << 16];
        while matches!(sound.read(&mut buf), Ok(n) if n > 0) {}
    }
    let out = dir.join("out.mp4");
    assert!(engine.export_timeline(&ExportRequest::new(t.clone(), out.clone()), &mut |_| {}, &CancelToken::new()).is_err());

    // The same without the check when opening: what the timeline names
    // still reaches ffmpeg as a file's name only.
    let mut raw = Timeline::default();
    raw.append(TrackRef::V1, Clip::video(format!("{url}/x.mp4").into(), s(2), true)).unwrap();
    raw.add_clip(TrackRef::A1, Clip::audio(format!("{url}/x.mp3").into(), s(2)), s(0)).unwrap();
    raw.subtitle = Some(format!("{url}/s.srt").into());
    let with_subtitles = TimelinePreview { subtitles: true, ..preview(&raw, s(0), true) };
    assert!(engine.open_timeline_frames(&with_subtitles).and_then(|mut f| f.next_frame()).is_err());
    if let Ok(mut sound) = engine.open_timeline_sound(&preview(&raw, s(0), false)) {
        let mut buf = vec![0u8; 1 << 16];
        while matches!(sound.read(&mut buf), Ok(n) if n > 0) {}
    }

    std::thread::sleep(Duration::from_millis(300));
    assert!(listener.accept().is_err(), "ffmpeg connected to {url}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A 320x160 picture: red on the left half, blue on the right.
fn halves(dir: &Path) -> PathBuf {
    let p = dir.join("halves.png");
    ffmpeg(
        &["-f", "lavfi", "-i", "color=c=red:size=160x160", "-f", "lavfi", "-i", "color=c=blue:size=160x160",
          "-filter_complex", "[0][1]hstack", "-frames:v", "1"],
        &p,
    );
    p
}

/// The preview's frame at `at` (160x120), as RGB at (x, y).
fn pixel_at(t: &Timeline, at: Duration, x: usize, y: usize) -> [u8; 3] {
    let mut one = FfmpegCli::locate().open_timeline_frames(&preview(t, at, true)).unwrap();
    let f = one.next_frame().unwrap().unwrap();
    let i = (y * f.width as usize + x) * 3;
    [f.rgb[i], f.rgb[i + 1], f.rgb[i + 2]]
}

fn is_red(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 70 && p[2] < 70
}

fn is_blue(p: [u8; 3]) -> bool {
    p[2] > 180 && p[0] < 70 && p[1] < 70
}

fn is_black(p: [u8; 3]) -> bool {
    p.iter().all(|&c| c < 30)
}

#[test]
fn preview_shows_flips_turns_crop_opacity_and_effects() {
    use flicker_core::timeline::{Anim, Crop, Effects, Motion, Side, Transform};
    let Some(dir) = workdir("looks") else { return };
    let mut t = Timeline::default();
    let mut pic = Clip::image(halves(&dir));
    (pic.width, pic.height) = (Some(320), Some(160));
    t.append(TrackRef::V1, pic).unwrap();
    let c = ClipRef { track: TrackRef::V1, index: 0 };
    let at = Duration::from_millis(2_000);
    // Letterboxed 160x80 in the 160x120 frame: red left, blue right.
    assert!(is_red(pixel_at(&t, at, 20, 60)) && is_blue(pixel_at(&t, at, 140, 60)));

    t.clip_mut(c).unwrap().transform = Transform::default().flipped_h();
    assert!(is_blue(pixel_at(&t, at, 20, 60)) && is_red(pixel_at(&t, at, 140, 60)), "mirrored");
    t.clip_mut(c).unwrap().transform = Transform::default().flipped_v();
    assert!(is_red(pixel_at(&t, at, 20, 60)), "top-bottom mirroring keeps the sides");
    // A quarter turn clockwise: the left (red) half goes on top, and it's
    // pillarboxed 60x120 in the middle.
    t.clip_mut(c).unwrap().transform = Transform::default().rotated_cw();
    assert!(is_red(pixel_at(&t, at, 80, 10)) && is_blue(pixel_at(&t, at, 80, 110)), "turned");
    assert!(is_black(pixel_at(&t, at, 10, 60)), "bars beside it");
    t.clip_mut(c).unwrap().transform = Transform::default().rotated_ccw();
    assert!(is_blue(pixel_at(&t, at, 80, 10)) && is_red(pixel_at(&t, at, 80, 110)), "turned back");
    t.clip_mut(c).unwrap().transform = Transform::default();

    // Cropping off the left 60% leaves mostly blue.
    t.clip_mut(c).unwrap().crop = Crop { left: 0.45, ..Crop::default() };
    assert!(is_blue(pixel_at(&t, at, 120, 60)), "{:?}", pixel_at(&t, at, 120, 60));
    t.clip_mut(c).unwrap().crop = Crop::default();

    // Half see-through over black: half as bright.
    t.clip_mut(c).unwrap().opacity = 0.5;
    let p = pixel_at(&t, at, 20, 60);
    assert!((100..160).contains(&p[0]) && p[1] < 40, "{p:?}");
    t.clip_mut(c).unwrap().opacity = 1.0;

    // Sliding in from the left: off the frame at first, in place after.
    let k = t.clip_mut(c).unwrap();
    k.effects = Effects { enter: Some(Anim::Slide(Side::Left)), exit: Some(Anim::Zoom), ..Effects::default() };
    assert!(is_black(pixel_at(&t, Duration::ZERO, 140, 60)), "{:?}", pixel_at(&t, Duration::ZERO, 140, 60));
    assert!(is_blue(pixel_at(&t, Duration::from_millis(1_000), 140, 60)));
    // Shrinking away at the end: nearly gone in the last frame.
    assert!(is_black(pixel_at(&t, Duration::from_millis(3_950), 20, 60)));
    // Zooming in (Ken Burns): by the end the red reaches further right.
    t.clip_mut(c).unwrap().effects = Effects { motion: Some(Motion::ZoomIn), ..Effects::default() };
    assert!(is_blue(pixel_at(&t, Duration::ZERO, 88, 60)));
    assert!(is_red(pixel_at(&t, Duration::from_millis(3_900), 76, 60)));
    assert!(is_red(pixel_at(&t, Duration::from_millis(3_900), 20, 25)), "the red grows past the bar");
}

#[test]
fn text_speed_and_effects_render_in_an_export() {
    use flicker_core::timeline::{Anim, Effects, Motion, Side, TextPlace, TextStyle, Transform};
    let Some(dir) = workdir("fx") else { return };
    let (a, b, img, music) = sources(&dir);
    let mut t = timeline(&a, &b, &img, &music);
    let v1 = |i| ClipRef { track: TrackRef::V1, index: i };
    // a at double speed: 2s. The picture turned and spinning, b bouncing.
    assert!(t.set_speed(v1(0), 2.0));
    let pic = t.clip_mut(v1(1)).unwrap();
    pic.transform = Transform::default().rotated_cw().flipped_h();
    pic.effects = Effects { motion: Some(Motion::Spin), enter: Some(Anim::Slide(Side::Top)), ..Effects::default() };
    let bars = t.clip_mut(v1(2)).unwrap();
    bars.effects = Effects { motion: Some(Motion::Bounce), exit: Some(Anim::Spin), ..Effects::default() };
    bars.opacity = 0.7;
    bars.crop = flicker_core::timeline::Crop { left: 0.2, top: 0.1, ..Default::default() };
    // A title over it all, with every character the graph treats specially.
    let v2 = t.add_track(TrackKind::Video);
    let style = TextStyle {
        text: "Flicker's \"test\": 100% [ok], a;b \\ c\nline two".into(),
        place: TextPlace::Top,
        boxed: true,
        ..TextStyle::default()
    };
    let mut title = Clip::text(style);
    title.effects = Effects { motion: Some(Motion::Pulse), enter: Some(Anim::Zoom), ..Effects::default() };
    t.add_clip(v2, title, Duration::from_millis(500)).unwrap();
    // a takes 2s now, leaving a 2s gap before the picture (4-6) and b (6-9).
    assert_eq!(t.duration(), s(9));

    let out = dir.join("fx.mp4");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    req.preset = "ultrafast".into();
    export(&req);
    let info = FfmpegCli::locate().probe(&out).unwrap();
    assert_close(info.duration.unwrap(), s(9));
    // Every frame decodes cleanly.
    let check = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&out).args(["-f", "null", "-"]).output().unwrap();
    assert!(check.status.success() && check.stderr.is_empty(), "{}", String::from_utf8_lossy(&check.stderr));
    // a's tone is in the first two seconds (played twice as fast).
    let p = pcm(&out);
    assert!(rms(&p, 8000.0, 0.1, 0.9) > 500.0, "a's sound at double speed");

    // The text is drawn: bright pixels near the top while it's on.
    let mut one = FfmpegCli::locate().open_timeline_frames(&preview(&t, s(2), true)).unwrap();
    let f = one.next_frame().unwrap().unwrap();
    let top = &f.rgb[..(f.width * 30 * 3) as usize];
    assert!(top.chunks(3).filter(|p| p.iter().all(|&c| c > 220)).count() > 20, "no text drawn");
}

#[test]
fn a_video_on_an_audio_track_is_only_its_sound() {
    let Some(dir) = workdir("vsnd") else { return };
    let (a, _b, img, _music) = sources(&dir);
    let mut t = Timeline::default();
    let mut pic = Clip::image(img.clone());
    pic.out_point = s(4);
    t.append(TrackRef::V1, pic).unwrap();
    // a.mp4 (test pattern + 440 Hz tone) dropped on A1.
    t.append(TrackRef::A1, clip(&a)).unwrap();
    // In a video export the picture stays the red still: a's picture isn't used.
    let out = dir.join("v.mp4");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    req.preset = "ultrafast".into();
    export(&req);
    let p = pcm(&out);
    assert!(rms(&p, 8000.0, 0.5, 3.5) > 500.0, "a's sound");
    let mut one = FfmpegCli::locate().open_timeline_frames(&preview(&t, s(2), true)).unwrap();
    let f = one.next_frame().unwrap().unwrap();
    let mid = ((60 * 160 + 80) * 3) as usize;
    assert!(f.rgb[mid] > 200 && f.rgb[mid + 1] < 60, "red: {:?}", &f.rgb[mid..mid + 3]);

    // Exported as sound only: one audio stream, no picture, and much
    // smaller than the video.
    for name in ["a.m4a", "a.mp3", "a.opus", "a.flac", "a.wav"] {
        let snd = dir.join(name);
        let mut t2 = t.clone();
        t2.video[0].clips.clear();
        let req = ExportRequest::new(t2, snd.clone());
        assert!(req.format.is_audio());
        export(&req);
        let info = FfmpegCli::locate().probe(&snd).unwrap();
        assert_eq!(info.count(StreamKind::Video), 0, "{name}");
        assert_eq!(info.count(StreamKind::Audio), 1, "{name}");
        assert_close(info.duration.unwrap(), s(4));
        assert!(rms(&pcm(&snd), 8000.0, 0.5, 3.5) > 500.0, "{name}");
        assert_no_tags(&snd);
        let bytes = std::fs::read(&snd).unwrap();
        assert!(!bytes.windows(4).any(|w| w == b"Lavf" || w == b"Lavc"), "{name} names its encoder");
    }
    let m4a = std::fs::metadata(dir.join("a.m4a")).unwrap().len();
    let mp4 = std::fs::metadata(&out).unwrap().len();
    assert!(m4a < mp4, "{m4a} vs {mp4}");
}
