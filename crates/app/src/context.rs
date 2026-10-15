//! Right-click menus.
//!
//! Drawn by us rather than handed to the platform: the menu needs to know
//! what was clicked — a tree row, a symbol, a selection — and a native
//! popup would mean marshalling that through the menu bar's command table
//! for no gain.
//!
//! The entries are built per click from what is actually actionable, so
//! the menu never offers something that would quietly do nothing.

use std::path::PathBuf;

use crate::keys;
use crate::settings::PreviewKind;

/// What a menu entry does when chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Action {
    // --- explorer ---
    NewFile,
    NewFolder,
    Duplicate,
    Rename,
    Trash,
    Delete,
    StageFile,
    UnstageFile,
    DiscardFile,
    // --- diff ---
    /// Put the committed text back for one change, as an undoable edit
    /// in the buffer rather than through git.
    RevertHunk,
    /// Copy the text of the diff row the menu was opened on.
    CopyDiffLine,
    OpenChange,
    ShowDiff,
    CopyPath,
    CopyRelativePath,
    RevealInFinder,
    OpenInDefaultApp,
    OpenInTerminal,
    /// Cut, copy and paste of files, which is a different clipboard from
    /// the text one: the system clipboard holds the paths as text, and we
    /// hold what to do with them.
    CutFile,
    CopyFile,
    PasteFile,
    Undo,
    Redo,
    // --- editor ---
    GoTo(Where),
    Calls {
        incoming: bool,
    },
    FindReferences,
    RenameSymbol,
    FormatBuffer,
    FormatSelection,
    CodeActions,
    /// One of the code actions the server offered, by index.
    CodeAction(usize),
    Cut,
    Copy,
    CopyAndTrim,
    Paste,
    BlameLine,
    FileHistory,
    TogglePreview,
    CopyPermalink,
}

/// The four "go to" questions, which are one request with four names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Where {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
}

impl Where {
    /// The LSP method that answers it.
    pub fn method(self) -> &'static str {
        match self {
            Where::Definition => "textDocument/definition",
            Where::Declaration => "textDocument/declaration",
            Where::TypeDefinition => "textDocument/typeDefinition",
            Where::Implementation => "textDocument/implementation",
        }
    }

    /// What to say when the server has no answer.
    pub fn missing(self) -> &'static str {
        match self {
            Where::Definition => "no definition found",
            Where::Declaration => "no declaration found",
            Where::TypeDefinition => "no type definition found",
            Where::Implementation => "no implementation found",
        }
    }
}

/// Which of the four "go to" questions are worth offering.
///
/// A running server says which requests it answers, and this one is
/// commonly missing: in Go and Rust a thing is declared where it is
/// defined, so neither `gopls` nor `rust-analyzer` implements
/// `textDocument/declaration` at all. Asking returns nothing, which reads
/// as "there is no declaration" rather than as "this server does not do
/// that" — so the entry is not offered, which is the rule the rest of
/// this menu is built on.
#[derive(Clone, Copy, Debug)]
pub struct GoesTo {
    pub definition: bool,
    pub declaration: bool,
    pub type_definition: bool,
    pub implementation: bool,
}

impl GoesTo {
    /// All four — what to offer before a server has said otherwise,
    /// since asking is also what starts one.
    pub const ALL: Self = Self {
        definition: true,
        declaration: true,
        type_definition: true,
        implementation: true,
    };

    pub fn has(self, which: Where) -> bool {
        match which {
            Where::Definition => self.definition,
            Where::Declaration => self.declaration,
            Where::TypeDefinition => self.type_definition,
            Where::Implementation => self.implementation,
        }
    }
}

pub struct Entry {
    /// Owned, because a code-action menu's labels come from the server.
    pub label: String,
    /// Shown right-aligned; empty for entries with no shortcut.
    pub hint: &'static str,
    pub action: Option<Action>,
}

impl Entry {
    fn item(label: impl Into<String>, hint: &'static str, action: Action) -> Self {
        Self {
            label: label.into(),
            hint,
            action: Some(action),
        }
    }

    fn separator() -> Self {
        Self {
            label: String::new(),
            hint: "",
            action: None,
        }
    }

    pub fn is_separator(&self) -> bool {
        self.action.is_none()
    }
}

/// What the editor can do right now, for the entries that depend on it.
#[derive(Default)]
pub struct State {
    pub can_paste: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// What undo would reverse, named, so the entry says what it does.
    pub undo_label: String,
    pub redo_label: String,
}

