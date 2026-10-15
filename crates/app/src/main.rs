//! lithe — a local-only code editor.
//!
//! On Windows this is a windowed subsystem binary: a console program that
//! opens a window leaves a black console box behind the editor for as long
//! as it runs. Logging and `--snapshot` still reach a terminal that
//! launched it — see `attach_to_console`.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

// First, so `key!` is defined before the modules that use it: a
// macro_rules macro is visible only to what comes after it.
#[macro_use]
mod keys;

mod context;
mod editor;
mod find;
mod gitpanel;
mod icons;
mod imageview;
mod lsp;
mod markdown;
mod menu;
mod mermaid;
mod openfiles;
mod picker;
mod recents;
mod settings;
#[cfg(test)]
mod testutil;
mod theme;
mod tree;
mod view;
mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use lithe_core::{Cursor, Motion, TextBuffer};
use lithe_gpu::Renderer;
use lithe_syntax::Span;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

#[cfg(target_os = "macos")]
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

use context::{Action as CtxAction, ContextMenu};
use editor::Editor;
use find::{Field, Find, ProjectSearch, Prompt, PromptAction};
use gitpanel::GitPanel;
use lsp::{Awaiting, Lsp};
use picker::{Action, Command, Picker};
use recents::Recents;
use settings::{PreviewKind, Settings};
use theme::Theme;
use tree::Tree;
use view::{Chrome, Hit, LandingAction, Mark, Scene, Scratch, Severity};
use workspace::Workspace;

const FONT_PT: f32 = 13.0;
const TAB_WIDTH: usize = 4;
/// Lines moved per notch of a line-based scroll wheel.
const WHEEL_LINES: f32 = 3.0;
/// Half the caret's blink period.
const BLINK: std::time::Duration = std::time::Duration::from_millis(530);
/// How long after the last keystroke the caret keeps blinking.
///
/// Blinking needs a timer, and a timer costs the 0% idle this editor is
/// built around. Settling to solid after a pause keeps both: it blinks
/// while you are working, and an editor left open overnight wakes for
/// nothing.
const BLINK_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the pointer must be still before the editor asks what is
/// under it. Long enough that crossing the window asks nothing, short
/// enough that stopping on a name feels like it answered.
const HOVER_DWELL: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a buffer must sit untouched before autosave writes it.
///
/// Long enough that it does not fire mid-word, short enough that you stop
/// thinking about saving at all.
const AUTOSAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(900);

/// Sent from a worker — nucleo's matcher, or a menu click — when the loop
/// needs to wake and look at something.
#[derive(Debug, Clone, Copy, Default)]
struct Wake;

/// One window, and everything it is showing.
///
/// Windows are fully independent: each has its own open buffers, file tree
/// and project root, which is what makes "open a second window on another
/// project" mean anything.
struct Pane {
    window: Arc<Window>,
    renderer: Renderer,
    ws: Workspace,
    tree: Option<Tree>,
    /// The source control panel. Shares the sidebar with the tree, so at
    /// most one of the two is ever open.
    git: Option<GitPanel>,
    /// Set when something could have changed what `git status` would say.
    git_stale: bool,
    /// Whether a save has already been refused for writing over a file
    /// that changed on disk, so the next one goes through.
    overwrite_ok: bool,
    /// Sidebar width in cells; dragged by its divider.
    sidebar_cols: f32,
    /// Rendered Markdown, and the buffer revision plus column count it was
    /// rendered from, so it is only redone when it would differ.
    preview: Option<Vec<markdown::Line>>,
    /// What the current render was made from: which file, at which
    /// revision, at which width.
    ///
    /// The path is part of the key, and that is the whole point of it.
    /// Keyed on revision and width alone, two freshly opened files both
    /// sat at revision 0 and the same width, so switching between them
    /// matched the cache and left the first file's render on screen.
    preview_for: Option<(PathBuf, u64, usize)>,
    preview_scroll: f32,
    /// Which kinds of file open straight into their preview. Mirrors the
    /// saved settings, so a new window agrees with the others.
    preview_defaults: Settings,
    /// An open right-click menu.
    context: Option<ContextMenu>,
    /// The code actions the menu is offering, in the order it lists them.
    code_actions: Vec<serde_json::Value>,
    /// A decoded image for the active file, kept on the GPU between
    /// frames rather than decoded per redraw.
    image: Option<imageview::ImageView>,
    image_for: Option<PathBuf>,
    picker: Option<Picker>,
    find: Option<Find>,
    search: Option<ProjectSearch>,
    /// In-buffer matches within the visible range, recomputed each frame.
    find_matches: Vec<lithe_search::Span>,
    /// Diagnostics for the active file, translated for drawing.
    marks: Vec<Mark>,
    /// Language-server state for the active file, for the status bar.
    lsp_state: Option<&'static str>,
    /// What the language server is busy doing, while it is.
    lsp_busy: Option<String>,
    completion: Option<find::Completion>,
    /// What the pointer is resting on, once the server has said.
    hover: Option<view::Hover>,
    /// When the pointer last moved, which is what the dwell is measured
    /// from, and whether it came to rest somewhere a hover means anything.
    pointer_moved: std::time::Instant,
    pointer_over_text: bool,
    /// Set while a dwell question is out, so resting asks once rather
    /// than once per wake-up.
    hover_asked: bool,
    /// The element under the pointer, so it can be highlighted.
    hovered: Option<view::Hit>,
    /// The repository this project sits in, if any. Discovered once.
    repo: Option<lithe_vcs::Repo>,
    /// Differences from HEAD for the active file, and what they were
    /// computed from, so a frame that changed nothing recomputes nothing.
    hunks: Vec<lithe_vcs::Hunk>,
    hunks_for: Option<(PathBuf, u64)>,
    /// Whether to annotate the cursor's line with who last changed it.
    /// A view setting per window, like the sidebar, not a saved one.
    blame_on: bool,
    /// The whole file's blame, and what it was read for. One subprocess
    /// per file, not per line: the annotation follows the cursor, and
    /// moving the cursor has to stay free.
    blame: Option<lithe_vcs::FileBlame>,
    blame_for: Option<(PathBuf, u64)>,
    /// The note for the line the cursor is on, as drawn.
    blame_note: Option<String>,
    prompt: Option<Prompt>,
    /// Reused per-frame scratch, so drawing allocates nothing in steady
    /// state.
    scratch: Scratch,
    /// Highlight spans for the visible range, recomputed each frame.
    highlights: Vec<Span>,
    /// The same, for a diff's two sides.
    diff_old_spans: Vec<Span>,
    diff_new_spans: Vec<Span>,
    /// Shown in the status bar until the next keystroke.
    status: Option<String>,
    pointer: PhysicalPosition<f64>,
    dragging: bool,
    /// Last tree row clicked, and when, for telling a double click from
    /// two single ones.
    last_tree_click: Option<(usize, std::time::Instant)>,
    /// Where the last click in the text landed, when, and how many clicks
    /// deep the run is — one places a caret, two takes a word, three a
    /// line.
    last_text_click: Option<(usize, std::time::Instant, u8)>,
    /// When the caret last moved or text last changed; drives the blink.
    last_input: std::time::Instant,
    /// True while the scrollbar thumb is being dragged.
    scrubbing: bool,
    /// The same for the horizontal one.
    scrubbing_x: bool,
    /// True while the sidebar divider is being dragged.
    resizing: bool,
    /// Where the pointer was when the middle button last moved the view,
    /// while it is held down. The document follows the pointer.
    panning: Option<PhysicalPosition<f64>>,
}

impl Pane {
    /// What is open, for hit-testing. Mirrors what `draw` is showing.
    fn panels<'a>(&'a self, recents: &'a [&'a Path]) -> view::Panels<'a> {
        view::Panels {
            workspace: &self.ws,
            tree: self.tree.as_ref(),
            git: self.git.as_ref(),
            recents,
            search_open: self.search.is_some(),
            preview_open: self.preview.is_some() || self.image.is_some(),
            settings_open: self.ws.showing_settings(),
            context: self.context.as_ref(),
            diff_editable: self.ws.diff().is_some_and(workspace::Diff::editable),
            diff_scroll: self.ws.diff().map_or(0.0, |d| d.scroll),
            preview: self.preview.as_deref(),
            preview_scroll: self.preview_scroll,
            diff_side_by_side: self.preview_defaults.side_by_side_diff,
        }
    }

    fn layout(&self) -> view::Layout {
        view::layout(
            &self.renderer,
            &self.ws,
            Chrome {
                sidebar: self.tree.is_some() || self.git.is_some(),
                sidebar_cols: self.sidebar_cols,
                find: self.find.is_some() || self.prompt.is_some(),
            },
        )
    }

    /// Where the caret is on screen, for anchoring a menu to it.
    fn caret_point(&self) -> (f32, f32) {
        let Some(editor) = self.ws.active() else {
            return (0.0, 0.0);
        };
        view::caret_point(&self.layout(), editor, TAB_WIDTH)
    }

    /// Moves the cursor to a 1-based line, opening the file if needed.
    fn jump_to(&mut self, path: PathBuf, line: u64) {
        self.open_path(path);
        let rows = self.visible_rows();
        if let Some(editor) = self.ws.active_mut() {
            let index =
                (line.saturating_sub(1) as usize).min(editor.doc.len_lines().saturating_sub(1));
            let at = editor.doc.line_to_char(index);
            editor.sel.collapse_to(Cursor::at(at));
            editor.scroll_to_cursor(rows);
        }
    }

    /// Moves the cursor to a span and selects it.
    fn reveal_span(&mut self, span: lithe_search::Span) {
        let rows = self.visible_rows();
        if let Some(editor) = self.ws.active_mut() {
            editor.sel.collapse_to(lithe_core::Cursor {
                anchor: span.start,
                head: span.end,
            });
            editor.scroll_to_cursor(rows);
        }
    }

    /// Recomputes the diff against HEAD, but only when the file or its
    /// revision has actually changed.
    fn refresh_hunks(&mut self) {
        let Some(repo) = self.repo.as_ref() else {
            self.hunks.clear();
            return;
        };
        let Some(editor) = self.ws.active() else {
            self.hunks.clear();
            self.hunks_for = None;
            return;
        };
        let Some(path) = editor.path.clone() else {
            self.hunks.clear();
            self.hunks_for = None;
            return;
        };
        let key = (path, editor.revision);
        if self.hunks_for.as_ref() == Some(&key) {
            return;
        }
        self.hunks.clear();
        if let Some(committed) = repo.committed(&key.0) {
            let current = editor.doc.rope().to_string();
            self.hunks = lithe_vcs::hunks(&committed, &current);
        }
        self.hunks_for = Some(key);
    }

    /// Recollects in-buffer matches for the visible range only.
    fn refresh_find_matches(&mut self) {
        self.find_matches.clear();
        let Some(pattern) = self.find.as_ref().and_then(|f| f.pattern.as_ref()) else {
            return;
        };
        let rows = self.visible_rows();
        let Some(editor) = self.ws.active() else {
            return;
        };
        let doc = &editor.doc;
        let last_line = doc.len_lines().saturating_sub(1);
        let first = editor.top_line().min(last_line);
        let last = (first + rows + 1).min(last_line);
        let range = doc.line_to_char(first)
            ..(doc.line_to_char(last) + doc.line_len_chars(last)).min(doc.len_chars());
        lithe_search::matches_in_range(doc, pattern, range, &mut self.find_matches);
    }

    fn visible_rows(&self) -> usize {
        self.layout().rows
    }

    /// Scrolls whatever the text area is showing, in lines and columns.
    ///
    /// One place, because there are three things it can be — a diff, a
    /// rendered preview, a buffer — and three ways to ask: the wheel, the
    /// scrollbar and a middle-button drag. Each of those used to know
    /// about the editor and nothing else, which is why dragging the
    /// scrollbar over a preview moved a buffer nobody could see.
    fn scroll_text(&mut self, lines: f32, columns: f32) {
        let l = self.layout();
        let (rows, cols) = (l.rows, l.cols());
        let diff_rows = self.diff_rows();
        if let Some(diff) = self.ws.diff_mut() {
            let max = (diff_rows as f32 - rows as f32 + 2.0).max(0.0);
            diff.scroll = (diff.scroll - lines).clamp(0.0, max);
        } else if let Some(preview) = self.preview.as_ref() {
            // Rows, not lines: a top-level heading is two rows tall, so
            // counting lines stopped short of the end.
            let max = (view::preview_rows(preview) as f32 - rows as f32 + 2.0).max(0.0);
            self.preview_scroll = (self.preview_scroll - lines).clamp(0.0, max);
        } else if let Some(editor) = self.ws.active_mut() {
            editor.scroll_by(lines, rows);
            editor.scroll_x_by(-columns, rows, cols);
        }
    }

    /// How many rows the open diff scrolls through.
    ///
    /// Side by side it is one row per *pair* of lines, which is fewer
    /// than the lines: scrolling by lines there runs off the end of a
    /// diff that is only half as tall as it counted.
    fn diff_rows(&self) -> usize {
        let Some(diff) = self.ws.diff() else {
            return 0;
        };
        if self.preview_defaults.side_by_side_diff {
            lithe_vcs::pair_columns(&diff.lines).len()
        } else {
            diff.lines.len()
        }
    }

    /// Puts the heading a `#fragment` names at the top of the preview.
    ///
    /// `false` when this document has no such heading, which is worth
    /// saying: a link to one that was renamed is a broken link, and
    /// scrolling nowhere looks like a click that missed.
    fn scroll_preview_to(&mut self, fragment: &str) -> bool {
        let Some(row) = self
            .preview
            .as_deref()
            .and_then(|lines| markdown::anchor(lines, fragment))
        else {
            return false;
        };
        let rows = self.layout().rows;
        let total = self.preview.as_deref().map_or(0, view::preview_rows);
        let max = (total as f32 - rows as f32 + 2.0).max(0.0);
        self.preview_scroll = (row as f32).clamp(0.0, max);
        true
    }

    /// Jumps the horizontal scrollbar to an x position on its track.
    fn scrub_x_to(&mut self, x: f32) {
        let l = self.layout();
        let rows = l.rows;
        let cols = l.cols();
        if let Some(editor) = self.ws.active_mut() {
            let widest = editor.widest_visible(rows, cols);
            editor.scroll_x = view::h_scrollbar_scroll_at(&l, widest, x);
            editor.clamp_scroll_x(rows, cols);
        }
    }

    /// Jumps the scrollbar of whatever the text area is showing to a y
    /// position on its track.
    fn scrub_to(&mut self, y: f32) {
        let l = self.layout();
        let rows = l.rows;
        let total = self.diff_rows();
        if let Some(diff) = self.ws.diff_mut() {
            let max = (total as f32 - rows as f32 + 2.0).max(0.0);
            diff.scroll = view::scrollbar_scroll_at(&l, total, y).clamp(0.0, max);
        } else if let Some(preview) = self.preview.as_ref() {
            let total = view::preview_rows(preview);
            let max = (total as f32 - rows as f32 + 2.0).max(0.0);
            self.preview_scroll = view::scrollbar_scroll_at(&l, total, y).clamp(0.0, max);
        } else {
            let total = self.ws.active().map(|e| e.doc.len_lines()).unwrap_or(0);
            if let Some(editor) = self.ws.active_mut() {
                editor.scroll = view::scrollbar_scroll_at(&l, total, y);
                editor.clamp_scroll(rows);
            }
        }
    }

    /// Tells the active editor how wide the window it is being drawn into
    /// is, so moving the caret can scroll sideways to keep up with it.
    ///
    /// Done here because this is the moment the answer is known: the
    /// width depends on the sidebar, the gutter and the font, none of
    /// which the editor can see from where it sits.
    fn note_viewport(&mut self) {
        let cols = self.layout().cols();
        if let Some(editor) = self.ws.active_mut() {
            editor.visible_cols = cols;
        }
    }

    fn redraw(&self) {
        self.window.request_redraw();
    }

    /// Restarts the blink, so the caret is solid the instant you type.
    fn touch(&mut self) {
        self.last_input = std::time::Instant::now();
    }

    /// Whether the caret is in the on half of its blink. Solid once the
    /// window has been left alone.
    fn caret_visible(&self) -> bool {
        let since = self.last_input.elapsed();
        since >= BLINK_FOR || (since.as_millis() / BLINK.as_millis()).is_multiple_of(2)
    }

    /// When this pane next needs an autosave, if one is pending.
    fn next_autosave(&self, enabled: bool) -> Option<std::time::Instant> {
        let editor = self.ws.active();
        let wait = autosave_delay(
            enabled,
            editor.is_some_and(|e| e.dirty),
            editor.is_some_and(|e| e.path.is_some()),
            self.last_input.elapsed(),
        )?;
        Some(std::time::Instant::now() + wait)
    }

    /// When the caret next needs repainting, if it is still blinking.
    fn next_blink(&self) -> Option<std::time::Instant> {
        let since = self.last_input.elapsed();
        if since >= BLINK_FOR || self.ws.active().is_none() {
            return None;
        }
        let elapsed = since.as_millis();
        let period = BLINK.as_millis();
        let remaining = period - (elapsed % period);
        Some(std::time::Instant::now() + std::time::Duration::from_millis(remaining as u64))
    }

    /// When the pointer will have rested long enough to ask what is
    /// under it, if it is somewhere that question means anything.
    fn next_hover(&self) -> Option<std::time::Instant> {
        if !self.pointer_over_text || self.hover_asked || self.hover.is_some() {
            return None;
        }
        Some(self.pointer_moved + HOVER_DWELL)
    }

    /// Records what the pointer is over. True when that changed, which
    /// is the only time the highlight needs repainting.
    ///
    /// Only discrete things are tracked. The text, the scrollbar and a
    /// diff row carry a position, so every pixel of movement over them
    /// would read as a different element and ask for a frame.
    fn set_hovered(&mut self, over: view::Hit) -> bool {
        let now = view::hoverable(over);
        let changed = now != self.hovered;
        self.hovered = now;
        changed
    }

    /// Takes down the tooltip, and re-arms the dwell.
    ///
    /// Called from everything that means the pointer is no longer just
    /// resting: typing, clicking, scrolling, moving away.
    fn dismiss_hover(&mut self) {
        self.hover = None;
        self.hover_asked = false;
    }

    /// Where `cmd+P` searches: the project root, else the working directory.
    fn search_root(&self) -> PathBuf {
        self.ws
            .root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn open_path(&mut self, path: PathBuf) {
        self.open_path_as(path, false);
    }

    /// `preview` opens provisionally; see [`Workspace::open`].
    fn open_path_as(&mut self, path: PathBuf, preview: bool) {
        match self.ws.open(&path, TAB_WIDTH, preview) {
            Ok(()) => {
                if let Some(tree) = self.tree.as_mut() {
                    tree.reveal(&path);
                }
                self.apply_preview_default();
                self.status = None;
            }
            Err(e) => self.status = Some(format!("{e}")),
        }
        self.retitle();
    }

    /// Replaces the project. Opening a folder starts fresh rather than
    /// piling a second project's files on top of the first.
    fn open_folder(&mut self, folder: PathBuf) {
        let root = real_path(&folder);
        self.repo = lithe_vcs::Repo::discover(&root);
        self.hunks_for = None;
        self.ws = Workspace::empty(Some(root.clone()));
        self.tree = Some(Tree::new(root));
        self.picker = None;
        self.highlights.clear();
        self.status = None;
        self.retitle();
    }

    /// Shows the diff for a changed file, or hides it again to leave the
    /// plain editor. One click to compare, a second to stop comparing.
    ///
    /// The file is opened either way, and the diff takes over that tab
    /// rather than adding one: the diff's new side *is* that buffer, so
    /// the two belong in one place. Switching to another tab and back
    /// finds the diff where it was left.
    fn show_diff(&mut self, path: PathBuf) {
        let showing = self
            .ws
            .index_of(&path)
            .filter(|index| self.ws.tabs[*index].diff().is_some_and(|d| d.path == path));
        if let Some(index) = showing {
            self.ws.hide_diff(index);
            self.ws.active = index;
            return;
        }
        if self.repo.is_none() {
            return;
        }
        self.open_path_as(path.clone(), true);
        self.build_diff(&path, 0.0);
    }

    /// Builds the diff of one working file against HEAD, into its tab.
    ///
    /// The new side is read from the buffer rather than from disk, so an
    /// edit shows up in the diff as it is typed. Reading the file would
    /// mean the diff only caught up at a save, which for an editable
    /// diff is the wrong moment.
    fn build_diff(&mut self, path: &Path, scroll: f32) {
        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let title = repo
            .relative(path)
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());

        // Against HEAD, and against nothing for a file git has never seen.
        let committed = repo.committed(path).unwrap_or_default();
        let open = self
            .ws
            .index_of(path)
            .and_then(|index| self.ws.tabs[index].editor());
        let revision = open.map(|e| e.revision).unwrap_or(0);
        let current = match open {
            Some(editor) => editor.doc.rope().to_string(),
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        let lines = lithe_vcs::unified(&committed, &current, 3);

        // The committed side is parsed once. It cannot change while
        // the diff is open, so a rebuild on every keystroke carries the
        // existing tree across rather than making another.
        let (old, old_syntax) = match self.ws.diff_mut() {
            Some(existing) if existing.path == path && existing.old.len_bytes() > 0 => (
                std::mem::take(&mut existing.old),
                existing.old_syntax.take(),
            ),
            _ => {
                let doc = lithe_core::TextBuffer::from_text(&committed);
                let mut syntax = lithe_syntax::Syntax::for_path(path);
                if let Some(syntax) = syntax.as_mut() {
                    syntax.parse(&doc);
                }
                (doc, syntax)
            }
        };

        self.ws.show_diff(
            path,
            workspace::Diff {
                // Filled in by `show_diff`, which moves the tab's own
                // buffer across rather than opening a second one.
                editor: None,
                old,
                old_syntax,
                title,
                path: path.to_path_buf(),
                lines,
                revision,
                scroll,
            },
        );
    }

    /// Rebuilds the diff when the file it shows has been edited here.
    fn refresh_diff(&mut self) {
        let Some(diff) = self.ws.diff().filter(|d| d.editable()) else {
            return;
        };
        let stale = diff
            .editor
            .as_ref()
            .is_some_and(|editor| editor.revision != diff.revision);
        if stale {
            let (path, scroll) = (diff.path.clone(), diff.scroll);
            self.build_diff(&path, scroll);
        }
        self.scroll_diff_to_cursor();
    }

    /// Scrolls an editable diff so the row the caret is on is visible.
    ///
    /// The diff is rebuilt on every keystroke, and its rows move as lines
    /// are added and removed, so "where the caret is" has to be found
    /// again rather than tracked.
    fn scroll_diff_to_cursor(&mut self) {
        let rows = self.layout().rows.saturating_sub(2);
        let side_by_side = self.preview_defaults.side_by_side_diff;
        let Some(diff) = self.ws.diff_mut() else {
            return;
        };
        let Some(editor) = diff.editor.as_ref() else {
            return;
        };
        let line = editor.doc.char_to_line(editor.sel.primary().head) + 1;

        // In the split view a pair is a row; in the unified view a diff
        // line is.
        let row = if side_by_side {
            lithe_vcs::pair_columns(&diff.lines)
                .iter()
                .position(|(_, right)| right.is_some_and(|i| diff.lines[i].new == Some(line)))
        } else {
            diff.lines.iter().position(|l| l.new == Some(line))
        };
        let Some(row) = row else { return };

        let top = diff.scroll.max(0.0) as usize;
        if row < top {
            diff.scroll = row as f32;
        } else if row >= top + rows {
            diff.scroll = (row + 1 - rows) as f32;
        }
    }

    /// Toggles the preview for the active file, and reports which kind
    /// of file that decision was about.
    ///
    /// Every previewable kind has both modes: Markdown renders or shows
    /// its source, and SVG draws or shows its markup. The two routes
    /// differ only in which renderer does the work.
    fn toggle_preview(&mut self) -> Option<(PreviewKind, bool)> {
        let kind = self
            .ws
            .active()
            .and_then(|e| e.path.as_deref())
            .and_then(PreviewKind::of)?;

        let showing = self.preview.is_some() || self.image.is_some();
        if showing {
            self.preview = None;
            self.preview_for = None;
            self.image = None;
            self.image_for = None;
            self.preview_defaults.set_preview(kind, false);
            return Some((kind, false));
        }
        self.preview_defaults.set_preview(kind, true);
        self.preview_scroll = 0.0;
        self.preview_for = None;
        match kind {
            PreviewKind::Markdown => {
                self.preview = Some(Vec::new());
                self.refresh_preview();
            }
            // Pictures, SVG among them, come back through the ordinary
            // refresh, which also owns freeing the old one.
            PreviewKind::Svg | PreviewKind::Image => {
                self.image_for = None;
                self.refresh_image();
            }
        }
        Some((kind, true))
    }

    /// Whether the tab in front is showing a file, as opposed to a diff
    /// or the settings page.
    ///
    /// The preview and the image decoder both work off the active
    /// buffer, and a diff tab has one — so without this, diffing a
    /// Markdown file left a rendered preview behind it and diffing a PNG
    /// drew the picture over the diff.
    fn showing_file(&self) -> bool {
        matches!(self.ws.active_tab(), Some(workspace::Tab::File(_)))
    }

    /// Whether the active file should be showing a text preview now.
    fn active_is_previewable(&self) -> bool {
        self.showing_file()
            && self
                .ws
                .active()
                .and_then(|e| e.path.as_deref())
                .is_some_and(|p| markdown::previewable(p) && self.preview_defaults.previews(p))
    }

    /// Opens the preview for a freshly opened file, when that is what the
    /// person last chose.
    fn apply_preview_default(&mut self) {
        let markdown = self.active_is_previewable();
        if markdown && self.preview.is_none() {
            self.preview_scroll = 0.0;
            self.preview_for = None;
            self.preview = Some(Vec::new());
            self.refresh_preview();
        } else if !markdown && self.preview.is_some() {
            self.preview = None;
            self.preview_for = None;
        }
    }

    /// Decodes the active file as an image, or drops a stale one.
    ///
    /// Keyed by path: switching tabs away from a picture frees it, and
    /// switching back decodes once rather than every frame.
    fn refresh_image(&mut self) {
        let path = self
            .ws
            .active()
            .filter(|_| self.showing_file())
            .and_then(|e| e.path.clone())
            .filter(|p| imageview::is_image(p) && self.preview_defaults.previews(p));

        if path == self.image_for {
            return;
        }
        self.image_for = path.clone();
        self.image = None;
        let Some(path) = path else { return };

        match imageview::load(&self.renderer, &path) {
            Ok(view) => self.image = Some(view),
            Err(e) => self.status = Some(e),
        }
    }

    /// Re-renders the preview when the text or the width has changed.
    ///
    /// Also the single place that decides the preview should close: it ran
    /// against whichever buffer was active, so switching tabs used to
    /// leave it rendering Rust as if it were Markdown. Checking here
    /// covers every way the active buffer can change, rather than every
    /// call site remembering to.
    fn refresh_preview(&mut self) {
        if self.preview.is_none() {
            return;
        }
        if !self.active_is_previewable() {
            self.preview = None;
            self.preview_for = None;
            return;
        }
        let columns = view::preview_columns(&self.layout());
        let Some(editor) = self.ws.active() else {
            self.preview = None;
            self.preview_for = None;
            return;
        };
        let path = editor.path.clone().unwrap_or_default();
        let key = (path.clone(), editor.revision, columns);
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        let source = editor.doc.rope().to_string();
        // Measured with the renderer that will draw it, so a line wraps
        // where it is actually going to run out of room.
        let mut measure = Measured {
            renderer: &self.renderer,
        };
        self.preview = Some(markdown::render_with(&source, columns, &mut measure));
        self.preview_for = Some(key);
    }

    /// Works out the blame note for the cursor's line.
    ///
    /// A modified buffer is not blamed: `git blame` reads the file on
    /// disk, so once the two differ its line numbers are about different
    /// lines, and a confident wrong name is worse than none.
    fn refresh_blame(&mut self) {
        if !self.blame_on {
            self.blame = None;
            self.blame_for = None;
            self.blame_note = None;
            return;
        }
        let Some(editor) = self.ws.active() else {
            self.blame_note = None;
            return;
        };
        let line = editor.doc.char_to_line(editor.sel.primary().head);

        // A modified buffer is not blamed: git reads the file on disk,
        // so once the two differ its line numbers are about different
        // lines, and a confident wrong name is worse than none.
        if editor.dirty {
            self.blame = None;
            self.blame_for = None;
            self.blame_note = Some("unsaved changes".into());
            return;
        }

        let here = editor.path.clone().map(|path| (path, editor.revision));
        if here != self.blame_for {
            self.blame_for = here.clone();
            self.blame = here
                .as_ref()
                .zip(self.repo.as_ref())
                .and_then(|((path, _), repo)| repo.blame_file(path));
        }
        self.blame_note = self
            .blame
            .as_ref()
            .and_then(|blame| blame.line(line))
            .map(lithe_vcs::BlameCommit::label);
    }

    /// Opens or closes the source control panel. It shares the sidebar
    /// with the tree, so showing one hides the other.
    fn toggle_git(&mut self) {
        match self.git {
            Some(_) => {
                self.git = None;
            }
            None => {
                if self.repo.is_none() {
                    self.status = Some("not a git repository".into());
                    return;
                }
                self.tree = None;
                self.git = Some(GitPanel::new(self.repo.as_ref()));
                self.git_stale = false;
            }
        }
    }

    /// Re-reads `git status` when something could have changed it.
    fn refresh_git(&mut self) {
        if !self.git_stale {
            return;
        }
        self.git_stale = false;
        let repo = self.repo.take();
        if let Some(panel) = self.git.as_mut() {
            panel.refresh(repo.as_ref());
        }
        self.repo = repo;
    }

    /// Re-reads the tree after something on disk has changed under it.
    fn refresh_tree(&mut self) {
        if let Some(tree) = self.tree.as_mut() {
            tree.rebuild();
        }
        self.git_stale = true;
    }

    fn toggle_tree(&mut self) {
        match self.tree {
            Some(_) => self.tree = None,
            None => {
                // The two share the sidebar.
                self.git = None;
                let mut tree = Tree::new(self.search_root());
                if let Some(path) = self.ws.active().and_then(|e| e.path.clone()) {
                    tree.reveal(&path);
                }
                self.tree = Some(tree);
            }
        }
    }

    fn save(&mut self) {
        let Some(editor) = self.ws.active_mut() else {
            return;
        };
        // Saving is a temp-file-and-rename, so writing over a file that
        // something else has changed replaces it wholesale and without
        // a word. Refuse once and say so; asking again goes through.
        if editor.changed_underneath() && !self.overwrite_ok {
            self.overwrite_ok = true;
            self.status =
                Some("changed on disk since you opened it — save again to overwrite".into());
            return;
        }
        self.overwrite_ok = false;
        self.git_stale = true;
        self.status = Some(match editor.save() {
            Ok(()) => format!(
                "saved {}",
                editor
                    .path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            Err(e) => format!("{e}"),
        });
        self.retitle();
    }

    fn retitle(&self) {
        // A tab that is not a file still has a name, and a window that
        // went back to saying just "lithe" when the settings page opened
        // looked like it had lost the project.
        let panel = match self.ws.active_tab() {
            Some(workspace::Tab::Settings) => Some("Settings".to_string()),
            Some(workspace::Tab::Diff(diff)) => Some(format!("diff · {}", diff.title)),
            _ => None,
        };
        let title = match (
            &self.ws.root,
            panel.or_else(|| {
                self.ws
                    .active()
                    .and_then(|e| e.path.as_ref())
                    .map(|p| p.display().to_string())
            }),
        ) {
            (_, Some(what)) => format!("lithe — {what}"),
            (Some(root), None) => format!("lithe — {}", root.display()),
            _ => "lithe".to_string(),
        };
        self.window.set_title(&title);
    }

    /// Recollects highlight spans for the lines about to be drawn. Bounded
    /// by the viewport, so this costs the same on any size of file.
    fn refresh_highlights(&mut self) {
        let rows = self.visible_rows();
        let Pane { ws, highlights, .. } = self;
        let Some(editor) = ws.active_mut() else {
            highlights.clear();
            return;
        };
        let range = {
            let doc = &editor.doc;
            let last_line = doc.len_lines().saturating_sub(1);
            let first = editor.top_line().min(last_line);
            // One line of slack past the bottom, so a partially visible row
            // is still coloured.
            let last = (first + rows + 1).min(last_line);
            let start_char = doc.line_to_char(first);
            let end_char = (doc.line_to_char(last) + doc.line_len_chars(last)).min(doc.len_chars());
            let rope = doc.rope();
            rope.char_to_byte(start_char)..rope.char_to_byte(end_char)
        };
        editor.highlights(range, highlights);
    }

    /// Highlight spans for the diff's two sides, over the rows showing.
    ///
    /// Two queries a frame rather than one per row, and bounded by the
    /// viewport like everything else: the new side comes off the live
    /// buffer's incremental tree, the old side off the one parsed when
    /// the diff was built.
    fn refresh_diff_highlights(&mut self) {
        self.diff_old_spans.clear();
        self.diff_new_spans.clear();
        let rows = self.visible_rows();
        let top = self.ws.diff().map_or(0, |d| d.scroll.max(0.0) as usize);
        let side_by_side = self.preview_defaults.side_by_side_diff;

        // Which lines of each side the visible rows show.
        let Some(diff) = self.ws.diff() else { return };
        let mut old_lines: Vec<usize> = Vec::new();
        let mut new_lines: Vec<usize> = Vec::new();
        let pairs = side_by_side.then(|| lithe_vcs::pair_columns(&diff.lines));
        for row in top..top + rows + 2 {
            let shown: [Option<usize>; 2] = match &pairs {
                Some(pairs) => match pairs.get(row) {
                    Some((left, right)) => [
                        left.and_then(|i| diff.lines.get(i)).and_then(|l| l.old),
                        right.and_then(|i| diff.lines.get(i)).and_then(|l| l.new),
                    ],
                    None => break,
                },
                None => match diff.lines.get(row) {
                    Some(line) => [line.old, line.new],
                    None => break,
                },
            };
            if let Some(old) = shown[0] {
                old_lines.push(old.saturating_sub(1));
            }
            if let Some(new) = shown[1] {
                new_lines.push(new.saturating_sub(1));
            }
        }

        let span = |doc: &lithe_core::TextBuffer, lines: &[usize]| {
            let (first, last) = (*lines.iter().min()?, *lines.iter().max()?);
            let last = last.min(doc.len_lines().saturating_sub(1));
            let start = doc.line_to_char(first.min(last));
            let end = (doc.line_to_char(last) + doc.line_len_chars(last)).min(doc.len_chars());
            let rope = doc.rope();
            Some(rope.char_to_byte(start)..rope.char_to_byte(end))
        };

        // The committed side, from the tree built with the diff.
        if let Some(range) = span(&diff.old, &old_lines) {
            let Some(diff) = self.ws.diff_mut() else {
                return;
            };
            let doc = std::mem::take(&mut diff.old);
            if let Some(syntax) = diff.old_syntax.as_mut() {
                syntax.highlights(&doc, range, &mut self.diff_old_spans);
            }
            if let Some(diff) = self.ws.diff_mut() {
                diff.old = doc;
            }
        }

        // The working side, from the buffer's own tree.
        let range = self
            .ws
            .active()
            .and_then(|editor| span(&editor.doc, &new_lines));
        if let (Some(range), Some(editor)) = (range, self.ws.active_mut()) {
            editor.highlights(range, &mut self.diff_new_spans);
        }
    }

    fn draw(&mut self, theme: &Theme, recents: &[&Path], autosave: bool) {
        self.note_viewport();
        self.refresh_highlights();
        self.refresh_diff_highlights();
        self.refresh_find_matches();
        self.refresh_hunks();
        self.refresh_git();
        self.refresh_preview();
        self.refresh_diff();
        self.refresh_image();
        self.refresh_blame();
        // Disjoint field borrows: the renderer is mutable while the scene
        // borrows the rest of the pane immutably.
        // Computed before the fields are split apart, since it borrows
        // the repository.
        let branch = self.repo.as_ref().and_then(lithe_vcs::Repo::branch);
        let caret_visible = self.caret_visible();
        let Pane {
            renderer,
            ws,
            tree,
            git,
            sidebar_cols,
            preview,
            preview_scroll,
            image,
            preview_defaults,
            context,
            picker,
            find,
            search,
            find_matches,
            marks,
            completion,
            hover,
            hovered,
            prompt,
            hunks,
            blame_note,
            scratch,
            highlights,
            diff_old_spans,
            diff_new_spans,
            status,
            lsp_state,
            lsp_busy,
            ..
        } = self;
        let scene = Scene {
            workspace: ws,
            blame: blame_note.as_deref(),
            recents,
            highlights,
            diff_old: ws.diff().map(|d| &d.old),
            diff_old_spans,
            diff_new_spans,
            diagnostics: marks,
            completion: completion.as_ref(),
            prompt: prompt.as_ref(),
            caret_visible,
            hunks,
            branch: branch.as_deref(),
            lsp: *lsp_state,
            lsp_busy: lsp_busy.as_deref(),
            autosave,
            hovered: *hovered,
            find: find.as_ref(),
            search: search.as_ref(),
            find_matches,
            tree: tree.as_ref(),
            git: git.as_ref(),
            sidebar_cols: *sidebar_cols,
            preview: preview.as_deref(),
            preview_scroll: *preview_scroll,
            diff: ws.diff().map(|d| d.lines.as_slice()),
            diff_title: ws.diff().map(|d| d.title.as_str()),
            diff_scroll: ws.diff().map_or(0.0, |d| d.scroll),
            diff_side_by_side: preview_defaults.side_by_side_diff,
            diff_editable: ws.diff().is_some_and(workspace::Diff::editable),
            hover: hover.as_ref(),
            image: image.as_ref(),
            settings: ws
                .showing_settings()
                .then_some(preview_defaults as &Settings),
            context: context.as_ref(),
            picker: picker.as_ref(),
            theme,
            status: status.as_deref(),
            tab_width: TAB_WIDTH,
        };
        view::draw(renderer, &scene, scratch);
    }
}

/// A [`markdown::Measure`] backed by the renderer the preview is drawn
/// with.
///
/// Widths come back in body cells, which is the unit the layout is in:
/// the renderer answers in pixels, and one cell is one cell of the grid
/// the window is otherwise built on.
struct Measured<'a> {
    renderer: &'a Renderer,
}

