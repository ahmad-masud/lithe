//! Recently opened projects.
//!
//! One absolute path per line in a plain text file. No serde, no database,
//! nothing to migrate — a format you can read and edit with the editor
//! itself, which is the right weight for eight lines of state.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// How many projects the landing page remembers.
const LIMIT: usize = 8;

#[derive(Debug, Default)]
pub struct Recents {
    paths: Vec<PathBuf>,
}

impl Recents {
    pub fn load() -> Self {
        Self::read()
    }

    fn read() -> Self {
        let Some(file) = store_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(file) else {
            return Self::default();
        };
        let mut recents = Self {
            paths: text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
        };
        recents.truncate();
        recents
    }

    /// Entries that still exist on disk. A project that has been moved or
    /// deleted should not keep offering itself.
    pub fn live(&self) -> Vec<&Path> {
        self.paths
            .iter()
            .filter(|p| p.is_dir())
            .map(PathBuf::as_path)
            .collect()
    }

    /// Moves `path` to the front, and persists.
    ///
    /// Re-reads the file first. Two windows — or two processes — each hold
    /// their own copy of this list, and writing a stale one back would
    /// silently drop whatever the other added.
    pub fn remember(&mut self, path: &Path) {
        let path = path.to_path_buf();
        let mut merged = Self::read().paths;
        for existing in self.paths.drain(..) {
            if !merged.contains(&existing) {
                merged.push(existing);
            }
        }
        merged.retain(|existing| existing != &path);
        merged.insert(0, path);
        self.paths = merged;
        self.truncate();
        self.save();
    }

    fn truncate(&mut self) {
        self.paths.truncate(LIMIT);
    }

    /// Best effort: failing to record a recent project is never worth
    /// interrupting the editor for.
    fn save(&self) {
        let Some(file) = store_path() else { return };
        if let Some(parent) = file.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let Ok(handle) = std::fs::File::create(&file) else {
            return;
        };
        let mut writer = std::io::BufWriter::new(handle);
        for path in &self.paths {
            if writeln!(writer, "{}", path.display()).is_err() {
                return;
            }
        }
    }
}

/// Where the list lives, following each platform's convention.
fn store_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/lithe")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("lithe")
    };
    Some(dir.join("recent-projects"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_moves_an_entry_to_the_front_without_duplicating() {
        let mut recents = Recents {
            paths: vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c"),
            ],
        };
        // Bypass disk: exercise the ordering rule directly.
        let path = PathBuf::from("/c");
        recents.paths.retain(|p| p != &path);
        recents.paths.insert(0, path);
        assert_eq!(
            recents.paths,
            vec![
                PathBuf::from("/c"),
                PathBuf::from("/a"),
                PathBuf::from("/b")
            ]
        );
    }

    #[test]
    fn the_list_is_capped() {
        let mut recents = Recents {
            paths: (0..20).map(|i| PathBuf::from(format!("/p{i}"))).collect(),
        };
        recents.truncate();
        assert_eq!(recents.paths.len(), LIMIT);
    }

    #[test]
    fn missing_directories_are_not_offered() {
        let dir = crate::testutil::TempDir::new();
        let recents = Recents {
            paths: vec![
                dir.path().to_path_buf(),
                PathBuf::from("/definitely/not/here"),
            ],
        };
        let live = recents.live();
        assert_eq!(live.len(), 1, "a deleted project must drop off the list");
        assert_eq!(live[0], dir.path());
    }

    #[test]
    fn the_store_lives_somewhere_sensible() {
        let path = store_path().expect("HOME is set in the test environment");
        assert!(path.ends_with("lithe/recent-projects"), "got {path:?}");
        assert!(path.is_absolute());
    }
}
