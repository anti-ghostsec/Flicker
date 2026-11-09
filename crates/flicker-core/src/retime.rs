//! Shift and clip subtitle cues to match a trimmed video.
//!
//! FFmpeg's own subtitle handling when seeking/cutting is unreliable (cues
//! from before the cut leak in, cue lengths stretch), so trims extract text
//! subtitles from the untouched source and retime them here.

use std::time::Duration;

use crate::edit::TimeRange;

/// Map a source cue `[start, end)` onto the trimmed output. Returns one span
/// per kept part it overlaps (a cue spanning a removed section is split).
pub fn map_cue(start: Duration, end: Duration, parts: &[TimeRange]) -> Vec<(Duration, Duration)> {
    let mut out = Vec::new();
    let mut offset = Duration::ZERO;
    for p in parts {
        let (s, e) = (start.max(p.start), end.min(p.end));
        if s < e {
            out.push((offset + (s - p.start), offset + (e - p.start)));
        }
        offset += p.len();
    }
    out
}

/// Retime an SRT file. Cues outside the kept parts are dropped; the rest are
/// renumbered.
pub fn retime_srt(srt: &str, parts: &[TimeRange]) -> String {
    let srt = srt.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out = String::new();
    let mut n = 0;
    for block in srt.split("\n\n") {
        let mut lines = block.lines().skip_while(|l| l.trim().is_empty());
        // The index line is optional in the wild; find the timing line.
        let mut first = lines.next();
        if first.is_some_and(|l| !l.contains("-->")) {
            first = lines.next();
        }
        let Some(timing) = first else { continue };
        let Some((start, end, rest)) = parse_srt_timing(timing) else { continue };
        let text: Vec<&str> = lines.collect();
        for (s, e) in map_cue(start, end, parts) {
            n += 1;
            out += &format!("{n}\n{} --> {}{rest}\n{}\n\n", srt_time(s), srt_time(e), text.join("\n"));
        }
    }
    out
}

/// Retime the `Dialogue:` lines of an ASS/SSA file; everything else (styles,
/// script info) is kept verbatim.
pub fn retime_ass(ass: &str, parts: &[TimeRange]) -> String {
    let mut out = String::with_capacity(ass.len());
    for line in ass.lines() {
        let Some(body) = line.strip_prefix("Dialogue:") else {
            out += line;
            out.push('\n');
            continue;
        };
        // Layer, Start, End, then the rest (which may contain commas).
        let mut f = body.splitn(4, ',');
        let (Some(layer), Some(start), Some(end), Some(rest)) = (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let (Some(start), Some(end)) = (parse_ass_time(start), parse_ass_time(end)) else { continue };
        for (s, e) in map_cue(start, end, parts) {
            out += &format!("Dialogue:{layer},{},{},{rest}\n", ass_time(s), ass_time(e));
        }
    }
    out
}

/// "00:01:02,500 --> 00:01:04,000 X1:..." → (start, end, " X1:...").
fn parse_srt_timing(line: &str) -> Option<(Duration, Duration, &str)> {
    let (a, b) = line.split_once("-->")?;
    let b = b.trim_start();
    let end_len = b.find(char::is_whitespace).unwrap_or(b.len());
    Some((parse_srt_time(a.trim())?, parse_srt_time(&b[..end_len])?, &b[end_len..]))
}

fn parse_srt_time(s: &str) -> Option<Duration> {
    let (hms, ms) = s.split_once([',', '.'])?;
    let mut parts = hms.split(':').map(|p| p.trim().parse::<u64>().ok());
    let (h, m, sec) = (parts.next()??, parts.next()??, parts.next()??);
    let ms: u64 = ms.trim().get(..3.min(ms.len()))?.parse().ok()?;
    Some(Duration::from_millis(((h * 60 + m) * 60 + sec) * 1000 + ms))
}

fn srt_time(d: Duration) -> String {
    let ms = d.as_millis();
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// ASS times look like "0:01:02.50" (centiseconds).
fn parse_ass_time(s: &str) -> Option<Duration> {
    let (hms, cs) = s.trim().split_once('.')?;
    let mut parts = hms.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (parts.next()??, parts.next()??, parts.next()??);
    let cs: u64 = cs.parse().ok()?;
    Some(Duration::from_millis(((h * 60 + m) * 60 + sec) * 1000 + cs * 10))
}

fn ass_time(d: Duration) -> String {
    let cs = d.as_millis() / 10;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
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
    fn cues_are_shifted_clipped_and_split() {
        let parts = [r(10.0, 20.0), r(30.0, 40.0)];
        assert_eq!(map_cue(s(12.0), s(13.0), &parts), vec![(s(2.0), s(3.0))]);
        // Straddles the start: clipped to begin at 0.
        assert_eq!(map_cue(s(8.0), s(11.0), &parts), vec![(s(0.0), s(1.0))]);
        // Inside the removed gap: gone.
        assert!(map_cue(s(22.0), s(25.0), &parts).is_empty());
        // Spans the removed gap: one piece each side.
        assert_eq!(
            map_cue(s(19.0), s(31.0), &parts),
            vec![(s(9.0), s(10.0)), (s(10.0), s(11.0))]
        );
    }

    #[test]
    fn srt_is_retimed_and_renumbered() {
        let srt = "\u{feff}1\r\n00:00:05,000 --> 00:00:06,000\r\nGone\r\n\r\n\
                   2\r\n00:00:15,500 --> 00:00:16,250 X1:10\r\nKept\r\nTwo lines\r\n\r\n\
                   3\r\n00:00:21,000 --> 00:00:25,000\r\nClipped\r\n";
        assert_eq!(
            retime_srt(srt, &[r(10.0, 22.0)]),
            "1\n00:00:05,500 --> 00:00:06,250 X1:10\nKept\nTwo lines\n\n\
             2\n00:00:11,000 --> 00:00:12,000\nClipped\n\n"
        );
    }

    #[test]
    fn ass_dialogue_is_retimed_and_rest_kept() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Text\n\
                   Dialogue: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,Gone\n\
                   Dialogue: 0,0:01:15.50,0:01:16.00,Default,,0,0,0,,Hi, there\n";
        assert_eq!(
            retime_ass(ass, &[r(70.0, 80.0)]),
            "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Text\n\
             Dialogue: 0,0:00:05.50,0:00:06.00,Default,,0,0,0,,Hi, there\n"
        );
    }
}
