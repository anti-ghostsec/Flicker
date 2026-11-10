//! Sound edits on the whole video: volume, mute and fades. The picture and
//! subtitle tracks are copied untouched; only the sound is re-encoded (AAC).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::{Error, Result};

/// Loudest volume offered, as a factor (2.0 = 200%).
pub const MAX_VOLUME: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioEdit {
    /// Multiply the volume: 1.0 unchanged, 0.5 half, 2.0 double.
    Volume(f32),
    /// Remove the sound.
    Mute,
    /// Fade the sound in from silence at the start and out at the end.
    Fade { fade_in: Duration, fade_out: Duration },
}

#[derive(Clone, Debug)]
pub struct AudioRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub edit: AudioEdit,
}

impl AudioRequest {
    pub fn new(input: PathBuf, output: PathBuf, edit: AudioEdit) -> Self {
        Self { input, output, edit }
    }

    pub fn validate(&self) -> Result<()> {
        if !self.input.is_file() {
            return Err(Error::MissingFile(self.input.clone()));
        }
        if crate::subtitles::same_file(&self.input, &self.output) {
            return Err(Error::OutputIsInput);
        }
        Ok(())
    }
}

fn secs(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64())
}

/// The `-af` filter for an edit, `None` for [`AudioEdit::Mute`]. `len` is
/// how long the sound plays (fade-outs end there).
pub fn filter(edit: AudioEdit, len: Duration) -> Option<String> {
    match edit {
        AudioEdit::Volume(v) => Some(format!("volume={:.3}", v.clamp(0.0, MAX_VOLUME))),
        AudioEdit::Mute => None,
        AudioEdit::Fade { fade_in, fade_out } => {
            let mut f = Vec::new();
            if !fade_in.is_zero() {
                f.push(format!("afade=t=in:st=0:d={}", secs(fade_in.min(len))));
            }
            if !fade_out.is_zero() {
                let d = fade_out.min(len);
                f.push(format!("afade=t=out:st={}:d={}", secs(len - d), secs(d)));
            }
            // Nothing asked: pass the sound through unchanged.
            Some(if f.is_empty() { "anull".into() } else { f.join(",") })
        }
    }
}

/// ffmpeg arguments for an audio edit. `len` is the sound's length.
pub fn audio_args(req: &AudioRequest, len: Duration) -> Vec<OsString> {
    let mut a = crate::edit::base_args();
    a.extend(crate::input::args(&req.input));
    let filter = filter(req.edit, len);
    // Mute leaves the sound tracks out.
    let sound: &[&str] = if filter.is_some() { &["-map", "0:a?"] } else { &[] };
    a.extend(["-map", "0:v"].map(OsString::from));
    a.extend(sound.iter().map(OsString::from));
    a.extend(["-map", "0:s?", "-c", "copy"].map(OsString::from));
    if let Some(f) = filter {
        a.extend(["-c:a", "aac", "-b:a", "192k", "-af"].map(OsString::from));
        a.push(f.into());
    }
    a.extend(crate::privacy::encoder_flags());
    if crate::privacy::is_mp4_family(&req.output) {
        a.extend(["-movflags", "+faststart"].map(OsString::from));
    }
    a.push(req.output.clone().into());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[OsString]) -> String {
        a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn filters() {
        let len = Duration::from_secs(10);
        assert_eq!(filter(AudioEdit::Volume(1.5), len).unwrap(), "volume=1.500");
        assert_eq!(filter(AudioEdit::Volume(9.0), len).unwrap(), "volume=2.000");
        assert_eq!(filter(AudioEdit::Mute, len), None);
        let fade = AudioEdit::Fade { fade_in: Duration::from_secs(1), fade_out: Duration::from_secs(2) };
        assert_eq!(filter(fade, len).unwrap(), "afade=t=in:st=0:d=1.000,afade=t=out:st=8.000:d=2.000");
        let out_only = AudioEdit::Fade { fade_in: Duration::ZERO, fade_out: Duration::from_secs(30) };
        assert_eq!(filter(out_only, len).unwrap(), "afade=t=out:st=0.000:d=10.000");
    }

    #[test]
    fn args_copy_the_picture() {
        let req = AudioRequest::new("in.mkv".into(), "out.mp4".into(), AudioEdit::Volume(0.5));
        let a = s(&audio_args(&req, Duration::from_secs(4)));
        assert!(a.contains("-map 0:v -map 0:a? -map 0:s? -c copy -c:a aac -b:a 192k -af volume=0.500"), "{a}");
        assert!(a.ends_with("-movflags +faststart out.mp4"), "{a}");
        let req = AudioRequest::new("in.mkv".into(), "out.mkv".into(), AudioEdit::Mute);
        let a = s(&audio_args(&req, Duration::from_secs(4)));
        assert!(a.contains("-map 0:v -map 0:s? -c copy -flags"), "{a}");
        assert!(!a.contains("0:a"), "{a}");
    }
}
