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

- **v0.7: The timeline** (this release)
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

## Next

1. **v0.8: Text.** Title cards (a picture clip made from text) and text
   over the video on V2 (ffmpeg `drawtext`), with a few fonts and positions.
   Clip thumbnails and audio waveforms on the timeline.
2. **v0.9: Picture.** Crop, rotate (upright phone videos), speed
   (0.5×–2×), brightness/contrast/saturation per clip; position and size for
   clips on V2 (picture-in-picture). Ducking: music gets quieter while a
   video talks.
3. **v1.0: Packaging.** Windows installer with ffmpeg next to the exe, and
   sound on Windows (WASAPI); FerroOS bundle; Android build (ffmpeg as a
   native library behind `Engine`, Storage Access Framework file picker,
   AAudio). Faster scrubbing by decoding in-process with libav
   (`ffmpeg-next`).

## Not planned

Effects plugins, keyframed animation, green screen, cloud anything.
