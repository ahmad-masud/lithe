//! A language server client.
//!
//! One process per language, started on the first buffer that needs it and
//! killed when the last one closes — invariant 5. Their memory is theirs;
//! rust-analyzer will use gigabytes and that is its business.
//!
//! Everything is non-blocking from the editor's point of view. Two threads
//! per server move bytes, and results arrive as [`Event`]s on a channel the
//! event loop drains when it is woken. The editor never waits on a server.

mod servers;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use anyhow::{Context, Result};
use lithe_core::TextBuffer;
use lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams,
    HoverClientCapabilities, HoverParams, InitializeParams, InitializedParams, MarkupKind,
    PublishDiagnosticsParams, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use serde_json::{json, Value};

// Re-exported so callers need not depend on lsp-types directly.
pub use lsp_types::{CompletionItem, DiagnosticSeverity, Location};
pub use lsp_types::{Diagnostic, Position, Range, Uri};
pub use servers::{config_for, widen_search_path, ServerConfig};

/// What a server told us, delivered to the event loop.
#[derive(Debug)]
pub enum Event {
    Diagnostics {
        path: PathBuf,
        items: Vec<Diagnostic>,
    },
    /// A reply to a request we made, keyed by the id we sent.
    Response {
        id: i64,
        result: Value,
    },
    Failed {
        id: i64,
        message: String,
    },
    /// The server is doing something long enough to say so, or has
    /// finished doing it.
    ///
    /// rust-analyzer loads a Cargo workspace and indexes it before it
    /// can answer anything, and until it has it replies `null` to every
    /// question. Without this that is indistinguishable from "there is
    /// no answer", which is what made it look broken.
    Progress {
        /// `None` once the work is done.
        message: Option<String>,
    },
    /// The server process ended.
    Exited {
        language: &'static str,
    },
}

/// What we asked for, so a reply can be interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    Initialize,
    Hover,
    Definition,
    Completion,
    References,
    Rename,
    Formatting,
    CallHierarchy,
    Calls,
    CodeActions,
    ResolveCodeAction,
}

pub struct Client {
    pub language: &'static str,
    language_id: &'static str,
    child: Child,
    to_server: Sender<String>,
    next_id: i64,
    /// Requests we are waiting on, and what each one was.
    pub pending: HashMap<i64, Pending>,
    /// Set once `initialize` has been answered; before that a server may
    /// legally ignore everything else.
    pub ready: bool,
    /// What the server said should open a completion list on its own —
    /// `.` nearly always, plus `::`, `->` or `<` depending on the
    /// language. Read from its capabilities rather than guessed, because
    /// which characters mean "a member follows" is a fact about the
    /// language, not about the editor.
    completion_triggers: Vec<String>,
    /// Documents we have sent `didOpen` for, and their version counters.
    open: HashMap<PathBuf, i32>,
    stop: Arc<AtomicBool>,
}

