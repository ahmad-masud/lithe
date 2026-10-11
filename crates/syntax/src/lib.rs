//! Syntax highlighting, via tree-sitter.
//!
//! Two things here matter for performance, and both follow from the
//! invariants:
//!
//! - **Parsing is incremental.** An edit is reported to the tree before the
//!   reparse, so tree-sitter reuses everything it can and a keystroke costs
//!   microseconds rather than re-parsing the file.
//! - **Querying is viewport-bounded.** Captures are collected only for the
//!   byte range on screen, so highlighting a 3-million-line file costs the
//!   same as highlighting a short one.
//!
//! The parser also reads straight out of the rope in chunks, so a document
//! is never flattened into a `String` just to be parsed.

mod languages;

use std::ops::Range;

use lithe_core::{Rope, TextBuffer};
use tree_sitter::{
    InputEdit, Node, Parser, Point, QueryCursor, StreamingIterator, TextProvider, Tree,
};

pub use languages::{language_for_path, LanguageConfig};

/// What a span of text is, for colouring. Deliberately a small, closed set:
/// themes stay simple and every grammar's capture names map onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyword,
    Function,
    Type,
    Constructor,
    String,
    Escape,
    Number,
    Boolean,
    Comment,
    Constant,
    Variable,
    Property,
    Parameter,
    Operator,
    Punctuation,
    Attribute,
    Namespace,
}

/// A highlighted byte range.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

/// Maps a tree-sitter capture name onto a [`Kind`].
///
/// Grammars disagree about naming — some say `conditional`, others
/// `keyword.conditional` — so the specific names are checked first and
/// anything else falls back to the part before the first dot.
pub fn kind_for_capture(capture: &str) -> Option<Kind> {
    let kind = match capture {
        "string.escape" | "escape" => Kind::Escape,
        "constant.builtin" | "boolean" => Kind::Boolean,
        "variable.parameter" | "parameter" => Kind::Parameter,
        "variable.member" | "property" | "field" => Kind::Property,
        "type.builtin" | "type.definition" => Kind::Type,
        "function.macro" | "function.builtin" | "function.method" => Kind::Function,
        _ => match capture.split('.').next().unwrap_or(capture) {
            "keyword" | "conditional" | "repeat" | "include" | "exception" | "storageclass"
            | "keyword_modifier" => Kind::Keyword,
            "function" | "method" => Kind::Function,
            "type" => Kind::Type,
            "constructor" => Kind::Constructor,
            "string" | "character" => Kind::String,
            "escape" => Kind::Escape,
            "number" | "float" | "integer" => Kind::Number,
            "boolean" => Kind::Boolean,
            "comment" => Kind::Comment,
            "constant" => Kind::Constant,
            "variable" | "identifier" | "label" => Kind::Variable,
            "property" | "field" => Kind::Property,
            "parameter" => Kind::Parameter,
            "operator" => Kind::Operator,
            "punctuation" | "delimiter" | "bracket" => Kind::Punctuation,
            "attribute" | "annotation" | "decorator" | "tag" => Kind::Attribute,
            "namespace" | "module" => Kind::Namespace,
            // Unknown captures are left unstyled rather than guessed at.
            _ => return None,
        },
    };
    Some(kind)
}

/// Lets tree-sitter read node text for query predicates straight from the
/// rope, instead of us materialising the document.
struct RopeText<'a>(&'a Rope);

impl<'a> TextProvider<Vec<u8>> for RopeText<'a> {
    type I = std::iter::Once<Vec<u8>>;

    fn text(&mut self, node: Node<'_>) -> Self::I {
        let range =
            node.start_byte().min(self.0.len_bytes())..node.end_byte().min(self.0.len_bytes());
        std::iter::once(self.0.byte_slice(range).to_string().into_bytes())
    }
}

pub struct Syntax {
    config: &'static LanguageConfig,
    parser: Parser,
    tree: Option<Tree>,
    cursor: QueryCursor,
}

impl Syntax {
    /// Builds a highlighter for a path's language, or `None` when the
    /// extension is not one we have a grammar for.
    pub fn for_path(path: &std::path::Path) -> Option<Self> {
        let config = language_for_path(path)?;
        let mut parser = Parser::new();
        parser.set_language(&config.compiled().language).ok()?;
        Some(Self {
            config,
            parser,
            tree: None,
            cursor: QueryCursor::new(),
        })
    }

