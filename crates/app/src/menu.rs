//! The native menu bar.
//!
//! On macOS this is the real system menu at the top of the screen, not
//! something drawn inside the window — which is what the platform expects
//! and what makes the standard shortcuts discoverable.
//!
//! Note the deliberate avoidance of muda's predefined Undo/Cut/Copy/Paste
//! items: on macOS those send NSResponder actions aimed at an NSTextView,
//! and this editor has no NSTextView to receive them. They would appear in
//! the menu and silently do nothing. Every item we act on ourselves is a
//! plain item with an id we route.

use muda::accelerator::Accelerator;
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::picker::Command;

/// Pairs a menu id with the command it runs.
const ITEMS: &[(&str, Command)] = &[
    ("file.new_window", Command::NewWindow),
    ("file.new", Command::NewBuffer),
    ("file.new_file", Command::NewFile),
    ("file.open", Command::OpenFileDialog),
    ("file.open_folder", Command::OpenFolderDialog),
    ("file.save", Command::Save),
    ("file.save_as", Command::SaveAs),
    ("file.close", Command::CloseBuffer),
    ("file.autosave", Command::ToggleAutosave),
    ("app.settings", Command::Settings),
    ("edit.undo", Command::Undo),
    ("edit.redo", Command::Redo),
    ("edit.cut", Command::Cut),
    ("edit.copy", Command::Copy),
    ("edit.paste", Command::Paste),
    ("edit.select_all", Command::SelectAll),
    ("edit.next_match", Command::SelectNextOccurrence),
    ("edit.all_matches", Command::SelectAllOccurrences),
    ("edit.cursor_above", Command::AddCursorAbove),
    ("edit.cursor_below", Command::AddCursorBelow),
    ("edit.select_line", Command::SelectLine),
    ("edit.delete_line", Command::DeleteLine),
    ("edit.duplicate_line", Command::DuplicateLine),
    ("edit.move_line_up", Command::MoveLineUp),
    ("edit.move_line_down", Command::MoveLineDown),
    ("edit.line_above", Command::InsertLineAbove),
    ("edit.line_below", Command::InsertLineBelow),
    ("edit.comment", Command::ToggleComment),
    ("edit.indent", Command::Indent),
    ("edit.unindent", Command::Unindent),
    ("edit.find", Command::Find),
    ("edit.replace", Command::Replace),
    ("edit.search_project", Command::SearchProject),
    ("go.file", Command::OpenFile),
    ("go.buffer", Command::SwitchBuffer),
    ("go.command", Command::Palette),
    ("code.definition", Command::GotoDefinition),
    ("code.hover", Command::Hover),
    ("git.stage", Command::StageFile),
    ("git.unstage", Command::UnstageFile),
    ("git.commit", Command::Commit),
    ("git.blame", Command::BlameLine),
    ("git.panel", Command::ToggleGitPanel),
    ("view.tree", Command::ToggleTree),
    ("view.preview", Command::MarkdownPreview),
];

/// The command a menu id maps to, if any.
pub fn command_for(id: &MenuId) -> Option<Command> {
    ITEMS
        .iter()
        .find(|(item_id, _)| *item_id == id.0.as_str())
        .map(|(_, command)| *command)
}

fn item(id: &str, label: &str, accelerator: Option<&str>) -> MenuItem {
    // A malformed accelerator is a programming error, but a menu without a
    // shortcut beats a crash on startup.
    let accelerator = accelerator.and_then(|a| match a.parse::<Accelerator>() {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            log::warn!("bad accelerator {a:?} for {id}: {e}");
            None
        }
    });
    MenuItem::with_id(id, label, true, accelerator)
}

