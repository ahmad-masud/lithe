//! Language server management, from the editor's side.
//!
//! One server process per language, started on the first buffer that needs
//! it and shut down when the last one closes. Diagnostics are kept per file
//! so they survive closing and reopening a buffer, and so two windows
//! showing the same file agree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use lithe_lsp::{Client, Diagnostic, Event, Pending};

use crate::context::Where;

/// Above this, a buffer is not sent to a language server.
///
/// Matches the syntax-highlighting cap for the same reason: a generated
/// blob is not what a language server is for, and whole-document sync of a
/// multi-megabyte file on every keystroke is a poor trade.
const MAX_SYNCED_BYTES: usize = 2 * 1024 * 1024;

/// What came of asking a server something.
///
/// The three failures used to be one message — "no language server for
/// this file" — which is a dead end whether the language has no server at
/// all, the binary is not installed, or the server is simply still coming
/// up. They are different problems with different answers.
#[derive(Debug)]
pub enum Asked {
    /// The request went out; the reply arrives later.
    Sent,
    /// A server exists and is still handshaking or indexing.
    Starting,
    /// We know a server for this language, but it is not installed.
    NotInstalled {
        language: &'static str,
        install: &'static str,
    },
    /// No server is configured for this language at all.
    Unsupported,
}

impl std::fmt::Display for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Asked::Sent => Ok(()),
            Asked::Starting => write!(f, "the language server is still starting"),
            Asked::NotInstalled { language, install } => {
                write!(f, "no {language} language server — install it: {install}")
            }
            Asked::Unsupported => write!(f, "no language server for this file type"),
        }
    }
}

/// What a reply should be used for, beyond what the protocol says.
///
/// Two variants carry the request's own arguments, because a reply is
/// only meaningful next to what was asked: a rename reply is a set of
/// edits with no hint of the name, and a formatting reply has to be
/// matched against the buffer it came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Awaiting {
    /// `at` is where the popup points, in physical pixels, fixed when the
    /// question was asked rather than when it was answered — by then the
    /// pointer has usually moved on. `dwell` tells a question the pointer
    /// asked by resting from one the person asked by name: only the
    /// second is worth a line in the status bar when there is no answer.
    Hover {
        at: (f32, f32),
        dwell: bool,
    },
    Definition,
    Completion,
    References,
    /// One of the four "go to" questions.
    GoTo(Where),
    /// Naming the symbol, which has to happen before asking about calls.
    PrepareCalls {
        incoming: bool,
    },
    Calls {
        incoming: bool,
        item: serde_json::Value,
    },
    CodeActions {
        range: lithe_lsp::Range,
    },
    ResolveCodeAction,
    Rename(String),
    Formatting {
        path: PathBuf,
        revision: u64,
        tab_width: usize,
        /// Set for Format Selections, which is the same reply shape over
        /// a smaller range.
        range: Option<lithe_lsp::Range>,
    },
}

pub struct Lsp {
    clients: HashMap<&'static str, Client>,
    /// Languages we tried and could not start, so we only say so once.
    unavailable: Vec<&'static str>,
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Maps a request id to what the editor asked for.
    outstanding: HashMap<i64, (&'static str, Awaiting)>,
    /// What a server is busy doing, while it is doing it.
    ///
    /// rust-analyzer answers `null` to everything until it has loaded
    /// and indexed the workspace, so without this the editor reported
    /// "no information" for a few seconds after opening a Rust project
    /// and looked like it had no language server at all.
    busy: Option<String>,
}

impl Lsp {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (sender, events) = channel();
        Self {
            clients: HashMap::new(),
            unavailable: Vec::new(),
            diagnostics: HashMap::new(),
            events,
            sender,
            wake,
            outstanding: HashMap::new(),
            busy: None,
        }
    }

    /// A word for the status bar: whether this language has a server, and
    /// whether it came up. Silence here is why "is the LSP even on?" is
    /// otherwise unanswerable from inside the editor.
    /// What the server is busy with, if it has said so.
    pub fn busy(&self) -> Option<&str> {
        self.busy.as_deref()
    }

