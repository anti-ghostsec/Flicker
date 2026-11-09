//! Building blocks shared across the app: file pickers and a background job
//! runner with progress and cancel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use flicker_core::{CancelToken, Progress};
use eframe::egui;

pub const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "m4v", "webm", "avi", "ts", "flv", "wmv"];
/// Containers Save As offers. Not WebM: saving stream-copies, and edits
/// produce H.264/AAC, which WebM can't hold.
pub const OUTPUT_EXTS: &[&str] = &["mp4", "mkv", "mov"];

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn dropped_files(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect())
}

enum JobMsg {
    Progress(Progress),
    Done(Result<(), String>),
}

struct RunningJob<T> {
    rx: Receiver<JobMsg>,
    cancel: CancelToken,
    progress: Option<Progress>,
    label: String,
    /// Handed back when the job finishes, so the caller knows what it was for.
    tag: T,
}

/// Runs one long job at a time on a worker thread.
pub struct JobRunner<T> {
    job: Option<RunningJob<T>>,
}

impl<T> Default for JobRunner<T> {
    fn default() -> Self {
        Self { job: None }
    }
}

impl<T> JobRunner<T> {
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Start `work` on a background thread. It gets a progress callback and a
    /// cancel token. `label` is shown while it runs.
    pub fn start<F>(&mut self, ctx: &egui::Context, label: impl Into<String>, tag: T, work: F)
    where
        F: FnOnce(&mut dyn FnMut(Progress), &CancelToken) -> flicker_core::Result<()>
            + Send
            + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let (token, ctx) = (cancel.clone(), ctx.clone());
        thread::spawn(move || {
            let mut on_progress = |p| {
                let _ = tx.send(JobMsg::Progress(p));
                ctx.request_repaint();
            };
            let result = work(&mut on_progress, &token).map_err(|e| e.to_string());
            let _ = tx.send(JobMsg::Done(result));
            ctx.request_repaint();
        });
        self.job = Some(RunningJob { rx, cancel, progress: None, label: label.into(), tag });
    }

    pub fn cancel(&self) {
        if let Some(job) = &self.job {
            job.cancel.cancel();
        }
    }

    /// Collect messages from the worker. Call once per frame. Returns the
    /// job's tag and result when it finishes.
    pub fn poll(&mut self) -> Option<(T, Result<(), String>)> {
        let job = self.job.as_mut()?;
        let mut finished = None;
        while let Ok(msg) = job.rx.try_recv() {
            match msg {
                JobMsg::Progress(p) => job.progress = Some(p),
                JobMsg::Done(r) => finished = Some(r),
            }
        }
        let result = finished?;
        self.job.take().map(|j| (j.tag, result))
    }

    /// Progress bar + Cancel while running. Returns true if a job is running.
    pub fn progress_ui(&self, ui: &mut egui::Ui) -> bool {
        let Some(job) = &self.job else { return false };
        let fraction = job.progress.and_then(|p| p.fraction());
        let mut text = match fraction {
            Some(f) => format!("{}  {:.0}%", job.label, f * 100.0),
            None => format!("{}…", job.label),
        };
        if let Some(speed) = job.progress.and_then(|p| p.speed) {
            text += &format!("  ·  {speed:.1}x");
        }
        if ui.button("Cancel").clicked() {
            job.cancel.cancel();
        }
        ui.add(egui::ProgressBar::new(fraction.unwrap_or(0.0)).text(text).desired_width(260.0));
        true
    }
}

#[cfg(not(target_os = "android"))]
pub fn pick_file(kind: &str, exts: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter(kind, exts).add_filter("All files", &["*"]).pick_file()
}

/// "Save as" dialog starting at `suggested`.
#[cfg(not(target_os = "android"))]
pub fn save_file(suggested: &Path) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().add_filter("Video", OUTPUT_EXTS);
    if let Some(dir) = suggested.parent().filter(|d| d.is_dir()) {
        dlg = dlg.set_directory(dir);
    }
    if let Some(name) = suggested.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy());
    }
    dlg.save_file()
}

// TODO(android): use the Storage Access Framework picker via JNI.
#[cfg(target_os = "android")]
pub fn pick_file(_kind: &str, _exts: &[&str]) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "android")]
pub fn save_file(_suggested: &Path) -> Option<PathBuf> {
    None
}
