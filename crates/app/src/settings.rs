//! Persisted preferences.
//!
//! A small TOML file next to the recent-projects list. Everything here is
//! something the person has turned on or off by using the editor, so it
//! has to survive a restart — a preference that resets is worse than no
//! preference.
//!
//! Unknown or malformed values fall back to the default rather than
//! refusing to start: a settings file you have hand-edited into a corner
//! should not lock you out of the editor that edits it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A kind of file the preview knows how to render.
///
/// Each gets its own switch. Turning off the Markdown preview should not
/// also stop images opening as pictures — they are not the same decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewKind {
    Markdown,
    /// SVG is its own switch rather than part of Image, because it is the
    /// one picture format that is also a text file worth editing. Turning
    /// off the preview for it shows the markup; doing the same to a PNG
    /// shows an empty buffer.
    Svg,
    Image,
}

impl PreviewKind {
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "md" | "markdown" => Self::Markdown,
            "svg" => Self::Svg,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => Self::Image,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Preview Markdown files",
            Self::Svg => "Preview SVG files",
            Self::Image => "Preview images",
        }
    }

    pub const ALL: [Self; 3] = [Self::Markdown, Self::Svg, Self::Image];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Write a modified buffer back after a pause in typing.
    pub autosave: bool,
    pub preview_markdown: bool,
    /// On by default, like the other pictures: an SVG you opened to look
    /// at is the common case, and the markup is one toggle away.
    pub preview_svg: bool,
    /// On by default: the alternative for an image is an empty buffer,
    /// which is not a view of anything.
    pub preview_image: bool,
    /// Show a diff with the two versions side by side rather than as one
    /// interleaved column.
    ///
    /// On by default: two columns is what a diff looks like everywhere
    /// else, and the interleaved form is the one you choose when the
    /// window is too narrow for two.
    pub side_by_side_diff: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autosave: false,
            preview_markdown: false,
            preview_svg: true,
            preview_image: true,
            side_by_side_diff: true,
        }
    }
}

impl Settings {
    /// Every key this version knows. Used only to tell a typo from a
    /// setting that no longer exists; both are ignored, and only the
    /// first is worth a line in the log.
    const KEYS: [&'static str; 5] = [
        "autosave",
        "preview_markdown",
        "preview_svg",
        "preview_image",
        "side_by_side_diff",
    ];

    pub fn preview(&self, kind: PreviewKind) -> bool {
        match kind {
            PreviewKind::Markdown => self.preview_markdown,
            PreviewKind::Svg => self.preview_svg,
            PreviewKind::Image => self.preview_image,
        }
    }

    pub fn set_preview(&mut self, kind: PreviewKind, on: bool) {
        match kind {
            PreviewKind::Markdown => self.preview_markdown = on,
            PreviewKind::Svg => self.preview_svg = on,
            PreviewKind::Image => self.preview_image = on,
        }
    }

    /// Whether `path` should open in its preview.
    pub fn previews(&self, path: &Path) -> bool {
        PreviewKind::of(path).is_some_and(|kind| self.preview(kind))
    }

    pub fn load() -> Self {
        let Some(path) = store_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let parsed: Result<toml::Table, _> = toml::from_str(&text);
        let Ok(table) = parsed.map_err(|e| {
            log::warn!(
                "{} is not valid settings, using defaults: {e}",
                path.display()
            )
        }) else {
            return Self::default();
        };
        // An unknown key is noted rather than refused. Refusing one meant
        // a setting that was retired took every other preference in the
        // file down with it, since the whole thing failed to parse and
        // fell back to the defaults.
        for key in table.keys() {
            if !Self::KEYS.contains(&key.as_str()) {
                log::warn!("{}: ignoring unknown setting {key}", path.display());
            }
        }
        table.try_into().unwrap_or_else(|e| {
            log::warn!(
                "{} has a setting of the wrong type, using defaults: {e}",
                path.display()
            );
            Self::default()
        })
    }

    /// Best effort: failing to record a preference is not worth
    /// interrupting the editor for, but it is worth a line in the log.
    pub fn save(&self) {
        let Some(path) = store_path() else { return };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        match toml::to_string_pretty(&self.chosen()) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    log::warn!("could not write {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("could not serialise settings: {e}"),
        }
    }

    /// Only the settings that differ from the default.
    ///
    /// Writing every value, chosen or not, freezes the defaults at the
    /// moment the file was first written: toggling one preference records
    /// all the others as though they had been decided, and changing a
    /// default afterwards can never reach anyone who has the file. That is
    /// not hypothetical — it is how turning on the Markdown preview wrote
    /// `side_by_side_diff = false` into every settings file that existed.
    ///
    /// Omitting what was never chosen means a default stays a default
    /// until someone actually changes it. A value deliberately set to what
    /// is now the default is still dropped, and then follows it; a value
    /// set to anything else differs, so it is kept.
    fn chosen(&self) -> toml::Table {
        let table = |settings: &Self| match toml::Value::try_from(settings) {
            Ok(toml::Value::Table(table)) => table,
            _ => toml::Table::new(),
        };
        let base = table(&Self::default());
        table(self)
            .into_iter()
            .filter(|(key, value)| base.get(key) != Some(value))
            .collect()
    }
}

