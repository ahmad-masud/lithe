//! End-to-end check against a real language server.
//!
//! The unit tests cover framing and parsing in isolation; this one proves
//! the handshake, `didOpen` and `publishDiagnostics` actually work against
//! a process that follows the protocol. It skips when no server is
//! installed, because a missing server is not a broken client.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lithe_lsp::{Client, Event};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lithe-lsp-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(binary).is_file()))
        .unwrap_or(false)
}

#[test]
fn a_real_server_initialises_and_reports_diagnostics() {
    if !on_path("clangd") {
        eprintln!("skipping: clangd is not installed");
        return;
    }

    let dir = Scratch::new("c");
    let file = dir.path().join("main.c");
    // Calls an undeclared function, which every C frontend complains about.
    std::fs::write(&file, "int main(void) { return nope(); }\n").unwrap();

    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let client = Client::start("c", dir.path(), tx, wake).expect("spawn should not error");
    let Some(mut client) = client else {
        panic!("clangd is on PATH but no client was created");
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut initialised = false;
    let mut diagnostics = None;

    while Instant::now() < deadline && diagnostics.is_none() {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        match event {
            Event::Response { id, result } if !initialised => {
                // The handshake reply; everything else depends on it.
                client.pending.remove(&id);
                client.on_initialized(&result);
                initialised = true;
                let text = std::fs::read_to_string(&file).unwrap();
                client.did_open(&file, text);
            }
            Event::Diagnostics { path, items } if !items.is_empty() => {
                assert_eq!(path, file, "diagnostics came back for the wrong file");
                diagnostics = Some(items);
            }
            Event::Exited { .. } => panic!("the server exited during the handshake"),
            _ => {}
        }
    }

    assert!(initialised, "the server never answered initialize");
    let items = diagnostics.expect("no diagnostics arrived within 30s");
    assert!(
        items
            .iter()
            .any(|d| d.message.to_lowercase().contains("nope")
                || d.message.to_lowercase().contains("declar")),
        "expected a complaint about the undeclared call, got {:?}",
        items.iter().map(|d| &d.message).collect::<Vec<_>>()
    );

    client.shutdown();
}

#[test]
fn go_to_definition_answers_against_a_real_server() {
    if !on_path("clangd") {
        eprintln!("skipping: clangd is not installed");
        return;
    }

    let dir = Scratch::new("def");
    let file = dir.path().join("main.c");
    // `target` is defined on line 1 and called on line 4.
    std::fs::write(
        &file,
        "int target(void) { return 1; }\n\nint main(void) {\n    return target();\n}\n",
    )
    .unwrap();

    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut client = Client::start("c", dir.path(), tx, wake)
        .expect("spawn")
        .expect("clangd is on PATH");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut initialised = false;
    let mut asked = false;
    let mut answer = None;

    while Instant::now() < deadline && answer.is_none() {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) else {
            // Once the file is open, ask; the server may answer before it
            // has anything else to say.
            if initialised && !asked {
                let position = lithe_lsp::Position {
                    line: 3,
                    character: 11,
                };
                client.definition(&file, position);
                asked = true;
            }
            continue;
        };
        match event {
            Event::Response { id, result } if !initialised => {
                client.pending.remove(&id);
                client.on_initialized(&result);
                initialised = true;
                client.did_open(&file, std::fs::read_to_string(&file).unwrap());
            }
            Event::Response { result, .. } if asked => answer = Some(result),
            Event::Exited { .. } => panic!("the server exited"),
            _ => {}
        }
    }

    let result = answer.expect("no definition answer within 30s");
    let text = result.to_string();
    assert!(
        text.contains("main.c"),
        "the definition should point into our file, got {text}"
    );
    // Line 0 is where `target` is defined; the reply is 0-based.
    assert!(
        text.contains("\"line\":0"),
        "expected the definition on line 0, got {text}"
    );

    client.shutdown();
}

