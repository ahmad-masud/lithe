# lithe — architecture

A local-only code editor. No account, no telemetry, no LLM, no cloud.
Code editing, LSP, search, version control. Nothing else.

## Non-negotiable performance invariants

These are the reason the project exists. Every feature must respect them,
and a feature that cannot is cut rather than accommodated.

1. **Redraw only on damage.** The event loop sits in `ControlFlow::Wait`.
   There is no animation ticker and no continuous render loop. An idle
   window costs 0% CPU. If a feature needs a steady frame clock, it is the
   feature that is wrong.

   One timer exists, and it is bounded: the caret blinks for ten seconds
   after the last keystroke and then goes solid, so the editor blinks while
   you work and wakes for nothing once you stop. Measured at 0.000% CPU
   once it settles. Anything else wanting a clock gets the same treatment —
   bounded, self-cancelling, and justified here.

   Measuring this is harder than it sounds, and two different mistakes
   both look exactly like a broken invariant:

   - `top` makes the window it samples redraw, so it reports one to three
     percent of its own making. Use a CPU-time delta (`ps -o time=`) across
     a quiet window instead.
   - A pointer resting over a focused window still delivers events. Seven
     percent CPU with the mouse drifting is correct behaviour, not a spin.

   So measure CPU *and* the event count over the same window:
   `LITHE_TRACE=1` logs every window event. Zero events with non-zero CPU
   is a real bug; events with CPU is the editor doing its job. Idle and
   frontmost, with the pointer still, the correct reading is zero of both.

2. **Lay out only what is visible.** Shaping and drawing are proportional
   to the viewport, never to the document. Measured: pulling the ~60 lines a
   frame needs out of the rope costs 13 us whether the file holds 60 lines
   or three million.

   What this does *not* buy, and an earlier draft of this file wrongly
   claimed it did: the document is fully resident, at about 1.15x its byte
   size. A 143 MB file is 164 MB of rope. Opening it is still quick (50 ms)
   and scrolling it is still free, but only the viewport is *shaped* — it is
   not the case that only the viewport is *loaded*. Making that true needs a
   virtual buffer over a memory map, which is a real project and not a
   milestone we have earned yet.

3. **No index of our own.** We never walk, parse, or cache the whole
   project. Symbols come from the language server; text search streams
   through ripgrep's engine and keeps only the matches on screen. This is
   the single biggest reason mainstream editors idle at 1GB and we do not.

4. **One draw call for the text plane.** The editor body is a character
   grid, so the whole screen is one instanced quad draw against one glyph
   atlas. Panels are the same primitive, not a widget tree.

5. **Language servers are separate, lazy, and killable.** Spawned on first
   buffer of that language, terminated when the last one closes. Their
   memory is theirs; rust-analyzer will use gigabytes and that is its
   business, not ours. Our own process stays in tens of megabytes.

6. **Nothing is loaded until opened.** Workspace open reads the directory
   listing of the root. Not the files, not the git objects, not the tree.

## Measured baseline

macOS 27, Apple M5, release build, window open and idle:

    63 MB   AppKit/Cocoa, before a single line of our code runs
    10 MB   + window creation
     3 MB   + wgpu instance, adapter, device, surface, shader
    11 MB   + font system, swash cache, 94 rasterised ASCII glyphs
    --------
    87 MB   baseline      (observed 97 MB RSS / 66 MB private)
    1.15x   + the document, as rope

    0.0%    CPU when idle, sampled over three seconds
    5.5 MB  stripped binary

The headline is that the floor is AppKit's, not ours. Being a native macOS
window costs ~73 MB before wgpu is even initialised, and wgpu then adds 3.
Our own contribution is the 11 MB font and atlas stack. That is the price of
the window: the same editor as a terminal UI would idle around 10 MB total.
Worth knowing, and worth not pretending otherwise — but for context, a
mainstream Electron editor idles between 500 MB and 1 GB.

