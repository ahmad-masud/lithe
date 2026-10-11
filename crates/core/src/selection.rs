//! Cursors and selections.
//!
//! Multi-cursor is here from the start rather than bolted on later: every
//! edit path takes a set of cursors, so there is no single-cursor assumption
//! to unpick afterwards. A lone cursor is just a set of one.
//!
//! Positions are char indices into the rope. Movement is char-wise, which is
//! correct for source code but not yet for combining marks or emoji
//! sequences; grapheme-aware movement is a known gap.

use std::ops::Range;

use crate::TextBuffer;

/// A cursor, and the selection it drags behind it.
///
/// `anchor` stays put while `head` moves, so a selection knows which end the
/// user is extending from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub anchor: usize,
    pub head: usize,
}

impl Cursor {
    pub fn at(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Collapses to a bare cursor at `pos`.
    pub fn collapsed(pos: usize) -> Self {
        Self::at(pos)
    }
}

/// Which way a motion goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    WordLeft,
    WordRight,
}

/// An ordered, non-overlapping set of cursors.
#[derive(Clone, Debug)]
pub struct Selection {
    cursors: Vec<Cursor>,
    primary: usize,
    /// Column a vertical motion is aiming for, so moving down through a
    /// short line and back out does not lose the original column.
    goal_col: Option<usize>,
}

impl Default for Selection {
    fn default() -> Self {
        Self::single(0)
    }
}

impl Selection {
    pub fn single(pos: usize) -> Self {
        Self {
            cursors: vec![Cursor::at(pos)],
            primary: 0,
            goal_col: None,
        }
    }

    pub fn from_cursor(cursor: Cursor) -> Self {
        Self {
            cursors: vec![cursor],
            primary: 0,
            goal_col: None,
        }
    }

    /// Builds a set from cursors already in document order. Falls back to a
    /// cursor at the origin rather than allowing an empty set, which every
    /// other method would have to guard against.
    pub fn from_cursors(cursors: Vec<Cursor>) -> Self {
        if cursors.is_empty() {
            return Self::single(0);
        }
        let primary = cursors.len() - 1;
        let mut sel = Self {
            cursors,
            primary,
            goal_col: None,
        };
        sel.normalize();
        sel
    }

    pub fn cursors(&self) -> &[Cursor] {
        &self.cursors
    }

    pub fn primary(&self) -> Cursor {
        self.cursors[self.primary]
    }

    pub fn len(&self) -> usize {
        self.cursors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cursors.is_empty()
    }

    /// Replaces everything with one cursor.
    pub fn collapse_to(&mut self, cursor: Cursor) {
        self.cursors.clear();
        self.cursors.push(cursor);
        self.primary = 0;
        self.goal_col = None;
    }

    /// Adds a cursor, keeping the set sorted and overlap-free. The new
    /// cursor becomes primary.
    pub fn add(&mut self, cursor: Cursor) {
        self.cursors.push(cursor);
        self.primary = self.cursors.len() - 1;
        self.normalize();
        self.goal_col = None;
    }

    /// Sets the head of the primary cursor, as a drag does.
    pub fn drag_primary_to(&mut self, pos: usize) {
        self.cursors[self.primary].head = pos;
        self.normalize();
        self.goal_col = None;
    }

    pub fn select_all(&mut self, buffer: &TextBuffer) {
        self.collapse_to(Cursor {
            anchor: 0,
            head: buffer.len_chars(),
        });
    }

    /// Sorts by start and merges anything that touches or overlaps, so two
    /// cursors can never edit the same text twice.
    pub fn normalize(&mut self) {
        if self.cursors.len() <= 1 {
            return;
        }
        let primary = self.cursors[self.primary];
        self.cursors.sort_by_key(|c| (c.start(), c.end()));

        let mut merged: Vec<Cursor> = Vec::with_capacity(self.cursors.len());
        for cursor in self.cursors.drain(..) {
            match merged.last_mut() {
                Some(prev) if cursor.start() <= prev.end() => {
                    // Keep the direction of the cursor being extended.
                    let (start, end) = (
                        prev.start().min(cursor.start()),
                        prev.end().max(cursor.end()),
                    );
                    let forward = cursor.head >= cursor.anchor;
                    *prev = if forward {
                        Cursor {
                            anchor: start,
                            head: end,
                        }
                    } else {
                        Cursor {
                            anchor: end,
                            head: start,
                        }
                    };
                }
                _ => merged.push(cursor),
            }
        }
        self.cursors = merged;

        // Primary follows the cursor that absorbed it.
        self.primary = self
            .cursors
            .iter()
            .position(|c| c.start() <= primary.head && primary.head <= c.end())
            .unwrap_or(0);
    }

    /// Clamps every cursor into the document, after an edit shortened it.
    pub fn clamp(&mut self, buffer: &TextBuffer) {
        let max = buffer.len_chars();
        for cursor in &mut self.cursors {
            cursor.anchor = cursor.anchor.min(max);
            cursor.head = cursor.head.min(max);
        }
        self.normalize();
    }

