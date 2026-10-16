//! Layout and drawing.
//!
//! Invariant 2 lives here: every loop below is bounded by the window, never
//! by the document, the project or the match set. The file tree draws the
//! rows that fit; the picker draws the results that fit; the editor shapes
//! the lines that fit.
//!
//! Tabs make visual columns and character offsets different things, so the
//! two mappings between them live here and every caller goes through them.

use std::fmt::Write as _;
use std::path::Path;

use lithe_core::{RopeSlice, TextBuffer};
use lithe_gpu::{CellMetrics, Renderer, Rgba};
use lithe_syntax::{Kind, Span};

use crate::context::ContextMenu;
use crate::editor::Editor;
use crate::find::{Completion, Field, Find, ProjectSearch, Prompt};
use crate::gitpanel::{GitPanel, Mode as GitMode, Row as GitRowKind};
use crate::icons::{self, Icon};
use crate::markdown::{
    Align as MdAlign, Block as MdBlock, Line as MdLine, Scale as MdScale, Style as MdStyle,
};
use crate::picker::Picker;
use crate::settings::{PreviewKind, Settings};
use crate::theme::Theme;
use crate::tree::Tree;
use crate::workspace::Workspace;

/// Default sidebar width, in cells.
pub const TREE_COLS: f32 = 28.0;
/// How far the sidebar can be dragged, in cells.
pub const SIDEBAR_MIN: f32 = 14.0;
pub const SIDEBAR_MAX: f32 = 70.0;
/// Grab area for the divider, in physical pixels either side.
const DIVIDER_GRAB: f32 = 4.0;
/// How much of a history row, after the hash, the author's name may take,
/// and the most columns it gets however wide the sidebar is. A name is
/// read at a glance; a summary is read.
const AUTHOR_SHARE: f32 = 0.45;
const AUTHOR_COLS: f32 = 14.0;
/// Most results the picker will draw, regardless of window height.
const PICKER_ROWS: usize = 14;
/// Cells reserved at the right of a tab for its close box.
const CLOSE_COLS: f32 = 3.0;
/// A switch on the settings page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    Autosave,
    Preview(PreviewKind),
    SideBySideDiff,
}

/// One line of the settings page. Blanks and headings are rows too, so
/// position is an index times the line height — the same arrangement the
/// landing page and the source control panel use.
enum SettingRow {
    Blank,
    Title,
    Heading(&'static str),
    Switch(Setting),
}

fn setting_rows() -> Vec<SettingRow> {
    let mut rows = vec![
        SettingRow::Title,
        SettingRow::Blank,
        SettingRow::Heading("Editor"),
        SettingRow::Switch(Setting::Autosave),
        SettingRow::Blank,
        SettingRow::Heading("Preview"),
    ];
    rows.extend(
        PreviewKind::ALL
            .into_iter()
            .map(|kind| SettingRow::Switch(Setting::Preview(kind))),
    );
    rows.push(SettingRow::Blank);
    rows.push(SettingRow::Heading("Diff"));
    rows.push(SettingRow::Switch(Setting::SideBySideDiff));
    rows
}

impl Setting {
    pub fn label(self) -> &'static str {
        match self {
            Setting::Autosave => "Save automatically after a pause",
            Setting::Preview(kind) => kind.label(),
            Setting::SideBySideDiff => "Show diffs side by side",
        }
    }

    pub fn value(self, settings: &Settings) -> bool {
        match self {
            Setting::Autosave => settings.autosave,
            Setting::Preview(kind) => settings.preview(kind),
            Setting::SideBySideDiff => settings.side_by_side_diff,
        }
    }

    pub fn toggle(self, settings: &mut Settings) {
        let now = !self.value(settings);
        match self {
            Setting::Autosave => settings.autosave = now,
            Setting::Preview(kind) => settings.set_preview(kind, now),
            Setting::SideBySideDiff => settings.side_by_side_diff = now,
        }
    }
}

/// Where the settings block sits, and how wide its rows are.
fn settings_origin(l: &Layout, rows: usize) -> (f32, f32, f32) {
    let width = (46.0 * l.cell.advance).min(l.width - l.text_x - 4.0 * l.cell.advance);
    let x = (l.text_x + (l.width - l.text_x - width) / 2.0)
        .max(l.text_x)
        .round();
    let height = rows as f32 * l.cell.line_height;
    let y = (l.text_y + (l.text_h - height) / 2.0)
        .max(l.text_y + l.cell.line_height)
        .round();
    (x, y, width)
}

/// A clickable segment of the status bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusButton {
    Explorer,
    Git,
    Search,
    Preview,
}

/// One button: what it does, what it says, and whether it is on.
pub struct ButtonSlot {
    pub button: StatusButton,
    pub icon: Icon,
    pub active: bool,
}

/// Rasterised diagrams, keyed by the markup they were drawn from and
/// the width they were drawn at.
pub type Figures = std::collections::HashMap<(u64, u32), crate::imageview::ImageView>;

/// For a scene with no window behind it, which cannot hold an uploaded
/// picture and so can never have one to look up.
#[cfg(test)]
static EMPTY_FIGURES: std::sync::LazyLock<Figures> = std::sync::LazyLock::new(Figures::new);

/// Which kind of diff a pane is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    /// Of a working file: the new side is the live buffer, so a click
    /// there places the caret in it.
    Editable,
    /// Of a commit: there is nothing behind it to type into.
    ReadOnly,
}

/// Which surfaces are open. Shared by drawing and hit-testing so the two
/// cannot disagree about what is on screen — and small enough that the
/// click path, which has no theme, can build one.
#[derive(Clone, Copy)]
pub struct Panels<'a> {
    pub workspace: &'a Workspace,
    pub tree: Option<&'a Tree>,
    pub git: Option<&'a GitPanel>,
    pub recents: &'a [&'a Path],
    pub search_open: bool,
    pub preview_open: bool,
    pub settings_open: bool,
    pub context: Option<&'a ContextMenu>,
    /// The diff filling the text area, if one is.
    ///
    /// One field rather than "is there one" and "is it editable",
    /// because the second says nothing unless the first is true and two
    /// bools that have to agree are a bug waiting to be written. One
    /// already was: a test set only the second and went on passing
    /// while the hit test read only the first.
    pub diff: Option<DiffKind>,
    /// How far that diff is scrolled, so a click can name the row it
    /// landed on rather than the row it would have landed on unscrolled.
    pub diff_scroll: f32,
    pub diff_side_by_side: bool,
    /// The rendered preview, for finding the fenced block under a click.
    pub preview: Option<&'a [MdLine]>,
    pub preview_scroll: f32,
}

/// The buttons, in order. Built from what is open, so "on" always matches
/// what is actually showing.
pub fn status_buttons(panels: &Panels<'_>) -> Vec<ButtonSlot> {
    let mut slots = vec![
        ButtonSlot {
            button: StatusButton::Explorer,
            icon: Icon::Files,
            active: panels.tree.is_some(),
        },
        ButtonSlot {
            button: StatusButton::Git,
            icon: Icon::Git,
            active: panels.git.is_some(),
        },
        ButtonSlot {
            button: StatusButton::Search,
            icon: Icon::Search,
            active: panels.search_open,
        },
    ];
    // Offered for every kind that has a preview, not just Markdown.
    // Without this an HTML or SVG file had no visible way back to its
    // source, which is half of having two modes.
    // And only on a file tab: a diff and the settings page are what the
    // tab shows, so there is nothing for the eye to switch.
    let previewable = matches!(
        panels.workspace.active_tab(),
        Some(crate::workspace::Tab::File(_))
    ) && panels
        .workspace
        .active()
        .and_then(|e| e.path.as_deref())
        .and_then(PreviewKind::of)
        .is_some();
    if previewable || panels.preview_open {
        slots.push(ButtonSlot {
            button: StatusButton::Preview,
            icon: Icon::Eye,
            active: panels.preview_open,
        });
    }
    slots
}

/// Where each button sits along the status bar.
pub fn status_button_extents(l: &Layout, slots: &[ButtonSlot]) -> Vec<(f32, f32)> {
    // Square-ish targets: an icon with a comfortable margin either side.
    let w = (icon_size(l) + 2.0 * l.cell.advance).round();
    (0..slots.len())
        .map(|i| (i as f32 * w, (i + 1) as f32 * w))
        .collect()
}

/// Icon size for the status bar, a little under the line height.
fn icon_size(l: &Layout) -> f32 {
    (l.cell.line_height * 0.52).round()
}

/// Cells reserved at the end of a source control row for its stage box.
const STAGE_COLS: f32 = 3.0;

/// Scrollbar width in physical pixels.
pub const SCROLLBAR_W: f32 = 10.0;

/// Width of the landing page's content column, in cells.
const LANDING_COLS: f32 = 46.0;

/// A diagnostic, reduced to what drawing needs.
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Hint,
    Information,
    Warning,
    Error,
}

/// What a landing-page row does when clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandingAction {
    NewFile,
    OpenFile,
    OpenFolder,
    NewWindow,
    OpenRecent(usize),
}

/// One line of the landing page. Gaps are rows too, so the whole page is a
/// simple index-times-line-height walk and drawing cannot drift from
/// hit-testing.
enum Row<'a> {
    Blank,
    Title,
    Subtitle,
    Heading(&'a str),
    Action { label: &'a str, hint: &'a str },
    Recent(usize),
    Footer(&'a str),
}

struct LandingRow<'a> {
    row: Row<'a>,
    action: Option<LandingAction>,
}

/// True when there is no project and nothing open — just the app.
pub fn showing_landing(workspace: &Workspace) -> bool {
    workspace.root.is_none() && workspace.is_empty()
}

/// The landing page, as a flat list of lines.
fn landing_rows<'a>(recents: &[&'a Path]) -> Vec<LandingRow<'a>> {
    let mut rows = vec![
        LandingRow {
            row: Row::Title,
            action: None,
        },
        LandingRow {
            row: Row::Subtitle,
            action: None,
        },
        LandingRow {
            row: Row::Blank,
            action: None,
        },
        LandingRow {
            row: Row::Heading("Start"),
            action: None,
        },
        LandingRow {
            row: Row::Action {
                label: "New buffer",
                hint: key!(" N"),
            },
            action: Some(LandingAction::NewFile),
        },
        LandingRow {
            row: Row::Action {
                label: "Open file…",
                hint: key!(" O"),
            },
            action: Some(LandingAction::OpenFile),
        },
        LandingRow {
            row: Row::Action {
                label: "Open folder…",
                hint: key!(" shift O"),
            },
            action: Some(LandingAction::OpenFolder),
        },
        LandingRow {
            row: Row::Action {
                label: "New window",
                hint: key!(" shift N"),
            },
            action: Some(LandingAction::NewWindow),
        },
    ];

    if !recents.is_empty() {
        rows.push(LandingRow {
            row: Row::Blank,
            action: None,
        });
        rows.push(LandingRow {
            row: Row::Heading("Recent"),
            action: None,
        });
        for index in 0..recents.len() {
            rows.push(LandingRow {
                row: Row::Recent(index),
                action: Some(LandingAction::OpenRecent(index)),
            });
        }
    }

    rows.push(LandingRow {
        row: Row::Blank,
        action: None,
    });
    rows.push(LandingRow {
        row: Row::Footer(key!(" shift P  for every command")),
        action: None,
    });
    rows
}

/// Where the landing block sits: a centred column, nudged above centre so
/// it does not look like it is sliding off the bottom.
fn landing_origin(l: &Layout, rows: usize) -> (f32, f32) {
    let width = LANDING_COLS * l.cell.advance;
    let x = ((l.width - width) / 2.0).max(l.cell.advance).round();
    let height = rows as f32 * l.cell.line_height;
    let y = ((l.height - height) / 2.0).max(l.cell.line_height).round();
    (x, y)
}

/// Everything drawing needs, gathered by the caller so this module never
/// reaches into application state.
pub struct Scene<'a> {
    pub workspace: &'a Workspace,
    pub find: Option<&'a Find>,
    pub prompt: Option<&'a Prompt>,
    /// False while the caret is in the off half of its blink.
    pub caret_visible: bool,
    pub search: Option<&'a ProjectSearch>,
    /// In-buffer matches within the visible range.
    pub find_matches: &'a [lithe_search::Span],
    /// Diagnostics for the active file, as char ranges.
    pub diagnostics: &'a [Mark],
    pub completion: Option<&'a Completion>,
    /// Differences from the committed version of the active file.
    pub hunks: &'a [lithe_vcs::Hunk],
    /// Current branch, for the status bar.
    pub branch: Option<&'a str>,
    /// Who last changed the cursor's line, when that is switched on.
    pub blame: Option<&'a str>,
    /// Language-server state, for the status bar.
    pub lsp: Option<&'a str>,
    /// What the language server is busy doing, while it is.
    pub lsp_busy: Option<&'a str>,
    /// Whether autosave is on, for the status bar.
    pub autosave: bool,
    /// What the pointer is over, so it can be highlighted.
    pub hovered: Option<Hit>,
    /// Projects offered on the landing page, newest first.
    pub recents: &'a [&'a Path],
    /// Highlight spans for the visible byte range, in paint order.
    pub highlights: &'a [Span],
    /// The committed side of a diff, and highlight spans for both
    /// sides of it.
    pub diff_old: Option<&'a lithe_core::TextBuffer>,
    pub diff_old_spans: &'a [Span],
    /// The identifier the command key has made clickable, underlined
    /// so that it looks like what it now is.
    pub cmd_link: Option<&'a std::ops::Range<usize>>,
    pub diff_new_spans: &'a [Span],
    pub tree: Option<&'a Tree>,
    pub git: Option<&'a GitPanel>,
    /// Sidebar width in cells, as the person has dragged it.
    pub sidebar_cols: f32,
    /// Rendered Markdown, shown instead of the file when open.
    pub preview: Option<&'a [MdLine]>,
    pub preview_scroll: f32,
    /// Rasterised diagrams, by markup and width. Looked up rather than
    /// carried on the line, because a picture belongs to the window it
    /// was uploaded to and a laid-out document does not.
    pub figures: &'a Figures,
    /// Selected text in the preview: where the drag began and where it
    /// is now, in either order.
    pub preview_sel: Option<(PreviewPos, PreviewPos)>,
    /// A unified diff, shown instead of the file.
    pub diff: Option<&'a [lithe_vcs::DiffLine]>,
    pub diff_title: Option<&'a str>,
    pub diff_scroll: f32,
    /// Whether this diff is of the working copy, which can be typed into.
    pub diff_editable: bool,
    /// What the pointer is resting on, if the server has said.
    pub hover: Option<&'a Hover>,
    /// Show the two versions in parallel columns.
    pub diff_side_by_side: bool,
    /// A decoded image, shown instead of the file.
    pub image: Option<&'a crate::imageview::ImageView>,
    /// The settings page, shown instead of everything else.
    pub settings: Option<&'a Settings>,
    /// An open right-click menu, drawn over everything.
    pub context: Option<&'a ContextMenu>,
    pub picker: Option<&'a Picker>,
    pub theme: &'a Theme,
    pub status: Option<&'a str>,
    pub tab_width: usize,
}

/// Per-frame scratch, reused so drawing allocates nothing in steady state.
#[derive(Default)]
pub struct Scratch {
    text: String,
    /// Highlight kind per character of the line being drawn.
    styles: Vec<Option<Kind>>,
}

/// Where things sit this frame. Shared by drawing and hit-testing so the two
/// can never disagree about the gutter width.
pub struct Layout {
    pub cell: CellMetrics,
    pub width: f32,
    pub height: f32,
    pub tree_w: f32,
    /// Height of the tab strip; zero when nothing is open.
    pub tab_h: f32,
    /// Height of the find bar; zero when it is closed.
    pub find_h: f32,
    /// Top of the text area.
    pub text_y: f32,
    /// Height of the text area, between the tabs and the status bar.
    pub text_h: f32,
    pub gutter_w: f32,
    pub text_x: f32,
    pub rows: usize,
}

impl Layout {
    /// How many whole columns of text fit, with the scrollbar's own
    /// column kept out of it. What the editor scrolls sideways within.
    pub fn cols(&self) -> usize {
        let room = self.width - SCROLLBAR_W - self.text_x;
        ((room / self.cell.advance).floor() as usize).max(1)
    }
}

/// Which pieces of chrome are showing, for layout.
#[derive(Clone, Copy, Default)]
pub struct Chrome {
    /// Either panel; they share the sidebar.
    pub sidebar: bool,
    /// Sidebar width in cells, as dragged.
    pub sidebar_cols: f32,
    pub find: bool,
}

pub fn layout(renderer: &Renderer, workspace: &Workspace, chrome: Chrome) -> Layout {
    let cell = renderer.cell();
    let (width, height) = renderer.size_px();

    let tree_w = if chrome.sidebar {
        chrome.sidebar_cols.clamp(SIDEBAR_MIN, SIDEBAR_MAX) * cell.advance
    } else {
        0.0
    };
    let tab_h = if workspace.is_empty() {
        0.0
    } else {
        cell.line_height
    };
    // The find bar — or the prompt, which shares its row — sits between
    // the tabs and the text.
    let find_h = if chrome.find { cell.line_height } else { 0.0 };
    let text_y = tab_h + find_h;
    let text_h = (height - text_y - cell.line_height).max(0.0);

    let lines = workspace.active().map(|e| e.doc.len_lines()).unwrap_or(1);
    let gutter_w = (decimal_width(lines) + 2) as f32 * cell.advance;

    Layout {
        cell,
        width,
        height,
        tree_w,
        tab_h,
        find_h,
        text_y,
        text_h,
        gutter_w,
        text_x: tree_w + gutter_w,
        rows: (text_h / cell.line_height).ceil() as usize,
    }
}

// ---- column mapping -----------------------------------------------------

/// Visual column of a character offset within a line.
fn visual_col(line: RopeSlice<'_>, char_col: usize, tab: usize) -> usize {
    let mut col = 0;
    for (i, ch) in line.chars().enumerate() {
        if i >= char_col || ch == '\n' || ch == '\r' {
            break;
        }
        col += if ch == '\t' { tab - (col % tab) } else { 1 };
    }
    col
}

/// Character offset within a line for a visual column.
///
/// `nearest_edge` picks between the two things a click can mean. Placing a
/// caret wants the nearer boundary, so clicking the right half of a letter
/// puts the cursor after it. Selecting a word wants the character actually
/// under the pointer, because the boundary after the last letter of a word
/// belongs to the space that follows — and double-clicking the end of a
/// word should take the word, not the space.
fn char_col_at_visual(line: RopeSlice<'_>, target: usize, tab: usize, nearest_edge: bool) -> usize {
    let mut col = 0;
    for (i, ch) in line.chars().enumerate() {
        if ch == '\n' || ch == '\r' {
            return i;
        }
        let width = if ch == '\t' { tab - (col % tab) } else { 1 };
        if target < col + width {
            return if nearest_edge && target >= col + width.div_ceil(2) {
                i + 1
            } else {
                i
            };
        }
        col += width;
    }
    line.chars()
        .take_while(|c| *c != '\n' && *c != '\r')
        .count()
}

// ---- hit testing --------------------------------------------------------

/// Maps a point in physical pixels to a position in the document.
#[allow(clippy::too_many_arguments)]
pub fn point_to_char(
    l: &Layout,
    doc: &TextBuffer,
    scroll: f32,
    scroll_x: f32,
    x: f32,
    y: f32,
    tab: usize,
) -> usize {
    point_to_char_with(l, doc, scroll, scroll_x, x, y, tab, true)
}

/// The character under the pointer, rather than the nearest caret
/// boundary. What a double click should ask about.
#[allow(clippy::too_many_arguments)]
pub fn point_to_char_under(
    l: &Layout,
    doc: &TextBuffer,
    scroll: f32,
    scroll_x: f32,
    x: f32,
    y: f32,
    tab: usize,
) -> usize {
    point_to_char_with(l, doc, scroll, scroll_x, x, y, tab, false)
}

#[allow(clippy::too_many_arguments)]
fn point_to_char_with(
    l: &Layout,
    doc: &TextBuffer,
    scroll: f32,
    scroll_x: f32,
    x: f32,
    y: f32,
    tab: usize,
    nearest_edge: bool,
) -> usize {
    // Fractional scroll means the first visible row is usually clipped, so
    // the hit row is found in continuous coordinates and floored once.
    let row = ((y - l.text_y) / l.cell.line_height + scroll.max(0.0)).floor();
    let line = (row.max(0.0) as usize).min(doc.len_lines().saturating_sub(1));
    // Plus whatever is scrolled off to the left: the column under the
    // pointer is not the column it would be at rest.
    let target_col =
        (((x - l.text_x) / l.cell.advance).round() + scroll_x.max(0.0).round()).max(0.0) as usize;
    let char_col = match doc.line(line) {
        Some(slice) => char_col_at_visual(slice, target_col, tab, nearest_edge),
        None => 0,
    };
    (doc.line_to_char(line) + char_col).min(doc.len_chars())
}

/// Which region of the window a point falls in.
// No `Eq`: one variant carries a y coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// An entry in the open right-click menu.
    Context(usize),
    /// A switch on the settings page.
    Setting(Setting),
    /// A toggle in the status bar.
    Status(StatusButton),
    /// The scrollbar track, at a y position.
    Scrollbar(f32),
    /// The horizontal scrollbar track, at an x position.
    ScrollbarX(f32),
    /// The draggable edge of the sidebar.
    SidebarDivider,
    /// A fenced block in the preview, by its line index. Clicking one
    /// copies it, which is the only way to get text out of a preview
    /// that is drawn rather than selectable.
    PreviewCode(usize),
    /// A row in an editable diff's new side: which row of the diff, and
    /// how many cells into the text the click landed.
    DiffText {
        row: usize,
        column: usize,
    },
    /// A row of a diff anywhere the caret cannot go: the committed
    /// column, and any row of a commit's read-only diff. It carries no
    /// column because there is nothing to put a caret at — it exists so
    /// a right-click on either side finds the change it is over.
    DiffRow(usize),
    /// A row in the source control panel.
    GitRow(usize),
    /// The stage/unstage box at the end of a source control row.
    GitStage(usize),
    /// A commit in the history.
    GitCommit(usize),
    /// The Changes / History switch.
    GitMode(GitMode),
    /// The button offered when there is no repository here yet.
    GitInit,
    /// A row on the landing page.
    Landing(LandingAction),
    Tree(usize),
    Tab(usize),
    /// The close box on a tab.
    TabClose(usize),
    Text,
    Elsewhere,
}

