# Flicker roadmap

Where Flicker is going: a light, usable video editor for Linux (FerroOS
first), Windows and Android, in the spirit of Shotcut but much smaller. It
stays one window: the player, the timeline under it and a menu bar, never a
home screen or separate tool screens. Edits show in the player at once;
only exporting takes time. ffmpeg stays the engine throughout; everything
goes through the `Engine` trait.

## Done

- **v0.1–v0.4** Embed subtitles (soft and burned in), preview player with a
  subtitle overlay, cutting with keyframe-aware fast cuts and subtitle
  retiming.
- **v0.5: Player and edit history.** One window with a menu bar, cut mode with
  handles on the timeline, undo/redo between working files, Save As, Windows
  95 / Light / Dark themes.
- **v0.6: Joining and sound**
  - **Edit > Append Video or Picture**: joins a file to the end with a cut,
    cross-fade or dip to black; a different size is fitted with black bars.
  - **Audio > Volume, Mute, Fade In/Out** for the whole video.
  - Save As to MP4 (H.264/AAC) or WebM (VP9/Opus), as well as MKV and MOV.
  - Privacy: saved files carry no source tags, encoder names or dates unless
    asked for (File > Preferences); no recent-files list; no network.
  - `FLICKER_THEME=classic|light|dark` (FerroOS's menu entry uses classic).
  - Signed release workflow (minisign), waiting for a real key.

- **v0.7: The timeline**
  - A timeline panel under the player: V1 and V2 (and more) video tracks,
    A1, A2… audio tracks, with per-track mute and hide.
  - Drop videos, pictures and sound on a track; move, trim, split, delete
    and ripple delete; snapping; volume, mute and fades per clip;
    cross-fades and dips to black between clips; undo/redo for all of it.
  - The preview plays the whole timeline with sound (ALSA, loaded at run
    time), in sync, and shows every change straight away.
  - Projects (`.flicker`), Export to MP4/WebM/MKV/MOV with the same privacy
    as before; subtitles left out, added as a track or burned in at export.
  - Cut mode, Edit > Append and the Audio menu are gone: the timeline does
    all of that.

- **v0.8: Text, picture and effects** (this release)
  - Titles (Edit > Add Text, `Ctrl+T`) on their own clips, drawn with
    ffmpeg `drawtext`: size, colour, place, dark box.
  - Right-click a clip: speed (0.25×–4×), detach audio, flip and rotate,
    opacity, crop, and motion effects (Ken Burns zoom, pan, pulse, bounce,
    float, shake, wobble, spin) with entrances and exits (fade, slide,
    zoom, spin). Same graph in the player and the export.
  - Cut, copy, paste, duplicate.
  - Trimming follows the pointer exactly: it's computed from where the edge
    was grabbed and no longer snaps to the playhead that follows it.
  - Sound-only export (M4A, MP3, Opus, FLAC, WAV); videos on audio tracks
    are never decoded for their picture.

## Next

1. **v0.9: Picture.** Clip thumbnails and audio waveforms on the timeline;
   fonts for titles; brightness/contrast/saturation per clip; position and
   size for clips on V2 (picture-in-picture). Ducking: music gets quieter
   while a video talks.
2. **v1.0: Packaging.** Windows installer with ffmpeg next to the exe, and
   sound on Windows (WASAPI); FerroOS bundle; Android build (ffmpeg as a
   native library behind `Engine`, Storage Access Framework file picker,
   AAudio). Faster scrubbing by decoding in-process with libav
   (`ffmpeg-next`).

## Not planned

Effects plugins, keyframed animation, green screen, cloud anything.
