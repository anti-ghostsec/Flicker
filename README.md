# Flicker

A cross-platform video editor written in Rust: a **timeline** with video, picture and audio tracks, **transitions**, **sound in the preview**, **subtitles** and **private exports**. It's meant to stay light enough for an old laptop with 1 GB of RAM. It runs on Linux now; Windows and Android are planned.

## Features (v0.7)

Flicker is one window: the player at the top, the timeline under it, and a menu bar. Every change shows in the player straight away: there's no render or "apply" step. Only exporting takes time.

### The timeline

- **Tracks:** V1 for videos and pictures, V2 (and more, with **+ Video Track**) drawn over V1, and audio tracks A1, A2… (**+ Audio Track**) for music and voice on top of the videos' own sound. Each track has **M** (mute its sound) and video tracks **H** (hide their picture). Right-click a track to remove it once it's empty.
- **Adding files:** drag videos, pictures and sound files from the file manager onto a track, at the spot where they should start. A video dropped on an audio track adds just its sound. **File > Add to Timeline…** (`Ctrl+I`) puts videos and pictures at the end of V1, and sound at the playhead on the first audio track with room (or a new one).
- **Moving:** drag a clip along its track or to another track of the same kind. Clips never cover each other: dropped on another clip, a clip goes to the nearest free spot. Edges snap to the playhead and to other clips.
- **Trimming:** drag a clip's left or right edge. The player shows the frame at the edge while you drag. A picture's length is just its right edge (or **Length** in the clip's properties).
- **Splitting:** `S` splits the selected clip at the playhead (or every clip under the playhead when none is selected). **Delete** (`Del`) leaves a gap; **Ripple Delete** (`Shift+Del`) closes it.
- **Clip properties** (in the strip above the tracks when a clip is selected, or right-click): volume from 0 to 200%, mute, fade in and fade out.
- **Transitions:** right-click the join between two clips that touch, or use **From previous** in the properties: *Cut*, *Cross-fade* or *Dip to black*, 0.5 s to 10 s. The second clip moves back to overlap the first by the transition's length (everything after it on that track moves with it). Cross-fades show as an X on the clip, dips as a V.
- **Zoom and scroll:** `+` / `-`, **Fit**, or `Ctrl`+wheel around the pointer. The wheel scrolls along the timeline (or up and down when the tracks don't all fit); the scrollbar under the tracks works too. Click or drag the ruler to move the playhead.
- **Undo / Redo** (`Ctrl+Z` / `Ctrl+Y`) for every change; a drag is one step.
- Clips of another size are fitted with black bars (on V1) or with see-through borders (on V2 and up, so V1 shows around them). The frame size and rate come from the first video.

### Playing

- **Space** plays the timeline from the playhead, with sound, as it is right now. Changing something while it plays carries on from the same spot with the change. `←` / `→` step one frame, `Shift+←` / `→` five seconds, `Home` / `End` jump to the start or end.
- The picture follows the sound card's clock. On Linux sound goes to ALSA's default device through `libasound.so.2`, loaded when playback starts (nothing to install for building). If no sound device opens, playback carries on silently and says why next to the time.

### Projects

- **File > New Project** (`Ctrl+N`), **Open Project…** (`Ctrl+O`), **Save Project** (`Ctrl+S`) and **Save Project As…** (`Ctrl+Shift+S`). A project is a small `.flicker` file that points at your files; nothing is copied. Files moved into the project's folder are found there.
- Project files are **encrypted** with a password you choose. The first Save of a new project asks for it twice; it's kept in memory until the project is closed, so later saves don't ask. Opening a project asks for its password (a wrong one says so and lets you try again). **File > Change Project Password…** sets a new one and saves with it. A password can't be recovered: without it the project can't be opened. Plain projects from earlier versions still open; they're saved encrypted the next time (the status bar says so).
- A project from someone else can't make Flicker open anything but files on your computer. When a project is opened (encrypted, or a plain one from an earlier version), a clip or subtitle file is used only if it's a full path to a file that's there, or a file of that name in the project's folder. A file that's gone stays on the timeline and the status bar names it as missing; anything else a project might name (a web address, a relative name, a device) is treated the same way, as a missing file of that name in the project's folder.
- New, Open and quitting ask first when there are unsaved changes.

### Exporting

- **File > Export Video…** (`Ctrl+E`) renders the timeline to MP4 (H.264 + AAC), WebM (VP9 + Opus), MKV or MOV, with progress and **Cancel** in the status bar. ffmpeg writes straight into the chosen file in one pass; nothing else is written anywhere (no staging copy, nothing in `/tmp`). If the export is cancelled or fails, the unfinished file is deleted and the status bar says so.

### Subtitles

- **Subtitles > Load…** draws a `.srt`, `.ass`/`.ssa` or `.vtt` file over the preview, timed to the timeline, exactly as burning in renders it. **Show on Video** turns it off and on.
- **Subtitles > When Exporting:** *Leave Out*, *Add as Track* (viewers can turn it on or off; **Track Language** sets its language) or *Burn into Picture*. It's saved with the project.

### Privacy

- Exported files carry no tags copied from the source (title, comment, camera, GPS location, dates), no chapters and no data tracks.
- They carry no encoder name or version and no creation time. x264's version string, which sits inside the H.264 stream, is removed too. All of this happens while the file is written: no unscrubbed copy is ever on disk, not even for a moment. (Each track's encoder tag is left as a single space, the only way to stop ffmpeg filling in "Lavc libx264".) **File > Preferences > Write encoder name and dates into exported files** turns the encoder name and today's date back on (off by default). MKV and WebM must name the writing program: there it's just `Lavf`, with no version.
- **Projects are encrypted.** A `.flicker` file is a short header (`FLKPRJ`, a version byte, a random salt and nonce) and then ciphertext: no clip names, paths or times in clear. The key comes from your password with Argon2id (64 MiB, 3 passes); the contents are sealed with XChaCha20-Poly1305 (the header too is authenticated, so any change to the file is noticed). Salt and nonce are new on every save. The project's JSON only exists in memory and is wiped after use; the `name.flicker-part` file written just before the rename holds only ciphertext and is readable by you alone. The password is wiped from memory on New, Open and quitting.
- **No network**, ever, and no recent-files list. ffmpeg and ffprobe are given every file as `file:<path>` and may read inputs from local files only (`-protocol_whitelist file`), so neither a file's name nor a playlist posing as a video can make them connect anywhere. Settings (the theme and that one tick) are written only when you change one, to `~/.config/flicker/settings` (`%APPDATA%\Flicker\settings` on Windows).

### General

- **View > Theme:** *Windows 95* (default: grey 3D bevels, navy highlights), *Light* or *Dark*. The choice is remembered. `FLICKER_THEME=classic` (the Windows 95 look; also `light` or `dark`) forces a theme for one run without saving it. FerroOS's menu entry starts Flicker that way.
- From the command line: `flicker project.flicker`, or `flicker clip1.mp4 clip2.mp4 music.mp3 subs.srt` to start a timeline with those files.
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

To check the UI without a screen grabber (for scripts and CI), set `FLICKER_SCREENSHOT=out.ppm`. `FLICKER_SCREENSHOT_AFTER=<secs>` sets how long it waits first, `FLICKER_AUTOPLAY=1` starts playing, `FLICKER_SEEK=3.5` moves the playhead, `FLICKER_SELECT=V1:2` selects the third clip on V1, `FLICKER_ZOOM=60` sets the zoom (pixels per second), `FLICKER_DIALOG=shortcuts`, `about`, `unsaved`, `password` or `change-password` opens that dialog and `FLICKER_EXPORT=out.mp4` starts exporting there. `FLICKER_SYNC_LOG=file` writes, for every frame played, the sound clock and the frame's time, and how long each decode took to show its first frame.

When testing playback on a machine with speakers, point ALSA at a file that only defines a null device, e.g. `ALSA_CONFIG_PATH=silent.conf` with `pcm.!default { type null }` in it. Don't list `/usr/share/alsa/alsa.conf` in front of it: that file loads `~/.asoundrc`, which can win.

To see the ffmpeg arguments behind a project: `cargo run -p flicker-core --example graph -- project.flicker [preview|sound|export] [from] [to]` (it reads an encrypted project's password from stdin).

## Project layout

```
crates/
  flicker-core/   UI-agnostic media logic (no GUI dependencies)
    engine.rs        `Engine` trait: the seam for swapping media backends
    ffmpeg.rs        FfmpegCli: engine that drives ffmpeg/ffprobe executables
    timeline.rs      Tracks and clips, and every timeline edit (move, trim, split, transitions…)
    render.rs        The timeline as an ffmpeg filter graph: export and preview arguments
    project.rs       .flicker project files (Argon2id + XChaCha20-Poly1305)
    privacy.rs       Private saves: no source tags, encoder names or dates
    subtitles.rs     Subtitle formats, filter-path escaping, embedding
    input.rs         How files are handed to ffmpeg (local files only), paths from projects
    preview.rs       Frames and PCM for on-screen playback
    edit.rs          TimeRange / TrimRequest (trim & cut of a single file)
    retime.rs        Shifts/clips SRT and ASS cues to match a trim
    audio.rs         Volume / mute / fade of a single file
    media.rs         MediaInfo / StreamInfo from probing
    job.rs           Progress + CancelToken for long-running jobs
  flicker-app/    egui/eframe GUI
    app.rs           Window: menu bar, player, transport, timeline panel, status bar, dialogs
    timeline.rs      The timeline panel: ruler, tracks, clips, dragging, menus, properties
    player.rs        Plays the timeline: decoder thread, sound clock, scrub frames
    sound.rs         Sound output (ALSA via libasound.so.2, loaded at run time)
    project.rs       The open project: timeline, file, undo/redo
    theme.rs         Windows 95 / Light / Dark looks
    settings.rs      Remembered theme and tags choice
    common.rs        File pickers, JobRunner
```

## Design notes

- **Core vs. UI.** All media work goes through `flicker_core::Engine`. The GUI never builds ffmpeg commands itself, so the backend can change without touching the UI.
- **Edits are model changes.** Moving, trimming, splitting, fades and transitions change a small `Timeline` value (`timeline.rs`); undo keeps whole snapshots of it. Nothing is rendered until the player or an export asks.
- **One graph for everything.** `render.rs` turns any stretch of the timeline into one `-filter_complex` graph: a black background, every visible clip scaled to the frame and laid over it at its time (`overlay`, V1 first, so V2 lands on top), and every audible clip's sound delayed to its time (`adelay`) and mixed (`amix`). Fades and transitions are `fade`/`afade` filters timed in each clip's own time, so a stretch can start halfway through a clip or a transition. A cross-fade fades the later clip in over the earlier one; a dip fades the first to black and the second in from black. Pictures are decoded once and repeated in the graph.
- **Preview.** The player plays the timeline as a chain of 8-second stretches, each its own ffmpeg process (the next one starts 1.5 s before it's needed), so ffmpeg only opens the files that play in the next few seconds. One process writes small PPM frames, a second one 48 kHz PCM for the sound card; decoding starts half a second before each seek point and cuts that off in the graph, because some decoders (MP3) give silence just after a seek. While paused, a single frame is decoded for each change or scrub position (the newest request wins; recent frames are kept). For faster scrubbing, decoding could move in-process with libav (`ffmpeg-next`).
- **Sound output.** `sound.rs` binds the eight libasound calls it needs by hand and loads the library with `dlopen`, so there are no ALSA headers or `-sys` crates at build time. The picture follows how much sound the card has played (written minus `snd_pcm_delay`); the sound waits for the first frame so both start together.
- **No tags.** An export is one ffmpeg pass into the chosen file: `-map_metadata -1 -map_chapters -1` (no source tags or chapters), only the picture, sound and subtitle outputs mapped (no data tracks), `-fflags +bitexact` (the muxer writes no version or dates), `-flags +bitexact` on every encoder, `-metadata:s encoder= ` (otherwise ffmpeg tags each track "Lavc libx264"), and `filter_units=remove_types=6` to strip x264's SEI message. The subtitle language is set on the track. The subtitle file is read where it is, for a track or for burning in. On Cancel or an error the half-written file is deleted (a file of that name ffmpeg never got to is left alone).
- **Android.** The GUI is egui/eframe, which runs on Android through `android-activity`. The app code lives in a library (`lib.rs`) so a `cdylib` with `android_main` can reuse it. File pickers are behind `cfg(target_os = "android")` stubs, and they still need the Storage Access Framework; sound would need AAudio or OpenSL.
- **Windows.** Release builds use the GUI subsystem, so no console window appears. FFmpeg child processes start with `CREATE_NO_WINDOW`. Ship `ffmpeg.exe` and `ffprobe.exe` next to `flicker.exe`. Preview sound on Windows isn't written yet (it plays silently).

## Roadmap

1. ~~Embed subtitles (soft/hard)~~
2. ~~Video preview player~~
3. ~~Trim / cut~~, ~~joining clips and pictures~~, ~~volume, mute and fades~~
4. ~~A multi-track timeline, music tracks, sound in the preview, projects~~
5. Text and titles; picture adjustments (crop, rotate, speed)
6. Subtitle editor (edit timings and text in-app)
7. Android build + packaging (cargo-apk / xbuild); Windows installer

More detail in [ROADMAP.md](ROADMAP.md).

## Releases

`.github/workflows/release.yml` builds, tests and publishes a signed Linux build (`flicker-linux.tar.gz` + `.minisig`) on every push to `master` (and only from `master`; the actions it uses are pinned to a commit). It refuses to run until `minisign.pub` holds Flicker's real public key (it's a placeholder now) and the `MINISIGN_SECRET_KEY` secret is set:

```bash
minisign -G -p minisign.pub -s flicker.key   # keep flicker.key private; put its contents in the secret
```