#[allow(clippy::too_many_arguments)]
/// Where an open context menu sits, clamped inside the window.
pub fn context_bounds(l: &Layout, menu: &ContextMenu) -> (f32, f32, f32, f32) {
    let width = menu.columns() as f32 * l.cell.advance;
    let height = menu.entries.len() as f32 * l.cell.line_height + 4.0;
    // Flip rather than overflow: a menu opened near an edge should still
    // be entirely on screen.
    let x = menu.at.0.min((l.width - width).max(0.0));
    let y = if menu.at.1 + height > l.height {
        (menu.at.1 - height).max(0.0)
    } else {
        menu.at.1
    };
    (x, y, width, height)
}

/// The hit, if it names something worth highlighting under the pointer.
///
/// Discrete things only. The text, the scrollbar and a diff row carry a
/// position, so every pixel of movement over them would read as a
/// different element and ask for a frame that would draw the same
/// thing. Narrowing to what can actually light up is what keeps moving
/// the pointer across a file free.
pub fn hoverable(hit: Hit) -> Option<Hit> {
    matches!(
        hit,
        Hit::Tab(_)
            | Hit::TabClose(_)
            | Hit::Tree(_)
            | Hit::GitRow(_)
            | Hit::GitStage(_)
            | Hit::GitCommit(_)
            | Hit::GitMode(_)
            | Hit::GitInit
            | Hit::Status(_)
            | Hit::Setting(_)
            | Hit::Landing(_)
            | Hit::Context(_)
    )
    .then_some(hit)
}

/// Paints the hover tint over a row or button, if it is the one under
/// the pointer. Over whatever is already there, so a selected row and a
/// bare one both read the same when hovered.
fn tint(
    f: &mut lithe_gpu::Frame<'_>,
    scene: &Scene<'_>,
    what: Hit,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    if scene.hovered == Some(what) {
        f.rect(x, y, w, h, scene.theme.hover);
    }
}

pub fn hit_test(l: &Layout, panels: &Panels<'_>, x: f32, y: f32) -> Hit {
    // An open menu takes every click: one inside it chooses, one outside
    // dismisses, and nothing underneath should act in either case.
    if let Some(menu) = panels.context {
        let (mx, my, width, height) = context_bounds(l, menu);
        if x >= mx && x < mx + width && y >= my && y < my + height {
            let row = ((y - my - 2.0) / l.cell.line_height).floor().max(0.0) as usize;
            return Hit::Context(row);
        }
        return Hit::Elsewhere;
    }
    let workspace = panels.workspace;
    let tree = panels.tree;
    let git = panels.git;
    let recents = panels.recents;
    // The find bar swallows clicks on its own row rather than letting them
    // land in the text underneath.
    if l.find_h > 0.0 && y >= l.tab_h && y < l.tab_h + l.find_h {
        return Hit::Elsewhere;
    }

    // The status bar is checked first, and before the landing page, so its
    // toggles stay clickable when there is no file open — which is exactly
    // when you are most likely to reach for one.
    if y >= l.text_y + l.text_h {
        // The status bar's left end is a row of toggles.
        let slots = status_buttons(panels);
        for (slot, (x0, x1)) in slots.iter().zip(status_button_extents(l, &slots)) {
            if x >= x0 && x < x1 {
                return Hit::Status(slot.button);
            }
        }
        return Hit::Elsewhere;
    }
    // The divider is grabbed slightly either side of the edge, since a
    // one-pixel target is not a target.
    if l.tree_w > 0.0 && (x - l.tree_w).abs() <= DIVIDER_GRAB {
        return Hit::SidebarDivider;
    }
    // The scrollbar sits over the right edge of the text area.
    if x >= l.width - SCROLLBAR_W && y >= l.text_y {
        return Hit::Scrollbar(y);
    }
    // And the horizontal one over the bottom edge of it, when there is
    // one — it is drawn only where a line runs off the right.
    if y >= l.text_y + l.text_h - SCROLLBAR_W
        && y < l.text_y + l.text_h
        && x >= l.text_x
        && h_scrollbar_band(l, panels)
    {
        return Hit::ScrollbarX(x);
    }
    if l.tab_h > 0.0 && y < l.tab_h && x >= l.tree_w {
        let labels = workspace.labels();
        for (index, (x0, x1)) in tab_extents(l, &labels).into_iter().enumerate() {
            if x >= x0 && x < x1 {
                // The close box occupies the trailing cells of the tab.
                return if x >= x1 - CLOSE_COLS * l.cell.advance {
                    Hit::TabClose(index)
                } else {
                    Hit::Tab(index)
                };
            }
        }
        return Hit::Elsewhere;
    }
    // The new side of a working-copy diff is the buffer, so a click there
    // places the caret in it. The old side is a committed version, which
    // is not a thing that can be typed into.
    if let Some(kind) = panels.diff {
        let frame = diff_frame(l, panels.diff_side_by_side);
        let in_body =
            y >= frame.body_y && x >= l.tree_w && x < l.width - SCROLLBAR_W && y >= l.text_y;
        if in_body {
            let row = ((y - frame.body_y) / l.cell.line_height + panels.diff_scroll.max(0.0))
                .max(0.0) as usize;
            // Only the new side of an editable diff takes a caret; the
            // rest of the body is still a row, so a right-click there
            // knows which change it is over.
            return if kind == DiffKind::Editable && x >= frame.new_text_x {
                Hit::DiffText {
                    row,
                    column: ((x - frame.new_text_x) / l.cell.advance).max(0.0).round() as usize,
                }
            } else {
                Hit::DiffRow(row)
            };
        }
    }
    if let Some(panel) = git.filter(|_| x < l.tree_w) {
        let row = (y / l.cell.line_height).floor().max(0.0) as usize + panel.scroll;
        // Row 0 is the Changes / History switch.
        if row == panel.scroll {
            let split = 10.0 * l.cell.advance;
            return Hit::GitMode(if x < split {
                GitMode::Changes
            } else {
                GitMode::History
            });
        }
        if panel.change_at(row).is_some() {
            // The trailing cells toggle staging; the rest opens the file.
            return if x >= l.tree_w - STAGE_COLS * l.cell.advance {
                Hit::GitStage(row)
            } else {
                Hit::GitRow(row)
            };
        }
        if panel.commit_at(row).is_some() {
            return Hit::GitCommit(row);
        }
        if panel.init_at(row) {
            return Hit::GitInit;
        }
        return Hit::Elsewhere;
    }
    if let Some(tree) = tree {
        if x < l.tree_w {
            let row = (y / l.cell.line_height).floor().max(0.0) as usize + tree.scroll;
            return if row < tree.rows.len() {
                Hit::Tree(row)
            } else {
                Hit::Elsewhere
            };
        }
    }

    // A fenced block in the preview takes its own clicks, so the code in
    // it can be copied out. Rows are not all one height, so the line is
    // found by walking them rather than by dividing.
    if let Some(preview) = panels.preview.filter(|p| !p.is_empty()) {
        if x >= l.tree_w && y >= l.text_y {
            let top = panels.preview_scroll.max(0.0) as usize;
            let slip = (panels.preview_scroll.max(0.0).fract() * l.cell.line_height).round();
            let mut row_y = l.text_y + l.cell.line_height - slip;
            for (offset, line) in preview.iter().skip(top).enumerate() {
                let height = line.rows() as f32 * l.cell.line_height;
                if y >= row_y && y < row_y + height {
                    return match line.code {
                        Some(_) => Hit::PreviewCode(top + offset),
                        None => Hit::Elsewhere,
                    };
                }
                row_y += height;
                if row_y > l.text_y + l.text_h {
                    break;
                }
            }
            return Hit::Elsewhere;
        }
    }

    // The two full-area pages are tested here rather than up front,
    // because that is where they are: they fill the text area and nothing
    // else. Checked before the chrome, they swallowed every click in the
    // sidebar and — once the settings page became a tab — the click on
    // the tab that would have left it.
    if panels.settings_open {
        let rows = setting_rows();
        let (ox, oy, width) = settings_origin(l, rows.len());
        if x >= ox && x < ox + width {
            let index = ((y - oy) / l.cell.line_height).floor();
            if index >= 0.0 {
                if let Some(SettingRow::Switch(setting)) = rows.get(index as usize) {
                    return Hit::Setting(*setting);
                }
            }
        }
        return Hit::Elsewhere;
    }
    if showing_landing(workspace) {
        let rows = landing_rows(recents);
        let (ox, oy) = landing_origin(l, rows.len());
        let width = LANDING_COLS * l.cell.advance;
        if x >= ox && x < ox + width {
            let index = ((y - oy) / l.cell.line_height).floor();
            if index >= 0.0 {
                if let Some(row) = rows.get(index as usize) {
                    if let Some(action) = row.action {
                        return Hit::Landing(action);
                    }
                }
            }
        }
        return Hit::Elsewhere;
    }
    Hit::Text
}

fn tab_extents(l: &Layout, labels: &[String]) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(labels.len());
    let mut x = l.tree_w;
    for label in labels {
        let w = (label.chars().count() as f32 + 2.0 + CLOSE_COLS) * l.cell.advance;
        out.push((x, x + w));
        x += w;
    }
    out
}

// ---- drawing ------------------------------------------------------------

impl<'a> Scene<'a> {
    pub fn panels(&self) -> Panels<'a> {
        Panels {
            workspace: self.workspace,
            tree: self.tree,
            git: self.git,
            recents: self.recents,
            search_open: self.search.is_some(),
            // Either kind of preview counts: an SVG showing as a picture
            // is as much "previewing" as rendered Markdown is.
            preview_open: self.preview.is_some() || self.image.is_some(),
            settings_open: self.settings.is_some(),
            context: self.context,
            diff: self.diff.map(|_| match self.diff_editable {
                true => DiffKind::Editable,
                false => DiffKind::ReadOnly,
            }),
            diff_scroll: self.diff_scroll,
            diff_side_by_side: self.diff_side_by_side,
            preview: self.preview,
            preview_scroll: self.preview_scroll,
        }
    }
}

pub fn draw(renderer: &mut Renderer, scene: &Scene<'_>, scratch: &mut Scratch) {
    let l = layout(
        renderer,
        scene.workspace,
        Chrome {
            sidebar: scene.tree.is_some() || scene.git.is_some(),
            sidebar_cols: scene.sidebar_cols,
            // The prompt shares the find bar's row, so either one reserves
            // it. Checking only `find` here drew the prompt over the first
            // line of the file.
            find: scene.find.is_some() || scene.prompt.is_some(),
        },
    );
    let theme = scene.theme;
    let labels = scene.workspace.labels();
    let tabs = tab_extents(&l, &labels);

    let mut f = renderer.frame(theme.bg);

    // ---- file tree ----
    if let Some(tree) = scene.tree {
        f.rect(0.0, 0.0, l.tree_w, l.height, theme.panel_bg);
        let active_path = scene.workspace.active().and_then(|e| e.path.as_deref());

        let visible = (l.height / l.cell.line_height).ceil() as usize;
        for row in 0..visible {
            let Some(entry) = tree.rows.get(tree.scroll + row) else {
                break;
            };
            let y = row as f32 * l.cell.line_height;
            let baseline = y + l.cell.ascent;
            let editing = tree.edit_at == Some(tree.scroll + row);

            // The row being typed into is highlighted like the active
            // file, because that is what it is about to be.
            tint(
                &mut f,
                scene,
                Hit::Tree(tree.scroll + row),
                0.0,
                y,
                l.tree_w,
                l.cell.line_height,
            );
            if editing || Some(entry.path.as_path()) == active_path {
                f.rect(0.0, y, l.tree_w, l.cell.line_height, theme.panel_active);
            }

            // Leaves a column for the disclosure caret, which hangs left.
            let indent = 2.0 + entry.depth as f32 * 2.0;
            let x = indent * l.cell.advance;

            // A caret for directories, so open and closed are tellable
            // apart without colour alone.
            if entry.is_dir {
                f.text(
                    x - l.cell.advance,
                    baseline,
                    if entry.expanded { "v" } else { ">" },
                    theme.gutter_fg,
                );
            }

            scratch.text.clear();
            let room = (l.tree_w / l.cell.advance - indent - 1.0).max(1.0) as usize;
            if editing {
                // The name as typed, with a block caret after it. No
                // separate text-input widget: one row, one caret, Enter
                // and Escape, which is the whole of what naming a file
                // needs.
                let typed = tree.typed().unwrap_or_default();
                let shown = typed.chars().count().min(room.saturating_sub(1));
                let tail: String = typed.chars().skip(typed.chars().count() - shown).collect();
                f.text(x, baseline, &tail, theme.fg);
                f.rect(
                    x + shown as f32 * l.cell.advance,
                    y + 2.0,
                    l.cell.advance,
                    l.cell.line_height - 4.0,
                    theme.cursor,
                );
                continue;
            }
            truncate_tail(&mut scratch.text, &entry.name, room);
            let colour = if entry.is_dir {
                theme.tree_dir
            } else if Some(entry.path.as_path()) == active_path {
                theme.fg
            } else {
                theme.tree_file
            };
            f.text(x, baseline, &scratch.text, colour);
        }
    }

    // ---- source control panel, sharing the sidebar ----
    if let Some(git) = scene.git {
        f.rect(0.0, 0.0, l.tree_w, l.height, theme.panel_bg);
        let rows = git.rows();
        let visible = (l.height / l.cell.line_height).ceil() as usize;

        for offset in 0..visible {
            let index = git.scroll + offset;
            let Some(row) = rows.get(index) else { break };
            let y = offset as f32 * l.cell.line_height;
            let baseline = y + l.cell.ascent;
            for what in [
                Hit::GitRow(index),
                Hit::GitCommit(index),
                Hit::GitInit,
                Hit::GitStage(index),
            ] {
                let narrow = matches!(what, Hit::GitStage(_));
                let x = if narrow {
                    l.tree_w - STAGE_COLS * l.cell.advance
                } else {
                    0.0
                };
                tint(&mut f, scene, what, x, y, l.tree_w - x, l.cell.line_height);
            }

            match row {
                GitRowKind::Blank => {}
                GitRowKind::Tabs => {
                    // Two words acting as tabs, the active one lit.
                    let mut x = l.cell.advance;
                    for (mode, label) in
                        [(GitMode::Changes, "changes"), (GitMode::History, "history")]
                    {
                        let on = git.mode == mode;
                        if on {
                            f.rect(
                                x - 0.5 * l.cell.advance,
                                y,
                                (label.chars().count() as f32 + 1.0) * l.cell.advance,
                                l.cell.line_height,
                                theme.panel_active,
                            );
                        }
                        f.text(
                            x,
                            baseline,
                            label,
                            if on { theme.fg } else { theme.tree_file },
                        );
                        x += (label.chars().count() as f32 + 2.0) * l.cell.advance;
                    }
                }
                GitRowKind::Commit(index) => {
                    let Some(commit) = git.commits.get(*index) else {
                        continue;
                    };
                    f.text(
                        l.cell.advance,
                        baseline,
                        &commit.short,
                        theme.syntax.function,
                    );
                    // Who made it, between the hash and what they said: a
                    // history is read for both, and the name is the
                    // shorter of the two. It takes a share of what is
                    // left rather than a fixed column, and steps aside
                    // entirely when the sidebar is too narrow to hold a
                    // name and a summary at once — the divider is
                    // draggable, and this is one of the things to drag
                    // it for.
                    let columns = l.tree_w / l.cell.advance;
                    let mut column = 9.0;
                    let for_author = ((columns - column) * AUTHOR_SHARE).min(AUTHOR_COLS) as usize;
                    if for_author >= 5 {
                        scratch.text.clear();
                        shorten_name(&mut scratch.text, &commit.author, for_author);
                        f.text(
                            l.cell.advance * column,
                            baseline,
                            &scratch.text,
                            theme.status_fg,
                        );
                        column += for_author as f32 + 1.0;
                    }
                    scratch.text.clear();
                    let room = (columns - column - 1.0).max(6.0) as usize;
                    truncate_tail(&mut scratch.text, &commit.summary, room);
                    f.text(
                        l.cell.advance * column,
                        baseline,
                        &scratch.text,
                        theme.tree_file,
                    );
                }
                GitRowKind::Header(text) => {
                    f.text(l.cell.advance, baseline, text, theme.status_fg);
                }
                GitRowKind::Empty(text) => {
                    f.text(l.cell.advance, baseline, text, theme.gutter_fg);
                }
                // Drawn as a button, because it is the one row here that
                // does something to the world rather than describing it.
                GitRowKind::Init => {
                    const LABEL: &str = "Initialize Repository";
                    let width = (LABEL.chars().count() as f32 + 2.0) * l.cell.advance;
                    let x = ((l.tree_w - width) / 2.0).max(l.cell.advance);
                    f.rect(
                        x,
                        y,
                        width.min(l.tree_w - 2.0 * x),
                        l.cell.line_height,
                        theme.panel_active,
                    );
                    f.text(x + l.cell.advance, baseline, LABEL, theme.fg);
                }
                GitRowKind::Change(change) => {
                    let Some(change) = git.changes.get(*change) else {
                        continue;
                    };
                    // git's own letter, in the colour the gutter uses for
                    // that kind of change.
                    let colour = match change.status {
                        'A' | '?' => theme.vcs_added,
                        'D' => theme.vcs_removed,
                        _ => theme.vcs_modified,
                    };
                    scratch.text.clear();
                    scratch.text.push(change.status);
                    f.text(l.cell.advance, baseline, &scratch.text, colour);

                    scratch.text.clear();
                    let room = (l.tree_w / l.cell.advance - 3.0 - STAGE_COLS).max(4.0) as usize;
                    truncate_tail(&mut scratch.text, &change.display, room);
                    f.text(
                        l.cell.advance * 3.0,
                        baseline,
                        &scratch.text,
                        theme.tree_file,
                    );

                    // Stage with +, unstage with −.
                    f.text(
                        l.tree_w - STAGE_COLS * l.cell.advance,
                        baseline,
                        if change.staged { "-" } else { "+" },
                        theme.gutter_fg,
                    );
                }
            }
        }
    }

    // A hairline at the sidebar's edge, so the drag target is visible.
    if l.tree_w > 0.0 {
        f.rect(l.tree_w - 1.0, 0.0, 1.0, l.height, theme.panel_active);
    }

    // ---- editor, or the landing page when there is nothing to edit ----
    if let Some(settings) = scene.settings {
        draw_settings(&mut f, &l, settings, scene, scratch);
    } else if let Some(view) = scene.image {
        draw_image(&mut f, &l, view, theme, scratch);
    } else if let Some(lines) = scene.diff {
        draw_diff(&mut f, &l, lines, scene, theme, scratch);
    } else if let Some(lines) = scene.preview {
        draw_preview(&mut f, &l, lines, scene, theme);
    } else if showing_landing(scene.workspace) {
        draw_landing(&mut f, &l, scene, scratch);
    } else {
        match scene.workspace.active() {
            Some(editor) => draw_editor(&mut f, &l, editor, scene, scratch),
            None => {
                // A project is open but no file is: the tree is the
                // affordance, so this only needs to point at it.
                let hint = key!("+P  open a file        ", "+shift+P  commands");
                let x = l
                    .text_x
                    .max((l.width - hint.chars().count() as f32 * l.cell.advance) / 2.0);
                f.text(x, l.text_y + l.text_h / 2.0, hint, theme.gutter_fg);
            }
        }
    }

    // ---- tab strip ----
    //
    // After the editor, not before: with fractional scrolling the top
    // row is usually clipped, and a strip drawn first had document
    // text sliding over it.
    if l.tab_h > 0.0 {
        f.rect(l.tree_w, 0.0, l.width - l.tree_w, l.tab_h, theme.panel_bg);
        for (index, (x0, x1)) in tabs.iter().enumerate() {
            if *x0 > l.width {
                break;
            }
            let active = index == scene.workspace.active;
            if active {
                f.rect(*x0, 0.0, x1 - x0, l.tab_h, theme.bg);
                // A bright rule under the active tab.
                f.rect(*x0, l.tab_h - 2.0, x1 - x0, 2.0, theme.cursor);
            }
            tint(&mut f, scene, Hit::Tab(index), *x0, 0.0, x1 - x0, l.tab_h);
            let close_w = CLOSE_COLS * l.cell.advance;
            tint(
                &mut f,
                scene,
                Hit::TabClose(index),
                x1 - close_w,
                0.0,
                close_w,
                l.tab_h,
            );
            // A provisional tab is drawn dimmer even when it is active,
            // so "this will be replaced by the next click" is visible
            // without having to remember how it was opened.
            let tab = scene.workspace.tabs.get(index);
            let preview = tab
                .and_then(crate::workspace::Tab::editor)
                .is_some_and(|b| b.preview);
            // A diff is the same file in another mode, so its tab is the
            // file's tab in another colour — the one the gutter marks a
            // modified line with. The `±` in the label says the same
            // thing, but a tab is read at a glance and a colour is what
            // reaches a glance.
            let diff = tab.is_some_and(|t| t.diff().is_some());
            let colour = match (active, preview, diff) {
                (_, _, true) if active => theme.vcs_modified,
                (_, _, true) => theme.vcs_modified.alpha(0.7),
                (true, false, _) => theme.fg,
                (true, true, _) => theme.status_fg,
                (false, _, _) => theme.tree_file,
            };
            f.text(
                x0 + 1.0 * l.cell.advance,
                l.cell.ascent,
                &labels[index],
                colour,
            );
            // Close box, dim until the tab is the active one.
            f.text(
                x1 - CLOSE_COLS * l.cell.advance + 0.5 * l.cell.advance,
                l.cell.ascent,
                "x",
                if active {
                    theme.status_fg
                } else {
                    theme.gutter_fg
                },
            );
        }
    }

    // ---- status bar ----
    let status_y = l.text_y + l.text_h;
    f.rect(0.0, status_y, l.width, l.cell.line_height, theme.status_bg);

    let slots = status_buttons(&scene.panels());
    let extents = status_button_extents(&l, &slots);
    let mut text_x = l.cell.advance;
    for (slot, (x0, x1)) in slots.iter().zip(&extents) {
        if slot.active {
            f.rect(
                *x0,
                status_y,
                x1 - x0,
                l.cell.line_height,
                theme.panel_active,
            );
        }
        tint(
            &mut f,
            scene,
            Hit::Status(slot.button),
            *x0,
            status_y,
            x1 - x0,
            l.cell.line_height,
        );
        // An active toggle gets a rule along its top edge as well as a
        // fill, so it reads as pressed rather than merely tinted.
        if slot.active {
            f.rect(*x0, status_y, x1 - x0, 2.0, theme.cursor);
        }
        let size = icon_size(&l);
        let drawn = icons::width(size);
        icons::draw(
            &mut f,
            slot.icon,
            (x0 + (x1 - x0 - drawn) / 2.0).round(),
            (status_y + (l.cell.line_height - drawn) / 2.0).round(),
            size,
            if slot.active {
                theme.fg
            } else {
                theme.tree_file
            },
        );
        text_x = x1 + 2.0 * l.cell.advance;
    }
    // A divider, so the controls are not mistaken for the readout.
    if let Some((_, end)) = extents.last() {
        f.rect(
            end + 0.5 * l.cell.advance,
            status_y + 4.0,
            1.0,
            l.cell.line_height - 8.0,
            theme.panel_active,
        );
    }

    scratch.text.clear();
    match scene.status {
        Some(message) => scratch.text.push_str(message),
        None => {
            if let Some(editor) = scene.workspace.active() {
                let (line, col) = editor.line_col(editor.sel.primary().head);
                let _ = write!(
                    scratch.text,
                    "{}{}:{}   {} lines",
                    if editor.dirty { "* " } else { "" },
                    line + 1,
                    col + 1,
                    editor.doc.len_lines()
                );
                if editor.sel.len() > 1 {
                    let _ = write!(scratch.text, "   {} cursors", editor.sel.len());
                }
                if let Some(language) = editor.language_name() {
                    let _ = write!(scratch.text, "   {language}");
                    // What the server is busy with outranks the fact
                    // that it is up: while rust-analyzer indexes, every
                    // answer is empty, and "lsp" alone does not explain
                    // why nothing is happening.
                    match (scene.lsp_busy, scene.lsp) {
                        (Some(doing), _) => {
                            let _ = write!(scratch.text, " · {doing}");
                        }
                        (None, Some(state)) => {
                            let _ = write!(scratch.text, " · {state}");
                        }
                        (None, None) => {}
                    }
                }
            }
            if scene.autosave {
                let _ = write!(scratch.text, "   autosave");
            }
            if let Some(branch) = scene.branch {
                let _ = write!(scratch.text, "   {branch}");
            }
            if let Some(root) = &scene.workspace.root {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let _ = write!(scratch.text, "   [{name}]");
            }
        }
    }
    f.text(
        text_x,
        status_y + l.cell.ascent,
        &scratch.text,
        if scene.status.is_some() {
            theme.fg
        } else {
            theme.status_fg
        },
    );

    // ---- find bar, or the prompt that shares its row ----
    if let Some(prompt) = scene.prompt {
        draw_prompt(&mut f, &l, prompt, theme, scratch);
    } else if let Some(find) = scene.find {
        draw_find_bar(&mut f, &l, find, theme, scratch);
    }

    // ---- overlays, on top of everything ----
    if let Some(picker) = scene.picker {
        draw_picker(&mut f, &l, picker, theme, scratch);
    }
    if let Some(search) = scene.search {
        draw_project_search(&mut f, &l, search, theme, scratch);
    }
    if let Some(completion) = scene.completion {
        draw_completion(&mut f, &l, completion, scene, scratch);
    }
    // After the completion popup: if both are up, the list is what is
    // being used and the tooltip is what is in the way.
    if let Some(hover) = scene.hover.filter(|_| scene.completion.is_none()) {
        draw_hover(&mut f, &l, hover, theme);
    }
    if let Some(menu) = scene.context {
        draw_context_menu(&mut f, &l, menu, theme, scratch);
    }

    f.present();
}

