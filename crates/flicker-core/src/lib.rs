//! Flicker core: media processing that knows nothing about the UI.
//!
//! Everything here talks to an [`Engine`]. Today the only engine shells out to
//! the `ffmpeg`/`ffprobe` executables; later an in-process engine (e.g. linked
//! libav* or Android MediaCodec) can implement the same trait without the GUI
//! or the future editor timeline having to change.

pub mod audio;
pub mod edit;
pub mod engine;
pub mod error;
pub mod ffmpeg;
pub mod input;
pub mod job;
pub mod media;
pub mod preview;
pub mod privacy;
pub mod project;
pub mod render;
pub mod retime;
pub mod subtitles;
pub mod timeline;

pub use audio::{AudioEdit, AudioRequest};
pub use edit::{Accuracy, Selection, TimeRange, TrimRequest};
pub use engine::Engine;
pub use error::{Error, Result};
pub use ffmpeg::FfmpegCli;
pub use job::{CancelToken, Progress};
pub use media::{MediaInfo, StreamInfo, StreamKind};
pub use preview::{Frame, FrameSource, PcmSource, PreviewRequest, TimelinePreview};
pub use privacy::SaveRequest;
pub use subtitles::{EmbedMode, EmbedRequest, SubtitleFormat};
pub use render::{ExportFormat, ExportRequest, SAMPLE_RATE};
pub use timeline::{
    Canvas, Clip, ClipKind, ClipRef, Edge, SubtitleExport, Timeline, Track, TrackKind, TrackRef, TransitionKind,
};
