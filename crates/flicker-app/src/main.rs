#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    flicker_app::run_desktop()
}