/// Drives the handshake, opens `file`, then runs `ask` and hands back
/// whatever the server answers.
///
/// Every request below needs the same ceremony, and repeating it four
/// times would make the tests about the ceremony.
fn ask_a_real_server(
    name: &str,
    source: &str,
    ask: impl FnOnce(&mut Client, &Path),
) -> Option<serde_json::Value> {
    if !on_path("clangd") {
        eprintln!("skipping: clangd is not installed");
        return None;
    }

    let dir = Scratch::new(name);
    let file = dir.path().join("main.c");
    std::fs::write(&file, source).unwrap();

    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut client = Client::start("c", dir.path(), tx, wake)
        .expect("spawn")
        .expect("clangd is on PATH");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut initialised = false;
    let mut asked = false;
    let mut answer = None;
    let mut ask = Some(ask);

    while Instant::now() < deadline && answer.is_none() {
        let event = rx.recv_timeout(Duration::from_millis(500));
        match event {
            Err(_) => {
                // The server has gone quiet, which is the cue that the
                // file is indexed and the question can go out.
                if initialised && !asked {
                    if let Some(ask) = ask.take() {
                        ask(&mut client, &file);
                    }
                    asked = true;
                }
            }
            Ok(Event::Response { id, result }) if !initialised => {
                client.pending.remove(&id);
                client.on_initialized(&result);
                initialised = true;
                client.did_open(&file, source.to_string());
            }
            Ok(Event::Response { result, .. }) if asked => answer = Some(result),
            Ok(Event::Exited { .. }) => panic!("the server exited"),
            Ok(_) => {}
        }
    }

    let answer = answer.expect("no answer within 30s");
    client.shutdown();
    Some(answer)
}

#[test]
fn references_come_back_for_every_use() {
    let source = "int target(void) { return 1; }\n\
                  int a(void) { return target(); }\n\
                  int b(void) { return target(); }\n";
    let Some(result) = ask_a_real_server("refs", source, |client, file| {
        // On `target`'s own name, line 0.
        client.references(
            file,
            lithe_lsp::Position {
                line: 0,
                character: 4,
            },
        );
    }) else {
        return;
    };

    let items = result.as_array().expect("references should be a list");
    assert!(
        items.len() >= 3,
        "the declaration and both calls, got {} in {result}",
        items.len()
    );
}

#[test]
fn formatting_comes_back_as_edits_we_can_apply() {
    // Badly spaced on purpose; clang-format has an opinion about it.
    let source = "int main(void){return    0;}\n";
    let Some(result) = ask_a_real_server("fmt", source, |client, file| {
        client.formatting(file, 4);
    }) else {
        return;
    };

    let edits = result.as_array().expect("formatting should be a list");
    assert!(!edits.is_empty(), "nothing to do on {source:?}?");
    for edit in edits {
        assert!(edit.get("range").is_some(), "an edit with no range: {edit}");
        assert!(
            edit.get("newText").is_some(),
            "an edit with no text: {edit}"
        );
    }
}

#[test]
fn a_rename_comes_back_as_a_workspace_edit() {
    let source = "int target(void) { return 1; }\nint a(void) { return target(); }\n";
    let Some(result) = ask_a_real_server("rename", source, |client, file| {
        client.rename(
            file,
            lithe_lsp::Position {
                line: 0,
                character: 4,
            },
            "renamed",
        );
    }) else {
        return;
    };

    let text = result.to_string();
    assert!(
        result.get("changes").is_some() || result.get("documentChanges").is_some(),
        "neither shape of workspace edit: {text}"
    );
    assert!(text.contains("renamed"), "the new name is not in {text}");
}

#[test]
fn the_call_hierarchy_names_the_symbol_before_its_callers() {
    let source = "int target(void) { return 1; }\nint caller(void) { return target(); }\n";
    let Some(prepared) = ask_a_real_server("calls", source, |client, file| {
        client.prepare_call_hierarchy(
            file,
            lithe_lsp::Position {
                line: 0,
                character: 4,
            },
        );
    }) else {
        return;
    };

    let items = prepared.as_array().expect("prepare should be a list");
    let first = items.first().expect("no call hierarchy item");
    assert_eq!(
        first.get("name").and_then(|n| n.as_str()),
        Some("target"),
        "got {prepared}"
    );
    assert!(
        first.get("selectionRange").is_some(),
        "an item with no selectionRange cannot be turned into a location: {first}"
    );
}

/// What a real server says opens a completion list.
///
/// The parsing has unit tests; this is the other half — that a server
/// actually sends the field, in the shape those tests assume. A default
/// of `.` would hide a mistake here, so the assertion is that clangd
/// names more than the default.
#[test]
fn a_real_server_names_its_completion_triggers() {
    if !on_path("clangd") {
        eprintln!("skipping: clangd is not installed");
        return;
    }
    let dir = Scratch::new("triggers");
    let file = dir.path().join("main.c");
    std::fs::write(
        &file,
        "struct P { int x; };\nint f(struct P *p) { return 0; }\n",
    )
    .unwrap();

    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut client = Client::start("c", dir.path(), tx, wake)
        .expect("spawn")
        .expect("clangd is on PATH");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = None;
    while Instant::now() < deadline && !client.ready {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        if let Event::Response { id, result } = event {
            client.pending.remove(&id);
            client.on_initialized(&result);
            seen = Some(result.clone());
            assert!(
                lithe_lsp::completion_triggers(&result).len() > 1,
                "clangd should name its own triggers, not fall back to the \
                 default: {result}"
            );
        }
    }
    assert!(client.ready, "the handshake never completed");

    assert!(client.triggers_completion("."), "a dot opens a member list");
    // Triggers arrive one character at a time, which is also how a
    // keystroke does: clangd names `>` and `:`, and the server works out
    // for itself that they came after `-` and `:`.
    assert!(
        client.triggers_completion(">") && client.triggers_completion(":"),
        "the halves of `->` and `::` are triggers in C: {:?}",
        seen
    );
    assert!(
        !client.triggers_completion("x"),
        "an ordinary letter is not a trigger"
    );
}

