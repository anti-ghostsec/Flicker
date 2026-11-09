//! Engine that drives the `ffmpeg` / `ffprobe` command-line tools.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;

use crate::edit::{self, TrimRequest};
use crate::engine::Engine;
use crate::job::{CancelToken, Progress};
use crate::media::{parse_timestamp, MediaInfo, StreamInfo, StreamKind};
use crate::preview::{self, Frame, FrameSource, PcmSource, PreviewRequest, TimelinePreview};
use crate::audio::{self, AudioRequest};
use crate::privacy::{self, SaveRequest};
use crate::subtitles::{ffmpeg_args, EmbedRequest};
use crate::render::{self, ExportRequest, PreviewOpts};
use crate::timeline::Timeline;
use crate::{Error, Result};

/// How many trailing stderr lines to keep for error messages.
const LOG_TAIL: usize = 30;

#[derive(Clone, Debug)]
pub struct FfmpegCli {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Default for FfmpegCli {
    fn default() -> Self {
        Self::locate()
    }
}

impl FfmpegCli {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self { ffmpeg: ffmpeg.into(), ffprobe: ffprobe.into() }
    }

    /// Prefer binaries shipped next to our executable (how we'll bundle FFmpeg
    /// on Windows), otherwise fall back to whatever is on PATH.
    pub fn locate() -> Self {
        let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let bundled = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let pick = |name: &str| -> PathBuf {
            if let Some(dir) = &bundled {
                let p = dir.join(exe(name));
                if p.is_file() {
                    return p;
                }
            }
            PathBuf::from(exe(name))
        };
        Self::new(pick("ffmpeg"), pick("ffprobe"))
    }

    /// Returns the first line of `ffmpeg -version`, or an error if it can't run.
    pub fn version(&self) -> Result<String> {
        let out = command(&self.ffmpeg)
            .arg("-version")
            .output()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_owned())
    }
}

impl Engine for FfmpegCli {
    fn name(&self) -> String {
        self.version().unwrap_or_else(|_| "FFmpeg (not found)".into())
    }

