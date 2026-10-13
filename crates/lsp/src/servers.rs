//! Which language server to run for which language.
//!
//! Servers are not bundled. If the binary is not on `PATH` the language
//! simply has no server, the editor says so once, and everything else keeps
//! working — an editor that refuses to open a file because rust-analyzer is
//! missing would be worse than one with no LSP at all.

/// A server we know how to start.
pub struct ServerConfig {
    /// Matches `lithe_syntax`'s language names.
    pub language: &'static str,
    /// The LSP `languageId` for this language.
    pub language_id: &'static str,
    /// Candidate binaries, in order of preference.
    pub commands: &'static [&'static [&'static str]],
    /// What to run to get one. The editor knows which binary it looked
    /// for, so telling the person is strictly better than telling them it
    /// failed.
    pub install: &'static str,
}

pub static SERVERS: &[ServerConfig] = &[
    ServerConfig {
        language: "rust",
        language_id: "rust",
        commands: &[
            &["rust-analyzer"],
            &["rustup", "run", "stable", "rust-analyzer"],
        ],
        install: "rustup component add rust-analyzer",
    },
    ServerConfig {
        language: "python",
        language_id: "python",
        commands: &[
            &["pyright-langserver", "--stdio"],
            &["pylsp"],
            &["ruff", "server"],
        ],
        install: "pip install pyright  (or ruff, or python-lsp-server)",
    },
    ServerConfig {
        language: "go",
        language_id: "go",
        commands: &[&["gopls"]],
        install: "go install golang.org/x/tools/gopls@latest",
    },
    ServerConfig {
        language: "typescript",
        language_id: "typescript",
        commands: &[&["typescript-language-server", "--stdio"]],
        install: "npm i -g typescript-language-server typescript",
    },
    ServerConfig {
        language: "tsx",
        language_id: "typescriptreact",
        commands: &[&["typescript-language-server", "--stdio"]],
        install: "npm i -g typescript-language-server typescript",
    },
    ServerConfig {
        language: "javascript",
        language_id: "javascript",
        commands: &[&["typescript-language-server", "--stdio"]],
        install: "npm i -g typescript-language-server typescript",
    },
    ServerConfig {
        language: "c",
        language_id: "c",
        commands: &[&["clangd"]],
        install: "install clangd (brew install llvm, or your package manager)",
    },
];

pub fn config_for(language: &str) -> Option<&'static ServerConfig> {
    SERVERS.iter().find(|s| s.language == language)
}

impl ServerConfig {
    /// Candidates whose binary exists on `PATH`, in preference order.
    ///
    /// Existing is necessary but not sufficient — see `spawn_server` — so
    /// this returns all of them rather than picking one.
    pub fn live_commands(&self) -> impl Iterator<Item = &'static [&'static str]> + '_ {
        self.commands
            .iter()
            .copied()
            .filter(|argv| argv.first().is_some_and(|bin| on_path(bin)))
    }
}

fn on_path(binary: &str) -> bool {
    which(binary).is_some()
}

/// The file on `PATH` a binary name resolves to.
///
/// The lookup is spelled out rather than left to the OS because Windows
/// does half of it: `CreateProcess` appends `.exe` and nothing else, so an
/// npm-installed server — `typescript-language-server.cmd`, a shim that is
/// on `PATH` and runs perfectly — is invisible to a bare
/// `Command::new("typescript-language-server")`. Resolving the name here
/// means the editor finds what the shell would find, and spawns the file
/// rather than the name.
pub fn which(binary: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    which_in(binary, &path, &extensions())
}

/// Pure, and given its inputs rather than reading them, so it can be
/// tested without changing a process-global that every other test in the
/// binary is also reading.
///
/// `extensions` is `PATHEXT`: what Windows treats as executable, in the
/// order it prefers. Empty everywhere else, where a name is just a name.
fn which_in(
    binary: &str,
    path: &std::ffi::OsStr,
    extensions: &[String],
) -> Option<std::path::PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
        extensions.iter().find_map(|ext| {
            let candidate = dir.join(format!("{binary}{ext}"));
            candidate.is_file().then_some(candidate)
        })
    })
}

#[cfg(windows)]
fn extensions() -> Vec<String> {
    let listed = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    listed
        .split(';')
        .filter(|ext| ext.starts_with('.'))
        .map(str::to_owned)
        .collect()
}

#[cfg(not(windows))]
fn extensions() -> Vec<String> {
    Vec::new()
}

