//! The overlay picker: fuzzy file open, buffer switch, command palette.
//!
//! Matching runs on nucleo's worker pool, and the file walk runs on its own
//! thread feeding nucleo's injector, so neither blocks a keystroke no matter
//! how large the repository is. Results stream in while you type.
//!
//! The walk only ever collects *paths*, never file contents, and it only
//! happens when the picker is opened — invariant 3 and 6 respectively.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};

/// What choosing an entry does.
#[derive(Clone, Debug)]
pub enum Action {
    OpenPath(PathBuf),
    SwitchBuffer(usize),
    Run(Command),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewWindow,
    NewBuffer,
    NewFile,
    OpenFileDialog,
    OpenFolderDialog,
    Save,
    SaveAs,
    CloseBuffer,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    SelectNextOccurrence,
    SelectAllOccurrences,
    AddCursorAbove,
    AddCursorBelow,
    SelectLine,
    GotoLine,
    DeleteLine,
    DuplicateLine,
    MoveLineUp,
    MoveLineDown,
    InsertLineAbove,
    InsertLineBelow,
    ToggleComment,
    Indent,
    Unindent,
    Find,
    Replace,
    SearchProject,
    GotoDefinition,
    Hover,
    StageFile,
    UnstageFile,
    Commit,
    BlameLine,
    ToggleGitPanel,
    MarkdownPreview,
    ToggleAutosave,
    Settings,
    ToggleTree,
    OpenFile,
    SwitchBuffer,
    Palette,
    Quit,
}

impl Command {
    /// What the palette offers. `CommandPalette` is absent on purpose:
    /// offering it from inside itself is noise.
    pub const ALL: &'static [(Command, &'static str)] = &[
        (Command::OpenFile, "Go to file…"),
        (Command::SwitchBuffer, "Switch buffer…"),
        (Command::NewWindow, "New window"),
        (Command::NewBuffer, "New buffer"),
        (Command::NewFile, "New file…"),
        (Command::OpenFileDialog, "Open file…"),
        (Command::OpenFolderDialog, "Open folder…"),
        (Command::Save, "Save"),
        (Command::SaveAs, "Save as…"),
        (Command::CloseBuffer, "Close buffer"),
        (Command::Undo, "Undo"),
        (Command::Redo, "Redo"),
        (Command::Cut, "Cut"),
        (Command::Copy, "Copy"),
        (Command::Paste, "Paste"),
        (Command::SelectAll, "Select all"),
        (Command::SelectNextOccurrence, "Add selection to next match"),
        (Command::SelectAllOccurrences, "Select all occurrences"),
        (Command::AddCursorAbove, "Add cursor above"),
        (Command::AddCursorBelow, "Add cursor below"),
        (Command::SelectLine, "Select line"),
        (Command::GotoLine, "Go to line…"),
        (Command::DeleteLine, "Delete line"),
        (Command::DuplicateLine, "Duplicate line"),
        (Command::MoveLineUp, "Move line up"),
        (Command::MoveLineDown, "Move line down"),
        (Command::InsertLineAbove, "Insert line above"),
        (Command::InsertLineBelow, "Insert line below"),
        (Command::ToggleComment, "Toggle line comment"),
        (Command::Indent, "Indent lines"),
        (Command::Unindent, "Outdent lines"),
        (Command::Find, "Find…"),
        (Command::Replace, "Replace…"),
        (Command::SearchProject, "Search in project…"),
        (Command::GotoDefinition, "Go to definition"),
        (Command::Hover, "Show hover information"),
        (Command::StageFile, "Git: stage this file"),
        (Command::UnstageFile, "Git: unstage this file"),
        (Command::Commit, "Git: commit…"),
        (Command::BlameLine, "Git: toggle line blame"),
        (Command::ToggleGitPanel, "Git: show source control"),
        (Command::MarkdownPreview, "Toggle preview for this file"),
        (Command::ToggleAutosave, "Toggle autosave"),
        (Command::Settings, "Settings"),
        (Command::ToggleTree, "Toggle file tree"),
        (Command::Quit, "Quit"),
    ];
}

pub struct Entry {
    pub label: String,
    pub action: Action,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Files,
    Buffers,
    Commands,
}

