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

macOS and Windows, from the same source: Metal or DX12 under wgpu, the
platform's own menu bar, its own file dialogs, and its own idea of which
key the shortcuts hang off.

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

### As a Windows app

    .\scripts\bundle.ps1            # -> target\release\lithe.exe
    .\scripts\bundle.ps1 -Install   # -> %LOCALAPPDATA%\Programs\lithe,
                                    #    with a Start menu shortcut

Windows has no bundle format — an application is one .exe — so what makes
it an app rather than a loose binary is compiled into it: the icon Explorer
and the taskbar draw, and the version information Properties ▸ Details
shows. `crates/app/build.rs` does that from `assets/lithe.ico`, which
`scripts/make-icon.py --ico` draws, which is why the script draws it before
starting cargo. Both are optional: a machine with no Python or no resource
compiler builds the same editor with the generic icon on it.

It is a windowed binary, so opening it from Explorer or the Start menu
gives you the editor and nothing else — a console program that opens a
window leaves a black box behind it for as long as it runs. Started from a
terminal it attaches to that terminal, so `LITHE_TRACE=1` still logs and
`--snapshot` still prints; a shell does not wait for a windowed process, so
the prompt comes back while the editor is still open.

**The modifier is ctrl rather than cmd.** Every shortcut below reads the
same with one swapped for the other — `ctrl+P`, `ctrl+shift+P`,
`ctrl+shift+G` — and the hints the editor draws say `ctrl` when it is the
one running. `ctrl+space` and `ctrl+Tab` mean what they mean everywhere.

The menu bar is the row under the title bar rather than the one at the top
of the screen, with Settings and Exit at the foot of File and About under
Help, which is where Windows keeps them. It is there to be read and
clicked: the editor runs the shortcuts itself, because a Win32 accelerator
fires only if the message loop translates it and winit's does not.