impl Client {
    /// Starts a server for `language`, rooted at `root`.
    ///
    /// `Ok(None)` means no server binary is installed for that language,
    /// which is a normal state and not an error.
    pub fn start(
        language: &'static str,
        root: &Path,
        events: Sender<Event>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Option<Self>> {
        let Some(config) = servers::config_for(language) else {
            return Ok(None);
        };
        // Try each candidate until one actually stays running. Being on
        // PATH is not enough: `rust-analyzer` there is often a rustup
        // proxy for a component that was never installed, which spawns
        // happily and then exits. Without this the editor would report a
        // server it does not have.
        let mut child = None;
        for argv in config.live_commands() {
            match spawn_server(argv, root) {
                Some(running) => {
                    child = Some(running);
                    break;
                }
                None => log::info!("{language}: {} did not stay running", argv[0]),
            }
        }
        let Some(mut child) = child else {
            log::info!("no working language server for {language}");
            return Ok(None);
        };

        let stdin = child.stdin.take().context("server has no stdin")?;
        let stdout = child.stdout.take().context("server has no stdout")?;
        let stderr = child.stderr.take().context("server has no stderr")?;
        let stop = Arc::new(AtomicBool::new(false));

        let (to_server, outgoing) = channel::<String>();
        spawn_writer(stdin, outgoing);
        spawn_reader(stdout, events, wake, language, Arc::clone(&stop));
        spawn_logger(stderr, language);

        let mut client = Self {
            language,
            language_id: config.language_id,
            child,
            to_server,
            next_id: 1,
            pending: HashMap::new(),
            ready: false,
            completion_triggers: Vec::new(),
            open: HashMap::new(),
            stop,
        };
        client.initialize(root)?;
        Ok(Some(client))
    }

    fn send(&self, payload: Value) {
        if let Ok(text) = serde_json::to_string(&payload) {
            // A closed channel means the server died; the Exited event has
            // already been or will be delivered.
            let _ = self.to_server.send(text);
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value, kind: Pending) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        id
    }

    fn initialize(&mut self, root: &Path) -> Result<()> {
        let params = InitializeParams {
            // Deprecated in the protocol, still what several servers read.
            #[allow(deprecated)]
            root_uri: path_to_uri(root).ok(),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    hover: Some(HoverClientCapabilities {
                        content_format: Some(vec![MarkupKind::PlainText, MarkupKind::Markdown]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        // The rest of what we support, declared as raw JSON rather than
        // through the typed struct: these are five nested builders deep
        // for what is, in the end, five booleans, and a server reads the
        // wire format either way.
        let mut params = serde_json::to_value(params)?;
        if let Some(text_document) = params
            .get_mut("capabilities")
            .and_then(|c| c.get_mut("textDocument"))
            .and_then(Value::as_object_mut)
        {
            text_document.insert("references".into(), json!({}));
            // A server sends plaintext unless told otherwise, and the
            // editor renders a hover through the Markdown reader: without
            // this a signature arrived as a flat string instead of a
            // fenced code block, and the popup had nothing to colour.
            text_document.insert(
                "hover".into(),
                json!({ "contentFormat": ["markdown", "plaintext"] }),
            );
            text_document.insert(
                "completion".into(),
                json!({
                    // No snippet support: the editor inserts `insertText`
                    // as written, and a server told otherwise would send
                    // `${1:placeholder}` for it to type out literally.
                    "completionItem": {
                        "documentationFormat": ["markdown", "plaintext"],
                        "snippetSupport": false,
                    },
                }),
            );
            text_document.insert("declaration".into(), json!({ "linkSupport": true }));
            text_document.insert("typeDefinition".into(), json!({ "linkSupport": true }));
            text_document.insert("implementation".into(), json!({ "linkSupport": true }));
            text_document.insert("callHierarchy".into(), json!({}));
            text_document.insert("formatting".into(), json!({}));
            text_document.insert("rangeFormatting".into(), json!({}));
            text_document.insert("rename".into(), json!({ "prepareSupport": false }));
            text_document.insert(
                "codeAction".into(),
                // Without `resolveSupport` a server sends actions whose
                // edits it has not computed and will not fill in, which
                // leaves every action a no-op.
                json!({
                    "dataSupport": true,
                    "resolveSupport": { "properties": ["edit"] },
                    "codeActionLiteralSupport": {
                        "codeActionKind": { "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract",
                            "refactor.inline", "refactor.rewrite", "source",
                            "source.organizeImports",
                        ] },
                    },
                }),
            );
        }
        if let Some(capabilities) = params
            .get_mut("capabilities")
            .and_then(Value::as_object_mut)
        {
            // A server sends `$/progress` only to a client that says it
            // can receive it. Without this rust-analyzer loads a
            // workspace in silence, answering `null` to everything, and
            // there is no way to tell that from having no answer.
            capabilities.insert("window".into(), json!({ "workDoneProgress": true }));
        }
        if let Some(workspace) = params
            .get_mut("capabilities")
            .and_then(Value::as_object_mut)
        {
            // A rename touches files we have not opened.
            workspace.insert(
                "workspace".into(),
                json!({ "workspaceEdit": { "documentChanges": true } }),
            );
        }

        self.request("initialize", params, Pending::Initialize);
        Ok(())
    }

    /// Called when `initialize` is answered.
    ///
    /// The reply is the only place a server says what it can do, so this
    /// is where the completion trigger characters are kept.
    pub fn on_initialized(&mut self, result: &Value) {
        self.ready = true;
        self.completion_triggers = completion_triggers(result);
        self.notify(
            "initialized",
            serde_json::to_value(InitializedParams {}).unwrap_or(Value::Null),
        );
    }

    /// Whether typing `text` should open a completion list by itself.
    pub fn triggers_completion(&self, text: &str) -> bool {
        self.completion_triggers.iter().any(|t| t == text)
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.open.contains_key(path)
    }

    pub fn did_open(&mut self, path: &Path, text: String) {
        let Ok(uri) = path_to_uri(path) else { return };
        self.open.insert(path.to_path_buf(), 1);
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: self.language_id.to_string(),
                version: 1,
                text,
            },
        };
        if let Ok(params) = serde_json::to_value(params) {
            self.notify("textDocument/didOpen", params);
        }
    }

    /// Whole-document sync.
    ///
    /// Incremental sync would send less, but it has to agree with the
    /// server about UTF-16 ranges on every edit, and a single disagreement
    /// desynchronises the server silently for the rest of the session.
    /// Sending the text is a few microseconds and cannot drift.
    pub fn did_change(&mut self, path: &Path, text: String) {
        let Ok(uri) = path_to_uri(path) else { return };
        let Some(version) = self.open.get_mut(path) else {
            return;
        };
        *version += 1;
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri,
                version: *version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text,
            }],
        };
        if let Ok(params) = serde_json::to_value(params) {
            self.notify("textDocument/didChange", params);
        }
    }

    pub fn did_save(&mut self, path: &Path) {
        let Ok(uri) = path_to_uri(path) else { return };
        let params = DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text: None,
        };
        if let Ok(params) = serde_json::to_value(params) {
            self.notify("textDocument/didSave", params);
        }
    }

    pub fn did_close(&mut self, path: &Path) {
        let Ok(uri) = path_to_uri(path) else { return };
        if self.open.remove(path).is_none() {
            return;
        }
        let params = DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        };
        if let Ok(params) = serde_json::to_value(params) {
            self.notify("textDocument/didClose", params);
        }
    }

    fn position_params(
        &self,
        path: &Path,
        position: Position,
    ) -> Option<TextDocumentPositionParams> {
        Some(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(path).ok()?,
            },
            position,
        })
    }

    pub fn hover(&mut self, path: &Path, position: Position) -> Option<i64> {
        let params = HoverParams {
            text_document_position_params: self.position_params(path, position)?,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        Some(self.request(
            "textDocument/hover",
            serde_json::to_value(params).ok()?,
            Pending::Hover,
        ))
    }

    pub fn definition(&mut self, path: &Path, position: Position) -> Option<i64> {
        let params = GotoDefinitionParams {
            text_document_position_params: self.position_params(path, position)?,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        Some(self.request(
            "textDocument/definition",
            serde_json::to_value(params).ok()?,
            Pending::Definition,
        ))
    }

    pub fn completion(&mut self, path: &Path, position: Position) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "position": { "line": position.line, "character": position.character },
        });
        Some(self.request("textDocument/completion", params, Pending::Completion))
    }

    /// The four "go to" requests, which differ only in their method name.
    pub fn goto(&mut self, method: &'static str, path: &Path, position: Position) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "position": { "line": position.line, "character": position.character },
        });
        Some(self.request(method, params, Pending::Definition))
    }

    /// Step one of a call hierarchy: name the symbol under the cursor.
    pub fn prepare_call_hierarchy(&mut self, path: &Path, position: Position) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "position": { "line": position.line, "character": position.character },
        });
        Some(self.request(
            "textDocument/prepareCallHierarchy",
            params,
            Pending::CallHierarchy,
        ))
    }

    /// Step two: who calls it, or what it calls.
    pub fn calls(&mut self, item: Value, incoming: bool) -> i64 {
        let method = if incoming {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        self.request(method, json!({ "item": item }), Pending::Calls)
    }

    pub fn code_actions(
        &mut self,
        path: &Path,
        range: Range,
        diagnostics: Vec<Value>,
    ) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "range": range,
            "context": { "diagnostics": diagnostics },
        });
        Some(self.request("textDocument/codeAction", params, Pending::CodeActions))
    }

    /// Fills in an action a server offered without its edits.
    pub fn resolve_code_action(&mut self, action: Value) -> i64 {
        self.request("codeAction/resolve", action, Pending::ResolveCodeAction)
    }

    pub fn range_formatting(&mut self, path: &Path, range: Range, tab_width: usize) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "range": range,
            "options": { "tabSize": tab_width, "insertSpaces": true },
        });
        Some(self.request("textDocument/rangeFormatting", params, Pending::Formatting))
    }

    pub fn references(&mut self, path: &Path, position: Position) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "position": { "line": position.line, "character": position.character },
            // The symbol's own definition is a reference like any other,
            // and leaving it out makes the count look wrong.
            "context": { "includeDeclaration": true },
        });
        Some(self.request("textDocument/references", params, Pending::References))
    }

    pub fn rename(&mut self, path: &Path, position: Position, to: &str) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "position": { "line": position.line, "character": position.character },
            "newName": to,
        });
        Some(self.request("textDocument/rename", params, Pending::Rename))
    }

    pub fn formatting(&mut self, path: &Path, tab_width: usize) -> Option<i64> {
        let params = json!({
            "textDocument": { "uri": path_to_uri(path).ok()?.as_str() },
            "options": {
                "tabSize": tab_width,
                "insertSpaces": true,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": true,
            },
        });
        Some(self.request("textDocument/formatting", params, Pending::Formatting))
    }

    /// Asks the server to exit, then makes sure it does.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.send(json!({ "jsonrpc": "2.0", "id": 0, "method": "shutdown" }));
        self.notify("exit", Value::Null);
        // Servers that ignore `exit` still have to go: their memory is the
        // reason this is worth being firm about.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a candidate and gives it a moment to fall over.