Where the remaining headroom is, in order of size:

- the 63 MB AppKit floor is not reducible from inside the process
- the rope's 1.15x is reducible only by a virtual buffer (see invariant 2)
- the 11 MB font stack could shrink by handing `FontSystem` an explicit
  font list rather than the system database, if it ever matters

## Crate layout

Split for compile-time isolation and to keep editing logic pure.

    crates/core    lithe-core    rope, cursors, selections, edits, undo
                                 no I/O, no GPU, no async — unit tested
    crates/gpu     lithe-gpu     winit surface, wgpu device, glyph atlas,
                                 draw_rect / draw_row primitives
    crates/syntax  lithe-syntax  tree-sitter grammars, incremental parse,
                                 viewport-bounded highlight queries
    crates/app     lithe         binary: input, layout, panels, wiring

Later milestones add `syntax` (tree-sitter), `lsp`, `vcs` (gix),
`search` (ignore + grep). Each stays a leaf so touching cursor logic
never recompiles wgpu.

## Text rendering

A code editor is overwhelmingly a monospace ASCII grid. Exploiting that is
most of the performance story:

- **Fast path.** The printable ASCII range is rasterized once, at the
  current size and weight, into a single-channel atlas texture. Drawing a
  screenful is then filling an instance buffer with
  `(cell_x, cell_y, glyph_index, fg, bg)` and issuing one draw. No shaping,
  no per-line layout, no allocation.
- **Slow path.** Anything outside that — CJK, emoji, combining marks,
  RTL — goes through full `cosmic-text` shaping, and the shaped result is
  cached per unique run. Rare in source code, correct when it happens.

Glyph rasterization uses cosmic-text's swash backend, so we get real
hinting and font fallback without owning a font stack.

## Milestones

Each one ends at something usable, not a demo.

- **M1  the frame**  window, surface, atlas, `draw_rect`/`draw_row`;
                     render a file read-only; prove 0% idle CPU  [done]
- **M2  the editor** rope buffer, multi-cursor, selections, undo, scroll,
                     open/save  [done]
- **M3  syntax**     tree-sitter incremental parse, theme, highlight spans  [done]
- **M4  files**      buffers/tabs, file tree, fuzzy open (nucleo),
                     command palette  [done]
- **M5  search**     in-buffer find/replace, project search (grep+ignore)  [done]
- **M6  lsp**        diagnostics, completion, hover, goto-def  [done]
- **M7  vcs**        gutter hunks, status, blame, stage, commit  [done]

Since: the four goto requests, references, call hierarchy, rename, whole-
buffer and range formatting, and code actions with `codeAction/resolve`;
right-click menus for the text and the tree; in-place naming in the tree;
a file clipboard with its own undo stack.

A language-server reply is only meaningful next to the question, so
`Awaiting` carries the request's own arguments — the name a rename asked
for, the buffer revision a formatting reply has to match. A reply that
arrives after the buffer has moved on is dropped rather than applied to
text it does not describe.

Two of the requests are two-step: a call hierarchy has to name the symbol
before it can ask who calls it, and a code action may arrive without the
edits it implies. Both are handled by issuing the second request from the
first one's reply, which is why `Lsp::follow_up` exists alongside `ask`:
it takes no document position, because by then the question is about
something the server already named.

Edits from a rename land in open buffers as an ordinary undoable change
and in closed files directly on disk. Opening every touched file as a tab
would be tidier for undo and worse to use: a rename across forty files
would leave forty tabs.

The tree's inline edit row is a real row in `rows`, inserted where the
result will appear. Drawing and hit-testing then need no notion of it;
the only place that knows is the one line that draws its label.

Line blame reads the whole file once, not a line at a time. The
annotation follows the cursor, so a subprocess per line would mean one
per keypress on the way down a file — invariant 1 in everything but
name. `git blame --porcelain` already sends a commit's details only the
first time its sha appears, so `FileBlame` keeps that shape: commits
once, an index per line.