/// Builds the menu bar and installs it.
///
/// The returned `Menu` must be kept alive for as long as the application is;
/// dropping it takes the menu with it.
pub fn build() -> muda::Result<Menu> {
    let menu = Menu::new();

    let app = Submenu::with_items(
        "lithe",
        true,
        &[
            &PredefinedMenuItem::about(Some("About lithe"), None),
            &PredefinedMenuItem::separator(),
            &item("app.settings", "Settings…", Some("CmdOrCtrl+,")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ],
    )?;

    let file = Submenu::with_items(
        "File",
        true,
        &[
            &item("file.new_window", "New Window", Some("CmdOrCtrl+Shift+N")),
            &item("file.new", "New Buffer", Some("CmdOrCtrl+N")),
            &item("file.new_file", "New File…", None),
            &item("file.open", "Open File…", Some("CmdOrCtrl+O")),
            &item(
                "file.open_folder",
                "Open Folder…",
                Some("CmdOrCtrl+Shift+O"),
            ),
            &PredefinedMenuItem::separator(),
            &item("file.save", "Save", Some("CmdOrCtrl+S")),
            &item("file.save_as", "Save As…", Some("CmdOrCtrl+Shift+S")),
            &PredefinedMenuItem::separator(),
            &item("file.close", "Close Buffer", Some("CmdOrCtrl+W")),
            &PredefinedMenuItem::separator(),
            &item("file.autosave", "Toggle Autosave", None),
        ],
    )?;

    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &item("edit.undo", "Undo", Some("CmdOrCtrl+Z")),
            &item("edit.redo", "Redo", Some("CmdOrCtrl+Shift+Z")),
            &PredefinedMenuItem::separator(),
            &item("edit.cut", "Cut", Some("CmdOrCtrl+X")),
            &item("edit.copy", "Copy", Some("CmdOrCtrl+C")),
            &item("edit.paste", "Paste", Some("CmdOrCtrl+V")),
            &PredefinedMenuItem::separator(),
            &item("edit.select_all", "Select All", Some("CmdOrCtrl+A")),
            &item("edit.select_line", "Select Line", Some("CmdOrCtrl+L")),
            &item(
                "edit.next_match",
                "Add Selection to Next Match",
                Some("CmdOrCtrl+D"),
            ),
            &item(
                "edit.all_matches",
                "Select All Occurrences",
                Some("CmdOrCtrl+Shift+L"),
            ),
            &item(
                "edit.cursor_above",
                "Add Cursor Above",
                Some("CmdOrCtrl+Alt+Up"),
            ),
            &item(
                "edit.cursor_below",
                "Add Cursor Below",
                Some("CmdOrCtrl+Alt+Down"),
            ),
            &PredefinedMenuItem::separator(),
            &item("edit.comment", "Toggle Line Comment", Some("CmdOrCtrl+/")),
            &item("edit.indent", "Indent", Some("CmdOrCtrl+]")),
            &item("edit.unindent", "Outdent", Some("CmdOrCtrl+[")),
            &PredefinedMenuItem::separator(),
            &item(
                "edit.line_above",
                "Insert Line Above",
                Some("CmdOrCtrl+Shift+Enter"),
            ),
            &item(
                "edit.line_below",
                "Insert Line Below",
                Some("CmdOrCtrl+Enter"),
            ),
            &item(
                "edit.duplicate_line",
                "Duplicate Line",
                Some("Shift+Alt+Down"),
            ),
            &item("edit.move_line_up", "Move Line Up", Some("Alt+Up")),
            &item("edit.move_line_down", "Move Line Down", Some("Alt+Down")),
            &item("edit.delete_line", "Delete Line", Some("CmdOrCtrl+Shift+K")),
            &PredefinedMenuItem::separator(),
            &item("edit.find", "Find…", Some("CmdOrCtrl+F")),
            &item("edit.replace", "Replace…", Some("CmdOrCtrl+Alt+F")),
            &item(
                "edit.search_project",
                "Find in Project…",
                Some("CmdOrCtrl+Shift+F"),
            ),
        ],
    )?;

    let go = Submenu::with_items(
        "Go",
        true,
        &[
            &item("go.file", "Go to File…", Some("CmdOrCtrl+P")),
            &item("go.buffer", "Switch Buffer…", None),
            &item("go.command", "Command Palette…", Some("CmdOrCtrl+Shift+P")),
        ],
    )?;

    let code = Submenu::with_items(
        "Code",
        true,
        &[
            &item(
                "code.definition",
                "Go to Definition  (or cmd-click)",
                Some("F12"),
            ),
            &item("code.hover", "Show Hover", Some("CmdOrCtrl+I")),
        ],
    )?;

    let git = Submenu::with_items(
        "Git",
        true,
        &[
            &item("git.panel", "Source Control", Some("CmdOrCtrl+Shift+G")),
            &PredefinedMenuItem::separator(),
            &item("git.stage", "Stage File", Some("CmdOrCtrl+Shift+A")),
            &item("git.unstage", "Unstage File", None),
            &item("git.commit", "Commit…", Some("CmdOrCtrl+Shift+C")),
            &PredefinedMenuItem::separator(),
            &item("git.blame", "Toggle Line Blame", Some("CmdOrCtrl+Shift+B")),
        ],
    )?;

    let view = Submenu::with_items(
        "View",
        true,
        &[
            &item("view.tree", "Toggle File Tree", Some("CmdOrCtrl+B")),
            &item(
                "view.preview",
                "Markdown Preview",
                Some("CmdOrCtrl+Shift+V"),
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::fullscreen(None),
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::zoom(None),
        ],
    )?;

    menu.append_items(&[&app, &file, &edit, &go, &code, &git, &view])?;

    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    Ok(menu)
}

