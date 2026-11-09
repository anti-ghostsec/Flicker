//! Edit history for the open video. Every applied edit writes a new working
//! file and becomes the current video, so editing continues without
//! re-importing. Undo/redo just move between those files; nothing touches the
//! original until the user saves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct Step {
    pub path: PathBuf,
    /// What produced this step, e.g. "Cut", for the Edit menu.
    pub label: String,
    /// Preview subtitles retimed to match this step, if any were loaded.
    pub subtitle: Option<PathBuf>,
}

pub struct Session {
    original: PathBuf,
    original_subtitle: Option<PathBuf>,
    /// Working directory holding every step's file; removed on drop.
    dir: PathBuf,
    steps: Vec<Step>,
    redo: Vec<Step>,
    /// Number of steps at the last save (0 = untouched original).
    saved: usize,
    next_file: u32,
}

impl Session {
    pub fn new(original: PathBuf) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "flicker-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Self { original, original_subtitle: None, dir, steps: Vec::new(), redo: Vec::new(), saved: 0, next_file: 0 }
    }

    pub fn original(&self) -> &Path {
        &self.original
    }

    /// The video as it is now.
    pub fn current(&self) -> &Path {
        self.steps.last().map_or(&self.original, |s| &s.path)
    }

    /// Preview subtitles matching the current video.
    pub fn subtitle(&self) -> Option<&Path> {
        match self.steps.last() {
            Some(s) => s.subtitle.as_deref(),
            None => self.original_subtitle.as_deref(),
        }
    }

    /// Load preview subtitles for the current video.
    pub fn set_subtitle(&mut self, path: Option<PathBuf>) {
        match self.steps.last_mut() {
            Some(s) => s.subtitle = path,
            None => self.original_subtitle = path,
        }
    }

    /// A fresh path in the working directory for the next step's output.
    pub fn next_path(&mut self, ext: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        self.next_file += 1;
        Ok(self.dir.join(format!("step{:03}.{ext}", self.next_file)))
    }

    /// Extension for working files: the original's, when it can hold what
    /// edits produce (H.264/AAC, subtitle tracks).
    pub fn working_ext(&self) -> String {
        let ext = self.original.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some(e @ ("mp4" | "mkv" | "mov" | "m4v")) => e.to_owned(),
            _ => "mkv".to_owned(),
        }
    }

    /// Make a finished edit the current video.
    pub fn push(&mut self, step: Step) {
        for s in self.redo.drain(..) {
            remove_step_files(&s);
        }
        if self.saved > self.steps.len() {
            // The saved state was in the redo history we just dropped.
            self.saved = usize::MAX;
        }
        self.steps.push(step);
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.steps.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    pub fn undo(&mut self) -> bool {
        self.steps.pop().map(|s| self.redo.push(s)).is_some()
    }

    pub fn redo(&mut self) -> bool {
        self.redo.pop().map(|s| self.steps.push(s)).is_some()
    }

    /// True when the current video differs from what was last saved.
    pub fn is_dirty(&self) -> bool {
        self.steps.len() != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.steps.len();
    }

    /// Suggested "Save as" path: `<name>.edited.<ext>` next to the original.
    pub fn suggested_output(&self) -> PathBuf {
        let stem = self.original.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
        self.original.with_file_name(format!("{stem}.edited.{}", self.working_ext()))
    }
}

fn remove_step_files(step: &Step) {
    let _ = std::fs::remove_file(&step.path);
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(s: &mut Session, label: &str) {
        let path = s.next_path("mp4").unwrap();
        s.push(Step { path, label: label.into(), subtitle: None });
    }

    #[test]
    fn undo_redo_moves_between_steps() {
        let mut s = Session::new("/videos/clip.mp4".into());
        assert_eq!(s.current(), Path::new("/videos/clip.mp4"));
        assert!(!s.is_dirty());

        step(&mut s, "Cut");
        step(&mut s, "Trim");
        assert!(s.current().ends_with("step002.mp4"));
        assert!(s.is_dirty());

        assert!(s.undo());
        assert!(s.current().ends_with("step001.mp4"));
        assert_eq!(s.redo_label(), Some("Trim"));
        assert!(s.redo());
        assert!(s.current().ends_with("step002.mp4"));

        s.undo();
        s.undo();
        assert_eq!(s.current(), Path::new("/videos/clip.mp4"));
        assert!(!s.is_dirty(), "back at the original");
        assert!(!s.undo());
    }

    #[test]
    fn new_edit_after_undo_drops_redo() {
        let mut s = Session::new("/videos/clip.mkv".into());
        step(&mut s, "Cut");
        s.mark_saved();
        s.undo();
        step(&mut s, "Trim");
        assert_eq!(s.redo_label(), None);
        assert!(s.is_dirty(), "the saved state is gone for good");
    }

    #[test]
    fn subtitles_follow_the_current_step() {
        let mut s = Session::new("/videos/clip.mp4".into());
        s.set_subtitle(Some("/videos/clip.srt".into()));
        let path = s.next_path("mp4").unwrap();
        s.push(Step { path, label: "Cut".into(), subtitle: Some("/tmp/x.srt".into()) });
        assert_eq!(s.subtitle(), Some(Path::new("/tmp/x.srt")));
        s.undo();
        assert_eq!(s.subtitle(), Some(Path::new("/videos/clip.srt")));
    }

    #[test]
    fn working_files_use_a_container_that_fits() {
        assert_eq!(Session::new("/v/a.MP4".into()).working_ext(), "mp4");
        assert_eq!(Session::new("/v/a.webm".into()).working_ext(), "mkv");
        assert_eq!(
            Session::new("/v/a.mov".into()).suggested_output(),
            PathBuf::from("/v/a.edited.mov")
        );
    }
}