/// Typing a dot really does come back with the members.
///
/// The editor sends this request the moment a trigger character is
/// typed, so the thing worth proving is that the position just after the
/// dot is the one a server answers usefully at.
#[test]
fn a_dot_comes_back_with_the_members() {
    let source = "struct Point { int x; int y; };\nint f(struct Point p) { return p.; }\n";
    // Just after the `.` on the second line.
    let after_dot = lithe_lsp::Position {
        line: 1,
        character: 33,
    };
    let Some(answer) = ask_a_real_server("members", source, |client, file| {
        client.completion(file, after_dot);
    }) else {
        return;
    };
    let items = answer
        .get("items")
        .and_then(|i| i.as_array())
        .or_else(|| answer.as_array())
        .expect("a completion list");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label")?.as_str())
        .map(str::trim)
        .collect();
    assert!(
        labels.contains(&"x") && labels.contains(&"y"),
        "the members of Point should be offered: {labels:?}"
    );
}

/// Hovering a name really does come back with something to show.
#[test]
fn hovering_a_name_describes_it() {
    let source = "int the_answer(void) { return 42; }\nint main(void) { return the_answer(); }\n";
    // On `the_answer` where it is called.
    let on_call = lithe_lsp::Position {
        line: 1,
        character: 26,
    };
    let Some(answer) = ask_a_real_server("hovering", source, |client, file| {
        client.hover(file, on_call);
    }) else {
        return;
    };
    let text = answer
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        text.contains("the_answer"),
        "a hover should name what it is about: {answer}"
    );
    // The editor renders this through the Markdown reader, so what comes
    // back has to be Markdown rather than a bare string.
    assert_eq!(
        answer
            .get("contents")
            .and_then(|c| c.get("kind"))
            .and_then(|k| k.as_str()),
        Some("markdown")
    );
}

/// rust-analyzer answers nothing until it has loaded the workspace.
///
/// It is the one server here that does, and the reason it looked
/// broken: a hover asked the instant `initialize` is answered comes
/// back `null`, which is indistinguishable from "there is nothing to
/// say" unless the progress notifications are read. This proves both
/// halves — that it says it is working, and that it answers once it
/// has stopped.
#[test]
fn rust_analyzer_says_it_is_busy_before_it_answers() {
    if !on_path("rust-analyzer") {
        eprintln!("skipping: rust-analyzer is not installed");
        return;
    }
    let dir = Scratch::new("ra");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let file = dir.path().join("src/lib.rs");
    let source = "pub fn answer() -> u32 {\n    42\n}\n\npub fn ask() -> u32 {\n    answer()\n}\n";
    std::fs::write(&file, source).unwrap();

    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut client = Client::start("rust", dir.path(), tx, wake)
        .expect("spawn")
        .expect("rust-analyzer is on PATH");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut said_busy = false;
    let mut answer = None;
    let mut asked_at = Instant::now();

    while Instant::now() < deadline && answer.is_none() {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(400)) else {
            // Keep asking. The first answers are null; one of them will
            // not be, and that is the behaviour being pinned down.
            if client.ready && asked_at.elapsed() > Duration::from_millis(800) {
                client.hover(
                    &file,
                    lithe_lsp::Position {
                        line: 5,
                        character: 6,
                    },
                );
                asked_at = Instant::now();
            }
            continue;
        };
        match event {
            Event::Response { id, result } if !client.ready => {
                client.pending.remove(&id);
                client.on_initialized(&result);
                client.did_open(&file, source.to_string());
            }
            // The whole point: it tells us it is working.
            Event::Progress { message: Some(_) } => said_busy = true,
            Event::Response { result, .. } if !result.is_null() => answer = Some(result),
            Event::Exited { .. } => panic!("the server exited"),
            _ => {}
        }
    }

    assert!(
        said_busy,
        "rust-analyzer should report progress while it loads; without that \
         an empty answer cannot be told from an unready one"
    );
    let answer = answer.expect("no hover within 120s");
    let text = answer
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(text.contains("answer"), "got {answer}");
}
