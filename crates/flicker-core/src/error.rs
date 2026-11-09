use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not find `{0}`. Install FFmpeg or set its path in Settings")]
    ToolNotFound(String),

    #[error("file not found: {0}")]
    MissingFile(PathBuf),

    #[error("unsupported subtitle file: {0} (use .srt, .ass, .ssa or .vtt)")]
    UnsupportedSubtitle(PathBuf),

    #[error("nothing would be left: choose at least one part of the video to keep")]
    NothingToKeep,

    #[error("output must be different from the input video")]
    OutputIsInput,

    #[error("ffprobe output could not be parsed: {0}")]
    Probe(String),

    #[error("ffmpeg failed (exit code {code:?}):\n{log}")]
    Ffmpeg { code: Option<i32>, log: String },

    #[error("cancelled")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