Two entries are named differently because they do something different.
**Reveal in Explorer** is the obvious one. **Move to Trash** puts the file
in `%APPDATA%\lithe\trash` rather than the Recycle Bin: the shell will not
say where it put a file it recycled, and the file tree's Undo is built on
knowing exactly that. Settings and the recent-projects list are in that
same `%APPDATA%\lithe\`.

Language servers are looked up the way a shell looks them up, `PATHEXT`
and all, so an npm-installed `typescript-language-server.cmd` is a server
like any other — `CreateProcess` would only ever have tried `.exe`. None
of them, and no `git`, flashes a console window of its own.

Launched with no argument, lithe opens on a landing page: New buffer, Open
file, Open folder, New window, and the projects you opened recently. Every
row is clickable.

There is a native menu bar — File, Edit, Go, View — so the shortcuts below
are discoverable rather than folklore. Open File, Open Folder and Save As
use the system dialogs.

The table is written in the Mac idiom. On Windows and Linux the modifier
is `ctrl`: read every `cmd` below as `ctrl`, which is also what the editor
itself prints there.

| | |
|---|---|
| window | `cmd+shift+N` new window · opens empty, on the landing page · each has its own project, buffers and tree |
| file | `cmd+N` new buffer · File ▸ New File… creates one on disk · `cmd+O` open · `cmd+shift+O` folder · `cmd+S` save · `cmd+shift+S` save as |
| tabs | click to switch · click the `x` to close · `cmd+W` close · `ctrl+Tab` cycle |
| project | `cmd+P` fuzzy open · `cmd+B` file tree |
| search | `cmd+F` find · `cmd+alt+F` replace · `cmd+shift+F` in project |
| code | **cmd-click** or `F12` definition · `cmd+I` hover · `ctrl+Space` complete |
| settings | `cmd+,` |
| view | `cmd+shift+V` preview · drag the sidebar edge to resize |
| tabs | files, diffs and Settings are all tabs |
| select | double-click a word · triple-click a line |
| scroll | wheel · `shift`+wheel or a horizontal wheel for sideways · **middle-button drag** to pan · drag either scrollbar |
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
Terminal. (Reveal in **Explorer** on Windows, which is the same entry
naming the same thing.)

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

### The preview is a document, not a dump of the file

**Prose is set in a proportional face**, which is the whole difference
between a preview and a terminal showing Markdown. The editor is one
monospace face by design — a uniform grid is what lets a screenful cost
one instance per cell — and the preview is the one view that is not a
grid, so it gets the platform's own text face: Segoe UI, SF Pro,
whichever of them is installed. Emphasis is *slanted* rather than
coloured, bold is heavier, and body text sits a shade below the editor's
own contrast, because a page of prose at full contrast is a page nobody
finishes.

**Everything whose meaning is its alignment stays on the grid.** Code,
tables and drawn diagrams are still monospace, still measured in cells,
and so are the bullets and checkboxes that line up down the margin. Set a
box-drawing frame proportionally and it comes apart in your hands.

**The page is the pane, less its margins.** The margin is a share of the
window rather than a fixed inset, so the page looks like a page at every
size, and the column takes what is left — capped, because a line that runs
the full width of a maximised window is one the eye loses its way back to
the start of. The cap is measured in alphabets, which is how type is
measured: the lowercase a–z of a face set solid is about thirteen ems, and
four of those is the hundred-odd characters a line reads comfortably at in
a window. Past the width where that still leaves a sensible margin the
column keeps growing with the pane, because a document floating in the
middle of a screenful of air is the other way to get this wrong.

Everything in the document shares that column — code set wider than the
prose beside it reads as a different page — with one exception: a drawn
diagram is a figure, so it may use the whole page inside the margins. A
flowchart refused for want of a few cells is shown as its own source,
which is worse than a figure wider than the text above it.

The layout asks the renderer how wide a run is rather than counting its
characters, and the answer comes from the same glyph advances the
renderer then draws with. A line cannot wrap in one place and be drawn in
another, because there is only one number. Measuring a document that way
costs what it sounds like it costs, so printable ASCII is summed from a
per-face table rather than hashed a character at a time: this README, 36
KB of it, lays out across a maximised window in **2.2 ms** — about what
the hand-rolled monospace reader took to do something worse.

### What the Markdown preview understands

**CommonMark, as pulldown-cmark parses it**, plus the GitHub extensions a
README actually uses: pipe tables, `~~struck~~`, `- [x]` checkboxes and
footnotes. Headings of both kinds with the top two levels drawn larger
than body text, paragraphs, hard breaks, `**bold**`, `*em*`, `***both***`,
`` `code` ``, links of all three kinds — inline, reference and bare URL —
images named by their alt text, nested and ordered lists tight or loose,
block quotes, rules, indented as well as fenced code, YAML front matter
(hidden, as a renderer hides it), backslash escapes, entity references,
and the inline HTML a README actually contains.

The parser is a dependency rather than ours on purpose. CommonMark is a
standard with a long tail — a reference link defined after its use, lazy
continuation, what counts as emphasis inside a word — and none of that
tail is this editor's product. What *is* ours is everything after the
events: which runs are bold, how a table fits in a column of characters,
where a heading earns a second grid row. Half the module went when the
parser arrived, and a 32 KB README now lays out in about a millisecond,
which is less than half what the hand-rolled one took.

Fenced code is highlighted by the same tree-sitter grammars the editor
uses, picked from the language on the fence. A fence naming a language
nothing is installed for stays plain rather than being coloured by a
guess, and an indented block names nothing, so it stays plain too.

**Click a fenced block to copy it.** A drawn preview has no selection to
drag, so the block is the unit: click anywhere in one and the source
goes to the clipboard, as written rather than as rendered. The first row
of each block says `copy`, so it is discoverable that it can be.

**Links are followed.** The pointer turns to a hand over one, and a click
does what the link says. A url — inline, reference, bare, or an `<a href>`
written in HTML — opens in whatever the system opens urls with, handed
over as one argument rather than through a shell, because a query string
holds `&`. A `#fragment` scrolls to the heading with that anchor, matched
on the slug the way every other renderer of the same file matches it.
Anything else is a path beside the file being read, which is how a README
points at the rest of its own repository: it opens in the editor, with
`%20` unescaped and `docs/guide.md#writing` landing on the heading in the
file it just opened. A badge is an image inside a link, so clicking it
means the link rather than the picture; an image on its own means the
image. A link to a folder is shown in Explorer rather than opened as a
project, because replacing the project on a click would be a surprise.

