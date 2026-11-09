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
