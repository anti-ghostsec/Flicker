//! Project files (`.flicker`): the timeline as a small JSON file. Media is
//! referenced by path, never copied in.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::timeline::Timeline;
use crate::{Error, Result};

pub const EXTENSION: &str = "flicker";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    flicker_project: u32,
    timeline: Timeline,
}

pub fn to_json(timeline: &Timeline) -> String {
    let file = ProjectFile { flicker_project: VERSION, timeline: timeline.clone() };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

/// Read a project. Files that moved along with the project (into its
/// folder) are found there.
pub fn from_json(json: &str, project_dir: Option<&Path>) -> Result<Timeline> {
    let file: ProjectFile = serde_json::from_str(json).map_err(|e| Error::Project(e.to_string()))?;
    if file.flicker_project > VERSION {
        return Err(Error::Project("it was made by a newer Flicker".into()));
    }
    let mut t = file.timeline;
    if t.video.is_empty() {
        t.video.push(crate::Track::new(crate::TrackKind::Video));
    }
    if t.audio.is_empty() {
        t.audio.push(crate::Track::new(crate::TrackKind::Audio));
    }
    let relocate = |p: &mut PathBuf| {
        if let (false, Some(dir), Some(name)) = (p.is_file(), project_dir, p.file_name()) {
            let near = dir.join(name);
            if near.is_file() {
                *p = near;
            }
        }
    };
    for track in t.video.iter_mut().chain(t.audio.iter_mut()) {
        track.clips.sort_by_key(|c| c.start);
        for c in &mut track.clips {
            relocate(&mut c.path);
        }
    }
    if let Some(s) = &mut t.subtitle {
        relocate(s);
    }
    Ok(t)
}

pub fn save(path: &Path, timeline: &Timeline) -> Result<()> {
    // Write next to it first, so a failed save can't eat the old project.
    let tmp = path.with_extension("flicker-part");
    std::fs::write(&tmp, to_json(timeline))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Timeline> {
    let json = std::fs::read_to_string(path)?;
    from_json(&json, path.parent())
}

pub fn is_project(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, TrackRef};
    use std::time::Duration;

    #[test]
    fn save_load_and_find_moved_files() {
        let dir = std::env::temp_dir().join(format!("flicker-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("clip.mp4");
        std::fs::write(&media, b"x").unwrap();
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("/gone/away/clip.mp4".into(), Duration::from_secs(3), true)).unwrap();
        t.append(TrackRef::A1, Clip::audio(media.clone(), Duration::from_millis(1500))).unwrap();
        let file = dir.join("p.flicker");
        save(&file, &t).unwrap();
        assert!(is_project(&file) && !dir.join("p.flicker-part").exists());
        let back = load(&file).unwrap();
        // The missing file is found next to the project.
        assert_eq!(back.video[0].clips[0].path, media);
        assert_eq!(back.audio[0].clips[0], t.audio[0].clips[0]);
        assert_eq!(back.duration(), Duration::from_secs(3));
        assert!(from_json("{\"flicker_project\":99,\"timeline\":{\"video\":[],\"audio\":[]}}", None).is_err());
        assert!(from_json("not json", None).is_err());
        let empty = from_json("{\"flicker_project\":1,\"timeline\":{\"video\":[],\"audio\":[]}}", None).unwrap();
        assert_eq!(empty, Timeline::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