**A `mermaid` fence is drawn as a diagram**, in box-drawing characters
on the same grid as everything else. Two kinds, which is what a
project's documentation actually contains:

- **Flowcharts** — `flowchart`/`graph` with `TB`, `TD`, `BT`, `LR` or
  `RL`, node shapes, `subgraph` groups drawn as labelled frames, `A & B
  --> C` for every edge between two sides, and labelled edges in either
  `-->|label|` or `-- label -->` form. A step is square and everything
  else is rounded: a monospace grid can honestly express two outlines,
  and a rhombus made of box characters reads as a mistake.
- **Sequence diagrams** — participants across the top, lifelines down,
  messages as arrows between them, replies dashed, and a message to
  itself turning back on its own lifeline.

`style`, `classDef`, `class`, `click`, `linkStyle` and a `direction`
inside a subgraph are read and dropped. They are presentation: this draws
in box characters on a monochrome grid, so there is no fill to honour and
no node to click, and refusing them threw away the structure as well —
which is what made real charts in real READMEs come out as their own
source.

Anything that names an edge and cannot be read is still refused, and so
is anything else: a pie chart, a gantt chart, an ER diagram, `loop`/`alt`
blocks, or a chart too wide for the page it has. A diagram drawn wrong is
worse than the fence it came from — the source at least says what was
meant — but a diagram missing an arrow says something false, which is
worse than both.

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

**Launched from Finder, the app widens its own `PATH` first.** macOS
gives a GUI application launchd's `PATH` — four system directories —
not the shell's, so a server installed into `~/.cargo/bin` or
`~/go/bin` would be invisible and reported missing. lithe asks the
login shell what `PATH` should be, and falls back to the usual places
if that is slow or absent. Started from a terminal it does nothing.

**rust-analyzer loads before it answers.** It is the one server that
does: clangd and gopls reply the moment they start, while rust-analyzer
reads a Cargo workspace and indexes it first, answering nothing until
it has. The status bar says what it is busy with, and a question asked
too early reports that rather than "nothing found". On a large project
the first few seconds after opening are a wait, not a failure.

