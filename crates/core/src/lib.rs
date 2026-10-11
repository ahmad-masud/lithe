//! Editing primitives.
//!
//! This crate is kept pure on purpose: no filesystem, no GPU, no async. That
//! makes every editing rule unit testable in milliseconds, and it means
//! touching cursor logic never recompiles wgpu or tree-sitter.

pub mod history;
pub mod selection;

use std::io;
use std::ops::Range;

pub use history::{Change, History, Transaction};
pub use ropey::{Rope, RopeSlice};
pub use selection::{Cursor, Motion, Selection};

/// How a character counts for word-wise movement.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punctuation,
}

fn class_of(ch: char) -> Class {
    if ch.is_whitespace() {
        Class::Space
    } else if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

/// A text document.
///
/// Backed by a rope, so an edit in the middle of a 500 MB file is O(log n)
/// rather than a memmove, and snapshots for the syntax tree or the language
/// server are cheap structural clones rather than copies.
#[derive(Clone, Debug, Default)]
pub struct TextBuffer {
    rope: Rope,
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    /// Streams a document in without ever holding the whole file as a
    /// contiguous `String`. The caller owns the file handle, keeping this
    /// crate free of filesystem concerns.
    pub fn from_reader<R: io::Read>(reader: R) -> io::Result<Self> {
        Ok(Self {
            rope: Rope::from_reader(reader)?,
        })
    }

    pub fn write_to<W: io::Write>(&self, writer: W) -> io::Result<()> {
        self.rope.write_to(writer)
    }

    // ---- queries ---------------------------------------------------------

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// A line including its trailing newline. `None` past the end, rather
    /// than panicking like `Rope::line`.
    pub fn line(&self, index: usize) -> Option<RopeSlice<'_>> {
        (index < self.rope.len_lines()).then(|| self.rope.line(index))
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.len_chars()))
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        let line = line.min(self.len_lines().saturating_sub(1));
        self.rope.line_to_char(line)
    }

    /// Length of a line in chars, not counting its line ending.
    pub fn line_len_chars(&self, line: usize) -> usize {
        let Some(slice) = self.line(line) else {
            return 0;
        };
        let mut len = slice.len_chars();
        // Trim \n, then a preceding \r, so CRLF files behave.
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && slice.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        len
    }

    /// Leading whitespace of a line, in chars.
    pub fn indent_chars(&self, line: usize) -> usize {
        let Some(slice) = self.line(line) else {
            return 0;
        };
        slice
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count()
    }

    pub fn char_at(&self, char_idx: usize) -> Option<char> {
        (char_idx < self.len_chars()).then(|| self.rope.char(char_idx))
    }

    pub fn slice_to_string(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.len_chars());
        let start = range.start.min(end);
        self.rope.slice(start..end).to_string()
    }

    pub fn next_word_boundary(&self, from: usize) -> usize {
        let len = self.len_chars();
        let mut i = from;
        if i >= len {
            return len;
        }
        // Step over the run the cursor sits in, then any whitespace after it.
        let start_class = class_of(self.rope.char(i));
        while i < len && class_of(self.rope.char(i)) == start_class {
            i += 1;
        }
        if start_class != Class::Space {
            while i < len && class_of(self.rope.char(i)) == Class::Space {
                i += 1;
            }
        }
        i
    }

    pub fn prev_word_boundary(&self, from: usize) -> usize {
        let mut i = from.min(self.len_chars());
        if i == 0 {
            return 0;
        }
        // Skip whitespace behind the cursor, then the run before that.
        while i > 0 && class_of(self.rope.char(i - 1)) == Class::Space {
            i -= 1;
        }
        if i == 0 {
            return 0;
        }
        let run = class_of(self.rope.char(i - 1));
        while i > 0 && class_of(self.rope.char(i - 1)) == run {
            i -= 1;
        }
        i
    }

    /// The run of like characters around `char_idx` — what a double click
    /// should select.
    ///
    /// Runs are by character class, so a double click inside an
    /// identifier takes the identifier, inside whitespace takes the
    /// whitespace, and on an operator takes the operator rather than
    /// swallowing the names either side of it.
    pub fn word_at(&self, char_idx: usize) -> Range<usize> {
        let len = self.len_chars();
        if len == 0 {
            return 0..0;
        }
        // A click at the very end of a line belongs to the run before it.
        let probe = char_idx.min(len.saturating_sub(1));
        let class = class_of(self.rope.char(probe));

        let mut start = probe;
        while start > 0 && class_of(self.rope.char(start - 1)) == class {
            start -= 1;
        }
        let mut end = probe;
        while end < len && class_of(self.rope.char(end)) == class {
            end += 1;
        }
        start..end
    }

    /// The whole line containing `char_idx`, including its ending — what a
    /// triple click should select.
    pub fn line_span(&self, char_idx: usize) -> Range<usize> {
        let line = self.char_to_line(char_idx);
        let start = self.line_to_char(line);
        let end = self
            .line(line)
            .map(|slice| start + slice.len_chars())
            .unwrap_or(start);
        start..end.min(self.len_chars())
    }

    // ---- edits -----------------------------------------------------------

    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx.min(self.len_chars()), text);
    }

    pub fn remove(&mut self, range: Range<usize>) {
        let end = range.end.min(self.len_chars());
        let start = range.start.min(end);
        if start < end {
            self.rope.remove(start..end);
        }
    }

    /// Applies a transaction's changes. Offsets are relative to the document
    /// as it is now, so changes are applied back-to-front and earlier ones
    /// are never shifted out from under us.
    pub fn apply(&mut self, transaction: &Transaction) {
        for change in transaction.changes.iter().rev() {
            let removed = change.removed.chars().count();
            self.remove(change.at..change.at + removed);
            if !change.inserted.is_empty() {
                self.insert(change.at, &change.inserted);
            }
        }
    }

    /// Undoes a transaction. The stored offsets index the *pre*-transaction
    /// document, so each one is shifted by the net length change of every
    /// earlier change before being reverted.
    pub fn revert(&mut self, transaction: &Transaction) {
        let mut shift: isize = 0;
        let mut positions = Vec::with_capacity(transaction.changes.len());
        for change in &transaction.changes {
            positions.push((change.at as isize + shift) as usize);
            shift +=
                change.inserted.chars().count() as isize - change.removed.chars().count() as isize;
        }

        for (change, at) in transaction.changes.iter().zip(positions).rev() {
            let inserted = change.inserted.chars().count();
            self.remove(at..at + inserted);
            if !change.removed.is_empty() {
                self.insert(at, &change.removed);
            }
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }
}