/// Where the caret is on screen, for anchoring a popup to it.
///
/// Shared with the completion popup rather than recomputed: a menu that
/// appeared a line off from the caret would be its own bug.
pub fn caret_point(l: &Layout, editor: &Editor, tab_width: usize) -> (f32, f32) {
    let head = editor.sel.primary().head;
    let line = editor.doc.char_to_line(head);
    let col = head - editor.doc.line_to_char(line);
    let caret_x = match editor.doc.line(line) {
        Some(slice) => visual_col(slice, col, tab_width) as f32 - editor.scroll_x.max(0.0),
        None => 0.0,
    };
    let row = line.saturating_sub(editor.top_line()) as f32;
    (
        l.text_x + caret_x * l.cell.advance,
        // From the top of the text area, not the tab strip: with the
        // find bar open those differ by its height, and every popup
        // anchored here was that far out.
        l.text_y + (row + 1.0) * l.cell.line_height - editor.pixel_offset(l.cell.line_height),
    )
}

/// The completion popup, anchored under the cursor.
fn draw_completion(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    completion: &Completion,
    scene: &Scene<'_>,
    scratch: &mut Scratch,
) {
    const ROWS: usize = 10;
    let theme = scene.theme;
    let Some(editor) = scene.workspace.active() else {
        return;
    };
    if completion.is_empty() {
        return;
    }

    // Anchor on the cursor, in the same coordinates the caret is drawn in.
    let head = editor.sel.primary().head;
    let line = editor.doc.char_to_line(head);
    if line < editor.top_line() {
        return;
    }
    let col = head - editor.doc.line_to_char(line);
    let caret_x = match editor.doc.line(line) {
        Some(slice) => visual_col(slice, col, scene.tab_width) as f32 - editor.scroll_x.max(0.0),
        None => 0.0,
    };
    let x = (l.text_x + caret_x * l.cell.advance)
        .max(l.text_x)
        .min(l.width - 24.0 * l.cell.advance);
    let row = (line - editor.top_line()) as f32;

    let widest = completion
        .shown
        .iter()
        .take(ROWS)
        .filter_map(|_| None::<usize>)
        .max()
        .unwrap_or(0);
    let _ = widest;
    let shown = completion.shown.len().min(ROWS);
    let width = 40.0 * l.cell.advance;
    let height = shown as f32 * l.cell.line_height;

    // Below the cursor normally, above it when there is no room.
    let below = l.text_y + (row + 1.0) * l.cell.line_height;
    let y = if below + height > l.text_y + l.text_h {
        (l.text_y + row * l.cell.line_height - height).max(l.text_y)
    } else {
        below
    };

    f.rect(x, y, width, height, theme.panel_bg);
    f.rect(x, y, 2.0, height, theme.cursor);

    let first = completion.selected.saturating_sub(ROWS - 1);
    for offset in 0..shown {
        let index = first + offset;
        let Some(entry) = completion.get(index) else {
            break;
        };
        let row_y = y + offset as f32 * l.cell.line_height;
        let selected = index == completion.selected;
        if selected {
            f.rect(x, row_y, width, l.cell.line_height, theme.panel_active);
        }
        scratch.text.clear();
        truncate_tail(&mut scratch.text, &entry.label, 26);
        f.text(
            x + l.cell.advance,
            row_y + l.cell.ascent,
            &scratch.text,
            if selected { theme.fg } else { theme.tree_file },
        );
        if let Some(detail) = entry.detail.as_deref() {
            scratch.text.clear();
            truncate_tail(&mut scratch.text, detail, 11);
            let detail_x = x + width - l.cell.advance * (scratch.text.chars().count() as f32 + 1.0);
            f.text(
                detail_x,
                row_y + l.cell.ascent,
                &scratch.text,
                theme.gutter_fg,
            );
        }
    }
}

/// Where the scrollbar thumb sits, or `None` when everything fits.
///
/// Shared by drawing and by dragging, so the thumb cannot be drawn
/// somewhere the mouse does not find it.
pub fn scrollbar_extent(l: &Layout, total_lines: usize, scroll: f32) -> Option<(f32, f32)> {
    let rows = l.rows as f32;
    let total = total_lines as f32;
    if total <= rows || l.text_h <= 0.0 {
        return None;
    }
    // A thumb proportional to how much is visible, with a floor so it
    // stays grabbable in a very long file.
    let fraction = (rows / total).clamp(0.0, 1.0);
    let height = (l.text_h * fraction).max(24.0).min(l.text_h);
    let max_scroll = (total - rows).max(1.0);
    let travel = l.text_h - height;
    let top = l.text_y + travel * (scroll.max(0.0) / max_scroll).clamp(0.0, 1.0);
    Some((top, height))
}

/// Turns a y position on the scrollbar into a scroll offset in lines.
pub fn scrollbar_scroll_at(l: &Layout, total_lines: usize, y: f32) -> f32 {
    let rows = l.rows as f32;
    let total = total_lines as f32;
    let max_scroll = (total - rows).max(0.0);
    let Some((_, height)) = scrollbar_extent(l, total_lines, 0.0) else {
        return 0.0;
    };
    let travel = (l.text_h - height).max(1.0);
    // Grab the thumb by its middle, which is where the pointer feels like
    // it is holding it.
    let offset = (y - l.text_y - height / 2.0).clamp(0.0, travel);
    (offset / travel) * max_scroll
}

/// The horizontal scrollbar's track, which is as wide as the text area.
fn h_track(l: &Layout) -> (f32, f32) {
    let x = l.text_x;
    (x, (l.width - SCROLLBAR_W - x).max(0.0))
}

/// Where the horizontal thumb sits, or `None` when the widest line in
/// view already fits.
///
/// Measured against what is on screen rather than the whole file, for the
/// same reason the scrolling is: the longest line in a three-million-line
/// file is not a thing this editor is allowed to go looking for.
pub fn h_scrollbar_extent(l: &Layout, widest_cols: usize, scroll_x: f32) -> Option<(f32, f32)> {
    let cols = l.cols() as f32;
    let total = widest_cols as f32;
    let (track_x, track_w) = h_track(l);
    if total <= cols || track_w <= 0.0 {
        return None;
    }
    let fraction = (cols / total).clamp(0.0, 1.0);
    let width = (track_w * fraction).max(24.0).min(track_w);
    let max_scroll = (total - cols).max(1.0);
    let travel = track_w - width;
    let left = track_x + travel * (scroll_x.max(0.0) / max_scroll).clamp(0.0, 1.0);
    Some((left, width))
}

/// Turns an x position on the horizontal scrollbar into a column offset.
pub fn h_scrollbar_scroll_at(l: &Layout, widest_cols: usize, x: f32) -> f32 {
    let cols = l.cols() as f32;
    let total = widest_cols as f32;
    let max_scroll = (total - cols).max(0.0);
    let Some((_, width)) = h_scrollbar_extent(l, widest_cols, 0.0) else {
        return 0.0;
    };
    let (track_x, track_w) = h_track(l);
    let travel = (track_w - width).max(1.0);
    // By the middle of the thumb, as the vertical one is.
    let offset = (x - track_x - width / 2.0).clamp(0.0, travel);
    (offset / travel) * max_scroll
}

/// Whether the horizontal scrollbar is showing.
///
/// The band it occupies floats over the last row of text, so it is only
/// in the way when there is something to scroll to — and only when the
/// text area is showing a buffer at all. A preview wraps and a diff has
/// its own columns; a click along the bottom of either of those is a
/// click on what is drawn there.
fn h_scrollbar_band(l: &Layout, panels: &Panels<'_>) -> bool {
    if panels.preview_open || panels.workspace.diff().is_some() {
        return false;
    }
    let Some(editor) = panels.workspace.active() else {
        return false;
    };
    h_scrollbar_extent(l, editor.widest_visible(l.rows, l.cols()), editor.scroll_x).is_some()
}

/// How many columns the preview lays out to: the pane it is drawn in.
///
/// The document's margins and the cap on its measure come out of this in
/// [`markdown::column`], which is also what draws it, so the width the
/// text was wrapped to and the width it is drawn at are one number.
pub fn preview_columns(l: &Layout) -> usize {
    (((l.width - l.tree_w - SCROLLBAR_W) / l.cell.advance) as usize).max(24)
}

fn draw_context_menu(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    menu: &ContextMenu,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    let (x, y, width, height) = context_bounds(l, menu);
    f.rect(x, y, width, height, theme.panel_bg);
    f.rect(x, y, width, 1.0, theme.panel_active);
    f.rect(x, y + height - 1.0, width, 1.0, theme.panel_active);

    for (index, entry) in menu.entries.iter().enumerate() {
        let row_y = y + 2.0 + index as f32 * l.cell.line_height;
        if entry.is_separator() {
            f.rect(
                x + l.cell.advance,
                row_y + l.cell.line_height / 2.0,
                width - 2.0 * l.cell.advance,
                1.0,
                theme.panel_active,
            );
            continue;
        }
        let selected = index == menu.selected;
        if selected {
            f.rect(x, row_y, width, l.cell.line_height, theme.panel_active);
        }
        f.text(
            x + 1.5 * l.cell.advance,
            row_y + l.cell.ascent,
            &entry.label,
            if selected { theme.fg } else { theme.tree_file },
        );
        if !entry.hint.is_empty() {
            scratch.text.clear();
            scratch.text.push_str(entry.hint);
            let hint_x = x + width - l.cell.advance * (entry.hint.chars().count() as f32 + 1.5);
            f.text(
                hint_x,
                row_y + l.cell.ascent,
                &scratch.text,
                theme.gutter_fg,
            );
        }
    }
}

fn draw_settings(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    settings: &Settings,
    scene: &Scene<'_>,
    scratch: &mut Scratch,
) {
    let theme = scene.theme;
    let rows = setting_rows();
    let (ox, oy, width) = settings_origin(l, rows.len());

    for (index, row) in rows.iter().enumerate() {
        let y = oy + index as f32 * l.cell.line_height;
        let baseline = y + l.cell.ascent;
        match row {
            SettingRow::Blank => {}
            SettingRow::Title => {
                f.text(ox, baseline, "Settings", theme.fg);
            }
            SettingRow::Heading(text) => {
                f.text(ox, baseline, text, theme.status_fg);
            }
            SettingRow::Switch(setting) => {
                tint(
                    f,
                    scene,
                    Hit::Setting(*setting),
                    ox,
                    y,
                    width,
                    l.cell.line_height,
                );
                let on = setting.value(settings);
                // A box that is filled or empty, rather than a tick: at
                // one weight a tick and a cross look the same.
                let box_x = ox + 2.0 * l.cell.advance;
                let size = (l.cell.line_height * 0.42).round();
                let box_y = (y + (l.cell.line_height - size) / 2.0).round();
                f.rect(box_x, box_y, size, size, theme.panel_active);
                if on {
                    let pad = (size / 4.0).round();
                    f.rect(
                        box_x + pad,
                        box_y + pad,
                        size - 2.0 * pad,
                        size - 2.0 * pad,
                        theme.cursor,
                    );
                }
                scratch.text.clear();
                let room = ((width / l.cell.advance) as usize).saturating_sub(6);
                truncate_tail(&mut scratch.text, setting.label(), room);
                f.text(
                    box_x + size + l.cell.advance,
                    baseline,
                    &scratch.text,
                    if on { theme.fg } else { theme.tree_file },
                );
            }
        }
    }

    let hint = "changes are saved as you make them";
    f.text(
        ox,
        oy + rows.len() as f32 * l.cell.line_height + l.cell.ascent,
        hint,
        theme.gutter_fg,
    );
}

fn draw_image(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    view: &crate::imageview::ImageView,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    // A caption, so the real pixel size is visible even when the picture
    // has been scaled to fit.
    scratch.text.clear();
    let _ = write!(scratch.text, "{} × {}", view.width, view.height);
    f.text(
        l.text_x + l.cell.advance,
        l.text_y + l.cell.ascent,
        &scratch.text,
        theme.gutter_fg,
    );

    let margin = 2.0 * l.cell.advance;
    let top = l.text_y + l.cell.line_height;
    let area_w = (l.width - l.text_x - 2.0 * margin).max(1.0);
    let area_h = (l.text_h - l.cell.line_height - margin).max(1.0);
    let (w, h) = crate::imageview::fit(view.width, view.height, area_w, area_h);

    // Centred in what is left.
    let x = (l.text_x + margin + (area_w - w) / 2.0).round();
    let y = (top + (area_h - h) / 2.0).round();

    // A one-pixel frame, so a white image against a dark window still has
    // an edge.
    f.rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, theme.panel_active);
    f.image(&view.image, x, y, w, h);
}

/// Where the parts of a diff sit.
///
/// Shared by drawing and hit-testing, because an editable diff means a
/// click has to land on the character it looks like it landed on, and two
/// copies of this arithmetic would drift apart the first time either
/// changed.
struct DiffFrame {
    body_y: f32,
    rows: usize,
    /// Left edge of the new side's text, where the working copy is shown.
    new_text_x: f32,
    /// How wide that text column is.
    new_text_w: f32,
}

fn diff_frame(l: &Layout, side_by_side: bool) -> DiffFrame {
    let title_h = l.cell.line_height;
    let body_y = l.text_y + title_h;
    let rows = ((l.text_h - title_h) / l.cell.line_height).ceil() as usize;

    if side_by_side {
        let full = l.width - l.text_x - SCROLLBAR_W;
        let column = (full / 2.0).floor();
        let numbers = SPLIT_NUMBER_COLS * l.cell.advance;
        DiffFrame {
            body_y,
            rows,
            new_text_x: l.text_x + column + numbers,
            new_text_w: column - numbers,
        }
    } else {
        // Two line-number columns, then the sign.
        let text_x = l.text_x + UNIFIED_NUMBER_COLS * l.cell.advance;
        DiffFrame {
            body_y,
            rows,
            new_text_x: text_x + 2.0 * l.cell.advance,
            new_text_w: l.width - text_x - SCROLLBAR_W - 2.0 * l.cell.advance,
        }
    }
}

/// Cells reserved for the two line-number columns and the sign.
const UNIFIED_NUMBER_COLS: f32 = 9.0;
/// Cells reserved for one line-number column in the split view.
const SPLIT_NUMBER_COLS: f32 = 5.0;

fn draw_diff(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    lines: &[lithe_vcs::DiffLine],
    scene: &Scene<'_>,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    use lithe_vcs::LineKind;

    // Title row, so it is obvious this is a diff and of what.
    let title_h = l.cell.line_height;
    f.rect(
        l.text_x,
        l.text_y,
        l.width - l.text_x,
        title_h,
        theme.panel_bg,
    );
    scratch.text.clear();
    let _ = write!(
        scratch.text,
        "diff · {}",
        scene.diff_title.unwrap_or("working copy")
    );
    f.text(
        l.text_x + l.cell.advance,
        l.text_y + l.cell.ascent,
        &scratch.text,
        theme.fg,
    );
    let hint = if scene.diff_editable {
        "the right side is the file · type in it"
    } else {
        "a committed version · read only"
    };
    f.text(
        l.width - l.cell.advance * (hint.chars().count() as f32 + 1.0),
        l.text_y + l.cell.ascent,
        hint,
        theme.gutter_fg,
    );

    let frame = diff_frame(l, scene.diff_side_by_side);
    let DiffFrame { body_y, rows, .. } = frame;
    let top = scene.diff_scroll.max(0.0) as usize;
    let slip = (scene.diff_scroll.max(0.0).fract() * l.cell.line_height).round();

    if scene.diff_side_by_side {
        draw_diff_split(f, l, lines, &frame, scene, theme, scratch);
        if let Some((top_y, height)) =
            scrollbar_extent(l, lithe_vcs::pair_columns(lines).len(), scene.diff_scroll)
        {
            let x = l.width - SCROLLBAR_W;
            f.rect(x, l.text_y, SCROLLBAR_W, l.text_h, theme.scrollbar_track);
            f.rect(
                x + 2.0,
                top_y,
                SCROLLBAR_W - 4.0,
                height,
                theme.scrollbar_thumb,
            );
        }
        return;
    }
    let numbers = UNIFIED_NUMBER_COLS;

    for row in 0..=rows {
        let Some(line) = lines.get(top + row) else {
            break;
        };
        let y = body_y + row as f32 * l.cell.line_height - slip;
        let baseline = y + l.cell.ascent;

        let (sign, colour, tint) = match line.kind {
            LineKind::Added => ("+", theme.vcs_added, Some(theme.diff_added_bg)),
            LineKind::Removed => ("-", theme.vcs_removed, Some(theme.diff_removed_bg)),
            LineKind::Skipped => ("", theme.gutter_fg, None),
            LineKind::Context => (" ", theme.fg, None),
        };
        if let Some(tint) = tint {
            f.rect(
                l.text_x + numbers * l.cell.advance,
                y,
                l.width - l.text_x - numbers * l.cell.advance - SCROLLBAR_W,
                l.cell.line_height,
                tint,
            );
        }

        // Old and new line numbers, each blank where the line does not
        // exist on that side.
        scratch.text.clear();
        match (line.old, line.new) {
            (Some(old), Some(new)) => {
                let _ = write!(scratch.text, "{old:>4} {new:>4}");
            }
            (Some(old), None) => {
                let _ = write!(scratch.text, "{old:>4}     ");
            }
            (None, Some(new)) => {
                let _ = write!(scratch.text, "     {new:>4}");
            }
            (None, None) => {}
        }
        f.text(l.text_x, baseline, &scratch.text, theme.gutter_fg);

        let text_x = l.text_x + numbers * l.cell.advance;
        f.text(text_x, baseline, sign, colour);

        let room = (frame.new_text_w / l.cell.advance) as usize;
        diff_text(
            f,
            scene,
            line,
            // One column, so a removed line is read against the old
            // side and everything else against the new.
            line.kind == LineKind::Removed,
            text_x + 2.0 * l.cell.advance,
            baseline,
            room,
            if line.kind == LineKind::Skipped {
                theme.gutter_fg
            } else {
                colour
            },
            scratch,
        );
        if let Some(span) = selected_cells(scene, line) {
            f.rect(
                frame.new_text_x + span.start as f32 * l.cell.advance,
                y,
                (span.end - span.start).max(1) as f32 * l.cell.advance,
                l.cell.line_height,
                theme.selection,
            );
        }
        if let Some(column) = caret_column(scene, line) {
            f.rect(
                frame.new_text_x + column as f32 * l.cell.advance,
                y,
                (l.cell.advance * 0.12).max(1.5),
                l.cell.line_height,
                theme.cursor,
            );
        }
    }

    if let Some((top_y, height)) = scrollbar_extent(l, lines.len(), scene.diff_scroll) {
        let x = l.width - SCROLLBAR_W;
        f.rect(x, l.text_y, SCROLLBAR_W, l.text_h, theme.scrollbar_track);
        f.rect(
            x + 2.0,
            top_y,
            SCROLLBAR_W - 4.0,
            height,
            theme.scrollbar_thumb,
        );
    }
}

