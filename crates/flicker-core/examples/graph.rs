//! Print the ffmpeg arguments Flicker uses for a project, one per line:
//! `cargo run -p flicker-core --example graph -- project.flicker [preview|sound|export] [from] [to]`
use std::time::Duration;

use flicker_core::render::{preview_audio_args, preview_video_args, export_args, PreviewOpts};
use flicker_core::{project, Canvas, ExportRequest};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: graph PROJECT [preview|sound|export] [FROM] [TO]");
        std::process::exit(2);
    };
    let t = project::load(path.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let secs = |i: usize, d: f64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let opts = PreviewOpts {
        canvas: t.suggested_canvas().fit_within(960, 540, 30.0),
        from: Duration::from_secs_f64(secs(2, 0.0)),
        to: Duration::from_secs_f64(secs(3, t.duration().as_secs_f64())),
        subtitles: true,
        single_frame: false,
    };
    let _ = Canvas::default();
    let a = match args.get(1).map(String::as_str).unwrap_or("preview") {
        "sound" => preview_audio_args(&t, &opts),
        "export" => export_args(&ExportRequest::new(t.clone(), "out.mp4".into())),
        _ => preview_video_args(&t, &opts),
    };
    for x in a {
        println!("{}", x.to_string_lossy());
    }
}