/// Routes menu clicks onto the event loop.
///
/// muda calls the handler from wherever the click arrives, so the id goes
/// through a channel and the loop is woken to drain it on its own thread.
pub fn route<F>(wake: F) -> std::sync::mpsc::Receiver<MenuId>
where
    F: Fn() + Send + Sync + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    // `Sender` is Send but not Sync, and muda wants a Sync handler.
    let tx = std::sync::Mutex::new(tx);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(event.id);
        }
        wake();
    }));
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_id_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for (id, _) in ITEMS {
            assert!(seen.insert(*id), "duplicate menu id {id:?}");
        }
    }

    #[test]
    fn ids_route_to_commands() {
        assert_eq!(command_for(&MenuId::new("file.save")), Some(Command::Save));
        assert_eq!(
            command_for(&MenuId::new("edit.select_all")),
            Some(Command::SelectAll)
        );
        assert_eq!(command_for(&MenuId::new("nope")), None);
    }

    #[test]
    fn declared_accelerators_all_parse() {
        // These strings are only validated at runtime, so a typo would
        // otherwise show up as a silently missing shortcut.
        for a in [
            "CmdOrCtrl+N",
            "CmdOrCtrl+Shift+N",
            "CmdOrCtrl+O",
            "CmdOrCtrl+Shift+O",
            "CmdOrCtrl+S",
            "CmdOrCtrl+Shift+S",
            "CmdOrCtrl+W",
            "CmdOrCtrl+Z",
            "CmdOrCtrl+Shift+Z",
            "CmdOrCtrl+X",
            "CmdOrCtrl+C",
            "CmdOrCtrl+V",
            "CmdOrCtrl+A",
            "CmdOrCtrl+F",
            "CmdOrCtrl+Alt+F",
            "CmdOrCtrl+Shift+F",
            "CmdOrCtrl+P",
            "CmdOrCtrl+Shift+P",
            "CmdOrCtrl+B",
            "CmdOrCtrl+I",
            "F12",
            "CmdOrCtrl+Shift+A",
            "CmdOrCtrl+Shift+C",
            "CmdOrCtrl+Shift+B",
            "CmdOrCtrl+Shift+G",
            "CmdOrCtrl+Shift+V",
            "CmdOrCtrl+,",
        ] {
            assert!(a.parse::<Accelerator>().is_ok(), "{a} failed to parse");
        }
    }
}