    pub fn language_name(&self) -> &'static str {
        self.config.name
    }

    /// Tells the tree what changed, before the buffer is reparsed.
    ///
    /// Edits must be reported in descending position order, matching how the
    /// buffer applies them, so earlier offsets are never invalidated.
    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(edit);
        }
    }

    /// Discards the tree, forcing a full reparse. Used after undo, where
    /// deriving the inverse edits is more trouble than it is worth.
    pub fn invalidate(&mut self) {
        self.tree = None;
    }

    /// Reparses, reusing the old tree where [`Syntax::edit`] described the
    /// changes. Reads the rope in chunks rather than flattening it.
    pub fn parse(&mut self, buffer: &TextBuffer) {
        let rope = buffer.rope();
        let mut read = |byte: usize, _: Point| -> &[u8] {
            if byte >= rope.len_bytes() {
                return &[];
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        };
        let old = self.tree.take();
        self.tree = self
            .parser
            .parse_with_options(&mut read, old.as_ref(), None)
            .or(old);
    }

    /// Collects highlight spans overlapping `range`, in paint order.
    ///
    /// Captures are appended to `out` sorted so that wider spans come first
    /// and narrower, more specific ones later — painting in this order lets
    /// the specific capture win, which is what the grammars intend.
    pub fn highlights(&mut self, buffer: &TextBuffer, range: Range<usize>, out: &mut Vec<Span>) {
        out.clear();
        let Some(tree) = self.tree.as_ref() else {
            return;
        };
        let compiled = self.config.compiled();

        // This is the line that keeps highlighting proportional to the
        // window rather than the file.
        self.cursor.set_byte_range(range.clone());

        let names = compiled.query.capture_names();
        let mut captures =
            self.cursor
                .captures(&compiled.query, tree.root_node(), RopeText(buffer.rope()));

        while let Some((m, _)) = captures.next() {
            for capture in m.captures() {
                let Some(kind) = names
                    .get(capture.index as usize)
                    .and_then(|name| kind_for_capture(name))
                else {
                    continue;
                };
                let (start, end) = (capture.node.start_byte(), capture.node.end_byte());
                if end <= range.start || start >= range.end {
                    continue;
                }
                out.push(Span { start, end, kind });
            }
        }

        out.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    }
}