impl Kind {
    pub fn prompt(self) -> &'static str {
        match self {
            Kind::Files => "open file",
            Kind::Buffers => "switch buffer",
            Kind::Commands => "command",
        }
    }
}

pub struct Picker {
    pub kind: Kind,
    pub query: String,
    pub selected: u32,
    engine: Nucleo<Entry>,
}

impl Picker {
    fn engine(notify: Arc<dyn Fn() + Send + Sync>, paths: bool) -> Nucleo<Entry> {
        let mut config = Config::DEFAULT;
        if paths {
            // Weights path separators as boundaries, so "cgr" finds
            // crates/gpu/renderer.rs the way you would expect.
            config.set_match_paths();
        }
        Nucleo::new(config, notify, None, 1)
    }

    /// Fuzzy file open. The walk runs on its own thread and streams into the
    /// matcher, so the overlay is usable on the first frame.
    pub fn files(root: &Path, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let engine = Self::engine(notify, true);
        let injector = engine.injector();
        let root = root.to_path_buf();

        std::thread::Builder::new()
            .name("lithe-walk".into())
            .spawn(move || {
                let walker = ignore::WalkBuilder::new(&root)
                    .hidden(true)
                    .git_ignore(true)
                    .git_global(true)
                    .parents(true)
                    // Matches the file tree; see the note there.
                    .require_git(false)
                    .build();
                for entry in walker.filter_map(Result::ok) {
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        continue;
                    }
                    let path = entry.path();
                    let label = path
                        .strip_prefix(&root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned();
                    injector.push(
                        Entry {
                            label,
                            action: Action::OpenPath(path.to_path_buf()),
                        },
                        |entry, columns| columns[0] = entry.label.as_str().into(),
                    );
                }
            })
            .ok();

        Self::wrap(Kind::Files, engine)
    }

    pub fn buffers(labels: Vec<String>, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let engine = Self::engine(notify, false);
        let injector = engine.injector();
        for (index, label) in labels.into_iter().enumerate() {
            injector.push(
                Entry {
                    label,
                    action: Action::SwitchBuffer(index),
                },
                |entry, columns| columns[0] = entry.label.as_str().into(),
            );
        }
        Self::wrap(Kind::Buffers, engine)
    }

    pub fn commands(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let engine = Self::engine(notify, false);
        let injector = engine.injector();
        for (command, label) in Command::ALL {
            injector.push(
                Entry {
                    label: (*label).to_string(),
                    action: Action::Run(*command),
                },
                |entry, columns| columns[0] = entry.label.as_str().into(),
            );
        }
        Self::wrap(Kind::Commands, engine)
    }

    fn wrap(kind: Kind, mut engine: Nucleo<Entry>) -> Self {
        // An empty pattern matches everything, so the list is populated
        // before a single key is pressed.
        engine
            .pattern
            .reparse(0, "", CaseMatching::Smart, Normalization::Smart, false);
        Self {
            kind,
            query: String::new(),
            selected: 0,
            engine,
        }
    }

    pub fn set_query(&mut self, query: String) {
        let append = query.starts_with(&self.query) && !self.query.is_empty();
        self.engine
            .pattern
            .reparse(0, &query, CaseMatching::Smart, Normalization::Smart, append);
        self.query = query;
        self.selected = 0;
    }

    /// Advances matching. Returns true when the visible result set changed
    /// and the overlay needs redrawing.
    pub fn tick(&mut self) -> bool {
        self.engine.tick(10).changed
    }

    pub fn matched(&self) -> u32 {
        self.engine.snapshot().matched_item_count()
    }

    pub fn total(&self) -> u32 {
        self.engine.snapshot().item_count()
    }

    pub fn label(&self, index: u32) -> Option<&str> {
        Some(
            self.engine
                .snapshot()
                .get_matched_item(index)?
                .data
                .label
                .as_str(),
        )
    }

    pub fn chosen(&self) -> Option<Action> {
        Some(
            self.engine
                .snapshot()
                .get_matched_item(self.selected)?
                .data
                .action
                .clone(),
        )
    }

