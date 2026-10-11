//! Search: in a buffer, and across a project.
//!
//! Both use ripgrep's own engine — `grep-regex` for matching and
//! `grep-searcher` for streaming files — so project search is as fast as
//! ripgrep and in-buffer search means exactly the same thing as project
//! search. One pattern syntax, one set of case rules.
//!
//! Project search streams: results are handed to a callback as they are
//! found, never collected into a list first, so a search across a large
//! repository fills the panel as it goes and can be abandoned part-way.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use lithe_core::TextBuffer;

/// What to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub pattern: String,
    /// When false, the pattern is matched literally.
    pub regex: bool,
    /// Smart case: lower-case patterns match any case, a pattern with a
    /// capital in it is taken to mean that capital. Same rule as ripgrep.
    pub smart_case: bool,
}

impl Query {
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            regex: false,
            smart_case: true,
        }
    }

    /// Compiles the pattern. `None` for an empty pattern or a malformed
    /// regex — both mean "nothing to search for" rather than an error worth
    /// interrupting typing over.
    pub fn compile(&self) -> Option<Pattern> {
        if self.pattern.is_empty() {
            return None;
        }
        let source = if self.regex {
            self.pattern.clone()
        } else {
            regex_escape(&self.pattern)
        };
        let case_insensitive = self.smart_case && !self.pattern.chars().any(|c| c.is_uppercase());

        RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .line_terminator(Some(b'\n'))
            .build(&source)
            .map_err(|e| log::debug!("pattern {:?} did not compile: {e}", self.pattern))
            .ok()
            .map(Pattern)
    }
}

/// A compiled query.
pub struct Pattern(RegexMatcher);

impl Pattern {
    /// Byte range of the first match at or after `at` within one line.
    fn find_in_line(&self, line: &str, at: usize) -> Option<Range<usize>> {
        self.0
            .find_at(line.as_bytes(), at)
            .ok()
            .flatten()
            .map(|m| m.start()..m.end())
    }
}

/// A match found in a buffer, as char offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Every match within a char range. Used for highlighting, so it is called
/// with the visible range only and stays proportional to the window.
pub fn matches_in_range(
    buffer: &TextBuffer,
    pattern: &Pattern,
    range: Range<usize>,
    out: &mut Vec<Span>,
) {
    out.clear();
    if range.is_empty() {
        return;
    }
    let first_line = buffer.char_to_line(range.start);
    let last_line = buffer.char_to_line(range.end.min(buffer.len_chars()));

    for line_index in first_line..=last_line {
        let Some(slice) = buffer.line(line_index) else {
            break;
        };
        let line_start = buffer.line_to_char(line_index);
        let text = slice.to_string();

        let mut at = 0;
        while let Some(found) = pattern.find_in_line(&text, at) {
            if found.is_empty() {
                break;
            }
            let start = line_start + text[..found.start].chars().count();
            let end = line_start + text[..found.end].chars().count();
            if end > range.start && start < range.end {
                out.push(Span { start, end });
            }
            at = found.end;
        }
    }
}

/// The next match at or after `from`, wrapping to the top of the buffer.
///
/// Scans line by line and stops at the first hit, so jumping to the next
/// match in a huge file costs the distance to that match, not the file.
pub fn next_match(buffer: &TextBuffer, pattern: &Pattern, from: usize) -> Option<Span> {
    let total = buffer.len_lines();
    let start_line = buffer.char_to_line(from.min(buffer.len_chars()));

    for step in 0..=total {
        let line_index = (start_line + step) % total.max(1);
        let Some(slice) = buffer.line(line_index) else {
            continue;
        };
        let line_start = buffer.line_to_char(line_index);
        let text = slice.to_string();

        // On the starting line, begin past the cursor so repeated presses
        // advance instead of finding the same match forever.
        let begin = if step == 0 {
            let offset = from.saturating_sub(line_start);
            text.char_indices()
                .nth(offset)
                .map(|(i, _)| i)
                .unwrap_or(text.len())
        } else {
            0
        };

        if let Some(found) = pattern.find_in_line(&text, begin) {
            if !found.is_empty() {
                return Some(Span {
                    start: line_start + text[..found.start].chars().count(),
                    end: line_start + text[..found.end].chars().count(),
                });
            }
        }
    }
    None
}

