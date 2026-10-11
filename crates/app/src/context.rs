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
        }
    }

    /// The menu for a row in the file tree.
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
                Entry::item("Reveal in Finder", "", Action::RevealInFinder),
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
                    Entry::item("Open Preview", "cmd shift V", Action::TogglePreview),
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
    ) -> Self {
        let mut entries = vec![
            Entry::item(
                "Go to Definition",
                "cmd click",
                Action::GoTo(Where::Definition),
            ),
            Entry::item("Go to Declaration", "", Action::GoTo(Where::Declaration)),
            Entry::item(
                "Go to Type Definition",
                "",
                Action::GoTo(Where::TypeDefinition),
            ),
            Entry::item(
                "Go to Implementation",
                "",
                Action::GoTo(Where::Implementation),
            ),
            Entry::separator(),
            Entry::item("Find All References", "", Action::FindReferences),
            Entry::item("Show Incoming Calls", "", Action::Calls { incoming: true }),
            Entry::item("Show Outgoing Calls", "", Action::Calls { incoming: false }),
            Entry::separator(),
            Entry::item("Rename Symbol", "", Action::RenameSymbol),
            Entry::item("Format Buffer", "", Action::FormatBuffer),
        ];
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
            entries.push(Entry::item("Cut", "cmd X", Action::Cut));
            entries.push(Entry::item("Copy", "cmd C", Action::Copy));
            // Copies the selection with its common indentation removed,
            // which is what you want when lifting a block out of a
            // nested function and into a message.
            entries.push(Entry::item("Copy and Trim", "", Action::CopyAndTrim));
        }
        entries.push(Entry::item("Paste", "cmd V", Action::Paste));

        if in_repo {
            entries.extend([
                Entry::separator(),
                Entry::item("Toggle Line Blame", "cmd shift B", Action::BlameLine),
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
                    "cmd shift V",
                    Action::TogglePreview,
                ));
            }
            entries.extend([
                Entry::item("Reveal in Finder", "", Action::RevealInFinder),
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

    #[test]
    fn cut_and_copy_appear_only_with_a_selection() {
        let without = ContextMenu::for_editor((0.0, 0.0), None, false, false, None);
        let with = ContextMenu::for_editor((0.0, 0.0), None, true, false, None);
        let has = |m: &ContextMenu, a: Action| m.entries.iter().any(|e| e.action == Some(a));
        assert!(!has(&without, Action::Cut));
        assert!(has(&with, Action::Cut));
        assert!(has(&without, Action::Paste), "paste needs no selection");
    }

    #[test]
    fn git_entries_appear_only_inside_a_repository() {
        let outside = ContextMenu::for_editor((0.0, 0.0), None, false, false, None);
        let inside = ContextMenu::for_editor((0.0, 0.0), None, false, true, None);
        let has = |m: &ContextMenu, a: Action| m.entries.iter().any(|e| e.action == Some(a));
        assert!(!has(&outside, Action::BlameLine));
        assert!(has(&inside, Action::BlameLine));
    }

    #[test]
    fn the_highlight_starts_on_something_choosable() {
        let menu = ContextMenu::for_editor((0.0, 0.0), None, false, false, None);
        assert!(!menu.entries[menu.selected].is_separator());
        assert!(menu.chosen().is_some());
    }

    #[test]
    fn moving_the_highlight_steps_over_separators_and_wraps() {
        let mut menu = ContextMenu::for_editor((0.0, 0.0), None, true, true, None);
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
            "Reveal in Finder",
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
            "Reveal in Finder",
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
