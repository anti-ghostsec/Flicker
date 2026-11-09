//! Run the trim engine from the command line (handy for debugging real files):
//! `cargo run -p flicker-core --example trim -- IN OUT START END [precise]`
//! START/END accept the same formats as the app (e.g. 90, 1:30, 1:30.5).

use flicker_core::media::parse_timestamp;
use flicker_core::{Accuracy, CancelToken, Engine, FfmpegCli, TimeRange, TrimRequest};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, start, end, rest @ ..] = args.as_slice() else {
        eprintln!("usage: trim IN OUT START END [precise]");
        std::process::exit(2);
    };
    let time = |s: &str| parse_timestamp(s).unwrap_or_else(|| panic!("bad time {s:?}"));
    let mut req = TrimRequest::new(
        input.into(),
        output.into(),
        vec![TimeRange::new(time(start), time(end))],
    );
    if rest.first().is_some_and(|m| m == "precise") {
        req.accuracy = Accuracy::Precise;
    }
    let mut last = -1;
    FfmpegCli::locate()
        .trim(
            &req,
            &mut |p| {
                let pct = (p.fraction().unwrap_or(0.0) * 100.0) as i32;
                if pct / 10 != last / 10 {
                    eprint!("{pct}% ");
                    last = pct;
                }
            },
            &CancelToken::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("done");
}