The preview grew a second and third glyph size, and that is the only
place the editor is not one size. A uniform grid is what lets a screenful
of text cost one instance per cell, so the atlas does not take a number:
it takes `Size::{Body, Large, Huge}`, measures each on first use, and
caches glyphs under `(char, face, size)`. A heading then occupies two
grid rows rather than a fractional height, which keeps the preview's
model — a line is a scroll step, and it sits on the grid.

Fenced code in the preview is highlighted by building a throwaway
`Syntax` for the language on the fence. Nothing is kept: a fence is tens
of lines, the result is cached with the rendered preview, and a parser
per code block would cost more than re-running this on the edit that
invalidates it.

Wrapping treats a run boundary as part of a word, not a break between
two. The reader emits one run per style, so `[link](x).` is three runs
and one word; splitting each run on its own spaces and rejoining them
with a single space put a gap before every full stop that followed a
link. Words are built across runs first, then laid out.

An HTML void element must never be in the hidden-tag set. `<meta>` was,
and since nothing ever closes it, every page with a charset declaration
rendered as a blank screen. The two lists are now separate and the void
one is checked first.

A diff of a working file is editable, and that is a mapping rather than a
second editor: a diff row carries the line number it shows, the buffer's
cursor is measured in the same line numbers, and that is enough for a
click to place the caret and for the caret and selection to be drawn
where they belong. `diff_frame` gives drawing and hit-testing the same
geometry, because an editable diff means a click has to land on the
character it looks like it landed on. The new side is read from the
buffer, not from disk, so the diff tracks typing; and it closes when the
active buffer is no longer its file, since otherwise the keystrokes and
the picture would be about different files.

Completion opens itself from two signals, and both are bounded. The
trigger characters come from the server's own `initialize` reply rather
than a list of our own, because which characters mean "a member follows"
is a fact about the language; they arrive one character at a time, which
is also how a keystroke does, so `>` covers `->`. The other signal is two
characters of a word, and it fires once rather than once per letter:
after the list is open, typing narrows what is already there. Neither
signal starts a server — a keystroke must not spawn a process — so a
language whose server is not yet up completes only when asked by name.

The hover popup is the Markdown preview in a box. A server answers in
Markdown, which is why `initialize` now says we can read it: without
`hover.contentFormat` a server must send plaintext, and clangd did, so
a signature arrived as a flat string with nothing to colour. The reply
is kept as written rather than flattened to one line, which is what the
status bar used to need.

The dwell is the one thing the editor does without being asked, so it is
fenced in on every side: only over text, only after the pointer has been
still, only once per rest, and only when a server is already running. It
schedules itself through the same deadline the caret blink and the
autosave use, so an idle window still waits for nothing at all.

A tab is not always a buffer. `Workspace` holds `Vec<Tab>`, where a tab
is a file, a diff, or the settings page, and `active()` still answers
with `Option<&Editor>` — `None` for a tab that is not editing anything,
which is the same answer an empty workspace already gave and the reason
every call site already handled it. That is what kept this from touching
the hundred places that ask for the active buffer.

A working file's diff carries the buffer rather than pointing at another
tab, and takes over the tab the file was in rather than adding one. Both
follow from the same fact: the diff's right-hand column *is* that
buffer. Two tabs would mean two undo histories for one file, and a diff
that pointed at a tab would have to be put back together every time
focus moved. It also retires a workaround — the diff used to close when
you switched tabs, because its keystrokes would otherwise have gone to
whatever buffer was now in front.

The preview and the image decoder both work off the active buffer, and a
diff tab has one, so both now check that the tab in front is a file.
Without that, diffing a Markdown file left a rendered preview behind it
and diffing a PNG drew the picture over the diff.

A commit's diff carries no buffer, so hiding it removes the tab outright
rather than handing a file back. It no longer closes with the panel that
opened it: a tab outlives the thing that opened it, which is most of
what being a tab means.