/// Puts the tools a person actually has back on `PATH`.
///
/// macOS hands a GUI application launchd's `PATH`, not the shell's:
/// `/usr/bin:/bin:/usr/sbin:/sbin` and nothing else. Every language
/// server installed into a user directory — rust-analyzer in
/// `~/.cargo/bin`, gopls in `~/go/bin` — is then invisible, and the
/// editor reports "lsp missing" for a server that is installed and
/// working. Run from a terminal the same build finds everything,
/// which is what makes this so confusing to hit.
///
/// The process environment is changed rather than a private search
/// list kept, because the servers need it too: rust-analyzer shells
/// out to `cargo` to read a workspace, and a `cargo` it cannot find
/// leaves it running but useless.
///
/// Must be called before any threads are started — it mutates
/// process-global state.
pub fn widen_search_path() {
    // Not Windows: Explorer hands a process the `PATH` the person's own
    // environment holds, there is no login shell to ask what it should
    // have been, and none of the directories guessed at below exist.
    if cfg!(windows) {
        return;
    }
    let Some(current) = std::env::var_os("PATH") else {
        return;
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&current).collect();
    // Asking the login shell costs a subprocess, so it is only asked
    // when the `PATH` we were given looks like one nobody chose.
    let from_shell = if looks_stripped(&dirs, home.as_deref()) {
        login_shell_path()
    } else {
        Vec::new()
    };
    if let Some(widened) = widened(&current, home.as_deref(), &from_shell) {
        // SAFETY: called from `main` before any thread is started.
        unsafe { std::env::set_var("PATH", widened) };
    }
}

