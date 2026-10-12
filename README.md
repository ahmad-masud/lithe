# lithe

A bare-bones code editor in Rust. Local only: no account, no telemetry, no
network, no LLM. Code editing, LSP, search, version control, and nothing
else.

Rendered with a hand-rolled wgpu text renderer rather than a UI toolkit,
because a code editor is a character grid and that fact is worth exploiting.

## Status — all seven milestones done

A working editor: projects, tabs, fuzzy open, syntax highlighting in nine
languages, find and replace, project-wide search, language servers, and
git — gutter, staging, commit and blame.

Languages: Rust, TypeScript, TSX, JavaScript, Python, Go, C, JSON, TOML.
Adding one is a dependency and a row in `crates/syntax/src/languages.rs`.

    cargo run --release -- path/to/file     # open a file
    cargo run --release -- path/to/project  # open a folder

### As a Mac app

    ./scripts/bundle.sh          # -> target/lithe.app
    open target/lithe.app

A bare binary run from a terminal is only half an app to macOS: the menu bar
is unreliable, the Dock shows the executable name, and there is no icon. The
bundle adds an `Info.plist`, a generated icon and an ad-hoc signature, which
is what makes the native menu behave. Drag it to /Applications if you want
it to stay.

The menu bar belongs to whichever app is frontmost, so click the lithe
window to see **lithe · File · Edit · Go · View** rather than your
terminal's menus.

Launched with no argument, lithe opens on a landing page: New buffer, Open
file, Open folder, New window, and the projects you opened recently. Every
row is clickable.

There is a native menu bar — File, Edit, Go, View — so the shortcuts below
are discoverable rather than folklore. Open File, Open Folder and Save As
use the system dialogs.

| | |
|---|---|
| window | `cmd+shift+N` new window · each has its own project, buffers and tree |
| file | `cmd+N` new buffer · File ▸ New File… creates one on disk · `cmd+O` open · `cmd+shift+O` folder · `cmd+S` save · `cmd+shift+S` save as |
| tabs | click to switch · click the `x` to close · `cmd+W` close · `ctrl+Tab` cycle |
| project | `cmd+P` fuzzy open · `cmd+B` file tree |
| search | `cmd+F` find · `cmd+alt+F` replace · `cmd+shift+F` in project |
| code | **cmd-click** or `F12` definition · `cmd+I` hover · `ctrl+Space` complete |
| settings | `cmd+,` |
| view | `cmd+shift+V` preview · drag the sidebar edge to resize |
| tabs | files, diffs and Settings are all tabs |
| select | double-click a word · triple-click a line |
| git | `cmd+shift+G` source control · `cmd+shift+A` stage · `cmd+shift+C` commit · `cmd+shift+B` line blame |
| menu keys | `↑`/`↓` move · `Enter` choose · `Esc` or any click dismisses |
| commands | `cmd+shift+P` |
| edit | type, `Enter`, `Backspace`/`Delete`, `Tab` / `shift+Tab` |
| move | arrows, `alt+`arrows (word), `cmd+`arrows (line/doc), `Home`/`End`, `PageUp`/`PageDown` |
| select | `shift` + any motion, `cmd+A`, click and drag |
| multi-cursor | `cmd+D` next match · `cmd+shift+L` every match · `cmd+alt+`↑`/`↓ cursor above/below · `alt+click` · `Esc` collapses |
| lines | `cmd+L` select · `cmd+shift+K` delete · `shift+alt+`↓ duplicate · `alt+`↑`/`↓ move · `cmd+Enter` / `cmd+shift+Enter` open below/above |
| code | `cmd+/` comment · `cmd+]` / `cmd+[` indent |
| clipboard | `cmd+C` / `cmd+X` / `cmd+V` |
| history | `cmd+Z`, `cmd+shift+Z` |
| quit | `Esc` on a bare cursor |
| menus | **right-click** the text, a tab or the file tree |

## Right-click

Two menus, built per click from what is actually actionable — an entry
that would quietly do nothing is not offered.

**In the text, or on a tab:** Go to Definition / Declaration / Type
Definition / Implementation, Find All References, Show Incoming Calls,
Show Outgoing Calls, Rename Symbol, Format Buffer, Format Selections,
Show Code Actions, Cut, Copy, Copy and Trim, Paste, Blame Line, View File
History, Copy Permalink to Line, Open Preview, Reveal in Finder, Open in
Terminal.

