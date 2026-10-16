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

A diff is coloured by the grammar on both sides, and the asymmetry is
deliberate: a removed line on the left and an added line on the right
keep their flat red and green, because on a split view the colour is
what tells you which side you are reading. Everything else — context,
and the counterpart of a change — is painted the way the editor paints
it, so a diff of code reads as code.

The committed side is a document in its own right, with its own parse
tree, built once when the diff is built and carried across every
rebuild. An editable diff is rebuilt on every keystroke, but what a
commit says cannot change while you type, so re-parsing it per keystroke
would be work for an answer that is already known. The working side
needs no document of its own: it is the buffer, and the buffer's tree is
already incremental.

Both queries are bounded to the visible rows, like the editor's. The
mapping is the diff row's line number, which the editable diff already
had to carry for the caret.

A menu's highlight is its selection. Moving the pointer over an entry
makes it the one Return would take, rather than tinting it the way a
hovered tree row is tinted: two highlights on screen would be two
answers to what Return picks. The pointer passes over a separator
without taking the selection with it, since a separator is not
something Return could choose.

Hit-testing and drawing work a menu row out separately, so a test
pins them together: the middle of where each entry is drawn has to
hit-test as that entry, or the highlight sits under the wrong one.

A change in a diff can be put back on its own, from a right-click on
either column. Two things make that cheap. The first is that a change
is already a run of added and removed lines in the list the diff is
drawn from, so finding the one under the pointer is finding the run
that row is in — and reverting takes the whole run, because the two
halves of a replacement are one edit and undoing half of it would
leave the file in a state neither version ever had.

The second is that it goes through the buffer rather than through git.
`revert_at` says which lines of the working copy to replace and what to
put there, in lines, knowing nothing about any buffer; `revert_edit`
turns that into one character span, which `replace_span` applies as an
ordinary edit. So it lands in the undo history, the diff rebuilds
itself from the result the way it does after any keystroke, and nothing
reaches disk until the file is saved. That is also why it is not asked
for twice the way discarding a whole file is: one `cmd+Z` takes it
back.

The line arithmetic is its own function because it is where this is
easy to get wrong. `line_to_char` clamps to the last line, so a range
that runs past the end has to be recognised rather than converted, and
putting lines back at the end of a file that does not end in a newline
has to supply one or the first of them is glued to the line already
there.

An elided run can be opened. Clicking the "… N unchanged lines" marker
shows exactly those lines, and the right-click menu offers the same
thing and the way back. The whole run, not a window onto it: the marker
says how many lines it is standing in for, so expanding it gives you
what it said. The rows below move down and the rows above do not, so
the scroll position still points at the same place and no adjustment is
needed.

Nothing is stored on the marker to make this work. A gap's edges are
the last real line before it and the first after it, so `elided_at`
reads the range off the list the diff is already drawn from — which
keeps `DiffLine` the size it was, and keeps a field that is meaningful
on one row in a thousand from existing on all of them. A marker with
nothing after it saturates, because the list does not know how long the
file is, and `unified_revealing` clamps.

Which runs are open is kept in committed line numbers, because those
are the stable name for unchanged text: typing above a gap moves its
working line numbers and leaves these alone, so an expanded gap
survives the rebuild that every keystroke causes. It travels across
that rebuild beside the committed document and its tree, for the same
reason they do. A gap can also be part open — `emit_gap` walks the run
and changes form wherever the answer changes, rather than deciding once
per gap.

Hit-testing a diff learned the committed column at the same time, and
the two bools that described a diff — whether one is open, and whether
it is editable — became one `Option<DiffKind>`. They only ever made
sense together, and a test that set the second without the first had
been passing against a hit test that read only the first.

The preview can be selected and copied. It is drawn rather than typed
into, so it has no buffer and no cursor — its selection model is two
places in the laid-out lines, a line index and a character offset into
that line's text, moved by the pointer and read back when something
asks to copy. Characters of the whole line rather than of a span,
because a selection runs across the runs a line is broken into and
does not care where one style ends.

Both halves measure, and measure the same way the drawing does: a
proportional face has no column to divide by, so finding the character
under a point walks the runs and then the characters of the one it
lands in, and drawing the band asks the same walk where a boundary
sits. A round-trip test pins them together — a point taken from where
a character is drawn has to come back as that character, which is
exactly where two separate walks could drift.

