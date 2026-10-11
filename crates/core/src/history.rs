//! Undo history.
//!
//! An edit is stored with both the text it removed and the text it inserted,
//! so undo and redo are the same operation with the two swapped. Changes
//! within one transaction carry offsets into the document as it was *before*
//! the transaction, and are applied back-to-front so earlier offsets stay
//! valid — which is also what makes multi-cursor edits work.

use crate::selection::Selection;

/// One contiguous replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Char index into the pre-transaction document.
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self {
            at,
            removed: String::new(),
            inserted: text.into(),
        }
    }

    pub fn delete(at: usize, removed: impl Into<String>) -> Self {
        Self {
            at,
            removed: removed.into(),
            inserted: String::new(),
        }
    }
}

/// One undo step: a set of changes plus where the cursors were on each side
/// of it, so undo restores the selection as well as the text.
#[derive(Clone, Debug)]
pub struct Transaction {
    /// Sorted ascending by `at`, non-overlapping.
    pub changes: Vec<Change>,
    pub before: Selection,
    pub after: Selection,
}

impl Transaction {
    pub fn new(mut changes: Vec<Change>, before: Selection, after: Selection) -> Self {
        changes.sort_by_key(|c| c.at);
        Self {
            changes,
            before,
            after,
        }
    }

    fn is_plain_typing(&self) -> bool {
        self.changes.len() == 1
            && self.changes[0].removed.is_empty()
            && self.changes[0].inserted.chars().count() == 1
            && !self.changes[0].inserted.contains('\n')
    }
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Records a transaction, merging it into the previous one when the two
    /// are consecutive single-character typing. Without this, undo after a
    /// sentence would step back one letter at a time.
    pub fn push(&mut self, transaction: Transaction) {
        self.redo.clear();

        if transaction.is_plain_typing() {
            if let Some(prev) = self.undo.last_mut() {
                let prev_is_typing = prev.changes.len() == 1
                    && prev.changes[0].removed.is_empty()
                    && !prev.changes[0].inserted.contains('\n');
                let adjacent = prev_is_typing
                    && prev.changes[0].at + prev.changes[0].inserted.chars().count()
                        == transaction.changes[0].at;
                if adjacent {
                    prev.changes[0]
                        .inserted
                        .push_str(&transaction.changes[0].inserted);
                    prev.after = transaction.after;
                    return;
                }
            }
        }
        self.undo.push(transaction);
    }

    pub fn pop_undo(&mut self) -> Option<Transaction> {
        let t = self.undo.pop()?;
        self.redo.push(t.clone());
        Some(t)
    }

    pub fn pop_redo(&mut self) -> Option<Transaction> {
        let t = self.redo.pop()?;
        self.undo.push(t.clone());
        Some(t)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