/// The infallible conversion `from_str` would imply. Spelled as `From`
/// rather than `FromStr`, because nothing here can fail to parse.
impl From<&str> for TextBuffer {
    fn from(text: &str) -> Self {
        Self::from_text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_without_trailing_newline() {
        // Ropey counts the implicit empty line after a trailing newline, so
        // these two differ by one. Worth pinning: off-by-ones here show up as
        // a phantom last line in the viewport.
        assert_eq!(TextBuffer::from_text("a\nb").len_lines(), 2);
        assert_eq!(TextBuffer::from_text("a\nb\n").len_lines(), 3);
    }

    #[test]
    fn line_is_bounds_checked() {
        let b = TextBuffer::from_text("one\ntwo\n");
        assert_eq!(b.line(0).unwrap().to_string(), "one\n");
        assert!(b.line(99).is_none());
    }

    #[test]
    fn reads_from_a_reader() {
        let b = TextBuffer::from_reader(io::Cursor::new("x\ny\n")).unwrap();
        assert_eq!(b.len_lines(), 3);
    }

    #[test]
    fn line_length_excludes_the_line_ending() {
        let b = TextBuffer::from_text("abc\r\nde\nf");
        assert_eq!(b.line_len_chars(0), 3, "CRLF must not count");
        assert_eq!(b.line_len_chars(1), 2);
        assert_eq!(b.line_len_chars(2), 1, "last line has no ending at all");
    }

    #[test]
    fn word_motion_crosses_runs_and_whitespace() {
        let b = TextBuffer::from_text("let foo_bar = baz();");
        assert_eq!(b.next_word_boundary(0), 4, "'let ' -> start of foo_bar");
        assert_eq!(b.next_word_boundary(4), 12, "'foo_bar ' -> the equals");
        assert_eq!(b.prev_word_boundary(11), 4, "back into foo_bar");
        assert_eq!(b.prev_word_boundary(0), 0);
    }

    #[test]
    fn a_double_click_takes_the_run_it_lands_in() {
        let b = TextBuffer::from_text("let foo_bar = baz(1);\n");
        // Inside an identifier, from any position in it.
        assert_eq!(b.word_at(4), 4..11);
        assert_eq!(b.word_at(7), 4..11);
        assert_eq!(b.word_at(10), 4..11, "the last character still selects it");

        // Whitespace is its own run, not the words either side.
        assert_eq!(b.word_at(3), 3..4);
        // Punctuation likewise.
        let open = "let foo_bar = baz(1);".find('(').unwrap();
        assert_eq!(b.word_at(open), open..open + 1);
    }

    #[test]
    fn word_selection_copes_with_the_edges_of_the_document() {
        let b = TextBuffer::from_text("ab");
        assert_eq!(b.word_at(0), 0..2);
        assert_eq!(b.word_at(2), 0..2, "past the end clamps into the last run");
        assert_eq!(TextBuffer::new().word_at(0), 0..0);
    }

    #[test]
    fn a_triple_click_takes_the_whole_line() {
        let b = TextBuffer::from_text("one\ntwo\nthree\n");
        assert_eq!(b.line_span(5), 4..8, "line two, newline included");
        assert_eq!(b.line_span(0), 0..4);
    }

    #[test]
    fn apply_then_revert_round_trips_multi_cursor_edits() {
        let original = "aaa\nbbb\nccc\n";
        let mut b = TextBuffer::from_text(original);

        // Insert at three places at once, as three cursors would.
        let t = Transaction::new(
            vec![
                Change::insert(0, "X"),
                Change::insert(4, "Y"),
                Change::insert(8, "Z"),
            ],
            Selection::single(0),
            Selection::single(0),
        );
        b.apply(&t);
        assert_eq!(b.rope().to_string(), "Xaaa\nYbbb\nZccc\n");

        b.revert(&t);
        assert_eq!(
            b.rope().to_string(),
            original,
            "revert must restore exactly"
        );
    }

    #[test]
    fn revert_handles_mixed_length_changes() {
        let original = "hello world\n";
        let mut b = TextBuffer::from_text(original);
        let t = Transaction::new(
            vec![
                Change {
                    at: 0,
                    removed: "hello".into(),
                    inserted: "hi".into(),
                },
                Change {
                    at: 6,
                    removed: "world".into(),
                    inserted: "everyone".into(),
                },
            ],
            Selection::single(0),
            Selection::single(0),
        );
        b.apply(&t);
        assert_eq!(b.rope().to_string(), "hi everyone\n");
        b.revert(&t);
        assert_eq!(b.rope().to_string(), original);
    }
}
