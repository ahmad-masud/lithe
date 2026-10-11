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
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(binary);
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn path_lookup_finds_something_that_exists() {
        // `sh` is on PATH on every platform this builds for.
        assert!(on_path("sh"));
        assert!(!on_path("definitely-not-a-real-binary-xyzzy"));
    }
}