impl markdown::Measure for Measured<'_> {
    fn width(&mut self, text: &str, style: markdown::Style, scale: markdown::Scale) -> f32 {
        let cell = self.renderer.cell().advance;
        if cell <= 0.0 {
            return text.chars().count() as f32;
        }
        let pixels = self.renderer.measure(
            text,
            style.face(),
            markdown::Style::size(scale),
            style.family(),
        );
        pixels / cell
    }
}

/// Whether the platform's command modifier is held.
///
/// macOS puts the editing shortcuts on Command and leaves Control to the
/// terminal conventions; Windows and Linux put them on Control. One key per
/// platform, read here so the shortcut table below can be written once.
fn command_held(modifiers: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

struct App {
    panes: HashMap<WindowId, Pane>,
    /// The window the menu and shortcuts act on.
    focused: Option<WindowId>,
    theme: Theme,
    modifiers: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    /// What the explorer's Cut/Copy holds. Separate from the text
    /// clipboard: a path copied here is a file to move, not a string to
    /// type, and conflating the two loses one of them.
    file_clipboard: Option<FileClip>,
    /// The file a discard has been offered for and not yet confirmed.
    discarding: Option<PathBuf>,
    /// Filesystem changes made from the explorer, for its Undo/Redo.
    file_undo: Vec<FileOp>,
    file_redo: Vec<FileOp>,
    recents: Recents,
    settings: Settings,
    lsp: Lsp,
    /// Last revision sent to a language server, per file.
    synced: HashMap<PathBuf, u64>,
    proxy: EventLoopProxy<Wake>,
    /// What the first window should open, taken on startup.
    initial: Option<PathBuf>,
    /// Kept alive for the lifetime of the app; dropping it drops the menu.
    _menu: Option<muda::Menu>,
    menu_events: Option<std::sync::mpsc::Receiver<muda::MenuId>>,
    /// True once a native menu owns the standard shortcuts, so the keyboard
    /// handler must not run them a second time.
    menu_active: bool,
    /// `LITHE_TRACE=1` logs every window event. Read once at startup, not
    /// per event, so leaving it compiled in costs a single bool test.
    trace: bool,
}

impl App {
    fn new(initial: Option<PathBuf>, proxy: EventLoopProxy<Wake>) -> Self {
        Self {
            panes: HashMap::new(),
            focused: None,
            theme: Theme::default(),
            modifiers: ModifiersState::empty(),
            // A missing clipboard is survivable; the editor still edits.
            file_clipboard: None,
            discarding: None,
            file_undo: Vec::new(),
            file_redo: Vec::new(),
            clipboard: arboard::Clipboard::new()
                .map_err(|e| log::warn!("clipboard unavailable: {e}"))
                .ok(),
            recents: Recents::load(),
            settings: Settings::load(),
            lsp: Lsp::new({
                let proxy = proxy.clone();
                Arc::new(move || {
                    let _ = proxy.send_event(Wake);
                })
            }),
            synced: HashMap::new(),
            proxy,
            initial,
            _menu: None,
            menu_events: None,
            menu_active: false,
            trace: std::env::var_os("LITHE_TRACE").is_some(),
        }
    }

    /// Opens the active file with its language server if needed, and
    /// sends the text on when it has changed since the last sync.
    ///
    /// Driven from the frame rather than from each edit: a keystroke
    /// produces exactly one redraw, so this coalesces naturally and a
    /// burst of typing cannot outrun it.
    fn sync_lsp(&mut self) {
        let Some(id) = self.focused else { return };
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let root = pane.search_root();
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let (Some(path), Some(language)) = (editor.path.clone(), editor.language_name()) else {
            return;
        };
        let revision = editor.revision;
        if self.synced.get(&path) == Some(&revision) {
            return;
        }
        let text = editor.doc.rope().to_string();
        let first_time = !self.synced.contains_key(&path);
        if first_time {
            self.lsp.open(language, &root, &path, text);
        } else {
            self.lsp.changed(language, &path, text);
        }
        self.synced.insert(path, revision);
    }

    /// Translates the active file's diagnostics into drawing coordinates.
    fn refresh_marks(&mut self) {
        let Some(id) = self.focused else { return };
        let App { panes, lsp, .. } = self;
        let Some(pane) = panes.get_mut(&id) else {
            return;
        };
        pane.marks.clear();
        pane.lsp_state = None;
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let Some(path) = editor.path.as_deref() else {
            return;
        };
        pane.lsp_state = editor.language_name().map(|l| lsp.state_of(l));
        // What the server is busy with outranks the fact that it is up:
        // "indexing 40%" is the answer to "why is nothing happening".
        pane.lsp_busy = lsp.busy().map(str::to_string);

        for diagnostic in lsp.diagnostics_for(path) {
            let range = lithe_lsp::range_to_chars(&editor.doc, diagnostic.range);
            pane.marks.push(Mark {
                start: range.start,
                end: range.end,
                severity: match diagnostic.severity {
                    Some(lithe_lsp::DiagnosticSeverity::ERROR) => Severity::Error,
                    Some(lithe_lsp::DiagnosticSeverity::WARNING) => Severity::Warning,
                    Some(lithe_lsp::DiagnosticSeverity::INFORMATION) => Severity::Information,
                    _ => Severity::Hint,
                },
                message: diagnostic.message.clone(),
            });
        }
    }

    /// The most severe diagnostic under the cursor, for the status bar.
    fn diagnostic_at_cursor(&self) -> Option<String> {
        let pane = self.pane()?;
        let head = pane.ws.active()?.sel.primary().head;
        pane.marks
            .iter()
            .filter(|m| m.start <= head && head <= m.end)
            .max_by_key(|m| m.severity)
            .map(|m| m.message.replace('\n', " "))
    }

    /// Persists the settings and pushes them into every window.
    fn apply_settings(&mut self) {
        self.settings.save();
        let settings = self.settings.clone();
        for pane in self.panes.values_mut() {
            pane.preview_defaults = settings.clone();
            // Re-decide what this pane should be showing now.
            pane.image_for = None;
            pane.apply_preview_default();
        }
    }

    /// Writes the active buffer back if it has been sitting dirty.
    ///
    /// Only buffers with a path: a scratch buffer has nowhere to go, and
    /// silently inventing a filename for one would be worse than leaving
    /// it unsaved.
    fn autosave(&mut self) {
        if !self.settings.autosave {
            return;
        }
        let due = self.pane().is_some_and(|p| {
            p.next_autosave(true)
                .is_some_and(|at| at <= std::time::Instant::now())
        });
        if !due {
            return;
        }
        if let Some(pane) = self.pane_mut() {
            // Reuse the ordinary save path, so autosave gets the same
            // temp-file-and-rename guarantee a manual save does.
            pane.save();
            // Saving is not "input", so the caret should not restart its
            // blink and the next autosave should not be rescheduled off
            // the back of this one.
            pane.status = None;
        }
        let saved = self
            .pane()
            .and_then(|p| p.ws.active())
            .and_then(|e| Some((e.language_name()?, e.path.clone()?)));
        if let Some((language, path)) = saved {
            self.lsp.saved(language, &path);
        }
    }

    /// Asks the server about the cursor position.
    fn ask_lsp(&mut self, what: Awaiting) {
        self.ask_lsp_at(what, None);
    }

    /// As [`App::ask_lsp`], about a given position rather than the caret.
    ///
    /// Only the pointer needs this: every other question is about where
    /// the caret is, and threading a position through all of them to
    /// serve one would make the common call the awkward one.
    fn ask_lsp_at(&mut self, what: Awaiting, position: Option<lithe_lsp::Position>) {
        let Some(pane) = self.pane() else { return };
        let root = pane.search_root();
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let Some(path) = editor.path.clone() else {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("save this buffer first".into());
            }
            return;
        };
        let Some(language) = editor.language_name() else {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("no language server for this file type".into());
            }
            return;
        };
        let position = position
            .unwrap_or_else(|| lithe_lsp::to_position(&editor.doc, editor.sel.primary().head));

        let outcome = self.lsp.ask(language, &root, &path, position, what);
        if !matches!(outcome, lsp::Asked::Sent) {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some(outcome.to_string());
            }
        }
    }

    /// Where a popup should point when the question was asked by name
    /// rather than by the pointer: at the caret, where the person is
    /// looking.
    fn caret_anchor(&self) -> (f32, f32) {
        let Some(pane) = self.pane() else {
            return (0.0, 0.0);
        };
        let l = pane.layout();
        match pane.ws.active() {
            Some(editor) => view::caret_point(&l, editor, TAB_WIDTH),
            None => (l.text_x, l.text_y),
        }
    }

    /// Asks the pointer's question, once it has stopped moving.
    ///
    /// A hover is the one thing the editor does without being told to, so
    /// it is bounded on every side: only over text, only after the
    /// pointer has been still for [`HOVER_DWELL`], only once per rest,
    /// and only when a server for that language is already up — resting a
    /// pointer must not start a process.
    fn dwell_hover(&mut self) {
        let Some(pane) = self.pane() else { return };
        if !dwell_due(
            pane.pointer_over_text,
            pane.hover_asked,
            pane.hover.is_some() || pane.completion.is_some() || pane.context.is_some(),
            pane.pointer_moved.elapsed(),
        ) {
            return;
        }
        // This rest has been considered, whether or not there turns out
        // to be anything to ask. Marking it only on the paths that do
        // ask left `next_hover` handing back a deadline already in the
        // past, so the loop woke, declined and re-armed without pause —
        // 20% of a core spent resting a pointer over a file with no
        // language server, which is invariant 1 broken outright.
        if let Some(pane) = self.pane_mut() {
            pane.hover_asked = true;
        }
        let Some(pane) = self.pane() else { return };
        let l = pane.layout();
        let (x, y) = (pane.pointer.x as f32, pane.pointer.y as f32);
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let Some(language) = editor.language_name() else {
            return;
        };
        if !self.lsp.is_running(language) {
            return;
        }
        let under = view::point_to_char_under(
            &l,
            &editor.doc,
            editor.scroll,
            editor.scroll_x,
            x,
            y,
            TAB_WIDTH,
        );
        // Past the end of a line there is no word to ask about.
        if under >= editor.doc.len_chars() {
            return;
        }
        let position = lithe_lsp::to_position(&editor.doc, under);

        // Anchored on the character, not the pointer: the popup should
        // sit against the thing it describes.
        let anchor = (x, y);
        self.ask_lsp_at(
            Awaiting::Hover {
                at: anchor,
                dwell: true,
            },
            Some(position),
        );
    }

    /// Why an empty answer was empty.
    ///
    /// A server that has not finished loading answers `null` to
    /// everything, which is not the same as having nothing to say.
    /// Reporting the two alike is what made rust-analyzer look broken
    /// for the first few seconds of every project.
    fn empty_because(&self, otherwise: &str) -> String {
        match self.lsp.busy() {
            Some(doing) => format!("language server is still {doing}"),
            None => otherwise.to_string(),
        }
    }

    /// Acts on a reply the editor asked for.
    fn on_lsp_reply(&mut self, what: Awaiting, result: serde_json::Value) {
        match what {
            Awaiting::Hover { at, dwell } => {
                let text = hover_text(&result);
                let why = self.empty_because("no information");
                let Some(pane) = self.pane_mut() else { return };
                // Deliberately left set: an empty answer must not re-arm
                // the dwell while the pointer has not moved, or the
                // editor asks the same question forever. Moving the
                // pointer is what clears it.
                match text {
                    Some(text) => {
                        pane.hover = Some(view::Hover {
                            lines: markdown::render_boxed(&text, view::Hover::COLUMNS),
                            at,
                        })
                    }
                    // Nothing to say. Worth a line when it was asked for
                    // by name; not worth one every time the pointer
                    // happens to rest on a comma.
                    None if !dwell => pane.status = Some(why),
                    None => {}
                }
            }
            Awaiting::Definition => match first_location(&result) {
                Some((path, line)) => {
                    if let Some(pane) = self.pane_mut() {
                        pane.jump_to(path, line + 1);
                    }
                }
                None => {
                    let why = self.empty_because("no definition found");
                    if let Some(pane) = self.pane_mut() {
                        pane.status = Some(why);
                    }
                }
            },
            Awaiting::Completion => {
                let items = completion_items(&result);
                let why = self.empty_because("no completions");
                if let Some(pane) = self.pane_mut() {
                    if items.is_empty() {
                        pane.status = Some(why);
                        return;
                    }
                    let mut completion = find::Completion::new(items);
                    // Filter by the word already typed, and remember it as
                    // what accepting replaces. Without this a list asked
                    // for mid-word showed everything the server knows, and
                    // accepting one appended it to the partial word rather
                    // than finishing it: `fo` + `foo` came out `fofoo`.
                    let prefix = pane.ws.active().map(typed_prefix).unwrap_or_default();
                    if !prefix.is_empty() {
                        completion.narrow(prefix);
                    }
                    if completion.is_empty() {
                        pane.status = Some("no completions".into());
                    } else {
                        pane.completion = Some(completion);
                    }
                }
            }
            Awaiting::References => self.show_locations(&result, "reference", locations(&result)),
            Awaiting::GoTo(which) => match locations(&result).into_iter().next() {
                Some((path, line)) => {
                    if let Some(pane) = self.pane_mut() {
                        pane.jump_to(path, line + 1);
                    }
                }
                None => {
                    if let Some(pane) = self.pane_mut() {
                        pane.status = Some(which.missing().into());
                    }
                }
            },
            Awaiting::PrepareCalls { incoming } => {
                // The reply names the symbol; the question we actually
                // had needs a second request carrying that name.
                let item = match &result {
                    serde_json::Value::Array(items) => items.first().cloned(),
                    serde_json::Value::Object(_) => Some(result.clone()),
                    _ => None,
                };
                let Some(item) = item else {
                    if let Some(pane) = self.pane_mut() {
                        pane.status = Some("no symbol under the cursor".into());
                    }
                    return;
                };
                let language = self
                    .pane()
                    .and_then(|p| p.ws.active())
                    .and_then(|e| e.language_name());
                if let Some(language) = language {
                    let what = Awaiting::Calls {
                        incoming,
                        item: item.clone(),
                    };
                    self.lsp
                        .follow_up(language, what, |client| Some(client.calls(item, incoming)));
                }
            }
            Awaiting::Calls { incoming, .. } => {
                let places = call_locations(&result, incoming);
                let what = if incoming { "caller" } else { "call" };
                self.show_locations(&result, what, places);
            }
            Awaiting::CodeActions { .. } => self.show_code_actions(&result),
            Awaiting::ResolveCodeAction => {
                let files = result.get("edit").map(workspace_edits).unwrap_or_default();
                if files.is_empty() {
                    if let Some(pane) = self.pane_mut() {
                        pane.status = Some("that action changed nothing".into());
                    }
                    return;
                }
                let (edits, touched) = self.apply_workspace_edit(files);
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some(format!("applied {edits} edits in {touched} files"));
                }
            }
            Awaiting::Rename(to) => {
                let files = workspace_edits(&result);
                if files.is_empty() {
                    if let Some(pane) = self.pane_mut() {
                        pane.status = Some(format!("nothing to rename to {to}"));
                    }
                    return;
                }
                let (edits, touched) = self.apply_workspace_edit(files);
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some(format!(
                        "renamed {edits} occurrence{} in {touched} file{}",
                        if edits == 1 { "" } else { "s" },
                        if touched == 1 { "" } else { "s" },
                    ));
                }
            }
            Awaiting::Formatting {
                path,
                revision,
                tab_width: _,
                range: _,
            } => {
                let edits = text_edits(&result);
                let rows = self.pane().map(Pane::visible_rows).unwrap_or(1);
                let Some(pane) = self.pane_mut() else { return };
                let Some(editor) = pane.ws.active_mut() else {
                    return;
                };
                // The reply describes the text as it was when we asked.
                // Applying it to a buffer that has since been typed into
                // would corrupt it, so a stale reply is dropped.
                if editor.path.as_deref() != Some(path.as_path()) || editor.revision != revision {
                    pane.status = Some("the buffer changed; formatting dropped".into());
                    return;
                }
                let spans: Vec<_> = edits
                    .into_iter()
                    .map(|(range, text)| (lithe_lsp::range_to_chars(&editor.doc, range), text))
                    .collect();
                let applied = editor.apply_edits(spans);
                editor.scroll_to_cursor(rows);
                pane.status = Some(if applied == 0 {
                    "already formatted".into()
                } else {
                    format!("formatted ({applied} edits)")
                });
            }
        }
    }

    /// Puts a list of places in the search panel.
    fn show_locations(
        &mut self,
        _result: &serde_json::Value,
        what: &str,
        places: Vec<(PathBuf, u64)>,
    ) {
        if places.is_empty() {
            let why = self.empty_because(&format!("no {what}s found"));
            if let Some(pane) = self.pane_mut() {
                pane.status = Some(why);
            }
            return;
        }
        let count = places.len();

        // One read per file, not per hit: a symbol used forty times in
        // one file should not open it forty times.
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let hits: Vec<lithe_search::Hit> = places
            .into_iter()
            .map(|(path, line)| {
                let lines = cache.entry(path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(&path)
                        .map(|text| text.lines().map(str::to_string).collect())
                        .unwrap_or_default()
                });
                let text = lines
                    .get(line as usize)
                    .map(|l| l.trim().to_string())
                    .unwrap_or_default();
                lithe_search::Hit {
                    path,
                    line: line + 1,
                    text,
                }
            })
            .collect();

        let notifier = self.notifier();
        let Some(pane) = self.pane_mut() else { return };
        let label = format!("{count} {what}{}", if count == 1 { "" } else { "s" });
        pane.search = Some(ProjectSearch::fixed(
            pane.search_root(),
            notifier,
            label,
            hits,
        ));
    }

    /// Applies a rename's edits, to open buffers where it can and to disk
    /// where it must.
    ///
    /// Opening every touched file as a tab would be tidier for undo, but a
    /// rename across forty files would then leave forty tabs open, which
    /// is worse than the thing it fixes.
    fn apply_workspace_edit(
        &mut self,
        files: Vec<(PathBuf, Vec<(lithe_lsp::Range, String)>)>,
    ) -> (usize, usize) {
        let (mut edits, mut touched) = (0, 0);
        for (path, ranges) in files {
            let open = self.pane_mut().and_then(|p| {
                p.ws.files_mut()
                    .find(|e| e.path.as_deref() == Some(path.as_path()))
            });
            let applied = match open {
                Some(editor) => {
                    let spans: Vec<_> = ranges
                        .into_iter()
                        .map(|(range, text)| (lithe_lsp::range_to_chars(&editor.doc, range), text))
                        .collect();
                    editor.apply_edits(spans)
                }
                None => edit_on_disk(&path, ranges),
            };
            if applied > 0 {
                edits += applied;
                touched += 1;
            }
        }
        if let Some(pane) = self.pane_mut() {
            pane.git_stale = true;
            pane.hunks_for = None;
        }
        (edits, touched)
    }

    /// Offers the code actions a server replied with, as a menu at the
    /// caret.
    fn show_code_actions(&mut self, result: &serde_json::Value) {
        let actions: Vec<serde_json::Value> = result
            .as_array()
            .map(|items| items.to_vec())
            .unwrap_or_default();
        let titles: Vec<String> = actions
            .iter()
            .filter_map(|a| {
                let title = a.get("title").and_then(|t| t.as_str())?;
                Some(trim_to(title, context::ContextMenu::MAX_COLUMNS))
            })
            .collect();
        if titles.is_empty() {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("no code actions here".into());
            }
            return;
        }

        let at = self.pane().map(Pane::caret_point).unwrap_or((0.0, 0.0));
        if let Some(pane) = self.pane_mut() {
            pane.code_actions = actions;
            pane.context = Some(ContextMenu::for_code_actions(at, titles));
        }
    }

    /// Asks what could be done at the cursor.
    fn request_code_actions(&mut self) {
        let range = self.pane().and_then(|p| {
            let editor = p.ws.active()?;
            let cursor = editor.sel.primary();
            Some(lithe_lsp::Range {
                start: lithe_lsp::to_position(&editor.doc, cursor.start()),
                end: lithe_lsp::to_position(&editor.doc, cursor.end()),
            })
        });
        if let Some(range) = range {
            self.ask_lsp(Awaiting::CodeActions { range });
        }
    }

    /// Applies one of the offered code actions.
    ///
    /// A server may reply with an action that has no edits yet, only the
    /// data to compute them; that needs a second request, which is what
    /// `codeAction/resolve` is for.
    fn run_code_action(&mut self, index: usize) {
        let action = self.pane().and_then(|p| p.code_actions.get(index).cloned());
        let Some(action) = action else { return };

        if let Some(edit) = action.get("edit") {
            let files = workspace_edits(edit);
            if !files.is_empty() {
                let (edits, touched) = self.apply_workspace_edit(files);
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some(format!("applied {edits} edits in {touched} files"));
                }
                return;
            }
        }

        if action.get("data").is_some() {
            let language = self
                .pane()
                .and_then(|p| p.ws.active())
                .and_then(|e| e.language_name());
            if let Some(language) = language {
                let sent = self
                    .lsp
                    .follow_up(language, Awaiting::ResolveCodeAction, |c| {
                        Some(c.resolve_code_action(action))
                    });
                if sent {
                    return;
                }
            }
        }

        if let Some(pane) = self.pane_mut() {
            // An action that is only a server-side command needs
            // workspace/executeCommand and the applyEdit traffic that
            // comes back from it, which this editor does not speak.
            pane.status = Some("that action needs a command this editor does not run".into());
        }
    }

    /// Asks the server to format just the selection.
    fn format_selection(&mut self) {
        let asked = self.pane().and_then(|p| {
            let editor = p.ws.active()?;
            let cursor = editor.sel.primary();
            if cursor.is_empty() {
                return None;
            }
            Some((
                editor.path.clone()?,
                editor.revision,
                editor.tab_width,
                lithe_lsp::Range {
                    start: lithe_lsp::to_position(&editor.doc, cursor.start()),
                    end: lithe_lsp::to_position(&editor.doc, cursor.end()),
                },
            ))
        });
        match asked {
            Some((path, revision, tab_width, range)) => self.ask_lsp(Awaiting::Formatting {
                path,
                revision,
                tab_width,
                range: Some(range),
            }),
            None => {
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some("select something to format first".into());
                }
            }
        }
    }

    /// Asks the server to format the whole buffer.
    fn format_buffer(&mut self) {
        let asked = self
            .pane()
            .and_then(|p| p.ws.active())
            .and_then(|e| Some((e.path.clone()?, e.revision, e.tab_width)));
        match asked {
            Some((path, revision, tab_width)) => self.ask_lsp(Awaiting::Formatting {
                path,
                revision,
                tab_width,
                range: None,
            }),
            None => {
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some("save this buffer first".into());
                }
            }
        }
    }

    /// Asks for a new name, then renames through the language server.
    fn begin_rename_symbol(&mut self) {
        let Some(pane) = self.pane_mut() else { return };
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let word = editor.doc.word_at(editor.sel.primary().head);
        let current = editor.doc.slice_to_string(word);
        if current.trim().is_empty() {
            pane.status = Some("put the cursor on a symbol first".into());
            return;
        }
        pane.prompt = Some(Prompt::rename(current));
    }

    /// Copies a link to the current line on the project's remote.
    fn copy_permalink(&mut self) {
        let Some(pane) = self.pane() else { return };
        let here = pane.ws.active().and_then(|e| {
            Some((
                e.path.clone()?,
                e.doc.char_to_line(e.sel.primary().head) + 1,
            ))
        });
        let Some((path, line)) = here else {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("this buffer has no file".into());
            }
            return;
        };
        let link = pane.repo.as_ref().and_then(|r| r.permalink(&path, line));
        match link {
            Some(link) => self.put_on_clipboard(Some(link), "permalink"),
            None => {
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some("no remote to link to".into());
                }
            }
        }
    }

    /// Opens source control on one file's history.
    fn show_file_history(&mut self, target: Option<PathBuf>) {
        let target = target.or_else(|| {
            self.pane()
                .and_then(|p| p.ws.active())
                .and_then(|e| e.path.clone())
        });
        let Some(path) = target else { return };
        let Some(pane) = self.pane_mut() else { return };
        if pane.repo.is_none() {
            pane.status = Some("not a git repository".into());
            return;
        }
        if pane.git.is_none() {
            pane.toggle_git();
        }
        let repo = pane.repo.take();
        if let Some(panel) = pane.git.as_mut() {
            panel.show_file(&path, repo.as_ref());
        }
        pane.repo = repo;
    }

    fn pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused?)
    }

    fn pane_mut(&mut self) -> Option<&mut Pane> {
        self.panes.get_mut(&self.focused?)
    }

    fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let proxy = self.proxy.clone();
        Arc::new(move || {
            let _ = proxy.send_event(Wake);
        })
    }

    /// Opens a window. The second and later ones share the first window's
    /// GPU device and glyph atlas rather than building their own.
    /// Picks up changes made to open files by something else.
    ///
    /// Coming back to the window is the moment to look: a checkout, a
    /// rebase or another editor happened while you were away, and the
    /// buffer is now showing a file that no longer exists in that form.
    ///
    /// A clean buffer is reloaded without asking, because nothing is
    /// lost by it. A dirty one is left alone and says so — the two
    /// versions both matter and only you know which wins.
    fn refresh_from_disk(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let mut reloaded = 0usize;
        let mut conflicted: Vec<String> = Vec::new();
        for editor in pane.ws.files_mut() {
            if !editor.changed_underneath() {
                continue;
            }
            let name = editor
                .path
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if editor.dirty {
                conflicted.push(name);
            } else if editor.reload().is_ok() {
                reloaded += 1;
            }
        }
        if reloaded > 0 {
            pane.git_stale = true;
            pane.hunks_for = None;
            pane.preview_for = None;
        }
        pane.status = match (conflicted.as_slice(), reloaded) {
            ([], 0) => return,
            ([], 1) => Some("reloaded, changed on disk".into()),
            ([], n) => Some(format!("reloaded {n} files, changed on disk")),
            (names, _) => Some(format!(
                "{} changed on disk and has unsaved edits — save to overwrite, or close to discard",
                names.join(", ")
            )),
        };
    }

    /// Opens whatever Finder, the Dock or `open -a` has handed us.
    ///
    /// Into the focused window, because that is where "open this file"
    /// means it should go; a second window is what `cmd+shift+N` is for.
    fn open_handed_files(&mut self, el: &ActiveEventLoop) {
        for path in openfiles::take() {
            if self.panes.is_empty() {
                self.open_window(el, Some(path));
                continue;
            }
            if let Some(pane) = self.pane_mut() {
                if path.is_dir() {
                    // A folder dropped on the Dock is a project, the
                    // same as `cmd+shift+O`.
                    pane.open_folder(path);
                } else {
                    // Deliberate, not provisional: asking the system to
                    // open a file is not browsing past it.
                    pane.open_path_as(path, false);
                }
                pane.retitle();
                pane.redraw();
            }
        }
    }

    fn open_window(&mut self, el: &ActiveEventLoop, arg: Option<PathBuf>) {
        let attrs = Window::default_attributes()
            .with_title("lithe")
            .with_inner_size(LogicalSize::new(1200.0, 800.0));
        // The title bar, Alt-Tab and the taskbar each ask the window for an
        // icon, and a window that has none is drawn with the generic one
        // even though the .exe behind it has a perfectly good icon. It is
        // the same icon: resource 1, put there by `build.rs`.
        #[cfg(target_os = "windows")]
        let attrs = {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            match winit::window::Icon::from_resource(1, None) {
                Ok(icon) => attrs
                    .with_window_icon(Some(icon.clone()))
                    .with_taskbar_icon(Some(icon)),
                Err(e) => {
                    log::warn!("no icon in this build: {e}");
                    attrs
                }
            }
        };
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("could not create window: {e}");
                return;
            }
        };
        // Before the renderer: on Windows the menu bar is part of the
        // window and takes a row of it, so adding it afterwards would
        // size the surface to a client area that is about to shrink.
        if let Some(menu) = self._menu.as_ref() {
            menu::attach(menu, &window);
        }

        let renderer = match self.panes.values().next() {
            Some(existing) => existing.renderer.new_sharing(window.clone()),
            None => Renderer::new(window.clone(), FONT_PT),
        };
        let renderer = match renderer {
            Ok(r) => r,
            Err(e) => {
                log::error!("could not initialise GPU: {e:#}");
                if self.panes.is_empty() {
                    el.exit();
                }
                return;
            }
        };

        let (ws, tree) = match arg {
            // A directory is a project: show the tree, open nothing yet.
            Some(path) if path.is_dir() => {
                let root = real_path(&path);
                self.recents.remember(&root);
                (Workspace::empty(Some(root.clone())), Some(Tree::new(root)))
            }
            // A file opens on its own, rooted at its parent so cmd+P still
            // has somewhere sensible to search.
            Some(path) => {
                let root = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(real_path)
                    .filter(|p| p.is_absolute());
                let mut ws = Workspace::empty(root);
                match Editor::open(&path, TAB_WIDTH) {
                    Ok(editor) => ws.push(editor),
                    Err(e) => log::error!("{e:#}"),
                }
                (ws, None)
            }
            // No argument: no project, no buffer. The landing page is
            // what fills the window, and it is a real surface rather than
            // a scratch buffer pretending to be one.
            None => (Workspace::empty(None), None),
        };

        let id = window.id();
        // Without this the launching terminal often keeps focus, and the
        // menu bar on screen stays the terminal's.
        window.focus_window();

        let repo = ws.root.as_deref().and_then(lithe_vcs::Repo::discover);
        let pane = Pane {
            overwrite_ok: false,
            code_actions: Vec::new(),
            blame_on: false,
            blame: None,
            blame_for: None,
            blame_note: None,
            window,
            renderer,
            repo,
            ws,
            tree,
            git: None,
            git_stale: false,
            sidebar_cols: view::TREE_COLS,
            preview: None,
            preview_for: None,
            preview_scroll: 0.0,
            preview_defaults: self.settings.clone(),
            context: None,
            image: None,
            image_for: None,
            picker: None,
            find: None,
            search: None,
            find_matches: Vec::new(),
            marks: Vec::new(),
            lsp_state: None,
            lsp_busy: None,
            completion: None,
            hover: None,
            pointer_moved: std::time::Instant::now(),
            pointer_over_text: false,
            hover_asked: false,
            hovered: None,
            hunks: Vec::new(),
            hunks_for: None,
            prompt: None,
            scratch: Scratch::default(),
            highlights: Vec::new(),
            diff_old_spans: Vec::new(),
            diff_new_spans: Vec::new(),
            status: None,
            pointer: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            last_tree_click: None,
            last_text_click: None,
            last_input: std::time::Instant::now(),
            scrubbing: false,
            scrubbing_x: false,
            resizing: false,
            panning: None,
        };
        pane.retitle();
        // Ask for the first frame. Drawing is damage-driven and the loop
        // waits rather than polls, so without this nothing paints until
        // some event happens to arrive — the window came up blank and
        // stayed blank until it was clicked.
        pane.redraw();
        self.panes.insert(id, pane);
        self.focused = Some(id);
    }

    fn copy(&mut self, cut: bool) {
        let Some(text) = self.pane().and_then(|p| p.ws.active()?.selected_text()) else {
            return;
        };
        let Some(cb) = self.clipboard.as_mut() else {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("no clipboard available".into());
            }
            return;
        };
        if let Err(e) = cb.set_text(text) {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some(format!("copy failed: {e}"));
            }
            return;
        }
        if cut {
            if let Some(editor) = self.pane_mut().and_then(|p| p.ws.active_mut()) {
                editor.insert("");
            }
        }
    }

    fn paste(&mut self) {
        let text = match self.clipboard.as_mut() {
            Some(cb) => cb.get_text(),
            None => {
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some("no clipboard available".into());
                }
                return;
            }
        };
        match text {
            Ok(text) if !text.is_empty() => {
                if let Some(editor) = self.pane_mut().and_then(|p| p.ws.active_mut()) {
                    editor.insert(&text);
                }
            }
            Ok(_) => {}
            Err(e) => {
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some(format!("paste failed: {e}"));
                }
            }
        }
    }

    /// Creates an empty file on disk and opens it. Distinct from a new
    /// buffer, which exists only in memory until it is saved.
    fn new_file(&mut self) {
        let Some(pane) = self.pane() else { return };
        let mut dialog = rfd::FileDialog::new()
            .set_parent(&*pane.window)
            .set_file_name("untitled.txt");
        if let Some(root) = &pane.ws.root {
            dialog = dialog.set_directory(root);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        if let Err(e) = std::fs::write(&path, "") {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some(format!("cannot create {}: {e}", path.display()));
            }
            return;
        }
        if let Some(pane) = self.pane_mut() {
            pane.open_path(path);
        }
    }

    fn save_as(&mut self) {
        let Some(pane) = self.pane() else { return };
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let mut dialog = rfd::FileDialog::new().set_parent(&*pane.window);
        if let Some(directory) = editor
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .or(pane.ws.root.as_deref())
        {
            dialog = dialog.set_directory(directory);
        }
        if let Some(name) = editor.path.as_deref().and_then(|p| p.file_name()) {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        if let Some(pane) = self.pane_mut() {
            if let Some(editor) = pane.ws.active_mut() {
                editor.set_path(path.clone());
            }
            pane.save();
            if let Some(tree) = pane.tree.as_mut() {
                tree.reveal(&path);
            }
        }
    }

    /// Closes the active buffer, asking first when it has unsaved edits.
    /// Returns false when the person cancelled.
    fn close_active_buffer(&mut self) -> bool {
        let Some(pane) = self.pane() else { return true };
        let Some(editor) = pane.ws.active() else {
            return true;
        };
        if editor.dirty {
            let name = editor
                .path
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "This buffer".to_string());
            // The dialog is owned by this window, so the platform keeps it
            // in front of the editor rather than letting it fall behind
            // one that is not answering while the dialog is up.
            let window = pane.window.clone();
            match confirm_discard(&name, &window) {
                Decision::Cancel => return false,
                Decision::Save => {
                    if let Some(pane) = self.pane_mut() {
                        pane.save();
                    }
                    // A failed save leaves it dirty; do not drop the work.
                    if self
                        .pane()
                        .and_then(|p| p.ws.active())
                        .is_some_and(|e| e.dirty)
                    {
                        return false;
                    }
                }
                Decision::Discard => {}
            }
        }

        let closed = self
            .pane()
            .and_then(|p| p.ws.active())
            .and_then(|e| Some((e.language_name()?, e.path.clone()?)));
        if let Some(pane) = self.pane_mut() {
            pane.ws.close_active();
            pane.retitle();
        }
        if let Some((language, path)) = closed {
            self.lsp.closed(language, &path);
            self.synced.remove(&path);
        }
        true
    }

    /// Asks about every unsaved buffer in a window. False means cancelled.
    fn close_window(&mut self, id: WindowId) -> bool {
        while self
            .panes
            .get(&id)
            .is_some_and(|p| p.ws.files().any(|b| b.dirty))
        {
            // Focus the first dirty buffer so the dialog names the file the
            // person is being asked about.
            let Some(pane) = self.panes.get_mut(&id) else {
                return true;
            };
            let Some(index) = pane
                .ws
                .tabs
                .iter()
                .position(|t| t.editor().is_some_and(|e| e.dirty))
            else {
                break;
            };
            pane.ws.active = index;
            let previous = self.focused;
            self.focused = Some(id);
            let proceeded = self.close_active_buffer();
            self.focused = previous.filter(|f| *f != id).or(Some(id));
            if !proceeded {
                return false;
            }
        }
        // While the window is still here to be taken off, which on Windows
        // is where the menu bar lives.
        if let (Some(menu), Some(pane)) = (self._menu.as_ref(), self.panes.get(&id)) {
            menu::detach(menu, &pane.window);
        }
        self.panes.remove(&id);
        if self.focused == Some(id) {
            self.focused = self.panes.keys().copied().next();
        }
        true
    }

    fn run_command(&mut self, el: &ActiveEventLoop, command: Command) {
        if let Some(pane) = self.pane_mut() {
            pane.status = None;
        }
        let rows = self.pane().map(Pane::visible_rows).unwrap_or(1);
        let notifier = self.notifier();

        match command {
            Command::NewWindow => {
                // Empty, on the landing page. A new window is where you
                // go to work on something else: opening it on the
                // project you are already in gives you a second view of
                // that one, which is the thing you did not ask for — and
                // the landing page is one click from any project you
                // have had open, including this one.
                self.open_window(el, None);
                return;
            }
            Command::GotoDefinition => {
                self.ask_lsp(Awaiting::Definition);
                return;
            }
            Command::CloseBuffer => {
                self.close_active_buffer();
                return;
            }
            Command::StageFile | Command::UnstageFile => {
                self.run_git(command == Command::StageFile);
                return;
            }
            Command::Commit => {
                if let Some(pane) = self.pane_mut() {
                    if pane.repo.is_some() {
                        pane.prompt = Some(Prompt::commit());
                    } else {
                        pane.status = Some("not a git repository".into());
                    }
                }
                return;
            }
            Command::BlameLine => {
                self.toggle_blame();
                return;
            }
            Command::ToggleAutosave => {
                self.settings.autosave = !self.settings.autosave;
                self.settings.save();
                let on = self.settings.autosave;
                if let Some(pane) = self.pane_mut() {
                    pane.status = Some(if on { "autosave on" } else { "autosave off" }.into());
                }
                // Write what is already pending, rather than waiting for
                // the next keystroke to start the clock.
                self.autosave();
                return;
            }
            Command::Settings => {
                if let Some(pane) = self.pane_mut() {
                    // A tab rather than a toggle: asking twice focuses
                    // the one that is already open, and it is closed the
                    // way every other tab is.
                    pane.ws.show_settings();
                    pane.retitle();
                }
                return;
            }
            Command::MarkdownPreview => {
                match self.pane_mut().and_then(Pane::toggle_preview) {
                    // Remember it per kind: the next file of that kind
                    // opens the same way, and the others are untouched.
                    Some((kind, on)) => {
                        self.settings.set_preview(kind, on);
                        self.apply_settings();
                    }
                    None => {
                        if let Some(pane) = self.pane_mut() {
                            pane.status = Some("nothing to preview for this file type".into());
                        }
                    }
                }
                return;
            }
            Command::ToggleGitPanel => {
                if let Some(pane) = self.pane_mut() {
                    pane.toggle_git();
                }
                return;
            }
            Command::Hover => {
                let at = self.caret_anchor();
                self.ask_lsp(Awaiting::Hover { at, dwell: false });
                return;
            }
            Command::NewFile => {
                self.new_file();
                return;
            }
            Command::SaveAs => {
                self.save_as();
                return;
            }
            Command::Cut => {
                self.copy(true);
                return;
            }
            Command::Copy => {
                self.copy(false);
                return;
            }
            Command::Paste => {
                self.paste();
                return;
            }
            Command::Quit => {
                let ids: Vec<_> = self.panes.keys().copied().collect();
                for id in ids {
                    if !self.close_window(id) {
                        return; // cancelled; nothing is lost
                    }
                }
                el.exit();
                return;
            }
            _ => {}
        }

        // Told to the language server after the pane borrow ends.
        let mut saved: Option<(&'static str, PathBuf)> = None;
        let closed: Option<(&'static str, PathBuf)> = None;

        // Everything below acts purely on the focused pane. `recents` is a
        // sibling field, so it borrows alongside rather than through self.
        let App {
            panes,
            focused,
            recents,
            ..
        } = self;
        let Some(pane) = focused.and_then(|id| panes.get_mut(&id)) else {
            return;
        };
        match command {
            Command::NewBuffer => {
                pane.ws
                    .push(Editor::new(TextBuffer::new(), None, TAB_WIDTH));
                pane.retitle();
            }
            Command::OpenFileDialog => {
                let mut dialog = rfd::FileDialog::new().set_parent(&*pane.window);
                if let Some(root) = &pane.ws.root {
                    dialog = dialog.set_directory(root);
                }
                if let Some(path) = dialog.pick_file() {
                    pane.open_path(path);
                }
            }
            Command::OpenFolderDialog => {
                let mut dialog = rfd::FileDialog::new().set_parent(&*pane.window);
                if let Some(root) = &pane.ws.root {
                    dialog = dialog.set_directory(root);
                }
                if let Some(folder) = dialog.pick_folder() {
                    let root = real_path(&folder);
                    recents.remember(&root);
                    pane.open_folder(root);
                }
            }
            Command::Save => {
                pane.save();
                saved = pane
                    .ws
                    .active()
                    .and_then(|e| Some((e.language_name()?, e.path.clone()?)));
            }
            Command::Undo => {
                if let Some(e) = pane.ws.active_mut() {
                    e.undo();
                    e.scroll_to_cursor(rows);
                }
            }
            Command::Redo => {
                if let Some(e) = pane.ws.active_mut() {
                    e.redo();
                    e.scroll_to_cursor(rows);
                }
            }
            // The VS Code multiple-cursor and whole-line set. All of
            // them act on every cursor, so each is a no-op with nothing
            // open rather than a special case.
            Command::SelectNextOccurrence => {
                if let Some(e) = pane.ws.active_mut() {
                    if !e.select_next_occurrence() {
                        pane.status = Some("no more matches".into());
                    }
                    e.scroll_to_cursor(rows);
                }
            }
            Command::SelectAllOccurrences => {
                if let Some(e) = pane.ws.active_mut() {
                    let found = e.select_all_occurrences();
                    pane.status = Some(match found {
                        0 => "nothing to select".into(),
                        n => format!("{n} cursors"),
                    });
                }
            }
            Command::AddCursorAbove => {
                if let Some(e) = pane.ws.active_mut() {
                    e.add_cursor(-1);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::AddCursorBelow => {
                if let Some(e) = pane.ws.active_mut() {
                    e.add_cursor(1);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::GotoLine => {
                if pane.ws.active().is_some() {
                    pane.prompt = Some(Prompt::goto_line());
                }
            }
            Command::SelectLine => {
                if let Some(e) = pane.ws.active_mut() {
                    e.select_lines();
                }
            }
            Command::DeleteLine => {
                if let Some(e) = pane.ws.active_mut() {
                    e.delete_lines();
                    e.scroll_to_cursor(rows);
                }
            }
            Command::DuplicateLine => {
                if let Some(e) = pane.ws.active_mut() {
                    e.duplicate_lines();
                    e.scroll_to_cursor(rows);
                }
            }
            Command::MoveLineUp => {
                if let Some(e) = pane.ws.active_mut() {
                    e.move_lines(-1);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::MoveLineDown => {
                if let Some(e) = pane.ws.active_mut() {
                    e.move_lines(1);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::InsertLineAbove => {
                if let Some(e) = pane.ws.active_mut() {
                    e.open_line(true);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::InsertLineBelow => {
                if let Some(e) = pane.ws.active_mut() {
                    e.open_line(false);
                    e.scroll_to_cursor(rows);
                }
            }
            Command::ToggleComment => {
                if let Some(e) = pane.ws.active_mut() {
                    e.toggle_comment();
                }
            }
            Command::Indent => {
                if let Some(e) = pane.ws.active_mut() {
                    e.indent();
                }
            }
            Command::Unindent => {
                if let Some(e) = pane.ws.active_mut() {
                    e.unindent();
                }
            }
            Command::SelectAll => {
                if let Some(e) = pane.ws.active_mut() {
                    e.select_all();
                }
            }
            Command::Find | Command::Replace => {
                let replacing = command == Command::Replace;
                // Seed the query from the selection, which is what you
                // almost always meant when you select a word and hit find.
                let seed = pane
                    .ws
                    .active()
                    .and_then(|e| e.selected_text())
                    .filter(|t| !t.contains('\n'));
                let mut bar = pane.find.take().unwrap_or_else(|| Find::new(replacing));
                bar.replacing = replacing;
                bar.focus = Field::Query;
                if let Some(seed) = seed {
                    bar.set_query(seed);
                }
                pane.find = Some(bar);
            }
            Command::SearchProject => {
                // A toggle, so the status bar button can close it again.
                if pane.search.is_some() {
                    pane.search = None;
                    return;
                }
                let mut search = ProjectSearch::new(pane.search_root(), notifier);
                if let Some(seed) = pane
                    .ws
                    .active()
                    .and_then(|e| e.selected_text())
                    .filter(|t| !t.contains('\n'))
                {
                    search.set_query(seed);
                }
                pane.search = Some(search);
            }
            Command::ToggleTree => pane.toggle_tree(),
            // Handled after the pane borrow, since they touch the servers.
            Command::GotoDefinition | Command::Hover => {}
            Command::OpenFile => {
                pane.picker = Some(Picker::files(&pane.search_root(), notifier));
            }
            Command::SwitchBuffer => {
                pane.picker = Some(Picker::buffers(pane.ws.labels(), notifier));
            }
            Command::Palette => {
                pane.picker = Some(Picker::commands(notifier));
            }
            // Handled above, before the pane borrow.
            Command::NewWindow
            | Command::NewFile
            | Command::SaveAs
            | Command::Cut
            | Command::Copy
            | Command::Paste
            | Command::Quit
            | Command::CloseBuffer
            | Command::StageFile
            | Command::UnstageFile
            | Command::Commit
            | Command::BlameLine
            | Command::ToggleGitPanel
            | Command::MarkdownPreview
            | Command::ToggleAutosave
            | Command::Settings => {}
        }

        if let Some((language, path)) = saved {
            self.lsp.saved(language, &path);
        }
        if let Some((language, path)) = closed {
            self.lsp.closed(language, &path);
            self.synced.remove(&path);
        }
    }

    /// Runs a right-click menu choice.
    /// `target` is what the menu was opened on; it is passed in rather
    /// than read back off the pane because choosing an entry closes the
    /// menu, and the menu is where the target lived.
    fn run_context(&mut self, action: CtxAction, target: Option<PathBuf>) {
        use tree::Editing;

        let root = self.pane().map(Pane::search_root);

        match action {
            // --- making things ---
            CtxAction::NewFile | CtxAction::NewFolder => {
                // Into the clicked directory, or alongside the clicked
                // file, or the project root when nothing was clicked.
                let dir = match target.as_deref() {
                    Some(p) if p.is_dir() => p.to_path_buf(),
                    Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
                    None => root.clone().unwrap_or_default(),
                };
                let editing = if action == CtxAction::NewFile {
                    Editing::NewFile(dir)
                } else {
                    Editing::NewFolder(dir)
                };
                if let Some(pane) = self.pane_mut() {
                    if pane.tree.is_none() {
                        pane.toggle_tree();
                    }
                    if let Some(t) = pane.tree.as_mut() {
                        t.begin_edit(editing);
                    }
                }
            }
            CtxAction::Rename => {
                if let (Some(path), Some(pane)) = (target.clone(), self.pane_mut()) {
                    if let Some(t) = pane.tree.as_mut() {
                        t.begin_edit(Editing::Rename(path));
                    }
                }
            }
            CtxAction::Duplicate => self.duplicate(target),
            CtxAction::Trash => self.trash(target),

            // --- shelling out ---
            CtxAction::OpenChange => {
                if let (Some(pane), Some(path)) = (self.pane_mut(), target) {
                    pane.open_path_as(path, false);
                    pane.retitle();
                }
            }
            CtxAction::ShowDiff => {
                if let (Some(pane), Some(path)) = (self.pane_mut(), target) {
                    pane.show_diff(path);
                }
            }
            CtxAction::StageFile => self.stage_path(target, true),
            CtxAction::UnstageFile => self.stage_path(target, false),
            CtxAction::DiscardFile => self.discard_path(target),
            CtxAction::RevealInFinder => self.reveal(target),
            CtxAction::OpenInDefaultApp => self.open_in_default_app(target),
            CtxAction::OpenInTerminal => {
                let dir = target
                    .as_deref()
                    .map(|p| {
                        if p.is_dir() {
                            p.to_path_buf()
                        } else {
                            p.parent().map(Path::to_path_buf).unwrap_or_default()
                        }
                    })
                    .or(root);
                self.open_in_terminal(dir);
            }

            // --- clipboard ---
            CtxAction::CopyPath => {
                let text = target.map(|p| p.display().to_string());
                self.put_on_clipboard(text, "path");
            }
            CtxAction::CopyRelativePath => {
                let text = target.and_then(|p| {
                    let root = root.as_deref()?;
                    Some(p.strip_prefix(root).unwrap_or(&p).display().to_string())
                });
                self.put_on_clipboard(text, "relative path");
            }
            CtxAction::CopyPermalink => self.copy_permalink(),
            CtxAction::Cut => self.copy(true),
            CtxAction::Copy => self.copy(false),
            CtxAction::CopyAndTrim => self.copy_and_trim(),
            CtxAction::Paste => self.paste(),

            // --- the file clipboard, which is not the text one ---
            CtxAction::CutFile | CtxAction::CopyFile => {
                let cut = action == CtxAction::CutFile;
                let text = target.as_ref().map(|p| p.display().to_string());
                self.file_clipboard = target.map(|path| FileClip { path, cut });
                self.put_on_clipboard(text, if cut { "path (cut)" } else { "path" });
            }
            CtxAction::PasteFile => self.paste_files(target, root),
            CtxAction::Undo => self.undo_file_op(false),
            CtxAction::Redo => self.undo_file_op(true),
            CtxAction::Delete => self.delete_forever(target),

            // --- things that already have a command ---
            CtxAction::GoTo(where_) => self.ask_lsp(Awaiting::GoTo(where_)),
            CtxAction::Calls { incoming } => self.ask_lsp(Awaiting::PrepareCalls { incoming }),
            CtxAction::CodeActions => self.request_code_actions(),
            CtxAction::CodeAction(index) => self.run_code_action(index),
            CtxAction::FormatSelection => self.format_selection(),
            CtxAction::FindReferences => self.ask_lsp(Awaiting::References),
            CtxAction::RenameSymbol => self.begin_rename_symbol(),
            CtxAction::FormatBuffer => self.format_buffer(),
            CtxAction::BlameLine => self.toggle_blame(),
            CtxAction::FileHistory => self.show_file_history(target),
            CtxAction::TogglePreview => {
                // From the explorer the file may not be open yet, and
                // "Open Preview" has to mean open — so it is only a
                // toggle once what it names is already on screen.
                let was_showing = self
                    .pane()
                    .is_some_and(|p| p.preview.is_some() || p.image.is_some());
                let active = self
                    .pane()
                    .and_then(|p| p.ws.active())
                    .and_then(|e| e.path.clone());
                let same = target.is_none() || target == active;
                if let Some(path) = target {
                    if let Some(pane) = self.pane_mut() {
                        pane.open_path_as(path, true);
                    }
                }
                if same || !was_showing {
                    if let Some((kind, on)) = self.pane_mut().and_then(Pane::toggle_preview) {
                        self.settings.set_preview(kind, on);
                        self.apply_settings();
                    }
                }
            }
        }
    }

    /// Copies or moves what the file clipboard holds into a directory.
    fn paste_files(&mut self, target: Option<PathBuf>, root: Option<PathBuf>) {
        let Some(clip) = self.file_clipboard.clone() else {
            return;
        };
        let into = match target.as_deref() {
            Some(p) if p.is_dir() => p.to_path_buf(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
            None => root.unwrap_or_default(),
        };
        let Some(name) = clip.path.file_name() else {
            return;
        };
        let destination = free_name(&into.join(name));

        // Pasting something into itself would recurse forever.
        if destination.starts_with(&clip.path) {
            if let Some(pane) = self.pane_mut() {
                pane.status = Some("cannot paste a folder into itself".into());
            }
            return;
        }

        let result = if clip.cut {
            std::fs::rename(&clip.path, &destination).map(|()| FileOp::Moved {
                from: clip.path.clone(),
                to: destination.clone(),
            })
        } else if clip.path.is_dir() {
            copy_tree(&clip.path, &destination).map(|()| FileOp::Added(destination.clone()))
        } else {
            std::fs::copy(&clip.path, &destination).map(|_| FileOp::Added(destination.clone()))
        };

        let message = match result {
            Ok(op) => {
                self.record(op);
                // A cut is spent once it has been pasted; a copy is not.
                if clip.cut {
                    self.file_clipboard = None;
                }
                format!(
                    "{} to {}",
                    if clip.cut { "moved" } else { "copied" },
                    destination
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            }
            Err(e) => format!("could not paste: {e}"),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
            pane.refresh_tree();
        }
    }

    /// Pushes a filesystem change onto the undo stack.
    fn record(&mut self, op: FileOp) {
        self.file_undo.push(op);
        // A new change makes the redo stack unreachable, the same as it
        // does in the buffer's history.
        self.file_redo.clear();
        // Enough to take back a misclick, not a session-long journal of
        // the filesystem.
        if self.file_undo.len() > 32 {
            self.file_undo.remove(0);
        }
    }

    /// Takes back the last filesystem change, or puts it back.
    fn undo_file_op(&mut self, redo: bool) {
        let op = if redo {
            self.file_redo.pop()
        } else {
            self.file_undo.pop()
        };
        let Some(op) = op else { return };
        let what = op.label();
        let message = match op.undo() {
            Ok(inverse) => {
                if redo {
                    self.file_undo.push(inverse);
                } else {
                    self.file_redo.push(inverse);
                }
                format!("{} {what}", if redo { "redid" } else { "undid" })
            }
            Err(e) => format!("could not undo: {e}"),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
            pane.refresh_tree();
        }
    }

    /// Deletes a path outright, after asking.
    ///
    /// Separate from Trash because it is the one action here that cannot
    /// be undone, so it is the one action that asks first.
    fn delete_forever(&mut self, target: Option<PathBuf>) {
        let Some(path) = target else { return };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let window = self.pane().map(|pane| pane.window.clone());
        let mut dialog = rfd::MessageDialog::new();
        if let Some(window) = window.as_deref() {
            dialog = dialog.set_parent(window);
        }
        let confirmed = dialog
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Delete")
            .set_description(format!("Delete {name} permanently? This cannot be undone."))
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show();
        if confirmed != rfd::MessageDialogResult::Ok {
            return;
        }

        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        let message = match result {
            Ok(()) => format!("deleted {name}"),
            Err(e) => format!("could not delete: {e}"),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
            pane.refresh_tree();
        }
    }

    /// Copies the selection with its shared indentation removed.
    fn copy_and_trim(&mut self) {
        let text = self
            .pane()
            .and_then(|p| p.ws.active())
            .and_then(Editor::selected_text)
            .map(|text| trim_indent(&text));
        self.put_on_clipboard(text, "trimmed selection");
    }

    /// Shows a file where the system browses files, selected.
    fn reveal(&mut self, target: Option<PathBuf>) {
        #[cfg(target_os = "macos")]
        self.shell("open", &["-R"], target);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        self.shell("xdg-open", &[], target);

        // `/select,<path>` is one argument, not two: Explorer reads the
        // path as part of the switch. It also exits non-zero having done
        // exactly what was asked, so only a failure to start it is worth
        // reporting — which is all `hand_off` reports.
        #[cfg(target_os = "windows")]
        {
            let Some(path) = target else {
                self.say("nothing to open");
                return;
            };
            let mut select = std::ffi::OsString::from("/select,");
            select.push(&path);
            let mut command = spawnable("explorer");
            command.arg(select);
            self.hand_off("explorer", command);
        }
    }

    /// Opens a file with whatever the system opens it with.
    fn open_in_default_app(&mut self, target: Option<PathBuf>) {
        #[cfg(target_os = "macos")]
        self.shell("open", &[], target);
        // `start` is a shell builtin rather than a program, and the empty
        // string is the window title it would otherwise read the path as.
        #[cfg(target_os = "windows")]
        self.shell("cmd", &["/c", "start", ""], target);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        self.shell("xdg-open", &[], target);
    }

    /// Follows a link clicked in the Markdown preview.
    ///
    /// Three kinds, because a README holds three kinds. A url goes to
    /// whatever opens urls. A `#fragment` is a place in this document.
    /// Anything else is a path beside the file being read, which is how
    /// a README points at the rest of its own repository — so that one
    /// opens in the editor rather than in a browser.
    fn follow_link(&mut self, href: &str) {
        let target = href.trim();
        if target.is_empty() {
            return;
        }
        if let Some(fragment) = target.strip_prefix('#') {
            let found = self
                .pane_mut()
                .is_some_and(|pane| pane.scroll_preview_to(fragment));
            if !found {
                self.say(&format!("no heading called “{fragment}”"));
            }
            if let Some(pane) = self.pane() {
                pane.redraw();
            }
            return;
        }
        if is_url(target) {
            self.hand_off(target, url_opener(target));
            return;
        }
        // `guide.md#writing` is a file and a place in it.
        let (relative, fragment) = match target.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment.to_string())),
            None => (target, None),
        };
        let Some(pane) = self.pane_mut() else {
            return;
        };
        // Beside the file the link was written in, as every other
        // renderer of it resolves one; the project root when the buffer
        // has no path of its own yet.
        let base = pane
            .ws
            .active()
            .and_then(|editor| editor.path.as_deref())
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| pane.search_root());
        let path = base.join(unescape(relative));
        if !path.exists() {
            let missing = relative.to_string();
            self.say(&format!("{missing} is not here"));
            if let Some(pane) = self.pane() {
                pane.redraw();
            }
            return;
        }
        if path.is_dir() {
            // A folder is not a buffer, and replacing the project
            // because a link was clicked would be a surprise.
            self.reveal(Some(real_path(&path)));
            return;
        }
        pane.open_path(real_path(&path));
        // A document arrived at through a link starts at its own top,
        // not where the last one was left.
        pane.preview_scroll = 0.0;
        if let Some(fragment) = fragment {
            // The file that just opened has its own preview, and the
            // fragment is a place in that one.
            pane.refresh_preview();
            pane.scroll_preview_to(&fragment);
        }
        pane.redraw();
    }

    /// Opens a terminal in a directory.
    fn open_in_terminal(&mut self, dir: Option<PathBuf>) {
        #[cfg(target_os = "macos")]
        self.shell("open", &["-a", "Terminal"], dir);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        self.shell("x-terminal-emulator", &[], dir);

        // Windows Terminal is what Windows 11 ships and `-d` is how it
        // takes a directory. A machine that has had it removed still has
        // `cmd`, which takes the directory as its own instead.
        #[cfg(target_os = "windows")]
        {
            let Some(dir) = dir else {
                self.say("nothing to open");
                return;
            };
            let mut terminal = spawnable("wt");
            terminal.arg("-d").arg(&dir);
            if terminal.status().is_ok() {
                return;
            }
            let mut fallback = spawnable("cmd");
            fallback.args(["/c", "start", "cmd"]).current_dir(&dir);
            self.hand_off("a terminal", fallback);
        }
    }

    /// Runs a command on a path, reporting whatever it says went wrong.
    fn shell(&mut self, program: &str, args: &[&str], target: Option<PathBuf>) {
        let Some(path) = target else {
            self.say("nothing to open");
            return;
        };
        let mut command = spawnable(program);
        command.args(args).arg(&path);
        self.hand_off(program, command);
    }

    /// Runs something whose job is to hand the path to another program,
    /// saying so if it could not be started at all.
    fn hand_off(&mut self, what: &str, mut command: std::process::Command) {
        if let Err(e) = command.status() {
            self.say(&format!("could not run {what}: {e}"));
        }
    }

    /// Puts one line in the status bar.
    fn say(&mut self, message: &str) {
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message.to_string());
        }
    }

    fn put_on_clipboard(&mut self, text: Option<String>, what: &str) {
        let Some(text) = text else { return };
        let message = match self.clipboard.as_mut() {
            Some(cb) => match cb.set_text(text) {
                Ok(()) => format!("copied {what}"),
                Err(e) => format!("copy failed: {e}"),
            },
            None => "no clipboard available".into(),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
        }
    }

    /// Copies a path to `name copy.ext`, or the next free number after it.
    fn duplicate(&mut self, target: Option<PathBuf>) {
        let Some(path) = target else { return };
        let Some(parent) = path.parent() else { return };
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let extension = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();

        let candidate = free_name(&parent.join(format!("{stem} copy{extension}")));

        let result = if path.is_dir() {
            copy_tree(&path, &candidate)
        } else {
            std::fs::copy(&path, &candidate).map(|_| ())
        };
        let message = match result {
            Ok(()) => {
                self.record(FileOp::Added(candidate.clone()));
                format!(
                    "duplicated as {}",
                    candidate.file_name().unwrap_or_default().to_string_lossy()
                )
            }
            Err(e) => format!("could not duplicate: {e}"),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
            pane.refresh_tree();
        }
    }

    /// Moves a path to the user's Trash rather than unlinking it.
    ///
    /// Deleting outright is not something an editor should do on a menu
    /// click; the Trash is recoverable and is where the platform expects
    /// things to go.
    fn trash(&mut self, target: Option<PathBuf>) {
        let Some(path) = target else { return };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let message = match move_to_trash(&path) {
            Ok(backup) => {
                self.record(FileOp::Removed {
                    path: path.clone(),
                    backup,
                });
                format!("moved {name} to {}", keys::TRASH)
            }
            Err(e) => format!("could not trash: {e}"),
        };
        if let Some(pane) = self.pane_mut() {
            pane.status = Some(message);
            pane.refresh_tree();
        }
    }

    /// Builds the right-click menu for whatever is under the pointer.
    fn open_context_menu(&mut self, id: WindowId) {
        let live: Vec<PathBuf> = self
            .recents
            .live()
            .into_iter()
            .map(Path::to_path_buf)
            .collect();
        let live: Vec<&Path> = live.iter().map(PathBuf::as_path).collect();
        let state = context::State {
            can_paste: self.file_clipboard.is_some(),
            can_undo: !self.file_undo.is_empty(),
            can_redo: !self.file_redo.is_empty(),
            undo_label: self
                .file_undo
                .last()
                .map(FileOp::label)
                .unwrap_or("")
                .into(),
            redo_label: self
                .file_redo
                .last()
                .map(FileOp::label)
                .unwrap_or("")
                .into(),
        };
        // Before the pane is borrowed, since the answer is the language
        // server's rather than the pane's.
        let goes_to = {
            let language = self
                .panes
                .get(&id)
                .and_then(|pane| pane.ws.active())
                .and_then(|editor| editor.language_name());
            self.lsp.goes_to(language)
        };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        let l = pane.layout();
        let (x, y) = (pane.pointer.x as f32, pane.pointer.y as f32);
        let at = (x, y);
        // Close one menu rather than stacking another on it.
        pane.context = None;

        let hit = view::hit_test(&l, &pane.panels(&live), x, y);
        // Right-clicking a tab is about that file, so it is brought to
        // the front first and the menu is the one for the text.
        if let Hit::Tab(index) | Hit::TabClose(index) = hit {
            if index < pane.ws.len() {
                pane.ws.active = index;
                pane.retitle();
                pane.apply_preview_default();
            }
        }

        let menu = match hit {
            Hit::Tree(row) => {
                let entry = pane.tree.as_ref().and_then(|t| t.rows.get(row));
                ContextMenu::for_explorer(
                    at,
                    entry.map(|e| e.path.clone()),
                    entry.is_some_and(|e| e.is_dir),
                    &state,
                )
            }
            // A changed file: the menu is about the change, not the
            // file on disk.
            Hit::GitRow(row) | Hit::GitStage(row) => {
                let change = pane.git.as_ref().and_then(|g| g.change_at(row));
                let Some((path, staged)) = change.map(|c| (c.path.clone(), c.staged)) else {
                    return;
                };
                ContextMenu::for_change(at, path, staged)
            }
            // Empty space in the sidebar acts on the project root.
            Hit::Elsewhere if x < l.tree_w && pane.tree.is_some() => {
                ContextMenu::for_explorer(at, None, true, &state)
            }
            Hit::Text | Hit::Tab(_) | Hit::TabClose(_) => {
                let editor = pane.ws.active();
                let path = editor.and_then(|e| e.path.clone());
                let showing = pane.preview.is_some() || pane.image.is_some();
                let preview = path
                    .as_deref()
                    .and_then(settings::PreviewKind::of)
                    .map(|_| showing);
                ContextMenu::for_editor(
                    at,
                    path,
                    editor.is_some_and(|e| !e.sel.primary().is_empty()),
                    pane.repo.is_some(),
                    preview,
                    goes_to,
                )
            }
            _ => return,
        };
        pane.context = Some(menu);
    }

    /// Stages or unstages one file by path.
    fn stage_path(&mut self, target: Option<PathBuf>, stage: bool) {
        let Some(pane) = self.pane_mut() else { return };
        let (Some(path), Some(repo)) = (target, pane.repo.as_ref()) else {
            return;
        };
        let result = if stage {
            repo.stage(&path)
        } else {
            repo.unstage(&path)
        };
        pane.status = Some(match result {
            Ok(()) => format!(
                "{} {}",
                if stage { "staged" } else { "unstaged" },
                name_of(&path)
            ),
            Err(e) => format!("{e}"),
        });
        pane.git_stale = true;
        pane.hunks_for = None;
    }

    /// Throws away one file's uncommitted changes.
    ///
    /// Asked for twice. This is the one thing the editor does that
    /// destroys work git cannot give back, and a menu entry is one
    /// slip away from a row you meant to click.
    fn discard_path(&mut self, target: Option<PathBuf>) {
        let Some(path) = target else { return };
        let confirmed = self.discarding.as_deref() == Some(path.as_path());
        let Some(pane) = self.pane_mut() else { return };
        if !confirmed {
            pane.status = Some(format!(
                "discard every change to {}? choose it again to confirm",
                name_of(&path)
            ));
            self.discarding = Some(path);
            return;
        }
        self.discarding = None;
        let Some(pane) = self.pane_mut() else { return };
        let Some(repo) = pane.repo.as_ref() else {
            return;
        };
        pane.status = Some(match repo.discard(&path) {
            Ok(()) => format!("discarded changes to {}", name_of(&path)),
            Err(e) => format!("{e}"),
        });
        pane.git_stale = true;
        pane.hunks_for = None;
        // The buffer is now behind the file, and reloading it is the
        // whole point of having discarded.
        for editor in pane.ws.files_mut() {
            if editor.path.as_deref() == Some(path.as_path()) && editor.reload().is_err() {
                pane.status = Some(format!(
                    "discarded, but could not reload {}",
                    name_of(&path)
                ));
            }
        }
    }

    /// Stages or unstages the active file.
    fn run_git(&mut self, stage: bool) {
        let Some(pane) = self.pane_mut() else { return };
        let Some(path) = pane.ws.active().and_then(|e| e.path.clone()) else {
            pane.status = Some("this buffer has no file".into());
            return;
        };
        let Some(repo) = pane.repo.as_ref() else {
            pane.status = Some("not a git repository".into());
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let result = if stage {
            repo.stage(&path)
        } else {
            repo.unstage(&path)
        };
        pane.status = Some(match result {
            Ok(()) if stage => format!("staged {name}"),
            Ok(()) => format!("unstaged {name}"),
            Err(e) => format!("{e}"),
        });
        // Staging moves the index, which is what the gutter is compared
        // against for untracked files.
        pane.hunks_for = None;
        pane.git_stale = true;
    }

    /// Turns the line-blame annotation on or off.
    ///
    /// A toggle rather than a one-shot readout: the question "who wrote
    /// this" is asked about whatever line you are reading, and answering
    /// it once about the line you happened to be on means asking again
    /// for every line after it.
    fn toggle_blame(&mut self) {
        let Some(pane) = self.pane_mut() else { return };
        if pane.repo.is_none() {
            pane.status = Some("not a git repository".into());
            return;
        }
        pane.blame_on = !pane.blame_on;
        // Stale by construction: the switch just changed what it answers.
        pane.blame_for = None;
        pane.status = Some(if pane.blame_on {
            "line blame on".into()
        } else {
            "line blame off".into()
        });
    }

    /// Keys for an open right-click menu. It takes everything: a menu is
    /// a mode, and typing into the buffer behind it would be a surprise.
    fn on_menu_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let mut chosen = None;
        {
            let Some(pane) = self.pane_mut() else {
                return false;
            };
            let Some(menu) = pane.context.as_mut() else {
                return false;
            };
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => pane.context = None,
                Key::Named(NamedKey::ArrowDown) => menu.move_selection(1),
                Key::Named(NamedKey::ArrowUp) => menu.move_selection(-1),
                Key::Named(NamedKey::Enter) => {
                    chosen = menu.chosen().map(|action| (action, menu.target.clone()));
                    pane.context = None;
                }
                _ => {}
            }
        }
        if let Some((action, target)) = chosen {
            self.run_context(action, target);
        }
        true
    }

    /// Keys for a name being typed in the file tree.
    fn on_naming_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let mut made = None;
        {
            let Some(pane) = self.pane_mut() else {
                return false;
            };
            let Some(tree) = pane.tree.as_mut() else {
                return false;
            };
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => tree.cancel_edit(),
                Key::Named(NamedKey::Backspace) => tree.pop_typed(),
                Key::Named(NamedKey::Enter) => match tree.finish_edit() {
                    Ok(done) => made = done,
                    Err(e) => pane.status = Some(format!("{e}")),
                },
                _ => {
                    let typed: String = event
                        .text
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect();
                    if typed.is_empty() {
                        return false;
                    }
                    tree.push_typed(&typed);
                }
            }
        }
        if let Some(made) = made {
            if let Some(pane) = self.pane_mut() {
                pane.git_stale = true;
                match made.from {
                    // A renamed file that is open keeps its tab, pointed
                    // at the new name — otherwise the next save writes
                    // the old one back.
                    Some(from) => {
                        for editor in pane.ws.files_mut() {
                            if editor.path.as_deref() == Some(from.as_path()) {
                                editor.set_path(made.to.clone());
                            }
                        }
                        pane.retitle();
                    }
                    // A new file opens so you can start typing in it; a
                    // new folder has nothing to open.
                    None if made.to.is_file() => pane.open_path_as(made.to, false),
                    None => {}
                }
            }
        }
        true
    }

    /// Prompt-bar keys. One line of input, Enter submits.
    fn on_prompt_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        // Set inside the borrow below and acted on once it has ended:
        // renaming needs the language server, which lives on `self`.
        let mut rename_to = None;
        let handled = {
            let Some(pane) = self.pane_mut() else {
                return false;
            };
            let Some(prompt) = pane.prompt.as_mut() else {
                return false;
            };
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    pane.prompt = None;
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    prompt.text.pop();
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    let submitted = pane.prompt.take();
                    if let Some(prompt) = submitted {
                        match prompt.action {
                            PromptAction::Commit => {
                                let result = pane
                                    .repo
                                    .as_ref()
                                    .map(|repo| repo.commit(prompt.text.trim()));
                                pane.status = Some(match result {
                                    Some(Ok(hash)) => format!("committed {hash}"),
                                    Some(Err(e)) => format!("{e}"),
                                    None => "not a git repository".into(),
                                });
                                pane.hunks_for = None;
                                pane.git_stale = true;
                            }
                            PromptAction::GotoLine => {
                                // 1-based, as every editor and every
                                // compiler error reports a line.
                                match prompt.text.trim().parse::<usize>() {
                                    Ok(line) if line >= 1 => {
                                        let rows = pane.visible_rows();
                                        let total = pane
                                            .ws
                                            .active()
                                            .map(|e| e.doc.len_lines())
                                            .unwrap_or(0);
                                        if let Some(editor) = pane.ws.active_mut() {
                                            let at = (line - 1).min(total.saturating_sub(1));
                                            let head = editor.doc.line_to_char(at);
                                            editor.sel.collapse_to(Cursor::at(head));
                                            editor.scroll_to_cursor(rows);
                                        }
                                    }
                                    _ => {
                                        pane.status =
                                            Some(format!("{:?} is not a line", prompt.text.trim()))
                                    }
                                }
                            }
                            PromptAction::RenameSymbol => {
                                let name = prompt.text.trim().to_string();
                                if name.is_empty() {
                                    pane.status = Some("a name is required".into());
                                } else {
                                    rename_to = Some(name);
                                }
                            }
                        }
                    }
                    true
                }
                _ => {
                    let Some(text) = event.text.as_ref() else {
                        return false;
                    };
                    let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                    if typed.is_empty() {
                        return false;
                    }
                    prompt.text.push_str(&typed);
                    true
                }
            }
        };
        if let Some(name) = rename_to {
            self.ask_lsp(Awaiting::Rename(name));
        }
        handled
    }

    /// Completion popup keys. Typing passes through to the editor and
    /// narrows the list, which is what makes the popup feel like part of
    /// typing rather than a mode.
    fn on_completion_key(&mut self, event: &winit::event::KeyEvent) -> Option<bool> {
        let pane = self.pane_mut()?;
        pane.completion.as_ref()?;
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                pane.completion = None;
                Some(true)
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(c) = pane.completion.as_mut() {
                    c.move_selection(1);
                }
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(c) = pane.completion.as_mut() {
                    c.move_selection(-1);
                }
                Some(true)
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                let chosen = pane
                    .completion
                    .as_ref()
                    .and_then(|c| c.chosen().map(|e| (e.insert.clone(), c.prefix.len())));
                pane.completion = None;
                if let Some((insert, typed)) = chosen {
                    let rows = pane.visible_rows();
                    if let Some(editor) = pane.ws.active_mut() {
                        // Replace what has been typed since the popup
                        // opened, so the word is not doubled.
                        let head = editor.sel.primary().head;
                        let start = head.saturating_sub(typed);
                        editor.replace_span(start, head, &insert);
                        editor.scroll_to_cursor(rows);
                    }
                }
                Some(true)
            }
            // Anything else falls through to the editor, and the list is
            // narrowed afterwards.
            _ => None,
        }
    }

    /// Re-narrows the popup after a keystroke reached the buffer.
    fn narrow_completion(&mut self, typed: Option<&str>) {
        let Some(pane) = self.pane_mut() else { return };
        let Some(completion) = pane.completion.as_mut() else {
            return;
        };
        match typed {
            Some(text) if text.chars().all(|c| c.is_alphanumeric() || c == '_') => {
                let prefix = format!("{}{text}", completion.prefix);
                completion.narrow(prefix);
            }
            _ => pane.completion = None,
        }
        if pane.completion.as_ref().is_some_and(|c| c.is_empty()) {
            pane.completion = None;
        }
    }

    /// How many identifier characters are typed before a list is offered
    /// without being asked for.
    ///
    /// One would fire on the first letter of every word, including every
    /// keyword; two is enough to be a request for help rather than noise,
    /// and matches what the popup is useful at.
    const SUGGEST_AFTER: usize = 2;

    /// Opens a completion list in response to what was just typed.
    ///
    /// Two reasons to: the character is one the server named as a trigger
    /// — `.` in every language, plus `::` or `->` in some — or enough of a
    /// word has been typed that a list is likely what is wanted.
    ///
    /// Exactly one request per word, not one per keystroke: once the
    /// popup is open, further typing narrows the list it already has,
    /// which costs nothing and needs no round trip.
    fn maybe_complete(&mut self, typed: Option<&str>) {
        let Some(text) = typed else { return };
        let Some(pane) = self.pane() else { return };
        if pane.completion.is_some() || pane.find.is_some() || pane.picker.is_some() {
            return;
        }
        let Some(editor) = pane.ws.active() else {
            return;
        };
        let Some(language) = editor.language_name() else {
            return;
        };

        let trigger = self.lsp.triggers_completion(language, text);
        let word = text.chars().all(|c| c.is_alphanumeric() || c == '_')
            && typed_prefix(editor).chars().count() == Self::SUGGEST_AFTER;
        if trigger || word {
            self.ask_lsp(Awaiting::Completion);
        }
    }

    /// Find-bar keys. The bar captures typing until Escape, which is what
    /// every editor does and what stops a search query landing in the file.
    fn on_find_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let shift = self.modifiers.shift_key();
        let cmd = command_held(self.modifiers);
        let Some(pane) = self.pane_mut() else {
            return false;
        };
        let Some(find) = pane.find.as_mut() else {
            return false;
        };

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                pane.find = None;
                pane.find_matches.clear();
                true
            }
            Key::Named(NamedKey::Tab) => {
                find.toggle_field();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                find.backspace();
                true
            }
            Key::Named(NamedKey::Enter) => {
                let Some(pattern) = find.pattern.as_ref() else {
                    return true;
                };
                let replacing = find.replacing;
                let replacement = find.replace.clone();
                let from = pane.ws.active().map(|e| e.sel.primary().head).unwrap_or(0);

                if replacing && cmd {
                    // cmd+enter replaces every match, as one undo step.
                    let count = pane
                        .ws
                        .active_mut()
                        .map(|e| e.replace_all(pattern, &replacement))
                        .unwrap_or(0);
                    pane.status = Some(format!("replaced {count}"));
                    if let Some(find) = pane.find.as_mut() {
                        find.missed = count == 0;
                    }
                    return true;
                }

                let doc_hit = pane.ws.active().and_then(|e| {
                    if replacing && !shift {
                        // Replace the current selection when it is already
                        // the match, then move on.
                        let sel = e.sel.primary();
                        if !sel.is_empty() {
                            return Some(lithe_search::Span {
                                start: sel.start(),
                                end: sel.end(),
                            });
                        }
                    }
                    None
                });

                if let (true, Some(span)) = (replacing && !shift, doc_hit) {
                    if let Some(editor) = pane.ws.active_mut() {
                        editor.replace_span(span.start, span.end, &replacement);
                    }
                }

                let from = pane
                    .ws
                    .active()
                    .map(|e| e.sel.primary().head)
                    .unwrap_or(from);
                let next = pane.ws.active().and_then(|e| {
                    if shift {
                        lithe_search::previous_match(&e.doc, pattern, e.sel.primary().start())
                    } else {
                        lithe_search::next_match(&e.doc, pattern, from)
                    }
                });
                match next {
                    Some(span) => {
                        pane.reveal_span(span);
                        if let Some(find) = pane.find.as_mut() {
                            find.missed = false;
                        }
                    }
                    None => {
                        if let Some(find) = pane.find.as_mut() {
                            find.missed = true;
                        }
                    }
                }
                true
            }
            _ => {
                let Some(text) = event.text.as_ref() else {
                    return false;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return false;
                }
                find.push(&typed);
                true
            }
        }
    }

    /// Project-search panel keys.
    fn on_search_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let Some(pane) = self.pane_mut() else {
            return false;
        };
        let Some(search) = pane.search.as_mut() else {
            return false;
        };

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                pane.search = None;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                search.move_selection(1);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                search.move_selection(-1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let target = search.chosen().map(|hit| (hit.path.clone(), hit.line));
                pane.search = None;
                if let Some((path, line)) = target {
                    pane.jump_to(path, line);
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                let mut query = search.query.clone();
                query.pop();
                search.set_query(query);
                true
            }
            _ => {
                let Some(text) = event.text.as_ref() else {
                    return false;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return false;
                }
                let query = format!("{}{typed}", search.query);
                search.set_query(query);
                true
            }
        }
    }

    /// Picker keys. Returns true when a redraw is needed.
    fn on_picker_key(&mut self, el: &ActiveEventLoop, event: &winit::event::KeyEvent) -> bool {
        let Some(pane) = self.pane_mut() else {
            return false;
        };
        let Some(picker) = pane.picker.as_mut() else {
            return false;
        };
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                pane.picker = None;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                picker.move_selection(1);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                picker.move_selection(-1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let chosen = picker.chosen();
                pane.picker = None;
                match chosen {
                    Some(Action::OpenPath(path)) => pane.open_path(path),
                    Some(Action::SwitchBuffer(index)) => {
                        if index < pane.ws.len() {
                            pane.ws.active = index;
                            pane.retitle();
                        }
                    }
                    Some(Action::Run(command)) => self.run_command(el, command),
                    None => {}
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                let mut query = picker.query.clone();
                query.pop();
                picker.set_query(query);
                true
            }
            _ => {
                let Some(text) = event.text.as_ref() else {
                    return false;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return false;
                }
                let query = format!("{}{typed}", picker.query);
                picker.set_query(query);
                true
            }
        }
    }

    /// Editor keys. Returns true when a redraw is needed.
    fn on_key(&mut self, el: &ActiveEventLoop, event: &winit::event::KeyEvent) -> bool {
        let shift = self.modifiers.shift_key();
        let cmd = command_held(self.modifiers);
        let alt = self.modifiers.alt_key();
        let control = self.modifiers.control_key();

        // The two bindings that are on Control everywhere, including where
        // Control is also the command modifier — so they are read before
        // the command layer rather than after it, which is where they used
        // to sit when only macOS was in question.
        if control {
            // Ctrl+Space asks for completions, the near-universal binding.
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Space)) {
                self.ask_lsp(Awaiting::Completion);
                return true;
            }
            // Ctrl+Tab cycles buffers.
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Tab)) {
                if let Some(pane) = self.pane_mut() {
                    pane.ws.cycle(if shift { -1 } else { 1 });
                    pane.retitle();
                    pane.apply_preview_default();
                }
                return true;
            }
        }

        // Command layer first, so shortcuts never fall through to text.
        //
        // With a native menu these never arrive — AppKit consumes a key
        // that matches an accelerator — but the guard makes that explicit
        // rather than relying on it, and keeps the shortcuts working on a
        // platform where no menu was installed.
        if cmd && !self.menu_active {
            let command = match event.logical_key.as_ref() {
                Key::Character("n") | Key::Character("N") if shift => Some(Command::NewWindow),
                Key::Character("n") => Some(Command::NewBuffer),
                Key::Character("p") | Key::Character("P") => Some(if shift {
                    Command::Palette
                } else {
                    Command::OpenFile
                }),
                Key::Character("b") => Some(Command::ToggleTree),
                Key::Character("w") => Some(Command::CloseBuffer),
                Key::Character("o") | Key::Character("O") => Some(if shift {
                    Command::OpenFolderDialog
                } else {
                    Command::OpenFileDialog
                }),
                Key::Character("s") | Key::Character("S") => Some(if shift {
                    Command::SaveAs
                } else {
                    Command::Save
                }),
                Key::Character("z") | Key::Character("Z") => {
                    Some(if shift { Command::Redo } else { Command::Undo })
                }
                Key::Character("a") => Some(Command::SelectAll),
                Key::Character("f") | Key::Character("F") if shift => Some(Command::SearchProject),
                Key::Character("f") if alt => Some(Command::Replace),
                Key::Character("f") => Some(Command::Find),
                Key::Character("g") | Key::Character("G") if !shift => Some(Command::GotoLine),
                Key::Character("i") => Some(Command::Hover),
                Key::Character("c") => Some(Command::Copy),
                Key::Character("x") => Some(Command::Cut),
                Key::Character("v") => Some(Command::Paste),
                // The multiple-cursor and whole-line set, bound where
                // everyone already expects to find it.
                Key::Character("d") | Key::Character("D") => Some(Command::SelectNextOccurrence),
                Key::Character("l") | Key::Character("L") => Some(if shift {
                    Command::SelectAllOccurrences
                } else {
                    Command::SelectLine
                }),
                Key::Character("k") | Key::Character("K") if shift => Some(Command::DeleteLine),
                Key::Character("/") => Some(Command::ToggleComment),
                Key::Character("]") => Some(Command::Indent),
                Key::Character("[") => Some(Command::Unindent),
                Key::Named(NamedKey::ArrowUp) if alt => Some(Command::AddCursorAbove),
                Key::Named(NamedKey::ArrowDown) if alt => Some(Command::AddCursorBelow),
                Key::Named(NamedKey::Enter) => Some(if shift {
                    Command::InsertLineAbove
                } else {
                    Command::InsertLineBelow
                }),
                _ => None,
            };
            if let Some(command) = command {
                self.run_command(el, command);
                return true;
            }
        }

        let rows = self.pane().map(Pane::visible_rows).unwrap_or(1);

        // Document-wide and line-wide motions live on cmd even when the
        // menu owns the editing shortcuts.
        if cmd {
            let motion = match event.logical_key.as_ref() {
                Key::Named(NamedKey::ArrowLeft) => Some(Motion::LineStart),
                Key::Named(NamedKey::ArrowRight) => Some(Motion::LineEnd),
                Key::Named(NamedKey::ArrowUp) => Some(Motion::DocStart),
                Key::Named(NamedKey::ArrowDown) => Some(Motion::DocEnd),
                _ => None,
            };
            if let Some(motion) = motion {
                if let Some(editor) = self.pane_mut().and_then(|p| p.ws.active_mut()) {
                    editor.motion(motion, shift);
                    editor.scroll_to_cursor(rows);
                }
                return true;
            }
            return false;
        }

        if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::F12)) {
            self.run_command(el, Command::GotoDefinition);
            return true;
        }

        let Some(pane) = self.pane_mut() else {
            return false;
        };
        pane.status = None;
        let Some(editor) = pane.ws.active_mut() else {
            return false;
        };

        // Alt with a vertical arrow moves the line, and with shift
        // copies it. Checked before the motion table, which otherwise
        // reads the same keys as plain cursor movement.
        if alt {
            let delta = match event.logical_key.as_ref() {
                Key::Named(NamedKey::ArrowUp) => Some(-1),
                Key::Named(NamedKey::ArrowDown) => Some(1),
                _ => None,
            };
            if let Some(delta) = delta {
                if shift {
                    editor.duplicate_lines();
                } else {
                    editor.move_lines(delta);
                }
                editor.scroll_to_cursor(rows);
                return true;
            }
        }

        let motion = match event.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowLeft) if alt => Some(Motion::WordLeft),
            Key::Named(NamedKey::ArrowRight) if alt => Some(Motion::WordRight),
            Key::Named(NamedKey::ArrowLeft) => Some(Motion::Left),
            Key::Named(NamedKey::ArrowRight) => Some(Motion::Right),
            Key::Named(NamedKey::ArrowUp) => Some(Motion::Up),
            Key::Named(NamedKey::ArrowDown) => Some(Motion::Down),
            Key::Named(NamedKey::Home) => Some(Motion::LineStart),
            Key::Named(NamedKey::End) => Some(Motion::LineEnd),
            _ => None,
        };
        if let Some(motion) = motion {
            editor.motion(motion, shift);
            editor.scroll_to_cursor(rows);
            return true;
        }

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::PageUp) | Key::Named(NamedKey::PageDown) => {
                let down = matches!(event.logical_key.as_ref(), Key::Named(NamedKey::PageDown));
                for _ in 0..rows.saturating_sub(2) {
                    editor.motion(if down { Motion::Down } else { Motion::Up }, shift);
                }
                editor.scroll_to_cursor(rows);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                // An empty pair goes as one, so backspacing the `(` of
                // `()` does not leave the `)` behind.
                if !editor.backspace_pair() {
                    editor.backspace();
                }
                editor.scroll_to_cursor(rows);
                true
            }
            Key::Named(NamedKey::Delete) => {
                editor.delete_forward();
                editor.scroll_to_cursor(rows);
                true
            }
            Key::Named(NamedKey::Enter) => {
                editor.newline();
                editor.scroll_to_cursor(rows);
                true
            }
            Key::Named(NamedKey::Tab) => {
                if shift {
                    editor.unindent();
                } else {
                    editor.indent();
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Collapse a multi-cursor set back to one, the way Escape
                // works everywhere else. Only close the window when there is
                // nothing to collapse, so it cannot throw away work by
                // surprise.
                if editor.sel.len() > 1 || !editor.sel.primary().is_empty() {
                    let head = editor.sel.primary().head;
                    editor.sel.collapse_to(Cursor::at(head));
                    true
                } else {
                    false
                }
            }
            _ => {
                let Some(text) = event.text.as_ref() else {
                    return false;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return false;
                }
                // A bracket or quote closes itself, a closer already
                // there is stepped over, and a selection is surrounded.
                // Anything else is just typed.
                if !editor.type_pair(&typed) {
                    editor.insert(&typed);
                }
                editor.scroll_to_cursor(rows);
                true
            }
        }
    }

    fn on_click(&mut self, el: &ActiveEventLoop) {
        let alt = self.modifiers.alt_key();
        let shift = self.modifiers.shift_key();
        let cmd = command_held(self.modifiers);
        let live: Vec<PathBuf> = self
            .recents
            .live()
            .into_iter()
            .map(Path::to_path_buf)
            .collect();
        let live: Vec<&Path> = live.iter().map(PathBuf::as_path).collect();

        // Chosen inside the pane borrow, acted on after it ends.
        let mut landing = None;
        let mut jump = false;
        let mut link = None;
        let mut status_click = None;
        let mut setting_click = None;
        let mut context_choice = None;
        let Some(id) = self.focused else { return };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        let l = pane.layout();
        let (x, y) = (pane.pointer.x as f32, pane.pointer.y as f32);
        pane.status = None;
        pane.touch();
        pane.dismiss_hover();

        let hit = view::hit_test(&l, &pane.panels(&live), x, y);
        // A link in the preview takes the click before the block it sits
        // in does, so that a link inside a fenced block is still a link.
        if matches!(hit, Hit::Elsewhere | Hit::PreviewCode(_)) {
            if let Some(lines) = pane.preview.as_deref() {
                let mut measure = Measured {
                    renderer: &pane.renderer,
                };
                link = view::preview_link_at(&l, lines, pane.preview_scroll, x, y, &mut measure);
            }
        }
        // Any click closes the menu, whether or not it chose something:
        // while one is open `hit_test` answers only about the menu, so a
        // click outside it would otherwise land nowhere and leave it up.
        let menu_choice = pane.context.take();
        let had_menu = menu_choice.is_some();

        match hit {
            // Handled below, once the pane is no longer borrowed.
            _ if link.is_some() => {}
            Hit::Status(button) => status_click = Some(button),
            Hit::Landing(action) => landing = Some(action),
            Hit::Tree(row) => {
                let Some(tree) = pane.tree.as_mut() else {
                    return;
                };
                // The row being typed into is not a file yet, so a click
                // on it does nothing but keep the caret there.
                if tree.edit_at == Some(row) {
                    return;
                }
                let Some(entry) = tree.rows.get(row) else {
                    return;
                };
                let (path, is_dir) = (entry.path.clone(), entry.is_dir);
                if is_dir {
                    tree.toggle(&path);
                    pane.last_tree_click = None;
                    return;
                }

                // Single click previews, double click keeps.
                const DOUBLE: std::time::Duration = std::time::Duration::from_millis(400);
                let now = std::time::Instant::now();
                let doubled = pane
                    .last_tree_click
                    .is_some_and(|(last, at)| last == row && now.duration_since(at) < DOUBLE);
                pane.last_tree_click = Some((row, now));

                pane.open_path_as(path, !doubled);
                if doubled {
                    pane.ws.pin_active();
                }
            }
            Hit::Tab(index) => {
                if index < pane.ws.len() {
                    pane.ws.active = index;
                    pane.retitle();
                    pane.apply_preview_default();
                }
            }
            Hit::TabClose(index) => {
                if index < pane.ws.len() {
                    pane.ws.active = index;
                    pane.ws.close_active();
                    pane.retitle();
                }
            }
            // A click on the new side of an editable diff places the
            // caret in the buffer, at the line that row shows.
            Hit::DiffText { row, column } => {
                let side_by_side = pane.preview_defaults.side_by_side_diff;
                let line = pane
                    .ws
                    .diff()
                    .and_then(|diff| row_line(diff, row, side_by_side));
                let Some(line) = line else { return };
                let Some(editor) = pane.ws.active_mut() else {
                    return;
                };
                // Diff line numbers are 1-based; buffer lines are not.
                let line = line.saturating_sub(1).min(editor.doc.len_lines() - 1);
                let start = editor.doc.line_to_char(line);
                let at = (start + column).min(start + editor.doc.line_len_chars(line));
                editor.sel.collapse_to(Cursor::at(at));
                pane.last_text_click = None;
            }
            Hit::Text => {
                // Count the click before the editor is borrowed.
                const MULTI: std::time::Duration = std::time::Duration::from_millis(400);
                let now = std::time::Instant::now();

                let Some(editor) = pane.ws.active_mut() else {
                    return;
                };
                let at = view::point_to_char(
                    &l,
                    &editor.doc,
                    editor.scroll,
                    editor.scroll_x,
                    x,
                    y,
                    TAB_WIDTH,
                );

                // A run of clicks has to stay in roughly one place; moving
                // across the file between them is two separate clicks.
                let count = match pane.last_text_click {
                    Some((last, when, n))
                        if now.duration_since(when) < MULTI && last.abs_diff(at) <= 1 =>
                    {
                        n.saturating_add(1).min(3)
                    }
                    _ => 1,
                };
                pane.last_text_click = Some((at, now, count));

                if count > 1 && !alt && !shift {
                    // Double and triple click ask about the character the
                    // pointer is over, not the nearest caret boundary.
                    let under = view::point_to_char_under(
                        &l,
                        &editor.doc,
                        editor.scroll,
                        editor.scroll_x,
                        x,
                        y,
                        TAB_WIDTH,
                    );
                    let span = if count == 2 {
                        editor.doc.word_at(under)
                    } else {
                        editor.doc.line_span(under)
                    };
                    editor.sel.collapse_to(lithe_core::Cursor {
                        anchor: span.start,
                        head: span.end,
                    });
                    pane.dragging = false;
                    return;
                }
                // Alt-click drops an extra cursor, as in Zed and VS Code.
                if alt {
                    editor.sel.add(Cursor::at(at));
                } else if shift {
                    editor.sel.drag_primary_to(at);
                } else {
                    editor.sel.collapse_to(Cursor::at(at));
                }
                // Cmd-click puts the caret down and then follows the
                // symbol under it, which is how everyone expects to
                // navigate. Dragging is suppressed so the jump is not
                // turned into a selection by a pixel of mouse movement.
                if cmd {
                    jump = true;
                } else {
                    pane.dragging = true;
                }
            }
            Hit::GitRow(row) => {
                if let Some(path) = pane
                    .git
                    .as_ref()
                    .and_then(|g| g.change_at(row))
                    .map(|c| c.path.clone())
                {
                    pane.show_diff(path);
                }
            }
            Hit::Context(_) if !had_menu => {}
            Hit::Context(row) => {
                // The menu was taken above, so the choice is read from
                // the entries that were on screen when it was clicked.
                context_choice = menu_choice
                    .as_ref()
                    .and_then(|m| Some((m.entries.get(row)?.action?, m.target.clone())));
            }
            Hit::Setting(setting) => setting_click = Some(setting),
            Hit::GitMode(mode) => {
                let repo = pane.repo.take();
                if let Some(panel) = pane.git.as_mut() {
                    panel.set_mode(mode, repo.as_ref());
                }
                pane.repo = repo;
            }
            // Clicking a fenced block in the preview copies it. A drawn
            // preview has no selection to drag, so this is the only way
            // the code in it gets out.
            Hit::PreviewCode(row) => {
                let text = pane
                    .preview
                    .as_ref()
                    .and_then(|lines| lines.get(row))
                    .and_then(|line| line.code.clone());
                let Some(text) = text else { return };
                let lines = text.lines().count();
                self.put_on_clipboard(
                    Some(text.to_string()),
                    if lines == 1 { "1 line" } else { "the block" },
                );
                return;
            }
            Hit::GitInit => {
                let root = pane.search_root();
                match lithe_vcs::Repo::init(&root) {
                    Ok(repo) => {
                        pane.repo = Some(repo);
                        pane.git_stale = true;
                        pane.status = Some(format!("git repository created in {}", root.display()));
                    }
                    Err(e) => pane.status = Some(e.to_string()),
                }
                let repo = pane.repo.take();
                if let Some(panel) = pane.git.as_mut() {
                    panel.refresh(repo.as_ref());
                }
                pane.repo = repo;
            }
            Hit::GitCommit(row) => {
                let chosen = pane
                    .git
                    .as_ref()
                    .and_then(|g| g.commit_at(row))
                    .map(|c| (c.hash.clone(), c.short.clone(), c.summary.clone()));
                if let (Some((hash, short, summary)), Some(repo)) = (chosen, pane.repo.as_ref()) {
                    let lines = repo.commit_diff(&hash);
                    pane.ws.show_commit(workspace::Diff {
                        // History is the past: there is no buffer behind
                        // it to type into, and nothing to rebuild it
                        // from.
                        editor: None,
                        // A commit's diff spans many files, so there is
                        // no single document to colour it against.
                        old: lithe_core::TextBuffer::new(),
                        old_syntax: None,
                        path: PathBuf::new(),
                        title: format!("{short}  {summary}"),
                        lines,
                        revision: u64::MAX,
                        scroll: 0.0,
                    });
                    pane.retitle();
                }
            }
            Hit::GitStage(row) => {
                let target = pane
                    .git
                    .as_ref()
                    .and_then(|g| g.change_at(row))
                    .map(|c| (c.path.clone(), c.staged));
                if let (Some((path, staged)), Some(repo)) = (target, pane.repo.as_ref()) {
                    let result = if staged {
                        repo.unstage(&path)
                    } else {
                        repo.stage(&path)
                    };
                    if let Err(e) = result {
                        pane.status = Some(format!("{e}"));
                    }
                    pane.git_stale = true;
                    pane.hunks_for = None;
                }
            }
            Hit::SidebarDivider => pane.resizing = true,
            Hit::Scrollbar(y) => {
                pane.scrub_to(y);
                pane.scrubbing = true;
            }
            Hit::ScrollbarX(x) => {
                pane.scrub_x_to(x);
                pane.scrubbing_x = true;
            }
            Hit::Elsewhere => {}
        }

        if let Some(href) = link {
            self.follow_link(&href);
        }
        if let Some(action) = landing {
            self.run_landing(el, action);
        }
        if jump {
            self.ask_lsp(Awaiting::Definition);
        }
        if let Some((action, target)) = context_choice {
            self.run_context(action, target);
        }
        if let Some(setting) = setting_click {
            setting.toggle(&mut self.settings);
            self.apply_settings();
        }
        if let Some(button) = status_click {
            let command = match button {
                view::StatusButton::Explorer => Command::ToggleTree,
                view::StatusButton::Git => Command::ToggleGitPanel,
                view::StatusButton::Search => Command::SearchProject,
                view::StatusButton::Preview => Command::MarkdownPreview,
            };
            self.run_command(el, command);
        }
    }

    fn run_landing(&mut self, el: &ActiveEventLoop, action: LandingAction) {
        match action {
            LandingAction::NewFile => self.run_command(el, Command::NewBuffer),
            LandingAction::OpenFile => self.run_command(el, Command::OpenFileDialog),
            LandingAction::OpenFolder => self.run_command(el, Command::OpenFolderDialog),
            LandingAction::NewWindow => self.run_command(el, Command::NewWindow),
            LandingAction::OpenRecent(index) => {
                let Some(root) = self.recents.live().get(index).map(|p| p.to_path_buf()) else {
                    return;
                };
                self.recents.remember(&root);
                if let Some(pane) = self.pane_mut() {
                    pane.open_folder(root);
                }
            }
        }
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if !self.panes.is_empty() {
            return;
        }
        // Wait, not Poll. There is no animation clock, so an idle editor
        // wakes for input and nothing else.
        el.set_control_flow(ControlFlow::Wait);
        // A document double-clicked in Finder arrives through the
        // delegate before the loop is running, so the first window
        // opens on it rather than on the landing page.
        let initial = self.initial.take().or_else(|| openfiles::take().pop());
        self.open_window(el, initial);
        if self.panes.is_empty() {
            el.exit();
        }
        self.open_handed_files(el);
    }

    /// Schedules the next timed wake-up, or goes back to waiting for
    /// input.
    ///
    /// Two things want a clock: the caret blink, which stops on its own
    /// after `BLINK_FOR`, and a pending autosave. Both are bounded and
    /// self-cancelling, so an untouched editor still settles to waking for
    /// nothing at all.
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let autosave = self.settings.autosave;
        let next = self.pane().and_then(|pane| {
            [
                pane.next_blink(),
                pane.next_autosave(autosave),
                pane.next_hover(),
            ]
            .into_iter()
            .flatten()
            .min()
        });
        match next {
            Some(at) => el.set_control_flow(ControlFlow::WaitUntil(at)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            self.autosave();
            self.dwell_hover();
            if let Some(pane) = self.pane() {
                pane.redraw();
            }
        }
    }

    /// A worker finished something: nucleo found more matches, or a menu
    /// item was clicked.
    fn user_event(&mut self, el: &ActiveEventLoop, _event: Wake) {
        self.open_handed_files(el);
        let clicked: Vec<_> = self
            .menu_events
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for id in clicked {
            if let Some(command) = menu::command_for(&id) {
                self.run_command(el, command);
                if let Some(pane) = self.pane() {
                    pane.redraw();
                }
            }
        }

        for (what, result) in self.lsp.drain() {
            self.on_lsp_reply(what, result);
        }
        self.refresh_marks();
        if let Some(pane) = self.pane() {
            pane.redraw();
        }

        if let Some(pane) = self.pane_mut() {
            let mut dirty = false;
            if let Some(picker) = pane.picker.as_mut() {
                if picker.tick() {
                    picker.clamp();
                    dirty = true;
                }
            }
            if let Some(search) = pane.search.as_mut() {
                dirty |= search.tick();
            }
            if dirty {
                pane.redraw();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Correlating events against CPU time is the only reliable way to
        // tell "the editor is spinning" from "the pointer is moving over
        // the window". Both look identical in a CPU graph.
        if self.trace {
            eprintln!("EV {event:?}");
        }
        if !self.panes.contains_key(&id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if !self.close_window(id) {
                    return; // cancelled; the window stays
                }
                if self.panes.is_empty() {
                    el.exit();
                }
            }

            // Becoming frontmost, and being uncovered, are the two
            // moments a window is first really on screen. Both redraw.
            //
            // Asking for a frame when the window is created is not
            // enough, and that is what made a bundle launched from
            // Finder come up blank: the frame is drawn before AppKit has
            // finished activating the app, so nothing composites it, and
            // the next frame only came when something else happened to
            // ask for one — a keystroke, or the pointer arriving. Run
            // from a terminal the pointer was usually already over the
            // window, which is why this looked fixed.
            // Coming back is also the moment to look at the disk again.
            //
            // Anything that changes a project from outside the editor —
            // a `git pull`, a branch switch, a build — happens while the
            // window is not the one being typed into, and you find out
            // by coming back to it. Re-reading here is what a watcher
            // would be for, without a thread, a dependency, or a queue
            // of events arriving when nobody is looking: the tree reads
            // only the directories that are expanded, so this costs what
            // drawing it costs.
            WindowEvent::Focused(true) => {
                self.focused = Some(id);
                // Coming back is when a checkout or another editor has
                // happened; look before anything is drawn or saved.
                self.refresh_from_disk(id);
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.refresh_tree();
                    // HEAD may be a different commit now, so the gutter's
                    // answer about this file is about the wrong one.
                    pane.hunks_for = None;
                    pane.blame_for = None;
                    pane.redraw();
                }
            }

            WindowEvent::Occluded(false) => {
                if let Some(pane) = self.panes.get(&id) {
                    pane.redraw();
                }
            }

            // Leaving the window is the other moment people expect their
            // work to be written.
            WindowEvent::Focused(false) => {
                if self.settings.autosave && self.focused == Some(id) {
                    if let Some(pane) = self.panes.get_mut(&id) {
                        if pane
                            .ws
                            .active()
                            .is_some_and(|e| e.dirty && e.path.is_some())
                        {
                            pane.save();
                            pane.status = None;
                        }
                    }
                }
            }

            // Leaving the window is leaving every element in it.
            WindowEvent::CursorLeft { .. } => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    if pane.set_hovered(Hit::Elsewhere) {
                        pane.redraw();
                    }
                }
            }

            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),

            WindowEvent::Resized(size) => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.renderer.resize(size.width, size.height);
                    pane.redraw();
                }
            }

            // Moving between displays changes the physical pixel size of a
            // glyph, so the atlas has to be rasterised again. Rebuilding
            // this window's renderer is the honest way to do that; the cost
            // is a few milliseconds.
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    match Renderer::new(pane.window.clone(), FONT_PT) {
                        Ok(renderer) => pane.renderer = renderer,
                        Err(e) => {
                            log::error!("could not rebuild renderer after DPI change: {e:#}")
                        }
                    }
                    pane.redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let live: Vec<PathBuf> = self
                    .recents
                    .live()
                    .into_iter()
                    .map(Path::to_path_buf)
                    .collect();
                let live: Vec<&Path> = live.iter().map(PathBuf::as_path).collect();
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                pane.pointer = position;
                let l = pane.layout();
                let (x, y) = (position.x as f32, position.y as f32);

                // An I-beam over text, an arrow over the chrome. Without
                // this the editor looks like a picture of an editor.
                let over = view::hit_test(&l, &pane.panels(&live), x, y);

                // Moving re-starts the dwell, and takes down a tooltip
                // once the pointer has left the character it was about.
                if pane.set_hovered(over) {
                    pane.redraw();
                }
                let was = pane.pointer_over_text;
                pane.pointer_over_text = matches!(over, Hit::Text);
                let moved_away = pane
                    .hover
                    .as_ref()
                    .is_some_and(|h| (h.at.0 - x).abs() + (h.at.1 - y).abs() > l.cell.advance);
                if moved_away || (was && !pane.pointer_over_text) {
                    pane.dismiss_hover();
                }
                pane.pointer_moved = std::time::Instant::now();

                // A link under the pointer makes it a hand, which is the
                // only thing that says a drawn document can be clicked.
                let on_link = match pane.preview.as_deref() {
                    Some(lines) if matches!(over, Hit::Elsewhere | Hit::PreviewCode(_)) => {
                        let mut measure = Measured {
                            renderer: &pane.renderer,
                        };
                        view::preview_link_at(&l, lines, pane.preview_scroll, x, y, &mut measure)
                            .is_some()
                    }
                    _ => false,
                };

                // While the wheel is held the pointer is a pan handle,
                // whatever it happens to be over.
                pane.window.set_cursor(if pane.panning.is_some() {
                    winit::window::CursorIcon::AllScroll
                } else if on_link {
                    winit::window::CursorIcon::Pointer
                } else {
                    match over {
                        // The new side of an editable diff is the buffer,
                        // so it gets the buffer's cursor.
                        Hit::Text | Hit::DiffText { .. } => winit::window::CursorIcon::Text,
                        Hit::Landing(_)
                        | Hit::Tab(_)
                        | Hit::TabClose(_)
                        | Hit::Tree(_)
                        | Hit::GitRow(_)
                        | Hit::GitStage(_)
                        | Hit::GitCommit(_)
                        | Hit::GitMode(_)
                        | Hit::Status(_) => winit::window::CursorIcon::Pointer,
                        Hit::SidebarDivider => winit::window::CursorIcon::ColResize,
                        _ => winit::window::CursorIcon::Default,
                    }
                });

                // Panning first: it is a button being held, like the
                // others below, and it takes the pointer with it.
                if let Some(from) = pane.panning {
                    let dx = (position.x - from.x) as f32;
                    let dy = (position.y - from.y) as f32;
                    pane.panning = Some(position);
                    // The document follows the pointer rather than
                    // running away from it: dragging down pulls the text
                    // down, the way dragging a sheet of paper does.
                    pane.scroll_text(dy / l.cell.line_height, dx / l.cell.advance);
                    pane.redraw();
                    return;
                }
                if pane.resizing {
                    pane.sidebar_cols =
                        (x / l.cell.advance).clamp(view::SIDEBAR_MIN, view::SIDEBAR_MAX);
                    pane.redraw();
                    return;
                }
                if pane.scrubbing {
                    pane.scrub_to(y);
                    pane.redraw();
                    return;
                }
                if pane.scrubbing_x {
                    pane.scrub_x_to(x);
                    pane.redraw();
                    return;
                }
                if !pane.dragging {
                    return;
                }
                if let Some(editor) = pane.ws.active_mut() {
                    let at = view::point_to_char(
                        &l,
                        &editor.doc,
                        editor.scroll,
                        editor.scroll_x,
                        x,
                        y,
                        TAB_WIDTH,
                    );
                    editor.sel.drag_primary_to(at);
                    pane.redraw();
                }
            }

            // The wheel pressed down and dragged pans the view, which is
            // the one way to scroll sideways that needs no keyboard and no
            // trackpad. Not the auto-scroll that Windows does on the same
            // button: that keeps moving while the pointer sits still,
            // which means a clock ticking frames out, and this editor
            // redraws on damage and nothing else.
            WindowEvent::MouseInput {
                button: MouseButton::Middle,
                state,
                ..
            } => {
                self.focused = Some(id);
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.panning = match state {
                        ElementState::Pressed => Some(pane.pointer),
                        ElementState::Released => None,
                    };
                }
            }

            WindowEvent::MouseInput {
                button: MouseButton::Right,
                state: ElementState::Pressed,
                ..
            } => {
                self.focused = Some(id);
                self.open_context_menu(id);
                if let Some(pane) = self.panes.get(&id) {
                    pane.redraw();
                }
            }

            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state,
                ..
            } => {
                self.focused = Some(id);
                match state {
                    ElementState::Pressed => {
                        // A click outside the overlay dismisses it.
                        let had_picker = self
                            .panes
                            .get(&id)
                            .is_some_and(|pane| pane.picker.is_some());
                        if had_picker {
                            if let Some(pane) = self.panes.get_mut(&id) {
                                pane.picker = None;
                            }
                        } else {
                            self.on_click(el);
                        }
                    }
                    ElementState::Released => {
                        if let Some(pane) = self.panes.get_mut(&id) {
                            pane.dragging = false;
                            pane.scrubbing = false;
                            pane.scrubbing_x = false;
                            pane.resizing = false;
                        }
                    }
                }
                if let Some(pane) = self.panes.get(&id) {
                    pane.redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let cell = pane.renderer.cell();
                let (line_height, advance) = (cell.line_height, cell.advance);
                let (mut lines, mut columns) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (y * WHEEL_LINES, x * WHEEL_LINES),
                    MouseScrollDelta::PixelDelta(p) => {
                        (p.y as f32 / line_height, p.x as f32 / advance)
                    }
                };
                // A mouse with one wheel scrolls sideways with shift held,
                // which is the convention everywhere and the only way to
                // reach a long line without a trackpad. A wheel that
                // really is horizontal already arrived as one.
                if self.modifiers.shift_key() && columns == 0.0 {
                    columns = lines;
                    lines = 0.0;
                }

                // Scrolling moves the text out from under the tooltip,
                // which would otherwise point at whatever arrived there.
                pane.dismiss_hover();

                // Scroll whichever panel the pointer is over.
                let tree_w = pane.layout().tree_w;
                let over_tree = tree_w > 0.0 && (pane.pointer.x as f32) < tree_w;
                if over_tree {
                    if let Some(tree) = pane.tree.as_mut() {
                        let max = tree.rows.len().saturating_sub(1);
                        let next = (tree.scroll as f32 - lines).round();
                        tree.scroll = next.clamp(0.0, max as f32) as usize;
                    }
                } else {
                    pane.scroll_text(lines, columns);
                }
                pane.redraw();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.focused = Some(id);
                let pane = self.panes.get(&id);
                let in_search = pane.is_some_and(|p| p.search.is_some());
                let in_picker = pane.is_some_and(|p| p.picker.is_some());
                let in_find = pane.is_some_and(|p| p.find.is_some());
                let in_prompt = pane.is_some_and(|p| p.prompt.is_some());
                let in_menu = pane.is_some_and(|p| p.context.is_some());
                let naming =
                    pane.is_some_and(|p| p.tree.as_ref().is_some_and(|t| t.editing.is_some()));

                // A command-layer shortcut still works while a bar is open,
                // so cmd+shift+F from the find bar widens to the project.
                let shortcut = command_held(self.modifiers)
                    && !matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Enter));

                // The popup gets first refusal; what it declines falls
                // through to the editor and then narrows the list.
                if let Some(handled) = self.on_completion_key(&event) {
                    if handled {
                        if let Some(pane) = self.panes.get(&id) {
                            pane.redraw();
                        }
                    }
                    return;
                }
                let typing = event.text.clone();

                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.touch();
                    pane.dismiss_hover();
                }
                let handled = if in_menu {
                    self.on_menu_key(&event)
                } else if naming {
                    self.on_naming_key(&event)
                } else if in_prompt {
                    self.on_prompt_key(&event)
                } else if in_search {
                    self.on_search_key(&event)
                } else if in_picker {
                    self.on_picker_key(el, &event)
                } else if in_find && !shortcut {
                    self.on_find_key(&event)
                } else {
                    self.on_key(el, &event)
                };
                if handled {
                    self.narrow_completion(typing.as_deref());
                    self.maybe_complete(typing.as_deref());
                    if let Some(pane) = self.panes.get(&id) {
                        pane.redraw();
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                self.sync_lsp();
                self.refresh_marks();
                if self.panes.get(&id).is_some_and(|p| p.status.is_none()) {
                    if let Some(message) = self.diagnostic_at_cursor() {
                        if let Some(pane) = self.panes.get_mut(&id) {
                            pane.status = Some(message);
                        }
                    }
                }
                let App {
                    theme,
                    recents,
                    panes,
                    settings,
                    ..
                } = self;
                let autosave = settings.autosave;
                let live = recents.live();
                if let Some(pane) = panes.get_mut(&id) {
                    if let Some(picker) = pane.picker.as_mut() {
                        // Advance matching before drawing, so the overlay
                        // shows the freshest results the workers have.
                        picker.tick();
                        picker.clamp();
                    }
                    if let Some(search) = pane.search.as_mut() {
                        search.tick();
                    }
                    pane.draw(theme, &live, autosave);
                }
            }

            _ => {}
        }
    }
}

