use std::path::Path;
use std::time::Duration;

use crate::edit::TrimRequest;
use crate::job::{CancelToken, Progress};
use crate::media::MediaInfo;
use crate::preview::{FrameSource, PreviewRequest};
use crate::subtitles::EmbedRequest;
use crate::Result;

/// A media backend. Implementations must be usable from a worker thread.
pub trait Engine: Send + Sync {
    /// Human readable name, shown in the UI.
    fn name(&self) -> String;

    /// Read container/stream information from a media file.
    fn probe(&self, path: &Path) -> Result<MediaInfo>;

    /// Timestamps of the video keyframes, sorted. Fast (copy) cuts can only
    /// start on these.
    fn keyframes(&self, path: &Path) -> Result<Vec<Duration>>;

    /// Put a subtitle file into a video, reporting progress as it goes.
    fn embed_subtitles(
        &self,
        req: &EmbedRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()>;

    /// Cut a video down to the request's kept ranges.
    fn trim(
        &self,
        req: &TrimRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()>;

    /// Start decoding frames for on-screen preview.
    fn open_preview(&self, req: &PreviewRequest) -> Result<Box<dyn FrameSource>>;
}
