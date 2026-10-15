//! Find state: the in-buffer find bar, and project-wide search.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use lithe_search::{Hit, Pattern, Query};

/// Which field of the find bar is taking keystrokes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Query,
    Replace,
}

/// The in-buffer find bar.
pub struct Find {
    pub query: String,
    pub replace: String,
    /// Whether the replace field is shown and reachable.
    pub replacing: bool,
    pub focus: Field,
    /// Compiled from `query`; `None` while the query is empty or invalid.
    pub pattern: Option<Pattern>,
    /// Set when a search ran and found nothing, so the bar can say so
    /// rather than silently doing nothing.
    pub missed: bool,
}

impl Find {
    pub fn new(replacing: bool) -> Self {
        Self {
            query: String::new(),
            replace: String::new(),
            replacing,
            focus: Field::Query,
            pattern: None,
            missed: false,
        }
    }

    pub fn set_query(&mut self, query: String) {
        self.pattern = Query::literal(&query).compile();
        self.query = query;
        self.missed = false;
    }

    /// Tab moves between the two fields, when both are shown.
    pub fn toggle_field(&mut self) {
        if !self.replacing {
            return;
        }
        self.focus = match self.focus {
            Field::Query => Field::Replace,
            Field::Replace => Field::Query,
        };
    }

    pub fn push(&mut self, text: &str) {
        match self.focus {
            Field::Query => {
                let mut query = self.query.clone();
                query.push_str(text);
                self.set_query(query);
            }
            Field::Replace => self.replace.push_str(text),
        }
    }

    pub fn backspace(&mut self) {
        match self.focus {
            Field::Query => {
                let mut query = self.query.clone();
                query.pop();
                self.set_query(query);
            }
            Field::Replace => {
                self.replace.pop();
            }
        }
    }
}

/// Most results a project search will hold.
///
/// Invariant 3 says we keep no index of our own; a search result set is the
/// closest thing we have to one, so it is capped. Nobody reads the
/// two-thousandth hit — they narrow the query instead.
pub const MAX_HITS: usize = 2000;

/// A project-wide search, streaming results in from a worker thread.
pub struct ProjectSearch {
    pub query: String,
    pub hits: Vec<Hit>,
    pub selected: usize,
    /// True until the worker stops producing.
    pub running: bool,
    /// Set when the cap was reached, so the panel can say the list is
    /// truncated rather than implying it is complete.
    pub truncated: bool,
    cancel: Arc<AtomicBool>,
    receiver: Option<Receiver<Hit>>,
    root: std::path::PathBuf,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl ProjectSearch {
    pub fn new(root: std::path::PathBuf, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            running: false,
            truncated: false,
            cancel: Arc::new(AtomicBool::new(false)),
            receiver: None,
            root,
            notify,
        }
    }

    /// A result list handed to us rather than searched for, for a
    /// language server's reply.
    ///
    /// Reuses the search panel instead of growing a second list widget:
    /// "every place this symbol is used" and "every line matching this
    /// text" want exactly the same scrolling, clicking and jumping.
    pub fn fixed(
        root: std::path::PathBuf,
        notify: Arc<dyn Fn() + Send + Sync>,
        label: String,
        hits: Vec<Hit>,
    ) -> Self {
        Self {
            query: label,
            hits,
            selected: 0,
            running: false,
            truncated: false,
            cancel: Arc::new(AtomicBool::new(false)),
            receiver: None,
            root,
            notify,
        }
    }

    /// Restarts the search. The previous worker is cancelled rather than
    /// left running — retyping a query should not leave a thread per
    /// keystroke chewing through the repository.
    pub fn set_query(&mut self, query: String) {
        self.cancel.store(true, Ordering::Relaxed);
        self.hits.clear();
        self.selected = 0;
        self.truncated = false;
        self.query = query;

        let Some(_) = Query::literal(&self.query).compile() else {
            self.receiver = None;
            self.running = false;
            return;
        };

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Arc::clone(&cancel);

        let (tx, rx) = channel();
        self.receiver = Some(rx);
        self.running = true;

        let notify = Arc::clone(&self.notify);
        lithe_search::spawn_project_search(
            self.root.clone(),
            Query::literal(&self.query),
            cancel,
            move |hit| {
                if tx.send(hit).is_ok() {
                    notify();
                }
            },
        );
    }

    /// Drains whatever the worker has produced. Returns true when the list
    /// changed and the panel needs redrawing.
    pub fn tick(&mut self) -> bool {
        let Some(rx) = self.receiver.as_ref() else {
            return false;
        };
        let mut changed = false;
        loop {
            if self.hits.len() >= MAX_HITS {
                self.truncated = true;
                self.cancel.store(true, Ordering::Relaxed);
                self.running = false;
                break;
            }
            match rx.try_recv() {
                Ok(hit) => {
                    self.hits.push(hit);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    break;
                }
            }
        }
        changed
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.hits.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.hits.len() - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (self.selected + delta as usize).min(last)
        };
    }

    pub fn chosen(&self) -> Option<&Hit> {
        self.hits.get(self.selected)
    }
}

impl Drop for ProjectSearch {
    /// Closing the panel stops the work behind it.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A completion popup.
///
/// The server is asked once; typing narrows the list on this side rather
/// than issuing a request per keystroke. That keeps the popup instant and
/// keeps a slow server from making the editor feel slow.
pub struct Completion {
    all: Vec<Entry>,
    pub shown: Vec<usize>,
    pub selected: usize,
    pub prefix: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub label: String,
    /// What is actually inserted, which is not always the label.
    pub insert: String,
    pub detail: Option<String>,
}

impl Completion {
    pub fn new(all: Vec<Entry>) -> Self {
        let shown = (0..all.len()).collect();
        Self {
            all,
            shown,
            selected: 0,
            prefix: String::new(),
        }
    }

    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.shown.get(index).and_then(|i| self.all.get(*i))
    }

    pub fn chosen(&self) -> Option<&Entry> {
        self.get(self.selected)
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// Narrows to entries containing `prefix`, case-insensitively, with
    /// those that start with it first.
    pub fn narrow(&mut self, prefix: String) {
        let needle = prefix.to_lowercase();
        let mut starts = Vec::new();
        let mut contains = Vec::new();
        for (index, entry) in self.all.iter().enumerate() {
            let label = entry.label.to_lowercase();
            if needle.is_empty() || label.starts_with(&needle) {
                starts.push(index);
            } else if label.contains(&needle) {
                contains.push(index);
            }
        }
        starts.extend(contains);
        self.shown = starts;
        self.selected = 0;
        self.prefix = prefix;
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.shown.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.shown.len() - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (self.selected + delta as usize).min(last)
        };
    }
}

/// A one-line prompt in the find bar's position, for the few things that
/// need a word of input but not a whole panel.
pub struct Prompt {
    pub label: &'static str,
    pub text: String,
    pub action: PromptAction,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptAction {
    Commit,
    RenameSymbol,
    GotoLine,
}

impl Prompt {
    pub fn commit() -> Self {
        Self {
            label: "commit message",
            text: String::new(),
            action: PromptAction::Commit,
        }
    }

    pub fn goto_line() -> Self {
        Self {
            label: "line",
            text: String::new(),
            action: PromptAction::GotoLine,
        }
    }

    /// Seeded with the current name, so renaming one character of a long
    /// symbol does not mean retyping it.
    pub fn rename(current: String) -> Self {
        Self {
            label: "rename to",
            text: current,
            action: PromptAction::RenameSymbol,
        }
    }
}