**A server that asks the editor something is answered.** The protocol
runs both ways, and a server left waiting on a reply can simply stop:
gopls asks the editor to create a progress indicator before it will say
another word, so an unanswered question meant no diagnostics, no hover
and no go-to-definition for the whole session — in silence, which is the
worst way for it to fail. The answers are mechanical ("go ahead", "no
settings of ours", "that edit is declined") and are sent from the reading
thread, which is the only place that cannot be busy when they arrive.

**What a server says it cannot do is not offered.** Each one announces
the requests it answers, and the right-click menu is built from that:
neither `gopls` nor `rust-analyzer` implements `textDocument/declaration`
— in Go and Rust a thing is declared where it is defined — so **Go to
Declaration** is not in the menu for those languages, where it could only
ever have reported "no declaration found". `clangd` does implement it,
because C makes the distinction, and there it is offered. Asked by
keyboard rather than by menu, the answer names the server: *gopls does
not answer declarations*.

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

It opens in that file's own tab rather than in a second one: the diff and
the buffer are one thing, so switching away and back finds it where you
left it. Clicking the file in Changes again puts the plain editor back;
closing the tab closes both.

That one tab is in two modes, so it says which it is in two ways: a `±`
before the name, and the name itself in the colour the gutter marks a
modified line with. A tab is read at a glance, and a colour is what
reaches a glance.

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
**history** lists recent commits — hash, who made it, and what they said —
and clicking one shows what it changed. **View File History** on a file
narrows that tab to the commits that touched it — `--follow`, so a rename
does not cut it short. Clicking the History tab widens it back to the
project.

The name takes a share of the row rather than a column of its own, and
steps aside entirely when the sidebar is too narrow to hold a name and a
summary at once; drag the divider to give it room. A name that does not
fit is cut at a word — "Ahmad Masud" in eight columns is "Ahmad", not
"Ahmad M…", because a given name is a name where half a surname is a
typo.

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
fences, inline code and links, laid out as a page. Structure is carried by
face, weight, slant and size — the glyph atlas holds each of them keyed by
the same cache, rasterised on first use, so a document that never uses
italics never pays for one. Links are live: the pointer turns to a hand
over one, and a click opens the url, jumps to the heading, or opens the
file beside this one.

Scrolling is fractional rather than line-by-line, because a trackpad sends
pixel deltas and rounding each one to a whole line is what makes scrolling
feel like it is catching. The scrollbar is draggable — the one belonging to
whatever the text area is showing, so it drags a rendered preview and a
diff as readily as a file — and the pointer is an I-beam over text.

**Sideways, too.** A line longer than the window is reached with a
horizontal wheel, with `shift` and a vertical one, by dragging the bar that
appears along the bottom when there is something out there to reach, or by
holding the **middle button** and dragging: the document follows the
pointer, both ways at once. Typing off the right edge brings the view along
by itself.

The limit on how far right you can scroll is the longest line *in view*,
not the longest in the file. Finding that one means reading every line,
which is the one thing this editor does not do — so the limit moves as you
scroll, which is also the honest answer: there is nothing to the right of
what is in front of you. The same goes for the bar's own length.

Not the auto-scroll that Windows does on the same button, where the view
keeps moving while the pointer sits still. That needs a clock ticking
frames out, and this editor redraws on damage and nothing else.

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

Four parsers, none of them ours: tree-sitter for code, ripgrep's engine
for search, gitoxide for reading a repository, and pulldown-cmark for the
preview. Each one is a standard with a long tail that is nobody's
product, and each is wrapped in a crate here that decides what to do with
what it says.

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
projects' files in one tree is never what was meant. A new window opens
empty, on the landing page: a second window is where you go to work on
something else, and the page is one click from any project you have had
open — including the one you just came from.

**Coming back to a window re-reads the disk.** Anything that changes a
project from outside the editor — a `git pull`, a branch switch, a build
— happens while that window is not the one being typed into, and you find
out by coming back to it. So that is when the file tree is rebuilt and the
gutter's idea of HEAD is dropped. It is what a filesystem watcher would be
for, without the thread, the dependency, or the queue of events arriving
while nobody is looking; the tree reads only the directories that are
expanded, so it costs what drawing it costs.

Recent projects live in one plain text file, one absolute path per line —
`~/Library/Application Support/lithe/recent-projects` on macOS,
`%APPDATA%\lithe\` on Windows, `$XDG_CONFIG_HOME/lithe/` elsewhere. No
database, nothing to migrate, and you can edit it in the editor. Writing it re-reads first and merges, since
several windows each hold their own copy and a stale write would drop
whatever another added.

## Testing the renderer

A hand-rolled text renderer fails in ways a type checker cannot see, so the
renderer can draw to an offscreen texture and the tests read the pixels
back:

    cargo test --release                        # 395 tests, incl. pixel, live-server and real-repo tests
    LITHE_TRACE=1 ./target/release/lithe .      # log every window event
    cargo run --release -- --snapshot out.raw file.rs  # one frame, raw RGBA
    cargo run --release -- --snapshot out.raw --picker .   # with the overlay up

The render tests skip rather than fail where there is no GPU. Three of the
379 are about the `PATH` a GUI application is launched with, which is a
Unix question, and do not run on Windows.

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
