//! How files are handed to ffmpeg and ffprobe: as files on this computer,
//! and nothing else.
//!
//! ffmpeg takes its inputs as URLs. A name like `http://host/x.mp4`,
//! `concat:a|b` or `tcp://host:1` would make it connect out or read
//! something else than the file meant, and one starting with `-` would be
//! read as an option. So every input is passed as `file:<path>` (always the
//! file of that name, whatever the name looks like), and every input is
//! opened with `-protocol_whitelist file`, so nothing a file refers to
//! (a playlist's entries, say) is fetched from anywhere but the disk either.
//!
//! Paths that come out of a project file are checked as well, when the
//! project is opened (see [`crate::project`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The protocols ffmpeg may read an input with. Frames, sound and progress
/// written to stdout are outputs, which this doesn't touch.
pub const PROTOCOLS: &str = "file";

/// `path` as ffmpeg must be given it: never taken for a URL of another
/// protocol or for an option.
pub fn url(path: &Path) -> OsString {
    let mut s = OsString::from("file:");
    s.push(path);
    s
}

/// The arguments that open `path` as an input (they follow any `-ss`, `-t`
/// and other options of that input): `-protocol_whitelist file -i file:<path>`.
pub fn args(path: &Path) -> [OsString; 4] {
    ["-protocol_whitelist".into(), PROTOCOLS.into(), "-i".into(), url(path)]
}

/// The same for ffprobe, which takes its input without `-i`.
pub fn probe_args(path: &Path) -> [OsString; 3] {
    ["-protocol_whitelist".into(), PROTOCOLS.into(), url(path)]
}

/// True for a full path on one of this computer's drives. A relative name,
/// a URL or (on Windows) a path on another computer isn't.
pub fn is_local_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        // `\\server\share\x.mp4` is absolute too, and just looking at it
        // would connect to that server.
        use std::path::{Component, Prefix};
        let disk = match path.components().next() {
            Some(Component::Prefix(p)) => matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
            _ => false,
        };
        if !disk {
            return false;
        }
    }
    path.is_absolute()
}

/// True for a full path to an ordinary file that's there (not a folder, a
/// device or a pipe).
pub fn is_local_file(path: &Path) -> bool {
    is_local_path(path) && path.is_file()
}

/// What a path read from a project becomes. `dir` is the project's folder.
///
/// - A full path to a file that's there is kept.
/// - Otherwise a file of that name in the project's folder is used (files
///   moved along with the project are found this way).
/// - A full path with nothing there is kept as it is: the clip stays on the
///   timeline, shown as missing, and works again when the file is back.
/// - Anything else (a URL, a relative name, something that looks like an
///   option, a device or a pipe) is never used as written: the clip stays,
///   as a missing file of that name in the project's folder.
pub fn from_project(path: &Path, dir: Option<&Path>) -> PathBuf {
    if is_local_file(path) {
        return path.to_path_buf();
    }
    let dir = dir.map(Path::to_path_buf).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let near = dir.join(path.file_name().unwrap_or("missing".as_ref()));
    if is_local_file(&near) {
        return near;
    }
    if is_local_path(path) && std::fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    near
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_files_whatever_their_name() {
        for name in ["http://127.0.0.1:9/x.mp4", "concat:a.mp4|b.mp4", "-map", "pipe:0", "a.mp4", "/v/a.mp4"] {
            let a = args(Path::new(name));
            assert_eq!(a[..3], ["-protocol_whitelist", "file", "-i"].map(OsString::from));
            assert_eq!(a[3], OsString::from(format!("file:{name}")));
            let p = probe_args(Path::new(name));
            assert_eq!(p[..2], ["-protocol_whitelist", "file"].map(OsString::from));
            assert_eq!(p[2], OsString::from(format!("file:{name}")));
        }
    }

    #[test]
    fn only_full_paths_to_real_files_come_out_of_a_project() {
        let dir = std::env::temp_dir().join(format!("flicker-input-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let here = dir.join("here.mp4");
        std::fs::write(&here, b"x").unwrap();
        std::fs::write(dir.join("x.mp4"), b"x").unwrap();

        // A file that's there, and one that moved next to the project.
        assert_eq!(from_project(&here, Some(&dir)), here);
        assert_eq!(from_project(Path::new("/gone/away/here.mp4"), Some(&dir)), here);
        // A missing file keeps its place.
        assert_eq!(from_project(Path::new("/gone/away/clip.mp4"), Some(&dir)), Path::new("/gone/away/clip.mp4"));
        // A URL whose name matches a file next to the project is that file.
        assert_eq!(from_project(Path::new("http://127.0.0.1:9/x.mp4"), Some(&dir)), dir.join("x.mp4"));
        // Nothing else is used as written.
        for bad in [
            "http://127.0.0.1:9/clip.mp4",
            "tcp://127.0.0.1:9",
            "concat:/etc/hostname|/etc/hostname",
            "subfile,,start,0,end,10,,:/etc/hostname",
            "pipe:0",
            "-f",
            "-protocol_whitelist",
            "clip.mp4",
            "../clip.mp4",
            "",
        ] {
            let p = from_project(Path::new(bad), Some(&dir));
            assert!(is_local_path(&p) && p.starts_with(&dir) && !p.is_file(), "{bad:?} became {p:?}");
        }
        // Folders, devices and pipes aren't media.
        let p = from_project(&dir.join("sub"), Some(&dir));
        assert!(!p.is_file());
        #[cfg(unix)]
        assert_eq!(from_project(Path::new("/dev/zero"), Some(&dir)), dir.join("zero"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