/// An open right-click menu.
pub struct ContextMenu {
    pub entries: Vec<Entry>,
    /// Top-left corner, in physical pixels.
    pub at: (f32, f32),
    pub selected: usize,
    /// The path the menu was opened on, for the entries that need one.
    pub target: Option<PathBuf>,
    /// The diff row it was opened on, for the entries that act on one.
    pub row: Option<usize>,
}

impl ContextMenu {
    pub const MAX_COLUMNS: usize = 56;

    fn new(entries: Vec<Entry>, at: (f32, f32), target: Option<PathBuf>) -> Self {
        let selected = entries.iter().position(|e| !e.is_separator()).unwrap_or(0);
        Self {
            entries,
            at,
            selected,
            target,
            row: None,
        }
    }

    /// The menu for a row in the file tree.
    /// The menu for a file in the source control panel.
    ///
    /// Short on purpose: the panel is a list of changes, so the things
    /// worth offering are the ones that act on a change. Opening and
    /// revealing are there because a list of file names is also a way
    /// of getting to a file.
    pub fn for_change(at: (f32, f32), target: PathBuf, staged: bool) -> Self {
        let entries = vec![
            Entry::item("Open File", "", Action::OpenChange),
            Entry::item("Show Diff", "", Action::ShowDiff),
            Entry::separator(),
            if staged {
                Entry::item("Unstage", "", Action::UnstageFile)
            } else {
                Entry::item("Stage", "", Action::StageFile)
            },
            // Last, and after a separator, because it is the only one
            // here that destroys work and cannot be undone by us.
            Entry::separator(),
            Entry::item("Discard Changes…", "", Action::DiscardFile),
            Entry::separator(),
            Entry::item("Copy Path", "", Action::CopyPath),
            Entry::item("Copy Relative Path", "", Action::CopyRelativePath),
            Entry::item("Reveal in Finder", "", Action::RevealInFinder),
        ];
        Self::new(entries, at, Some(target))
    }

    /// The menu for a row in an open diff.
    ///
    /// A diff is a list of changes, so the entry that matters is the
    /// one that undoes the change under the pointer. It is offered only
    /// on a row that is part of one — a context line has nothing to
    /// revert, and an entry that would quietly do nothing is worse than
    /// no entry.
    ///
    /// Reverting goes through the buffer, not through git, so it joins
    /// the undo history: a change put back by mistake is one `cmd+Z`
    /// away. That is also why it needs no confirmation, unlike
    /// discarding the whole file, which does not.
    pub fn for_diff(at: (f32, f32), target: PathBuf, row: usize, revertible: bool) -> Self {
        let mut entries = Vec::new();
        if revertible {
            entries.extend([
                Entry::item("Revert This Change", "", Action::RevertHunk),
                Entry::separator(),
            ]);
        }
        entries.extend([
            Entry::item("Copy Line", "", Action::CopyDiffLine),
            Entry::separator(),
            Entry::item("Open File", "", Action::OpenChange),
            Entry::separator(),
            Entry::item("Discard All Changes…", "", Action::DiscardFile),
            Entry::separator(),
            Entry::item("Copy Path", "", Action::CopyPath),
            Entry::item("Copy Relative Path", "", Action::CopyRelativePath),
            Entry::item("Reveal in Finder", "", Action::RevealInFinder),
        ]);
        Self {
            row: Some(row),
            ..Self::new(entries, at, Some(target))
        }
    }