/// The last match strictly before `from`, wrapping to the bottom.
pub fn previous_match(buffer: &TextBuffer, pattern: &Pattern, from: usize) -> Option<Span> {
    let total = buffer.len_lines().max(1);
    let start_line = buffer.char_to_line(from.min(buffer.len_chars()));

    for step in 0..=total {
        let line_index = (start_line + total - (step % total)) % total;
        let Some(slice) = buffer.line(line_index) else {
            continue;
        };
        let line_start = buffer.line_to_char(line_index);
        let text = slice.to_string();

        // Collect this line's matches and take the last one before the
        // cursor; regexes only scan forwards.
        let mut best: Option<Span> = None;
        let mut at = 0;
        while let Some(found) = pattern.find_in_line(&text, at) {
            if found.is_empty() {
                break;
            }
            let start = line_start + text[..found.start].chars().count();
            let end = line_start + text[..found.end].chars().count();
            if step == 0 && start >= from {
                break;
            }
            best = Some(Span { start, end });
            at = found.end;
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// One project-search result.
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    /// 1-based, as every tool reports it.
    pub line: u64,
    pub text: String,
}

/// Searches a project on a background thread, handing each hit to `sink`.
///
/// Returns immediately. Set `cancel` to abandon the search — the walk and
/// every file in flight check it, so retyping a query does not leave the
/// previous one burning a core.
pub fn spawn_project_search(
    root: PathBuf,
    query: Query,
    cancel: Arc<AtomicBool>,
    sink: impl Fn(Hit) + Send + 'static,
) {
    let Some(pattern) = query.compile() else {
        return;
    };
    std::thread::Builder::new()
        .name("lithe-search".into())
        .spawn(move || run(&root, &pattern, &cancel, &sink))
        .map(|_| ())
        .unwrap_or_else(|e| log::warn!("could not start the search thread: {e}"));
}

fn run(root: &Path, pattern: &Pattern, cancel: &AtomicBool, sink: &impl Fn(Hit)) {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .parents(true)
        // Matches the file tree and the fuzzy finder; see the note in
        // `tree.rs`.
        .require_git(false)
        .build();

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        // Skip binaries rather than spraying their bytes into the results.
        .binary_detection(grep_searcher::BinaryDetection::quit(0))
        .build();

    for entry in walker.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let result = searcher.search_path(
            &pattern.0,
            &path,
            UTF8(|line, text| {
                if cancel.load(Ordering::Relaxed) {
                    // Returning false ends this file early.
                    return Ok(false);
                }
                sink(Hit {
                    path: path.clone(),
                    line,
                    text: text.trim_end().to_string(),
                });
                Ok(true)
            }),
        );
        if let Err(e) = result {
            log::debug!("search skipped {}: {e}", path.display());
        }
    }
}

/// Escapes a literal so it can go through the regex engine unchanged.
fn regex_escape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len() + 8);
    for ch in literal.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> TextBuffer {
        TextBuffer::from_text("alpha beta\nGamma delta\nalpha again\n")
    }

    fn pattern(text: &str) -> Pattern {
        Query::literal(text).compile().expect("compiles")
    }

    #[test]
    fn an_empty_pattern_matches_nothing() {
        assert!(Query::literal("").compile().is_none());
    }

    #[test]
    fn literals_are_escaped_rather_than_interpreted() {
        let doc = TextBuffer::from_text("a.c and abc\n");
        let p = pattern("a.c");
        let mut out = Vec::new();
        matches_in_range(&doc, &p, 0..doc.len_chars(), &mut out);
        assert_eq!(out.len(), 1, "the dot must be literal, got {out:?}");
        assert_eq!(doc.slice_to_string(out[0].start..out[0].end), "a.c");
    }

    #[test]
    fn regex_mode_interprets_the_pattern() {
        let doc = TextBuffer::from_text("a.c and abc\n");
        let q = Query {
            pattern: "a.c".into(),
            regex: true,
            smart_case: true,
        };
        let mut out = Vec::new();
        matches_in_range(&doc, &q.compile().unwrap(), 0..doc.len_chars(), &mut out);
        assert_eq!(out.len(), 2, "the dot should match any character");
    }

    #[test]
    fn smart_case_follows_the_pattern() {
        let doc = buf();
        let mut out = Vec::new();

        matches_in_range(&doc, &pattern("gamma"), 0..doc.len_chars(), &mut out);
        assert_eq!(out.len(), 1, "lower case should match any case");

        matches_in_range(&doc, &pattern("Gamma"), 0..doc.len_chars(), &mut out);
        assert_eq!(out.len(), 1);

        matches_in_range(&doc, &pattern("GAMMA"), 0..doc.len_chars(), &mut out);
        assert_eq!(out.len(), 0, "a capital in the pattern means that capital");
    }

    #[test]
    fn highlighting_is_limited_to_the_requested_range() {
        let doc = buf();
        let second_line = doc.line_to_char(2);
        let mut out = Vec::new();
        matches_in_range(
            &doc,
            &pattern("alpha"),
            second_line..doc.len_chars(),
            &mut out,
        );
        assert_eq!(out.len(), 1, "only the alpha on the last line is in range");
        assert!(out[0].start >= second_line);
    }

    #[test]
    fn next_wraps_around_the_buffer() {
        let doc = buf();
        let p = pattern("alpha");

        let first = next_match(&doc, &p, 0).expect("a match");
        assert_eq!(first.start, 0);

        let second = next_match(&doc, &p, first.end).expect("a second match");
        assert_eq!(second.start, doc.line_to_char(2));

        // Past the last match, it comes back to the first.
        let wrapped = next_match(&doc, &p, second.end).expect("wraps");
        assert_eq!(wrapped.start, 0);
    }

    #[test]
    fn previous_walks_backwards_and_wraps() {
        let doc = buf();
        let p = pattern("alpha");
        let last = doc.line_to_char(2);

        let back = previous_match(&doc, &p, last).expect("a match before the last");
        assert_eq!(back.start, 0);

        let wrapped = previous_match(&doc, &p, 0).expect("wraps to the end");
        assert_eq!(wrapped.start, last);
    }

    #[test]
    fn searching_for_something_absent_finds_nothing() {
        let doc = buf();
        let p = pattern("zebra");
        assert!(next_match(&doc, &p, 0).is_none());
        assert!(previous_match(&doc, &p, 0).is_none());
    }
}
