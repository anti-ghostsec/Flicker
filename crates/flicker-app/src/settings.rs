//! The few things Flicker remembers between runs: the theme and whether saved
//! files may carry the encoder name and dates. Written only when one of them
//! changes, to `~/.config/flicker/settings` (`%APPDATA%\Flicker\settings` on
//! Windows). There is no recent-files list and nothing ever goes over the
//! network.

use std::path::PathBuf;

use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    /// "Write encoder name and dates into saved files". Off by default.
    pub write_tags: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: Theme::Win95, write_tags: false }
    }
}

impl Settings {
    /// Saved settings. `FLICKER_THEME=classic|light|dark` forces the theme
    /// for this run without saving it.
    pub fn load() -> Self {
        let mut s = Self::load_saved();
        if let Some(t) = std::env::var("FLICKER_THEME").ok().and_then(|k| Theme::from_key(&k)) {
            s.theme = t;
        }
        s
    }

    /// What's in the file, ignoring `FLICKER_THEME`.
    pub fn load_saved() -> Self {
        file().and_then(|f| std::fs::read_to_string(f).ok()).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    /// Change one setting in the file, leaving the others as saved (so a
    /// theme forced by `FLICKER_THEME` isn't written).
    pub fn update(change: impl FnOnce(&mut Self)) {
        let mut s = Self::load_saved();
        change(&mut s);
        s.save();
    }

    fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, self.to_text());
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "theme" => s.theme = Theme::from_key(value).unwrap_or(s.theme),
                "write_tags" => s.write_tags = value.trim() == "true",
                _ => {}
            }
        }
        s
    }

    fn to_text(self) -> String {
        format!("theme={}\nwrite_tags={}\n", self.theme.key(), self.write_tags)
    }
}

fn file() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("flicker").join("settings"));
    }
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(app_data).join("Flicker").join("settings"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("flicker").join("settings"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let s = Settings { theme: Theme::Dark, write_tags: true };
        assert_eq!(Settings::parse(&s.to_text()), s);
        // Private by default; unknown lines and values are ignored.
        assert_eq!(Settings::parse("junk\ntheme=purple\n"), Settings::default());
        assert!(!Settings::default().write_tags);
        assert_eq!(Settings::parse("theme=classic").theme, Theme::Win95);
        assert_eq!(Theme::from_key(" Light "), Some(Theme::Light));
    }
}