The language-server entries need a server for the file's language; the
git ones need the project to be a repository. **Copy Permalink to Line**
builds a link to the current commit on GitHub, GitLab or Bitbucket, and
says it cannot rather than guessing at an unknown host.

**Find All References** and the two call lists fill the search panel, so
they scroll and click through the same way a project search does.

**Rename Symbol** asks for the new name, then applies the server's
workspace edit — to open buffers as an undoable change, and to closed
files directly on disk.

## Preview, and the way back

Markdown, SVG and images each open either rendered or as their source,
and each remembers which you chose. The eye in the status bar switches
the file you are looking at; `cmd+shift+V` does the same, and Settings
(`cmd+,`) holds the three defaults.

SVG has its own switch rather than sharing the images one, because it is
the one picture format that is also a text file worth editing: turning
its preview off shows the markup, while doing the same to a PNG would
show an empty buffer. Markdown defaults to its source; SVG and images
default to the picture.

HTML has no preview. It had one, and it was never what anyone wanted: a
reader that understands the structural tags but no stylesheet, script or
layout shows you a document nothing on the web would render that way.
Inline HTML inside Markdown is still read, because there it is part of
the Markdown.

### What the Markdown preview understands

Headings (`#` and the underlined kind, with the top two levels drawn
larger than body text), paragraphs, hard breaks, `**bold**`, `*em*`,
`***both***`, `~~struck~~`, `` `code` ``, links of all three kinds —
inline, reference and bare URL — images named by their alt text, nested
and ordered lists, `- [x]` checkboxes, block quotes, rules, pipe tables
laid out as an aligned grid, YAML front matter (hidden, as a renderer
hides it), backslash escapes, and the inline HTML a README actually
contains.

Fenced code is highlighted by the same tree-sitter grammars the editor
uses, picked from the language on the fence. A fence naming a language
nothing is installed for stays plain rather than being coloured by a
guess.

**Click a fenced block to copy it.** A drawn preview has no selection to
drag, so the block is the unit: click anywhere in one and the source
goes to the clipboard, as written rather than as rendered. The first row
of each block says `copy`, so it is discoverable that it can be.

**A `mermaid` fence is drawn as a diagram**, in box-drawing characters
on the same grid as everything else. Two kinds, which is what a
project's documentation actually contains:

- **Flowcharts** — `flowchart`/`graph` with `TB`, `TD`, `BT`, `LR` or
  `RL`, node shapes, `subgraph` groups drawn as labelled frames, and
  labelled edges in either `-->|label|` or `-- label -->` form. A step
  is square and everything else is rounded: a monospace grid can
  honestly express two outlines, and a rhombus made of box characters
  reads as a mistake.
- **Sequence diagrams** — participants across the top, lifelines down,
  messages as arrows between them, replies dashed, and a message to
  itself turning back on its own lifeline.

Anything else — a pie chart, a gantt chart, an ER diagram, `loop`/`alt`
blocks, `style` directives, or a chart too wide for the column — stays
a code block. A diagram drawn wrong is worse than the fence it came
from: the source at least says what was meant.

Two limits worth knowing. A subgraph frame is dropped, and the nodes
drawn without it, when the group's nodes did not land in a clean
rectangle — a frame that encloses something it does not own says
something false. And a quoted label's `\n` becomes a space, because one
row per box is what keeps the layout arithmetic honest.

Tables honour their alignment row: `:---` left, `:--:` centred, `---:`
right. They are drawn inside a frame, and a column too wide for the
window is narrowed from the widest one first, so the narrow columns stay
readable instead of every column being trimmed into uselessness.

Two boundaries worth stating. An unknown tag is left as written rather
than dropped, because `Vec<T>` in prose is commoner in a code editor's
README than hand-written HTML. And an image is named, not drawn: a
picture inside flowed text would mean laying out rows of unequal height
around it, and for the badges at the top of a README the name is all
there was to say.


## Suggestions and hovers

A completion list opens by itself: on a character the server names as a
trigger — `.` everywhere, plus `>` and `:` in C and Rust, which is how
`->` and `::` arrive — and after two characters of a word. `ctrl+space`
still asks at any point.

One request per word, not one per keystroke. Once the list is open,
further typing narrows what it already has, which costs nothing and
needs no round trip. The list is filtered by the word already typed, and
accepting one replaces that word rather than following it.