/// How long until this buffer should be written, if it should be at all.
///
/// Split out from the pane so the rules can be tested without a window:
/// off means never, a clean buffer has nothing to write, and a buffer with
/// no path has nowhere to write it — inventing a filename for a scratch
/// buffer would be worse than leaving it unsaved.
fn autosave_delay(
    enabled: bool,
    dirty: bool,
    has_path: bool,
    since_input: std::time::Duration,
) -> Option<std::time::Duration> {
    if !enabled || !dirty || !has_path {
        return None;
    }
    Some(AUTOSAVE_AFTER.saturating_sub(since_input))
}

#[cfg(test)]
mod autosave_tests {
    use super::{autosave_delay, AUTOSAVE_AFTER};
    use std::time::Duration;

    #[test]
    fn nothing_is_written_when_autosave_is_off() {
        assert!(autosave_delay(false, true, true, Duration::ZERO).is_none());
    }

    #[test]
    fn a_clean_buffer_is_left_alone() {
        assert!(autosave_delay(true, false, true, Duration::ZERO).is_none());
    }

    #[test]
    fn a_scratch_buffer_has_nowhere_to_go() {
        assert!(
            autosave_delay(true, true, false, Duration::ZERO).is_none(),
            "a buffer with no path must never be given one silently"
        );
    }

