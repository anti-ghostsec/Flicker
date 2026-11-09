//! End-to-end tests of rendering clips one after another (Edit > Append)
//! against the real ffmpeg. Skipped if ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use flicker_core::{
    CancelToken, Canvas, Clip, Engine, ExportFormat, ExportRequest, FfmpegCli, SaveRequest, StreamKind,
    Timeline, Transition, TransitionKind,
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

/// 4s 320x240 25fps clip with sound, and a 3s 640x360 clip without.
fn sources(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (a, b, img) = (dir.join("a.mp4"), dir.join("b.mkv"), dir.join("still.png"));
    ffmpeg(
        &["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=4", "-f", "lavfi", "-i",
          "sine=frequency=440:duration=4", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"],
        &a,
    );
    ffmpeg(&["-f", "lavfi", "-i", "smptebars=size=640x360:rate=30:duration=3", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &b);
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red:size=200x300", "-frames:v", "1"], &img);
    (a, b, img)
}

fn timeline(a: &Path, b: &Path, img: &Path) -> Timeline {
    let engine = FfmpegCli::locate();
    let mut clips = Vec::new();
    for p in [a, b] {
        let info = engine.probe(p).unwrap();
        let v = info.first_video().unwrap();
        let mut c = Clip::video(p.to_path_buf(), info.duration.unwrap(), info.count(StreamKind::Audio) > 0);
        c.width = v.width;
        c.height = v.height;
        c.fps = v.frame_rate;
        clips.push(c);
    }
    let mut pic = Clip::image(img.to_path_buf());
    pic.set_range(Duration::ZERO, Duration::from_secs(2));
    clips.insert(1, pic);
    Timeline { clips }
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

fn save(input: &Path, output: &Path, keep_tags: bool) {
    let mut req = SaveRequest::new(input.to_path_buf(), output.to_path_buf());
    req.keep_tags = keep_tags;
    FfmpegCli::locate().save(&req, &mut |_| {}, &CancelToken::new()).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn mp4_with_cut_split_fades_and_crossfade_saves_privately() {
    let Some(dir) = workdir("mp4") else { return };
    let (a, b, img) = sources(&dir);
    let mut t = timeline(&a, &b, &img);
    // 4 + 2 + 3 = 9s. Split the first clip and drop its second half (2s).
    assert_eq!(t.split_at(Duration::from_secs(2)), Some(1));
    t.remove(1);
    t.clips[0].volume = 0.5;
    t.clips[0].fade_in = Duration::from_millis(500);
    // Picture → bars: 1s cross-fade; 2 + 2 + 3 - 1 = 6s.
    t.clips[2].transition = Transition { kind: TransitionKind::Crossfade, duration: Duration::from_secs(1) };
    assert_eq!(t.duration(), Duration::from_secs(6));

    let out = dir.join("work.mp4");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    req.preset = "ultrafast".into();
    // The first clip's size and frame rate; the bars are letterboxed into it.
    assert_eq!(req.canvas, Canvas { width: 320, height: 240, fps: 25.0 });
    let progress = export(&req);
    assert_eq!(progress.last().copied(), Some(1.0));

    let info = FfmpegCli::locate().probe(&out).unwrap();
    assert_close(info.duration.unwrap(), Duration::from_secs(6));
    let v = info.first_video().unwrap();
    assert_eq!((v.codec.as_str(), v.width, v.height), ("h264", Some(320), Some(240)));
    let audio = info.streams.iter().find(|s| s.kind == StreamKind::Audio).unwrap();
    assert_eq!(audio.codec, "aac");

    // Saved: no encoder or date tags, no x264 version string in the stream.
    let saved = dir.join("saved.mp4");
    save(&out, &saved, false);
    assert_no_tags(&saved);
    let bytes = std::fs::read(&saved).unwrap();
    assert!(!bytes.windows(4).any(|w| w == b"x264"), "x264 SEI left in the file");
    assert!(!bytes.windows(4).any(|w| w == b"Lavf"), "muxer name left in the file");
    assert_close(FfmpegCli::locate().probe(&saved).unwrap().duration.unwrap(), Duration::from_secs(6));
}

#[test]
fn mkv_with_dip_to_black_saves_as_webm_and_with_tags() {
    let Some(dir) = workdir("webm") else { return };
    let (a, b, img) = sources(&dir);
    let mut t = timeline(&a, &b, &img);
    t.clips[1].transition = Transition { kind: TransitionKind::DipToBlack, duration: Duration::from_millis(500) };
    t.clips[2].muted = true;
    let out = dir.join("work.mkv");
    let mut req = ExportRequest::new(t.clone(), out.clone());
    assert_eq!(req.format, ExportFormat::Mp4, "H.264/AAC in the MKV");
    req.canvas = Canvas { width: 160, height: 120, fps: 15.0 };
    req.preset = "ultrafast".into();
    export(&req);
    let info = FfmpegCli::locate().probe(&out).unwrap();
    assert_close(info.duration.unwrap(), t.duration());
    assert!(info.format.contains("matroska"), "{}", info.format);

    let webm = dir.join("out.webm");
    save(&out, &webm, false);
    let info = FfmpegCli::locate().probe(&webm).unwrap();
    assert_close(info.duration.unwrap(), t.duration());
    assert_eq!(info.first_video().unwrap().codec, "vp9");
    assert!(info.streams.iter().any(|s| s.kind == StreamKind::Audio && s.codec == "opus"));
    assert_no_tags(&webm);
    assert!(!dir.join(".out.flicker-part.webm").exists(), "staging file left behind");

    // Asking for tags puts the encoder name and a date in.
    let tagged = dir.join("tags.mp4");
    save(&out, &tagged, true);
    let t = tags(&tagged);
    assert!(t.contains("encoder") && t.contains("creation_time"), "{t}");
}

#[test]
fn cancel_removes_partial_file() {
    let Some(dir) = workdir("cancel") else { return };
    let (a, b, img) = sources(&dir);
    let out = dir.join("out.mp4");
    let req = ExportRequest::new(timeline(&a, &b, &img), out.clone());
    let cancel = CancelToken::new();
    cancel.cancel();
    let r = FfmpegCli::locate().export_timeline(&req, &mut |_| {}, &cancel);
    assert!(matches!(r, Err(flicker_core::Error::Cancelled)), "{r:?}");
    assert!(!out.exists());
}