    pub fn for_explorer(
        at: (f32, f32),
        target: Option<PathBuf>,
        is_dir: bool,
        state: &State,
    ) -> Self {
        let mut entries = vec![
            Entry::item("New File", "", Action::NewFile),
            Entry::item("New Folder", "", Action::NewFolder),
        ];
        // The root listing has no item to act on, so most of the rest
        // would be a no-op — but pasting into it, and undoing what was
        // done to it, both still make sense.
        if target.is_some() {
            entries.extend([
                Entry::separator(),
                Entry::item(keys::REVEAL, "", Action::RevealInFinder),
                Entry::item("Open in Default App", "", Action::OpenInDefaultApp),
                Entry::item("Open in Terminal", "", Action::OpenInTerminal),
                Entry::separator(),
                Entry::item("Cut", "", Action::CutFile),
                Entry::item("Copy", "", Action::CopyFile),
                Entry::item("Duplicate", "", Action::Duplicate),
            ]);
            // Only the kinds that have a preview; offering one for a
            // .rs file would be an entry that does nothing.
            let previewable = target
                .as_deref()
                .is_some_and(|p| PreviewKind::of(p).is_some());
            if !is_dir && previewable {
                entries.insert(
                    3,
                    Entry::item("Open Preview", key!(" shift V"), Action::TogglePreview),
                );
            }
        }
        if state.can_paste {
            entries.push(Entry::item("Paste", "", Action::PasteFile));
        }
        if state.can_undo || state.can_redo {
            entries.push(Entry::separator());
            if state.can_undo {
                entries.push(Entry::item(
                    format!("Undo {}", state.undo_label),
                    "",
                    Action::Undo,
                ));
            }
            if state.can_redo {
                entries.push(Entry::item(
                    format!("Redo {}", state.redo_label),
                    "",
                    Action::Redo,
                ));
            }
        }
        if target.is_some() {
            entries.extend([
                Entry::separator(),
                Entry::item("Copy Path", "", Action::CopyPath),
                Entry::item("Copy Relative Path", "", Action::CopyRelativePath),
                Entry::separator(),
                Entry::item("Rename", "", Action::Rename),
                Entry::item("Move to Trash", "", Action::Trash),
                Entry::item("Delete", "", Action::Delete),
            ]);
        }
        Self::new(entries, at, target)
    }

    /// The menu for the text area.
    pub fn for_editor(
        at: (f32, f32),
        target: Option<PathBuf>,
        has_selection: bool,
        in_repo: bool,
        // None when this file has no preview; otherwise whether it is
        // showing, so the entry can say which way it goes.
        preview: Option<bool>,
        goes_to: GoesTo,
    ) -> Self {
        let mut entries: Vec<Entry> = [
            (Where::Definition, "Go to Definition", key!(" click")),
            (Where::Declaration, "Go to Declaration", ""),
            (Where::TypeDefinition, "Go to Type Definition", ""),
            (Where::Implementation, "Go to Implementation", ""),
        ]
        .into_iter()
        .filter(|(which, _, _)| goes_to.has(*which))
        .map(|(which, label, hint)| Entry::item(label, hint, Action::GoTo(which)))
        .collect();
        if !entries.is_empty() {
            entries.push(Entry::separator());
        }
        entries.extend([
            Entry::item("Find All References", "", Action::FindReferences),
            Entry::item("Show Incoming Calls", "", Action::Calls { incoming: true }),
            Entry::item("Show Outgoing Calls", "", Action::Calls { incoming: false }),
            Entry::separator(),
            Entry::item("Rename Symbol", "", Action::RenameSymbol),
            Entry::item("Format Buffer", "", Action::FormatBuffer),
        ]);
        if has_selection {
            entries.push(Entry::item(
                "Format Selections",
                "",
                Action::FormatSelection,
            ));
        }
        entries.push(Entry::item("Show Code Actions", "", Action::CodeActions));
        entries.push(Entry::separator());

        if has_selection {
            entries.push(Entry::item("Cut", key!(" X"), Action::Cut));
            entries.push(Entry::item("Copy", key!(" C"), Action::Copy));
            // Copies the selection with its common indentation removed,
            // which is what you want when lifting a block out of a
            // nested function and into a message.
            entries.push(Entry::item("Copy and Trim", "", Action::CopyAndTrim));
        }
        entries.push(Entry::item("Paste", key!(" V"), Action::Paste));

        if in_repo {
            entries.extend([
                Entry::separator(),
                Entry::item("Toggle Line Blame", key!(" shift B"), Action::BlameLine),
                Entry::item("View File History", "", Action::FileHistory),
                Entry::item("Copy Permalink to Line", "", Action::CopyPermalink),
            ]);
        }
        if target.is_some() {
            entries.push(Entry::separator());
            if let Some(showing) = preview {
                entries.push(Entry::item(
                    if showing {
                        "Close Preview"
                    } else {
                        "Open Preview"
                    },
                    key!(" shift V"),
                    Action::TogglePreview,
                ));
            }
            entries.extend([
                Entry::item(keys::REVEAL, "", Action::RevealInFinder),
                Entry::item("Open in Terminal", "", Action::OpenInTerminal),
            ]);
        }
        Self::new(entries, at, target)
    }

    /// The code actions a server offered, as a menu at the caret.
    pub fn for_code_actions(at: (f32, f32), titles: Vec<String>) -> Self {
        let entries = titles
            .into_iter()
            .enumerate()
            .map(|(index, title)| Entry::item(title, "", Action::CodeAction(index)))
            .collect();
        Self::new(entries, at, None)
    }