///
/// A real server blocks on stdin and lives; a broken shim prints to stderr
/// and exits within milliseconds. Two hundred and fifty of them is long
/// enough to tell the two apart and short enough not to be felt, and this
/// happens once per language per session.
fn spawn_server(argv: &'static [&'static str], root: &Path) -> Option<Child> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            // Already gone: not a usable server.
            Ok(Some(_)) => return None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(15)),
            Err(_) => return None,
        }
    }
    Some(child)
}

fn spawn_writer(mut stdin: std::process::ChildStdin, outgoing: Receiver<String>) {
    std::thread::Builder::new()
        .name("lsp-write".into())
        .spawn(move || {
            while let Ok(body) = outgoing.recv() {
                let header = format!("Content-Length: {}\r\n\r\n", body.len());
                if stdin.write_all(header.as_bytes()).is_err()
                    || stdin.write_all(body.as_bytes()).is_err()
                    || stdin.flush().is_err()
                {
                    return;
                }
            }
        })
        .ok();
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    events: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    language: &'static str,
    stop: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("lsp-read".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match read_message(&mut reader) {
                    Ok(Some(message)) => {
                        if let Some(event) = classify(message) {
                            if events.send(event).is_err() {
                                return;
                            }
                            wake();
                        }
                    }
                    // Clean end of stream, or a frame we could not parse.
                    Ok(None) | Err(_) => break,
                }
            }
            if !stop.load(Ordering::Relaxed) {
                let _ = events.send(Event::Exited { language });
                wake();
            }
        })
        .ok();
}