    fn probe(&self, path: &Path) -> Result<MediaInfo> {
        if !path.is_file() {
            return Err(Error::MissingFile(path.to_path_buf()));
        }
        let out = command(&self.ffprobe)
            .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
            .arg(path)
            .output()
            .map_err(|e| spawn_error(&self.ffprobe, e))?;
        if !out.status.success() {
            return Err(Error::Probe(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
        }
        parse_probe(&out.stdout)
    }

    fn embed_subtitles(
        &self,
        req: &EmbedRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        let info = self.probe(&req.video)?;
        let args = ffmpeg_args(req, info.count(StreamKind::Subtitle));
        self.run_job(&args, info.duration, &req.output, on_progress, cancel)
    }

    fn trim(
        &self,
        req: &TrimRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        let info = self.probe(&req.input)?;
        let has_audio = info.count(StreamKind::Audio) > 0;
        // (index among subtitle streams, codec) of the text tracks we can retime.
        let text_subs: Vec<(usize, String)> = info
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Subtitle)
            .enumerate()
            .filter(|(_, s)| edit::is_text_subtitle(&s.codec))
            .map(|(i, s)| (i, s.codec.clone()))
            .collect();

        let tmp = TempDir::new()?;
        // Video/audio go straight to the output unless subtitles still need adding.
        let av_out = if text_subs.is_empty() {
            req.output.clone()
        } else {
            let ext = req.output.extension().map(|e| e.to_string_lossy()).unwrap_or("mkv".into());
            tmp.0.join(format!("av.{ext}"))
        };

        let parts = match req.accuracy {
            edit::Accuracy::Precise => {
                let args = edit::precise_args(req, has_audio, &av_out);
                self.run_job(&args, Some(req.output_duration()), &av_out, on_progress, cancel)?;
                req.keep.clone()
            }
            edit::Accuracy::Fast => {
                let keyframes = self.keyframes(&req.input)?;
                let parts = edit::snap_to_keyframes(&req.keep, &keyframes);
                self.trim_fast(req, &parts, &keyframes, &av_out, &tmp.0, on_progress, cancel)?;
                parts
            }
        };

        if !text_subs.is_empty() {
            self.add_retimed_subtitles(req, &text_subs, &parts, &av_out, &tmp.0, cancel)?;
        }
        Ok(())
    }

    fn keyframes(&self, path: &Path) -> Result<Vec<Duration>> {
        // Packet flags come straight from the container index: no decoding.
        let out = command(&self.ffprobe)
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .map_err(|e| spawn_error(&self.ffprobe, e))?;
        if !out.status.success() {
            return Err(Error::Probe(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
        }
        let mut keys: Vec<Duration> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (pts, flags) = l.split_once(',')?;
                if !flags.starts_with('K') {
                    return None;
                }
                let t: f64 = pts.parse().ok()?;
                (t.is_finite() && t >= 0.0).then(|| Duration::from_secs_f64(t))
            })
            .collect();
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    fn edit_audio(
        &self,
        req: &AudioRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        let info = self.probe(&req.input)?;
        let len = info.duration.unwrap_or_default();
        self.run_job(&audio::audio_args(req, len), info.duration, &req.output, on_progress, cancel)
    }

    fn export_timeline(
        &self,
        req: &ExportRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        // Render next to the output, then write the file privately from
        // there (a copy pass, like every save: encoders insist on naming
        // themselves in stream tags).
        let staging = privacy::staging_path(&req.output);
        let mut render_req = req.clone();
        render_req.output = staging.clone();
        let args = render::export_args(&render_req);
        let result = (|| {
            self.run_job(&args, Some(req.timeline.duration()), &staging, on_progress, cancel)?;
            let info = self.probe(&staging)?;
            let args = privacy::scrub_args(&staging, &req.output, &info, req.keep_tags);
            self.run_job(&args, None, &req.output, &mut |_| {}, cancel)
        })();
        let _ = std::fs::remove_file(&staging);
        result
    }

    fn save(
        &self,
        req: &SaveRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        let info = self.probe(&req.input)?;
        if !req.changes_container() {
            let args = privacy::scrub_args(&req.input, &req.output, &info, req.keep_tags);
            return self.run_job(&args, None, &req.output, on_progress, cancel);
        }
        // Another container: convert next to the output first, then write
        // the file privately from there.
        let staging = privacy::staging_path(&req.output);
        let result = (|| {
            if req.is_webm() {
                let args = privacy::webm_args(&req.input, &staging, &info, req.keep_tags);
                self.run_job(&args, info.duration, &staging, on_progress, cancel)?;
            } else {
                // Remux without re-encoding; trim knows how to carry
                // subtitle tracks across containers.
                let whole = info.duration.unwrap_or_default();
                let trim = TrimRequest::new(req.input.clone(), staging.clone(), vec![edit::TimeRange::new(Duration::ZERO, whole)]);
                self.trim(&trim, on_progress, cancel)?;
            }
            let info = self.probe(&staging)?;
            let args = privacy::scrub_args(&staging, &req.output, &info, req.keep_tags);
            self.run_job(&args, None, &req.output, &mut |_| {}, cancel)
        })();
        let _ = std::fs::remove_file(&staging);
        result
    }

    fn open_preview(&self, req: &PreviewRequest) -> Result<Box<dyn FrameSource>> {
        if !req.video.is_file() {
            return Err(Error::MissingFile(req.video.clone()));
        }
        let mut child = command(&self.ffmpeg)
            .args(preview::ffmpeg_args(req))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout = BufReader::with_capacity(1 << 20, child.stdout.take().expect("stdout piped"));
        Ok(Box::new(FfmpegFrames {
            child,
            stdout,
            log: Some(thread::spawn(move || tail_lines(stderr, LOG_TAIL))),
            start: req.start,
            frame_time: 1.0 / req.fps,
            decoded: 0,
        }))
    }

    fn open_timeline_frames(&self, req: &TimelinePreview) -> Result<Box<dyn FrameSource>> {
        let total = req.timeline.duration();
        let (canvas, subtitles, single_frame) = (req.canvas, req.subtitles, req.single_frame);
        let opts = move |from, to| PreviewOpts { canvas, from, to, subtitles, single_frame };
        if req.single_frame {
            // Past the end shows the last frame.
            let frame = Duration::from_secs_f64(1.0 / req.canvas.normalized().fps);
            let from = req.start.min(total.saturating_sub(frame));
            return Ok(Box::new(self.spawn_frames(&req.timeline, &opts(from, from + frame * 2))?));
        }
        let (engine, timeline) = (self.clone(), req.timeline.clone());
        let mk = move |from, to| engine.spawn_frames(&timeline, &opts(from, to));
        Ok(Box::new(Chain::new(preview::stretches(req.start, total), Box::new(mk))))
    }

    fn open_timeline_sound(&self, req: &TimelinePreview) -> Result<Box<dyn PcmSource>> {
        let total = req.timeline.duration();
        let canvas = req.canvas;
        let (engine, timeline) = (self.clone(), req.timeline.clone());
        let mk = move |from, to| {
            let args = render::preview_audio_args(&timeline, &PreviewOpts { canvas, from, to, subtitles: false, single_frame: false });
            let (child, stdout, log) = engine.spawn_pipe(&args)?;
            Ok(FfmpegPcm { bytes: 0, child, stdout, log: Some(log) })
        };
        Ok(Box::new(Chain::new(preview::stretches(req.start, total), Box::new(mk))))
    }
}

impl FfmpegCli {
    /// Start ffmpeg with its output on a pipe and its log collected on a thread.
    fn spawn_pipe(&self, args: &[std::ffi::OsString]) -> Result<(Child, ChildStdout, JoinHandle<String>)> {
        let mut child = command(&self.ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout = child.stdout.take().expect("stdout piped");
        Ok((child, stdout, thread::spawn(move || tail_lines(stderr, LOG_TAIL))))
    }

    fn spawn_frames(&self, timeline: &Timeline, opts: &PreviewOpts) -> Result<FfmpegFrames> {
        let args = render::preview_video_args(timeline, opts);
        let (child, stdout, log) = self.spawn_pipe(&args)?;
        Ok(FfmpegFrames {
            child,
            stdout: BufReader::with_capacity(1 << 20, stdout),
            log: Some(log),
            start: opts.from,
            frame_time: 1.0 / opts.canvas.normalized().fps,
            decoded: 0,
        })
    }
}

type Spawn<S> = Box<dyn Fn(Duration, Duration) -> Result<S> + Send>;

/// Start the next stretch's ffmpeg this long before the current one ends,
/// so it's ready in time without both sitting in memory for long.
const LEAD: Duration = Duration::from_millis(1500);

/// Stretches of the timeline played one after another.
struct Chain<S> {
    stretches: VecDeque<(Duration, Duration)>,
    spawn: Spawn<S>,
    cur: Option<S>,
    /// The current stretch.
    span: (Duration, Duration),
    /// The next stretch, started ahead of time.
    next: Option<((Duration, Duration), Result<S>)>,
}

impl<S> Chain<S> {
    fn new(stretches: Vec<(Duration, Duration)>, spawn: Spawn<S>) -> Self {
        Self { stretches: stretches.into(), spawn, cur: None, span: Default::default(), next: None }
    }

    /// Move on to the next stretch. False at the end.
    fn advance(&mut self) -> Result<bool> {
        self.cur = None;
        let (span, s) = match self.next.take() {
            Some(n) => n,
            None => match self.stretches.pop_front() {
                Some((a, b)) => ((a, b), (self.spawn)(a, b)),
                None => return Ok(false),
            },
        };
        self.span = span;
        self.cur = Some(s?);
        Ok(true)
    }

    /// Playback has got to `pos`: start the next stretch if it's close.
    fn reached(&mut self, pos: Duration) {
        if self.next.is_none() && pos + LEAD >= self.span.1 {
            if let Some((a, b)) = self.stretches.pop_front() {
                self.next = Some(((a, b), (self.spawn)(a, b)));
            }
        }
    }
}

impl FrameSource for Chain<FfmpegFrames> {
    fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if self.cur.is_none() && !self.advance()? {
                return Ok(None);
            }
            match self.cur.as_mut().map(|c| c.next_frame()).transpose()?.flatten() {
                Some(f) => {
                    self.reached(f.pts);
                    return Ok(Some(f));
                }
                None => self.cur = None,
            }
        }
    }
}

impl PcmSource for Chain<FfmpegPcm> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            if self.cur.is_none() && !self.advance()? {
                return Ok(0);
            }
            match self.cur.as_mut().map(|c| c.read(buf)).transpose()?.unwrap_or(0) {
                0 => self.cur = None,
                n => {
                    let read = self.cur.as_ref().map_or(0, |c| c.bytes);
                    let at = self.span.0 + Duration::from_secs_f64(read as f64 / (render::SAMPLE_RATE as f64 * 4.0));
                    self.reached(at);
                    return Ok(n);
                }
            }
        }
    }
}