    /// Moves the highlight, stepping over separators.
    pub fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as i32;
        let mut index = self.selected as i32;
        for _ in 0..len {
            index = (index + delta).rem_euclid(len);
            if !self.entries[index as usize].is_separator() {
                self.selected = index as usize;
                return;
            }
        }
    }

    pub fn chosen(&self) -> Option<Action> {
        self.entries.get(self.selected)?.action
    }

    /// Width in cells, from the widest entry.
    ///
    /// A label from a server can be a sentence, so it is capped: the menu
    /// is a menu, not a document.
    pub fn columns(&self) -> usize {
        self.entries
            .iter()
            .map(|e| {
                e.label.chars().count()
                    + if e.hint.is_empty() {
                        0
                    } else {
                        e.hint.chars().count() + 3
                    }
            })
            .max()
            .unwrap_or(12)
            .min(Self::MAX_COLUMNS)
            + 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing cut, nothing to undo: the plainest menu there is.
    fn quiet() -> State {
        State::default()
    }

    #[test]
    fn the_root_listing_offers_only_what_it_can_do() {
        let menu = ContextMenu::for_explorer((0.0, 0.0), None, false, &quiet());
        let actions: Vec<_> = menu.entries.iter().filter_map(|e| e.action).collect();
        assert_eq!(actions, vec![Action::NewFile, Action::NewFolder]);
        assert!(
            !actions.contains(&Action::Rename),
            "there is nothing to rename when no row was clicked"
        );
    }

    #[test]
    fn a_folder_is_not_offered_a_preview() {
        let dir =
            ContextMenu::for_explorer((0.0, 0.0), Some(PathBuf::from("/p/src")), true, &quiet());
        let file =
            ContextMenu::for_explorer((0.0, 0.0), Some(PathBuf::from("/p/a.md")), false, &quiet());
        let has = |m: &ContextMenu| {
            m.entries
                .iter()
                .any(|e| e.action == Some(Action::TogglePreview))
        };
        assert!(!has(&dir));
        assert!(has(&file));
    }

    /// What a server does not do is not offered. Neither `gopls` nor
    /// `rust-analyzer` answers `textDocument/declaration`, and an entry
    /// that can only ever report "no declaration found" is worse than no
    /// entry at all.
    #[test]
    fn a_question_the_server_does_not_answer_is_not_offered() {
        let only_definitions = GoesTo {
            definition: true,
            declaration: false,
            type_definition: false,
            implementation: true,
        };
        let menu = ContextMenu::for_editor((0.0, 0.0), None, false, false, None, only_definitions);
        let labels: Vec<&str> = menu.entries.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"Go to Definition"), "{labels:?}");
        assert!(labels.contains(&"Go to Implementation"), "{labels:?}");
        assert!(!labels.contains(&"Go to Declaration"), "{labels:?}");
        assert!(!labels.contains(&"Go to Type Definition"), "{labels:?}");
        // The rest of the menu is still there, and still starts on
        // something choosable.
        assert!(labels.contains(&"Find All References"), "{labels:?}");
        assert!(!menu.entries[menu.selected].is_separator());
    }

    /// And with none of them, the menu does not open on a separator.
    #[test]
    fn a_server_that_goes_nowhere_leaves_the_rest_of_the_menu_intact() {
        let none = GoesTo {
            definition: false,
            declaration: false,
            type_definition: false,
            implementation: false,
        };
        let menu = ContextMenu::for_editor((0.0, 0.0), None, false, false, None, none);
        assert!(!menu.entries[menu.selected].is_separator());
        assert_eq!(
            menu.entries.first().map(|e| e.label.as_str()),
            Some("Find All References")
        );
    }

    #[test]
    fn cut_and_copy_appear_only_with_a_selection() {
        let without = ContextMenu::for_editor((0.0, 0.0), None, false, false, None, GoesTo::ALL);
        let with = ContextMenu::for_editor((0.0, 0.0), None, true, false, None, GoesTo::ALL);
        let has = |m: &ContextMenu, a: Action| m.entries.iter().any(|e| e.action == Some(a));
        assert!(!has(&without, Action::Cut));
        assert!(has(&with, Action::Cut));
        assert!(has(&without, Action::Paste), "paste needs no selection");
    }

    #[test]
    fn git_entries_appear_only_inside_a_repository() {
        let outside = ContextMenu::for_editor((0.0, 0.0), None, false, false, None, GoesTo::ALL);
        let inside = ContextMenu::for_editor((0.0, 0.0), None, false, true, None, GoesTo::ALL);
        let has = |m: &ContextMenu, a: Action| m.entries.iter().any(|e| e.action == Some(a));
        assert!(!has(&outside, Action::BlameLine));
        assert!(has(&inside, Action::BlameLine));
    }

    #[test]
    fn the_highlight_starts_on_something_choosable() {
        let menu = ContextMenu::for_editor((0.0, 0.0), None, false, false, None, GoesTo::ALL);
        assert!(!menu.entries[menu.selected].is_separator());
        assert!(menu.chosen().is_some());
    }

    #[test]
    fn moving_the_highlight_steps_over_separators_and_wraps() {
        let mut menu = ContextMenu::for_editor((0.0, 0.0), None, true, true, None, GoesTo::ALL);
        for _ in 0..menu.entries.len() * 2 {
            menu.move_selection(1);
            assert!(
                !menu.entries[menu.selected].is_separator(),
                "landed on a separator"
            );
        }
        for _ in 0..menu.entries.len() * 2 {
            menu.move_selection(-1);
            assert!(!menu.entries[menu.selected].is_separator());
        }
    }

    /// The editor menu was specified as a list; this is that list.
    /// A missing entry is a regression, not a judgement call.
    #[test]
    fn the_editor_menu_offers_everything_it_is_meant_to() {
        let menu = ContextMenu::for_editor(
            (0.0, 0.0),
            Some(PathBuf::from("/p/a.md")),
            true,
            true,
            Some(false),
            GoesTo::ALL,
        );
        let labels: Vec<&str> = menu.entries.iter().map(|e| e.label.as_str()).collect();
        for wanted in [
            "Go to Definition",
            "Go to Declaration",
            "Go to Type Definition",
            "Go to Implementation",
            "Find All References",
            "Show Incoming Calls",
            "Show Outgoing Calls",
            "Rename Symbol",
            "Format Buffer",
            "Format Selections",
            "Show Code Actions",
            "Cut",
            "Copy",
            "Copy and Trim",
            "Paste",
            keys::REVEAL,
            "Open Preview",
            "Open in Terminal",
            "Copy Permalink to Line",
            "View File History",
        ] {
            assert!(
                labels.contains(&wanted),
                "{wanted} is missing from {labels:?}"
            );
        }
    }

    /// Likewise for the explorer.
    #[test]
    fn the_explorer_menu_offers_everything_it_is_meant_to() {
        let state = State {
            can_paste: true,
            can_undo: true,
            can_redo: true,
            undo_label: "move".into(),
            redo_label: "trash".into(),
        };
        let menu =
            ContextMenu::for_explorer((0.0, 0.0), Some(PathBuf::from("/p/a.rs")), false, &state);
        let labels: Vec<&str> = menu.entries.iter().map(|e| e.label.as_str()).collect();
        for wanted in [
            "New File",
            "New Folder",
            keys::REVEAL,
            "Open in Default App",
            "Open in Terminal",
            "Cut",
            "Copy",
            "Duplicate",
            "Paste",
            "Undo move",
            "Redo trash",
            "Copy Path",
            "Copy Relative Path",
            "Rename",
            "Move to Trash",
            "Delete",
        ] {
            assert!(
                labels.contains(&wanted),
                "{wanted} is missing from {labels:?}"
            );
        }
    }

    #[test]
    fn what_cannot_be_done_is_not_offered() {
        let menu =
            ContextMenu::for_explorer((0.0, 0.0), Some(PathBuf::from("/p/a.rs")), false, &quiet());
        let has = |a: Action| menu.entries.iter().any(|e| e.action == Some(a));
        assert!(!has(Action::PasteFile), "nothing was cut or copied");
        assert!(!has(Action::Undo), "nothing has been done to undo");
        assert!(
            !has(Action::TogglePreview),
            "a .rs file has no preview to open"
        );
    }

    #[test]
    fn a_code_action_menu_is_built_from_what_the_server_said() {
        let menu = ContextMenu::for_code_actions((0.0, 0.0), vec!["Import Foo".into()]);
        assert_eq!(menu.chosen(), Some(Action::CodeAction(0)));
        assert_eq!(menu.entries[0].label, "Import Foo");
    }

    #[test]
    fn the_menu_is_wide_enough_for_its_widest_entry() {
        let menu =
            ContextMenu::for_explorer((0.0, 0.0), Some(PathBuf::from("/p/a")), false, &quiet());
        let widest = menu
            .entries
            .iter()
            .map(|e| e.label.chars().count() + e.hint.chars().count())
            .max()
            .unwrap();
        assert!(menu.columns() > widest, "labels would be clipped");
    }
}