fn spawn_logger(stderr: std::process::ChildStderr, language: &'static str) {
    std::thread::Builder::new()
        .name("lsp-log".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!("{language}: {line}");
            }
        })
        .ok();
}

/// Reads one `Content-Length` framed message.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Ok(None);
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

/// Turns a message into an [`Event`], or `None` when it needs no action.
/// Reads a `$/progress` notification into something worth showing.
///
/// Only the title and percentage, not the token: one line in the
/// status bar is all the room there is, and a server doing two things
/// at once is still just "busy".
fn progress(params: &Value) -> Option<Event> {
    let value = params.get("value")?;
    match value.get("kind").and_then(Value::as_str)? {
        "end" => Some(Event::Progress { message: None }),
        kind => {
            let title = value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("working");
            let detail = value.get("message").and_then(Value::as_str);
            let percent = value.get("percentage").and_then(Value::as_u64);
            let mut text = title.to_string();
            if let Some(detail) = detail {
                text.push_str(" · ");
                text.push_str(detail);
            }
            if let Some(percent) = percent {
                text.push_str(&format!(" {percent}%"));
            }
            // `begin` and `report` both mean "still going".
            let _ = kind;
            Some(Event::Progress {
                message: Some(text),
            })
        }
    }
}

fn classify(message: Value) -> Option<Event> {
    let id = message.get("id").and_then(Value::as_i64);

    if let Some(method) = message.get("method").and_then(Value::as_str) {
        // A request from the server. We answer nothing useful, but we must
        // not leave it waiting: several servers block their own progress on
        // a reply.
        if id.is_some() {
            return None;
        }
        if method == "textDocument/publishDiagnostics" {
            let params: PublishDiagnosticsParams =
                serde_json::from_value(message.get("params")?.clone()).ok()?;
            return Some(Event::Diagnostics {
                path: uri_to_path(&params.uri)?,
                items: params.diagnostics,
            });
        }
        if method == "$/progress" {
            return progress(message.get("params")?);
        }
        // Log messages and the rest are noise for now.
        return None;
    }

    let id = id?;
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Some(Event::Failed {
            id,
            message: text.to_string(),
        });
    }
    Some(Event::Response {
        id,
        result: message.get("result").cloned().unwrap_or(Value::Null),
    })
}

