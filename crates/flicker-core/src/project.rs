//! Project files (`.flicker`): the timeline, encrypted with a password the
//! user chooses. Media is referenced by path, never copied in.
//!
//! The file is a short header and the encrypted project:
//!
//! ```text
//! "FLKPRJ" (6 bytes) · version (1 byte, 1) · salt (16) · nonce (24) · ciphertext + tag
//! ```
//!
//! The key comes from the password with Argon2id (64 MiB, 3 passes, one
//! lane); the contents (the timeline as JSON) are sealed with
//! XChaCha20-Poly1305, with the header as associated data, so changing any
//! byte of the file is noticed. Salt and nonce are new for every save.
//! Nothing else is in clear: no clip names, paths or times.
//!
//! The JSON only ever exists in memory (and is wiped after use); the
//! `.flicker-part` file written before the rename holds only ciphertext.
//! Plain JSON projects from earlier versions still open.

use std::io::Write;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Block, Params, Version};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::timeline::Timeline;
use crate::{Error, Result};

pub const EXTENSION: &str = "flicker";
const VERSION: u32 = 1;

const MAGIC: &[u8; 6] = b"FLKPRJ";
const FILE_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;
const TAG_LEN: usize = 16;

/// Argon2id cost: 64 MiB, 3 passes, 1 lane.
const KDF_MEMORY_KIB: u32 = 64 * 1024;
const KDF_PASSES: u32 = 3;
const KDF_LANES: u32 = 1;

/// Shorter passwords get a warning (but are allowed).
pub const SHORT_PASSWORD: usize = 8;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    flicker_project: u32,
    timeline: Timeline,
}

/// The project as JSON (in memory only; see [`save`]).
pub fn to_json(timeline: &Timeline) -> Zeroizing<Vec<u8>> {
    let file = ProjectFile { flicker_project: VERSION, timeline: timeline.clone() };
    // Measure first, so the buffer never grows (growing would leave copies
    // of the JSON behind in freed memory). Room for the tag too, which
    // encryption appends in place.
    let mut count = Counter(0);
    let _ = serde_json::to_writer(&mut count, &file);
    let mut out = Zeroizing::new(Vec::with_capacity(count.0 + TAG_LEN));
    let _ = serde_json::to_writer(&mut *out, &file);
    out
}

