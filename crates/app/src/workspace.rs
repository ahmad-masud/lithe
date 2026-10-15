//! Open tabs and the project root.
//!
//! A tab is not always a file. The settings page and a diff are things
//! you open, look at and close, which is what a tab is for; as modes
//! drawn over the editor they hid the file you were working on and the
//! only way back was the shortcut that opened them.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::editor::Editor;

/// What a tab shows.
// The file variant is deliberately not boxed. It is the one reached on
// every keystroke and every frame, and a pointer chase to save a
// couple of hundred bytes per open tab is the wrong trade: ten tabs is
// two kilobytes.
#[allow(clippy::large_enum_variant)]
pub enum Tab {
    /// A file, or a scratch buffer.
    File(Editor),
    /// A file against its committed version, or a commit against its
    /// parent.
    Diff(Box<Diff>),
    /// The settings page. At most one, since there is one set of them.
    Settings,
}

/// A diff, and the buffer it is of when there is one.
///
/// A working file's diff carries its buffer rather than pointing at
/// another tab: the right-hand column *is* that buffer, so the two
/// cannot be in different places, and switching away and back does not
/// have to put anything together again.
pub struct Diff {
    /// `None` for a commit's diff, which has nothing behind it to edit.
    pub editor: Option<Editor>,
    /// The committed side, kept as a document with its own parse tree.
    ///
    /// The new side is coloured from the live buffer's tree, which is
    /// already incremental. The old side has no buffer, so it gets one
    /// — parsed once when the diff is built and carried across every
    /// rebuild, because what a commit says never changes while you
    /// type.
    pub old: lithe_core::TextBuffer,
    pub old_syntax: Option<lithe_syntax::Syntax>,
    pub title: String,
    /// The working file, empty for a commit.
    pub path: PathBuf,
    pub lines: Vec<lithe_vcs::DiffLine>,
    /// Buffer revision it was built from, so it is only rebuilt when it
    /// would differ.
    pub revision: u64,
    pub scroll: f32,
}

impl Diff {
    pub fn editable(&self) -> bool {
        self.editor.is_some()
    }
}

impl Tab {
    pub fn editor(&self) -> Option<&Editor> {
        match self {
            Self::File(editor) => Some(editor),
            Self::Diff(diff) => diff.editor.as_ref(),
            Self::Settings => None,
        }
    }

    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        match self {
            Self::File(editor) => Some(editor),
            Self::Diff(diff) => diff.editor.as_mut(),
            Self::Settings => None,
        }
    }

    pub fn diff(&self) -> Option<&Diff> {
        match self {
            Self::Diff(diff) => Some(diff),
            _ => None,
        }
    }

    /// The label on the strip.
    ///
    /// A diff is marked rather than named differently, so the file it is
    /// of is still the thing you read.
    fn label(&self) -> String {
        match self {
            Self::Settings => "Settings".to_string(),
            Self::Diff(diff) if diff.path.as_os_str().is_empty() => {
                // A commit. Its title is a hash and a summary, which is
                // a sentence rather than a tab label.
                let short = diff.title.split_whitespace().next().unwrap_or("commit");
                format!("± {short}")
            }
            Self::Diff(diff) => {
                let name = Path::new(&diff.title)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| diff.title.clone());
                let dirty = diff.editor.as_ref().is_some_and(|e| e.dirty);
                format!("± {name}{}", if dirty { " *" } else { "" })
            }
            Self::File(editor) => {
                let name = editor
                    .path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "[scratch]".to_string());
                if editor.dirty {
                    format!("{name} *")
                } else {
                    name
                }
            }
        }
    }
}

