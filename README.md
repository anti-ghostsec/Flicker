# Flicker

A cross-platform video tool written in Rust. Right now it can **embed subtitles** and **trim/cut videos**. It's meant to grow into a full video editor for Windows, Linux and Android.

## Features (v0.5)

Flicker is a video player with a menu bar. Playback loops, and the timeline ends where the picture does (even when a file's audio runs longer). Everything you can do lives in the menus, so the window stays a plain video screen and new features slot in as new menu entries.

### Cutting

1. **Cut > Cut Mode** (or press `C`). Two handles appear on the timeline under the video.
2. Drag the handles: grab them anywhere along their height. The picture follows the handle you're dragging, so you see exactly where the cut lands. `I` / `O` move the start / end handle to the playhead.
3. Press **Space** to play: playback loops over the result, between the handles in *Keep* mode, or around the removed part in *Remove* mode.
4. **Apply** (in the menu bar, or `Enter`) cuts the video. The result becomes the current video and the timeline adapts, so you can keep cutting without re-importing. `Esc` cancels.

- **Cut > Keep / Remove between handles** picks what applying does.
- **Cut > Accuracy:** *Precise* (default) re-encodes to H.264/AAC so the cut lands on the exact frame. *Fast* copies the streams losslessly but starts each part on a keyframe.
- Subtitle tracks in the video are cut along with it. Subtitles loaded for preview are retimed too, so they stay in sync after every cut.

### Undo and saving

- Every applied edit is a step. **Edit > Undo / Redo** (`Ctrl+Z` / `Ctrl+Y`) move between them. **File > Revert to Original** goes all the way back.
- Edits go to temporary working files: the original is never touched. **File > Save As** (`Ctrl+S`) writes the current video. Quitting or opening another video with unsaved edits asks first.

### Subtitles

- **Subtitles > Load for Preview** draws a `.srt`, `.ass`/`.ssa` or `.vtt` file over the video, exactly as burn-in renders it.
- **Add as Track** adds it as a track viewers can turn on or off (fast, no quality loss). **Burn into Picture** draws it into the video so it shows on every player (re-encodes). Both are edits you can undo. **Track Language** sets the track's language.

### General

- **View > Theme:** *Windows 95* (default: grey 3D bevels, navy highlights), *Light* or *Dark*. The choice is remembered.
- Drag and drop a video to open it, or a subtitle file to preview it. From the command line: `flicker video.mp4 subs.srt`.
- **Help > Keyboard Shortcuts** lists every key.

## Running

You need [Rust](https://rustup.rs) and [FFmpeg](https://ffmpeg.org/download.html) (`ffmpeg` and `ffprobe` must be on `PATH`, or sit next to the `flicker` executable).

```bash
cargo run --release
```

Try it with the files in `samples/`.

```bash
cargo test            # unit tests + end-to-end tests against real ffmpeg
```

To check the UI without a screen grabber (for scripts and CI), set `FLICKER_SCREENSHOT=out.ppm`. `FLICKER_SCREENSHOT_AFTER=<secs>` sets how long it waits first, `FLICKER_AUTOPLAY=1` starts playing, and `FLICKER_CUT=2,4` opens cut mode with the handles at 2s and 4s.

## Project layout

```
crates/
  flicker-core/   UI-agnostic media logic (no GUI dependencies)
    engine.rs        `Engine` trait: the seam for swapping media backends
    ffmpeg.rs        FfmpegCli: engine that drives ffmpeg/ffprobe executables
    subtitles.rs     EmbedRequest + ffmpeg argument building (pure, unit-tested)
    edit.rs          TimeRange / keep_ranges / TrimRequest (trim & cut)
    retime.rs        Shifts/clips SRT and ASS cues to match a trim
    preview.rs       PreviewRequest / Frame / FrameSource for on-screen playback
    media.rs         MediaInfo / StreamInfo from probing
    job.rs           Progress + CancelToken for long-running jobs
  flicker-app/    egui/eframe GUI
    app.rs           Window: menu bar, video, transport, status bar, dialogs
    player.rs        Player (decoder thread + frame clock + range looping)
    timeline.rs      The timeline and its cut handles
    session.rs       Edit history: one working file per applied edit
    theme.rs         Windows 95 / Light / Dark looks
    common.rs        File pickers, JobRunner
```

## Design notes for the full editor

- **Core vs. UI.** All media work goes through `flicker_core::Engine`. The GUI never builds ffmpeg commands itself, so the backend can change without touching the UI.
- **Engines.** `FfmpegCli` is the simplest way to get going on desktop. Options for later:
  - Link libav* in-process with the `ffmpeg-next` crate. You need this for frame-accurate preview and scrubbing on a timeline.
  - On Android you can't spawn an `ffmpeg` binary from `PATH`. Use one of these:
    - bundle FFmpeg as a native library (`.so`) and implement `Engine` over it
    - use `MediaCodec`/`MediaMuxer` through JNI
- **Preview.** `FfmpegCli` streams scaled frames from ffmpeg as PPM images (each image carries its own size) at up to 30 fps. Subtitles go through the same libass filter as burn-in, so what you see is what you'll export. The preview has no audio yet: the plan is `cpal` playback with the audio as the master clock. For a real timeline, switch the decoder to `ffmpeg-next` so seeking is faster.
- **Trimming without re-encoding.** Copy mode never seeks, because FFmpeg's quick-seek lands on different keyframes in different containers. One pass with the segment muxer splits the file at keyframes into pieces whose timestamps start at 0. The pieces are joined, each cut to length. Subtitles are never cut by FFmpeg: they're extracted from the untouched source and retimed in Rust (`retime.rs`). To debug a real file, run `cargo run -p flicker-core --example trim -- IN OUT START END [precise]`.
- **Menus, not screens.** New features are menu entries that run an edit on the current video and push a step onto the session (`session.rs`), the same way cuts do. That keeps undo, saving and "keep working on the result" free for every feature.
- **Android.** The GUI is egui/eframe, which runs on Android through `android-activity`. The app code lives in a library (`lib.rs`) so a `cdylib` with `android_main` can reuse it. File pickers are behind `cfg(target_os = "android")` stubs, and they still need the Storage Access Framework.
- **Windows.** Release builds use the GUI subsystem, so no console window appears. FFmpeg child processes start with `CREATE_NO_WINDOW`. Ship `ffmpeg.exe` and `ffprobe.exe` next to `flicker.exe`.

## Roadmap

1. ~~Embed subtitles (soft/hard)~~
2. ~~Video preview player~~ (audio playback still to do)
3. ~~Trim / cut~~. Still to do: joining separate files (concatenate).
4. Timeline with multiple tracks
5. Subtitle editor (edit timings and text in-app)
6. Android build + packaging (cargo-apk / xbuild); Windows installer
