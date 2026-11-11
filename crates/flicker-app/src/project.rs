//! The open project: its timeline, where it's saved, and the edit history.
//! Every edit is a cheap change to the timeline; undo and redo swap whole
//! timeline snapshots (a few kilobytes even for long projects).

use std::path::{Path, PathBuf};

use flicker_core::Timeline;
use zeroize::Zeroizing;

/// How many edits can be undone.
const HISTORY: usize = 200;

pub struct Project {
    timeline: Timeline,
    /// The `.flicker` file, once saved or opened.
    pub path: Option<PathBuf>,
    /// The timeline as last saved (or opened).
    saved: Timeline,
    undo: Vec<(Timeline, String)>,
    redo: Vec<(Timeline, String)>,
    /// A drag or slider in progress: the timeline before it, and its label.
    pending: Option<(Timeline, String)>,
    /// Bumped on every change, so the player knows to catch up.
    version: u64,
    /// The password the file is encrypted with, once chosen or entered.
    /// Wiped when the project is closed (New, Open, quitting).
    pub password: Option<Zeroizing<String>>,
    /// Opened from a plain file written by an earlier Flicker: it counts as
    /// unsaved until it's saved encrypted.
    pub was_plain: bool,
}

impl Default for Project {
    fn default() -> Self {
        Self::new(Timeline::default(), None)
    }
}

impl Project {
    pub fn new(timeline: Timeline, path: Option<PathBuf>) -> Self {
        Self {
            saved: timeline.clone(),
            timeline,
            path,
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            version: 0,
            password: None,
            was_plain: false,
        }
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// One edit, undoable as `label`. Nothing is recorded if it changed nothing.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Timeline) -> R) -> R {
        self.finish();
        let before = self.timeline.clone();
        let r = f(&mut self.timeline);
        if self.timeline != before {
            self.record(before, label);
        }
        r
    }

    /// Change the timeline as part of a gesture (dragging a clip edge,
    /// moving a slider). All changes until [`Project::finish`] (or another
    /// gesture with a different label) undo as one step.
    pub fn live<R>(&mut self, label: &str, f: impl FnOnce(&mut Timeline) -> R) -> R {
        if self.pending.as_ref().is_some_and(|(_, l)| l != label) {
            self.finish();
        }
        if self.pending.is_none() {
            self.pending = Some((self.timeline.clone(), label.to_owned()));
        }
        let before_version = self.timeline.clone();
        let r = f(&mut self.timeline);
        if self.timeline != before_version {
            self.version += 1;
        }
        r
    }

    /// Like [`Project::live`], but `f` always starts from the timeline as
    /// it was when the gesture began. A drag computes where things go from
    /// where they were when it was picked up, never from the last frame's
    /// result, so rounding and clamping can't build up and the outcome
    /// depends only on where the pointer is now.
    pub fn gesture<R>(&mut self, label: &str, f: impl FnOnce(&mut Timeline) -> R) -> R {
        if self.pending.as_ref().is_some_and(|(_, l)| l != label) {
            self.finish();
        }
        let start = match &self.pending {
            Some((t, _)) => t.clone(),
            None => {
                self.pending = Some((self.timeline.clone(), label.to_owned()));
                self.timeline.clone()
            }
        };
        let mut next = start;
        let r = f(&mut next);
        if next != self.timeline {
            self.timeline = next;
            self.version += 1;
        }
        r
    }

    /// End the gesture in progress, recording it if it changed anything.
    pub fn finish(&mut self) {
        if let Some((before, label)) = self.pending.take() {
            if before != self.timeline {
                // `live` already bumped the version.
                self.version -= 1;
                self.record(before, &label);
            }
        }
    }

    pub fn in_gesture(&self) -> bool {
        self.pending.is_some()
    }