/// Draws one diff row's text, coloured by its own side's grammar.
///
/// Falls back to the flat colour when there is nothing to colour with:
/// a commit's diff spans many files and has no single document behind
/// it, and a language with no grammar has no spans either.
#[allow(clippy::too_many_arguments)]
fn diff_text(
    f: &mut lithe_gpu::Frame<'_>,
    scene: &Scene<'_>,
    line: &lithe_vcs::DiffLine,
    old_side: bool,
    x: f32,
    baseline: f32,
    room: usize,
    flat: Rgba,
    scratch: &mut Scratch,
) {
    use lithe_vcs::LineKind;

    let theme = scene.theme;
    let plain = |f: &mut lithe_gpu::Frame<'_>, scratch: &mut Scratch| {
        scratch.text.clear();
        truncate_tail(&mut scratch.text, &line.text, room);
        f.text(x, baseline, &scratch.text, flat);
    };

    // An elided run is a marker, not code, and a removed or added line
    // keeps its red or green: the colour is what says which side it is
    // on, and grammar colours would take that away.
    if line.kind == LineKind::Skipped
        || (old_side && line.kind == LineKind::Removed)
        || (!old_side && line.kind == LineKind::Added)
    {
        plain(f, scratch);
        return;
    }

    let (doc, spans) = if old_side {
        (scene.diff_old, scene.diff_old_spans)
    } else {
        (
            scene.workspace.active().map(|e| &e.doc),
            scene.diff_new_spans,
        )
    };
    let number = if old_side { line.old } else { line.new };
    let (Some(doc), Some(number)) = (doc, number) else {
        plain(f, scratch);
        return;
    };
    let index = number.saturating_sub(1);
    if spans.is_empty() || index >= doc.len_lines() {
        plain(f, scratch);
        return;
    }

    let chars = doc.line_len_chars(index).min(line.text.chars().count());
    scratch.styles.clear();
    scratch.styles.resize(chars, None);
    if chars > 0 {
        let rope = doc.rope();
        let start_char = doc.line_to_char(index);
        let start = rope.char_to_byte(start_char);
        let end = rope.char_to_byte(start_char + chars);
        for span in spans {
            if span.start >= end {
                break;
            }
            if span.end <= start {
                continue;
            }
            let from = rope.byte_to_char(span.start.max(start)) - start_char;
            let to = rope.byte_to_char(span.end.min(end)) - start_char;
            for style in scratch.styles[from.min(chars)..to.min(chars)].iter_mut() {
                *style = Some(span.kind);
            }
        }
    }

    // One run per colour change, as the editor does.
    let mut pen = x;
    let mut run = String::new();
    let mut kind = scratch.styles.first().copied().flatten();
    for (i, ch) in line.text.chars().take(room).enumerate() {
        let here = scratch.styles.get(i).copied().flatten();
        if here != kind && !run.is_empty() {
            pen = f.text(pen, baseline, &run, theme.colour(kind));
            run.clear();
        }
        kind = here;
        run.push(ch);
    }
    if !run.is_empty() {
        f.text(pen, baseline, &run, theme.colour(kind));
    }
}

/// Where the caret sits on a diff row, in cells from the start of the
/// new side's text.
///
/// `None` unless this row is the one the cursor is on. The mapping is the
/// line number: a diff row knows which line of the working copy it shows,
/// which is the same thing the buffer's cursor is measured in.
fn caret_column(scene: &Scene<'_>, line: &lithe_vcs::DiffLine) -> Option<usize> {
    if !scene.diff_editable {
        return None;
    }
    let editor = scene.workspace.active()?;
    let here = line.new?;
    let cursor = editor.sel.primary().head;
    let row = editor.doc.char_to_line(cursor);
    // Diff line numbers are 1-based, as every tool reports them.
    if row + 1 != here {
        return None;
    }
    Some(cursor - editor.doc.line_to_char(row))
}

/// The selected span on a diff row, in cells from the start of its text.
///
/// Empty when the row is not selected. Mapped through the line number,
/// like the caret: the row knows which line of the working copy it shows.
fn selected_cells(scene: &Scene<'_>, line: &lithe_vcs::DiffLine) -> Option<std::ops::Range<usize>> {
    if !scene.diff_editable {
        return None;
    }
    let editor = scene.workspace.active()?;
    let here = line.new?;
    let cursor = editor.sel.primary();
    if cursor.is_empty() {
        return None;
    }
    let row = here.checked_sub(1)?;
    if row >= editor.doc.len_lines() {
        return None;
    }
    let start = editor.doc.line_to_char(row);
    let end = start + editor.doc.line_len_chars(row);
    let (from, to) = (cursor.start().max(start), cursor.end().min(end));
    (from < to || (from == to && to < end)).then(|| from - start..to - start)
}

/// The same diff in two columns: the committed version on the left, the
/// working copy on the right.
fn draw_diff_split(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    lines: &[lithe_vcs::DiffLine],
    frame: &DiffFrame,
    scene: &Scene<'_>,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    use lithe_vcs::LineKind;

    let DiffFrame { body_y, rows, .. } = *frame;
    let top = scene.diff_scroll.max(0.0) as usize;
    let slip = (scene.diff_scroll.max(0.0).fract() * l.cell.line_height).round();
    let pairs = lithe_vcs::pair_columns(lines);
    let full = l.width - l.text_x - SCROLLBAR_W;
    let column = (full / 2.0).floor();
    let numbers = SPLIT_NUMBER_COLS * l.cell.advance;

    // A divider down the middle, so the two sides are clearly two sides.
    f.rect(l.text_x + column, body_y, 1.0, l.text_h, theme.panel_active);

    for row in 0..=rows {
        let Some((left, right)) = pairs.get(top + row) else {
            break;
        };
        let y = body_y + row as f32 * l.cell.line_height - slip;
        let baseline = y + l.cell.ascent;

        for (side, index) in [(0.0, left), (1.0, right)] {
            let origin = l.text_x + side * column;
            let Some(index) = index else {
                // Nothing on this side of the pair: a faint fill, so the
                // gap reads as "absent" rather than "end of file".
                f.rect(origin, y, column - 1.0, l.cell.line_height, theme.panel_bg);
                continue;
            };
            let Some(line) = lines.get(*index) else {
                continue;
            };

            // The left column shows the old side, the right the new.
            let (colour, tint, number) = match (side == 0.0, line.kind) {
                (true, LineKind::Removed) => {
                    (theme.vcs_removed, Some(theme.diff_removed_bg), line.old)
                }
                (false, LineKind::Added) => (theme.vcs_added, Some(theme.diff_added_bg), line.new),
                (true, _) => (theme.fg, None, line.old),
                (false, _) => (theme.fg, None, line.new),
            };
            if let Some(tint) = tint {
                f.rect(origin, y, column - 1.0, l.cell.line_height, tint);
            }

            scratch.text.clear();
            if let Some(n) = number {
                let _ = write!(scratch.text, "{n:>4}");
            }
            f.text(origin, baseline, &scratch.text, theme.gutter_fg);

            let room = ((frame.new_text_w / l.cell.advance) as usize).saturating_sub(1);
            diff_text(
                f,
                scene,
                line,
                side == 0.0,
                origin + numbers,
                baseline,
                room,
                if line.kind == LineKind::Skipped {
                    theme.gutter_fg
                } else {
                    colour
                },
                scratch,
            );
            // The caret belongs on the new side only; the old one is a
            // committed version and has no cursor in it.
            if side > 0.0 {
                if let Some(span) = selected_cells(scene, line) {
                    f.rect(
                        frame.new_text_x + span.start as f32 * l.cell.advance,
                        y,
                        (span.end - span.start).max(1) as f32 * l.cell.advance,
                        l.cell.line_height,
                        theme.selection,
                    );
                }
                if let Some(cell) = caret_column(scene, line) {
                    f.rect(
                        frame.new_text_x + cell as f32 * l.cell.advance,
                        y,
                        (l.cell.advance * 0.12).max(1.5),
                        l.cell.line_height,
                        theme.cursor,
                    );
                }
            }
        }
    }
}

/// What the pointer is resting on, as the server described it.
///
/// A language server answers `textDocument/hover` in Markdown, so the
/// preview's reader renders it: the same bold, the same code colouring,
/// from the same code. A signature is the commonest answer and is almost
/// always a fenced code block.
pub struct Hover {
    pub lines: Vec<MdLine>,
    /// Where the popup points, in physical pixels: the character the
    /// pointer was over, or the caret when it was asked for by name.
    pub at: (f32, f32),
}

impl Hover {
    /// How wide the text is laid out, and so how wide the popup is.
    pub const COLUMNS: usize = 64;
    /// Past this it is a document, not a tooltip, and scrolling a thing
    /// that vanishes when the mouse moves is not a way to read it.
    pub const ROWS: usize = 14;

    pub fn rows(&self) -> usize {
        preview_rows(&self.lines).min(Self::ROWS)
    }
}

/// Where the hover popup sits, clamped inside the window.
///
/// Its own function for the same reason the context menu's is: the popup
/// is placed relative to a point that can be anywhere, and a placement
/// that is only in the drawing code cannot be checked.
pub fn hover_bounds(l: &Layout, hover: &Hover) -> (f32, f32, f32, f32) {
    let width = (Hover::COLUMNS as f32 + 2.0 * HOVER_PAD) * l.cell.advance;
    let height = (hover.rows() as f32 + 2.0 * HOVER_PAD) * l.cell.line_height;

    // Above the character rather than below: a tooltip under the pointer
    // is a tooltip the pointer is about to cover.
    let x = hover.at.0.min(l.width - width).max(0.0);
    let above = hover.at.1 - height;
    let y = if above >= l.text_y {
        above
    } else {
        hover.at.1 + l.cell.line_height
    };
    // Clamped after the choice, not inside it: the anchor is a pointer
    // position, and a pointer can be outside the window — or have been,
    // when the window was last a different size.
    let y = y.clamp(0.0, (l.height - height).max(0.0));
    (x, y, width, height)
}

/// Cells of air between the popup's edge and its text.
const HOVER_PAD: f32 = 1.0;

fn draw_hover(f: &mut lithe_gpu::Frame<'_>, l: &Layout, hover: &Hover, theme: &Theme) {
    if hover.lines.is_empty() {
        return;
    }
    const PAD: f32 = HOVER_PAD;
    let (x, y, width, height) = hover_bounds(l, hover);

    f.rect(x, y, width, height, theme.panel_bg);
    f.rect(x, y, 2.0, height, theme.cursor);

    let left = x + PAD * l.cell.advance;
    let mut row_y = y + PAD * l.cell.line_height;
    let mut used = 0usize;
    for line in &hover.lines {
        let rows = line.rows();
        if used + rows > Hover::ROWS {
            break;
        }
        used += rows;
        let height = rows as f32 * l.cell.line_height;
        let baseline = row_y + height - (l.cell.line_height - l.cell.ascent);
        if line.block == MdBlock::Code {
            f.rect(
                left,
                row_y,
                width - 2.0 * PAD * l.cell.advance,
                height,
                theme.panel_active,
            );
        }
        let scale = match line.scale {
            MdScale::Body => lithe_gpu::Size::Body,
            MdScale::Large => lithe_gpu::Size::Large,
            MdScale::Huge => lithe_gpu::Size::Huge,
        };
        let mut pen = left + line.indent as f32 * l.cell.advance;
        for span in &line.spans {
            let (colour, face, mut family) = span_style(span.style, theme);
            if line.is_grid() {
                family = lithe_gpu::Family::Mono;
            }
            pen = f.text_run(pen, baseline, &span.text, colour, face, scale, family);
        }
        row_y += height;
    }
}

/// Colour and weight for a rendered-Markdown run.
///
/// Shared by the preview and the hover popup, which show the same thing
/// from the same reader and should not disagree about what bold looks
/// like. Structure is carried by weight, colour and — for the top two
/// heading levels — size; everything else is one size, because a uniform
/// grid is what makes the rest of the editor cheap.
/// What a run of the preview is drawn in: a colour, and the face and
/// family the layout already measured it with.
///
/// Weight and slant carry what they carry in a document, so colour is
/// free to do less of the work: emphasis is italic rather than violet,
/// and bold is the one that is brighter. Only the things that are not
/// prose — code, a link, a table's frame — keep a colour of their own.
fn span_style(style: MdStyle, theme: &Theme) -> (Rgba, lithe_gpu::Face, lithe_gpu::Family) {
    let colour = match style {
        MdStyle::Heading(1 | 2) => theme.fg,
        MdStyle::Heading(_) => theme.fg,
        MdStyle::Bold | MdStyle::BoldEmphasis => theme.fg,
        MdStyle::Emphasis => theme.fg,
        MdStyle::Code => theme.syntax.string,
        MdStyle::Quote => theme.status_fg,
        MdStyle::Link => theme.cursor,
        // Quieter than the text it introduces: a marker is there to
        // be counted, not read.
        MdStyle::Bullet => theme.gutter_fg,
        MdStyle::Strike => theme.status_fg,
        MdStyle::Syntax(kind) => theme.colour(Some(kind)),
        // A shade quieter than the editor's own text: a page of prose at
        // full contrast is a page nobody finishes.
        MdStyle::Body => theme.preview_fg,
    };
    (colour, style.face(), style.family())
}

/// A [`markdown::Measure`] over the frame being drawn, so the column the
/// preview is laid out in and the column it is drawn in are one number.
struct Measured<'a, 'f> {
    frame: &'a lithe_gpu::Frame<'f>,
}

impl crate::markdown::Measure for Measured<'_, '_> {
    fn aspect(&mut self) -> f32 {
        let cell = self.frame.cell();
        match cell.advance > 0.0 {
            true => cell.line_height / cell.advance,
            false => 2.0,
        }
    }

    fn width(&mut self, text: &str, style: MdStyle, scale: MdScale) -> f32 {
        let cell = self.frame.cell().advance;
        if cell <= 0.0 {
            return text.chars().count() as f32;
        }
        self.frame
            .measure(text, style.face(), style.size(scale), style.family())
            / cell
    }
}

/// Where the preview's page sits in the window.
///
/// One place decides it, because the text was wrapped to these numbers
/// and a click has to land on the glyph that was drawn.
struct Page {
    /// Left edge of the text column, and its width in cells.
    left: f32,
    columns: f32,
    /// Left edge of the page itself, which is where a figure too wide for
    /// the column starts.
    margin: f32,
}

impl Page {
    /// Where a line's cells begin.
    ///
    /// A drawn diagram may be wider than the measure prose is set to; it
    /// starts at the margin rather than hanging off the right of the
    /// column.
    fn origin(&self, line: &MdLine) -> f32 {
        if line.is_grid() && line_cells(line) > self.columns {
            self.margin
        } else {
            self.left
        }
    }
}

/// The document's column, measured exactly as the layout measured it, and
/// centred in what is left of the window.
///
/// A page of prose with its margins is what the eye reads as a document;
/// the same words pressed into the corner of a window are a terminal.
fn preview_page(l: &Layout, measure: &mut dyn crate::markdown::Measure) -> Page {
    let room = preview_columns(l);
    let columns = crate::markdown::column(room, measure);
    // Centred in the window rather than in the text area: the gutter the
    // editor keeps for line numbers means nothing to a document, and
    // measuring from it put the page off to one side.
    let available = l.width - l.tree_w - SCROLLBAR_W;
    let gutter = ((available - columns * l.cell.advance) / 2.0).max(2.0 * l.cell.advance);
    Page {
        left: l.tree_w + gutter,
        columns,
        margin: l.tree_w + crate::markdown::margin(room) * l.cell.advance,
    }
}

/// How many cells of grid content a line holds, its indent included.
fn line_cells(line: &MdLine) -> f32 {
    let text: usize = line.spans.iter().map(|s| s.text.chars().count()).sum();
    (line.indent + text) as f32
}

/// Which link, if any, is under a point in the preview.
///
/// Measured rather than counted, and measured with the same page and the
/// same advances the drawing uses: a proportional line has no column to
/// divide by, so the only honest answer walks the runs.
pub fn preview_link_at(
    l: &Layout,
    lines: &[MdLine],
    scroll: f32,
    x: f32,
    y: f32,
    measure: &mut dyn crate::markdown::Measure,
) -> Option<std::sync::Arc<str>> {
    if x < l.tree_w || y < l.text_y {
        return None;
    }
    let page = preview_page(l, measure);
    let top = scroll.max(0.0) as usize;
    let slip = (scroll.max(0.0).fract() * l.cell.line_height).round();
    let mut row_y = l.text_y + l.cell.line_height - slip;
    for line in lines.iter().skip(top) {
        if row_y > l.text_y + l.text_h {
            break;
        }
        let height = line.rows() as f32 * l.cell.line_height;
        if y >= row_y && y < row_y + height {
            let mut at = page.origin(line) + line.indent as f32 * l.cell.advance;
            for span in &line.spans {
                let width = run_cells(line, span, measure) * l.cell.advance;
                if x >= at && x < at + width {
                    return span.href.clone();
                }
                at += width;
            }
            return None;
        }
        row_y += height;
    }
    None
}

/// A place in the preview: which laid-out line, and how many
/// characters into it.
///
/// Characters of [`MdLine::text`] rather than of a span, because a
/// selection runs across the runs a line is broken into and does not
/// care where one style ends.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct PreviewPos {
    pub line: usize,
    pub char: usize,
}

/// Where in the preview a point lands.
///
/// Clamped rather than optional past the edges: a drag that runs off
/// the bottom of the column means "to the end", which is what every
/// other text selection does and what makes dragging feel like
/// dragging. `None` only when the pointer is outside the preview
/// altogether, or there is no preview.
pub fn preview_char_at(
    l: &Layout,
    lines: &[MdLine],
    scroll: f32,
    x: f32,
    y: f32,
    measure: &mut dyn crate::markdown::Measure,
) -> Option<PreviewPos> {
    if lines.is_empty() || x < l.tree_w {
        return None;
    }
    let end = |line: usize| PreviewPos {
        line,
        char: lines[line].len_chars(),
    };
    if y < l.text_y {
        return Some(PreviewPos {
            line: scroll.max(0.0) as usize,
            char: 0,
        });
    }

    let page = preview_page(l, measure);
    let top = (scroll.max(0.0) as usize).min(lines.len() - 1);
    let slip = (scroll.max(0.0).fract() * l.cell.line_height).round();
    let mut row_y = l.text_y + l.cell.line_height - slip;
    let mut last = top;
    for (offset, line) in lines.iter().enumerate().skip(top) {
        let height = line.rows() as f32 * l.cell.line_height;
        if y >= row_y && y < row_y + height {
            return Some(PreviewPos {
                line: offset,
                char: preview_column(l, line, &page, x, measure),
            });
        }
        last = offset;
        row_y += height;
        if row_y > l.text_y + l.text_h {
            break;
        }
    }
    // Below the last row drawn: the end of it.
    Some(end(last))
}

/// How many characters into a line a point is, horizontally.
///
/// Walks the runs and then the characters of the one it lands in,
/// because a proportional face has no column to divide by. Both walks
/// are over one line of one visible row, so the cost is bounded by the
/// window rather than by the document.
fn preview_column(
    l: &Layout,
    line: &MdLine,
    page: &Page,
    x: f32,
    measure: &mut dyn crate::markdown::Measure,
) -> usize {
    let indent = line.indent;
    let start = page.origin(line);
    if x <= start {
        return 0;
    }
    let mut at = start + indent as f32 * l.cell.advance;
    if x < at {
        // Inside the indent: the nearest edge of it.
        let into = ((x - start) / l.cell.advance).round() as usize;
        return into.min(indent);
    }
    let mut passed = indent;
    for span in &line.spans {
        let width = run_cells(line, span, measure) * l.cell.advance;
        if x < at + width {
            // Inside this run: the character boundary nearest `x`.
            let chars: Vec<char> = span.text.chars().collect();
            let mut best = (f32::MAX, 0usize);
            for taken in 0..=chars.len() {
                let prefix: String = chars[..taken].iter().collect();
                let edge = at
                    + run_cells(
                        line,
                        &crate::markdown::Span::plain(prefix, span.style),
                        measure,
                    ) * l.cell.advance;
                let gap = (edge - x).abs();
                if gap < best.0 {
                    best = (gap, taken);
                }
            }
            return passed + best.1;
        }
        at += width;
        passed += span.text.chars().count();
    }
    passed
}

/// Which characters of `line` a selection covers, if any.
///
/// The ends are whole lines except where the selection starts or stops,
/// which is what makes a multi-line drag look like one block rather
/// than three ragged ones.
fn selected_chars(
    line: &MdLine,
    index: usize,
    start: PreviewPos,
    end: PreviewPos,
) -> Option<(usize, usize)> {
    if index < start.line || index > end.line {
        return None;
    }
    let chars = line.len_chars();
    let from = if index == start.line { start.char } else { 0 };
    let to = if index == end.line { end.char } else { chars };
    let (from, to) = (from.min(chars), to.min(chars));
    // A line wholly inside the selection is covered even when it is
    // empty; one that only touches it at a point is not selected.
    (from < to || index > start.line).then_some((from, to.max(from)))
}

/// Where a character boundary of a preview line sits horizontally.
fn preview_x_at(
    l: &Layout,
    line: &MdLine,
    page: &Page,
    index: usize,
    measure: &mut dyn crate::markdown::Measure,
) -> f32 {
    let origin = page.origin(line);
    if index <= line.indent {
        return origin + index as f32 * l.cell.advance;
    }
    let mut at = origin + line.indent as f32 * l.cell.advance;
    let mut passed = line.indent;
    for span in &line.spans {
        let count = span.text.chars().count();
        if index <= passed + count {
            let taken: String = span.text.chars().take(index - passed).collect();
            let run = crate::markdown::Span::plain(taken, span.style);
            return at + run_cells(line, &run, measure) * l.cell.advance;
        }
        at += run_cells(line, span, measure) * l.cell.advance;
        passed += count;
    }
    at
}