/// Raw PCM read from an ffmpeg process.
struct FfmpegPcm {
    /// Bytes read so far.
    bytes: u64,
    child: Child,
    stdout: ChildStdout,
    log: Option<JoinHandle<String>>,
}

impl PcmSource for FfmpegPcm {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let want = buf.len() / 4 * 4;
        let mut n = self.stdout.read(&mut buf[..want])?;
        if n == 0 {
            let status = self.child.wait()?;
            let log = self.log.take().and_then(|h| h.join().ok()).unwrap_or_default();
            return if status.success() { Ok(0) } else { Err(Error::Ffmpeg { code: status.code(), log }) };
        }
        // Whole sample frames only.
        while n % 4 != 0 {
            match self.stdout.read(&mut buf[n..want])? {
                0 => {
                    n -= n % 4;
                    break;
                }
                m => n += m,
            }
        }
        self.bytes += n as u64;
        Ok(n)
    }
}

impl Drop for FfmpegPcm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl FfmpegCli {
    /// Fast trim without re-encoding. One sequential pass (no seeking, which
    /// is imprecise in copy mode and differs between containers) splits the
    /// input at keyframes into pieces with clean timestamps; the pieces that
    /// start each part are then joined, each cut to length.
    #[allow(clippy::too_many_arguments)]
    fn trim_fast(
        &self,
        req: &TrimRequest,
        parts: &[edit::TimeRange],
        keyframes: &[Duration],
        output: &Path,
        tmp: &Path,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        // Pieces use the input's container, which can always hold its streams.
        let ext = req.input.extension().map(|e| e.to_string_lossy()).unwrap_or("mkv".into());
        let list = tmp.join("pieces.csv");
        let until = parts.last().map_or(Duration::ZERO, |p| p.end);
        let args = edit::split_args(
            &req.input,
            until,
            &edit::split_times(parts, keyframes),
            &list,
            &tmp.join(format!("piece%04d.{ext}")),
        );
        // Splitting is nearly all the work; joining is quick.
        let mut split_progress = |p: Progress| {
            let f = p.fraction().unwrap_or(0.0) as f64 * 0.95;
            on_progress(Progress { position: until.mul_f64(f), total: Some(until), ..p })
        };
        self.run_job(&args, Some(until), &list, &mut split_progress, cancel)?;

        let pieces = edit::parse_pieces(&std::fs::read_to_string(&list)?, tmp);
        let chosen = edit::pick_pieces(parts, &pieces)
            .ok_or_else(|| Error::Probe("could not split the video at its keyframes".into()))?;
        let script = tmp.join("join.ffconcat");
        std::fs::write(&script, edit::concat_list(&chosen))?;
        let total: Duration = parts.iter().map(edit::TimeRange::len).sum();
        self.run_job(&edit::concat_args(&script, output, total), None, output, &mut |_| {}, cancel)?;
        on_progress(Progress { position: until, total: Some(until), speed: None });
        Ok(())
    }

