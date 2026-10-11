//! The source control panel.
//!
//! Shares the sidebar with the file tree — one or the other, never both,
//! because a bare-bones editor should not be teaching you a layout.
//!
//! `git status` is a subprocess, so it runs when something could have
//! changed rather than every frame: when the panel opens, and after a
//! save, stage, unstage or commit.

use std::path::{Path, PathBuf};

use lithe_vcs::{Commit, FileChange, Repo};

/// One line of the panel. Headers and blanks are rows too, so position is
/// an index times the line height and drawing cannot drift from
/// hit-testing — the same arrangement the landing page uses.
pub enum Row {
    Blank,
    Header(String),
    /// Index into `changes`.
    Change(usize),
    /// Index into `commits`.
    Commit(usize),
    Empty(&'static str),
    /// The Changes / History switch, always the first row.
    Tabs,
    /// Offered instead of everything else when there is no repository
    /// here. The panel's whole job needs one, so the first thing it
    /// should offer is making one.
    Init,
}

/// Which half of the panel is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Changes,
    History,
}

pub struct GitPanel {
    pub changes: Vec<FileChange>,
    pub commits: Vec<Commit>,
    pub mode: Mode,
    pub scroll: usize,
    /// When set, the history is one file's rather than the project's.
    /// Clicking the History tab clears it, so there is always a way back
    /// to the whole log.
    pub file: Option<PathBuf>,
    /// Whether there is a repository to talk about at all.
    pub in_repo: bool,
}

impl GitPanel {
    /// How many commits the history shows. Enough to find your way back
    /// through a day's work; not an excuse to hold the whole repository.
    const HISTORY: usize = 100;

    pub fn new(repo: Option<&Repo>) -> Self {
        let mut panel = Self {
            changes: Vec::new(),
            commits: Vec::new(),
            mode: Mode::default(),
            scroll: 0,
            file: None,
            in_repo: false,
        };
        panel.refresh(repo);
        panel
    }

    pub fn refresh(&mut self, repo: Option<&Repo>) {
        self.in_repo = repo.is_some();
        self.changes = repo.map(Repo::changes).unwrap_or_default();
        // Only read the log when it is being looked at; it is a
        // subprocess like everything else here.
        self.commits = match (self.mode, repo) {
            (Mode::History, Some(repo)) => match self.file.as_deref() {
                Some(path) => repo.file_log(path, Self::HISTORY),
                None => repo.log(Self::HISTORY),
            },
            _ => Vec::new(),
        };
        let rows = self.rows().len();
        self.scroll = self.scroll.min(rows.saturating_sub(1));
    }

    pub fn set_mode(&mut self, mode: Mode, repo: Option<&Repo>) {
        if self.mode == mode && self.file.is_none() {
            return;
        }
        // Picking a tab by hand means the whole project, even when the
        // panel was last showing one file's history.
        self.file = None;
        self.mode = mode;
        self.scroll = 0;
        self.refresh(repo);
    }

    /// Shows one file's history instead of the project's.
    pub fn show_file(&mut self, path: &Path, repo: Option<&Repo>) {
        self.file = Some(path.to_path_buf());
        self.mode = Mode::History;
        self.scroll = 0;
        self.refresh(repo);
    }

    fn indices(&self, staged: bool) -> Vec<usize> {
        self.changes
            .iter()
            .enumerate()
            .filter(|(_, c)| c.staged == staged)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Tabs, Row::Blank];

        // Nothing here means anything without a repository, so the panel
        // offers to start one rather than reporting an empty list of
        // changes as though there were simply none.
        if !self.in_repo {
            rows.push(Row::Empty("not a git repository"));
            rows.push(Row::Blank);
            rows.push(Row::Init);
            return rows;
        }

        if self.mode == Mode::History {
            if let Some(name) = self
                .file
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
            {
                rows.push(Row::Header(format!("History of {name}")));
            }
            if self.commits.is_empty() {
                rows.push(Row::Empty("no commits yet"));
            } else {
                rows.extend((0..self.commits.len()).map(Row::Commit));
            }
            return rows;
        }

        let staged = self.indices(true);
        let unstaged = self.indices(false);

        if self.changes.is_empty() {
            rows.push(Row::Empty("nothing to commit"));
            return rows;
        }