/// How wide one run is, in cells.
///
/// A line that is set on the grid is drawn in the grid's own face, one
/// cell per character, whatever the run's style would otherwise measure.
fn run_cells(
    line: &MdLine,
    span: &crate::markdown::Span,
    measure: &mut dyn crate::markdown::Measure,
) -> f32 {
    if line.is_grid() {
        span.text.chars().count() as f32 * line.scale.width()
    } else {
        measure.width(&span.text, span.style, line.scale)
    }
}

fn draw_preview(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    lines: &[MdLine],
    scene: &Scene<'_>,
    theme: &Theme,
) {
    let (scroll, sel) = (scene.preview_scroll, scene.preview_sel);
    use lithe_gpu::Size;

    let top = scroll.max(0.0) as usize;
    let page = {
        let mut measure = Measured { frame: f };
        preview_page(l, &mut measure)
    };
    let (left, columns) = (page.left, page.columns);
    let right = left + columns * l.cell.advance;
    let top_margin = l.cell.line_height;

    // Lines are no longer all one row tall, so the y of each is the sum of
    // the ones above it rather than its index times the line height. The
    // fractional part of the scroll still slips by one body row, which is
    // what a wheel notch means.
    let slip = (scroll.max(0.0).fract() * l.cell.line_height).round();
    let mut y = l.text_y + top_margin - slip;
    let bottom = l.text_y + l.text_h;

    // Ordered, because which end of a selection is the anchor depends
    // on which way it was dragged and the drawing does not care.
    let span = sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

    for (drawn, line) in lines.iter().skip(top).enumerate() {
        if y >= bottom {
            break;
        }
        let index = top + drawn;
        let height = line.rows() as f32 * l.cell.line_height;
        // A heading sits on the lower of its two rows, so the extra row
        // becomes air above it rather than a gap underneath.
        let baseline = y + height - (l.cell.line_height - l.cell.ascent);
        // Where this line starts, and how far whatever is drawn behind it
        // runs: the column, or the figure when the figure is wider.
        let origin = page.origin(line);
        let behind = (line_cells(line) * l.cell.advance).max(right - left);

        // Block decoration goes behind the text.
        match line.block {
            // A panel the width of the column, not of the window: a
            // tint that runs to both edges of the pane reads as a
            // change of mode rather than as a block in a document.
            // Rounded at the ends, which is the whole block's first
            // and last row.
            MdBlock::Code | MdBlock::Diagram => {
                let x0 = origin - l.cell.advance;
                let panel = (behind + 2.0 * l.cell.advance).min(right + l.cell.advance - x0);
                let radius = (l.cell.line_height * 0.3).round();
                let first = lines
                    .get((top + drawn).wrapping_sub(1))
                    .is_none_or(|above| above.block != line.block);
                let last = lines
                    .get(top + drawn + 1)
                    .is_none_or(|below| below.block != line.block);
                // Square where the block continues, so consecutive
                // rows join into one panel instead of a stack of
                // lozenges. Drawn as one rounded rect plus a square
                // one over the join, which is cheaper than a path.
                f.rounded(x0, y, panel, height, radius, theme.panel_bg);
                if !first {
                    f.rect(x0, y, panel, radius, theme.panel_bg);
                }
                if !last {
                    f.rect(x0, y + height - radius, panel, radius, theme.panel_bg);
                }
            }
            // At the column's own edge, where the text would start
            // if it were not quoted — a bar further out than that
            // reads as furniture rather than as part of the block.
            MdBlock::Quote => f.rounded(
                origin,
                y,
                (l.cell.advance * 0.3).max(2.0).round(),
                height,
                1.5,
                theme.panel_active,
            ),
            MdBlock::Rule => f.rect(
                left,
                y + height / 2.0,
                right - left,
                1.0,
                theme.panel_active,
            ),
            MdBlock::HeadingRule => f.rect(
                left,
                y + height / 3.0,
                right - left,
                1.0,
                theme.panel_active,
            ),
            // A table row draws its own rules, beside its own cells.
            MdBlock::Table | MdBlock::TableHead | MdBlock::Plain => {}
        }

        // Selected text, behind everything the line draws. Measured in
        // its own scope because the measurer borrows the frame, which
        // the rect that follows also needs.
        if let Some((from, to)) = span.and_then(|(a, b)| selected_chars(line, index, a, b)) {
            let (x0, x1) = {
                let mut measure = Measured { frame: f };
                (
                    preview_x_at(l, line, &page, from, &mut measure),
                    preview_x_at(l, line, &page, to, &mut measure),
                )
            };
            // A blank line inside a selection still reads as selected,
            // which a zero-width band would not.
            let width = (x1 - x0).max(l.cell.advance * 0.4);
            f.rect(x0, y, width, height, theme.selection);
        }

        // `copy` on the first row of a fenced block, so it is discoverable
        // that the block is clickable at all. Only the first row: a label
        // on every row would read as part of the code.
        let first_of_block = line.code.is_some()
            && lines
                .get((top + drawn).wrapping_sub(1))
                .is_none_or(|above| above.code.is_none());
        if first_of_block {
            const COPY: &str = "copy";
            f.text(
                origin + behind - COPY.len() as f32 * l.cell.advance,
                baseline,
                COPY,
                theme.gutter_fg,
            );
        }

        // A figure: the shapes as a picture, then its labels in the
        // editor's own face. The text is drawn here rather than baked
        // into the picture so that it matches the prose around it and
        // the rasteriser needs no fonts of its own.
        if let Some(figure) = line.figure.as_deref() {
            let fx = origin + line.indent as f32 * l.cell.advance;
            let key = (
                crate::figure_key(&figure.svg)
                    ^ crate::figure_key(&format!("{:?}", theme.preview_fg)),
                (figure.cols as f32 * l.cell.advance).round().max(1.0) as u32,
            );
            if let Some(picture) = scene.figures.get(&key) {
                f.image(
                    &picture.image,
                    fx,
                    y,
                    picture.width as f32,
                    picture.height as f32,
                );
            }
            for label in &figure.labels {
                let colour = if label.faint {
                    theme.gutter_fg
                } else {
                    theme.fg
                };
                let width = f.measure(
                    &label.text,
                    lithe_gpu::Face::Regular,
                    Size::Body,
                    lithe_gpu::Family::Mono,
                );
                let lx = match label.centred {
                    true => fx + label.x * l.cell.advance - width / 2.0,
                    false => fx + label.x * l.cell.advance,
                };
                let ly = y + label.row * l.cell.line_height + l.cell.ascent;
                f.text_run(
                    lx,
                    ly,
                    &label.text,
                    colour,
                    lithe_gpu::Face::Regular,
                    Size::Body,
                    lithe_gpu::Family::Mono,
                );
            }
            y += height;
            continue;
        }

        // A table row: each cell placed in its own column, with a
        // hairline under the heading and between the rows. No outer
        // frame and no vertical rules — the gap between columns is
        // enough to keep them apart, and a box round a table is what
        // made this read as a terminal.
        if let Some(row) = &line.row {
            let left = origin + line.indent as f32 * l.cell.advance;
            for cell in &row.cells {
                let width: f32 = cell
                    .spans
                    .iter()
                    .map(|s| {
                        let (_, face, family) = span_style(s.style, theme);
                        f.measure(&s.text, face, s.style.size(line.scale), family)
                    })
                    .sum();
                let room = cell.width * l.cell.advance;
                let slack = (room - width).max(0.0);
                let inset = match cell.align {
                    MdAlign::Left => 0.0,
                    MdAlign::Centre => slack / 2.0,
                    MdAlign::Right => slack,
                };
                let mut cx = left + cell.x * l.cell.advance + inset;
                for span in &cell.spans {
                    let (colour, face, family) = span_style(span.style, theme);
                    cx = f.text_run(
                        cx,
                        baseline,
                        &span.text,
                        colour,
                        face,
                        span.style.size(line.scale),
                        family,
                    );
                }
            }
            if row.rule {
                // Under the heading it marks where the data starts;
                // between rows it is only there to help the eye track
                // across, so it is fainter.
                let colour = match row.heading {
                    true => theme.panel_active,
                    false => theme.gutter_bg,
                };
                f.rect(
                    left,
                    (y + height - 1.0).round(),
                    row.width * l.cell.advance,
                    1.0,
                    colour,
                );
            }
            y += height;
            continue;
        }

        let mut x = origin + line.indent as f32 * l.cell.advance;
        for span in &line.spans {
            let (colour, face, mut family) = span_style(span.style, theme);
            // A table's cells and a diagram's characters are drawn where
            // the layout counted cells for them.
            if line.is_grid() {
                family = lithe_gpu::Family::Mono;
            }
            let run = span.style.size(line.scale);
            // Inline code sits on a tinted chip. Colour alone left a
            // word of code looking like an error in the sentence
            // rather than a thing being named.
            if span.style == MdStyle::Code && !line.is_grid() && !span.text.trim().is_empty() {
                let lead = span.text.len() - span.text.trim_start().len();
                let tail = span.text.len() - span.text.trim_end().len();
                let from = x + f.measure(&span.text[..lead], face, run, family);
                let to = x + f.measure(span.text.trim_end(), face, run, family);
                let pad = (l.cell.advance * 0.35).round();
                f.rounded(
                    from - pad,
                    y + (l.cell.line_height * 0.12).round(),
                    to - from + pad * 2.0,
                    (l.cell.line_height * 0.82).round(),
                    (l.cell.line_height * 0.18).round(),
                    theme.panel_active,
                );
                let _ = tail;
            }
            let next = f.text_run(x, baseline, &span.text, colour, face, run, family);
            // A line through or under the run, where colour alone is doing
            // too much work in a single-weight preview.
            let rule = match span.style {
                // Skip the leading space a wrap may have put on the span,
                // or the rule starts before the word does.
                MdStyle::Link => Some(l.cell.line_height - 3.0),
                MdStyle::Strike => Some(l.cell.ascent * 0.6),
                _ => None,
            };
            if let Some(offset) = rule {
                let lead = &span.text[..span.text.len() - span.text.trim_start().len()];
                let from = x + f.measure(lead, face, run, family);
                f.rect(from, y + offset, next - from, 1.0, colour);
            }
            x = next;
        }
        y += height;
    }

    // Scrollbar, same as the editor's.
    if let Some((top_y, height)) = scrollbar_extent(l, preview_rows(lines), scroll) {
        let x = l.width - SCROLLBAR_W;
        f.rect(x, l.text_y, SCROLLBAR_W, l.text_h, theme.scrollbar_track);
        f.rect(
            x + 2.0,
            top_y,
            SCROLLBAR_W - 4.0,
            height,
            theme.scrollbar_thumb,
        );
    }
}

/// How many grid rows a rendered preview occupies.
///
/// More than `lines.len()`, because a top-level heading is two rows tall.
/// The scrollbar and the scroll limit both want rows, not lines.
pub fn preview_rows(lines: &[MdLine]) -> usize {
    lines.iter().map(MdLine::rows).sum()
}

fn draw_prompt(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    prompt: &Prompt,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    let y = l.tab_h;
    f.rect(0.0, y, l.width, l.find_h, theme.panel_bg);
    f.rect(0.0, y, l.width, 1.0, theme.cursor);

    scratch.text.clear();
    let _ = write!(scratch.text, "{}  {}", prompt.label, prompt.text);
    f.text(l.cell.advance, y + l.cell.ascent, &scratch.text, theme.fg);
    f.rect(
        l.cell.advance * (scratch.text.chars().count() as f32 + 1.2),
        y,
        (l.cell.advance * 0.12).max(1.5),
        l.find_h,
        theme.cursor,
    );

    let hint = "enter commits · esc cancels";
    let hint_x = l.width - l.cell.advance * (hint.chars().count() as f32 + 1.0);
    f.text(hint_x, y + l.cell.ascent, hint, theme.gutter_fg);
}

fn draw_find_bar(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    find: &Find,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    let y = l.tab_h;
    f.rect(0.0, y, l.width, l.find_h, theme.panel_bg);
    f.rect(0.0, y, l.width, 1.0, theme.panel_active);
    let baseline = y + l.cell.ascent;

    // find: <query>     replace: <text>        [no matches]
    scratch.text.clear();
    let _ = write!(scratch.text, "find  {}", find.query);
    let query_cells = scratch.text.chars().count() as f32;
    f.text(l.cell.advance, baseline, &scratch.text, theme.fg);
    if find.focus == Field::Query {
        f.rect(
            l.cell.advance * (query_cells + 1.2),
            y,
            (l.cell.advance * 0.12).max(1.5),
            l.find_h,
            theme.cursor,
        );
    }

    if find.replacing {
        let x = l.cell.advance * (query_cells + 6.0);
        scratch.text.clear();
        let _ = write!(scratch.text, "replace  {}", find.replace);
        f.text(x, baseline, &scratch.text, theme.fg);
        if find.focus == Field::Replace {
            let cells = scratch.text.chars().count() as f32;
            f.rect(
                x + l.cell.advance * (cells + 0.2),
                y,
                (l.cell.advance * 0.12).max(1.5),
                l.find_h,
                theme.cursor,
            );
        }
    }

    scratch.text.clear();
    if find.missed {
        scratch.text.push_str("no matches");
    } else if find.replacing {
        scratch
            .text
            .push_str("tab switches field · enter replaces · cmd+enter all");
    } else {
        scratch
            .text
            .push_str("enter next · shift+enter previous · esc close");
    }
    let hint_x = l.width - l.cell.advance * (scratch.text.chars().count() as f32 + 1.0);
    f.text(
        hint_x,
        baseline,
        &scratch.text,
        if find.missed {
            theme.error
        } else {
            theme.gutter_fg
        },
    );
}

fn draw_project_search(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    search: &ProjectSearch,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    f.rect(0.0, 0.0, l.width, l.height, theme.scrim);

    let shown = search.hits.len().min(PICKER_ROWS);
    let panel_w = (l.width * 0.78).clamp(40.0 * l.cell.advance, l.width - 4.0 * l.cell.advance);
    let panel_x = ((l.width - panel_w) / 2.0).round();
    let panel_y = (l.height * 0.1).round();
    let panel_h = l.cell.line_height * (shown as f32 + 2.0);

    f.rect(panel_x, panel_y, panel_w, panel_h, theme.panel_bg);
    f.rect(panel_x, panel_y, panel_w, 2.0, theme.cursor);

    scratch.text.clear();
    let _ = write!(scratch.text, "search  {}", search.query);
    f.text(
        panel_x + l.cell.advance,
        panel_y + l.cell.ascent,
        &scratch.text,
        theme.fg,
    );
    f.rect(
        // +1 for the cell the text itself starts at; without it the
        // caret trails the last character typed.
        panel_x + l.cell.advance * (scratch.text.chars().count() as f32 + 1.2),
        panel_y,
        (l.cell.advance * 0.12).max(1.5),
        l.cell.line_height,
        theme.cursor,
    );

    scratch.text.clear();
    if search.running {
        let _ = write!(scratch.text, "{}…", search.hits.len());
    } else if search.truncated {
        let _ = write!(scratch.text, "{}+", search.hits.len());
    } else {
        let _ = write!(scratch.text, "{}", search.hits.len());
    }
    let count_x = panel_x + panel_w - l.cell.advance * (scratch.text.chars().count() as f32 + 1.0);
    f.text(
        count_x,
        panel_y + l.cell.ascent,
        &scratch.text,
        theme.gutter_fg,
    );

    // Scroll so the selection stays visible.
    let first = search.selected.saturating_sub(PICKER_ROWS - 1);
    let columns = (panel_w / l.cell.advance) as usize;
    for row in 0..shown {
        let index = first + row;
        let Some(hit) = search.hits.get(index) else {
            break;
        };
        let y = panel_y + l.cell.line_height * (row as f32 + 1.0);
        let selected = index == search.selected;
        if selected {
            f.rect(panel_x, y, panel_w, l.cell.line_height, theme.panel_active);
        }

        // "path:line" in one colour, the matching line in another, so the
        // eye can separate location from content.
        scratch.text.clear();
        let name = hit
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = write!(scratch.text, "{name}:{}", hit.line);
        let location_cells = scratch.text.chars().count();
        f.text(
            panel_x + l.cell.advance * 1.5,
            y + l.cell.ascent,
            &scratch.text,
            theme.syntax.function,
        );

        let text_x = panel_x + l.cell.advance * (location_cells as f32 + 3.0);
        let room = columns.saturating_sub(location_cells + 5);
        scratch.text.clear();
        let trimmed = hit.text.trim_start();
        truncate_tail(&mut scratch.text, trimmed, room);
        f.text(
            text_x,
            y + l.cell.ascent,
            &scratch.text,
            if selected { theme.fg } else { theme.tree_file },
        );
    }

    if search.hits.is_empty() {
        f.text(
            panel_x + l.cell.advance * 1.5,
            panel_y + l.cell.line_height + l.cell.ascent,
            if search.running {
                "searching…"
            } else {
                "no matches"
            },
            theme.gutter_fg,
        );
    }
}

/// Copies a person's name, cut to fit at a word boundary where there is
/// one.
///
/// "Ahmad Masud" in eight columns is "Ahmad", not "Ahmad M…": a given name
/// is a name, where half a surname is a typo. Only when even that will not
/// fit does it fall back to cutting mid-word.
fn shorten_name(out: &mut String, name: &str, room: usize) {
    if name.chars().count() <= room {
        out.push_str(name);
        return;
    }
    let first = name.split_whitespace().next().unwrap_or(name);
    if !first.is_empty() && first.chars().count() <= room {
        out.push_str(first);
        return;
    }
    truncate_tail(out, name, room);
}

/// Copies `text`, cutting the end off when it will not fit. The opposite of
/// `truncate_into`: a matching line is most informative at its start.
fn truncate_tail(out: &mut String, text: &str, room: usize) {
    if text.chars().count() <= room {
        out.push_str(text);
        return;
    }
    out.extend(text.chars().take(room.saturating_sub(1)));
    out.push('…');
}

fn draw_landing(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    scene: &Scene<'_>,
    scratch: &mut Scratch,
) {
    let theme = scene.theme;
    let rows = landing_rows(scene.recents);
    let (ox, oy) = landing_origin(l, rows.len());
    let width = LANDING_COLS * l.cell.advance;
    let indent = 2.0 * l.cell.advance;

    for (index, entry) in rows.iter().enumerate() {
        let y = oy + index as f32 * l.cell.line_height;
        let baseline = y + l.cell.ascent;
        if let Some(action) = entry.action {
            tint(
                f,
                scene,
                Hit::Landing(action),
                ox,
                y,
                width,
                l.cell.line_height,
            );
        }

        match &entry.row {
            Row::Blank => {}
            Row::Title => {
                f.text(ox, baseline, "lithe", theme.fg);
            }
            Row::Subtitle => {
                scratch.text.clear();
                let _ = write!(
                    scratch.text,
                    "a local editor  ·  v{}",
                    env!("CARGO_PKG_VERSION")
                );
                f.text(ox, baseline, &scratch.text, theme.gutter_fg);
            }
            Row::Heading(text) => {
                f.text(ox, baseline, text, theme.status_fg);
            }
            Row::Action { label, hint } => {
                f.text(ox + indent, baseline, label, theme.cursor);
                // Shortcut hint, right-aligned in the column.
                let hint_x = ox + width - hint.chars().count() as f32 * l.cell.advance;
                f.text(hint_x, baseline, hint, theme.gutter_fg);
            }
            Row::Recent(recent) => {
                let Some(path) = scene.recents.get(*recent) else {
                    continue;
                };
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                f.text(ox + indent, baseline, &name, theme.cursor);

                // The parent directory, right-aligned and elided from the
                // left: the tail of a path is what identifies it.
                let parent = path.parent().map(shorten_home).unwrap_or_default();
                let room = (LANDING_COLS - 2.0 - name.chars().count() as f32 - 2.0).max(4.0);
                scratch.text.clear();
                truncate_into(&mut scratch.text, &parent, room as usize);
                let detail_x = ox + width - scratch.text.chars().count() as f32 * l.cell.advance;
                f.text(detail_x, baseline, &scratch.text, theme.gutter_fg);
            }
            Row::Footer(text) => {
                f.text(ox, baseline, text, theme.gutter_fg);
            }
        }
    }
}

/// Replaces the home directory with `~`, the way every shell shows it.
fn shorten_home(path: &Path) -> String {
    let text = path.display().to_string();
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => text,
        },
        None => text,
    }
}