Resting the pointer over a name for 400ms asks what it is, and the
answer is drawn as a small document — a server sends Markdown, so the
preview's reader renders it, and a signature arrives as a highlighted
code block. `cmd+I` asks about the caret instead. Nothing is asked
unless a server for that language is already running: resting a pointer
must not start a process.

## Starting a repository

Open Source Control (`cmd+shift+G`) somewhere that is not a repository
and it offers **Initialize Repository** rather than an empty list of
changes — an empty list reads as "nothing has changed", which is not
what is going on. It shells out to `git init`, so the result is the
repository git would have made: its hooks, its default branch name, its
templates.

## Line blame

`cmd+shift+B` (Git ▸ Toggle Line Blame, or the right-click menu) turns on
an annotation at the end of the line the cursor is on: who last changed
it, when, and the commit summary. It follows the cursor.

One `git blame` per file, not per line — the annotation moves with the
caret, and spawning a subprocess every time you press Down is not
something this editor is allowed to do. Commits are stored once and
referenced per line, so the table for a ten-thousand-line file is forty
kilobytes rather than ten thousand strings.

A modified buffer says `unsaved changes` instead. `git blame` reads the
file on disk; once the buffer and the disk differ, its line numbers are
about different lines, and a confident wrong name is worse than none.

## Diffs you can type in

A diff opens side by side by default — two columns is what a diff looks
like everywhere else, and the interleaved form is there in Settings for
when the window is too narrow for two.

`settings.toml` records only what you have actually changed. Writing
every value would freeze the defaults at the moment the file was first
written: toggling one preference would record all the others as though
they had been decided, and changing a default afterwards could never
reach anyone who had the file.

A diff opened from **Changes** is of the file in front of you, so its
right-hand column *is* that buffer: click in it to put the caret there,
type, and the diff rebuilds as you go. The new side is read from the
buffer rather than from disk, so an edit shows up as it is typed instead
of catching up at the next save.

It opens in that file's own tab, marked `±`, rather than in a second
one: the diff and the buffer are one thing, so switching away and back
finds it where you left it. Clicking the file in Changes again puts the
plain editor back; closing the tab closes both.

A diff opened from **History** is of a commit. There is no buffer behind
it, so it says `read only` and means it, and clicking through a history
reuses one tab rather than leaving one per commit.

## Tabs

A tab is not always a file. Settings (`cmd+,`) and diffs are things you
open, look at and close, which is what a tab is for — as modes drawn
over the editor they hid the file you were working on, and the only way
back was the shortcut that opened them. They close like any other tab,
and there is only ever one settings tab however many times you ask.

**In the file tree:** New File, New Folder, Reveal in Finder, Open in
Default App, Open in Terminal, Cut, Copy, Duplicate, Paste, Undo, Redo,
Copy Path, Copy Relative Path, Rename, Move to Trash, Delete.

New File, New Folder and Rename are typed **in place**, on a row in the
tree where the result will appear — `Enter` accepts, `Esc` cancels. A
name is a name: one containing a path separator is refused rather than
silently writing somewhere else, and an existing file is never
overwritten.

Right-clicking empty space in the tree acts on the project root.

Cut, Copy and Paste move and copy files; the path also goes on the system
clipboard so it is not a black hole. Undo and Redo take back what the
menu did to the filesystem — a create, a move, a trash. **Move to Trash**
is recoverable and is what Undo reverses; **Delete** is not, so it is the
one entry that asks first.

The source control panel has two tabs. **changes** stages and unstages;
**history** lists recent commits, and clicking one shows what it changed.
**View File History** on a file narrows that tab to the commits that
touched it — `--follow`, so a rename does not cut it short. Clicking the
History tab widens it back to the project.

Clicking a file in the source control panel shows its **diff** against
HEAD — unified, with both sides' line numbers and long unchanged runs
elided. Clicking the same file again opens it for editing.

**Autosave** (File ▸ Toggle Autosave) writes a modified buffer back after
a pause in typing, and when the window loses focus. Only buffers that have
a path: inventing a filename for a scratch buffer would be worse than
leaving it unsaved. It goes through the same temp-file-and-rename path a
manual save does.

Toggling the Markdown preview is remembered, so `.md` files open the way
you last chose. Preferences live in `settings.toml` beside the recent
projects; a malformed one falls back to defaults rather than refusing to
start.

The status bar's left end is a row of toggles — **files**, **git**,
**search**, and **preview** for Markdown — lit when their panel is open.
They stay clickable on the landing page, which is when you are most likely
to reach for one.