/// Describes an edit to tree-sitter, in the coordinates of the buffer
/// *before* it is applied.
pub fn input_edit(
    before: &TextBuffer,
    char_start: usize,
    chars_removed: usize,
    inserted: &str,
) -> InputEdit {
    let rope = before.rope();
    let start_byte = rope.char_to_byte(char_start.min(rope.len_chars()));
    let old_end_char = (char_start + chars_removed).min(rope.len_chars());
    let old_end_byte = rope.char_to_byte(old_end_char);
    let new_end_byte = start_byte + inserted.len();

    let point_at = |byte: usize| {
        let line = rope.byte_to_line(byte);
        Point {
            row: line,
            column: byte - rope.line_to_byte(line),
        }
    };
    let start_position = point_at(start_byte);
    let old_end_position = point_at(old_end_byte);

    // The new end is derived from the inserted text, since the buffer does
    // not contain it yet.
    let newlines = inserted.matches('\n').count();
    let new_end_position = if newlines == 0 {
        Point {
            row: start_position.row,
            column: start_position.column + inserted.len(),
        }
    } else {
        Point {
            row: start_position.row + newlines,
            column: inserted.len() - inserted.rfind('\n').map(|i| i + 1).unwrap_or(0),
        }
    };

    InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position,
        old_end_position,
        new_end_position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rust(text: &str) -> (Syntax, TextBuffer) {
        let mut syntax = Syntax::for_path(Path::new("x.rs")).expect("rust grammar");
        let buffer = TextBuffer::from_text(text);
        syntax.parse(&buffer);
        (syntax, buffer)
    }

    fn kinds_in(
        syntax: &mut Syntax,
        buffer: &TextBuffer,
        range: Range<usize>,
    ) -> Vec<(String, Kind)> {
        let mut spans = Vec::new();
        syntax.highlights(buffer, range, &mut spans);
        spans
            .iter()
            .map(|s| (buffer.rope().byte_slice(s.start..s.end).to_string(), s.kind))
            .collect()
    }

    #[test]
    fn language_is_chosen_by_extension() {
        assert_eq!(
            Syntax::for_path(Path::new("a/b/main.rs")).map(|s| s.language_name()),
            Some("rust")
        );
        assert_eq!(
            Syntax::for_path(Path::new("Cargo.toml")).map(|s| s.language_name()),
            Some("toml")
        );
        assert!(
            Syntax::for_path(Path::new("notes.xyz")).is_none(),
            "unknown extensions must not guess at a grammar"
        );
    }

    #[test]
    fn keywords_strings_and_comments_are_recognised() {
        let (mut syntax, buffer) = rust("// hi\nfn main() { let s = \"x\"; }\n");
        let found = kinds_in(&mut syntax, &buffer, 0..buffer.len_bytes());

        let has = |text: &str, kind: Kind| found.iter().any(|(t, k)| t == text && *k == kind);
        assert!(has("fn", Kind::Keyword), "got {found:?}");
        assert!(has("let", Kind::Keyword), "got {found:?}");
        assert!(
            has("// hi\n", Kind::Comment) || has("// hi", Kind::Comment),
            "got {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|(t, k)| t.contains('x') && *k == Kind::String),
            "got {found:?}"
        );
    }

    #[test]
    fn querying_is_limited_to_the_requested_range() {
        let text = format!("{}fn needle() {{}}\n", "// filler\n".repeat(400));
        let (mut syntax, buffer) = rust(&text);
        let needle_at = text.find("fn needle").unwrap();

        // Ask only about the tail, and nothing from the filler should appear.
        let found = kinds_in(&mut syntax, &buffer, needle_at..buffer.len_bytes());
        assert!(
            found.iter().any(|(t, _)| t == "needle"),
            "range query missed the function name: {found:?}"
        );
        assert!(
            !found.iter().any(|(t, _)| t.contains("filler")),
            "captures leaked in from outside the range"
        );
    }

    #[test]
    fn an_incremental_edit_keeps_the_tree_correct() {
        let original = "fn main() {}\n";
        let mut buffer = TextBuffer::from_text(original);
        let mut syntax = Syntax::for_path(Path::new("x.rs")).unwrap();
        syntax.parse(&buffer);

        // Insert "  let x = 1;\n" inside the body, as the editor would.
        let at = original.find('}').unwrap();
        let inserted = "  let x = 1;\n";
        let edit = input_edit(&buffer, at, 0, inserted);
        buffer.insert(at, inserted);
        syntax.edit(&edit);
        syntax.parse(&buffer);

        let found = kinds_in(&mut syntax, &buffer, 0..buffer.len_bytes());
        assert!(
            found.iter().any(|(t, k)| t == "let" && *k == Kind::Keyword),
            "incremental reparse lost the new code: {found:?}"
        );

        // And it agrees with a parse from scratch.
        let mut fresh = Syntax::for_path(Path::new("x.rs")).unwrap();
        fresh.parse(&buffer);
        let expected = kinds_in(&mut fresh, &buffer, 0..buffer.len_bytes());
        assert_eq!(
            found.len(),
            expected.len(),
            "incremental and full parse disagree:\n{found:?}\n{expected:?}"
        );
    }

    #[test]
    fn spans_are_ordered_so_specific_captures_paint_last() {
        let (mut syntax, buffer) = rust("fn main() { foo::bar(); }\n");
        let mut spans = Vec::new();
        syntax.highlights(&buffer, 0..buffer.len_bytes(), &mut spans);

        for pair in spans.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                a.start < b.start || (a.start == b.start && a.end >= b.end),
                "spans out of paint order: {a:?} then {b:?}"
            );
        }
    }

    #[test]
    fn capture_names_map_onto_kinds() {
        assert_eq!(kind_for_capture("keyword"), Some(Kind::Keyword));
        assert_eq!(kind_for_capture("keyword.control"), Some(Kind::Keyword));
        assert_eq!(kind_for_capture("conditional"), Some(Kind::Keyword));
        assert_eq!(kind_for_capture("string.escape"), Some(Kind::Escape));
        assert_eq!(
            kind_for_capture("variable.parameter"),
            Some(Kind::Parameter)
        );
        assert_eq!(
            kind_for_capture("punctuation.bracket"),
            Some(Kind::Punctuation)
        );
        assert_eq!(
            kind_for_capture("spellcheck"),
            None,
            "unknown stays unstyled"
        );
    }
}
