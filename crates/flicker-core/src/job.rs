use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Shared flag a UI can flip to stop a running job.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    /// How far into the output we are.
    pub position: Duration,
    /// Total length, if known.
    pub total: Option<Duration>,
    /// Encoding speed relative to realtime (e.g. 2.5 = 2.5x).
    pub speed: Option<f32>,
}

impl Progress {
    /// 0.0..=1.0, or `None` when the total length is unknown.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?.as_secs_f64();
        if total <= 0.0 {
            return None;
        }
        Some((self.position.as_secs_f64() / total).clamp(0.0, 1.0) as f32)
    }
}