`cmd+shift+G` swaps the sidebar from the file tree to source control:
staged and unstaged changes with git's own status letters, `+` and `−` to
stage and unstage, and a click to open the file. `git status` runs when
something could have changed it, not every frame.

Clicking a file in the tree opens it in a **preview** tab, drawn dimmer:
the next single click reuses that slot, so browsing twenty files leaves one
tab rather than twenty. Double-clicking it, or typing in it, makes it
permanent.

`cmd+,` opens a settings page. Each previewable kind — Markdown, HTML,
images and SVG — has its own switch, because turning off the Markdown
preview should not stop images opening as pictures. Diffs can be shown
side by side or interleaved.

`cmd+shift+V` previews **Markdown and HTML**, and images (PNG, JPEG, GIF,
BMP, WebP, and SVG rasterised through the same path) open as pictures
rather than as mojibake — scaled to fit,
captioned with their real size. A file that is not text opens as an image
or as a note saying so, and is never written back over.

A Markdown file previews with `cmd+shift+V`: headings, lists, quotes,
fences, inline code and links, wrapped to the window. There is one font at
one size, so structure is carried by weight and colour — the glyph atlas
holds a bold face keyed by weight alongside the regular one, rasterised on
first use.

Scrolling is fractional rather than line-by-line, because a trackpad sends
pixel deltas and rounding each one to a whole line is what makes scrolling
feel like it is catching. The scrollbar is draggable, and the pointer is an
I-beam over text.

Saving goes through a temp file and a rename, so an interrupted write cannot
truncate your code. Closing a buffer, a window or the app with unsaved edits
asks first, in a native dialog.

## Measured, not asserted

macOS 27, Apple M5, release build:

| | |
|---|---|
| idle CPU, window open | **0.000%** (CPU-time delta over 14s) |
| footprint, project + tree open | 111 MB RSS (94 MB before the native menu) |
| a second window | **+5 MB** — shares the GPU device, pipeline and glyph atlas |
| footprint, source file + highlighting | 42 MB private / 97 MB RSS |
| footprint, 143 MB file | 230 MB private / 259 MB RSS |
| open a 143 MB / 3M-line file | **0.05 s** |
| document work per frame | **13 us**, independent of file size |
| walk a 7,000-file project | **0.11 s**, 4.8 MB of paths |
| fuzzy keystroke, 200,000 paths | **11.8 ms** worst case, 42.7 MB of paths |
| keystroke reparse, 14,000-line file | **0.26 ms** mean, 0.70 ms worst |
| project search | ripgrep's engine, streamed, cancelled on retype |
| highlight query per frame | **97 us**, bounded by the viewport |
| parse tree | ~26x the source it describes |
| stripped binary | 12 MB (9 tree-sitter grammars are ~6 MB of it) |

The two memory figures differ because `ps` RSS counts shared AppKit pages
that every Mac app maps; `top`'s real-memory column is closer to what this
process actually costs the machine. Either way the baseline is mostly not
ours:

    63 MB   AppKit/Cocoa, before a line of our code runs
    10 MB   + window creation
     3 MB   + wgpu instance, adapter, device, surface, shader
    11 MB   + font system, swash cache, 94 rasterised ASCII glyphs

So the floor is the platform's. The same editor as a terminal UI would idle
near 10 MB; a native window costs ~80 MB to exist on macOS and there is no
way around it from inside the process. For scale, Electron-based editors
idle between 500 MB and 1 GB.

A document costs about **1.15x its byte size** in rope. Only the viewport is
*shaped* — it is not the case that only the viewport is *loaded*.

## Design

Three invariants, in `docs/ARCHITECTURE.md`, that every feature has to
respect:

1. **Redraw only on damage.** `ControlFlow::Wait`, no animation clock.
2. **Lay out only what is visible.** Cost scales with the window, not the
   file.
3. **No index of our own.** Symbols come from the language server, text
   search streams through ripgrep's engine. The fuzzy finder keeps *paths*
   and never file contents, and only builds that list when you first press
   `cmd+P`. This is the main reason mainstream editors idle at a gigabyte
   and this one does not.

Syntax follows the same shape. Parsing is incremental — an edit is described
to the tree before the reparse, so a keystroke costs a fraction of a
millisecond instead of re-parsing the file — and the highlight query is
restricted to the visible byte range, so colouring a three-million-line file
costs what colouring a short one does. The parser reads the rope in chunks
and never flattens it into a `String`.