    pub fn state_of(&self, language: &str) -> &'static str {
        if self.clients.contains_key(language) {
            return "lsp";
        }
        if self.unavailable.contains(&language) {
            return "lsp missing";
        }
        if lithe_lsp::config_for(language).is_some() {
            "lsp starting"
        } else {
            "no lsp"
        }
    }

    pub fn diagnostics_for(&self, path: &Path) -> &[Diagnostic] {
        self.diagnostics.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Starts a server for `language` if one is installed and not already
    /// running. Returns false when the language has no server.
    fn ensure(&mut self, language: &'static str, root: &Path) -> bool {
        if self.clients.contains_key(language) {
            return true;
        }
        if self.unavailable.contains(&language) {
            return false;
        }
        match Client::start(language, root, self.sender.clone(), Arc::clone(&self.wake)) {
            Ok(Some(client)) => {
                log::info!("started a language server for {language}");
                self.clients.insert(language, client);
                true
            }
            Ok(None) => {
                self.unavailable.push(language);
                false
            }
            Err(e) => {
                log::warn!("could not start a server for {language}: {e:#}");
                self.unavailable.push(language);
                false
            }
        }
    }

    pub fn open(&mut self, language: &'static str, root: &Path, path: &Path, text: String) {
        if text.len() > MAX_SYNCED_BYTES || !self.ensure(language, root) {
            return;
        }
        if let Some(client) = self.clients.get_mut(language) {
            if !client.is_open(path) {
                client.did_open(path, text);
            }
        }
    }

    pub fn changed(&mut self, language: &'static str, path: &Path, text: String) {
        if text.len() > MAX_SYNCED_BYTES {
            return;
        }
        if let Some(client) = self.clients.get_mut(language) {
            client.did_change(path, text);
        }
    }

    pub fn saved(&mut self, language: &'static str, path: &Path) {
        if let Some(client) = self.clients.get_mut(language) {
            client.did_save(path);
        }
    }

    pub fn closed(&mut self, language: &'static str, path: &Path) {
        if let Some(client) = self.clients.get_mut(language) {
            client.did_close(path);
        }
        self.diagnostics.remove(path);
    }

    /// Issues a request that needs no document position, for the second
    /// half of a two-step exchange.
    pub fn follow_up(
        &mut self,
        language: &'static str,
        what: Awaiting,
        send: impl FnOnce(&mut Client) -> Option<i64>,
    ) -> bool {
        let Some(client) = self.clients.get_mut(language) else {
            return false;
        };
        match send(client) {
            Some(id) => {
                self.outstanding.insert(id, (language, what.clone()));
                true
            }
            None => false,
        }
    }

    /// Issues a request and records what it was for.
    pub fn ask(
        &mut self,
        language: &'static str,
        root: &Path,
        path: &Path,
        position: lithe_lsp::Position,
        what: Awaiting,
    ) -> Asked {
        let Some(config) = lithe_lsp::config_for(language) else {
            return Asked::Unsupported;
        };
        // Start it if it is not up yet, rather than reporting its absence:
        // a cmd-click on the first file of a language should work.
        if !self.ensure(language, root) {
            return Asked::NotInstalled {
                language,
                install: config.install,
            };
        }
        let Some(client) = self.clients.get_mut(language) else {
            return Asked::NotInstalled {
                language,
                install: config.install,
            };
        };
        if !client.ready {
            return Asked::Starting;
        }
        let id = match &what {
            Awaiting::Hover { .. } => client.hover(path, position),
            Awaiting::Definition => client.definition(path, position),
            Awaiting::Completion => client.completion(path, position),
            Awaiting::References => client.references(path, position),
            Awaiting::GoTo(which) => client.goto(which.method(), path, position),
            Awaiting::PrepareCalls { .. } => client.prepare_call_hierarchy(path, position),
            Awaiting::Calls { incoming, item } => Some(client.calls(item.clone(), *incoming)),
            Awaiting::CodeActions { range } => {
                // The diagnostics on the range are part of the question:
                // without them a server has no quick fixes to offer.
                let here = self
                    .diagnostics
                    .get(path)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|d| {
                        d.range.start.line <= range.end.line && d.range.end.line >= range.start.line
                    })
                    .filter_map(|d| serde_json::to_value(d).ok())
                    .collect();
                client.code_actions(path, *range, here)
            }
            Awaiting::ResolveCodeAction => None,
            Awaiting::Rename(to) => client.rename(path, position, to),
            Awaiting::Formatting {
                tab_width, range, ..
            } => match range {
                Some(range) => client.range_formatting(path, *range, *tab_width),
                None => client.formatting(path, *tab_width),
            },
        };
        match id {
            Some(id) => {
                self.outstanding.insert(id, (language, what));
                Asked::Sent
            }
            None => Asked::Starting,
        }
    }

    /// Whether a server for `language` is up and has finished starting.
    pub fn is_running(&self, language: &str) -> bool {
        self.clients.get(language).is_some_and(|c| c.ready)
    }

    /// Whether typing `text` in `language` should open a completion list
    /// on its own.
    ///
    /// Answered only by a server that is up and has told us its triggers.
    /// Starting one because a character was typed would mean a keystroke
    /// could spawn a process, which is not a thing typing should do.
    pub fn triggers_completion(&self, language: &str, text: &str) -> bool {
        self.clients
            .get(language)
            .is_some_and(|client| client.ready && client.triggers_completion(text))
    }

    /// Drains everything the servers have said. Returns replies the editor
    /// asked for, paired with what they were for; diagnostics are stored
    /// internally rather than returned.
    pub fn drain(&mut self) -> Vec<(Awaiting, serde_json::Value)> {
        let mut replies = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Diagnostics { path, items } => {
                    if items.is_empty() {
                        self.diagnostics.remove(&path);
                    } else {
                        self.diagnostics.insert(path, items);
                    }
                }
                Event::Response { id, result } => {
                    // The initialize handshake is ours to complete, not the
                    // editor's to see.
                    let initialising = self
                        .clients
                        .values_mut()
                        .find(|c| c.pending.get(&id) == Some(&Pending::Initialize));
                    if let Some(client) = initialising {
                        client.pending.remove(&id);
                        client.on_initialized(&result);
                        continue;
                    }
                    if let Some((language, what)) = self.outstanding.remove(&id) {
                        if let Some(client) = self.clients.get_mut(language) {
                            client.pending.remove(&id);
                        }
                        replies.push((what, result));
                    }
                }
                Event::Failed { id, message } => {
                    log::debug!("language server request {id} failed: {message}");
                    self.outstanding.remove(&id);
                }
                Event::Progress { message } => self.busy = message,
                Event::Exited { language } => {
                    // Mark it unavailable, not merely absent. A server that
                    // dies on startup — a rustup proxy whose component is
                    // not installed, say — would otherwise be respawned on
                    // the next keystroke, forever.
                    log::warn!("the {language} language server exited");
                    self.clients.remove(language);
                    if !self.unavailable.contains(&language) {
                        self.unavailable.push(language);
                    }
                    self.outstanding.retain(|_, (l, _)| *l != language);
                }
            }
        }
        replies
    }

    /// Stops every server. Called on the way out so nothing is orphaned.
    pub fn shutdown(&mut self) {
        for (_, mut client) in self.clients.drain() {
            client.shutdown();
        }
    }
}