Running off the edge clamps rather than returning nothing, because a
drag past the bottom means "to the end" and anything else makes
dragging feel broken. Clicking a fenced block still copies it, but now
on release and only when nothing was dragged: selecting code would
otherwise copy the block out from under the selection. The pointer is
an I-beam over a preview, which is the only thing that says a drawn
document can be selected at all.

The selection is taken down whenever the lines it points at stop
meaning the same thing: a re-render after an edit or a resize, the
preview closing, a click outside it, and Escape.

Cmd-clicking a name jumps to its definition, and nothing said so. The
name under the pointer is now underlined while the command key is
held, in the foreground colour rather than a new one, so it reads as a
link over whatever the grammar already painted the word.

What is underlined is computed from the same conditions the click has
— over the text, in a buffer whose language is known, on something
that could be a name — so the two cannot drift apart and offer a jump
where there is none. `name_at` is the last of those: `word_at` takes
the run of like characters around a point, which over an operator is a
run of operators and over a gap is a run of spaces.

It is recomputed wherever the answer can change: the pointer moving,
the command key going down or coming up, the wheel moving a different
word under a pointer that has not, and the pointer leaving the window.
Each of those redraws only when the answer actually changed, because
every pixel of movement comes through the first of them.

The offscreen snapshot computes those spans too. It did not at first,
and the still frame it produced showed a plain diff and gave no sign
anything was missing — a verification tool that silently agrees is worse
than no tool, so the snapshot now mirrors the pane: it parses the
committed side rather than only constructing a grammar for it, and fills
both span lists before the scene is built.

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

A sublist ending is not a block ending. The blank line that separates
one block from the next was emitted whenever any list closed, so a
nested list put a gap between its own items and whatever followed in
the parent, and one list read as three. Only the outermost closes a
block now.

A fenced block carries its own source on every one of its rows, so a
click anywhere in it copies the block without a walk back to find
where it started; an `Arc` means a row still costs a refcount to
clone.

Asking for a frame when a window is created is not enough on macOS. The
frame is drawn before AppKit has finished activating the app, so nothing
composites it, and the next frame only came when something else happened
to ask for one. Run from a terminal the pointer was usually already over
the window, so a `CursorEntered` covered it up; launched from Finder
with the pointer elsewhere, nothing did, and the window stayed blank
until it was clicked. `Focused(true)` and `Occluded(false)` — the two
events that mean the window is really on screen — now redraw.

The preview scrolls in rows, and a line is not a row. The scrollbar
and the scroll limit always counted rows; four other places read the
same number as a line index and took its fraction as a sub-row slip.
That was close enough while every line was a row or two — a heading
made it drift by one — and it broke outright once a figure could be
thirty rows tall on one line: a notch of the wheel skipped the whole
diagram, so it appeared and vanished all at once instead of sliding
in. One function now walks the lines accumulating their heights and
answers which line the window starts on and how far into it, and
drawing, link hit-testing, fence hit-testing and text hit-testing all
ask it.

Past the end it answers with a line index one off the end rather than
the last line, so a caller walking from there draws nothing. Pinning
the last line to the top instead would mean a document could never be
scrolled off its own bottom.

The preview is a document, not a terminal with a nice font. Four
things were giving away where it came from, and all four were the
grid.

A table was one padded monospace string per row, with `│` and `─`
between the cells. That did line the columns up, and it set every
cell — prose included — in the grid's face. A table now carries its
cells, each with the column it sits in, measured the way it will be
drawn; the painter places them and draws a rule under the heading and
between the rows. No outer frame and no vertical rules: the gap
between columns keeps them apart, and a box round a table is most of
what made it read as a terminal. Headings are promoted to bold
*before* the columns are measured, or the header is measured in one
face and drawn in another — `Default` came out as `Def…` in a column
sized for `exp`.

Inline code sits on a tinted chip, a shade smaller than the prose
around it. A monospace face at the same nominal size reads as larger,
so a word of code stood proud of its own line and the colour alone
made it look like an error in the sentence rather than a thing being
named.