/// The widened `PATH`, or `None` when a shell evidently built it and
/// there is nothing to add.
///
/// Pure, and given its inputs rather than reading them, so it can be
/// tested without changing a process-global that every other test in
/// the binary is also reading.
fn widened(
    current: &std::ffi::OsStr,
    home: Option<&std::path::Path>,
    from_shell: &[std::path::PathBuf],
) -> Option<std::ffi::OsString> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::split_paths(current).collect();
    if !looks_stripped(&dirs, home) {
        return None;
    }

    let mut add = |dir: std::path::PathBuf| {
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    // What the login shell says, which is the only answer that covers
    // version managers and hand-rolled setups.
    for dir in from_shell {
        add(dir.clone());
    }
    // Then the usual places, for when there is no shell to ask or it
    // took too long to answer.
    if let Some(home) = home {
        for suffix in [
            ".cargo/bin",
            "go/bin",
            ".local/bin",
            ".bun/bin",
            ".deno/bin",
        ] {
            add(home.join(suffix));
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        add(std::path::PathBuf::from(dir));
    }
    std::env::join_paths(&dirs).ok()
}

/// Whether `PATH` looks like the one launchd gives a GUI app rather
/// than one a shell built.
///
/// A shell's `PATH` almost always holds something under `$HOME` or a
/// package manager's prefix. launchd's holds four system directories.
fn looks_stripped(dirs: &[std::path::PathBuf], home: Option<&std::path::Path>) -> bool {
    !dirs.iter().any(|dir| {
        home.is_some_and(|home| dir.starts_with(home))
            || dir.starts_with("/opt/homebrew")
            || dir.starts_with("/usr/local")
    })
}

/// `PATH` as the person's login shell builds it.
///
/// Empty when there is no shell, it fails, or it takes too long: a
/// slow `.zshrc` must not hold up the editor, so the answer is given
/// up on rather than waited for.
fn login_shell_path() -> Vec<std::path::PathBuf> {
    let Some(shell) = std::env::var_os("SHELL") else {
        return Vec::new();
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // A thread, because `Command` cannot be waited on with a deadline.
    // One that outlives the wait is left to finish on its own.
    std::thread::Builder::new()
        .name("lithe-path".into())
        .spawn(move || {
            let out = std::process::Command::new(shell)
                // Login and interactive, which is where `PATH` is set.
                // `command printf` steps past a shell function of the
                // same name, and the marker separates our answer from
                // whatever the profile printed on its way past.
                .args(["-lic", "command printf '<<PATH>>%s' \"$PATH\""])
                .output();
            let _ = tx.send(out);
        })
        .ok();

    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(2)) else {
        log::debug!("no PATH from the login shell; falling back to the usual places");
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(answer) = text.rsplit("<<PATH>>").next() else {
        return Vec::new();
    };
    std::env::split_paths(answer.trim()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS hands a GUI app launchd's `PATH`, so a server installed
    /// into a user directory is invisible and the editor reports "lsp
    /// missing" for one that is installed and working.
    ///
    /// Unix-only, as the behaviour is: these spell out `PATH` with `:`
    /// between the directories and `/` inside them, which is the shape
    /// the function is there to repair.
    #[cfg(not(windows))]
    #[test]
    fn a_launchd_path_is_widened_back_to_the_usual_places() {
        let home = std::env::temp_dir().join("lithe-path-fixture");
        let cargo = home.join(".cargo/bin");
        std::fs::create_dir_all(&cargo).unwrap();

        let stripped = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        let widened = widened(&stripped, Some(&home), &[]).expect("should widen");
        let dirs: Vec<_> = std::env::split_paths(&widened).collect();

        assert!(
            dirs.contains(&cargo),
            "~/.cargo/bin should be back: {dirs:?}"
        );
        assert!(
            dirs.starts_with(&[std::path::PathBuf::from("/usr/bin")]),
            "what was there already comes first: {dirs:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_shell_built_path_is_left_alone() {
        let home = std::path::Path::new("/Users/someone");
        // Anything under $HOME, or a package prefix, means a shell set
        // this up and there is nothing to add.
        for path in [
            "/usr/bin:/Users/someone/.cargo/bin",
            "/usr/bin:/opt/homebrew/bin",
            "/usr/local/bin:/usr/bin",
        ] {
            assert!(
                widened(&std::ffi::OsString::from(path), Some(home), &[]).is_none(),
                "{path} should be left alone"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn what_the_login_shell_says_wins_over_the_guesses() {
        let home = std::env::temp_dir().join("lithe-path-shell");
        let exotic = home.join("mise/shims");
        std::fs::create_dir_all(&exotic).unwrap();

        let stripped = std::ffi::OsString::from("/usr/bin:/bin");
        let widened =
            widened(&stripped, Some(&home), std::slice::from_ref(&exotic)).expect("should widen");
        let dirs: Vec<_> = std::env::split_paths(&widened).collect();
        assert!(
            dirs.contains(&exotic),
            "a version manager's shims are only ever found this way: {dirs:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn every_language_maps_to_at_least_one_command() {
        for server in SERVERS {
            assert!(
                !server.commands.is_empty(),
                "{} has no candidate commands",
                server.language
            );
            for argv in server.commands {
                assert!(!argv.is_empty(), "{} has an empty command", server.language);
            }
        }
    }

    #[test]
    fn languages_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for server in SERVERS {
            assert!(
                seen.insert(server.language),
                "{} listed twice",
                server.language
            );
        }
    }

    #[test]
    fn every_server_says_how_to_install_it() {
        // A missing server is the common case on a fresh machine, and the
        // editor is the only thing that knows which binary it wanted.
        for server in SERVERS {
            assert!(
                !server.install.is_empty(),
                "{} has no install hint",
                server.language
            );
            let binary = server.commands[0][0];
            assert!(
                server.install.contains(binary)
                    || server.install.contains("clangd")
                    || server.install.contains("pyright"),
                "{}: the hint {:?} does not mention {binary}",
                server.language,
                server.install
            );
        }
    }

    #[test]
    fn an_unknown_language_has_no_server() {
        assert!(config_for("cobol").is_none());
        assert!(config_for("rust").is_some());
    }

    #[test]
    fn candidates_are_offered_in_preference_order() {
        let rust = config_for("rust").unwrap();
        // Both rust-analyzer candidates are listed; which one survives is
        // decided by spawning, not by this.
        assert_eq!(rust.commands.len(), 2);
        assert_eq!(rust.commands[0][0], "rust-analyzer");
    }

    /// The case Windows gets wrong on its own: an npm-installed server is
    /// a `.cmd` shim, which the shell finds through `PATHEXT` and
    /// `CreateProcess` — which only ever appends `.exe` — does not.
    #[test]
    fn a_name_resolves_through_the_extensions_a_shell_would_try() {
        let dir = std::env::temp_dir().join("lithe-which-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        // Spelled as `PATHEXT` spells it, so the two match on a
        // case-sensitive filesystem as well as on Windows' own.
        let shim = dir.join("probe-server.CMD");
        std::fs::write(&shim, "@echo off\n").unwrap();

        let path = std::ffi::OsString::from(&dir);
        let pathext = [".EXE".to_string(), ".CMD".to_string()];
        assert_eq!(which_in("probe-server", &path, &pathext), Some(shim));
        // Without the extensions — which is every other platform — the
        // name on its own is all there is, and it is not a file.
        assert_eq!(which_in("probe-server", &path, &[]), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_lookup_finds_something_that_exists() {
        // One per platform that is always installed and always on `PATH`.
        // `cmd` is also the case Windows gets wrong without `PATHEXT`:
        // the file is `cmd.exe`, and the name is not.
        let always = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(on_path(always), "{always} should be on PATH");
        assert!(!on_path("definitely-not-a-real-binary-xyzzy"));
    }
}