// ---- position conversion -------------------------------------------------

/// Converts a char offset into an LSP position.
///
/// LSP counts columns in UTF-16 code units, not characters and not bytes.
/// Getting this wrong puts diagnostics and definitions a few columns off on
/// any line containing an emoji or a CJK character, which is exactly the
/// kind of bug that survives for years.
/// The characters a server says should open a completion list.
///
/// A server that supports completion but names none still means `.`:
/// member access is what the character does in every language one of
/// these speaks, and leaving it out would mean typing a dot and getting
/// nothing back.
pub fn completion_triggers(initialize_result: &Value) -> Vec<String> {
    initialize_result
        .get("capabilities")
        .and_then(|c| c.get("completionProvider"))
        .and_then(|c| c.get("triggerCharacters"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|list: &Vec<String>| !list.is_empty())
        .unwrap_or_else(|| vec![".".to_string()])
}

pub fn to_position(buffer: &TextBuffer, char_index: usize) -> Position {
    let line = buffer.char_to_line(char_index);
    let line_start = buffer.line_to_char(line);
    let mut character = 0u32;
    if let Some(slice) = buffer.line(line) {
        for (i, ch) in slice.chars().enumerate() {
            if line_start + i >= char_index {
                break;
            }
            character += ch.len_utf16() as u32;
        }
    }
    Position {
        line: line as u32,
        character,
    }
}

/// The inverse: an LSP position back to a char offset.
pub fn from_position(buffer: &TextBuffer, position: Position) -> usize {
    let line = (position.line as usize).min(buffer.len_lines().saturating_sub(1));
    let line_start = buffer.line_to_char(line);
    let Some(slice) = buffer.line(line) else {
        return line_start;
    };
    let mut utf16 = 0u32;
    for (i, ch) in slice.chars().enumerate() {
        if utf16 >= position.character {
            return line_start + i;
        }
        utf16 += ch.len_utf16() as u32;
    }
    line_start + buffer.line_len_chars(line)
}

/// The char range a diagnostic covers.
pub fn range_to_chars(buffer: &TextBuffer, range: Range) -> std::ops::Range<usize> {
    let start = from_position(buffer, range.start);
    let end = from_position(buffer, range.end);
    start..end.max(start)
}

// ---- uris ----------------------------------------------------------------

pub fn path_to_uri(path: &Path) -> Result<Uri> {
    use std::str::FromStr;
    let text = path.to_string_lossy();
    let mut encoded = String::with_capacity(text.len() + 8);
    encoded.push_str("file://");
    for byte in text.bytes() {
        match byte {
            b'/' | b'-' | b'.' | b'_' | b'~' => encoded.push(byte as char),
            b if b.is_ascii_alphanumeric() => encoded.push(b as char),
            b => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    Uri::from_str(&encoded).map_err(|e| anyhow::anyhow!("bad uri {encoded:?}: {e}"))
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str().strip_prefix("file://")?;
    let mut out = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_server_names_the_characters_that_open_a_list() {
        let reply = serde_json::json!({
            "capabilities": {
                "completionProvider": { "triggerCharacters": [".", "::", "->"] }
            }
        });
        assert_eq!(completion_triggers(&reply), vec![".", "::", "->"]);
    }

    #[test]
    fn a_server_that_names_none_still_means_a_dot() {
        // Three shapes that all mean "it did not say": no list, an empty
        // list, and a `completionProvider: true` with no detail at all.
        for reply in [
            serde_json::json!({ "capabilities": { "completionProvider": {} } }),
            serde_json::json!({
                "capabilities": { "completionProvider": { "triggerCharacters": [] } }
            }),
            serde_json::json!({ "capabilities": { "completionProvider": true } }),
            serde_json::json!({}),
        ] {
            assert_eq!(
                completion_triggers(&reply),
                vec!["."],
                "typing a dot must still ask: {reply}"
            );
        }
    }
    use super::*;

    #[test]
    fn positions_count_utf16_units_not_characters() {
        // The emoji is one char but two UTF-16 units, so anything after it
        // sits two columns further along than a char count would say.
        let doc = TextBuffer::from_text("a😀b\n");
        assert_eq!(
            to_position(&doc, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            to_position(&doc, 1),
            Position {
                line: 0,
                character: 1
            }
        );
        assert_eq!(
            to_position(&doc, 2),
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(
            to_position(&doc, 3),
            Position {
                line: 0,
                character: 4
            }
        );
    }

    #[test]
    fn positions_round_trip() {
        let doc = TextBuffer::from_text("fn main() {\n    let x = 1;\n}\n");
        for offset in 0..doc.len_chars() {
            let position = to_position(&doc, offset);
            assert_eq!(from_position(&doc, position), offset, "at {offset}");
        }
    }

    #[test]
    fn positions_round_trip_through_wide_characters() {
        let doc = TextBuffer::from_text("let s = \"日本語\";\n");
        for offset in 0..doc.len_chars() {
            let position = to_position(&doc, offset);
            assert_eq!(from_position(&doc, position), offset, "at {offset}");
        }
    }

    #[test]
    fn a_position_past_the_end_clamps() {
        let doc = TextBuffer::from_text("ab\n");
        let past = Position {
            line: 99,
            character: 99,
        };
        assert!(from_position(&doc, past) <= doc.len_chars());
    }

    #[test]
    fn uris_round_trip_including_spaces() {
        let path = Path::new("/Users/someone/my projects/main.rs");
        let uri = path_to_uri(path).expect("encodes");
        assert!(uri.as_str().contains("%20"), "a space must be escaped");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn framing_reads_one_message() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":42}"#;
        let framed = format!(
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let mut reader = std::io::BufReader::new(framed.as_bytes());
        let message = read_message(&mut reader).unwrap().expect("a message");
        assert_eq!(message["result"], 42);
        // And the stream is then empty rather than looping.
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn responses_errors_and_notifications_are_told_apart() {
        let response = json!({"jsonrpc":"2.0","id":7,"result":{"ok":true}});
        assert!(matches!(
            classify(response),
            Some(Event::Response { id: 7, .. })
        ));

        let failure = json!({"jsonrpc":"2.0","id":8,"error":{"code":-1,"message":"nope"}});
        match classify(failure) {
            Some(Event::Failed { id: 8, message }) => assert_eq!(message, "nope"),
            other => panic!("expected a failure, got {other:?}"),
        }

        // A server-to-client request carries both id and method, and must
        // not be mistaken for a reply to something we sent.
        let server_request = json!({"jsonrpc":"2.0","id":9,"method":"workspace/configuration"});
        assert!(classify(server_request).is_none());

        let progress = json!({"jsonrpc":"2.0","method":"$/progress","params":{}});
        assert!(classify(progress).is_none());
    }

    #[test]
    fn diagnostics_are_decoded_with_their_path() {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/a.rs",
                "diagnostics": [{
                    "range": {"start": {"line": 1, "character": 2},
                              "end": {"line": 1, "character": 5}},
                    "severity": 1,
                    "message": "mismatched types"
                }]
            }
        });
        match classify(message) {
            Some(Event::Diagnostics { path, items }) => {
                assert_eq!(path, Path::new("/tmp/a.rs"));
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].message, "mismatched types");
            }
            other => panic!("expected diagnostics, got {other:?}"),
        }
    }
}