fn store_path() -> Option<PathBuf> {
    Some(config_dir()?.join("settings.toml"))
}

/// Where lithe keeps the two files it owns, following each platform's
/// convention.
///
/// Both of them live here: the settings and the recent-projects list are
/// one directory's worth of state, and there is nothing else.
#[cfg(target_os = "windows")]
pub fn config_dir() -> Option<PathBuf> {
    // `%APPDATA%` is where a Windows application's own settings go, and it
    // is set for a process launched from Explorer — where `HOME` is not.
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
    })?;
    Some(roaming.join("lithe"))
}

#[cfg(not(target_os = "windows"))]
pub fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/lithe")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("lithe")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previewable_kinds_are_told_apart() {
        assert_eq!(
            PreviewKind::of(Path::new("a.MD")),
            Some(PreviewKind::Markdown)
        );
        assert_eq!(PreviewKind::of(Path::new("a.svg")), Some(PreviewKind::Svg));
        assert_eq!(
            PreviewKind::of(Path::new("a.png")),
            Some(PreviewKind::Image)
        );
        assert_eq!(PreviewKind::of(Path::new("a.rs")), None);
    }

    #[test]
    fn each_kind_switches_independently() {
        let mut settings = Settings::default();
        settings.set_preview(PreviewKind::Markdown, true);

        assert!(settings.previews(Path::new("notes.md")));
        assert!(
            !settings.previews(Path::new("page.html")),
            "HTML has no preview at all any more"
        );
        assert!(
            settings.previews(Path::new("logo.svg")),
            "images default to previewing, since the alternative is blank"
        );

        settings.set_preview(PreviewKind::Svg, false);
        assert!(!settings.previews(Path::new("logo.svg")));
        assert!(
            settings.previews(Path::new("logo.png")),
            "SVG and PNG are different decisions"
        );
        assert!(
            settings.previews(Path::new("notes.md")),
            "and the others hold"
        );
    }

    #[test]
    fn a_file_with_no_preview_never_previews() {
        let mut settings = Settings::default();
        for kind in PreviewKind::ALL {
            settings.set_preview(kind, true);
        }
        assert!(!settings.previews(Path::new("main.rs")));
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let settings = Settings {
            autosave: true,
            preview_markdown: true,
            preview_svg: false,
            preview_image: false,
            side_by_side_diff: true,
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert!(back.autosave && back.preview_markdown && back.side_by_side_diff);
        assert!(!back.preview_image);
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let back: Settings = toml::from_str("autosave = true\n").unwrap();
        assert!(back.autosave);
        assert!(!back.preview_markdown, "the absent key should default");
        assert!(back.preview_image, "including the ones that default on");
    }

    #[test]
    fn a_broken_file_does_not_lock_anyone_out() {
        let parsed: Result<Settings, _> = toml::from_str("autosave = \"yes please\"");
        assert!(parsed.is_err(), "this really is invalid");
        assert!(!parsed.unwrap_or_default().autosave);
    }

    #[test]
    fn only_what_was_chosen_is_written_down() {
        let mut settings = Settings::default();
        assert_eq!(
            settings.chosen().len(),
            0,
            "an untouched settings file should say nothing at all"
        );

        settings.set_preview(PreviewKind::Markdown, true);
        let written = settings.chosen();
        assert_eq!(written.keys().collect::<Vec<_>>(), vec!["preview_markdown"]);
        assert!(
            !written.contains_key("side_by_side_diff"),
            "a default nobody touched must not be recorded as a decision, \
             or changing it later can never reach anyone: {written:?}"
        );

        // A value set away from its default is a decision, and is kept.
        settings.side_by_side_diff = false;
        assert_eq!(
            settings.chosen().get("side_by_side_diff"),
            Some(&toml::Value::Boolean(false))
        );
    }

    #[test]
    fn a_written_file_round_trips_through_the_defaults() {
        let settings = Settings {
            autosave: true,
            side_by_side_diff: false,
            ..Default::default()
        };

        let text = toml::to_string_pretty(&settings.chosen()).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert!(back.autosave && !back.side_by_side_diff);
        assert!(
            back.preview_image && !back.preview_markdown,
            "the keys left out come back as whatever the default now is"
        );
    }

    #[test]
    fn diffs_default_to_two_columns() {
        let settings = Settings::default();
        assert!(
            settings.side_by_side_diff,
            "two columns is what a diff looks like everywhere else"
        );
    }

    #[test]
    fn the_file_lives_beside_the_other_state() {
        let path = store_path().expect("HOME is set");
        assert!(path.ends_with("lithe/settings.toml"), "got {path:?}");
    }
}