struct Counter(usize);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Read a project from its JSON. Files that moved along with the project
/// (into its folder) are found there. A project can come from someone else,
/// so the paths in it aren't trusted: only full paths to files on this
/// computer are kept, and anything else becomes a missing file (see
/// [`crate::input::from_project`]). Clips are never dropped.
pub fn from_json(json: &[u8], project_dir: Option<&Path>) -> Result<Timeline> {
    let file: ProjectFile = serde_json::from_slice(json).map_err(|e| Error::Project(e.to_string()))?;
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
    let relocate = |p: &mut PathBuf| *p = crate::input::from_project(p, project_dir);
    for track in t.video.iter_mut().chain(t.audio.iter_mut()) {
        track.clips.sort_by_key(|c| c.start);
        for c in track.clips.iter_mut().filter(|c| c.has_file()) {
            relocate(&mut c.path);
        }
    }
    if let Some(s) = &mut t.subtitle {
        relocate(s);
    }
    Ok(t)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Project(format!("no random numbers from the system: {e}")))?;
    Ok(b)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(KDF_MEMORY_KIB, KDF_PASSES, KDF_LANES, Some(32)).map_err(|e| Error::Project(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    // Our own memory blocks, so they can be wiped afterwards.
    let mut blocks = vec![Block::default(); argon.params().block_count()];
    let r = argon.hash_password_into_with_memory(password.as_bytes(), salt, key.as_mut(), &mut blocks);
    blocks.zeroize();
    r.map_err(|e| Error::Project(e.to_string()))?;
    Ok(key)
}

/// Encrypt `plain` (the project JSON) with `password`, in place: on return
/// it holds the ciphertext. Returns the header to write before it.
fn seal(plain: &mut Vec<u8>, password: &str) -> Result<[u8; HEADER_LEN]> {
    let (salt, nonce) = (random::<SALT_LEN>()?, random::<NONCE_LEN>()?);
    let mut header = [0u8; HEADER_LEN];
    header[..6].copy_from_slice(MAGIC);
    header[6] = FILE_VERSION;
    header[7..7 + SALT_LEN].copy_from_slice(&salt);
    header[7 + SALT_LEN..].copy_from_slice(&nonce);
    let key = derive_key(password, &salt)?;
    XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
        .encrypt_in_place(XNonce::from_slice(&nonce), &header, plain)
        .map_err(|_| Error::Project("encryption failed".into()))?;
    Ok(header)
}

/// The encrypted file for `json`.
pub fn encrypt(json: &[u8], password: &str) -> Result<Vec<u8>> {
    let mut buf = Zeroizing::new(Vec::with_capacity(json.len() + TAG_LEN));
    buf.extend_from_slice(json);
    let header = seal(&mut buf, password)?;
    let mut out = Vec::with_capacity(HEADER_LEN + buf.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&buf);
    Ok(out)
}

/// True when `data` is an encrypted project (by its header).
pub fn is_encrypted(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// The JSON inside an encrypted project. A wrong password and a damaged or
/// changed file look the same: [`Error::WrongPassword`].
pub fn decrypt(data: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>> {
    if !is_encrypted(data) || data.len() < HEADER_LEN + TAG_LEN {
        return Err(Error::Project("it isn't a Flicker project".into()));
    }
    if data[6] != FILE_VERSION {
        return Err(Error::Project("it was made by a newer Flicker".into()));
    }
    let (header, body) = data.split_at(HEADER_LEN);
    let salt = &header[7..7 + SALT_LEN];
    let nonce = &header[7 + SALT_LEN..];
    let key = derive_key(password, salt)?;
    let mut buf = Zeroizing::new(body.to_vec());
    XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
        .decrypt_in_place(XNonce::from_slice(nonce), header, &mut *buf)
        .map_err(|_| Error::WrongPassword)?;
    Ok(buf)
}

/// Save the project encrypted with `password`.
pub fn save(path: &Path, timeline: &Timeline, password: &str) -> Result<()> {
    let mut data = to_json(timeline);
    let header = seal(&mut data, password)?;
    // Write next to it first, so a failed save can't eat the old project.
    // Only ciphertext ever reaches the disk.
    let tmp = path.with_extension("flicker-part");
    let write = || -> std::io::Result<()> {
        let mut f = create_private(&tmp)?;
        f.write_all(&header)?;
        f.write_all(&data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// A new file only the user can read.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// An opened project.
pub struct Opened {
    pub timeline: Timeline,
    /// It was a plain (unencrypted) file from an earlier Flicker.
    pub was_plain: bool,
}

/// Open a project. An encrypted one needs its `password`
/// ([`Error::PasswordNeeded`] without, [`Error::WrongPassword`] when it's
/// wrong); old plain ones open without.
pub fn load(path: &Path, password: Option<&str>) -> Result<Opened> {
    let data = Zeroizing::new(std::fs::read(path)?);
    if !is_encrypted(&data) {
        let timeline = from_json(&data, path.parent())?;
        return Ok(Opened { timeline, was_plain: true });
    }
    let password = password.ok_or(Error::PasswordNeeded)?;
    let json = decrypt(&data, password)?;
    Ok(Opened { timeline: from_json(&json, path.parent())?, was_plain: false })
}

pub fn is_project(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Clip, TrackRef};
    use std::time::Duration;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flicker-proj-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn timeline(media: &Path) -> Timeline {
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video("/gone/away/secret-holiday.mp4".into(), Duration::from_secs(3), true)).unwrap();
        t.append(TrackRef::A1, Clip::audio(media.to_path_buf(), Duration::from_millis(1500))).unwrap();
        t
    }

    #[test]
    fn save_load_and_find_moved_files() {
        let dir = dir("roundtrip");
        let media = dir.join("secret-holiday.mp4");
        std::fs::write(&media, b"x").unwrap();
        let t = timeline(&media);
        let file = dir.join("p.flicker");
        save(&file, &t, "correct horse").unwrap();
        assert!(is_project(&file) && !dir.join("p.flicker-part").exists());
        let back = load(&file, Some("correct horse")).unwrap();
        assert!(!back.was_plain);
        let back = back.timeline;
        // The missing file is found next to the project.
        assert_eq!(back.video[0].clips[0].path, media);
        assert_eq!(back.audio[0].clips[0], t.audio[0].clips[0]);
        assert_eq!(back.duration(), Duration::from_secs(3));
        assert!(matches!(load(&file, None), Err(Error::PasswordNeeded)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrong_password_fails_cleanly() {
        let dir = dir("wrong");
        let file = dir.join("p.flicker");
        save(&file, &timeline(&dir.join("m.mp3")), "right one").unwrap();
        assert!(matches!(load(&file, Some("wrong one")), Err(Error::WrongPassword)));
        assert!(matches!(load(&file, Some("")), Err(Error::WrongPassword)));
        assert!(load(&file, Some("right one")).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tampering_is_detected() {
        let json = to_json(&timeline(Path::new("/m/music.mp3")));
        let good = encrypt(&json, "pw").unwrap();
        assert_eq!(&*decrypt(&good, "pw").unwrap(), &*json);
        // Any byte, in the header (salt, nonce) or the contents.
        for i in [7, HEADER_LEN - 1, HEADER_LEN, good.len() / 2, good.len() - 1] {
            let mut bad = good.clone();
            bad[i] ^= 1;
            assert!(matches!(decrypt(&bad, "pw"), Err(Error::WrongPassword)), "byte {i}");
        }
        // Cut short, or a newer version.
        assert!(decrypt(&good[..good.len() - 1], "pw").is_err());
        assert!(decrypt(&good[..HEADER_LEN], "pw").is_err());
        let mut newer = good.clone();
        newer[6] = 2;
        assert!(matches!(decrypt(&newer, "pw"), Err(Error::Project(_))));
    }

    #[test]
    fn nothing_readable_on_disk() {
        let dir = dir("opaque");
        let file = dir.join("p.flicker");
        let t = timeline(&dir.join("music.mp3"));
        save(&file, &t, "pw").unwrap();
        let data = std::fs::read(&file).unwrap();
        assert!(data.starts_with(b"FLKPRJ\x01"));
        for needle in [&b"secret-holiday"[..], b"music", b"flicker_project", b"timeline", b"\"video\"", b".mp"] {
            assert!(
                !data.windows(needle.len()).any(|w| w == needle),
                "{:?} in the file",
                String::from_utf8_lossy(needle)
            );
        }
        // Header, then exactly the JSON's length plus the tag.
        assert_eq!(data.len(), HEADER_LEN + to_json(&t).len() + TAG_LEN);
        // A fresh salt and nonce every save.
        save(&file, &t, "pw").unwrap();
        let again = std::fs::read(&file).unwrap();
        assert_ne!(data[7..HEADER_LEN], again[7..HEADER_LEN]);
        assert_ne!(data[HEADER_LEN..], again[HEADER_LEN..]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A project naming things that aren't files on this computer, as the
    /// old plain format and encrypted.
    #[test]
    fn paths_in_a_project_are_not_trusted() {
        let dir = dir("hostile");
        let real = dir.join("real.mp4");
        std::fs::write(&real, b"x").unwrap();
        let bad = [
            "http://127.0.0.1:9/x.mp4",
            "tcp://127.0.0.1:9",
            "concat:/etc/hostname|/etc/hostname",
            "pipe:0",
            "-safe",
            "relative/clip.mp4",
            "subfile,,start,0,end,9,,:/etc/hostname",
        ];
        let mut t = Timeline::default();
        t.append(TrackRef::V1, Clip::video(real.clone(), Duration::from_secs(1), true)).unwrap();
        for b in bad {
            t.append(TrackRef::V1, Clip::video(b.into(), Duration::from_secs(1), true)).unwrap();
            t.append(TrackRef::A1, Clip::audio(b.into(), Duration::from_secs(1))).unwrap();
        }
        t.subtitle = Some("http://127.0.0.1:9/s.srt".into());

        let plain = dir.join("plain.flicker");
        std::fs::write(&plain, &*to_json(&t)).unwrap();
        let sealed = dir.join("sealed.flicker");
        save(&sealed, &t, "pw").unwrap();
        for (file, pw) in [(&plain, None), (&sealed, Some("pw"))] {
            let back = load(file, pw).unwrap().timeline;
            // Nothing is dropped: every clip is still there, in its place.
            assert_eq!(back.video[0].clips.len(), 1 + bad.len());
            assert_eq!(back.audio[0].clips.len(), bad.len());
            assert_eq!(back.duration(), t.duration());
            assert_eq!(back.video[0].clips[0].path, real);
            for f in back.files().filter(|f| *f != real) {
                // Each is now a missing file in the project's folder.
                assert!(f.is_absolute() && f.starts_with(&dir) && !f.exists(), "{f:?}");
            }
            assert_eq!(back.subtitle, Some(dir.join("s.srt")));
            assert_eq!(back.video[0].clips[1].name(), "x.mp4");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_plain_projects_still_open() {
        let dir = dir("plain");
        let file = dir.join("old.flicker");
        let old = "{\"flicker_project\":1,\"timeline\":{\"video\":[],\"audio\":[]}}";
        std::fs::write(&file, old).unwrap();
        let opened = load(&file, None).unwrap();
        assert!(opened.was_plain);
        assert_eq!(opened.timeline, Timeline::default());
        // A password given anyway doesn't matter.
        assert!(load(&file, Some("pw")).unwrap().was_plain);
        // The previous build's pretty-printed files too.
        let t = timeline(&dir.join("m.mp3"));
        let pretty = serde_json::to_string_pretty(&ProjectFile { flicker_project: 1, timeline: t.clone() }).unwrap();
        std::fs::write(&file, pretty).unwrap();
        assert_eq!(load(&file, None).unwrap().timeline, t);

        assert!(from_json(b"{\"flicker_project\":99,\"timeline\":{\"video\":[],\"audio\":[]}}", None).is_err());
        assert!(from_json(b"not json", None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