fn draw_editor(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    editor: &Editor,
    scene: &Scene<'_>,
    scratch: &mut Scratch,
) {
    let theme = scene.theme;
    let tab_width = scene.tab_width;
    let doc = &editor.doc;
    let total_lines = doc.len_lines();
    let top = editor.top_line();
    // How far the top line is clipped by, which every row shifts up by.
    let slip = editor.pixel_offset(l.cell.line_height);
    // Last line with any pixels on screen. Fractional scrolling means the
    // bottom row is usually partial, so this is inclusive — and every loop
    // below uses it. They did not always agree, and the row that only the
    // text loop reached drew characters with no caret under them.
    let last_visible = top + l.rows;
    // Reserve the scrollbar's column so a long line does not run under it.
    let text_right = l.width - SCROLLBAR_W;
    let max_cols = (((text_right - l.text_x) / l.cell.advance).ceil() as usize).max(1);

    // Horizontal scroll. Everything in the text area is drawn `scroll_x`
    // columns to the left, and what that pushes into the gutter is cut off
    // there rather than drawn over it.
    let scroll_x = editor.scroll_x.max(0.0);
    let first_col = scroll_x as usize;
    let last_col = first_col + max_cols;
    let col_x = |col: f32| l.text_x + (col - scroll_x) * l.cell.advance;
    // A rect between two columns, clipped to the text area — `None` when
    // none of it is in there.
    let band = |from: f32, to: f32| {
        let x0 = col_x(from).max(l.text_x);
        let x1 = col_x(to).min(text_right);
        (x1 > x0).then_some((x0, x1 - x0))
    };

    let cursor_lines: Vec<usize> = editor
        .sel
        .cursors()
        .iter()
        .map(|c| doc.char_to_line(c.head))
        .collect();

    let row_y = |line: usize| l.text_y + (line - top) as f32 * l.cell.line_height - slip;

    // Current-line highlight, behind everything else.
    for &line in &cursor_lines {
        if line >= top && line <= last_visible {
            f.rect(
                l.text_x,
                row_y(line),
                l.width - l.text_x,
                l.cell.line_height,
                theme.cursor_line,
            );
        }
    }

    // Selections, as rects behind the text.
    for cursor in editor.sel.cursors() {
        if cursor.is_empty() {
            continue;
        }
        let (start, end) = (cursor.start(), cursor.end());
        let first = doc.char_to_line(start).max(top);
        let last = doc.char_to_line(end).min(last_visible);

        for line in first..=last {
            let Some(slice) = doc.line(line) else {
                continue;
            };
            let line_start = doc.line_to_char(line);
            let line_chars = doc.line_len_chars(line);

            let from = start.saturating_sub(line_start).min(line_chars);
            let spans_past_end = end >= line_start + line_chars;
            let to = if spans_past_end {
                line_chars + 1
            } else {
                end - line_start
            };
            if to <= from && !spans_past_end {
                continue;
            }

            let x0 = visual_col(slice, from, tab_width) as f32;
            let x1 = visual_col(slice, to.min(line_chars), tab_width) as f32
                + if to > line_chars { 1.0 } else { 0.0 };
            if let Some((x, width)) = band(x0, x1.max(x0)) {
                f.rect(x, row_y(line), width, l.cell.line_height, theme.selection);
            }
        }
    }

    // The identifier the command key has made clickable. An underline
    // rather than a colour, because it has to read as "this is a link
    // now" over whatever the grammar already painted the word.
    if let Some(span) = scene.cmd_link {
        let line = doc.char_to_line(span.start);
        if line >= top && line <= last_visible {
            if let Some(slice) = doc.line(line) {
                let line_start = doc.line_to_char(line);
                let from = span.start.saturating_sub(line_start);
                let to = (span.end - line_start).min(doc.line_len_chars(line));
                let x0 = visual_col(slice, from, tab_width) as f32;
                let x1 = visual_col(slice, to, tab_width) as f32;
                if let Some((x, width)) = band(x0, x1.max(x0 + 1.0)) {
                    let thickness = (l.cell.line_height * 0.06).max(1.0).round();
                    let y = (row_y(line) + l.cell.ascent + thickness).round();
                    f.rect(x, y, width, thickness, theme.fg);
                }
            }
        }
    }

    // Search matches, behind the text and above the selection, so a
    // selected match still reads as both.
    for span in scene.find_matches {
        let line = doc.char_to_line(span.start);
        if line < top || line > last_visible {
            continue;
        }
        let Some(slice) = doc.line(line) else {
            continue;
        };
        let line_start = doc.line_to_char(line);
        let from = span.start - line_start;
        let to = (span.end - line_start).min(doc.line_len_chars(line));
        let x0 = visual_col(slice, from, tab_width) as f32;
        let x1 = visual_col(slice, to, tab_width) as f32;
        if let Some((x, width)) = band(x0, (x1 - x0).max(1.0) + x0) {
            f.rect(x, row_y(line), width, l.cell.line_height, theme.match_bg);
        }
    }

    f.rect(l.tree_w, l.text_y, l.gutter_w, l.text_h, theme.gutter_bg);

    // Text, bounded by the viewport.
    for row in 0..=l.rows {
        let line = top + row;
        if line >= total_lines {
            break;
        }
        let Some(slice) = doc.line(line) else { break };
        // Through `row_y`, like every decoration. Computing the text's
        // own y separately left it the one thing that did not subtract
        // the fractional scroll, so at any scroll position that was not
        // a whole number of rows the caret, the current-line highlight
        // and the selection all sat up to a row away from the text they
        // belonged to.
        let baseline = row_y(line) + l.cell.ascent;

        scratch.text.clear();
        let _ = write!(scratch.text, "{}", line + 1);
        let number_x = l.text_x - l.cell.advance * (1 + scratch.text.chars().count()) as f32;
        f.text(
            number_x,
            baseline,
            &scratch.text,
            if cursor_lines.contains(&line) {
                theme.gutter_fg_active
            } else {
                theme.gutter_fg
            },
        );

        // Highlight kind per character of this line. Spans are byte
        // ranges in paint order, so filling them in sequence lets the
        // narrower, more specific capture win.
        let line_chars = doc.line_len_chars(line);
        scratch.styles.clear();
        scratch.styles.resize(line_chars, None);
        if !scene.highlights.is_empty() && line_chars > 0 {
            let rope = doc.rope();
            let line_start_char = doc.line_to_char(line);
            let line_start_byte = rope.char_to_byte(line_start_char);
            let line_end_byte = rope.char_to_byte(line_start_char + line_chars);

            for span in scene.highlights {
                // Spans are sorted by start, so once one begins past this
                // line, every later one does too.
                if span.start >= line_end_byte {
                    break;
                }
                if span.end <= line_start_byte {
                    continue;
                }
                let from = rope.byte_to_char(span.start.max(line_start_byte)) - line_start_char;
                let to = rope.byte_to_char(span.end.min(line_end_byte)) - line_start_char;
                for style in scratch.styles[from.min(line_chars)..to.min(line_chars)].iter_mut() {
                    *style = Some(span.kind);
                }
            }
        }

        // Emit one run of text per colour change, so a line costs a handful
        // of draws rather than one per character.
        //
        // Columns are counted from the start of the line even while the
        // view is scrolled sideways, because a tab's width depends on
        // where it sits — but only the columns inside the viewport are
        // ever pushed.
        scratch.text.clear();
        let mut col = 0usize;
        let mut run_col = first_col;
        let mut run_kind: Option<Kind> = None;

        for (index, ch) in slice.chars().enumerate() {
            if col >= last_col || ch == '\n' || ch == '\r' {
                break;
            }
            let width = if ch == '\t' {
                tab_width - (col % tab_width)
            } else {
                1
            };
            // Entirely to the left of the viewport: count it and move on.
            if col + width <= first_col {
                col += width;
                continue;
            }
            let kind = scratch.styles.get(index).copied().flatten();
            if kind != run_kind && !scratch.text.is_empty() {
                f.text(
                    col_x(run_col as f32),
                    baseline,
                    &scratch.text,
                    theme.colour(run_kind),
                );
                scratch.text.clear();
                run_col = col.max(first_col);
            }
            if scratch.text.is_empty() {
                run_col = col.max(first_col);
            }
            run_kind = kind;

            if ch == '\t' {
                // Only the part of the tab that is on screen; one
                // straddling the left edge is cut short by it.
                let visible = (col + width).min(last_col) - col.max(first_col);
                for _ in 0..visible {
                    scratch.text.push(' ');
                }
            } else {
                scratch.text.push(ch);
            }
            col += width;
        }
        if !scratch.text.is_empty() {
            f.text(
                col_x(run_col as f32),
                baseline,
                &scratch.text,
                theme.colour(run_kind),
            );
        }
    }

    // Line blame, trailing the cursor's line.
    //
    // At the end of the text rather than in a column of its own: a fixed
    // column would either waste width on short files or clip on long
    // lines, and the thing you are reading is the line, not the margin.
    if let Some(note) = scene.blame {
        if let Some(editor) = scene.workspace.active() {
            let line = editor.doc.char_to_line(editor.sel.primary().head);
            if line >= top && line <= last_visible && !note.is_empty() {
                let end = doc
                    .line(line)
                    .map(|slice| visual_col(slice, doc.line_len_chars(line), tab_width))
                    .unwrap_or(0);
                // A gap wide enough that it reads as an annotation and
                // not as more of the line.
                let col = end + 4;
                let room = last_col.saturating_sub(col.max(first_col));
                if room >= 8 {
                    scratch.text.clear();
                    truncate_tail(&mut scratch.text, note, room);
                    f.text(
                        col_x(col.max(first_col) as f32),
                        row_y(line) + l.cell.ascent,
                        &scratch.text,
                        theme.gutter_fg,
                    );
                }
            }
        }
    }

    // Version control, at the very left of the gutter. Drawn before
    // diagnostics so a diagnostic bar sits beside it rather than under it.
    for hunk in scene.hunks {
        let colour = match hunk.change {
            lithe_vcs::Change::Added => theme.vcs_added,
            lithe_vcs::Change::Modified => theme.vcs_modified,
            lithe_vcs::Change::Removed => theme.vcs_removed,
        };
        if hunk.change == lithe_vcs::Change::Removed {
            // Nothing was left behind to mark, so show a stub at the
            // boundary the deleted lines collapsed to.
            let line = hunk.line.max(top);
            if line <= last_visible {
                f.rect(
                    l.tree_w,
                    row_y(line) + l.cell.line_height - 2.0,
                    l.cell.advance * 0.5,
                    3.0,
                    colour,
                );
            }
            continue;
        }
        for offset in 0..hunk.lines {
            let line = hunk.line + offset;
            if line < top || line > last_visible {
                continue;
            }
            f.rect(l.tree_w, row_y(line), 3.0, l.cell.line_height, colour);
        }
    }

    // Diagnostics: a bar in the gutter for the line, and a rule under the
    // offending span. A rule rather than a squiggle because at one device
    // pixel a squiggle is mud, and the colour already carries the meaning.
    for mark in scene.diagnostics {
        let line = doc.char_to_line(mark.start);
        if line < top || line > last_visible {
            continue;
        }
        let colour = match mark.severity {
            Severity::Error => theme.error,
            Severity::Warning => theme.diagnostic_warning,
            _ => theme.diagnostic_hint,
        };
        let y = row_y(line);
        // Offset past the version-control stripe.
        f.rect(l.tree_w + 4.0, y, 2.0, l.cell.line_height, colour);

        let Some(slice) = doc.line(line) else {
            continue;
        };
        let line_start = doc.line_to_char(line);
        let from = mark.start.saturating_sub(line_start);
        let to = (mark.end - line_start).min(doc.line_len_chars(line));
        let x0 = visual_col(slice, from, tab_width) as f32;
        let x1 = visual_col(slice, to.max(from + 1), tab_width) as f32;
        if let Some((x, width)) = band(x0, (x1 - x0).max(1.0) + x0) {
            f.rect(x, y + l.cell.line_height - 2.0, width, 2.0, colour);
        }
    }

    // Scrollbar. Only when there is something to scroll, so a short file
    // does not grow furniture it has no use for.
    if let Some((top_y, height)) = scrollbar_extent(l, total_lines, editor.scroll) {
        let x = l.width - SCROLLBAR_W;
        f.rect(x, l.text_y, SCROLLBAR_W, l.text_h, theme.scrollbar_track);
        f.rect(
            x + 2.0,
            top_y,
            SCROLLBAR_W - 4.0,
            height,
            theme.scrollbar_thumb,
        );
    }

    // The horizontal scrollbar, drawn over the last row rather than
    // taking a row of its own: it comes and goes with the lines in view,
    // and a window that lost a row whenever one long line scrolled past
    // would be worse than one the bar floats over.
    let widest = editor.widest_visible(l.rows, max_cols);
    if let Some((left, width)) = h_scrollbar_extent(l, widest, scroll_x) {
        let y = l.text_y + l.text_h - SCROLLBAR_W;
        let (track_x, track_w) = h_track(l);
        f.rect(track_x, y, track_w, SCROLLBAR_W, theme.scrollbar_track);
        f.rect(
            left + 2.0,
            y + 2.0,
            (width - 4.0).max(1.0),
            SCROLLBAR_W - 4.0,
            theme.scrollbar_thumb,
        );
    }

    // Cursors, on top. These blink while you are working and settle to
    // solid once the window is left alone; see `BLINK_FOR` in the app.
    let caret_w = (l.cell.advance * 0.12).max(1.5);
    for cursor in editor.sel.cursors() {
        let line = doc.char_to_line(cursor.head);
        if line < top || line > last_visible {
            continue;
        }
        let Some(slice) = doc.line(line) else {
            continue;
        };
        let col = cursor.head - doc.line_to_char(line);
        if !scene.caret_visible {
            continue;
        }
        let x = col_x(visual_col(slice, col, tab_width) as f32);
        // A caret scrolled out to the left would otherwise be drawn on
        // the gutter, where it reads as a caret on another line.
        if x < l.text_x || x > text_right {
            continue;
        }
        f.rect(x, row_y(line), caret_w, l.cell.line_height, theme.cursor);
    }
}

fn draw_picker(
    f: &mut lithe_gpu::Frame<'_>,
    l: &Layout,
    picker: &Picker,
    theme: &Theme,
    scratch: &mut Scratch,
) {
    // Dim what is behind, so the overlay reads as a layer rather than a
    // panel that happens to be on top.
    f.rect(0.0, 0.0, l.width, l.height, theme.scrim);

    let shown = (picker.matched() as usize).min(PICKER_ROWS);
    let panel_w = (l.width * 0.6).clamp(40.0 * l.cell.advance, l.width - 8.0 * l.cell.advance);
    let panel_x = ((l.width - panel_w) / 2.0).round();
    let panel_y = (l.height * 0.12).round();
    let panel_h = l.cell.line_height * (shown as f32 + 2.0);

    f.rect(panel_x, panel_y, panel_w, panel_h, theme.panel_bg);
    f.rect(panel_x, panel_y, panel_w, 2.0, theme.cursor);

    // Query line.
    scratch.text.clear();
    let _ = write!(scratch.text, "{}  {}", picker.kind.prompt(), picker.query);
    f.text(
        panel_x + l.cell.advance,
        panel_y + l.cell.ascent,
        &scratch.text,
        theme.fg,
    );
    // Caret at the end of the query.
    f.rect(
        // +1 for the cell the text itself starts at; without it the
        // caret trails the last character typed.
        panel_x + l.cell.advance * (scratch.text.chars().count() as f32 + 1.2),
        panel_y,
        (l.cell.advance * 0.12).max(1.5),
        l.cell.line_height,
        theme.cursor,
    );

    // Result count, right-aligned on the query line.
    scratch.text.clear();
    let _ = write!(scratch.text, "{}/{}", picker.matched(), picker.total());
    let count_x = panel_x + panel_w - l.cell.advance * (scratch.text.chars().count() as f32 + 1.0);
    f.text(
        count_x,
        panel_y + l.cell.ascent,
        &scratch.text,
        theme.gutter_fg,
    );

    // Results. Scrolled so the selection stays visible.
    let first = (picker.selected as usize).saturating_sub(PICKER_ROWS - 1);
    let room = ((panel_w / l.cell.advance) as usize).saturating_sub(3);
    for row in 0..shown {
        let index = (first + row) as u32;
        let Some(label) = picker.label(index) else {
            break;
        };
        let y = panel_y + l.cell.line_height * (row as f32 + 1.0);
        let selected = index == picker.selected;
        if selected {
            f.rect(panel_x, y, panel_w, l.cell.line_height, theme.panel_active);
        }
        scratch.text.clear();
        truncate_into(&mut scratch.text, label, room);
        f.text(
            panel_x + l.cell.advance * 1.5,
            y + l.cell.ascent,
            &scratch.text,
            if selected { theme.fg } else { theme.tree_file },
        );
    }
}

/// Copies `text` into `out`, eliding the middle when it will not fit. Paths
/// are most recognisable at their ends, so the tail is kept.
fn truncate_into(out: &mut String, text: &str, room: usize) {
    let len = text.chars().count();
    if len <= room || room < 4 {
        out.push_str(text);
        return;
    }
    let tail = room - 1;
    out.push('…');
    out.extend(text.chars().skip(len - tail));
}

fn decimal_width(mut n: usize) -> usize {
    let mut digits = 1;
    while n >= 10 {
        n /= 10;
        digits += 1;
    }
    digits
}