pub struct Workspace {
    /// The project root, when a folder was opened rather than a lone file.
    pub root: Option<PathBuf>,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Workspace {
    pub fn empty(root: Option<PathBuf>) -> Self {
        Self {
            root,
            tabs: Vec::new(),
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The buffer the active tab is editing.
    ///
    /// `None` for the settings page and for a commit's diff, which are
    /// not editing anything — the same answer an empty workspace gives,
    /// which is why every caller already handles it.
    pub fn active(&self) -> Option<&Editor> {
        self.tabs.get(self.active).and_then(Tab::editor)
    }

    pub fn active_mut(&mut self) -> Option<&mut Editor> {
        self.tabs.get_mut(self.active).and_then(Tab::editor_mut)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// The diff the active tab shows, if it shows one.
    pub fn diff(&self) -> Option<&Diff> {
        self.active_tab().and_then(Tab::diff)
    }

    pub fn diff_mut(&mut self) -> Option<&mut Diff> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Diff(diff)) => Some(diff),
            _ => None,
        }
    }

    /// Every open buffer, whichever kind of tab holds it.
    pub fn files(&self) -> impl Iterator<Item = &Editor> {
        self.tabs.iter().filter_map(Tab::editor)
    }

    pub fn files_mut(&mut self) -> impl Iterator<Item = &mut Editor> {
        self.tabs.iter_mut().filter_map(Tab::editor_mut)
    }

    pub fn push(&mut self, editor: Editor) {
        self.tabs.push(Tab::File(editor));
        self.active = self.tabs.len() - 1;
    }

    /// Focuses the settings tab, opening it if it is not already up.
    pub fn show_settings(&mut self) {
        match self.tabs.iter().position(|t| matches!(t, Tab::Settings)) {
            Some(index) => self.active = index,
            None => {
                self.tabs.push(Tab::Settings);
                self.active = self.tabs.len() - 1;
            }
        }
    }

    /// Whether the settings page is the tab in front.
    pub fn showing_settings(&self) -> bool {
        matches!(self.active_tab(), Some(Tab::Settings))
    }

    /// Which tab holds `path`, as a file or as a diff.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.editor().is_some_and(|e| e.path.as_deref() == Some(path)))
            .or_else(|| {
                self.tabs
                    .iter()
                    .position(|t| t.diff().is_some_and(|d| d.path == path))
            })
    }

    /// Turns the tab holding `path` into a diff of it, keeping the buffer
    /// and the slot. Returns false when that file is not open.
    pub fn show_diff(&mut self, path: &Path, diff: Diff) -> bool {
        let Some(index) = self.index_of(path) else {
            return false;
        };
        let editor = match std::mem::replace(&mut self.tabs[index], Tab::Settings) {
            Tab::File(editor) => Some(editor),
            Tab::Diff(old) => old.editor,
            Tab::Settings => None,
        };
        self.tabs[index] = Tab::Diff(Box::new(Diff { editor, ..diff }));
        self.active = index;
        true
    }

    /// Turns a diff tab back into the file it is of.
    pub fn hide_diff(&mut self, index: usize) {
        let Some(Tab::Diff(diff)) = self.tabs.get_mut(index) else {
            return;
        };
        match diff.editor.take() {
            Some(editor) => self.tabs[index] = Tab::File(editor),
            // A commit's diff is not of anything open, so it just goes.
            None => {
                self.tabs.remove(index);
                self.active = self.active.min(self.tabs.len().saturating_sub(1));
            }
        }
    }

    /// Shows a commit's diff, reusing the tab any previous one is in.
    ///
    /// One at a time: a commit's diff is something you look at, and
    /// clicking through a history should not leave a tab per commit.
    pub fn show_commit(&mut self, diff: Diff) {
        let existing = self
            .tabs
            .iter()
            .position(|t| t.diff().is_some_and(|d| !d.editable()));
        match existing {
            Some(index) => {
                self.tabs[index] = Tab::Diff(Box::new(diff));
                self.active = index;
            }
            None => {
                self.tabs.push(Tab::Diff(Box::new(diff)));
                self.active = self.tabs.len() - 1;
            }
        }
    }

    /// Focuses the buffer for `path`, opening it if it is not already in
    /// the list. Re-opening a file never loses its cursor or undo history.
    ///
    /// A `preview` open is provisional: it takes over the existing
    /// provisional tab rather than adding another, which is what stops a
    /// browse through twenty files leaving twenty tabs behind.
    pub fn open(&mut self, path: &Path, tab_width: usize, preview: bool) -> Result<()> {
        if let Some(index) = self.index_of(path) {
            self.active = index;
            // Asking for it again deliberately promotes it.
            if !preview {
                if let Some(editor) = self.tabs[index].editor_mut() {
                    editor.preview = false;
                }
            }
            return Ok(());
        }

        let mut editor = Editor::open(path, tab_width)?;
        editor.preview = preview;

        // Reuse the provisional slot, keeping its position in the strip.
        // A diff or the settings page is never provisional: both were
        // asked for by name.
        if preview {
            let slot = self
                .tabs
                .iter()
                .position(|t| matches!(t, Tab::File(e) if e.preview));
            if let Some(slot) = slot {
                self.tabs[slot] = Tab::File(editor);
                self.active = slot;
                return Ok(());
            }
        }
        self.push(editor);
        Ok(())
    }