A fenced block is a panel the width of the column rather than a tint
from one edge of the pane to the other, which reads as a change of
mode. It is rounded at the block's first and last row and square in
between, so consecutive rows join into one card instead of a stack of
lozenges.

All of that needed a rounded rectangle, which the renderer could not
draw. It is a third quad mode: the fragment shader evaluates the
signed distance to a rounded box and feathers the edge, so a rounded
rect costs the same one quad a square one does. That is what makes it
affordable to put a chip behind every piece of inline code on a page.

The rhythm was built out of whole grid rows, and a heading was paying
for its air twice: the larger scales take two rows with the text on
the lower one, so that row *is* the space above, and a blank line as
well made the gap above an `h2` three rows deep. A rule — under a
heading, or a thematic break across the page — is likewise the air
under itself, so nothing follows it with another blank.

Prose is shaped a run at a time, not a glyph at a time. Shaping each
character alone was deliberate once: it made the width reported by
`measure` and the width drawn by `text_run` the same number by
construction, so a line could never wrap in one place and be painted
in another. It also threw away kerning — `AVATAR` came out a tenth too
wide, `To the Wall` four per cent, ordinary lowercase prose under one
— and a heading is the worst case on both counts, most capitals and
the largest size.

The invariant is kept and strengthened rather than traded away. A run
is shaped once into an array of placed glyphs; measuring returns that
run's width and drawing reads that run's positions, so the two agree
because they are the same data rather than because two sums happen to
match. Runs are cached by what they say and how they are set, and the
cache is cleared wholesale when it grows past what a window could
hold — what is on screen is shaped again on the next frame, and a
frame only happens when something changed anyway.

The grid face is explicitly *not* shaped. Every cell being exactly one
advance wide is what the editor is built on, and that has to be
exactly true rather than approximately, so the monospace path returns
the cell metric and never asks the shaper. The editor's own hot path
is therefore untouched by any of this.

Glyph images are now cached twice over, by character for the grid and
by the identity shaping gives them for prose. A character cannot name
a shaped glyph: a ligature is one glyph for two letters, and a
contextual alternate is a different glyph for the same one.

Diagrams are drawn as shapes, not as box-drawing characters. The
layout is still the same grid — a figure takes whole rows of the
preview, so the rows it takes have to be countable before anything is
drawn — but the painting emits SVG, which is rasterised through the
same resvg path an `.svg` file already goes down and shown as a
picture.

That removes a whole class of bug rather than fixing one. A character
canvas had to lock the cells a box owned, because an arrow routed past
one wrote its corners through the label and `Node 0 (leader)` came out
as `N┴de╮0 (leader)`; it had to turn a corner into a tee where two
edges met; it had to find a clear cell for every label. A stroke
simply overlaps a stroke, so an edge is one rounded polyline and none
of that machinery exists any more.

The text is deliberately *not* in the SVG. Putting it there would mean
handing a font database to the rasteriser — scanning every font on the
machine at startup, for a picture that is mostly lines — and the
labels would then be set by a different engine from the prose around
them. The diagram hands back its labels in grid coordinates and lithe
typesets them with the same atlas as everything else, in the mono face
the box widths were measured in.

`currentColor` is substituted for the theme's ink when the picture is
rasterised rather than when it is laid out. An SVG outside a document
has nothing to resolve `currentColor` against, so resvg falls back to
black, and black lines on a dark theme are no lines at all.

Figures are cached by the markup they were drawn from and the width
they were drawn at, so a re-render that moves a diagram down the page
reuses the picture and a resize is the one thing that redraws it. The
cache is pruned to what the page still shows, or a resize would leave
every old size on the GPU.

A subgraph's border is broken for its title rather than drawn through
it, so the title needs no backing of its own and the frame still reads
as one shape.

Subgraphs are flattened into the layering and framed afterwards, which
is what lets the layering stay one simple pass. The frame is skipped
when the group's nodes did not land in a clean rectangle — a frame round
nodes it does not own is a worse error than no frame, and the diagram is
still drawn either way.

Three things decide whether they do land in one. A two-headed arrow
does not rank its target: `<-->` says two nodes are connected, not
that one comes before the other, and letting it push its target down a
layer put the three peers of a Raft cluster on three rows with the
subgraph round them unable to frame anything. A member of a subgraph
that nothing points at joins the layer its ranked peers settled on,
rather than floating to the top alone. And the relaxation runs again
afterwards, because pulling a node down can leave an edge out of it
pointing upwards.

