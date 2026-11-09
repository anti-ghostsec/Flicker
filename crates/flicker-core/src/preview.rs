//! Decoded frames for on-screen preview.

use std::ffi::OsString;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::Duration;

use crate::subtitles::escape_filter_path;
use crate::Result;

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub video: PathBuf,
    /// Draw these subtitles onto the frames (rendered exactly like burn-in).
    pub subtitle: Option<PathBuf>,
    /// Where to start decoding.
    pub start: Duration,
    /// Frames are scaled down to at most this width.
    pub max_width: u32,
    /// Output frame rate. Frames are spaced exactly `1 / fps` apart.
    pub fps: f64,
    /// Decode a single frame (for seeking while paused).
    pub single_frame: bool,
}

impl PreviewRequest {
    pub fn new(video: PathBuf) -> Self {
        Self {
            video,
            subtitle: None,
            start: Duration::ZERO,
            max_width: 960,
            fps: 30.0,
            single_frame: false,
        }
    }

    /// Clamp a source frame rate to something cheap enough to preview.
    pub fn preview_fps(source: Option<f64>) -> f64 {
        source.unwrap_or(30.0).clamp(1.0, 30.0)
    }
}

/// An RGB frame, 3 bytes per pixel, rows top to bottom.
#[derive(Clone)]
pub struct Frame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}x{} @ {:?})", self.width, self.height, self.pts)
    }
}

/// A stream of decoded frames. Dropping it stops decoding.
pub trait FrameSource: Send {
    /// The next frame, or `None` at the end of the video.
    fn next_frame(&mut self) -> Result<Option<Frame>>;
}

/// ffmpeg arguments that write the preview as a stream of PPM images to stdout.
/// PPM carries its own size in every frame, so we never have to predict the
/// scaled/rotated dimensions.
pub(crate) fn ffmpeg_args(req: &PreviewRequest) -> Vec<OsString> {
    let mut filters = vec![
        format!("fps={}", req.fps),
        // Even width/height keep scalers and pixel formats happy.
        format!("scale=w='min({},iw)':h=-2", req.max_width),
    ];
    if let Some(sub) = &req.subtitle {
        filters.push(format!("subtitles=filename={}", escape_filter_path(sub)));
    }

    let strs = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
    let mut a = strs(&["-hide_banner", "-v", "error", "-nostdin"]);
    if !req.start.is_zero() {
        a.push("-ss".into());
        a.push(format!("{:.3}", req.start.as_secs_f64()).into());
    }
    // Keep the original timestamps so subtitles line up after seeking.
    a.extend(strs(&["-copyts", "-i"]));
    a.push(req.video.clone().into());
    a.extend(strs(&["-an", "-sn", "-dn", "-vf"]));
    a.push(filters.join(",").into());
    if req.single_frame {
        a.extend(strs(&["-frames:v", "1"]));
    }
    a.extend(strs(&["-f", "image2pipe", "-c:v", "ppm", "-pix_fmt", "rgb24", "pipe:1"]));
    a
}

/// Read one binary PPM (P6) image. `Ok(None)` on clean end of stream.
pub(crate) fn read_ppm(r: &mut impl BufRead) -> io::Result<Option<(u32, u32, Vec<u8>)>> {
    if r.fill_buf()?.is_empty() {
        return Ok(None);
    }
    let magic = header_token(r)?;
    if magic != "P6" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad PPM magic {magic:?}")));
    }
    let width: u32 = parse_token(r)?;
    let height: u32 = parse_token(r)?;
    let maxval: u32 = parse_token(r)?;
    if maxval != 255 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "only 8-bit PPM is supported"));
    }
    let mut rgb = vec![0; width as usize * height as usize * 3];
    r.read_exact(&mut rgb)?;
    Ok(Some((width, height, rgb)))
}

fn parse_token<T: std::str::FromStr>(r: &mut impl BufRead) -> io::Result<T> {
    let tok = header_token(r)?;
    tok.parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("bad PPM header {tok:?}")))
}

/// Read a whitespace-separated header token, consuming exactly one trailing
/// whitespace byte (the last one before pixel data is significant).
fn header_token(r: &mut impl BufRead) -> io::Result<String> {
    let mut tok = String::new();
    let mut byte = [0u8];
    loop {
        r.read_exact(&mut byte)?;
        let c = byte[0];
        if c == b'#' && tok.is_empty() {
            let mut skip = Vec::new();
            r.read_until(b'\n', &mut skip)?;
        } else if c.is_ascii_whitespace() {
            if !tok.is_empty() {
                return Ok(tok);
            }
        } else {
            tok.push(c as char);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_consecutive_ppm_frames() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend([1, 2, 3, 4, 5, 6]);
        data.extend(b"P6 1 1 255\n");
        data.extend([10, 32, 30]); // pixel bytes that look like whitespace
        let mut r = io::Cursor::new(data);
        assert_eq!(read_ppm(&mut r).unwrap(), Some((2, 1, vec![1, 2, 3, 4, 5, 6])));
        assert_eq!(read_ppm(&mut r).unwrap(), Some((1, 1, vec![10, 32, 30])));
        assert_eq!(read_ppm(&mut r).unwrap(), None);
    }

    #[test]
    fn args_seek_and_burn_subtitles() {
        let mut req = PreviewRequest::new("in.mp4".into());
        req.start = Duration::from_millis(1500);
        req.subtitle = Some("s.srt".into());
        req.single_frame = true;
        let s: Vec<String> =
            ffmpeg_args(&req).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let s = s.join(" ");
        assert!(s.contains("-ss 1.500 -copyts -i in.mp4"), "{s}");
        assert!(s.contains("subtitles=filename=s.srt"), "{s}");
        assert!(s.contains("-frames:v 1"), "{s}");
    }
}