#[allow(dead_code)]
pub fn dim(colour: Rgba, alpha: f32) -> Rgba {
    colour.alpha(alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use lithe_core::{Cursor, Selection, TextBuffer};

    fn scene_for(editor: Editor, theme: &Theme) -> (Workspace, usize) {
        let mut ws = Workspace::empty(None);
        ws.push(editor);
        let _ = theme;
        (ws, 4)
    }

    /// A scene showing nothing but the workspace, for tests that care
    /// about one field of it.
    fn probe_scene<'a>(ws: &'a Workspace, theme: &'a Theme) -> Scene<'a> {
        Scene {
            blame: None,
            workspace: ws,
            find: None,
            search: None,
            find_matches: &[],
            completion: None,
            diagnostics: &[],
            hunks: &[],
            branch: None,
            lsp: None,
            lsp_busy: None,
            autosave: false,
            hovered: None,
            prompt: None,
            caret_visible: true,
            recents: &[],
            highlights: &[],
            diff_old: None,
            cmd_link: None,
            diff_old_spans: &[],
            diff_new_spans: &[],
            tree: None,
            git: None,
            sidebar_cols: TREE_COLS,
            preview: None,
            preview_scroll: 0.0,
            figures: &EMPTY_FIGURES,
            preview_sel: None,
            diff: None,
            diff_title: None,
            diff_scroll: 0.0,
            diff_side_by_side: false,
            diff_editable: false,
            hover: None,
            image: None,
            settings: None,
            context: None,
            picker: None,
            theme,
            status: None,
            tab_width: 4,
        }
    }

    fn render(editor: Editor) -> Option<(Vec<u8>, Layout, u32, Theme)> {
        const W: u32 = 640;
        const H: u32 = 240;
        let mut r = match Renderer::offscreen(W, H, 1.0, 16.0) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("skipping, no usable GPU: {e}");
                return None;
            }
        };
        let theme = Theme::default();
        let (ws, tab_width) = scene_for(editor, &theme);
        let scene = Scene {
            tab_width,
            ..probe_scene(&ws, &theme)
        };
        let mut scratch = Scratch::default();
        draw(&mut r, &scene, &mut scratch);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        Some((r.read_rgba().ok()?, l, W, Theme::default()))
    }

    fn probe_panels<'a>(ws: &'a Workspace) -> Panels<'a> {
        Panels {
            workspace: ws,
            tree: None,
            git: None,
            recents: &[],
            search_open: false,
            preview_open: false,
            settings_open: false,
            context: None,
            diff: None,
            diff_scroll: 0.0,
            diff_side_by_side: false,
            preview: None,
            preview_scroll: 0.0,
        }
    }

    fn probe_panels_with<'a>(ws: &'a Workspace, recents: &'a [&'a Path]) -> Panels<'a> {
        Panels {
            workspace: ws,
            recents,
            ..probe_panels(ws)
        }
    }

    fn is(px: &[u8], want: Rgba) -> bool {
        (0..3).all(|c| {
            let expected = (want.0[c] * 255.0).round() as i32;
            (px[c] as i32 - expected).abs() <= 1
        })
    }

    fn find(
        buf: &[u8],
        stride: u32,
        colour: Rgba,
        x: std::ops::Range<u32>,
        y: std::ops::Range<u32>,
    ) -> (usize, Option<(u32, u32)>) {
        let (mut count, mut lo, mut hi) = (0usize, u32::MAX, 0u32);
        for yy in y {
            for xx in x.clone() {
                let i = ((yy * stride + xx) * 4) as usize;
                if is(&buf[i..i + 4], colour) {
                    count += 1;
                    lo = lo.min(xx);
                    hi = hi.max(xx);
                }
            }
        }
        (count, (count > 0).then_some((lo, hi)))
    }

    #[test]
    fn gutter_width_tracks_line_count() {
        assert_eq!(decimal_width(0), 1);
        assert_eq!(decimal_width(9), 1);
        assert_eq!(decimal_width(10), 2);
        assert_eq!(decimal_width(1000), 4);
    }

    #[test]
    fn tabs_shift_visual_columns() {
        let doc = TextBuffer::from_text("\tx\n");
        let line = doc.line(0).unwrap();
        assert_eq!(visual_col(line, 0, 4), 0);
        assert_eq!(visual_col(line, 1, 4), 4, "one tab is a full stop");
        assert_eq!(visual_col(line, 2, 4), 5);
    }

    #[test]
    fn clicking_maps_back_through_tabs() {
        let doc = TextBuffer::from_text("\tx\n");
        let line = doc.line(0).unwrap();
        assert_eq!(char_col_at_visual(line, 0, 4, true), 0);
        assert_eq!(char_col_at_visual(line, 3, 4, true), 1);
        assert_eq!(char_col_at_visual(line, 99, 4, true), 2);
    }

    #[test]
    fn visual_and_char_columns_round_trip() {
        let doc = TextBuffer::from_text("a\tbc\td\n");
        let line = doc.line(0).unwrap();
        for char_col in 0..6 {
            let v = visual_col(line, char_col, 4);
            assert_eq!(
                char_col_at_visual(line, v, 4, true),
                char_col,
                "at {char_col}"
            );
        }
    }

    #[test]
    fn long_paths_keep_their_tail() {
        let mut out = String::new();
        truncate_into(&mut out, "crates/app/src/view.rs", 12);
        assert!(out.ends_with("src/view.rs"), "got {out:?}");
        assert_eq!(out.chars().count(), 12);

        let mut out = String::new();
        truncate_into(&mut out, "short.rs", 20);
        assert_eq!(out, "short.rs", "no eliding when it fits");
    }

    #[test]
    fn selection_is_drawn_over_exactly_the_selected_columns() {
        let mut editor = Editor::new(TextBuffer::from_text("abcdefghij\nklmno\n"), None, 4);
        editor.sel = Selection::from_cursor(Cursor { anchor: 2, head: 6 });

        let Some((buf, l, stride, theme)) = render(editor) else {
            return;
        };
        let y0 = l.text_y as u32;
        let row = y0..y0 + l.cell.line_height as u32;

        let (count, span) = find(&buf, stride, theme.selection, 0..stride, row);
        assert!(count > 0, "no selection highlight was drawn at all");
        let (lo, hi) = span.unwrap();
        let expect_lo = (l.text_x + 2.0 * l.cell.advance).round() as u32;
        let expect_hi = (l.text_x + 6.0 * l.cell.advance).round() as u32;
        assert!(
            lo.abs_diff(expect_lo) <= 2,
            "starts at {lo}, want ~{expect_lo}"
        );
        assert!(
            hi.abs_diff(expect_hi) <= 2,
            "ends at {hi}, want ~{expect_hi}"
        );
    }

    #[test]
    fn every_cursor_gets_a_caret() {
        let mut editor = Editor::new(TextBuffer::from_text("abcdef\nghijkl\n"), None, 4);
        editor.sel = Selection::from_cursors(vec![Cursor::at(2), Cursor::at(10)]);

        let Some((buf, l, stride, theme)) = render(editor) else {
            return;
        };
        let h = l.cell.line_height as u32;
        let y0 = l.text_y as u32;

        let (first, span0) = find(&buf, stride, theme.cursor, 0..stride, y0..y0 + h);
        let (second, span1) = find(&buf, stride, theme.cursor, 0..stride, y0 + h..y0 + h * 2);
        assert!(first > 0, "no caret on the first line");
        assert!(second > 0, "no caret on the second line");

        let want0 = (l.text_x + 2.0 * l.cell.advance).round() as u32;
        let want1 = (l.text_x + 3.0 * l.cell.advance).round() as u32;
        assert!(
            span0.unwrap().0.abs_diff(want0) <= 2,
            "first caret misplaced"
        );
        assert!(
            span1.unwrap().0.abs_diff(want1) <= 2,
            "second caret misplaced"
        );
    }

    #[test]
    fn the_landing_page_shows_only_with_no_project_and_no_buffer() {
        let empty = Workspace::empty(None);
        assert!(showing_landing(&empty));

        let with_project = Workspace::empty(Some(std::path::PathBuf::from("/p")));
        assert!(
            !showing_landing(&with_project),
            "an open project means the tree is the affordance, not the landing page"
        );

        let mut with_buffer = Workspace::empty(None);
        with_buffer.push(Editor::new(TextBuffer::new(), None, 4));
        assert!(!showing_landing(&with_buffer));
    }

    #[test]
    fn landing_rows_are_clickable_where_they_look_clickable() {
        let Ok(r) = Renderer::offscreen(1200, 800, 1.0, 16.0) else {
            eprintln!("skipping, no usable GPU");
            return;
        };
        let ws = Workspace::empty(None);
        let recents: Vec<&Path> = vec![Path::new("/projects/alpha"), Path::new("/projects/beta")];
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );

        let rows = landing_rows(&recents);
        let (ox, oy) = landing_origin(&l, rows.len());
        let x = ox + 3.0 * l.cell.advance;
        let row_y = |i: usize| oy + (i as f32 + 0.5) * l.cell.line_height;

        // Every row that declares an action must hit-test to that action,
        // and every row that does not must not be clickable. This is the
        // pairing that silently drifts when the two are written separately.
        for (index, entry) in rows.iter().enumerate() {
            let hit = hit_test(&l, &probe_panels_with(&ws, &recents), x, row_y(index));
            match entry.action {
                Some(expected) => assert!(
                    matches!(hit, Hit::Landing(got) if got == expected),
                    "row {index} should activate {expected:?}"
                ),
                None => assert!(
                    matches!(hit, Hit::Elsewhere),
                    "row {index} is a heading or gap and must not be clickable"
                ),
            }
        }

        // Both recents are reachable, by index.
        let recent_rows: Vec<_> = rows
            .iter()
            .filter_map(|r| match r.action {
                Some(LandingAction::OpenRecent(i)) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(recent_rows, vec![0, 1]);

        // Outside the column, nothing is hit.
        assert!(matches!(
            hit_test(&l, &probe_panels_with(&ws, &recents), ox - 10.0, row_y(4)),
            Hit::Elsewhere
        ));
    }

    #[test]
    fn the_landing_page_drops_the_recent_section_when_there_is_none() {
        let with_none = landing_rows(&[]);
        assert!(
            !with_none
                .iter()
                .any(|r| matches!(r.action, Some(LandingAction::OpenRecent(_)))),
            "an empty history should not leave a bare Recent heading"
        );
        let with_some = landing_rows(&[Path::new("/p")]);
        assert!(with_some.len() > with_none.len());
    }

    #[test]
    fn home_is_shortened_the_way_a_shell_shows_it() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        assert_eq!(shorten_home(&home), "~");
        assert_eq!(shorten_home(&home.join("code/thing")), "~/code/thing");
        assert_eq!(shorten_home(Path::new("/opt/elsewhere")), "/opt/elsewhere");
    }

    /// Where the caret is drawn for a char offset, in window coordinates.
    /// Mirrors `draw_editor` exactly; if the two drift, the test is the
    /// thing that notices.
    fn caret_at(l: &Layout, editor: &Editor, offset: usize, tab: usize) -> (f32, f32) {
        let doc = &editor.doc;
        let line = doc.char_to_line(offset);
        let col = offset - doc.line_to_char(line);
        let slice = doc.line(line).unwrap();
        let x = l.text_x + (visual_col(slice, col, tab) as f32 - editor.scroll_x) * l.cell.advance;
        let y = l.text_y + (line - editor.top_line()) as f32 * l.cell.line_height
            - editor.pixel_offset(l.cell.line_height);
        (x, y)
    }

    /// The text must sit inside its own current-line highlight.
    ///
    /// Everything drawn behind the text — the highlight, the selection,
    /// the caret — went through `row_y`, which subtracts the fractional
    /// scroll. The text computed its own baseline and did not, so at
    /// any scroll that was not a whole number of rows it sat up to a
    /// full row away from the highlight and caret that belonged to it.
    ///
    /// Checked against the pixels, because the previous caret test
    /// compared two things that were both on the decoration side of
    /// that split and so agreed with each other while both were wrong.
    #[test]
    fn the_caret_and_the_text_agree_at_any_scroll() {
        const W: u32 = 700;
        const H: u32 = 400;
        let Ok(mut r) = Renderer::offscreen(W, H, 1.0, 16.0) else {
            return;
        };
        let text: String = (0..300).map(|i| format!("line{i}\n")).collect();
        let theme = Theme::default();

        for scroll in [0.0_f32, 2.0, 3.5, 7.25, 12.75] {
            let mut editor = Editor::new(TextBuffer::from_text(&text), None, 4);
            editor.scroll = scroll;
            // A cursor a few rows down, so the band is not at an edge.
            let line = editor.top_line() + 3;
            editor.sel = Selection::single(editor.doc.line_to_char(line) + 2);

            let mut ws = Workspace::empty(None);
            ws.push(editor);
            let scene = Scene {
                tab_width: 4,
                ..probe_scene(&ws, &theme)
            };
            let mut scratch = Scratch::default();
            draw(&mut r, &scene, &mut scratch);
            let Ok(buf) = r.read_rgba() else { return };
            let l = layout(&r, &ws, Chrome::default());

            // Only inside the text area: the tab strip's active
            // underline is painted in the caret's own colour.
            let first = l.text_y as u32;
            let last = (l.text_y + l.text_h) as u32;
            // Far right of any glyph, where only the highlight paints.
            let bare = W - 24;
            let band: Vec<u32> = (first..last)
                .filter(|y| find(&buf, W, theme.cursor_line, bare..bare + 1, *y..*y + 1).0 > 0)
                .collect();
            assert!(!band.is_empty(), "no highlight at scroll {scroll}");

            // The cursor's line number, which is drawn on the text's
            // own baseline and in a colour no other row uses. Comparing
            // the caret to the highlight proves nothing: both go
            // through `row_y` and so always agreed, including while the
            // text they belonged to did not.
            let ink: Vec<u32> = (first..last)
                .filter(|y| {
                    find(
                        &buf,
                        W,
                        theme.gutter_fg_active,
                        0..l.text_x as u32,
                        *y..*y + 1,
                    )
                    .0 > 0
                })
                .collect();
            assert!(!ink.is_empty(), "no active line number at scroll {scroll}");
            assert!(
                ink.iter().all(|y| band.contains(y)),
                "scroll {scroll}: the cursor's line is drawn on rows \
                 {:?} but its highlight covers {}..{} — the text and its \
                 own decorations are on different rows",
                (ink[0], ink[ink.len() - 1]),
                band[0],
                band[band.len() - 1]
            );
        }
    }

    /// Hovering highlights the row `hit_test` names, and the band is
    /// drawn where `draw_context_menu` puts it. The two work the row
    /// out separately, so a test that a point inside a row comes back
    /// as that row is what keeps the highlight under the pointer.
    #[test]
    fn a_point_inside_a_menu_row_hit_tests_as_that_row() {
        let Ok(r) = Renderer::offscreen(900, 600, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(&r, &ws, Chrome::default());
        let menu = ContextMenu::for_diff(
            (120.0, 80.0),
            std::path::PathBuf::from("/w/a.rs"),
            0,
            crate::context::DiffRow {
                revertible: true,
                ..Default::default()
            },
        );
        let panels = Panels {
            context: Some(&menu),
            ..probe_panels(&ws)
        };
        let (x, y, width, _) = context_bounds(&l, &menu);

        for index in 0..menu.entries.len() {
            // The middle of the row, as the drawing lays it out.
            let row_y = y + 2.0 + index as f32 * l.cell.line_height;
            let at = row_y + l.cell.line_height / 2.0;
            assert_eq!(
                hit_test(&l, &panels, x + width / 2.0, at),
                Hit::Context(index),
                "row {index} is drawn at {row_y} and hit-tests elsewhere"
            );
        }

        // And a point outside it dismisses rather than choosing.
        assert_eq!(
            hit_test(&l, &panels, x - 4.0, y + 4.0),
            Hit::Elsewhere,
            "beside the menu is not in it"
        );
    }

    #[test]
    fn the_cmd_underline_sits_under_the_name_and_nothing_else() {
        const W: u32 = 700;
        const H: u32 = 300;
        let Ok(mut r) = Renderer::offscreen(W, H, 1.0, 16.0) else {
            return;
        };
        let theme = Theme::default();
        let text = "let total = compute(x);\n";
        let start = text.find("compute").expect("the name is there");
        let span = start..start + "compute".len();

        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(TextBuffer::from_text(text), None, 4));
        let l = layout(&r, &ws, Chrome::default());

        let mut scratch = Scratch::default();
        let without = {
            let scene = Scene {
                tab_width: 4,
                ..probe_scene(&ws, &theme)
            };
            draw(&mut r, &scene, &mut scratch);
            let Ok(buf) = r.read_rgba() else { return };
            buf
        };
        let with = {
            let scene = Scene {
                tab_width: 4,
                cmd_link: Some(&span),
                ..probe_scene(&ws, &theme)
            };
            draw(&mut r, &scene, &mut scratch);
            let Ok(buf) = r.read_rgba() else { return };
            buf
        };

        // The row the underline is on: the one row that gained
        // foreground pixels. Comparing against the same frame without
        // the link is what keeps the glyphs out of the answer.
        let first = l.text_y as u32;
        let last = ((l.text_y + l.cell.line_height * 2.0) as u32).min(H);
        let gained = |y: u32| {
            let on = |buf: &[u8]| find(buf, W, theme.fg, 0..W, y..y + 1).0;
            on(&with).saturating_sub(on(&without))
        };
        let rows: Vec<u32> = (first..last).filter(|y| gained(*y) > 0).collect();
        assert!(!rows.is_empty(), "nothing was underlined");

        // And it spans the name, not the whole line. The bounds come
        // from the pixels that *appeared*, because the row the
        // underline is on is also where a descender goes — `compute`
        // has a `p`, and counting it would widen the answer.
        let row = rows[0];
        let appeared: Vec<u32> = (0..W)
            .filter(|x| {
                let i = ((row * W + x) * 4) as usize;
                is(&with[i..i + 4], theme.fg) && !is(&without[i..i + 4], theme.fg)
            })
            .collect();
        let (lo, hi) = (
            *appeared.first().expect("the underline has pixels"),
            *appeared.last().expect("the underline has pixels"),
        );
        let cell = l.cell.advance;
        let want_lo = l.text_x + start as f32 * cell;
        let want_hi = want_lo + "compute".len() as f32 * cell;
        assert!(
            (lo as f32 - want_lo).abs() <= cell,
            "starts at {lo}, expected about {want_lo}"
        );
        assert!(
            (hi as f32 - want_hi).abs() <= cell,
            "ends at {hi}, expected about {want_hi}"
        );
    }

    #[test]
    fn clicking_where_the_caret_is_drawn_lands_back_on_it() {
        let Ok(r) = Renderer::offscreen(900, 500, 1.0, 16.0) else {
            return;
        };
        let text: String = (0..200)
            .map(|i| format!("line {i} with\ta tab and words\n"))
            .collect();
        let mut editor = Editor::new(TextBuffer::from_text(&text), None, 4);
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );

        // Whole and fractional scroll positions alike, on both axes: a
        // line scrolled sideways is the case where the column under the
        // pointer is not the column it would be at rest.
        for scroll in [0.0_f32, 1.0, 3.5, 7.25, 12.75, 40.5] {
            for scroll_x in [0.0_f32, 4.0, 9.0] {
                editor.scroll = scroll;
                editor.scroll_x = scroll_x;
                let top = editor.top_line();
                for line in top..(top + 4) {
                    let start = editor.doc.line_to_char(line);
                    for col in [0usize, 3, 6, 11] {
                        let offset = start + col;
                        let (x, y) = caret_at(&l, &editor, offset, 4);
                        // Aim at the middle of the cell, as a person would.
                        let got = point_to_char(
                            &l,
                            &editor.doc,
                            editor.scroll,
                            editor.scroll_x,
                            x + l.cell.advance * 0.25,
                            y + l.cell.line_height * 0.5,
                            4,
                        );
                        assert_eq!(
                            got, offset,
                            "scroll {scroll}/{scroll_x}, line {line}, col {col}:                              clicked the caret and got {got}"
                        );
                    }
                }
            }
        }
        editor.scroll_x = 0.0;
    }

    /// The whole point of the horizontal scroll: the text moves left by
    /// the columns asked for, and the caret moves with it rather than
    /// staying where it was drawn before.
    #[test]
    fn scrolling_sideways_carries_the_caret_with_the_text() {
        const W: u32 = 900;
        let Ok(mut r) = Renderer::offscreen(W, 300, 1.0, 16.0) else {
            return;
        };
        // Four columns of indent, so where the ink starts says how far
        // the line has been scrolled.
        let line = format!("    {}\n", "x".repeat(400));
        let mut editor = Editor::new(TextBuffer::from_text(&line), None, 4);
        // A column that is on screen at rest.
        editor.sel = Selection::single(12);

        // Laid out with the file open, because an open file has a tab
        // strip and the text starts under it.
        let mut ws = Workspace::empty(None);
        ws.push(editor);
        let l = layout(&r, &ws, Chrome::default());
        if let Some(editor) = ws.active_mut() {
            editor.visible_cols = l.cols();
        }

        let theme = Theme::default();
        let mut scratch = Scratch::default();
        // The first row of text, where the caret is: the only thing
        // drawn in `theme.cursor` there is the caret itself.
        let rows = (l.text_y as u32 + 1)..((l.text_y + l.cell.line_height) as u32);
        // Where the caret is, and where the line's first character is.
        let mut marks = |scroll_x: f32, ws: &mut Workspace, r: &mut Renderer| {
            if let Some(editor) = ws.active_mut() {
                editor.scroll_x = scroll_x;
            }
            draw(r, &probe_scene(ws, &theme), &mut scratch);
            let buf = r.read_rgba().expect("readback");
            let left = |colour| {
                find(&buf, W, colour, 0..W, rows.clone())
                    .1
                    .map(|(lo, _)| lo)
            };
            (left(theme.cursor), left(theme.fg))
        };

        let (rest_caret, rest_ink) = marks(0.0, &mut ws, &mut r);
        let (moved_caret, moved_ink) = marks(5.0, &mut ws, &mut r);
        let shift = (rest_caret.expect("a caret") - moved_caret.expect("a caret")) as f32;
        assert!(
            (shift - 5.0 * l.cell.advance).abs() <= 1.0,
            "five columns of scroll should move the caret five columns left, moved {shift}px"
        );
        // The text moves with it — by the four columns of indent, which
        // is as far as it can before being cut off at the left edge.
        let ink = (rest_ink.expect("ink") - moved_ink.expect("ink")) as f32;
        assert!(
            (ink - 4.0 * l.cell.advance).abs() <= 1.0,
            "the text should move with the caret: {ink}px"
        );

        // Scrolled past the caret, it is off the left edge — and drawn
        // there it would sit on the gutter, pointing at nothing.
        assert!(
            marks(40.0, &mut ws, &mut r).0.is_none(),
            "a caret scrolled off the left should not be drawn on the gutter"
        );
    }

    #[test]
    fn the_caret_is_drawn_on_the_partly_visible_bottom_row() {
        let Ok(mut r) = Renderer::offscreen(600, 240, 1.0, 16.0) else {
            return;
        };
        let text: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let mut editor = Editor::new(TextBuffer::from_text(&text), None, 4);

        let ws_probe = Workspace::empty(None);
        let l = layout(
            &r,
            &ws_probe,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        // Scroll by a half line, then put the cursor on the last row that
        // has any pixels on screen.
        editor.scroll = 5.5;
        let bottom = editor.top_line() + l.rows;
        editor.sel = Selection::single(editor.doc.line_to_char(bottom));

        let mut ws = Workspace::empty(None);
        ws.push(editor);
        let theme = Theme::default();
        let scene = Scene {
            blame: None,
            workspace: &ws,
            recents: &[],
            highlights: &[],
            diff_old: None,
            cmd_link: None,
            diff_old_spans: &[],
            diff_new_spans: &[],
            diagnostics: &[],
            completion: None,
            hunks: &[],
            branch: None,
            lsp: None,
            lsp_busy: None,
            autosave: false,
            hovered: None,
            prompt: None,
            caret_visible: true,
            find: None,
            search: None,
            find_matches: &[],
            tree: None,
            git: None,
            sidebar_cols: TREE_COLS,
            preview: None,
            preview_scroll: 0.0,
            figures: &EMPTY_FIGURES,
            preview_sel: None,
            diff: None,
            diff_title: None,
            diff_scroll: 0.0,
            diff_side_by_side: false,
            diff_editable: false,
            hover: None,
            image: None,
            settings: None,
            context: None,
            picker: None,
            theme: &theme,
            status: None,
            tab_width: 4,
        };
        let mut scratch = Scratch::default();
        draw(&mut r, &scene, &mut scratch);
        let buf = r.read_rgba().expect("readback");

        let (count, _) = find(&buf, 600, theme.cursor, 0..600, 0..240);
        assert!(
            count > 0,
            "the caret vanished on the bottom row; the draw loops disagreed \
             about which rows are visible"
        );
    }

    /// Every kind with a preview needs a way back to its source, and
    /// the eye is that way. It used to appear for Markdown alone, which
    /// left HTML and SVG with one mode each.
    #[test]
    fn the_eye_is_offered_for_every_previewable_kind() {
        let has_eye = |name: &str| {
            let mut ws = Workspace::empty(None);
            ws.push(Editor::new(
                lithe_core::TextBuffer::from_text(""),
                Some(std::path::PathBuf::from(name)),
                4,
            ));
            status_buttons(&probe_panels(&ws))
                .iter()
                .any(|s| s.button == StatusButton::Preview)
        };
        for name in ["a.md", "a.svg", "a.png"] {
            assert!(
                has_eye(name),
                "{name} has a preview but no way to toggle it"
            );
        }
        for name in ["a.rs", "a.html"] {
            assert!(!has_eye(name), "{name} has no preview to offer");
        }
    }

    #[test]
    fn status_buttons_report_what_is_open() {
        let ws = Workspace::empty(None);
        let mut panels = probe_panels(&ws);
        let slots = status_buttons(&panels);
        assert!(
            slots.iter().all(|s| !s.active),
            "nothing is open, so nothing should look on"
        );
        assert_eq!(
            slots.iter().map(|s| s.icon).collect::<Vec<_>>(),
            vec![Icon::Files, Icon::Git, Icon::Search],
            "the eye is only offered for a previewable file"
        );

        panels.search_open = true;
        let slots = status_buttons(&panels);
        assert!(
            slots
                .iter()
                .find(|s| s.button == StatusButton::Search)
                .unwrap()
                .active,
            "the search button should light up while the panel is open"
        );
    }

    #[test]
    fn the_preview_button_appears_only_for_markdown() {
        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(
            TextBuffer::new(),
            Some(std::path::PathBuf::from("notes.md")),
            4,
        ));
        let slots = status_buttons(&probe_panels(&ws));
        assert!(slots.iter().any(|s| s.button == StatusButton::Preview));

        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(
            TextBuffer::new(),
            Some(std::path::PathBuf::from("main.rs")),
            4,
        ));
        let slots = status_buttons(&probe_panels(&ws));
        assert!(!slots.iter().any(|s| s.button == StatusButton::Preview));
    }

    #[test]
    fn a_click_in_an_editable_diff_lands_on_the_text_it_looks_like() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        // A diff is only ever open over a file, so the workspace has one:
        // with no buffer at all the landing page takes the click first.
        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(TextBuffer::from_text("a\nb\nc\n"), None, 4));
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        for side_by_side in [false, true] {
            let panels = Panels {
                diff: Some(DiffKind::Editable),
                diff_side_by_side: side_by_side,
                diff_scroll: 7.0,
                ..probe_panels(&ws)
            };
            let frame = diff_frame(&l, side_by_side);

            // Third visible row, fifth cell of the new side's text.
            let x = frame.new_text_x + 5.0 * l.cell.advance;
            let y = frame.body_y + 2.5 * l.cell.line_height;
            assert_eq!(
                hit_test(&l, &panels, x, y),
                Hit::DiffText { row: 9, column: 5 },
                "side_by_side={side_by_side}: the scroll is part of the row"
            );

            // The old side is a committed version, so it is not text —
            // but it is still a row, so a right click on it knows which
            // change it is over.
            let old = frame.new_text_x - 4.0 * l.cell.advance;
            assert_eq!(
                hit_test(&l, &panels, old, y),
                Hit::DiffRow(9),
                "side_by_side={side_by_side}: the committed column is a row, not text"
            );
        }
    }

    #[test]
    fn a_read_only_diff_takes_no_caret_at_all() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(TextBuffer::from_text("a\nb\nc\n"), None, 4));
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let frame = diff_frame(&l, true);
        let panels = Panels {
            diff: Some(DiffKind::ReadOnly),
            diff_side_by_side: true,
            ..probe_panels(&ws)
        };
        // Both columns: a commit's diff has no buffer behind either of
        // them, so neither can take a caret, and both are still rows.
        for x in [
            frame.new_text_x + 2.0 * l.cell.advance,
            frame.new_text_x - 4.0 * l.cell.advance,
        ] {
            let hit = hit_test(&l, &panels, x, frame.body_y + l.cell.line_height);
            assert_eq!(
                hit,
                Hit::DiffRow(1),
                "a commit's diff has no buffer behind it, got {hit:?}"
            );
        }
    }

    #[test]
    fn the_caret_follows_the_line_the_diff_row_shows() {
        use lithe_vcs::{DiffLine, LineKind};

        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(
            TextBuffer::from_text("one\ntwo\nthree\n"),
            None,
            4,
        ));
        let editor = ws.active_mut().expect("the buffer just pushed");
        // Second line, two characters in.
        let at = editor.doc.line_to_char(1) + 2;
        editor.sel.collapse_to(lithe_core::Cursor::at(at));

        let theme = Theme::default();
        let scene = Scene {
            diff_editable: true,
            ..probe_scene(&ws, &theme)
        };
        let row = |new: Option<usize>| DiffLine {
            kind: LineKind::Context,
            old: None,
            new,
            text: String::new(),
        };
        assert_eq!(caret_column(&scene, &row(Some(2))), Some(2));
        assert_eq!(
            caret_column(&scene, &row(Some(1))),
            None,
            "only the cursor's own line carries the caret"
        );
        assert_eq!(
            caret_column(&scene, &row(None)),
            None,
            "a removed line is not in the working copy"
        );
    }

    #[test]
    fn the_hover_popup_stays_inside_the_window() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(TextBuffer::from_text("a\n"), None, 4));
        let l = layout(&r, &ws, Chrome::default());

        let hover = |at| Hover {
            lines: crate::markdown::render_boxed("a signature\n", Hover::COLUMNS),
            at,
        };
        // Every corner, including well outside the window.
        for at in [
            (0.0, 0.0),
            (l.width - 1.0, l.text_y + 4.0),
            (l.width * 2.0, l.height * 2.0),
            (l.width / 2.0, l.height - 1.0),
        ] {
            let h = hover(at);
            let (x, y, w, height) = hover_bounds(&l, &h);
            assert!(
                x >= 0.0 && x + w <= l.width + 0.5,
                "off the side at {at:?}: {x} + {w}"
            );
            assert!(y >= 0.0, "off the top at {at:?}: {y}");
            assert!(
                y + height <= l.height + 0.5,
                "off the bottom at {at:?}: {y} + {height}"
            );
        }
    }

    #[test]
    fn a_hover_with_room_above_it_sits_above_the_character() {
        let Ok(r) = Renderer::offscreen(900, 600, 1.0, 16.0) else {
            return;
        };
        let mut ws = Workspace::empty(None);
        ws.push(Editor::new(TextBuffer::from_text("a\n"), None, 4));
        let l = layout(&r, &ws, Chrome::default());

        let hover = Hover {
            lines: crate::markdown::render_boxed("short\n", Hover::COLUMNS),
            at: (100.0, l.text_y + l.text_h - 20.0),
        };
        let (_, y, _, height) = hover_bounds(&l, &hover);
        assert!(
            y + height <= hover.at.1 + 0.5,
            "a tooltip the pointer covers is no use: {y} + {height} vs {}",
            hover.at.1
        );
    }

    #[test]
    fn a_long_hover_is_capped_rather_than_filling_the_window() {
        let long = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let hover = Hover {
            lines: crate::markdown::render_boxed(&long, Hover::COLUMNS),
            at: (0.0, 0.0),
        };
        assert!(
            hover.lines.len() > Hover::ROWS,
            "the fixture is long enough"
        );
        assert_eq!(hover.rows(), Hover::ROWS);
    }

    #[test]
    fn a_full_area_page_leaves_the_chrome_clickable() {
        let Ok(r) = Renderer::offscreen(1200, 700, 1.0, 16.0) else {
            return;
        };
        // A real directory, so the tree has a row to be clicked.
        let dir = crate::testutil::TempDir::new();
        dir.write("a.rs", "fn x() {}\n");
        let mut ws = Workspace::empty(Some(dir.path().to_path_buf()));
        ws.push(Editor::new(TextBuffer::from_text("a\n"), None, 4));
        ws.show_settings();
        let tree = Tree::new(dir.path().to_path_buf());
        assert!(!tree.rows.is_empty(), "the fixture has a file in it");
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar: true,
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let panels = Panels {
            tree: Some(&tree),
            settings_open: true,
            ..probe_panels(&ws)
        };

        // The tab strip: with the settings page claiming every click
        // before the chrome was tested, there was no way back to the file.
        let labels = ws.labels();
        let (x0, x1) = tab_extents(&l, &labels)[0];
        assert_eq!(
            hit_test(&l, &panels, (x0 + x1) / 2.0, l.tab_h / 2.0),
            Hit::Tab(0)
        );

        // And the sidebar.
        // The sidebar's rows start at the top of the window, beside the
        // tab strip rather than below it.
        let in_tree = hit_test(&l, &panels, l.tree_w / 2.0, l.cell.line_height / 2.0);
        assert!(
            matches!(in_tree, Hit::Tree(_)),
            "the sidebar should still take its own clicks, got {in_tree:?}"
        );

        // The page itself still works, which is the thing that used to.
        let rows = setting_rows();
        let (ox, oy, _) = settings_origin(&l, rows.len());
        let switch = rows
            .iter()
            .position(|r| matches!(r, SettingRow::Switch(_)))
            .expect("a switch");
        assert!(matches!(
            hit_test(
                &l,
                &panels,
                ox + l.cell.advance,
                oy + (switch as f32 + 0.5) * l.cell.line_height
            ),
            Hit::Setting(_)
        ));
    }

    #[test]
    fn clicking_a_status_button_hits_the_one_it_looks_like() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let panels = probe_panels(&ws);
        let slots = status_buttons(&panels);
        let extents = status_button_extents(&l, &slots);

        let y = l.text_y + l.text_h + l.cell.line_height / 2.0;
        for (slot, (x0, x1)) in slots.iter().zip(&extents) {
            let middle = (x0 + x1) / 2.0;
            match hit_test(&l, &panels, middle, y) {
                Hit::Status(got) => assert_eq!(got, slot.button, "wrong button"),
                other => panic!("expected {:?}, got {other:?}", slot.button),
            }
        }
        // Past the last button, the status bar is inert again.
        let past = extents.last().unwrap().1 + 10.0 * l.cell.advance;
        assert!(matches!(hit_test(&l, &panels, past, y), Hit::Elsewhere));
    }

    /// The horizontal bar is furniture a file only grows when it needs
    /// it, and grabbing it has to land where it is drawn.
    #[test]
    fn a_line_that_fits_grows_no_horizontal_scrollbar() {
        let Ok(r) = Renderer::offscreen(900, 500, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(&r, &ws, Chrome::default());
        let cols = l.cols();

        assert!(
            h_scrollbar_extent(&l, cols, 0.0).is_none(),
            "a line exactly as wide as the window has nothing to scroll to"
        );
        let wide = cols * 3;
        let (left, width) = h_scrollbar_extent(&l, wide, 0.0).expect("a bar");
        assert!((left - l.text_x).abs() < 0.5, "at rest it is at the left");

        // Dragging the thumb by its middle to the far right asks for the
        // last column, and asking for the last column puts it back there.
        let max = (wide - cols) as f32;
        let far = h_scrollbar_scroll_at(&l, wide, l.width);
        assert!((far - max).abs() < 1.0, "{far} should be about {max}");
        let (end, _) = h_scrollbar_extent(&l, wide, max).expect("a bar");
        let track_end = l.width - SCROLLBAR_W - width;
        assert!((end - track_end).abs() < 0.5, "and the thumb is at the end");
    }

    #[test]
    fn a_name_is_cut_at_a_word_rather_than_mid_surname() {
        let mut out = String::new();
        shorten_name(&mut out, "Ahmad Masud", 20);
        assert_eq!(out, "Ahmad Masud", "it fits, so it is left alone");

        out.clear();
        shorten_name(&mut out, "Ahmad Masud", 8);
        assert_eq!(out, "Ahmad", "the given name, not `Ahmad M…`");

        out.clear();
        shorten_name(&mut out, "ahmad-masud", 8);
        assert_eq!(
            out, "ahmad-m…",
            "one word, so there is no boundary to cut at"
        );

        out.clear();
        shorten_name(&mut out, "", 8);
        assert_eq!(out, "");
    }

    #[test]
    fn a_short_file_grows_no_scrollbar() {
        let Ok(r) = Renderer::offscreen(600, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        assert!(
            scrollbar_extent(&l, 3, 0.0).is_none(),
            "three lines in a tall window need no scrollbar"
        );
        assert!(scrollbar_extent(&l, 10_000, 0.0).is_some());
    }

    #[test]
    fn the_thumb_tracks_the_scroll_and_stays_inside_the_track() {
        let Ok(r) = Renderer::offscreen(600, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let total = 1000;
        let max_scroll = total as f32 - l.rows as f32;

        let (top_at_start, height) = scrollbar_extent(&l, total, 0.0).unwrap();
        let (top_at_end, _) = scrollbar_extent(&l, total, max_scroll).unwrap();

        assert!(
            (top_at_start - l.text_y).abs() < 1.0,
            "should start at the top"
        );
        assert!(top_at_end > top_at_start, "should move down as you scroll");
        assert!(
            top_at_end + height <= l.text_y + l.text_h + 1.0,
            "the thumb must not run past the end of the track"
        );
        assert!(height >= 24.0, "the thumb has to stay grabbable");
    }

    #[test]
    fn dragging_the_thumb_round_trips_with_where_it_is_drawn() {
        let Ok(r) = Renderer::offscreen(600, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let total = 500;

        for scroll in [0.0, 37.0, 120.0, total as f32 - l.rows as f32] {
            let (top, height) = scrollbar_extent(&l, total, scroll).unwrap();
            // Grabbing the thumb by its middle should not move it.
            let back = scrollbar_scroll_at(&l, total, top + height / 2.0);
            assert!(
                (back - scroll).abs() < 1.0,
                "drag and draw disagree at {scroll}: got {back}"
            );
        }
    }

    #[test]
    fn a_tab_separates_its_body_from_its_close_box() {
        let Ok(r) = Renderer::offscreen(900, 200, 1.0, 16.0) else {
            eprintln!("skipping, no usable GPU");
            return;
        };
        let mut ws = Workspace::empty(None);
        for name in ["alpha.rs", "beta.rs"] {
            ws.push(Editor::new(
                TextBuffer::new(),
                Some(std::path::PathBuf::from(name)),
                4,
            ));
        }
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let labels = ws.labels();
        let tabs = tab_extents(&l, &labels);
        assert_eq!(tabs.len(), 2);

        // Clicking the name selects; clicking the trailing box closes.
        let (x0, x1) = tabs[1];
        let on_name = x0 + l.cell.advance;
        let on_close = x1 - l.cell.advance;
        let y = l.cell.line_height / 2.0;

        assert!(
            matches!(hit_test(&l, &probe_panels(&ws), on_name, y), Hit::Tab(1)),
            "clicking the label should select the tab"
        );
        assert!(
            matches!(
                hit_test(&l, &probe_panels(&ws), on_close, y),
                Hit::TabClose(1)
            ),
            "clicking the trailing box should close the tab"
        );
        // And the two regions must not overlap.
        assert!(on_name < x1 - CLOSE_COLS * l.cell.advance);
    }

    #[test]
    fn keywords_are_painted_in_the_keyword_colour() {
        const W: u32 = 640;
        const H: u32 = 240;
        let Ok(mut r) = Renderer::offscreen(W, H, 1.0, 16.0) else {
            eprintln!("skipping, no usable GPU");
            return;
        };

        // A path is enough to pick the grammar; nothing is read from disk.
        let mut editor = Editor::new(
            TextBuffer::from_text("fn main() {}\n"),
            Some(std::path::PathBuf::from("scratch.rs")),
            4,
        );
        assert_eq!(editor.language_name(), Some("rust"), "grammar not attached");

        let mut highlights = Vec::new();
        editor.highlights(0..editor.doc.len_bytes(), &mut highlights);
        assert!(!highlights.is_empty(), "no spans produced");

        let mut ws = Workspace::empty(None);
        ws.push(editor);
        let theme = Theme::default();
        let scene = Scene {
            diff_old: None,
            cmd_link: None,
            diff_old_spans: &[],
            diff_new_spans: &[],
            blame: None,
            workspace: &ws,
            find: None,
            search: None,
            find_matches: &[],
            completion: None,
            diagnostics: &[],
            hunks: &[],
            branch: None,
            lsp: None,
            lsp_busy: None,
            autosave: false,
            hovered: None,
            prompt: None,
            caret_visible: true,
            recents: &[],
            highlights: &highlights,
            tree: None,
            git: None,
            sidebar_cols: TREE_COLS,
            preview: None,
            preview_scroll: 0.0,
            figures: &EMPTY_FIGURES,
            preview_sel: None,
            diff: None,
            diff_title: None,
            diff_scroll: 0.0,
            diff_side_by_side: false,
            diff_editable: false,
            hover: None,
            image: None,
            settings: None,
            context: None,
            picker: None,
            theme: &theme,
            status: None,
            tab_width: 4,
        };
        let mut scratch = Scratch::default();
        draw(&mut r, &scene, &mut scratch);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let buf = r.read_rgba().expect("readback");

        let y0 = l.text_y as u32;
        let row = y0..y0 + l.cell.line_height as u32;

        // "fn" is a keyword and sits in the first two cells.
        let (keyword_px, span) = find(&buf, W, theme.syntax.keyword, 0..W, row.clone());
        assert!(
            keyword_px > 0,
            "no keyword-coloured pixels; highlighting is not reaching the renderer"
        );
        let first_cell = (l.text_x + 2.0 * l.cell.advance).round() as u32;
        assert!(
            span.unwrap().0 < first_cell,
            "keyword colour appeared outside the keyword"
        );

        // "main" is a function, and must be a different colour.
        let (function_px, _) = find(&buf, W, theme.syntax.function, 0..W, row);
        assert!(
            function_px > 0,
            "the function name was not painted in the function colour"
        );
    }

    #[test]
    fn a_tabbed_line_puts_the_caret_on_the_tab_stop() {
        let mut editor = Editor::new(TextBuffer::from_text("\tx\n"), None, 4);
        editor.sel = Selection::single(1);

        let Some((buf, l, stride, theme)) = render(editor) else {
            return;
        };
        let y0 = l.text_y as u32;
        let (count, span) = find(
            &buf,
            stride,
            theme.cursor,
            0..stride,
            y0..y0 + l.cell.line_height as u32,
        );
        assert!(count > 0, "no caret drawn");
        let want = (l.text_x + 4.0 * l.cell.advance).round() as u32;
        assert!(
            span.unwrap().0.abs_diff(want) <= 2,
            "caret missed the tab stop"
        );
    }

    #[test]
    fn a_click_in_the_preview_finds_the_link_under_it() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        // Laid out and hit-tested with the same measure, which is the
        // whole point: the column a run was wrapped to is the column it
        // is drawn in, so a click lands on the glyph it looks like.
        let mut measure = crate::markdown::Cells;
        let lines = crate::markdown::render_with(
            "go to [the docs](https://example.com) now\n",
            preview_columns(&l),
            &mut measure,
        );
        let page = preview_page(&l, &mut measure);
        // The first row of the document: the page leaves one line of air
        // above it.
        let y = l.text_y + 1.5 * l.cell.line_height;

        // "go to " is six cells, so the link runs from the seventh.
        let on_link = page.left + 8.0 * l.cell.advance;
        assert_eq!(
            preview_link_at(&l, &lines, 0.0, on_link, y, &mut measure).as_deref(),
            Some("https://example.com"),
        );

        // The words on either side of it go nowhere.
        let on_prose = page.left + 2.0 * l.cell.advance;
        assert!(preview_link_at(&l, &lines, 0.0, on_prose, y, &mut measure).is_none());
        let past_the_end = page.left + 60.0 * l.cell.advance;
        assert!(preview_link_at(&l, &lines, 0.0, past_the_end, y, &mut measure).is_none());
        // And neither does the row below the one it is on.
        let next_row = y + l.cell.line_height;
        assert!(preview_link_at(&l, &lines, 0.0, on_link, next_row, &mut measure).is_none());
    }

    #[test]
    fn a_scrolled_preview_hit_tests_the_rows_it_is_showing() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(
            &r,
            &ws,
            Chrome {
                sidebar_cols: TREE_COLS,
                ..Default::default()
            },
        );
        let mut measure = crate::markdown::Cells;
        let source = "a\n\nb\n\nc\n\n[the link](x.md)\n";
        let lines = crate::markdown::render_with(source, preview_columns(&l), &mut measure);
        let at = lines
            .iter()
            .position(|line| line.spans.iter().any(|s| s.href.is_some()))
            .expect("the link's line");
        let page = preview_page(&l, &mut measure);
        let y = l.text_y + 1.5 * l.cell.line_height;
        let x = page.left + 2.0 * l.cell.advance;
        // Scrolled so that the link's line is the top one.
        assert_eq!(
            preview_link_at(&l, &lines, at as f32, x, y, &mut measure).as_deref(),
            Some("x.md"),
        );
        // And not when it is scrolled past.
        assert!(preview_link_at(&l, &lines, at as f32 + 1.0, x, y, &mut measure).is_none());
    }
}

#[cfg(test)]
mod preview_selection_tests {
    use super::*;
    use crate::markdown::{Line as MdLine, Span as MdSpan, Style};

    fn line(indent: usize, text: &str) -> MdLine {
        MdLine {
            spans: vec![MdSpan::plain(text, Style::Body)],
            indent,
            ..Default::default()
        }
    }

    fn at(line: usize, char: usize) -> PreviewPos {
        PreviewPos { line, char }
    }

    #[test]
    fn a_selection_inside_one_line_covers_just_that_run() {
        let l = line(0, "hello world");
        assert_eq!(selected_chars(&l, 3, at(3, 2), at(3, 7)), Some((2, 7)));
    }

    #[test]
    fn lines_outside_the_selection_are_not_covered() {
        let l = line(0, "hello");
        assert_eq!(selected_chars(&l, 1, at(2, 0), at(4, 3)), None);
        assert_eq!(selected_chars(&l, 5, at(2, 0), at(4, 3)), None);
    }

    #[test]
    fn a_middle_line_is_covered_end_to_end() {
        // What makes a multi-line drag read as one block rather than
        // three ragged ones.
        let l = line(0, "hello");
        assert_eq!(selected_chars(&l, 3, at(2, 4), at(4, 1)), Some((0, 5)));
    }

    #[test]
    fn the_first_and_last_lines_are_cut_where_the_drag_is() {
        let l = line(0, "hello world");
        assert_eq!(selected_chars(&l, 2, at(2, 6), at(4, 1)), Some((6, 11)));
        assert_eq!(selected_chars(&l, 4, at(2, 6), at(4, 3)), Some((0, 3)));
    }

    #[test]
    fn a_caret_with_no_width_selects_nothing() {
        let l = line(0, "hello");
        assert_eq!(selected_chars(&l, 2, at(2, 3), at(2, 3)), None);
    }

    #[test]
    fn a_blank_line_in_the_middle_is_still_covered() {
        // Zero characters, but it is inside the selection, so it has
        // to read as selected rather than as a hole in it.
        let l = line(0, "");
        assert_eq!(selected_chars(&l, 3, at(2, 0), at(4, 2)), Some((0, 0)));
    }

    #[test]
    fn an_indent_counts_as_characters_of_the_line() {
        // The indent is drawn, so a selection across the line has to
        // cover it or the band starts in the wrong place.
        let l = line(4, "item");
        assert_eq!(l.len_chars(), 8);
        assert_eq!(l.text(), "    item");
        assert_eq!(selected_chars(&l, 3, at(2, 0), at(4, 0)), Some((0, 8)));
    }

    /// Where the renderer is available: that a point taken from where
    /// a character is drawn comes back as that character. The two
    /// walks are separate code, and a proportional face is exactly
    /// where they could disagree.
    #[test]
    fn a_point_where_a_character_is_drawn_maps_back_to_it() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(&r, &ws, Chrome::default());
        let lines = vec![
            line(0, "a paragraph of proportional prose"),
            line(4, "an indented one"),
        ];
        let mut measure = crate::Measured { renderer: &r };
        let page = preview_page(&l, &mut measure);

        let mut row_y = l.text_y + l.cell.line_height;
        for (index, md) in lines.iter().enumerate() {
            let y = row_y + l.cell.line_height / 2.0;
            for want in 0..=md.len_chars() {
                let x = preview_x_at(&l, md, &page, want, &mut measure);
                // Half a cell in, so the point is inside the character
                // rather than exactly on a boundary either side.
                let inside = x + l.cell.advance * 0.1;
                let got = preview_char_at(&l, &lines, 0.0, inside, y, &mut measure)
                    .expect("inside the preview");
                assert_eq!(got.line, index, "row {index}, character {want}");
                assert_eq!(
                    got.char, want,
                    "row {index}: the point where character {want} starts came back as {}",
                    got.char
                );
            }
            row_y += md.rows() as f32 * l.cell.line_height;
        }
    }

    #[test]
    fn a_drag_below_the_last_line_lands_at_its_end() {
        let Ok(r) = Renderer::offscreen(900, 400, 1.0, 16.0) else {
            return;
        };
        let ws = Workspace::empty(None);
        let l = layout(&r, &ws, Chrome::default());
        let lines = vec![line(0, "one"), line(0, "two")];
        let mut measure = crate::Measured { renderer: &r };

        let below = l.text_y + l.text_h - 1.0;
        let got = preview_char_at(&l, &lines, 0.0, l.width / 2.0, below, &mut measure);
        assert_eq!(
            got,
            Some(at(1, 3)),
            "a drag off the bottom means to the end, not nowhere"
        );
    }

    #[test]
    fn a_drag_past_the_end_of_a_line_stops_at_it() {
        let l = line(0, "hi");
        assert_eq!(
            selected_chars(&l, 2, at(2, 0), at(2, 99)),
            Some((0, 2)),
            "the selection cannot run past the characters there are"
        );
    }
}
