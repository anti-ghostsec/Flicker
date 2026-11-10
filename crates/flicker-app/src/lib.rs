//! Flicker GUI. The app lives in a library so the desktop binary and a future
//! Android `cdylib` (`android_main`) can share it.

mod app;
mod common;
pub mod player;
mod project;
mod settings;
mod sound;
mod theme;
mod timeline;

pub use app::FlickerApp;

pub const APP_NAME: &str = "Flicker";

#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    // `flicker project.flicker`, or `flicker video.mp4 music.mp3 [subs.srt]`
    // (also how "Open with…" passes files). Full paths, so a project saved
    // from here finds them again whatever folder Flicker starts in.
    let files: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .map(|p| std::path::absolute(&p).unwrap_or(p))
        .collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([640.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(FlickerApp::new(cc, files)))))
}