Two honest costs. A parse tree is about 26x the source it describes, so
highlighting is switched off above 2 MB; past that a file still opens
instantly, just uncoloured. And undo discards the tree and reparses rather
than deriving inverse edits, which is ~16 ms on a 14,000-line file — fine
for a keypress, visible if you hold the key down.

The renderer exposes exactly two primitives, `rect` and `text`. The editor
body, gutter, file tree, command palette, search results and status bar are
all the same two calls at different coordinates — one instanced draw per
frame against one glyph atlas. There is no widget tree.

## Layout

    crates/core    lithe-core    rope, cursors, edits, undo — no I/O, no GPU
    crates/gpu     lithe-gpu     surface, glyph atlas, rect + text
    crates/syntax  lithe-syntax  tree-sitter, languages, highlight spans
    crates/app     lithe         editor, workspace, tree, picker, menu, view

    crates/search  lithe-search  ripgrep's engine, in-buffer and project
    crates/lsp     lithe-lsp     JSON-RPC client, server lifecycle
    crates/vcs     lithe-vcs     gitoxide: HEAD blobs, line diff

Search uses ripgrep's own `grep-regex` and `grep-searcher`, so in-buffer
find means exactly what project search means — one pattern syntax, one set
of case rules. Project search streams: hits reach the panel as they are
found, the previous worker is cancelled when you retype, and the result set
is capped, because a result list is the closest thing to an index we keep.

Language servers run one process per language, started on the first buffer
that needs it and killed on exit. A candidate that spawns and then exits
within 250 ms is treated as broken and the next one is tried — being on
`PATH` is not enough, because `rust-analyzer` there is often a rustup proxy
for a component that was never installed. The status bar says which state
the file is in: `lsp`, `lsp starting`, `lsp missing` or `no lsp`, and
asking a missing server for something names the command that installs it. Whole-document sync, coalesced to one
send per frame: incremental sync would send less but has to agree with the
server about UTF-16 ranges on every edit, and a single disagreement
desynchronises it silently for the rest of the session. LSP counts columns
in UTF-16 code units, which is tested round-trip through emoji and CJK.

Git splits by direction. **Reads** are gitoxide: the gutter runs on every
frame and must never stall on a subprocess, and the diff against HEAD is
recomputed only when the file or its revision changes. **Writes** —
staging, committing — shell out to `git`. gix 0.88 exposes `commit()` but
no way to turn an index into a tree, so committing through it would mean
building tree objects and moving refs by hand; a bug there costs someone
their work, where a bug in the gutter costs a redraw. These are one-off
actions a person asks for, `git` is the reference implementation, and the
tests check the result by asking `git log` rather than asking ourselves.

Windows are independent: each owns its project root, open buffers and file
tree, so a second window can sit on a different repository. They share one
wgpu device and one rasterised glyph atlas, which is why a second window
costs five megabytes rather than twenty — the font system behind the atlas
is eight on its own.

Opening a folder replaces the project rather than merging into it. Two
projects' files in one tree is never what was meant.

Recent projects live in one plain text file, one absolute path per line —
`~/Library/Application Support/lithe/recent-projects` on macOS,
`$XDG_CONFIG_HOME/lithe/` elsewhere. No database, nothing to migrate, and
you can edit it in the editor. Writing it re-reads first and merges, since
several windows each hold their own copy and a stale write would drop
whatever another added.

## Testing the renderer

A hand-rolled text renderer fails in ways a type checker cannot see, so the
renderer can draw to an offscreen texture and the tests read the pixels
back:

    cargo test --release                        # 201 tests, incl. pixel, live-server and real-repo tests
    LITHE_TRACE=1 ./target/release/lithe .      # log every window event
    cargo run --release -- --snapshot out.raw file.rs  # one frame, raw RGBA
    cargo run --release -- --snapshot out.raw --picker .   # with the overlay up

The render tests skip rather than fail where there is no GPU.

## Roadmap

| | | |
|---|---|---|
| M1 | the frame | window, atlas, viewport — **done** |
| M2 | the editor | edits, multi-cursor, undo, save — **done** |
| M4 | files | buffers, tree, fuzzy open, palette — **done** |
| M3 | syntax | tree-sitter, 9 languages, theme — **done** |
| M5 | search | find/replace, project search — **done** |
| M6 | lsp | diagnostics, completion, hover, goto-def — **done** |
| M7 | vcs | gutter, branch, stage, commit, blame — **done** |