    #[test]
    fn the_wait_counts_down_from_the_last_keystroke() {
        let fresh = autosave_delay(true, true, true, Duration::ZERO).unwrap();
        assert_eq!(fresh, AUTOSAVE_AFTER);

        let halfway = autosave_delay(true, true, true, AUTOSAVE_AFTER / 2).unwrap();
        assert!(halfway < fresh && !halfway.is_zero());
    }

    #[test]
    fn a_long_pause_is_due_immediately_rather_than_overdue() {
        let due = autosave_delay(true, true, true, Duration::from_secs(60)).unwrap();
        assert!(due.is_zero(), "saturating, not negative");
    }
}

/// What to do about unsaved work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decision {
    Save,
    Discard,
    Cancel,
}

/// Asks before throwing away edits.
///
/// A native dialog rather than something drawn in the window: this is the
/// one moment where the editor must be sure it has the person's attention,
/// and the platform already knows how to do that.
fn confirm_discard(what: &str, window: &Window) -> Decision {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    let result = MessageDialog::new()
        .set_parent(window)
        .set_level(MessageLevel::Warning)
        .set_title("Unsaved changes")
        .set_description(format!("{what} has unsaved changes."))
        .set_buttons(MessageButtons::YesNoCancelCustom(
            "Save".into(),
            "Discard".into(),
            "Cancel".into(),
        ))
        .show();
    match result {
        MessageDialogResult::Yes | MessageDialogResult::Ok => Decision::Save,
        MessageDialogResult::No => Decision::Discard,
        MessageDialogResult::Custom(label) if label == "Save" => Decision::Save,
        MessageDialogResult::Custom(label) if label == "Discard" => Decision::Discard,
        // Cancel, or a dialog dismissed some other way: keep the work.
        _ => Decision::Cancel,
    }
}