    /// Extract text subtitles from the untouched source, retime them to the
    /// kept `parts`, and mux them with `av` into the request's output.
    fn add_retimed_subtitles(
        &self,
        req: &TrimRequest,
        subs: &[(usize, String)],
        parts: &[edit::TimeRange],
        av: &Path,
        tmp: &Path,
        cancel: &CancelToken,
    ) -> Result<()> {
        let ass = |codec: &str| matches!(codec, "ass" | "ssa");
        let files: Vec<PathBuf> = subs
            .iter()
            .map(|(i, codec)| tmp.join(format!("sub{i}.{}", if ass(codec) { "ass" } else { "srt" })))
            .collect();

        let mut extract = edit::base_args();
        extract.push("-i".into());
        extract.push(req.input.clone().into());
        for ((i, _), file) in subs.iter().zip(&files) {
            extract.push("-map".into());
            extract.push(format!("0:s:{i}").into());
            extract.push(file.clone().into());
        }
        self.run_job(&extract, None, &files[0], &mut |_| {}, cancel)?;

        // Retime each; tracks with no cues left in the kept parts are dropped.
        let mut kept = Vec::new();
        for ((i, codec), file) in subs.iter().zip(&files) {
            let text = std::fs::read_to_string(file)?;
            let retimed = if ass(codec) {
                crate::retime::retime_ass(&text, parts)
            } else {
                crate::retime::retime_srt(&text, parts)
            };
            let has_cues = if ass(codec) { retimed.contains("\nDialogue:") } else { !retimed.is_empty() };
            if has_cues {
                std::fs::write(file, retimed)?;
                kept.push((*i, codec.clone(), file.clone()));
            }
        }

        if kept.is_empty() {
            std::fs::rename(av, &req.output).or_else(|_| std::fs::copy(av, &req.output).map(|_| ()))?;
            return Ok(());
        }
        let args = edit::mux_subtitles_args(av, &kept, &req.input, &req.output);
        self.run_job(&args, None, &req.output, &mut |_| {}, cancel)
    }