#[cfg(test)]
mod change_menu_tests {
    use super::*;

    fn labels(menu: &ContextMenu) -> Vec<&str> {
        menu.entries
            .iter()
            .filter(|e| e.action.is_some())
            .map(|e| e.label.as_str())
            .collect()
    }

    #[test]
    fn a_changed_file_is_offered_what_acts_on_the_change() {
        let menu = ContextMenu::for_change((0.0, 0.0), PathBuf::from("a.rs"), false);
        let labels = labels(&menu);
        assert!(labels.contains(&"Stage"), "{labels:?}");
        assert!(!labels.contains(&"Unstage"), "it is not staged: {labels:?}");
        assert!(labels.contains(&"Discard Changes…"), "{labels:?}");
        assert!(labels.contains(&"Show Diff"), "{labels:?}");
    }

    #[test]
    fn a_staged_file_is_offered_the_other_half() {
        let menu = ContextMenu::for_change((0.0, 0.0), PathBuf::from("a.rs"), true);
        let labels = labels(&menu);
        assert!(labels.contains(&"Unstage"), "{labels:?}");
        assert!(
            !labels.contains(&"Stage"),
            "staging what is staged does nothing: {labels:?}"
        );
    }

    #[test]
    fn discarding_is_last_and_set_apart() {
        // It is the one entry here that destroys work git cannot give
        // back, so it must not sit under the pointer by accident.
        let menu = ContextMenu::for_change((0.0, 0.0), PathBuf::from("a.rs"), false);
        let at = menu
            .entries
            .iter()
            .position(|e| e.label == "Discard Changes…")
            .expect("a discard entry");
        assert!(
            menu.entries[at - 1].action.is_none(),
            "a separator should sit above it"
        );
        assert!(
            menu.entries[at].label.ends_with('…'),
            "the ellipsis says it will ask"
        );
    }