/// The places a call-hierarchy reply names.
///
/// Incoming calls arrive as `from`, outgoing as `to`; either way what we
/// want out of them is a file and a line.
fn call_locations(result: &serde_json::Value, incoming: bool) -> Vec<(PathBuf, u64)> {
    let key = if incoming { "from" } else { "to" };
    let Some(items) = result.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|call| call.get(key))
        .flat_map(locations)
        .collect()
}

/// Shortens a label to fit a menu, with an ellipsis if it had to.
fn trim_to(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        return text.to_string();
    }
    let kept: String = text.chars().take(columns.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Every location in a reply, as (path, 0-based line).
///
/// Servers answer with a `Location`, a `LocationLink`, or a list of
/// either, so all four shapes are read the same way.
fn locations(result: &serde_json::Value) -> Vec<(PathBuf, u64)> {
    use std::str::FromStr;
    let items: Vec<&serde_json::Value> = match result {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![result],
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|one| {
            let uri = one
                .get("uri")
                .or_else(|| one.get("targetUri"))
                .and_then(|v| v.as_str())?;
            let line = one
                .get("range")
                .or_else(|| one.get("targetSelectionRange"))
                .or_else(|| one.get("selectionRange"))
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let uri = lithe_lsp::Uri::from_str(uri).ok()?;
            Some((lithe_lsp::uri_to_path(&uri)?, line))
        })
        .collect()
}

fn parse_position(value: &serde_json::Value) -> Option<lithe_lsp::Position> {
    Some(lithe_lsp::Position {
        line: value.get("line")?.as_u64()? as u32,
        character: value.get("character")?.as_u64()? as u32,
    })
}

/// Reads an array of `TextEdit`s.
fn text_edits(value: &serde_json::Value) -> Vec<(lithe_lsp::Range, String)> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|edit| {
            let range = edit.get("range")?;
            Some((
                lithe_lsp::Range {
                    start: parse_position(range.get("start")?)?,
                    end: parse_position(range.get("end")?)?,
                },
                edit.get("newText")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Reads a `WorkspaceEdit`, which servers send in either of two shapes.
fn workspace_edits(result: &serde_json::Value) -> Vec<(PathBuf, Vec<(lithe_lsp::Range, String)>)> {
    use std::str::FromStr;
    let to_path = |uri: &str| {
        lithe_lsp::Uri::from_str(uri)
            .ok()
            .as_ref()
            .and_then(lithe_lsp::uri_to_path)
    };
    let mut out = Vec::new();

    if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(path) = to_path(uri) {
                out.push((path, text_edits(edits)));
            }
        }
    }
    if let Some(documents) = result.get("documentChanges").and_then(|c| c.as_array()) {
        for document in documents {
            // A documentChanges list can also hold create/rename/delete
            // operations; those have no `edits` and are skipped.
            let Some(uri) = document
                .get("textDocument")
                .and_then(|d| d.get("uri"))
                .and_then(|u| u.as_str())
            else {
                continue;
            };
            if let (Some(path), Some(edits)) = (to_path(uri), document.get("edits")) {
                out.push((path, text_edits(edits)));
            }
        }
    }
    out.retain(|(_, edits)| !edits.is_empty());
    out
}

/// Applies edits to a file that is not open, and writes it back.
fn edit_on_disk(path: &Path, edits: Vec<(lithe_lsp::Range, String)>) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut buffer = lithe_core::TextBuffer::from_text(&text);
    let mut spans: Vec<_> = edits
        .into_iter()
        .map(|(range, with)| (lithe_lsp::range_to_chars(&buffer, range), with))
        .filter(|(span, _)| span.end <= buffer.len_chars())
        .collect();
    // Back to front, so an earlier edit does not shift a later one.
    spans.sort_by_key(|(span, _)| span.start);
    let count = spans.len();
    for (span, with) in spans.into_iter().rev() {
        buffer.remove(span.clone());
        if !with.is_empty() {
            buffer.insert(span.start, &with);
        }
    }

    let Ok(file) = std::fs::File::create(path) else {
        return 0;
    };
    match buffer.write_to(std::io::BufWriter::new(file)) {
        Ok(()) => count,
        Err(e) => {
            log::warn!("could not write {}: {e}", path.display());
            0
        }
    }
}

/// What the file clipboard holds.
#[derive(Clone)]
struct FileClip {
    path: PathBuf,
    /// True when it was cut, so pasting moves rather than copies.
    cut: bool,
}

/// The identifier characters immediately before the cursor.
///
/// What a completion list should be filtered by, and what accepting one
/// replaces. Read from the buffer rather than tracked as the popup is
/// typed into, because the list arrives from the server some time after
/// the keystroke that asked for it.
fn typed_prefix(editor: &editor::Editor) -> String {
    let head = editor.sel.primary().head;
    let line = editor.doc.char_to_line(head);
    let start = editor.doc.line_to_char(line);
    let text = editor.doc.slice_to_string(start..head);
    let kept = text
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .count();
    text.chars().skip(text.chars().count() - kept).collect()
}

/// Whether a pointer that has come to rest should be asked about.
///
/// Pure, so the one rule that keeps the editor from spinning on its own
/// timer can be tested without a window: a rest is considered once, and
/// not again until the pointer moves.
fn dwell_due(
    over_text: bool,
    already_asked: bool,
    showing: bool,
    resting: std::time::Duration,
) -> bool {
    over_text && !already_asked && !showing && resting >= HOVER_DWELL
}

/// A path's file name, for a message about it.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Which line of the working copy a diff row shows.
///
/// In the split view a row is a pair of sides and only the right one is
/// the working copy; in the unified view a row is one diff line. Both
/// answer the same question, which is what a click needs to know.
fn row_line(view: &workspace::Diff, row: usize, side_by_side: bool) -> Option<usize> {
    if side_by_side {
        let (_, right) = *lithe_vcs::pair_columns(&view.lines).get(row)?;
        view.lines.get(right?)?.new
    } else {
        view.lines.get(row)?.new
    }
}

/// A filesystem change the explorer can take back.
///
/// Every variant's inverse is another variant, so undoing produces the op
/// that redoes it and the two stacks stay symmetrical with no second
/// enum describing the same three things backwards.
enum FileOp {
    /// Something was brought into existence; undoing removes it.
    Added(PathBuf),
    /// Something moved; undoing moves it back.
    Moved { from: PathBuf, to: PathBuf },
    /// Something was taken away, with the Trash copy at `backup`.
    Removed { path: PathBuf, backup: PathBuf },
}

impl FileOp {
    /// Reverses this change and returns the op that would reverse *that*.
    fn undo(self) -> std::io::Result<FileOp> {
        match self {
            FileOp::Added(path) => {
                let backup = move_to_trash(&path)?;
                Ok(FileOp::Removed { path, backup })
            }
            FileOp::Moved { from, to } => {
                std::fs::rename(&to, &from)?;
                Ok(FileOp::Moved { from: to, to: from })
            }
            FileOp::Removed { path, backup } => {
                std::fs::rename(&backup, &path)?;
                Ok(FileOp::Added(path))
            }
        }
    }

    fn label(&self) -> &'static str {
        match self {
            FileOp::Added(_) => "create",
            FileOp::Moved { .. } => "move",
            FileOp::Removed { .. } => "trash",
        }
    }
}

