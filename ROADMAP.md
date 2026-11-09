# Flicker roadmap

Where Flicker is going: a light, usable video editor for Linux (FerroOS
first), Windows and Android. It stays one video player window with a menu
bar: every feature is a menu entry that runs an edit on the current video and
becomes an undoable step, never a separate screen. ffmpeg stays the engine
throughout; everything goes through the `Engine` trait.

## Done

- **v0.1–v0.4** Embed subtitles (soft and burned in), preview player with a
  subtitle overlay, cutting with keyframe-aware fast cuts and subtitle
  retiming.
- **v0.5: Player and edit history.** One window with a menu bar, cut mode with
  handles on the timeline, undo/redo between working files, Save As, Windows
  95 / Light / Dark themes.
- **v0.6: Joining and sound** (this release)
  - **Edit > Append Video or Picture**: joins a file to the end with a cut,
    cross-fade or dip to black; a different size is fitted with black bars.
  - **Audio > Volume, Mute, Fade In/Out** for the whole video.
  - Save As to MP4 (H.264/AAC) or WebM (VP9/Opus), as well as MKV and MOV.
  - Privacy: saved files carry no source tags, encoder names or dates unless
    asked for (File > Preferences); no recent-files list; no network.
  - `FLICKER_THEME=classic|light|dark` (FerroOS's menu entry uses classic).
  - Signed release workflow (minisign), waiting for a real key.

## Next

1. **v0.7: Sound in the preview, and music.** Play sound with the picture
   (`cpal`, audio as the master clock). **Audio > Add Music…**: a music file
   under the video with its own volume and fades, and a "duck" option that
   lowers it while the video has sound.
2. **v0.8: Text.** **Edit > Insert Title Card…** (a picture clip made from
   text) and **Edit > Text over Video…** for the part between the cut
   handles (ffmpeg `drawtext`), with a few fonts and positions.
3. **v0.9: Picture.** A **Picture** menu: Crop, Rotate (upright phone videos),
   Speed (0.5×–2×), Brightness/Contrast/Saturation. Insert a file at the
   playhead, not just at the end. Thumbnails along the timeline.
4. **v1.0: Packaging.** Windows installer with ffmpeg next to the exe; FerroOS
   bundle; Android build (ffmpeg as a native library behind `Engine`, Storage
   Access Framework file picker). Faster preview and scrubbing by moving
   decoding to libav (`ffmpeg-next`) in-process.

## Not planned

Multi-track compositing (picture-in-picture, green screen), a project file
format with many tracks, effects plugins, cloud anything.