    pub fn move_selection(&mut self, delta: i32) {
        let matched = self.matched();
        if matched == 0 {
            self.selected = 0;
            return;
        }
        let last = matched - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs()),
            d => (self.selected + d as u32).min(last),
        };
    }

    /// Keeps the selection inside the result set as matches stream in.
    pub fn clamp(&mut self) {
        let matched = self.matched();
        if matched == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(matched - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn silent() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    /// Matching and the file walk are both asynchronous, and `tick` returns
    /// immediately rather than blocking, so spinning on it proves nothing.
    /// Wait until the injected count and the match count both stop moving.
    fn settle(picker: &mut Picker) {
        let mut last = (u32::MAX, u32::MAX);
        let mut stable = 0;
        for _ in 0..500 {
            picker.tick();
            let now = (picker.total(), picker.matched());
            if now == last {
                stable += 1;
            } else {
                stable = 0;
                last = now;
            }
            if stable >= 10 && now.0 > 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn an_empty_query_lists_everything() {
        let mut picker = Picker::commands(silent());
        settle(&mut picker);
        assert_eq!(
            picker.matched() as usize,
            Command::ALL.len(),
            "every command should be offered before any typing"
        );
    }

    #[test]
    fn fuzzy_query_narrows_and_ranks() {
        let mut picker = Picker::commands(silent());
        settle(&mut picker);

        // Non-contiguous letters match, and narrow the set.
        let everything = picker.matched();
        picker.set_query("tgl".into());
        settle(&mut picker);
        assert!(picker.matched() >= 1, "non-contiguous letters should match");
        assert!(picker.matched() < everything, "and should narrow it");

        // A query only one command can satisfy ranks that one first.
        // Asserting a winner for an ambiguous query just breaks whenever a
        // command is added.
        picker.set_query("filetree".into());
        settle(&mut picker);
        assert_eq!(
            picker.label(0),
            Some("Toggle file tree"),
            "got {:?}",
            picker.label(0)
        );
    }

    #[test]
    fn a_query_that_matches_nothing_yields_nothing() {
        let mut picker = Picker::commands(silent());
        settle(&mut picker);
        picker.set_query("zzzzqqq".into());
        for _ in 0..200 {
            picker.tick();
        }
        assert_eq!(picker.matched(), 0);
        assert!(
            picker.chosen().is_none(),
            "nothing to choose when nothing matches"
        );
    }

    #[test]
    fn selection_stays_inside_the_result_set() {
        let mut picker = Picker::commands(silent());
        settle(&mut picker);

        picker.move_selection(1000);
        assert_eq!(picker.selected, picker.matched() - 1, "clamped at the end");
        picker.move_selection(-1000);
        assert_eq!(picker.selected, 0, "clamped at the start");

        // Narrowing the results must not leave the selection past the end.
        picker.move_selection(1000);
        picker.set_query("save".into());
        settle(&mut picker);
        picker.clamp();
        assert!(picker.selected < picker.matched().max(1));
    }

    #[test]
    fn the_file_walk_finds_files_and_skips_ignored_ones() {
        let dir = TempDir::new();
        dir.write(".gitignore", "build/\n");
        dir.write("src/main.rs", "fn main() {}\n");
        dir.write("src/deep/nested.rs", "\n");
        dir.write("build/artifact.o", "\n");

        let mut picker = Picker::files(dir.path(), silent());
        settle(&mut picker);

        let found: Vec<String> = (0..picker.matched())
            .filter_map(|i| picker.label(i).map(str::to_string))
            .collect();
        assert!(
            found.iter().any(|f| f.ends_with("main.rs")),
            "walk missed main.rs: {found:?}"
        );
        assert!(
            found.iter().any(|f| f.ends_with("nested.rs")),
            "walk did not recurse: {found:?}"
        );
        assert!(
            !found.iter().any(|f| f.contains("artifact")),
            "ignored directory leaked in: {found:?}"
        );
        assert!(
            !found.iter().any(|f| f.contains(".gitignore")),
            "dotfiles should stay hidden: {found:?}"
        );
    }

    #[test]
    fn choosing_a_file_yields_its_absolute_path() {
        let dir = TempDir::new();
        dir.write("hello.rs", "\n");

        let mut picker = Picker::files(dir.path(), silent());
        settle(&mut picker);
        picker.set_query("hello".into());
        settle(&mut picker);

        match picker.chosen() {
            Some(Action::OpenPath(path)) => {
                assert!(path.is_absolute(), "picker handed back {path:?}");
                assert!(path.ends_with("hello.rs"));
            }
            other => panic!("expected a path, got {other:?}"),
        }
    }
}