/// The absolute path to something, with the symlinks and the `..` taken
/// out of it — but spelled the way the rest of the system spells it.
///
/// `canonicalize` on Windows hands back an extended-length path:
/// `\\?\C:\code\lithe`, which is the same directory with the Win32 path
/// limit lifted. That prefix would be in the window title, on the landing
/// page, in the recent-projects file and in front of every path the editor
/// shows, so it is taken off again. It stays on a UNC path, where it is
/// load-bearing, and on anything long enough to have needed it.
fn real_path(path: &Path) -> PathBuf {
    let canonical = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => return path.to_path_buf(),
    };
    if !cfg!(target_os = "windows") {
        return canonical;
    }
    let text = canonical.to_string_lossy();
    let Some(bare) = text.strip_prefix(r"\\?\") else {
        return canonical;
    };
    // A drive path only, and only while it is short enough that Windows
    // itself would accept it without the prefix.
    let drive = bare.as_bytes();
    let is_drive = drive.len() > 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':';
    if is_drive && bare.len() < 260 {
        PathBuf::from(bare)
    } else {
        canonical
    }
}

/// Whether a link target is a url rather than a path beside the file.
///
/// A scheme and `//`, or one of the schemes that has no authority to
/// follow it. A single letter is a drive rather than a scheme, and a bare
/// `www.example.com` has no scheme at all — as far as anything local is
/// concerned that is a file name, and treating it as a url would open a
/// browser for a path that merely looks like one.
fn is_url(target: &str) -> bool {
    let Some((scheme, rest)) = target.split_once(':') else {
        return false;
    };
    let named = scheme.len() > 1
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    named
        && (rest.starts_with("//")
            || matches!(
                scheme.to_ascii_lowercase().as_str(),
                "mailto" | "tel" | "sms"
            ))
}