    /// Applies a motion to every cursor. `extend` keeps anchors where they
    /// are, turning the motion into a selection.
    pub fn motion(&mut self, buffer: &TextBuffer, motion: Motion, extend: bool) {
        // Vertical motion is the only one that remembers a goal column.
        let keep_goal = matches!(motion, Motion::Up | Motion::Down);
        if !keep_goal {
            self.goal_col = None;
        }

        for i in 0..self.cursors.len() {
            let cursor = self.cursors[i];

            // A plain (non-extending) left/right out of a selection lands on
            // its edge rather than moving from the head, which is what every
            // editor does and what people expect.
            let head = if !extend && !cursor.is_empty() {
                match motion {
                    Motion::Left => cursor.start(),
                    Motion::Right => cursor.end(),
                    _ => move_head(buffer, cursor.head, motion, &mut self.goal_col),
                }
            } else {
                move_head(buffer, cursor.head, motion, &mut self.goal_col)
            };

            self.cursors[i] = if extend {
                Cursor {
                    anchor: cursor.anchor,
                    head,
                }
            } else {
                Cursor::at(head)
            };
        }
        self.normalize();
    }
}

fn move_head(
    buffer: &TextBuffer,
    head: usize,
    motion: Motion,
    goal_col: &mut Option<usize>,
) -> usize {
    match motion {
        Motion::Left => head.saturating_sub(1),
        Motion::Right => (head + 1).min(buffer.len_chars()),

        Motion::Up | Motion::Down => {
            let line = buffer.char_to_line(head);
            let col = head - buffer.line_to_char(line);
            let goal = *goal_col.get_or_insert(col);

            let target = match motion {
                Motion::Up => {
                    if line == 0 {
                        return 0;
                    }
                    line - 1
                }
                _ => {
                    if line + 1 >= buffer.len_lines() {
                        return buffer.len_chars();
                    }
                    line + 1
                }
            };
            buffer.line_to_char(target) + goal.min(buffer.line_len_chars(target))
        }

        Motion::LineStart => {
            // Smart home: first non-whitespace, then column zero.
            let line = buffer.char_to_line(head);
            let start = buffer.line_to_char(line);
            let indent = start + buffer.indent_chars(line);
            if head > indent || head == start {
                indent
            } else {
                start
            }
        }
        Motion::LineEnd => {
            let line = buffer.char_to_line(head);
            buffer.line_to_char(line) + buffer.line_len_chars(line)
        }

        Motion::DocStart => 0,
        Motion::DocEnd => buffer.len_chars(),

        Motion::WordLeft => buffer.prev_word_boundary(head),
        Motion::WordRight => buffer.next_word_boundary(head),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> TextBuffer {
        TextBuffer::from_text("fn main() {\n    let x = 1;\n}\n")
    }

    #[test]
    fn overlapping_cursors_merge() {
        let mut sel = Selection::from_cursor(Cursor { anchor: 0, head: 5 });
        sel.add(Cursor { anchor: 3, head: 8 });
        assert_eq!(sel.len(), 1, "touching cursors must collapse into one");
        assert_eq!(sel.cursors()[0].range(), 0..8);
    }

    #[test]
    fn disjoint_cursors_are_kept_and_sorted() {
        let mut sel = Selection::single(20);
        sel.add(Cursor::at(4));
        assert_eq!(sel.len(), 2);
        assert_eq!(
            sel.cursors()[0].head,
            4,
            "cursors must be in document order"
        );
    }

    #[test]
    fn vertical_motion_remembers_its_column() {
        let b = TextBuffer::from_text("longest line here\nshort\nlongest line here\n");
        let mut sel = Selection::single(15); // deep into line 0
        sel.motion(&b, Motion::Down, false); // line 1 is too short
        sel.motion(&b, Motion::Down, false); // line 2 is long again
        let head = sel.primary().head;
        let col = head - b.line_to_char(b.char_to_line(head));
        assert_eq!(
            col, 15,
            "column should be restored, not clipped to the short line"
        );
    }

    #[test]
    fn collapsing_a_selection_moves_to_its_edge() {
        let b = buf();
        let mut sel = Selection::from_cursor(Cursor { anchor: 3, head: 9 });
        sel.motion(&b, Motion::Left, false);
        assert_eq!(
            sel.primary().head,
            3,
            "left out of a selection lands on its start"
        );

        let mut sel = Selection::from_cursor(Cursor { anchor: 3, head: 9 });
        sel.motion(&b, Motion::Right, false);
        assert_eq!(
            sel.primary().head,
            9,
            "right out of a selection lands on its end"
        );
    }

    #[test]
    fn smart_home_toggles_between_indent_and_column_zero() {
        let b = buf();
        let line_start = b.line_to_char(1);
        let mut sel = Selection::single(line_start + 10);
        sel.motion(&b, Motion::LineStart, false);
        assert_eq!(
            sel.primary().head,
            line_start + 4,
            "first stop is the indent"
        );
        sel.motion(&b, Motion::LineStart, false);
        assert_eq!(sel.primary().head, line_start, "second stop is column zero");
    }

    #[test]
    fn motion_clamps_at_both_ends() {
        let b = buf();
        let mut sel = Selection::single(0);
        sel.motion(&b, Motion::Left, false);
        assert_eq!(sel.primary().head, 0);

        let mut sel = Selection::single(b.len_chars());
        sel.motion(&b, Motion::Right, false);
        assert_eq!(sel.primary().head, b.len_chars());
    }
}