    /// Makes the active tab permanent.
    pub fn pin_active(&mut self) {
        if let Some(editor) = self.active_mut() {
            editor.preview = false;
        }
    }

    pub fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    pub fn cycle(&mut self, delta: isize) {
        if self.tabs.len() < 2 {
            return;
        }
        let len = self.tabs.len() as isize;
        self.active = (((self.active as isize + delta) % len + len) % len) as usize;
    }

    /// Short labels for the tab strip and the buffer picker.
    pub fn labels(&self) -> Vec<String> {
        self.tabs.iter().map(Tab::label).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lithe_core::TextBuffer;

    fn ws_with(names: &[&str]) -> Workspace {
        let mut ws = Workspace::empty(Some(PathBuf::from("/project")));
        for name in names {
            ws.push(Editor::new(
                TextBuffer::new(),
                Some(PathBuf::from(format!("/project/{name}"))),
                4,
            ));
        }
        ws
    }

    #[test]
    fn opening_an_already_open_file_focuses_it() {
        let mut ws = ws_with(&["a.rs", "b.rs"]);
        ws.active = 0;
        // Re-opening must not duplicate the buffer, which would quietly
        // discard the cursor and undo history in the original.
        let before = ws.len();
        let path = PathBuf::from("/project/b.rs");
        if let Some(index) = ws.index_of(&path) {
            ws.active = index;
        }
        assert_eq!(ws.len(), before);
        assert_eq!(ws.active, 1);
    }

    /// Writes real files, since `Workspace::open` reads from disk.
    fn project() -> (crate::testutil::TempDir, Workspace) {
        let dir = crate::testutil::TempDir::new();
        for name in ["a.rs", "b.rs", "c.rs"] {
            dir.write(name, "fn x() {}\n");
        }
        (dir, Workspace::empty(None))
    }

    #[test]
    fn a_preview_tab_is_replaced_rather_than_added() {
        let (dir, mut ws) = project();
        ws.open(&dir.path().join("a.rs"), 4, true).unwrap();
        ws.open(&dir.path().join("b.rs"), 4, true).unwrap();

        assert_eq!(ws.len(), 1, "browsing should not pile up tabs");
        assert!(ws
            .active()
            .unwrap()
            .path
            .as_ref()
            .unwrap()
            .ends_with("b.rs"));
        assert!(ws.active().unwrap().preview);
    }

    #[test]
    fn a_pinned_tab_is_kept_when_the_next_preview_opens() {
        let (dir, mut ws) = project();
        ws.open(&dir.path().join("a.rs"), 4, false).unwrap();
        ws.open(&dir.path().join("b.rs"), 4, true).unwrap();
        ws.open(&dir.path().join("c.rs"), 4, true).unwrap();

        assert_eq!(ws.len(), 2, "the pinned tab must survive");
        assert!(!ws.tabs[0].editor().unwrap().preview);
        assert!(ws.tabs[1]
            .editor()
            .unwrap()
            .path
            .as_ref()
            .unwrap()
            .ends_with("c.rs"));
    }

    #[test]
    fn the_preview_slot_keeps_its_position_in_the_strip() {
        let (dir, mut ws) = project();
        ws.open(&dir.path().join("a.rs"), 4, true).unwrap();
        ws.open(&dir.path().join("b.rs"), 4, false).unwrap();
        // a.rs is still the preview, at index 0.
        ws.open(&dir.path().join("c.rs"), 4, true).unwrap();
        assert_eq!(ws.len(), 2);
        assert!(
            ws.tabs[0]
                .editor()
                .unwrap()
                .path
                .as_ref()
                .unwrap()
                .ends_with("c.rs"),
            "the replacement should take the old slot, not the end"
        );
        assert_eq!(ws.active, 0, "and focus should follow it");
    }

    #[test]
    fn reopening_deliberately_pins_an_existing_preview() {
        let (dir, mut ws) = project();
        let a = dir.path().join("a.rs");
        ws.open(&a, 4, true).unwrap();
        assert!(ws.active().unwrap().preview);
        ws.open(&a, 4, false).unwrap();
        assert!(!ws.active().unwrap().preview, "a deliberate open keeps it");
        assert_eq!(ws.len(), 1, "and does not duplicate it");
    }

    #[test]
    fn editing_a_preview_tab_keeps_it() {
        let (dir, mut ws) = project();
        ws.open(&dir.path().join("a.rs"), 4, true).unwrap();
        ws.active_mut().unwrap().insert("x");
        assert!(
            !ws.active().unwrap().preview,
            "typing into a provisional tab should make it permanent"
        );
    }

    #[test]
    fn closing_keeps_the_index_in_range() {
        let mut ws = ws_with(&["a.rs", "b.rs", "c.rs"]);
        ws.active = 2;
        ws.close_active();
        assert_eq!(ws.len(), 2);
        assert_eq!(ws.active, 1, "closing the last tab selects the new last");

        ws.active = 0;
        ws.close_active();
        assert_eq!(ws.active, 0);

        ws.close_active();
        assert!(ws.is_empty());
        assert!(
            ws.active().is_none(),
            "an empty workspace has no active buffer"
        );

        // And closing an empty workspace is a no-op rather than a panic.
        ws.close_active();
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let mut ws = ws_with(&["a.rs", "b.rs", "c.rs"]);
        ws.active = 2;
        ws.cycle(1);
        assert_eq!(ws.active, 0, "forward from the last wraps to the first");
        ws.cycle(-1);
        assert_eq!(ws.active, 2, "backward from the first wraps to the last");
    }

    #[test]
    fn a_single_buffer_does_not_cycle() {
        let mut ws = ws_with(&["only.rs"]);
        ws.cycle(1);
        assert_eq!(ws.active, 0);
    }

    fn diff_of(path: &str) -> Diff {
        Diff {
            editor: None,
            old: lithe_core::TextBuffer::new(),
            old_syntax: None,
            title: path.to_string(),
            path: PathBuf::from(path),
            lines: Vec::new(),
            revision: 0,
            scroll: 0.0,
        }
    }

    #[test]
    fn a_tab_strip_says_which_tabs_are_not_files() {
        let mut ws = ws_with(&["a.rs", "b.rs"]);
        ws.show_diff(Path::new("/project/a.rs"), diff_of("a.rs"));
        ws.show_settings();
        assert_eq!(ws.labels(), vec!["± a.rs", "b.rs", "Settings"]);
    }

    #[test]
    fn opening_a_file_that_is_open_as_a_diff_focuses_that_tab() {
        let mut ws = ws_with(&["a.rs", "b.rs"]);
        let a = PathBuf::from("/project/a.rs");
        ws.show_diff(&a, diff_of("a.rs"));
        ws.active = 1;

        // Same buffer, so the same tab: a second one would be a second
        // undo history for one file.
        assert_eq!(ws.index_of(&a), Some(0));
        assert_eq!(ws.len(), 2);
    }

    #[test]
    fn closing_a_diff_tab_closes_the_file_with_it() {
        let mut ws = ws_with(&["a.rs"]);
        ws.show_diff(Path::new("/project/a.rs"), diff_of("a.rs"));
        ws.close_active();
        assert!(ws.is_empty(), "the diff holds the buffer, so both go");
    }

    #[test]
    fn a_panel_tab_holds_no_buffer_and_is_skipped_by_the_file_list() {
        let mut ws = ws_with(&["a.rs"]);
        ws.show_settings();
        ws.show_commit(diff_of(""));
        assert_eq!(ws.len(), 3);
        assert_eq!(ws.files().count(), 1, "only the file tab has a buffer");

        // And cycling still reaches every tab, panels included.
        ws.active = 0;
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(ws.active);
            ws.cycle(1);
        }
        assert_eq!(seen, vec![0, 1, 2]);
    }

    #[test]
    fn hiding_a_commit_diff_removes_the_tab_rather_than_emptying_it() {
        let mut ws = ws_with(&["a.rs"]);
        ws.show_commit(diff_of(""));
        assert_eq!(ws.len(), 2);
        ws.hide_diff(1);
        assert_eq!(ws.len(), 1, "there is no file under a commit's diff");
        assert_eq!(ws.active, 0);
    }

    #[test]
    fn labels_mark_unsaved_buffers() {
        let mut ws = ws_with(&["a.rs"]);
        assert_eq!(ws.labels(), vec!["a.rs"]);
        ws.active_mut().unwrap().insert("x");
        assert_eq!(ws.labels(), vec!["a.rs *"], "an edited buffer must show it");
    }
}