    #[test]
    fn the_menu_knows_which_file_it_is_about() {
        let menu = ContextMenu::for_change((0.0, 0.0), PathBuf::from("src/a.rs"), false);
        assert_eq!(
            menu.target.as_deref(),
            Some(std::path::Path::new("src/a.rs"))
        );
    }
}

#[cfg(test)]
mod diff_menu_tests {
    use super::*;
    use std::path::Path;

    fn menu(revertible: bool) -> ContextMenu {
        ContextMenu::for_diff((0.0, 0.0), PathBuf::from("/w/a.rs"), 4, revertible)
    }

    fn actions(menu: &ContextMenu) -> Vec<Action> {
        menu.entries.iter().filter_map(|e| e.action).collect()
    }

    #[test]
    fn a_change_can_be_reverted_and_a_context_line_cannot() {
        assert!(
            actions(&menu(true)).contains(&Action::RevertHunk),
            "a row that is part of a change offers to put it back"
        );
        assert!(
            !actions(&menu(false)).contains(&Action::RevertHunk),
            "a context line has nothing to revert, so the entry is not there"
        );
    }

    #[test]
    fn the_row_travels_with_the_menu() {
        // Choosing an entry closes the menu, so whatever acts on the
        // row has to read it from here rather than from the pointer.
        assert_eq!(menu(true).row, Some(4));
        assert_eq!(
            menu(true).target.as_deref(),
            Some(Path::new("/w/a.rs")),
            "the file is still what the whole-file entries act on"
        );
    }

    #[test]
    fn reverting_is_offered_before_discarding_the_lot() {
        let menu = menu(true);
        let list = actions(&menu);
        let revert = list.iter().position(|a| *a == Action::RevertHunk);
        let discard = list.iter().position(|a| *a == Action::DiscardFile);
        assert!(
            revert < discard,
            "the narrow, undoable entry comes before the wide, permanent one"
        );
    }

    #[test]
    fn the_menu_never_opens_on_a_separator() {
        for revertible in [true, false] {
            let menu = menu(revertible);
            assert!(
                !menu.entries[menu.selected].is_separator(),
                "revertible={revertible}"
            );
        }
    }
}