That leaves edges between boxes on one row, which a top-to-bottom
route cannot express. They are collected while the rest are drawn and
routed under the row they share: down out of one, across, and up into
the other, with a head at each end of a two-headed arrow. The route
sits inside the subgraph frame, because an edge between two members
belongs to the subgraph.

A frame needs rows of its own. Without them the arrowhead arriving at
the layer inside it landed on the top border and ate the title, so
`Raft Cluster` came out as `Raft Cl▼ster`; the gap between layers
grows by three when there is a subgraph to spend it on, and only then.
The canvas is also widened to the longest title, since a title that
does not fit on its border is dropped and the subgraph is drawn
unlabelled.

A box is as tall as its label has lines. Mermaid writes a break inside
a label as a literal `\n` or as `<br>`, which the source carries
through verbatim, and collapsing those to a space made
`Node 0 (leader)\ngRPC + Raft` one very wide box saying something the
author did not write. Both layouts therefore measure each layer rather
than multiplying a constant.

What a node *is* comes from its shape, so four are drawn rather than
two: a step is square, a terminus rounded, a decision sloped at the
corners — as close to a diamond as a character grid gets, and close
enough because what matters is that it is not a step — and a store has
a disc across its top, which is the symbol everyone already reads as a
database. The disc is a row of its own, which is the other reason box
heights vary.

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

The text and its decorations must share one row position. Everything
drawn behind the text — the current-line highlight, the selection, the
find matches, the caret — went through a `row_y` closure that subtracts
the fractional scroll. The text computed its own baseline and did not,
so at any scroll position that was not a whole number of rows the
caret sat up to a full row away from the character it was in front of.
They share `row_y` now, which is the only arrangement in which they
cannot drift apart.

It survived a caret test that scrolled fractionally on purpose, because
that test compared the caret to a click, and both of those are on the
decoration side of the split: they agreed with each other while both
disagreed with the text. The test that catches it compares the cursor
line's gutter number — drawn on the text's baseline, in a colour no
other row uses — against the highlight band, in pixels.

An open buffer can go stale, and saving it would be a silent
overwrite. The write is a temp-file-and-rename, so a buffer whose file
has moved underneath it — a checkout, a rebase, another editor —
replaces that work wholesale and without a word. Each buffer stamps
the mtime and length it last read or wrote, which catches the case for
the price of a `stat`; a hash would cost the whole file on every check.

Coming back to the window is when the check happens, because that is
when something else has been at it. A clean buffer is reloaded without
asking, since nothing is lost. A dirty one is left alone and says so —
both versions matter and only the person knows which wins — and a save
is refused once, then allowed, so overwriting is a decision rather
than an accident.

Bracket pairing is deliberately restrained. A pair is closed only when
what follows is the end of the line or a character that closes or
separates: typing `(` in the middle of a word is reaching into
existing code, and the closer would land inside it. A quote is not
closed next to a word character at all, because an apostrophe in prose
is commoner than an empty pair. The whole pair is one undo step, and
backspacing an empty one takes both halves.

Hover highlights appear at once, with no fade. An animation would
need a clock, and the caret blink is the only timer this editor has;
adding a second to tint a row was a worse trade than it looked. The
highlight is a rect drawn when the pointer is on the element, and
nothing schedules anything.

Only discrete elements are hoverable. `Hit::Text`, `Hit::Scrollbar`
and `Hit::DiffText` all carry a position, so every pixel of movement
over them would count as hovering something new and ask for a frame
that would draw exactly the same thing. Narrowing to what can actually
light up is what keeps moving the pointer across a file free, and
`set_hovered` reports whether anything changed so a redraw is only
asked for when it did.

Measuring idle CPU for any of this needs the event count beside it.
Three of four runs during the hover work read 12-15% and were
contaminated by real trackpad input — `TouchpadPressure` and
`CursorMoved` arriving while nothing was supposed to be happening. The
clean run, 0 events, reads 0.00%. A CPU figure on its own cannot tell
a spin from a finger resting on the trackpad, which is exactly how the
hover-dwell spin went unnoticed earlier.