    fn record(&mut self, before: Timeline, label: &str) {
        self.undo.push((before, label.to_owned()));
        if self.undo.len() > HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.version += 1;
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(_, l)| l.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(_, l)| l.as_str())
    }

    pub fn undo(&mut self) -> bool {
        self.finish();
        let Some((t, label)) = self.undo.pop() else { return false };
        let now = std::mem::replace(&mut self.timeline, t);
        self.redo.push((now, label));
        self.version += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        self.finish();
        let Some((t, label)) = self.redo.pop() else { return false };
        let now = std::mem::replace(&mut self.timeline, t);
        self.undo.push((now, label));
        self.version += 1;
        true
    }

    /// The timeline changed since it was last saved.
    pub fn has_changes(&self) -> bool {
        self.timeline != self.saved
    }

    /// Unsaved: changed, or still a plain file on disk.
    pub fn is_dirty(&self) -> bool {
        self.was_plain || self.has_changes()
    }

    pub fn mark_saved(&mut self, path: &Path) {
        self.finish();
        self.saved = self.timeline.clone();
        self.path = Some(path.to_path_buf());
        self.was_plain = false;
    }

    /// "holiday.flicker", or "Untitled".
    pub fn name(&self) -> String {
        match &self.path {
            Some(p) => p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned(),
            None => "Untitled".into(),
        }
    }

    /// Where Save As suggests: next to the project, or the first clip.
    pub fn suggested_path(&self, ext: &str) -> PathBuf {
        let base = match &self.path {
            Some(p) => p.clone(),
            None => match self.timeline.files().next() {
                Some(f) => f.to_path_buf(),
                None => PathBuf::from("untitled"),
            },
        };
        let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled".into());
        base.with_file_name(format!("{stem}.{ext}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flicker_core::{Clip, ClipRef, Edge, TrackRef};
    use std::time::Duration;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn project() -> Project {
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("a.mp4".into(), s(10), true));
        t.append(TrackRef::V1, Clip::video("b.mp4".into(), s(5), true));
        Project::new(t, None)
    }

    const A: ClipRef = ClipRef { track: TrackRef::V1, index: 0 };

    #[test]
    fn edits_undo_and_redo() {
        let mut p = project();
        assert!(!p.is_dirty());
        p.edit("Split", |t| t.split(A, s(4)));
        p.edit("Delete", |t| t.ripple_remove(A));
        assert_eq!(p.timeline().video[0].clips.len(), 2);
        assert_eq!(p.timeline().video[0].clips[0].start, Duration::ZERO);
        assert!(p.is_dirty());
        assert_eq!(p.undo_label(), Some("Delete"));
        assert!(p.undo());
        assert_eq!(p.timeline().video[0].clips.len(), 3);
        assert_eq!(p.redo_label(), Some("Delete"));
        assert!(p.undo());
        assert!(!p.is_dirty(), "back where it started");
        assert!(!p.undo());
        assert!(p.redo() && p.redo());
        assert_eq!(p.timeline().video[0].clips.len(), 2);
        // A new edit drops the redo history.
        p.undo();
        p.edit("Mute", |t| t.video[0].muted = true);
        assert_eq!(p.redo_label(), None);
        // Edits that change nothing aren't recorded.
        p.edit("Nothing", |_| ());
        assert_eq!(p.undo_label(), Some("Mute"));
    }

    #[test]
    fn a_drag_is_one_step() {
        let mut p = project();
        let v0 = p.version();
        for x in [9, 8, 7, 6] {
            p.live("Trim", |t| t.trim(A, Edge::End, s(x)));
        }
        assert!(p.version() > v0, "the player sees every step of the drag");
        assert_eq!(p.undo_label(), None, "not recorded until it ends");
        p.finish();
        assert_eq!(p.undo_label(), Some("Trim"));
        p.undo();
        assert_eq!(p.timeline().video[0].clips[0].len(), s(10));
        // A drag that ends where it started records nothing.
        p.live("Trim", |t| t.trim(A, Edge::End, s(10)));
        p.finish();
        assert_eq!(p.undo_label(), None);
        // A different gesture ends the one before.
        p.live("Volume", |t| t.video[0].clips[0].volume = 0.5);
        p.live("Fade in", |t| t.video[0].clips[0].fade_in = s(1));
        p.finish();
        assert_eq!(p.undo_label(), Some("Fade in"));
        p.undo();
        assert_eq!(p.undo_label(), Some("Volume"));
    }

    #[test]
    fn a_gesture_always_starts_from_where_it_began() {
        let mut p = project();
        let v0 = p.version();
        // Each step is computed from the timeline before the drag: going
        // back to the start gives exactly the start.
        p.gesture("Trim", |t| t.trim(A, Edge::End, s(4)));
        assert_eq!(p.timeline().video[0].clips[0].len(), s(4));
        p.gesture("Trim", |t| t.trim(A, Edge::End, s(7)));
        assert_eq!(p.timeline().video[0].clips[0].len(), s(7));
        let v1 = p.version();
        p.gesture("Trim", |t| t.trim(A, Edge::End, s(7)));
        assert_eq!(p.version(), v1, "the same place again changes nothing");
        p.gesture("Trim", |t| t.trim(A, Edge::End, s(10)));
        assert!(p.version() > v0);
        p.finish();
        assert_eq!(p.undo_label(), None, "back where it began: nothing to undo");
        p.gesture("Trim", |t| t.trim(A, Edge::Start, s(1)));
        p.finish();
        assert_eq!(p.undo_label(), Some("Trim"));
        p.undo();
        assert_eq!(p.timeline().video[0].clips[0].start, Duration::ZERO);
    }

    #[test]
    fn saving_clears_dirty() {
        let mut p = project();
        p.edit("Mute", |t| t.audio[0].muted = true);
        assert!(p.is_dirty());
        p.mark_saved(Path::new("/x/holiday.flicker"));
        assert!(!p.is_dirty());
        assert_eq!(p.name(), "holiday.flicker");
        assert_eq!(p.suggested_path("mp4"), PathBuf::from("/x/holiday.mp4"));
        assert_eq!(Project::default().name(), "Untitled");
        // An old plain file is unsaved until it's saved encrypted.
        p.was_plain = true;
        assert!(p.is_dirty());
        p.mark_saved(Path::new("/x/holiday.flicker"));
        assert!(!p.is_dirty());
    }
}