A page that fills the text area is hit-tested where the text area is,
not before the chrome. Testing the settings page first meant it returned
"nothing here" for every click outside its own switches, which swallowed
the sidebar — and, once it became a tab, the click on the tab that would
have left it. The landing page had the same shape and the same fix.

The settings file tolerates a key it does not know. `deny_unknown_fields`
was right while the set only grew; the first time one was retired it took
every other preference in the file down with it, because the whole parse
failed and fell back to the defaults. Unknown keys are now logged and
ignored, which still catches a typo without punishing history.

The HTML preview is gone. A reader that understands the structural tags
but no stylesheet, script or layout renders a document the way nothing
on the web would, and "roughly a browser" is a worse promise than no
promise. Inline HTML inside Markdown stayed, because there it is part of
the Markdown rather than a claim to render a page.

The multiple-cursor commands all go through one definition of "the lines
a cursor is on" — `line_spans`, which merges overlaps and decides that a
selection ending exactly at a line start has not reached into that line.
Delete, duplicate, move and comment share it, so they cannot disagree
about what the current line is when a selection spans three of them.

Two of those are deliberately narrower than the rest. `move_lines` takes
one cursor only: with several, the blocks can meet in the middle and the
result is nobody's idea of a swap. `toggle_comment` makes one decision
for the whole block — if every line is commented it uncomments, else it
comments all of it — because line by line turns a half-commented block
into its own inverse, which is never what was meant.

A window must ask for its first frame. Drawing is damage-driven and the
loop waits rather than polls, so a window created without a
`request_redraw` stays blank until some unrelated event arrives — which,
for a freshly launched app, meant until it was clicked.

The Mermaid reader draws one diagram and refuses the rest. Refusing is
the half that matters: a sequence diagram rendered as a bad flowchart is
worse than the fence it came from, because the source at least says what
was meant. Anything it cannot lay out returns `None` and the preview
shows the code.

Its arrows are found as maximal runs of `-=.<>` rather than matched
against a table of literals. A table scanned from the right picks the
`->` *inside* `-->` and splits the line there, leaving `A -` as the
source node; a maximal run cannot, and it also handles the
`A -- label --> B` form, where the real arrow is the second run and the
label sits between them.

A fenced block carries its own source on every one of its rows. The
preview is drawn, not laid out as selectable text, so there is nothing
to drag-select; the block is the unit instead, and a click anywhere in
it copies. Carrying the text on every row rather than only the first
means the click needs no walk back to find where the block started, and
an `Arc` means a row still costs a refcount to clone.

Asking for a frame when a window is created is not enough on macOS. The
frame is drawn before AppKit has finished activating the app, so nothing
composites it, and the next frame only came when something else happened
to ask for one. Run from a terminal the pointer was usually already over
the window, so a `CursorEntered` covered it up; launched from Finder
with the pointer elsewhere, nothing did, and the window stayed blank
until it was clicked. `Focused(true)` and `Occluded(false)` — the two
events that mean the window is really on screen — now redraw.

The Mermaid canvas locks the cells a box owns. Checking for a blank cell
is not enough, because a box's own characters are not blank: an arrow
routed past one wrote its corners straight through the label, and
`Node 0 (leader)` came out as `N┴de╮0 (leader)`. Only box drawing may
write to a locked cell.

Subgraphs are flattened into the layering and framed afterwards, which
is what lets the layering stay one simple pass. The frame is skipped
when the group's nodes did not land in a clean rectangle — a frame round
nodes it does not own is a worse error than no frame, and the diagram is
still drawn either way.

A sequence diagram's message text goes down before its lifelines, which
are then drawn only where the text is not. The other order put a `│`
through every message that reached past one lifeline. The canvas is also
widened to the longest message rather than the text being cut at the
next lifeline, because `UNAVAILABLE + l` has lost the part that mattered.

