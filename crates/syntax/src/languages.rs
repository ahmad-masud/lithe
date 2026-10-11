//! The language registry.
//!
//! Each entry pairs a grammar with its highlight query. Both are compiled
//! once, on first use of that language, and cached — a `Query` is expensive
//! to build and there is no reason to do it per buffer.
//!
//! Adding a language is a dependency and one row in [`LANGUAGES`]. Note the
//! query constant is spelled `HIGHLIGHTS_QUERY` in most grammar crates and
//! `HIGHLIGHT_QUERY` in a few, which is why the name is given per entry
//! rather than assumed.

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::{Language, Query};
// Re-exported by the grammar crates; tree-sitter itself keeps it private.
use tree_sitter_language::LanguageFn;

pub struct Compiled {
    pub language: Language,
    pub query: Query,
}

pub struct LanguageConfig {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    grammar: LanguageFn,
    /// Query sources, concatenated in order.
    ///
    /// A list rather than one string because some grammars publish only
    /// their additions: `tree-sitter-typescript`'s query is meant to be
    /// appended to JavaScript's, and on its own it captures five things
    /// and leaves strings, comments and functions unhighlighted.
    highlights: &'static [&'static str],
    cache: OnceLock<Compiled>,
}

impl LanguageConfig {
    /// Compiles the grammar and its highlight query, once per language.
    ///
    /// A query that fails to compile is a bug in the grammar crate rather
    /// than something the user can fix, so it degrades to an empty query —
    /// the file still opens, just without colour.
    pub fn compiled(&self) -> &Compiled {
        self.cache.get_or_init(|| {
            let language = Language::new(self.grammar);
            let source = self.highlights.concat();
            let query = Query::new(&language, &source).unwrap_or_else(|e| {
                log::warn!("highlight query for {} failed to compile: {e}", self.name);
                Query::new(&language, "").expect("the empty query always compiles")
            });
            Compiled { language, query }
        })
    }
}

/// An explicit `static` array rather than a promoted slice literal: the
/// `OnceLock` inside each entry is interior-mutable, which blocks const
/// promotion.
static LANGUAGES: [LanguageConfig; 9] = [
    LanguageConfig {
        name: "rust",
        extensions: &["rs"],
        grammar: tree_sitter_rust::LANGUAGE,
        highlights: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "json",
        extensions: &["json", "jsonc"],
        grammar: tree_sitter_json::LANGUAGE,
        highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "toml",
        extensions: &["toml"],
        grammar: tree_sitter_toml_ng::LANGUAGE,
        highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "python",
        extensions: &["py", "pyi"],
        grammar: tree_sitter_python::LANGUAGE,
        highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "javascript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        grammar: tree_sitter_javascript::LANGUAGE,
        highlights: &[tree_sitter_javascript::HIGHLIGHT_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "typescript",
        extensions: &["ts", "mts", "cts"],
        grammar: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "tsx",
        extensions: &["tsx"],
        grammar: tree_sitter_typescript::LANGUAGE_TSX,
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "go",
        extensions: &["go"],
        grammar: tree_sitter_go::LANGUAGE,
        highlights: &[tree_sitter_go::HIGHLIGHTS_QUERY],
        cache: OnceLock::new(),
    },
    LanguageConfig {
        name: "c",
        extensions: &["c", "h"],
        grammar: tree_sitter_c::LANGUAGE,
        highlights: &[tree_sitter_c::HIGHLIGHT_QUERY],
        cache: OnceLock::new(),
    },
];

pub fn language_for_path(path: &Path) -> Option<&'static LanguageConfig> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|config| config.extensions.contains(&extension.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grammar_and_query_compiles() {
        // Catches a grammar/query version mismatch at test time rather than
        // silently dropping colour for that language at runtime.
        for config in LANGUAGES.iter() {
            let compiled = config.compiled();
            assert!(
                compiled.query.pattern_count() > 0,
                "{}: highlight query compiled to nothing, likely a grammar \
                 version mismatch",
                config.name
            );
        }
    }

    #[test]
    fn every_language_captures_the_basics() {
        // The bug this guards: a grammar whose published query is only its
        // own additions compiles fine and highlights almost nothing.
        for config in LANGUAGES.iter() {
            let names: Vec<&str> = config.compiled().query.capture_names().to_vec();
            let has = |want: &str| names.iter().any(|n| n.split('.').next() == Some(want));

            // Data formats have no keywords or functions to capture, and
            // demanding them would be demanding a wrong answer.
            let data = matches!(config.name, "json" | "toml");
            let needed: &[&str] = if data {
                &["string", "comment"]
            } else {
                &["keyword", "string", "comment", "function"]
            };
            for want in needed {
                assert!(
                    has(want),
                    "{} never captures {want}; it has only {names:?}",
                    config.name
                );
            }
        }
    }

    #[test]
    fn extensions_are_unique_across_languages() {
        let mut seen = std::collections::HashSet::new();
        for config in LANGUAGES.iter() {
            for extension in config.extensions {
                assert!(
                    seen.insert(*extension),
                    "{extension:?} is claimed by two languages"
                );
            }
        }
    }

    #[test]
    fn lookup_ignores_case_and_path() {
        assert_eq!(
            language_for_path(Path::new("/a/b/Main.RS")).map(|c| c.name),
            Some("rust")
        );
        assert_eq!(
            language_for_path(Path::new("x.tsx")).map(|c| c.name),
            Some("tsx")
        );
        assert!(language_for_path(Path::new("Makefile")).is_none());
    }
}
