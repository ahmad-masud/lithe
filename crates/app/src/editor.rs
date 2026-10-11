//! Editing operations: the layer that turns cursors plus history into the
//! things a keypress means.
//!
//! Every mutation funnels through [`Editor::edit`], which builds one
//! transaction covering all cursors at once. That is what keeps multi-cursor
//! correct — the alternative, editing per cursor and fixing up offsets
//! afterwards, is where editors usually get this wrong.

use std::io::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lithe_core::{Change, Cursor, History, Motion, Selection, TextBuffer, Transaction};
use lithe_syntax::{Span, Syntax};

/// Above this, syntax highlighting is switched off for the buffer.
///
/// A tree-sitter tree costs roughly 26x the source it describes — measured
/// at 15.8 MB for a 0.6 MB Rust file. That is a fine trade for code and a
/// terrible one for a generated blob or a log, which are exactly the files
/// the rope's streaming open exists to handle. 2 MB covers essentially all
/// hand-written source while capping the tree near 50 MB. Past it the file
/// still opens instantly; it just opens uncoloured.
const MAX_HIGHLIGHT_BYTES: usize = 2 * 1024 * 1024;

/// Moves an offset by a swap of `moved` characters in `delta`'s
/// direction, which is where a cursor ends up after `move_lines`.
fn shift(at: usize, moved: usize, delta: isize) -> usize {
    if delta < 0 {
        at.saturating_sub(moved)
    } else {
        at + moved
    }
}

pub struct Editor {
    pub doc: TextBuffer,
    pub sel: Selection,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// A provisional tab, opened by a single click. The next single click
    /// reuses its slot. Editing it, or double-clicking, makes it permanent.
    pub preview: bool,
    /// The file is not text. It still opens — as an image, or as a note
    /// saying so — rather than refusing with a decoding error.
    pub binary: bool,
    /// Bumped on every change. Lets the language server sync notice work
    /// without diffing the document.
    pub revision: u64,
    /// Vertical scroll, in lines. Fractional on purpose: a trackpad sends
    /// pixel deltas, and rounding each one to a whole line is what makes
    /// scrolling feel like it is catching on something.
    pub scroll: f32,
    /// `None` when the language is unknown or the file is too large.
    syntax: Option<Syntax>,
    history: History,
    pub tab_width: usize,
}

impl Editor {
    pub fn new(doc: TextBuffer, path: Option<PathBuf>, tab_width: usize) -> Self {
        let mut editor = Self {
            doc,
            sel: Selection::single(0),
            path,
            dirty: false,
            preview: false,
            binary: false,
            revision: 0,
            scroll: 0.0,
            syntax: None,
            history: History::new(),
            tab_width,
        };
        editor.attach_syntax();
        editor
    }

    pub fn open(path: &Path, tab_width: usize) -> Result<Self> {
        let file =
            std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;

        // A file that is not UTF-8 is not a failure to open — it is a
        // file that is not text. An image viewer, or a note that there is
        // nothing to show, beats a decoding error.
        match TextBuffer::from_reader(std::io::BufReader::new(file)) {
            Ok(doc) => Ok(Self::new(doc, Some(path.to_path_buf()), tab_width)),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let mut editor = Self::new(TextBuffer::new(), Some(path.to_path_buf()), tab_width);
                editor.binary = true;
                Ok(editor)
            }
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Picks a grammar from the path and does the first parse.
    fn attach_syntax(&mut self) {
        if self.doc.len_bytes() > MAX_HIGHLIGHT_BYTES {
            self.syntax = None;
            return;
        }
        self.syntax = self.path.as_deref().and_then(Syntax::for_path);
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.parse(&self.doc);
        }
    }