rust-analyzer is the one server that answers nothing until it is ready.
clangd and gopls reply usefully the moment `initialize` comes back;
rust-analyzer has to load a Cargo workspace and index it first, and
until it has, every request gets `null`. Asking once and reporting "no
information" made a working server look like a missing one — the whole
of what "the Rust LSP is broken" turned out to be.

Two things fix it, and the first is a capability. A server sends
`$/progress` only to a client that declares `window.workDoneProgress`,
which this did not, so the loading happened in silence. With it
declared, the status bar says what the server is busy with, and an
empty answer says "still indexing" instead of "nothing found".

There is deliberately no retry. A hover that arrives three seconds late
is tolerable; a go-to-definition that jumps three seconds after the
click is not, so the editor says why the answer was empty and leaves
asking again to the person.

A bundled app cannot see the tools the person installed. macOS hands a
GUI application launchd's `PATH` — `/usr/bin:/bin:/usr/sbin:/sbin` —
not the shell's, so rust-analyzer in `~/.cargo/bin` and gopls in
`~/go/bin` are both invisible and the editor reports "lsp missing" for
a server that is installed and working. clangd lives in `/usr/bin`, so
C kept working and made it look language-specific. The same build run
from a terminal finds everything, which is most of why this is hard to
notice.

`widen_search_path` runs first thing in `main`, before any thread, and
only when `PATH` holds nothing under `$HOME` or a package prefix —
launched from a shell it costs nothing and does nothing. It asks the
login shell, because a fixed list of directories never covers version
managers, and falls back to that list when the shell is slow or absent.

It sets the process environment rather than keeping a private search
list, because the servers need the wider `PATH` too: rust-analyzer
shells out to `cargo` to read a workspace, and a `cargo` it cannot find
leaves it running and useless.

A server hears nothing between `initialize` and `initialized`. The
protocol says so and rust-analyzer enforces it: sent a `didOpen` first
it answers "expected initialized notification" and exits, and the
editor reports a missing language server for one that is installed and
working. clangd and gopls tolerate the wrong order, which is the whole
reason this presented as a Rust-only fault.

The editor opens a file the moment it has one, which is normally before
the reply has arrived, so the ordering cannot be fixed by calling in a
different order. `Client::notify` holds anything sent before the
handshake finishes and `on_initialized` flushes it, which puts the rule
in the one place every notification already goes through.

A document's URI is made absolute first. A relative path yields
`file://crates/a.rs`, where `crates` parses as the host — the server is
told about a document on another machine and answers about nothing.

A bundled app is not handed its documents on the command line.
LaunchServices sends them to the application delegate, and an app that
declares no document types is refused outright — "can't open files of
this type", for every file, including from `open -a`. Declaring the
types without handling the event is the same refusal with extra steps.

winit registers an application delegate of its own and asserts the one
installed is that exact class, so replacing it panics the first time
winit looks. Its documentation describes a guarantee not to register
one; that is from a different version than the one here. The method is
therefore added to winit's own delegate class at runtime, before the
loop starts: Objective-C lets a class gain a method at any point,
existing instances pick it up, winit's check still passes because the
class is unchanged, and AppKit asks `respondsToSelector:` only when it
has a document to deliver.

Paths are buffered rather than acted on. They arrive on AppKit's thread
and can arrive before the event loop is running — a file double-clicked
in Finder is delivered before `resumed`, which is why the first window
opens on it rather than on the landing page.

The hover dwell nearly cost the first invariant. `dwell_hover` marked a
rest as considered only on the paths that went on to ask the server, so
a rest it declined — a file with no language server, most obviously —
left `next_hover` scheduling a deadline already in the past. The loop
woke, declined, re-armed, and spun: 11–21% of a core, measured, while
nothing at all was happening. It marks the rest unconditionally now,
and only the pointer moving clears it.

It was invisible until a benchmark opened a file. With no file open the
pointer is never over text, so the dwell never arms and idle CPU reads
0.000% — which is exactly what the invariant claims, measured in the
one state where it could not fail.