    /// Run an ffmpeg job that writes `-progress pipe:1`, deleting `output` if
    /// it fails or is cancelled.
    fn run_job(
        &self,
        args: &[std::ffi::OsString],
        total: Option<Duration>,
        output: &Path,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        let mut child = command(&self.ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;

        // Drain stderr on its own thread so ffmpeg never blocks on a full pipe.
        let stderr = child.stderr.take().expect("stderr piped");
        let log = thread::spawn(move || tail_lines(stderr, LOG_TAIL));

        let stdout = child.stdout.take().expect("stdout piped");
        let result = read_progress(stdout, total, on_progress, cancel, &mut child);

        let status = child.wait()?;
        let log = log.join().unwrap_or_default();

        if cancel.is_cancelled() {
            let _ = std::fs::remove_file(output);
            return Err(Error::Cancelled);
        }
        result?;
        if !status.success() {
            let _ = std::fs::remove_file(output);
            return Err(Error::Ffmpeg { code: status.code(), log });
        }
        Ok(())
    }
}

/// Frames read from an `ffmpeg ... -f image2pipe -c:v ppm pipe:1` process.
struct FfmpegFrames {
    child: Child,
    stdout: BufReader<ChildStdout>,
    log: Option<JoinHandle<String>>,
    start: Duration,
    frame_time: f64,
    decoded: u64,
}

impl FrameSource for FfmpegFrames {
    fn next_frame(&mut self) -> Result<Option<Frame>> {
        match preview::read_ppm(&mut self.stdout) {
            Ok(Some((width, height, rgb))) => {
                // The fps filter spaces output frames evenly from the seek point.
                let pts = self.start + Duration::from_secs_f64(self.decoded as f64 * self.frame_time);
                self.decoded += 1;
                Ok(Some(Frame { pts, width, height, rgb }))
            }
            Ok(None) => {
                let status = self.child.wait()?;
                let log = self.log.take().and_then(|h| h.join().ok()).unwrap_or_default();
                if status.success() {
                    Ok(None)
                } else {
                    Err(Error::Ffmpeg { code: status.code(), log })
                }
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for FfmpegFrames {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A scratch directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("flicker-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_progress(
    stdout: impl Read,
    total: Option<Duration>,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
    child: &mut Child,
) -> Result<()> {
    let mut p = Progress { position: Duration::ZERO, total, speed: None };
    for line in BufReader::new(stdout).lines() {
        if cancel.is_cancelled() {
            let _ = child.kill();
            break;
        }
        let line = line?;
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            // Despite the name, out_time_ms is also microseconds.
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = value.trim().parse::<u64>() {
                    p.position = Duration::from_micros(us);
                }
            }
            "speed" => p.speed = value.trim().trim_end_matches('x').parse().ok(),
            "progress" => {
                if value == "end" {
                    if let Some(t) = total {
                        p.position = t;
                    }
                }
                on_progress(p);
            }
            _ => {}
        }
    }
    Ok(())
}

fn tail_lines(r: impl Read, n: usize) -> String {
    let mut buf = VecDeque::with_capacity(n);
    for line in BufReader::new(r).lines().map_while(|l| l.ok()) {
        if buf.len() == n {
            buf.pop_front();
        }
        buf.push_back(line);
    }
    Vec::from(buf).join("\n")
}

fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        // Don't flash a console window when launched from the GUI.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn spawn_error(program: &Path, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::ToolNotFound(program.display().to_string())
    } else {
        Error::Io(e)
    }
}

#[derive(Deserialize)]
struct ProbeOut {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: usize,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    start_time: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

/// ffprobe's "12.500000" seconds, if valid.
fn parse_secs(s: Option<&str>) -> Option<Duration> {
    s?.parse::<f64>().ok().filter(|d| d.is_finite() && *d >= 0.0).map(Duration::from_secs_f64)
}

fn parse_probe(json: &[u8]) -> Result<MediaInfo> {
    let p: ProbeOut = serde_json::from_slice(json).map_err(|e| Error::Probe(e.to_string()))?;
    let streams = p
        .streams
        .into_iter()
        .map(|s| StreamInfo {
            index: s.index,
            kind: match s.codec_type.as_deref() {
                Some("video") => StreamKind::Video,
                Some("audio") => StreamKind::Audio,
                Some("subtitle") => StreamKind::Subtitle,
                _ => StreamKind::Other,
            },
            codec: s.codec_name.unwrap_or_else(|| "unknown".into()),
            language: s.tags.get("language").cloned(),
            width: s.width,
            height: s.height,
            frame_rate: parse_rate(s.avg_frame_rate.as_deref())
                .or_else(|| parse_rate(s.r_frame_rate.as_deref())),
            // Matroska keeps stream lengths in a tag instead.
            end: parse_secs(s.duration.as_deref())
                .or_else(|| s.tags.get("DURATION").and_then(|d| parse_timestamp(d)))
                .map(|d| d + parse_secs(s.start_time.as_deref()).unwrap_or_default()),
        })
        .collect();
    let (format, duration) = match p.format {
        Some(f) => (
            f.format_name.unwrap_or_default(),
            parse_secs(f.duration.as_deref()),
        ),
        None => (String::new(), None),
    };
    Ok(MediaInfo { format, duration, streams })
}

/// Parse ffprobe rates like "30000/1001". "0/0" means unknown.
fn parse_rate(r: Option<&str>) -> Option<f64> {
    let (n, d) = r?.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    let fps = n / d;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360,"avg_frame_rate":"30000/1001"},
            {"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}},
            {"index":2,"codec_type":"subtitle","codec_name":"subrip"}],
            "format":{"format_name":"matroska,webm","duration":"12.500000"}}"#;
        let info = parse_probe(json).unwrap();
        assert_eq!(info.count(StreamKind::Subtitle), 1);
        assert_eq!(info.duration, Some(Duration::from_millis(12_500)));
        assert_eq!(info.streams[1].language.as_deref(), Some("eng"));
        assert!((info.streams[0].frame_rate.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rate(Some("0/0")), None);
        assert_eq!(info.video_length(), info.duration, "no stream lengths: use the file's");
    }

    #[test]
    fn video_length_ends_with_the_picture() {
        // A trimmed MP4: AAC padding makes the file longer than its video.
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","start_time":"0.040000","duration":"2.520000"},
            {"index":1,"codec_type":"audio","codec_name":"aac","start_time":"0.000000","duration":"2.500000"}],
            "format":{"format_name":"mov,mp4","duration":"2.560000"}}"#;
        let info = parse_probe(json).unwrap();
        assert_eq!(info.video_length(), Some(Duration::from_millis(2_560)));

        // Matroska: lengths are in tags; the video stops before the audio.
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","tags":{"DURATION":"00:00:05.500000000"}},
            {"index":1,"codec_type":"audio","codec_name":"opus","tags":{"DURATION":"00:00:06.000000000"}}],
            "format":{"format_name":"matroska,webm","duration":"6.000000"}}"#;
        let info = parse_probe(json).unwrap();
        assert_eq!(info.video_length(), Some(Duration::from_millis(5_500)));
    }
}