    /// Points the buffer at a new path and re-picks the grammar, since the
    /// extension may have changed with it.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.attach_syntax();
    }

    pub fn language_name(&self) -> Option<&'static str> {
        self.syntax.as_ref().map(|s| s.language_name())
    }

    /// Highlight spans overlapping a byte range. Bounded by the viewport,
    /// never the document.
    pub fn highlights(&mut self, range: std::ops::Range<usize>, out: &mut Vec<Span>) {
        match self.syntax.as_mut() {
            Some(syntax) => syntax.highlights(&self.doc, range, out),
            None => out.clear(),
        }
    }

    // ---- the one mutation path ------------------------------------------

    /// Applies one change per cursor as a single undoable transaction.
    ///
    /// `make` returns the range to replace and the text to put there, in
    /// pre-edit coordinates. Cursors land after the inserted text.
    fn edit(
        &mut self,
        mut make: impl FnMut(&TextBuffer, Cursor) -> Option<(Range<usize>, String)>,
    ) {
        let before = self.sel.clone();

        let mut changes: Vec<Change> = Vec::with_capacity(self.sel.len());
        for &cursor in self.sel.cursors() {
            if let Some((range, inserted)) = make(&self.doc, cursor) {
                if range.is_empty() && inserted.is_empty() {
                    continue;
                }
                changes.push(Change {
                    at: range.start,
                    removed: self.doc.slice_to_string(range),
                    inserted,
                });
            }
        }
        self.commit(changes, before);
    }

    /// Applies a set of changes as one undoable transaction, leaving a
    /// cursor after each. Changes carry offsets into the document as it is
    /// now; they are sorted and applied back-to-front so earlier offsets
    /// stay valid.
    fn commit(&mut self, mut changes: Vec<Change>, before: Selection) {
        if changes.is_empty() {
            return;
        }
        changes.sort_by_key(|c| c.at);

        // Where each cursor ends up once everything before it has shifted.
        let mut shift: isize = 0;
        let mut landed = Vec::with_capacity(changes.len());
        for change in &changes {
            let removed = change.removed.chars().count() as isize;
            let inserted = change.inserted.chars().count() as isize;
            let at = (change.at as isize + shift + inserted) as usize;
            landed.push(Cursor::at(at));
            shift += inserted - removed;
        }

        let after = Selection::from_cursors(landed);
        let transaction = Transaction::new(changes, before, after.clone());

        // Describe the edits to tree-sitter while the buffer still holds the
        // coordinates they were computed in, then replay them in the same
        // back-to-front order the buffer uses.
        if self.syntax.is_some() {
            let edits: Vec<_> = transaction
                .changes
                .iter()
                .map(|change| {
                    lithe_syntax::input_edit(
                        &self.doc,
                        change.at,
                        change.removed.chars().count(),
                        &change.inserted,
                    )
                })
                .collect();
            if let Some(syntax) = self.syntax.as_mut() {
                for edit in edits.iter().rev() {
                    syntax.edit(edit);
                }
            }
        }

        self.doc.apply(&transaction);
        self.revision += 1;
        // Touching a provisional tab is how you say you meant to keep it.
        self.preview = false;
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.parse(&self.doc);
        }
        self.sel = after;
        self.history.push(transaction);
        self.dirty = true;
    }

    // ---- text input ------------------------------------------------------

    /// Replaces every match of `pattern` in the whole buffer. Returns how
    /// many were replaced, as one undo step.
    pub fn replace_all(&mut self, pattern: &lithe_search::Pattern, with: &str) -> usize {
        let mut spans = Vec::new();
        lithe_search::matches_in_range(&self.doc, pattern, 0..self.doc.len_chars(), &mut spans);
        if spans.is_empty() {
            return 0;
        }
        let changes: Vec<Change> = spans
            .iter()
            .map(|span| Change {
                at: span.start,
                removed: self.doc.slice_to_string(span.start..span.end),
                inserted: with.to_string(),
            })
            .collect();
        let count = changes.len();
        let before = self.sel.clone();
        self.commit(changes, before);
        count
    }

    /// Applies a set of non-overlapping replacements as one undo step.
    ///
    /// Offsets index the buffer as it is now. The caller owns that: the
    /// only source of these is a language server answering about a
    /// revision it was sent, so a reply that arrives after the buffer has
    /// moved on must be dropped rather than applied.
    ///
    /// The cursor is put back where it was rather than left on the last
    /// edit — formatting a file should not move you to the end of it.
    pub fn apply_edits(&mut self, edits: Vec<(Range<usize>, String)>) -> usize {
        let edits: Vec<_> = edits
            .into_iter()
            .filter(|(span, with)| {
                span.end <= self.doc.len_chars()
                    && (span.start != span.end || !with.is_empty())
                    && self.doc.slice_to_string(span.clone()) != *with
            })
            .collect();
        if edits.is_empty() {
            return 0;
        }

        let was = self.sel.primary().head;
        let before = self.sel.clone();
        let count = edits.len();
        let changes: Vec<Change> = edits
            .into_iter()
            .map(|(span, inserted)| Change {
                removed: self.doc.slice_to_string(span.clone()),
                at: span.start,
                inserted,
            })
            .collect();
        self.commit(changes, before);
        self.sel = Selection::single(was.min(self.doc.len_chars()));
        count
    }

    /// Replaces one span and leaves the cursor after the replacement.
    pub fn replace_span(&mut self, start: usize, end: usize, with: &str) {
        let before = self.sel.clone();
        let change = Change {
            at: start,
            removed: self.doc.slice_to_string(start..end),
            inserted: with.to_string(),
        };
        self.commit(vec![change], before);
    }

    pub fn insert(&mut self, text: &str) {
        let owned = text.to_string();
        self.edit(|_, cursor| Some((cursor.range(), owned.clone())));
    }

    /// Newline, carrying the current line's indentation, and one level more
    /// when the line opens a block.
    pub fn newline(&mut self) {
        let tab = self.tab_width;
        self.edit(|doc, cursor| {
            let line = doc.char_to_line(cursor.start());
            let indent_len = doc.indent_chars(line);
            let line_start = doc.line_to_char(line);
            let mut text = String::with_capacity(indent_len + tab + 1);
            text.push('\n');
            text.push_str(&doc.slice_to_string(line_start..line_start + indent_len));

            // One more level after a line that opens something.
            let before = doc.slice_to_string(line_start..cursor.start());
            if before.trim_end().ends_with(['{', '(', '[', ':']) {
                text.extend(std::iter::repeat_n(' ', tab));
            }
            Some((cursor.range(), text))
        });
    }

    pub fn backspace(&mut self) {
        let tab = self.tab_width;
        self.edit(|doc, cursor| {
            if !cursor.is_empty() {
                return Some((cursor.range(), String::new()));
            }
            let head = cursor.head;
            if head == 0 {
                return None;
            }
            // Inside leading indentation, delete back to the previous tab
            // stop rather than one space at a time.
            let line = doc.char_to_line(head);
            let line_start = doc.line_to_char(line);
            let col = head - line_start;
            let in_indent = col > 0 && col <= doc.indent_chars(line);
            let all_spaces = doc
                .slice_to_string(line_start..head)
                .chars()
                .all(|c| c == ' ');

            if in_indent && all_spaces && col % tab == 0 {
                return Some((head - tab..head, String::new()));
            }
            Some((head - 1..head, String::new()))
        });
    }

    pub fn delete_forward(&mut self) {
        self.edit(|doc, cursor| {
            if !cursor.is_empty() {
                return Some((cursor.range(), String::new()));
            }
            if cursor.head >= doc.len_chars() {
                return None;
            }
            Some((cursor.head..cursor.head + 1, String::new()))
        });
    }

    /// Tab: to the next tab stop, using spaces.
    pub fn indent(&mut self) {
        let tab = self.tab_width;
        self.edit(|doc, cursor| {
            let line = doc.char_to_line(cursor.start());
            let col = cursor.start() - doc.line_to_char(line);
            let width = tab - (col % tab);
            Some((cursor.range(), " ".repeat(width)))
        });
    }

    /// Shift-Tab: remove up to one tab stop of leading whitespace.
    pub fn unindent(&mut self) {
        let tab = self.tab_width;
        self.edit(|doc, cursor| {
            let line = doc.char_to_line(cursor.start());
            let line_start = doc.line_to_char(line);
            let indent = doc.indent_chars(line);
            if indent == 0 {
                return None;
            }
            let remove = tab.min(indent);
            Some((line_start..line_start + remove, String::new()))
        });
    }

    // ---- history ---------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        let Some(t) = self.history.pop_undo() else {
            return false;
        };
        self.doc.revert(&t);
        self.sel = t.before.clone();
        self.sel.clamp(&self.doc);
        self.reparse();
        self.revision += 1;
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(t) = self.history.pop_redo() else {
            return false;
        };
        self.doc.apply(&t);
        self.sel = t.after.clone();
        self.sel.clamp(&self.doc);
        self.reparse();
        self.revision += 1;
        self.dirty = true;
        true
    }

    /// Undo and redo move text around in ways whose inverse edits are more
    /// trouble to derive than a reparse is to pay for. Neither is a
    /// per-keystroke path.
    fn reparse(&mut self) {
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.invalidate();
            syntax.parse(&self.doc);
        }
    }

    // ---- selection -------------------------------------------------------

    pub fn motion(&mut self, motion: Motion, extend: bool) {
        self.sel.motion(&self.doc, motion, extend);
    }

    pub fn select_all(&mut self) {
        self.sel.select_all(&self.doc);
    }

    // ---- multiple cursors, and whole lines -------------------------------

    /// Adds a cursor on the next occurrence of what is selected.
    ///
    /// With nothing selected it selects the word under the caret first,
    /// which is the whole trick: the first press says what to look for
    /// and every press after it finds another one. Wraps to the top, so
    /// a run of presses eventually comes back to where it started
    /// rather than stopping halfway down the file.
    pub fn select_next_occurrence(&mut self) -> bool {
        let primary = self.sel.primary();
        if primary.is_empty() {
            let word = self.doc.word_at(primary.head);
            if word.is_empty() {
                return false;
            }
            self.sel.collapse_to(Cursor {
                anchor: word.start,
                head: word.end,
            });
            return true;
        }
        let needle = self.doc.slice_to_string(primary.range());
        let taken: Vec<usize> = self.sel.cursors().iter().map(|c| c.start()).collect();
        // Wrapping, so a run of presses comes back round rather than
        // stopping halfway down the file.
        let start = self
            .find_from(&needle, primary.end())
            .or_else(|| self.find_from(&needle, 0));
        let Some(start) = start.filter(|at| !taken.contains(at)) else {
            return false;
        };
        self.sel.add(Cursor {
            anchor: start,
            head: start + needle.chars().count(),
        });
        true
    }

    /// Puts a cursor on every occurrence of what is selected, or of the
    /// word under the caret.
    pub fn select_all_occurrences(&mut self) -> usize {
        if self.sel.primary().is_empty() && !self.select_next_occurrence() {
            return 0;
        }
        let primary = self.sel.primary();
        let needle = self.doc.slice_to_string(primary.range());
        if needle.is_empty() {
            return 0;
        }
        let width = needle.chars().count();
        let mut found = Vec::new();
        let mut at = 0;
        // Forward only. The wrapping search would hand back the first
        // match forever once the last one was passed.
        while let Some(start) = self.find_from(&needle, at) {
            found.push(Cursor {
                anchor: start,
                head: start + width,
            });
            at = start + width;
        }
        let count = found.len();
        if count > 0 {
            self.sel = Selection::from_cursors(found);
        }
        count
    }

    /// The next occurrence of `needle` at or after `from`, as a char
    /// offset. Forward only; wrapping is the caller's decision, and
    /// building it in here made a search for every match run forever.
    fn find_from(&self, needle: &str, from: usize) -> Option<usize> {
        if needle.is_empty() || needle.contains('\n') || from > self.doc.len_chars() {
            return None;
        }
        let text = self.doc.slice_to_string(0..self.doc.len_chars());
        // `find` answers in bytes; everything else here counts chars.
        let bytes: Vec<usize> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .collect();
        let start = *bytes.get(from)?;
        let found = text[start..].find(needle)? + start;
        Some(bytes.partition_point(|b| *b < found))
    }

    /// Extends the column of cursors one line further up or down.
    ///
    /// From the furthest cursor in that direction, at the widest column
    /// any of them has. Taking each cursor's own column instead meant a
    /// short line in the middle dragged the whole column in to its end
    /// and it never came back out.
    pub fn add_cursor(&mut self, delta: isize) {
        let mut cursors: Vec<Cursor> = self.sel.cursors().to_vec();
        let line_of = |at: usize| self.doc.char_to_line(at);
        let column_of = |at: usize| at - self.doc.line_to_char(line_of(at));

        let goal = cursors.iter().map(|c| column_of(c.head)).max().unwrap_or(0);
        let edge = if delta < 0 {
            cursors.iter().map(|c| line_of(c.head)).min()
        } else {
            cursors.iter().map(|c| line_of(c.head)).max()
        };
        let Some(target) = edge.and_then(|line| line.checked_add_signed(delta)) else {
            return;
        };
        if target >= self.doc.len_lines() {
            return;
        }
        // Short lines get a cursor at their end, as everywhere else.
        let start = self.doc.line_to_char(target);
        cursors.push(Cursor::at(
            start + goal.min(self.doc.line_len_chars(target)),
        ));
        self.sel = Selection::from_cursors(cursors);
    }

    /// Selects the whole of every line a cursor touches, newline included.
    pub fn select_lines(&mut self) {
        let spans: Vec<Cursor> = self
            .line_spans()
            .into_iter()
            .map(|range| Cursor {
                anchor: range.start,
                head: range.end,
            })
            .collect();
        if !spans.is_empty() {
            self.sel = Selection::from_cursors(spans);
        }
    }

    /// The full lines each cursor covers, merged and in order.
    ///
    /// Shared by every whole-line command, so they all agree about what
    /// "the current line" means when a selection spans three of them or
    /// two cursors sit on one.
    fn line_spans(&self) -> Vec<Range<usize>> {
        let mut lines: Vec<usize> = Vec::new();
        for cursor in self.sel.cursors() {
            let first = self.doc.char_to_line(cursor.start());
            // A selection ending exactly at the start of a line has not
            // reached into it.
            let last_char = cursor.end().saturating_sub(usize::from(
                cursor.end() > cursor.start()
                    && cursor.end() == self.doc.line_to_char(self.doc.char_to_line(cursor.end())),
            ));
            let last = self.doc.char_to_line(last_char);
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();

        let mut spans: Vec<Range<usize>> = Vec::new();
        for line in lines {
            let start = self.doc.line_to_char(line);
            let end = (start + self.doc.line_len_chars(line) + 1).min(self.doc.len_chars());
            match spans.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => spans.push(start..end),
            }
        }
        spans
    }

    /// Deletes every line a cursor is on.
    pub fn delete_lines(&mut self) {
        let edits: Vec<_> = self
            .line_spans()
            .into_iter()
            .map(|span| (span, String::new()))
            .collect();
        self.apply_edits(edits);
    }

    /// Copies every line a cursor is on, below itself.
    pub fn duplicate_lines(&mut self) {
        let edits: Vec<_> = self
            .line_spans()
            .into_iter()
            .map(|span| {
                let mut text = self.doc.slice_to_string(span.clone());
                // The last line of a file has no newline to copy.
                if !text.ends_with('\n') {
                    text = format!("\n{text}");
                }
                (span.end..span.end, text)
            })
            .collect();
        self.apply_edits(edits);
    }

    /// Swaps every selected line with the one above or below it.
    ///
    /// One cursor only. With several the lines can meet in the middle and
    /// the result is nobody's idea of a swap, so the rarer case is left
    /// to the commoner one rather than guessed at.
    pub fn move_lines(&mut self, delta: isize) {
        let spans = self.line_spans();
        let [span] = spans.as_slice() else { return };
        let first = self.doc.char_to_line(span.start);
        let last = self.doc.char_to_line(span.end.saturating_sub(1));

        let Some(other) = (if delta < 0 {
            first.checked_sub(1)
        } else {
            last.checked_add(1)
        }) else {
            return;
        };
        if other >= self.doc.len_lines() || (delta > 0 && other + 1 >= self.doc.len_lines()) {
            // Nothing below the last line to swap with. A trailing empty
            // line is not a line you can move text past.
            if delta > 0 && self.doc.line_len_chars(other) == 0 {
                return;
            }
        }
        if other >= self.doc.len_lines() {
            return;
        }

        let other_start = self.doc.line_to_char(other);
        let other_end =
            (other_start + self.doc.line_len_chars(other) + 1).min(self.doc.len_chars());
        let block = self.doc.slice_to_string(span.clone());
        let neighbour = self.doc.slice_to_string(other_start..other_end);

        // Both pieces are rewritten as one span, so the swap is one undo
        // step and the offsets cannot drift between the two halves.
        let (whole, text) = if delta < 0 {
            (other_start..span.end, format!("{block}{neighbour}"))
        } else {
            (span.start..other_end, format!("{neighbour}{block}"))
        };
        // Moving a block onto the last line, which has no newline, would
        // otherwise lose one or gain one.
        let text = match (block.ends_with('\n'), neighbour.ends_with('\n')) {
            (true, false) if delta > 0 => format!(
                "{}\n{}",
                neighbour.trim_end_matches('\n'),
                block.trim_end_matches('\n')
            ),
            _ => text,
        };

        let moved = if delta < 0 {
            span.start - other_start
        } else {
            other_end - span.end
        };
        let before = self.sel.clone();
        let cursors: Vec<Cursor> = self
            .sel
            .cursors()
            .iter()
            .map(|c| Cursor {
                anchor: shift(c.anchor, moved, delta),
                head: shift(c.head, moved, delta),
            })
            .collect();
        self.commit(
            vec![Change {
                at: whole.start,
                removed: self.doc.slice_to_string(whole),
                inserted: text,
            }],
            before,
        );
        self.sel = Selection::from_cursors(cursors);
        self.sel.clamp(&self.doc);
    }

    /// Opens a line below — or above — every cursor and moves there.
    pub fn open_line(&mut self, above: bool) {
        let tab = self.tab_width;
        self.edit(move |doc, cursor| {
            let line = doc.char_to_line(cursor.head);
            let line_start = doc.line_to_char(line);
            let indent = doc.slice_to_string(line_start..line_start + doc.indent_chars(line));
            let _ = tab;
            if above {
                Some((line_start..line_start, format!("{indent}\n")))
            } else {
                let end = line_start + doc.line_len_chars(line);
                Some((end..end, format!("\n{indent}")))
            }
        });
        if above {
            // `edit` leaves the caret after the inserted text, which for
            // a line opened above is the start of the old line.
            let cursors: Vec<Cursor> = self
                .sel
                .cursors()
                .iter()
                .map(|c| Cursor::at(c.head.saturating_sub(1)))
                .collect();
            self.sel = Selection::from_cursors(cursors);
        }
    }

    /// Comments or uncomments every line a cursor is on.
    ///
    /// One decision for the whole set, as every editor does it: if every
    /// line is already commented the block is uncommented, otherwise all
    /// of it is commented. Line by line would turn a mixed block into
    /// its own inverse, which is never what was meant.
    pub fn toggle_comment(&mut self) {
        let Some(marker) = self.line_comment() else {
            return;
        };
        let spans = self.line_spans();
        let lines: Vec<(usize, String)> = spans
            .iter()
            .flat_map(|span| {
                let first = self.doc.char_to_line(span.start);
                let last = self.doc.char_to_line(span.end.saturating_sub(1));
                (first..=last).map(|line| {
                    let start = self.doc.line_to_char(line);
                    (
                        line,
                        self.doc
                            .slice_to_string(start..start + self.doc.line_len_chars(line)),
                    )
                })
            })
            .filter(|(_, text)| !text.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return;
        }

        let all_commented = lines
            .iter()
            .all(|(_, text)| text.trim_start().starts_with(marker));
        // Commenting lines up at the shallowest indent, so a block keeps
        // its shape instead of each marker landing at its own depth.
        let column = lines
            .iter()
            .map(|(_, text)| text.len() - text.trim_start().len())
            .min()
            .unwrap_or(0);

        let edits: Vec<(Range<usize>, String)> = lines
            .iter()
            .map(|(line, text)| {
                let start = self.doc.line_to_char(*line);
                if all_commented {
                    let at = text.find(marker).unwrap_or(0);
                    let after = at + marker.len();
                    // Take the single space a comment is written with.
                    let width = marker.len() + usize::from(text[after..].starts_with(' '));
                    (start + at..start + at + width, String::new())
                } else {
                    let at = column.min(text.len());
                    (start + at..start + at, format!("{marker} "))
                }
            })
            .collect();
        self.apply_edits(edits);
    }

    /// How this file's language writes a line comment.
    fn line_comment(&self) -> Option<&'static str> {
        Some(match self.language_name()? {
            "python" | "toml" | "yaml" | "shell" => "#",
            "html" | "markdown" => return None,
            // The C family, and everything that borrowed its syntax.
            _ => "//",
        })
    }

    /// Text of every non-empty selection, joined by newlines — what a copy
    /// with several cursors should put on the clipboard.
    pub fn selected_text(&self) -> Option<String> {
        let parts: Vec<String> = self
            .sel
            .cursors()
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| self.doc.slice_to_string(c.range()))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        let line = self.doc.char_to_line(char_idx);
        (line, char_idx - self.doc.line_to_char(line))
    }

    // ---- viewport --------------------------------------------------------

    /// Scrolls the minimum distance that brings the primary cursor into
    /// view, keeping a few lines of context where there is room.
    /// First fully or partly visible line.
    pub fn top_line(&self) -> usize {
        self.scroll.max(0.0) as usize
    }

    /// How far the first visible line is scrolled past the top, in pixels,
    /// given a line height. This is what makes scrolling smooth.
    pub fn pixel_offset(&self, line_height: f32) -> f32 {
        (self.scroll.max(0.0).fract() * line_height).round()
    }

    /// Scrolls the minimum distance that brings the primary cursor into
    /// view, keeping a few lines of context where there is room.
    pub fn scroll_to_cursor(&mut self, visible_rows: usize) {
        const MARGIN: f32 = 3.0;
        if visible_rows == 0 {
            return;
        }
        let line = self.doc.char_to_line(self.sel.primary().head) as f32;
        let rows = visible_rows as f32;
        let margin = MARGIN.min((rows - 1.0).max(0.0) / 2.0);

        if line < self.scroll + margin {
            self.scroll = (line - margin).max(0.0);
        } else if line + margin > self.scroll + rows - 1.0 {
            self.scroll = line + margin + 1.0 - rows;
        }
        self.clamp_scroll(visible_rows);
    }

    pub fn scroll_by(&mut self, lines: f32, visible_rows: usize) {
        self.scroll -= lines;
        self.clamp_scroll(visible_rows);
    }

    /// Keeps the last line reachable without letting the document float
    /// off the top of the window.
    pub fn clamp_scroll(&mut self, visible_rows: usize) {
        // Stop with the final line at the bottom rather than letting the
        // buffer scroll away entirely; a couple of lines of overscroll
        // makes the end easier to work at.
        let max = (self.doc.len_lines() as f32 - visible_rows as f32 + 2.0).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    // ---- disk ------------------------------------------------------------

    /// Writes through a temporary file in the same directory and renames it
    /// over the original, so an interrupted save cannot leave a truncated
    /// file where the user's code used to be.
    pub fn save(&mut self) -> Result<()> {
        if self.binary {
            anyhow::bail!("this file is not text; lithe will not write over it");
        }
        let Some(path) = self.path.clone() else {
            anyhow::bail!("buffer has no path; save-as is not implemented yet");
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        let tmp = dir.join(format!(
            ".{}.lithe-tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));

        {
            let file = std::fs::File::create(&tmp)
                .with_context(|| format!("cannot create {}", tmp.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            self.doc.write_to(&mut writer)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;

        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(TextBuffer::from_text(text), None, 4)
    }

    fn text(e: &Editor) -> String {
        e.doc.rope().to_string()
    }

    #[test]
    fn typing_inserts_at_every_cursor() {
        let mut e = ed("a\na\na\n");
        e.sel = Selection::from_cursors(vec![Cursor::at(0), Cursor::at(2), Cursor::at(4)]);
        e.insert("X");
        assert_eq!(text(&e), "Xa\nXa\nXa\n");
        // Each cursor should sit just after its own insertion.
        assert_eq!(
            e.sel.cursors().iter().map(|c| c.head).collect::<Vec<_>>(),
            vec![1, 4, 7]
        );
    }

    #[test]
    fn typing_replaces_selections() {
        let mut e = ed("hello world\n");
        e.sel = Selection::from_cursor(Cursor { anchor: 0, head: 5 });
        e.insert("goodbye");
        assert_eq!(text(&e), "goodbye world\n");
    }

    #[test]
    fn undo_restores_text_and_cursors() {
        let mut e = ed("abc\n");
        e.sel = Selection::single(3);
        e.insert("!");
        assert_eq!(text(&e), "abc!\n");
        assert!(e.undo());
        assert_eq!(text(&e), "abc\n");
        assert_eq!(e.sel.primary().head, 3, "cursor returns to where it was");
        assert!(e.redo());
        assert_eq!(text(&e), "abc!\n");
    }

    #[test]
    fn consecutive_typing_is_one_undo_step() {
        let mut e = ed("");
        for ch in "hello".chars() {
            e.insert(&ch.to_string());
        }
        assert_eq!(text(&e), "hello");
        assert!(e.undo());
        assert_eq!(
            text(&e),
            "",
            "a word should undo in one go, not letter by letter"
        );
    }

    #[test]
    fn newline_carries_indentation_and_opens_a_block() {
        let mut e = ed("    let x = 1;");
        e.sel = Selection::single(e.doc.len_chars());
        e.newline();
        assert_eq!(text(&e), "    let x = 1;\n    ");

        let mut e = ed("fn main() {");
        e.sel = Selection::single(e.doc.len_chars());
        e.newline();
        assert_eq!(
            text(&e),
            "fn main() {\n    ",
            "an opening brace adds a level"
        );
    }

    #[test]
    fn backspace_eats_a_whole_indent_level() {
        let mut e = ed("        x");
        e.sel = Selection::single(8);
        e.backspace();
        assert_eq!(
            text(&e),
            "    x",
            "inside indentation, delete to the tab stop"
        );

        // But only inside indentation.
        let mut e = ed("ab");
        e.sel = Selection::single(2);
        e.backspace();
        assert_eq!(text(&e), "a");
    }

    #[test]
    fn tab_goes_to_the_next_stop() {
        let mut e = ed("ab");
        e.sel = Selection::single(2);
        e.indent();
        assert_eq!(text(&e), "ab  ", "column 2 with width 4 means two spaces");
    }

    #[test]
    fn unindent_removes_at_most_one_level() {
        let mut e = ed("      x");
        e.sel = Selection::single(6);
        e.unindent();
        assert_eq!(text(&e), "  x");
    }

    #[test]
    fn scrolling_follows_the_cursor_both_ways() {
        let mut e = ed(&"line\n".repeat(200));
        e.sel = Selection::single(e.doc.line_to_char(150));
        e.scroll_to_cursor(40);
        assert!(
            e.top_line() <= 150 && 150 < e.top_line() + 40,
            "cursor at 150 should be on screen, scroll is {}",
            e.scroll
        );

        e.sel = Selection::single(0);
        e.scroll_to_cursor(40);
        assert_eq!(e.scroll, 0.0);
    }

    #[test]
    fn scrolling_is_fractional_so_it_does_not_jump_a_line_at_a_time() {
        let mut e = ed(&"line\n".repeat(200));
        e.scroll_by(-0.4, 40);
        assert_eq!(e.top_line(), 0, "a partial line should not advance the top");
        assert!(e.pixel_offset(20.0) > 0.0, "but it should shift the pixels");

        e.scroll_by(-0.8, 40);
        assert_eq!(e.top_line(), 1, "and accumulate into a whole line");
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut e = ed(&"line\n".repeat(100));
        e.scroll_by(1000.0, 40);
        assert_eq!(e.scroll, 0.0, "cannot scroll above the first line");

        e.scroll_by(-100_000.0, 40);
        assert!(
            e.top_line() < e.doc.len_lines(),
            "the document must not scroll out of sight, top is {}",
            e.top_line()
        );
    }

    #[test]
    fn a_binary_file_opens_rather_than_erroring() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [0xffu8, 0xfe, 0x00, 0x01, 0x80]).unwrap();

        let editor = Editor::open(&path, 4).expect("should open, not fail");
        assert!(editor.binary, "it should know the file is not text");
        assert_eq!(editor.doc.len_chars(), 0, "and not invent contents for it");
    }

    #[test]
    fn a_binary_file_is_never_written_back() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join("blob.bin");
        let original = [0xffu8, 0xfe, 0x00, 0x01];
        std::fs::write(&path, original).unwrap();

        let mut editor = Editor::open(&path, 4).unwrap();
        assert!(editor.save().is_err(), "saving would truncate the file");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the bytes on disk must be untouched"
        );
    }

    #[test]
    fn a_text_file_is_not_mistaken_for_binary() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join("fine.txt");
        std::fs::write(&path, "héllo — wörld\n").unwrap();
        let editor = Editor::open(&path, 4).unwrap();
        assert!(!editor.binary);
        assert!(editor.doc.len_chars() > 0);
    }

    #[test]
    fn copying_several_cursors_joins_their_text() {
        let mut e = ed("abc\ndef\n");
        e.sel = Selection::from_cursors(vec![
            Cursor { anchor: 0, head: 3 },
            Cursor { anchor: 4, head: 7 },
        ]);
        assert_eq!(e.selected_text().as_deref(), Some("abc\ndef"));
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(TextBuffer::from_text(text), None, 4)
    }

    #[test]
    fn several_edits_apply_at_the_offsets_they_were_given() {
        let mut editor = ed("let old = 1;\nlet old = 2;\n");
        // Both offsets index the buffer as it is now; applying the first
        // must not shift the second.
        let applied = editor.apply_edits(vec![(4..7, "new".into()), (17..20, "new".into())]);
        assert_eq!(applied, 2);
        assert_eq!(
            editor.doc.rope().to_string(),
            "let new = 1;\nlet new = 2;\n"
        );
    }

    #[test]
    fn a_whole_formatting_pass_is_one_undo() {
        let mut editor = ed("a( 1 );\nb( 2 );\n");
        let before = editor.doc.rope().to_string();
        editor.apply_edits(vec![
            (2..3, "".into()),
            (4..5, "".into()),
            (10..11, "".into()),
            (12..13, "".into()),
        ]);
        assert_eq!(editor.doc.rope().to_string(), "a(1);\nb(2);\n");

        assert!(editor.undo(), "nothing to undo");
        assert_eq!(editor.doc.rope().to_string(), before, "one undo, all of it");
    }

    #[test]
    fn an_edit_that_changes_nothing_is_not_an_edit() {
        let mut editor = ed("same\n");
        let applied = editor.apply_edits(vec![(0..4, "same".into())]);
        assert_eq!(applied, 0);
        assert!(!editor.dirty, "the buffer was marked dirty for nothing");
    }

    #[test]
    fn the_cursor_stays_put_rather_than_following_the_last_edit() {
        let mut editor = ed("one\ntwo\nthree\n");
        editor.sel = Selection::single(2);
        editor.apply_edits(vec![(8..13, "THREE".into())]);
        assert_eq!(
            editor.sel.primary().head,
            2,
            "formatting the end of the file moved the caret there"
        );
    }

    #[test]
    fn an_edit_past_the_end_is_dropped_rather_than_panicking() {
        // A reply can arrive describing a buffer that has since shrunk.
        let mut editor = ed("short\n");
        assert_eq!(editor.apply_edits(vec![(0..999, "x".into())]), 0);
        assert_eq!(editor.doc.rope().to_string(), "short\n");
    }
}

#[cfg(test)]
mod vscode_tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(TextBuffer::from_text(text), None, 4)
    }

    fn rust(text: &str) -> Editor {
        Editor::new(
            TextBuffer::from_text(text),
            Some(std::path::PathBuf::from("a.rs")),
            4,
        )
    }

    fn text(e: &Editor) -> String {
        e.doc.rope().to_string()
    }

    fn selections(e: &Editor) -> Vec<String> {
        e.sel
            .cursors()
            .iter()
            .map(|c| e.doc.slice_to_string(c.range()))
            .collect()
    }

    #[test]
    fn the_first_press_takes_the_word_and_the_next_finds_another() {
        let mut e = ed("let total = total + other;\n");
        e.sel = Selection::single(5); // inside the first `total`

        assert!(e.select_next_occurrence());
        assert_eq!(selections(&e), vec!["total"], "the first press says what");

        assert!(e.select_next_occurrence());
        assert_eq!(selections(&e), vec!["total", "total"]);

        // Typing now replaces both, which is the whole point of it.
        e.insert("sum");
        assert_eq!(text(&e), "let sum = sum + other;\n");
    }

    #[test]
    fn running_out_of_matches_wraps_rather_than_stopping() {
        let mut e = ed("one two one\n");
        e.sel = Selection::single(0);
        e.select_next_occurrence(); // selects `one`
        assert!(e.select_next_occurrence(), "the second `one`");
        assert_eq!(e.sel.len(), 2);
        // Wrapping lands back on a cursor that already exists, so there
        // is nothing left to add.
        assert!(!e.select_next_occurrence());
        assert_eq!(e.sel.len(), 2, "and no duplicate cursor appears");
    }

    #[test]
    fn selecting_every_occurrence_finds_them_all_at_once() {
        let mut e = ed("x = x + x\n");
        e.sel = Selection::single(0);
        assert_eq!(e.select_all_occurrences(), 3);
        e.insert("y");
        assert_eq!(text(&e), "y = y + y\n");
    }

    #[test]
    fn a_cursor_added_below_keeps_its_column_and_clamps_on_a_short_line() {
        let mut e = ed("abcdef\nab\nabcdef\n");
        e.sel = Selection::single(4); // column 4 of the first line
        e.add_cursor(1);
        e.add_cursor(1);
        let columns: Vec<usize> = e
            .sel
            .cursors()
            .iter()
            .map(|c| c.head - e.doc.line_to_char(e.doc.char_to_line(c.head)))
            .collect();
        assert_eq!(columns, vec![4, 2, 4], "the short line gets its end");
    }

    #[test]
    fn a_cursor_is_not_added_past_either_end_of_the_file() {
        let mut e = ed("one\n");
        e.sel = Selection::single(0);
        e.add_cursor(-1);
        assert_eq!(e.sel.len(), 1, "there is nothing above the first line");
    }

    #[test]
    fn whole_line_commands_act_on_every_line_a_selection_touches() {
        let mut e = ed("one\ntwo\nthree\nfour\n");
        // From the middle of line 0 to the middle of line 2.
        e.sel = Selection::from_cursor(Cursor {
            anchor: 1,
            head: 10,
        });
        e.delete_lines();
        assert_eq!(text(&e), "four\n");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_has_not_reached_into_it() {
        let mut e = ed("one\ntwo\nthree\n");
        // Exactly to the start of line 1, which is the shape a
        // double-click-and-drag leaves.
        e.sel = Selection::from_cursor(Cursor { anchor: 0, head: 4 });
        e.delete_lines();
        assert_eq!(text(&e), "two\nthree\n", "line 1 was not selected");
    }

    #[test]
    fn duplicating_puts_the_copy_below() {
        let mut e = ed("one\ntwo\n");
        e.sel = Selection::single(1);
        e.duplicate_lines();
        assert_eq!(text(&e), "one\none\ntwo\n");
    }

    #[test]
    fn duplicating_the_last_line_of_a_file_without_a_newline_adds_one() {
        let mut e = ed("one\ntwo");
        e.sel = Selection::single(5);
        e.duplicate_lines();
        assert_eq!(text(&e), "one\ntwo\ntwo");
    }

    #[test]
    fn moving_a_line_swaps_it_with_its_neighbour() {
        let mut e = ed("one\ntwo\nthree\n");
        e.sel = Selection::single(5); // on `two`
        e.move_lines(-1);
        assert_eq!(text(&e), "two\none\nthree\n");

        e.move_lines(1);
        assert_eq!(text(&e), "one\ntwo\nthree\n", "and back again");
    }

    #[test]
    fn the_caret_travels_with_the_line_it_moves() {
        let mut e = ed("one\ntwo\nthree\n");
        e.sel = Selection::single(5); // column 1 of `two`
        e.move_lines(-1);
        assert_eq!(e.sel.primary().head, 1, "still column 1 of `two`");
        assert_eq!(e.doc.char_to_line(e.sel.primary().head), 0);
    }

    #[test]
    fn moving_onto_a_last_line_with_no_newline_keeps_the_file_whole() {
        let mut e = ed("one\ntwo");
        e.sel = Selection::single(0);
        e.move_lines(1);
        assert_eq!(
            text(&e),
            "two\none",
            "the newline belongs between the lines, not to one of them"
        );
    }

    #[test]
    fn a_line_does_not_move_off_either_end() {
        let mut e = ed("one\ntwo\n");
        e.sel = Selection::single(0);
        e.move_lines(-1);
        assert_eq!(text(&e), "one\ntwo\n", "nothing above the first line");
    }

    #[test]
    fn opening_a_line_keeps_the_indentation_and_moves_there() {
        let mut e = ed("fn f() {\n    body();\n}\n");
        e.sel = Selection::single(e.doc.line_to_char(1) + 8); // inside `body();`
        e.open_line(false);
        assert_eq!(text(&e), "fn f() {\n    body();\n    \n}\n");
        assert_eq!(e.sel.primary().head, e.doc.line_to_char(2) + 4);

        let mut e = ed("fn f() {\n    body();\n}\n");
        e.sel = Selection::single(e.doc.line_to_char(1) + 8);
        e.open_line(true);
        assert_eq!(text(&e), "fn f() {\n    \n    body();\n}\n");
    }

    #[test]
    fn commenting_lines_up_at_the_shallowest_indent() {
        let mut e = rust("fn f() {\n    a();\n        b();\n}\n");
        e.sel = Selection::from_cursor(Cursor {
            anchor: e.doc.line_to_char(1),
            head: e.doc.line_to_char(2) + 1,
        });
        e.toggle_comment();
        assert_eq!(
            text(&e),
            "fn f() {\n    // a();\n    //     b();\n}\n",
            "a block keeps its shape"
        );
    }

    #[test]
    fn a_fully_commented_block_uncomments() {
        let mut e = rust("// one\n// two\n");
        e.sel = Selection::from_cursor(Cursor {
            anchor: 0,
            head: 13,
        });
        e.toggle_comment();
        assert_eq!(text(&e), "one\ntwo\n");
    }

    #[test]
    fn a_half_commented_block_is_commented_rather_than_inverted() {
        let mut e = rust("// one\ntwo\n");
        e.sel = Selection::from_cursor(Cursor {
            anchor: 0,
            head: 10,
        });
        e.toggle_comment();
        assert_eq!(
            text(&e),
            "// // one\n// two\n",
            "all of it, or line by line would swap the two"
        );
    }

    #[test]
    fn commenting_knows_which_marker_the_language_uses() {
        let mut py = Editor::new(
            TextBuffer::from_text("x = 1\n"),
            Some(std::path::PathBuf::from("a.py")),
            4,
        );
        py.sel = Selection::single(0);
        py.toggle_comment();
        assert_eq!(text(&py), "# x = 1\n");

        // And leaves a file it has no comment syntax for alone.
        let mut plain = ed("text\n");
        plain.sel = Selection::single(0);
        plain.toggle_comment();
        assert_eq!(text(&plain), "text\n");
    }

    #[test]
    fn a_blank_line_inside_a_block_is_not_commented() {
        let mut e = rust("a();\n\nb();\n");
        e.sel = Selection::from_cursor(Cursor {
            anchor: 0,
            head: 11,
        });
        e.toggle_comment();
        assert_eq!(text(&e), "// a();\n\n// b();\n");
    }

    #[test]
    fn selecting_the_line_takes_the_newline_with_it() {
        let mut e = ed("one\ntwo\n");
        e.sel = Selection::single(1);
        e.select_lines();
        assert_eq!(selections(&e), vec!["one\n"]);
    }

    #[test]
    fn every_whole_line_command_is_one_undo_step() {
        let mut e = ed("one\ntwo\nthree\n");
        e.sel = Selection::from_cursor(Cursor { anchor: 0, head: 7 });
        e.delete_lines();
        assert_eq!(text(&e), "three\n");
        assert!(e.undo());
        assert_eq!(text(&e), "one\ntwo\nthree\n", "two lines, one undo");
    }
}
