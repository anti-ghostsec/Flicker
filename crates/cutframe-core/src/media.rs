use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub index: usize,
    pub kind: StreamKind,
    pub codec: String,
    pub language: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Average frames per second (video only).
    pub frame_rate: Option<f64>,
    /// Where this stream ends (start time + length), when the container says.
    pub end: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub format: String,
    pub duration: Option<Duration>,
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    pub fn count(&self, kind: StreamKind) -> usize {
        self.streams.iter().filter(|s| s.kind == kind).count()
    }

    pub fn first_video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind == StreamKind::Video)
    }

    /// How long there's a picture: the video track's end, when it's known and
    /// earlier than the file's (audio often runs a little longer), else the
    /// file's length.
    pub fn video_length(&self) -> Option<Duration> {
        let video = self.first_video().and_then(|v| v.end).filter(|v| !v.is_zero());
        match (video, self.duration) {
            (Some(v), Some(d)) => Some(v.min(d)),
            (v, d) => v.or(d),
        }
    }

    /// One-line description for the UI, e.g. "1920x1080 h264 · 2 audio · 0:42".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.first_video() {
            match (v.width, v.height) {
                (Some(w), Some(h)) => parts.push(format!("{w}x{h} {}", v.codec)),
                _ => parts.push(v.codec.clone()),
            }
        }
        let audio = self.count(StreamKind::Audio);
        if audio > 0 {
            parts.push(format!("{audio} audio"));
        }
        let subs = self.count(StreamKind::Subtitle);
        if subs > 0 {
            parts.push(format!("{subs} subtitle"));
        }
        if let Some(d) = self.duration {
            parts.push(format_duration(d));
        }
        parts.join(" · ")
    }
}

/// Precise timestamp for editing, e.g. "1:02.500" or "1:00:02.500".
pub fn format_timestamp(d: Duration) -> String {
    let ms = d.as_millis();
    let (h, m, s, ms) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// Parse "90", "1:30", "1:30.25" or "0:01:30.250".
pub fn parse_timestamp(s: &str) -> Option<Duration> {
    let mut secs = 0.0;
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        let v: f64 = part.trim().parse().ok()?;
        let is_last = i == parts.len() - 1;
        if v < 0.0 || !v.is_finite() || (!is_last && v.fract() != 0.0) || (i > 0 && v >= 60.0) {
            return None;
        }
        secs = secs * 60.0 + v;
    }
    Some(Duration::from_secs_f64(secs))
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        let d = Duration::from_millis(3_725_250);
        assert_eq!(format_timestamp(d), "1:02:05.250");
        assert_eq!(parse_timestamp("1:02:05.250"), Some(d));
        assert_eq!(parse_timestamp("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_timestamp("1:30.5"), Some(Duration::from_millis(90_500)));
        assert_eq!(parse_timestamp("1:75"), None);
        assert_eq!(parse_timestamp("abc"), None);
    }
}