        if !staged.is_empty() {
            rows.push(Row::Header(format!("Staged ({})", staged.len())));
            rows.extend(staged.into_iter().map(Row::Change));
            rows.push(Row::Blank);
        }
        if !unstaged.is_empty() {
            rows.push(Row::Header(format!("Changes ({})", unstaged.len())));
            rows.extend(unstaged.into_iter().map(Row::Change));
        }
        if self.has_staged() {
            rows.push(Row::Blank);
            rows.push(Row::Empty("cmd shift C to commit"));
        }
        rows
    }

    /// The change a visible row refers to, if it is a file row.
    pub fn change_at(&self, row: usize) -> Option<&FileChange> {
        match self.rows().get(row) {
            Some(Row::Change(index)) => self.changes.get(*index),
            _ => None,
        }
    }

    /// The commit a visible row refers to, if it is a history row.
    pub fn commit_at(&self, row: usize) -> Option<&Commit> {
        match self.rows().get(row) {
            Some(Row::Commit(index)) => self.commits.get(*index),
            _ => None,
        }
    }

    /// Whether a visible row is the button that starts a repository.
    pub fn init_at(&self, row: usize) -> bool {
        matches!(self.rows().get(row), Some(Row::Init))
    }

    pub fn has_staged(&self) -> bool {
        self.changes.iter().any(|c| c.staged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn change(name: &str, status: char, staged: bool) -> FileChange {
        FileChange {
            path: PathBuf::from("/p").join(name),
            display: name.to_string(),
            status,
            staged,
        }
    }

    fn with(changes: Vec<FileChange>) -> GitPanel {
        GitPanel {
            changes,
            commits: Vec::new(),
            mode: Mode::Changes,
            scroll: 0,
            file: None,
            in_repo: true,
        }
    }

    #[test]
    fn without_a_repository_the_panel_offers_to_make_one() {
        let mut panel = with(Vec::new());
        panel.in_repo = false;
        let rows = panel.rows();

        // The button is the point: an empty change list outside a
        // repository reads as "nothing has changed", which is not what
        // is going on.
        let init = rows
            .iter()
            .position(|r| matches!(r, Row::Init))
            .expect("an init row");
        assert!(panel.init_at(init));
        assert!(
            rows.iter()
                .any(|r| matches!(r, Row::Empty(text) if text.contains("not a git"))),
            "and it says why"
        );
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Change(_))),
            "there is nothing to list"
        );
    }

    #[test]
    fn inside_a_repository_there_is_no_button() {
        let panel = with(vec![change("a.rs", 'M', false)]);
        assert!(!panel.rows().iter().any(|r| matches!(r, Row::Init)));
        assert!((0..panel.rows().len()).all(|row| !panel.init_at(row)));
    }

    fn headers(panel: &GitPanel) -> Vec<String> {
        panel
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Header(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_clean_tree_says_so_instead_of_showing_empty_headings() {
        let panel = with(Vec::new());
        assert!(headers(&panel).is_empty(), "no section with nothing in it");
        assert!(panel.rows().iter().any(|r| matches!(r, Row::Empty(_))));
        assert!(!panel.has_staged());
    }

    #[test]
    fn the_mode_switch_is_always_the_first_row() {
        let panel = with(vec![change("a.rs", 'M', false)]);
        assert!(matches!(panel.rows().first(), Some(Row::Tabs)));
    }

    #[test]
    fn history_mode_lists_commits_instead_of_changes() {
        let mut panel = with(vec![change("a.rs", 'M', false)]);
        panel.mode = Mode::History;
        panel.commits = vec![lithe_vcs::Commit {
            hash: "abc123def".into(),
            short: "abc123d".into(),
            summary: "do a thing".into(),
            author: "Someone".into(),
            age: "2 days ago".into(),
        }];
        let rows = panel.rows();
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Change(_))),
            "changes should not show in history"
        );
        let commit_row = rows
            .iter()
            .position(|r| matches!(r, Row::Commit(_)))
            .expect("a commit row");
        assert_eq!(panel.commit_at(commit_row).unwrap().short, "abc123d");
        assert!(panel.change_at(commit_row).is_none());
    }

    #[test]
    fn an_empty_history_says_so() {
        let mut panel = with(Vec::new());
        panel.mode = Mode::History;
        assert!(panel.rows().iter().any(|r| matches!(r, Row::Empty(_))));
    }

    #[test]
    fn only_the_sections_that_have_something_are_shown() {
        let panel = with(vec![change("a.rs", 'M', false)]);
        assert_eq!(
            headers(&panel),
            vec!["Changes (1)"],
            "no empty Staged heading"
        );

        let panel = with(vec![change("a.rs", 'A', true)]);
        assert_eq!(headers(&panel), vec!["Staged (1)"]);
        assert!(panel.has_staged());
    }

    #[test]
    fn staged_comes_before_changes_and_both_are_counted() {
        let panel = with(vec![
            change("a.rs", 'M', false),
            change("b.rs", 'A', true),
            change("c.rs", '?', false),
        ]);
        assert_eq!(headers(&panel), vec!["Staged (1)", "Changes (2)"]);
    }

    #[test]
    fn rows_map_back_to_the_change_they_show() {
        let panel = with(vec![
            change("staged.rs", 'A', true),
            change("edited.rs", 'M', false),
        ]);
        let rows = panel.rows();
        // Positions shift as the panel grows a mode switch and headers, so
        // find the rows rather than counting them.
        assert!(
            panel.change_at(0).is_none(),
            "the mode switch is not a file"
        );
        let staged_row = (0..rows.len())
            .find(|i| {
                panel
                    .change_at(*i)
                    .is_some_and(|c| c.display == "staged.rs")
            })
            .expect("the staged file should be reachable");
        assert!(panel.change_at(staged_row).unwrap().staged);
        // A blank and the next header sit between the two sections.
        let edited = (0..rows.len())
            .filter_map(|i| panel.change_at(i))
            .find(|c| c.display == "edited.rs");
        assert!(edited.is_some(), "the unstaged file should be reachable");
    }

    #[test]
    fn a_file_on_both_sides_appears_in_both_sections() {
        let panel = with(vec![
            change("both.rs", 'M', true),
            change("both.rs", 'M', false),
        ]);
        assert_eq!(headers(&panel), vec!["Staged (1)", "Changes (1)"]);
    }
}