/// The command that opens a url in whatever the system opens urls with.
fn url_opener(url: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = spawnable("open");
        command.arg(url);
        command
    }
    // Not through `cmd`: a query string holds `&`, which a shell reads
    // as its own, and this takes the whole of the url as one argument.
    #[cfg(target_os = "windows")]
    {
        let mut command = spawnable("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = spawnable("xdg-open");
        command.arg(url);
        command
    }
}

/// Percent-decoding, for a link written as `docs/a%20file.md`.
///
/// Only what a link in a document actually carries: a run that is not two
/// hex digits is left as the three characters it is, which is what a file
/// genuinely called `100%.md` needs.
fn unescape(target: &str) -> String {
    let bytes = target.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let decoded = (bytes[at] == b'%' && at + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[at + 1..at + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| target.to_string())
}

/// A command that will not open a console window of its own.
///
/// Every program the editor hands a path to — Explorer, `cmd`, a terminal
/// — is a console program, and Windows gives one a console unless told
/// otherwise: a black box would flash over the editor every time someone
/// chose Reveal or Open in Terminal. Elsewhere this is just `Command::new`.
fn spawnable(program: &str) -> std::process::Command {
    let command = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Moves a path to the trash and says where it landed.
///
/// The trash rather than `remove_file`, because an editor should not make
/// a file unrecoverable on a menu click.
fn move_to_trash(path: &Path) -> std::io::Result<PathBuf> {
    let trash = trash_dir()?;
    std::fs::create_dir_all(&trash)?;
    let name = path.file_name().unwrap_or_default();
    let destination = free_name(&trash.join(name));
    match std::fs::rename(path, &destination) {
        Ok(()) => Ok(destination),
        // A rename cannot cross a volume, and the trash is on one
        // particular volume: a file on a second drive has to be copied
        // over and then removed.
        Err(_) => {
            if path.is_dir() {
                copy_tree(path, &destination)?;
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::copy(path, &destination)?;
                std::fs::remove_file(path)?;
            }
            Ok(destination)
        }
    }
}

/// Where a trashed file goes.
///
/// macOS: `~/.Trash`, which is the Finder's own, so what the editor
/// trashed is in the Trash the person already knows how to empty.
#[cfg(not(target_os = "windows"))]
fn trash_dir() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
    Ok(home.join(".Trash"))
}

/// Windows: a directory of lithe's own, beside its settings.
///
/// Not the Recycle Bin, deliberately. Putting a file there means asking
/// the shell to do it, and the shell does not say where it put it — while
/// Undo in the file tree is built on knowing exactly that. A folder that
/// Undo can reach beats an entry in a bin it cannot.
#[cfg(target_os = "windows")]
fn trash_dir() -> std::io::Result<PathBuf> {
    settings::config_dir()
        .map(|dir| dir.join("trash"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no settings directory"))
}

/// `path` if nothing is there, else the same name with a number on it.
fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| parent.join(format!("{stem} {n}{extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Removes the indentation every non-blank line shares.
fn trim_indent(text: &str) -> String {
    let shared = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line.get(shared..).unwrap_or(""));
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Recursively copies a directory, for Duplicate.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Pulls the Markdown out of a `textDocument/hover` reply.
///
/// The protocol allows three shapes here, two of them deprecated, and
/// servers use all three.
///
/// Returned as written, fences and all. It used to be flattened to one
/// line with the fences stripped, because it went in the status bar —
/// which threw away the one thing a signature has, its shape. It now
/// goes through the preview's Markdown reader and is drawn as a small
/// document, so the structure is the point.
fn hover_text(result: &serde_json::Value) -> Option<String> {
    let contents = result.get("contents")?;
    let text = match contents {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => map
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        // Separate sections, so each keeps its own block structure.
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(text) => Some(text.clone()),
                other => other
                    .get("value")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// First target of a `textDocument/definition` reply, as (path, 0-based
/// line). The reply may be one location, several, or a "link" with extra
/// range information.
fn first_location(result: &serde_json::Value) -> Option<(PathBuf, u64)> {
    let one = match result {
        serde_json::Value::Array(items) => items.first()?,
        serde_json::Value::Object(_) => result,
        _ => return None,
    };
    let uri = one
        .get("uri")
        .or_else(|| one.get("targetUri"))
        .and_then(|v| v.as_str())?;
    let range = one.get("range").or_else(|| one.get("targetSelectionRange"));
    let line = range
        .and_then(|r| r.get("start"))
        .and_then(|s| s.get("line"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    use std::str::FromStr;
    let uri = lithe_lsp::Uri::from_str(uri).ok()?;
    Some((lithe_lsp::uri_to_path(&uri)?, line))
}

/// Flattens a completion reply, which is either a list or an object with
/// the list inside it.
fn completion_items(result: &serde_json::Value) -> Vec<find::Entry> {
    let items = match result {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            // insertText wins over the label when the two differ, since the
            // label is often decorated for display.
            let insert = item
                .get("insertText")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    item.get("textEdit")
                        .and_then(|e| e.get("newText"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| label.clone());
            let detail = item
                .get("detail")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(find::Entry {
                label,
                insert,
                detail,
            })
        })
        .collect()
}

/// Renders one frame offscreen and writes raw RGBA. Lets the real drawing
/// code be inspected pixel-for-pixel without a display, which is the only
/// honest way to check a hand-rolled text renderer.
fn snapshot(arg: Option<PathBuf>, out: &std::path::Path, overlay: Option<&str>) -> Result<()> {
    const W: u32 = 1800;
    const H: u32 = 1200;
    const SCALE: f32 = 2.0;

    let mut renderer =
        Renderer::offscreen(W, H, SCALE, FONT_PT).context("could not create offscreen renderer")?;

    let (mut ws, tree) = match arg {
        Some(path) if path.is_dir() => {
            let root = real_path(&path);
            (Workspace::empty(Some(root.clone())), Some(Tree::new(root)))
        }
        Some(path) => {
            let mut ws = Workspace::empty(path.parent().map(PathBuf::from));
            ws.push(Editor::open(&path, TAB_WIDTH)?);
            (ws, None)
        }
        None => (Workspace::empty(None), None),
    };

    let picker = (overlay == Some("picker")).then(|| {
        let root = ws
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut picker = Picker::files(&root, Arc::new(|| {}));
        // Give the walk a moment, so the overlay is not drawn empty.
        for _ in 0..200 {
            picker.tick();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        picker.set_query("rend".into());
        for _ in 0..100 {
            picker.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        picker
    });

    // The offscreen renderer has no window, so the row count comes from its
    // own geometry rather than from a layout that assumes one.
    let rows = (H as f32 / renderer.cell().line_height).ceil() as usize;
    let mut highlights = Vec::new();
    if let Some(editor) = ws.active_mut() {
        let range = {
            let doc = &editor.doc;
            let last_line = doc.len_lines().saturating_sub(1);
            let last = rows.min(last_line);
            let end_char = (doc.line_to_char(last) + doc.line_len_chars(last)).min(doc.len_chars());
            0..doc.rope().char_to_byte(end_char)
        };
        editor.highlights(range, &mut highlights);
    }

    // The real pane decodes images; the snapshot has to as well or it
    // cannot show one.
    let picture = ws
        .active()
        .and_then(|e| e.path.clone())
        .filter(|p| imageview::is_image(p))
        .and_then(|p| imageview::load(&renderer, &p).ok());

    let theme = Theme::default();
    let recents = Recents::load();
    let live = recents.live();
    // Overlays, so the chrome can be eyeballed without driving the UI.
    let mut find = None;
    let mut find_matches = Vec::new();
    if overlay == Some("find") {
        let mut bar = Find::new(true);
        bar.set_query("fn ".into());
        bar.replace = "pub fn ".into();
        if let (Some(editor), Some(pattern)) = (ws.active(), bar.pattern.as_ref()) {
            let last = rows.min(editor.doc.len_lines().saturating_sub(1));
            let end = editor.doc.line_to_char(last);
            lithe_search::matches_in_range(&editor.doc, pattern, 0..end, &mut find_matches);
        }
        find = Some(bar);
    }
    let search = (overlay == Some("search")).then(|| {
        let root = ws
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut search = ProjectSearch::new(root, Arc::new(|| {}));
        search.set_query("fn ".into());
        for _ in 0..200 {
            search.tick();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        search
    });

    // The real pane discovers the repository and diffs the active file;
    // the snapshot has to do the same or it cannot show the gutter.
    let repo = ws
        .active()
        .and_then(|e| e.path.as_deref())
        .and_then(|p| p.parent())
        .or(ws.root.as_deref())
        .and_then(lithe_vcs::Repo::discover);
    let mut hunks = Vec::new();
    if let (Some(repo), Some(editor)) = (repo.as_ref(), ws.active()) {
        if let Some(path) = editor.path.as_deref() {
            if let Some(committed) = repo.committed(path) {
                hunks = lithe_vcs::hunks(&committed, &editor.doc.rope().to_string());
            }
        }
    }
    let branch = repo.as_ref().and_then(lithe_vcs::Repo::branch);

    // A diff of the active file, for eyeballing the view.
    // Through the workspace, as the real pane does, so the snapshot
    // shows the tab a diff opens in rather than a diff with no tab.
    if matches!(overlay, Some("diff") | Some("split")) {
        if let (Some(repo), Some(path)) = (repo.as_ref(), ws.active().and_then(|e| e.path.clone()))
        {
            let committed = repo.committed(&path).unwrap_or_default();
            let current = std::fs::read_to_string(&path).unwrap_or_default();
            // Parsed here as `build_diff` parses it. A syntax that was
            // made and not parsed has no tree and yields no spans, which
            // would make the snapshot show a plain diff and call it
            // correct.
            let old = lithe_core::TextBuffer::from_text(&committed);
            let mut old_syntax = lithe_syntax::Syntax::for_path(&path);
            if let Some(syntax) = old_syntax.as_mut() {
                syntax.parse(&old);
            }
            let diff = workspace::Diff {
                editor: None,
                old,
                old_syntax,
                title: repo.relative(&path).unwrap_or_default(),
                path: path.clone(),
                lines: lithe_vcs::unified(&committed, &current, 3),
                revision: 0,
                scroll: 0.0,
            };
            ws.show_diff(&path, diff);
        }
    }
    if overlay == Some("settings") {
        ws.show_settings();
    }

    // Both sides of a diff, as `refresh_diff_highlights` computes them.
    // A still frame is always at the top, so the window is the first
    // `rows` lines of each document rather than a scrolled slice.
    let whole = |doc: &lithe_core::TextBuffer| {
        let last = rows.min(doc.len_lines().saturating_sub(1));
        let end = (doc.line_to_char(last) + doc.line_len_chars(last)).min(doc.len_chars());
        0..doc.rope().char_to_byte(end)
    };
    let mut diff_old_spans = Vec::new();
    let mut diff_new_spans = Vec::new();
    if let Some(diff) = ws.diff_mut() {
        let doc = std::mem::take(&mut diff.old);
        if let Some(syntax) = diff.old_syntax.as_mut() {
            syntax.highlights(&doc, whole(&doc), &mut diff_old_spans);
        }
        diff.old = doc;
        if let Some(editor) = diff.editor.as_mut() {
            let range = whole(&editor.doc);
            editor.highlights(range, &mut diff_new_spans);
        }
    }

    let git_panel = matches!(overlay, Some("git") | Some("history")).then(|| {
        let mut panel = GitPanel::new(repo.as_ref());
        if overlay == Some("history") {
            panel.set_mode(gitpanel::Mode::History, repo.as_ref());
        }
        panel
    });
    // Laid out exactly as the running editor lays it out: the pane it
    // has, and the faces it will be drawn in. A snapshot wrapped to a
    // number picked here would be a picture of a preview nobody sees.
    let md = (overlay == Some("preview"))
        .then(|| {
            let editor = ws.active()?;
            let l = view::layout(
                &renderer,
                &ws,
                Chrome {
                    sidebar: tree.is_some(),
                    sidebar_cols: view::TREE_COLS,
                    find: false,
                },
            );
            let mut measure = Measured {
                renderer: &renderer,
            };
            Some(markdown::render_with(
                &editor.doc.rope().to_string(),
                view::preview_columns(&l),
                &mut measure,
            ))
        })
        .flatten();

    let prompt = (overlay == Some("prompt")).then(|| {
        let mut prompt = Prompt::commit();
        prompt.text = "tidy up the renderer".into();
        prompt
    });

    let defaults = Settings::default();

    let hover = (overlay == Some("hover")).then(|| view::Hover {
        lines: markdown::render_boxed(
            "```rust\npub fn blame_file(&self, path: &Path) -> Option<FileBlame>\n```\n\n             Who last touched each line of a file.\n\nThe whole file in one `git blame`,              not a subprocess per line: the annotation follows the cursor.",
            view::Hover::COLUMNS,
        ),
        at: (420.0, 520.0),
    });

    let context = match overlay {
        Some("menu") => Some(ContextMenu::for_editor(
            (520.0, 300.0),
            ws.active().and_then(|e| e.path.clone()),
            true,
            repo.is_some(),
            Some(false),
            context::GoesTo::ALL,
        )),
        Some("explorer-menu") => Some(ContextMenu::for_explorer(
            (120.0, 260.0),
            tree.as_ref()
                .and_then(|t| t.rows.first())
                .map(|r| r.path.clone()),
            false,
            &context::State {
                can_paste: true,
                can_undo: true,
                can_redo: false,
                undo_label: "move".into(),
                redo_label: String::new(),
            },
        )),
        _ => None,
    };

    // A name being typed in the tree, so the inline edit row is drawn.
    let tree = match (overlay, tree) {
        (Some("naming"), Some(mut tree)) => {
            tree.begin_edit(tree::Editing::NewFile(tree.root.clone()));
            tree.push_typed("notes.md");
            Some(tree)
        }
        (_, tree) => tree,
    };

    let scene = Scene {
        workspace: &ws,
        blame: (overlay == Some("blame"))
            .then_some("Ada Lovelace, 3 days ago · tidy up the renderer"),
        recents: &live,
        diagnostics: &[],
        hunks: &hunks,
        branch: branch.as_deref(),
        lsp: None,
        lsp_busy: None,
        autosave: false,
        // `--overlay hovered` parks the pointer on a tree row, which
        // is the only way to see a hover state in a still frame.
        // Distinct from `hover`, which is the language server's popup.
        hovered: (overlay == Some("hovered")).then_some(view::Hit::Tree(2)),
        image: picture.as_ref(),
        settings: ws.showing_settings().then_some(&defaults),
        context: context.as_ref(),
        prompt: prompt.as_ref(),
        caret_visible: true,
        completion: None,
        find: find.as_ref(),
        search: search.as_ref(),
        find_matches: &find_matches,
        highlights: &highlights,
        diff_old: ws.diff().map(|d| &d.old),
        diff_old_spans: &diff_old_spans,
        diff_new_spans: &diff_new_spans,
        tree: tree.as_ref(),
        git: git_panel.as_ref(),
        sidebar_cols: view::TREE_COLS,
        preview: md.as_deref(),
        preview_scroll: 0.0,
        diff: ws.diff().map(|d| d.lines.as_slice()),
        diff_title: ws.diff().map(|d| d.title.as_str()),
        diff_scroll: 0.0,
        diff_side_by_side: overlay == Some("split"),
        // Both overlays build a diff of the active working file, which
        // is the editable kind. History's read-only diff is a different
        // thing and is not what either one shows.
        diff_editable: ws.diff().is_some(),
        hover: hover.as_ref(),
        picker: picker.as_ref(),
        theme: &theme,
        status: None,
        tab_width: TAB_WIDTH,
    };
    let mut scratch = Scratch::default();
    view::draw(&mut renderer, &scene, &mut scratch);

    let pixels = renderer.read_rgba()?;
    std::fs::write(out, &pixels).with_context(|| format!("cannot write {}", out.display()))?;
    println!("{W} {H} {}", pixels.len());
    Ok(())
}

/// Puts stdout and stderr back where a terminal can see them.
///
/// This is a windowed application on Windows, so it is handed no console of
/// its own — which is the point: a console program that opens a window
/// leaves a black box behind the editor. But run from a terminal it should
/// still log, and `--snapshot` should still print, so it attaches to the
/// console that launched it when there is one. There is nothing to attach
/// to when launched from Explorer, and nothing is printed; the shell does
/// not wait for a windowed process either, so the prompt comes back while
/// the editor is still running.
#[cfg(target_os = "windows")]
fn attach_to_console() {
    // One function, declared rather than depended on: kernel32 is already
    // linked into every Windows binary the standard library builds.
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: a plain call with no pointers in it. A failure means there
    // was no console to attach to, which is the Explorer case.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(target_os = "windows"))]
fn attach_to_console() {}

fn main() -> Result<()> {
    attach_to_console();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // Before anything else, and before any thread exists: macOS gives a
    // bundled app launchd's PATH, which holds four system directories
    // and none of the places language servers are installed into.
    lithe_lsp::widen_search_path();

    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // Hidden development flag; not part of the editor's interface.
    if let Some(i) = args.iter().position(|a| a == "--snapshot") {
        args.remove(i);
        let out = PathBuf::from(
            args.get(i)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("--snapshot needs an output path"))?,
        );
        args.remove(i);
        let mut overlay = None;
        if let Some(i) = args.iter().position(|a| a == "--overlay") {
            overlay = args.get(i + 1).and_then(|v| v.clone().into_string().ok());
            let end = (i + 1).min(args.len().saturating_sub(1));
            args.drain(i..=end);
        }
        return snapshot(args.first().map(PathBuf::from), &out, overlay.as_deref());
    }

    let mut builder = EventLoop::<Wake>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // winit installs a default macOS menu of its own; ours replaces it,
        // and having both fight over NSApp.mainMenu is how you end up with
        // a half-populated menu bar.
        builder.with_default_menu(false);
        // Regular, so the app owns the menu bar and appears in the Dock
        // rather than running as a background accessory.
        builder.with_activation_policy(ActivationPolicy::Regular);
    }
    let event_loop = builder.build().context("could not create the event loop")?;
    let mut app = App::new(args.first().map(PathBuf::from), event_loop.create_proxy());

    // After the event loop exists, because installing a macOS menu needs an
    // NSApplication and winit is what creates it.
    match menu::build() {
        Ok(built) => {
            let proxy = event_loop.create_proxy();
            app.menu_events = Some(menu::route(move || {
                let _ = proxy.send_event(Wake);
            }));
            app._menu = Some(built);
            app.menu_active = menu::owns_shortcuts();
        }
        // A missing menu bar is a degraded experience, not a dead editor:
        // the keyboard handler picks the shortcuts back up.
        Err(e) => log::warn!("could not install the menu bar: {e}"),
    }

    // Likewise after the event loop exists: a bundled app is handed its
    // documents through the application delegate, and winit leaves that
    // slot free for exactly this.
    openfiles::install(event_loop.create_proxy());

    event_loop.run_app(&mut app).context("event loop failed")?;
    // Servers outlive the window otherwise, and rust-analyzer is not a
    // thing to leave running by accident.
    app.lsp.shutdown();
    Ok(())
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::testutil::TempDir;

    /// The project root ends up in the window title, on the landing page
    /// and in the recent-projects file, so it has to be the path a person
    /// would recognise — not Windows' extended-length spelling of it.
    #[test]
    fn a_real_path_is_absolute_and_plainly_spelled() {
        let dir = TempDir::new();
        let nested = dir.path().join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let real = real_path(&nested.join("..").join("src"));
        assert!(real.is_absolute(), "{real:?}");
        assert!(real.ends_with("src"), "the `..` should be gone: {real:?}");
        assert!(
            !real.to_string_lossy().starts_with(r"\\?\"),
            "no extended-length prefix: {real:?}"
        );
    }

    #[test]
    fn a_free_name_steps_past_what_is_there() {
        let dir = TempDir::new();
        let taken = dir.path().join("a.txt");
        assert_eq!(free_name(&taken), taken, "nothing there, nothing to dodge");

        std::fs::write(&taken, "").unwrap();
        let next = free_name(&taken);
        assert_eq!(next.file_name().unwrap(), "a 2.txt");

        std::fs::write(&next, "").unwrap();
        assert_eq!(free_name(&taken).file_name().unwrap(), "a 3.txt");
    }

    #[test]
    fn copying_a_tree_brings_its_contents() {
        let dir = TempDir::new();
        let from = dir.path().join("src");
        std::fs::create_dir_all(from.join("deep")).unwrap();
        std::fs::write(from.join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(from.join("deep/b.rs"), "fn b() {}").unwrap();

        let to = dir.path().join("copy");
        copy_tree(&from, &to).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("a.rs")).unwrap(),
            "fn a() {}"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("deep/b.rs")).unwrap(),
            "fn b() {}"
        );
    }

    /// The point of the symmetric `FileOp`: undo gives back the op that
    /// redoes it, so a move can be walked back and forth all day.
    #[test]
    fn undoing_a_move_and_redoing_it_lands_where_it_started() {
        let dir = TempDir::new();
        let from = dir.path().join("a.txt");
        let to = dir.path().join("b.txt");
        std::fs::write(&from, "hello").unwrap();
        std::fs::rename(&from, &to).unwrap();

        let op = FileOp::Moved {
            from: from.clone(),
            to: to.clone(),
        };
        let redo = op.undo().unwrap();
        assert!(from.exists() && !to.exists(), "undo did not move it back");

        let undo = redo.undo().unwrap();
        assert!(to.exists() && !from.exists(), "redo did not move it again");

        undo.undo().unwrap();
        assert!(from.exists(), "and back once more");
    }

    #[test]
    fn trimming_takes_the_shared_indentation_and_no_more() {
        let text = "    if x {\n        go();\n    }\n";
        assert_eq!(trim_indent(text), "if x {\n    go();\n}\n");
    }

    #[test]
    fn trimming_ignores_blank_lines_when_measuring() {
        // A blank line has no indentation, and letting it set the shared
        // depth would mean a block with one empty line never trims.
        let text = "    a\n\n    b";
        assert_eq!(trim_indent(text), "a\n\nb");
    }

    #[test]
    fn a_menu_label_too_long_for_the_menu_is_cut() {
        let long = "x".repeat(200);
        let short = trim_to(&long, 20);
        assert_eq!(short.chars().count(), 20);
        assert!(short.ends_with('…'));
        assert_eq!(trim_to("short", 20), "short", "a short label is left alone");
    }
}

#[cfg(test)]
mod lsp_reply_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn locations_reads_all_three_shapes_servers_send() {
        // A bare Location.
        let one = json!({ "uri": "file:///p/a.rs", "range": { "start": { "line": 4 } } });
        assert_eq!(locations(&one).len(), 1);
        assert_eq!(locations(&one)[0].1, 4);

        // A list of them.
        let many = json!([one, { "uri": "file:///p/b.rs", "range": { "start": { "line": 9 } } }]);
        let places = locations(&many);
        assert_eq!(places.len(), 2);
        assert_eq!(places[1].0, PathBuf::from("/p/b.rs"));

        // A LocationLink, which names its fields differently.
        let link = json!([{
            "targetUri": "file:///p/c.rs",
            "targetSelectionRange": { "start": { "line": 2 } },
        }]);
        assert_eq!(locations(&link)[0].1, 2);
    }

    #[test]
    fn call_hierarchy_replies_are_read_from_either_side() {
        let incoming = json!([{
            "from": { "uri": "file:///p/a.rs", "selectionRange": { "start": { "line": 7 } } },
            "fromRanges": [],
        }]);
        let places = call_locations(&incoming, true);
        assert_eq!(places, vec![(PathBuf::from("/p/a.rs"), 7)]);
        assert!(
            call_locations(&incoming, false).is_empty(),
            "an incoming reply has no `to`"
        );
    }

    #[test]
    fn a_workspace_edit_is_read_in_both_of_its_shapes() {
        let range = json!({
            "start": { "line": 1, "character": 2 },
            "end": { "line": 1, "character": 5 },
        });
        let old = json!({
            "changes": { "file:///p/a.rs": [{ "range": range, "newText": "new" }] },
        });
        let new = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///p/a.rs", "version": 1 },
                "edits": [{ "range": range, "newText": "new" }],
            }],
        });
        for edit in [&old, &new] {
            let files = workspace_edits(edit);
            assert_eq!(files.len(), 1, "got {files:?}", files = files.len());
            assert_eq!(files[0].0, PathBuf::from("/p/a.rs"));
            assert_eq!(files[0].1[0].1, "new");
            assert_eq!(files[0].1[0].0.start.character, 2);
        }
    }

    #[test]
    fn a_rename_operation_without_edits_is_skipped() {
        // documentChanges may also hold create/rename/delete operations;
        // they have no `edits` and must not come back as empty files.
        let edit = json!({
            "documentChanges": [{ "kind": "rename", "oldUri": "file:///a", "newUri": "file:///b" }],
        });
        assert!(workspace_edits(&edit).is_empty());
    }

    #[test]
    fn edits_apply_to_a_file_that_is_not_open() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "let old = 1;\nlet old = 2;\n").unwrap();

        let at = |line: u32, from: u32, to: u32| lithe_lsp::Range {
            start: lithe_lsp::Position {
                line,
                character: from,
            },
            end: lithe_lsp::Position {
                line,
                character: to,
            },
        };
        // Two edits on different lines, given in order; the later one
        // must not be shifted by the earlier one.
        let count = edit_on_disk(
            &path,
            vec![(at(0, 4, 7), "new".into()), (at(1, 4, 7), "new".into())],
        );
        assert_eq!(count, 2);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "let new = 1;\nlet new = 2;\n"
        );
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use lithe_core::TextBuffer;

    fn at(text: &str, head: usize) -> editor::Editor {
        let mut editor = editor::Editor::new(TextBuffer::from_text(text), None, 4);
        editor.sel.collapse_to(lithe_core::Cursor::at(head));
        editor
    }

    #[test]
    fn the_prefix_is_the_word_being_typed() {
        assert_eq!(typed_prefix(&at("let foo", 7)), "foo");
        assert_eq!(typed_prefix(&at("let foo_bar2", 12)), "foo_bar2");
        // Mid-word: only what is behind the caret is typed yet.
        assert_eq!(typed_prefix(&at("let foobar", 7)), "foo");
    }

    #[test]
    fn a_dot_leaves_no_prefix_to_filter_by() {
        // The commonest trigger. Everything the server offers for
        // `thing.` is a member, and none of it has been typed.
        assert_eq!(typed_prefix(&at("thing.", 6)), "");
        assert_eq!(typed_prefix(&at("a::", 3)), "");
        assert_eq!(typed_prefix(&at("", 0)), "");
    }

    #[test]
    fn the_prefix_stops_at_the_start_of_the_line() {
        let editor = at("one\ntwo", 7);
        assert_eq!(typed_prefix(&editor), "two");
    }

    #[test]
    fn accepting_replaces_the_word_rather_than_following_it() {
        // The prefix is both what the list is filtered by and what the
        // insertion replaces, which is what stops `fo` + `foo` coming out
        // as `fofoo`.
        let mut completion = find::Completion::new(vec![
            find::Entry {
                label: "foo".into(),
                insert: "foo".into(),
                detail: None,
            },
            find::Entry {
                label: "bar".into(),
                insert: "bar".into(),
                detail: None,
            },
        ]);
        completion.narrow(typed_prefix(&at("let fo", 6)));
        assert_eq!(completion.prefix, "fo");
        assert_eq!(completion.chosen().map(|e| e.label.as_str()), Some("foo"));

        let mut editor = at("let fo", 6);
        let head = editor.sel.primary().head;
        editor.replace_span(head - completion.prefix.len(), head, "foo");
        assert_eq!(editor.doc.rope().to_string(), "let foo");
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;
    use lithe_vcs::{DiffLine, LineKind};

    fn view(lines: Vec<DiffLine>) -> workspace::Diff {
        workspace::Diff {
            editor: Some(editor::Editor::new(lithe_core::TextBuffer::new(), None, 4)),
            old: lithe_core::TextBuffer::new(),
            old_syntax: None,
            path: PathBuf::from("a.rs"),
            title: "a.rs".into(),
            lines,
            revision: 0,
            scroll: 0.0,
        }
    }

    fn line(kind: LineKind, old: Option<usize>, new: Option<usize>) -> DiffLine {
        DiffLine {
            kind,
            old,
            new,
            text: String::new(),
        }
    }

    #[test]
    fn a_diff_row_names_the_working_copy_line_it_shows() {
        let v = view(vec![
            line(LineKind::Context, Some(1), Some(1)),
            line(LineKind::Removed, Some(2), None),
            line(LineKind::Added, None, Some(2)),
            line(LineKind::Context, Some(3), Some(3)),
        ]);

        // Unified: a row is a diff line, and a removed line is not in the
        // working copy at all.
        assert_eq!(row_line(&v, 0, false), Some(1));
        assert_eq!(row_line(&v, 1, false), None);
        assert_eq!(row_line(&v, 2, false), Some(2));
        assert_eq!(row_line(&v, 9, false), None, "past the end");

        // Split: a row is a pair, and only its right side is the working
        // copy — so the row that shows the removal on the left shows the
        // addition on the right.
        assert_eq!(row_line(&v, 0, true), Some(1));
        assert_eq!(row_line(&v, 1, true), Some(2));
    }

    #[test]
    fn history_is_read_only_and_changes_are_not() {
        let changes = view(vec![line(LineKind::Context, Some(1), Some(1))]);
        assert!(changes.editable(), "a working file can be typed into");

        let history = workspace::Diff {
            editor: None,
            path: PathBuf::new(),
            revision: u64::MAX,
            ..view(Vec::new())
        };
        assert!(
            !history.editable(),
            "a commit has no buffer behind it to edit"
        );
    }

    /// A diff takes over its file's tab rather than adding one, and
    /// hiding it hands the buffer back.
    #[test]
    fn a_diff_takes_over_the_tab_its_file_is_in() {
        let path = PathBuf::from("a.rs");
        let mut ws = workspace::Workspace::empty(None);
        ws.push(editor::Editor::new(
            lithe_core::TextBuffer::from_text("one\n"),
            Some(path.clone()),
            4,
        ));
        ws.push(editor::Editor::new(lithe_core::TextBuffer::new(), None, 4));
        ws.active_mut().unwrap().insert("typed");

        assert!(ws.show_diff(&path, view(Vec::new())));
        assert_eq!(ws.len(), 2, "the diff is the tab, not another one");
        assert_eq!(ws.active, 0, "and focus follows it");
        assert_eq!(
            ws.active().map(|e| e.doc.rope().to_string()),
            Some("one\n".to_string()),
            "the file's own buffer moved across, not a second copy"
        );

        // The other tab's unsaved work is untouched by any of this.
        ws.hide_diff(0);
        assert!(ws.diff().is_none());
        assert_eq!(ws.len(), 2);
        ws.active = 1;
        assert!(ws.active().unwrap().dirty);
    }

    #[test]
    fn a_commit_diff_is_one_tab_however_many_commits_are_clicked() {
        let mut ws = workspace::Workspace::empty(None);
        let commit = || workspace::Diff {
            editor: None,
            old: lithe_core::TextBuffer::new(),
            old_syntax: None,
            path: PathBuf::new(),
            title: "abc123  a commit".into(),
            lines: Vec::new(),
            revision: u64::MAX,
            scroll: 0.0,
        };
        ws.show_commit(commit());
        ws.show_commit(commit());
        ws.show_commit(commit());
        assert_eq!(ws.len(), 1, "clicking through history is browsing");
    }

    #[test]
    fn settings_is_a_tab_and_only_ever_one() {
        let mut ws = workspace::Workspace::empty(None);
        ws.push(editor::Editor::new(lithe_core::TextBuffer::new(), None, 4));
        ws.show_settings();
        assert!(ws.showing_settings());
        assert_eq!(ws.len(), 2);
        assert!(
            ws.active().is_none(),
            "the settings page is not editing anything"
        );

        ws.active = 0;
        assert!(!ws.showing_settings());
        ws.show_settings();
        assert_eq!(ws.len(), 2, "asking again focuses the one that is open");
        assert_eq!(ws.active, 1);

        ws.close_active();
        assert_eq!(ws.labels(), vec!["[scratch]"]);
    }
}

#[cfg(test)]
mod hover_tests {
    use super::*;

    #[test]
    fn the_three_shapes_a_server_may_answer_in() {
        let marked = serde_json::json!({ "contents": "plain string" });
        assert_eq!(hover_text(&marked).as_deref(), Some("plain string"));

        let content = serde_json::json!({
            "contents": { "kind": "markdown", "value": "### Title\n\nbody" }
        });
        assert_eq!(hover_text(&content).as_deref(), Some("### Title\n\nbody"));

        let list = serde_json::json!({
            "contents": [{ "value": "one" }, "two"]
        });
        assert_eq!(hover_text(&list).as_deref(), Some("one\n\ntwo"));
    }

    #[test]
    fn a_signature_keeps_the_shape_the_popup_draws_it_in() {
        // Flattening this to one line, as the status bar needed, left
        // the reader nothing to colour.
        let reply = serde_json::json!({
            "contents": {
                "kind": "markdown",
                "value": "```c\nint the_answer(void)\n```\n\nReturns 42.",
            }
        });
        let text = hover_text(&reply).expect("some text");
        let lines = markdown::render_boxed(&text, view::Hover::COLUMNS);
        assert!(
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| matches!(s.style, markdown::Style::Syntax(_))),
            "the fence should be highlighted: {lines:#?}"
        );
        assert!(lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.text.contains("Returns 42"))));
    }

    #[test]
    fn nothing_to_say_is_told_apart_from_an_empty_string() {
        assert_eq!(hover_text(&serde_json::json!({})), None);
        assert_eq!(
            hover_text(&serde_json::json!({ "contents": { "value": "  \n " } })),
            None
        );
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn a_url_is_told_apart_from_a_path_beside_the_file() {
        for url in [
            "https://example.com",
            "http://example.com/a?b=1&c=2",
            "mailto:someone@example.com",
            "file:///tmp/x",
        ] {
            assert!(is_url(url), "{url} is a url");
        }
        // A drive letter has a colon in it, and a hostname written
        // without a scheme is a file name as far as this is concerned.
        for path in [
            "docs/guide.md",
            "../README.md",
            "C:\\Users\\x\\a.md",
            "www.example.com/x",
            "guide.md#writing",
        ] {
            assert!(!is_url(path), "{path} is a path");
        }
    }

    #[test]
    fn a_target_is_unescaped_as_a_path() {
        assert_eq!(unescape("docs/a%20file.md"), "docs/a file.md");
        assert_eq!(unescape("plain.md"), "plain.md");
        // A character written as the bytes it is made of.
        assert_eq!(unescape("caf%C3%A9.md"), "café.md");
        // And not every percent is an escape: a file really can be
        // called this.
        assert_eq!(unescape("100%.md"), "100%.md");
        assert_eq!(unescape("a%zz.md"), "a%zz.md");
    }
}

#[cfg(test)]
mod dwell_tests {
    use super::{dwell_due, HOVER_DWELL};
    use std::time::Duration;

    #[test]
    fn a_rest_is_considered_once_and_not_again() {
        let rested = HOVER_DWELL + Duration::from_millis(1);
        assert!(
            dwell_due(true, false, false, rested),
            "a pointer resting over text should be asked about"
        );
        // The whole point. `dwell_hover` marks the rest considered even
        // when it decides there is nothing to ask — a file with no
        // language server, say. Without that the deadline it schedules
        // is already in the past, so the loop wakes, declines and
        // re-arms with no pause at all: 20% of a core spent doing
        // nothing, which is invariant 1 broken outright.
        assert!(
            !dwell_due(true, true, false, rested),
            "a rest already considered must not re-arm"
        );
    }

    #[test]
    fn nothing_is_asked_before_the_pointer_has_settled() {
        assert!(!dwell_due(true, false, false, Duration::ZERO));
        assert!(!dwell_due(
            true,
            false,
            false,
            HOVER_DWELL - Duration::from_millis(1)
        ));
    }

    #[test]
    fn nothing_is_asked_over_the_chrome_or_behind_a_popup() {
        let rested = HOVER_DWELL * 2;
        assert!(!dwell_due(false, false, false, rested), "not over text");
        assert!(
            !dwell_due(true, false, true, rested),
            "a popup is already saying something"
        );
    }
}

#[cfg(test)]
mod hover_highlight_tests {
    use crate::view::{hoverable, Hit};

    /// Only discrete elements light up. The text, the scrollbar and a
    /// diff row carry a position, so every pixel of movement over them
    /// would count as hovering something new and ask for a frame that
    /// would draw exactly the same thing.
    #[test]
    fn nothing_that_moves_with_the_pointer_is_hoverable() {
        assert_eq!(hoverable(Hit::Text), None);
        assert_eq!(hoverable(Hit::Scrollbar(12.0)), None);
        assert_eq!(hoverable(Hit::Scrollbar(13.0)), None);
        assert_eq!(hoverable(Hit::DiffText { row: 1, column: 2 }), None);
        assert_eq!(hoverable(Hit::SidebarDivider), None);
        assert_eq!(hoverable(Hit::Elsewhere), None);
    }

    #[test]
    fn the_things_you_click_are_hoverable() {
        for hit in [
            Hit::Tab(0),
            Hit::TabClose(0),
            Hit::Tree(3),
            Hit::GitRow(1),
            Hit::GitStage(1),
            Hit::GitInit,
            Hit::Status(crate::view::StatusButton::Git),
            Hit::Landing(crate::view::LandingAction::NewFile),
        ] {
            assert_eq!(hoverable(hit), Some(hit), "{hit:?} should highlight");
        }
    }
}
