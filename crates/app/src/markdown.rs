//! A small Markdown renderer for the preview pane.
//!
//! Deliberately not a conforming CommonMark implementation: it handles the
//! constructs people actually put in a README and leaves the rest as plain
//! text. A half-correct parser that silently mangles input would be worse
//! than one with an honest boundary.
//!
//! Output is a flat list of laid-out lines, so the preview scrolls and
//! draws much as the editor does — a line is one scroll step, and a
//! heading is a line that happens to be two grid rows tall.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// How a run of text is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Body,
    Heading(u8),
    Bold,
    Emphasis,
    /// Both at once. `***x***` and `**_x_**` are common in a README, and
    /// collapsing them to one or the other loses half of what was written.
    BoldEmphasis,
    Code,
    Quote,
    Link,
    Bullet,
    /// Struck through, from `~~text~~`.
    Strike,
    /// A run inside a fenced block, coloured by the grammar named on the
    /// fence. The preview highlights code for the same reason the editor
    /// does, and from the same tree-sitter queries.
    Syntax(lithe_syntax::Kind),
    /// A cell divider in a table, dimmed so the contents read first.
    Divider,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// Decoration drawn behind or beside a line, which spans cannot express.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Block {
    #[default]
    Plain,
    /// Tinted background, as a fenced block.
    Code,
    /// A vertical bar down the left, as a quotation.
    Quote,
    /// A hairline across the column, drawn rather than typed.
    Rule,
    /// A hairline under a heading, as under an `<h1>`.
    HeadingRule,
    /// A table's header: tinted, inside the table's own border.
    TableHead,
    /// A row of a rendered Mermaid diagram. Tinted like code, because it
    /// came from a fence and is copied back out as its source.
    Diagram,
}

/// How tall a line is, in grid rows, and how large its glyphs are.
///
/// Two rows rather than a fractional height: the preview keeps the
/// editor's model, where scrolling counts lines and a line sits on the
/// grid, and a top-level heading earns the extra row for the air around
/// it as much as for the glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scale {
    #[default]
    Body,
    Large,
    Huge,
}

impl Scale {
    /// Grid rows this scale occupies.
    pub fn rows(self) -> usize {
        match self {
            Self::Body => 1,
            Self::Large | Self::Huge => 2,
        }
    }

    /// Cell width relative to a body cell.
    ///
    /// Nominal, and deliberately so: the real advance is whatever the
    /// face measures at that size, which only the renderer knows. These
    /// have to agree with `lithe_gpu::Size`, and being a shade generous
    /// costs an early wrap rather than a heading running off the column.
    fn width(self) -> f32 {
        match self {
            Self::Body => 1.0,
            Self::Large => 1.45,
            Self::Huge => 1.9,
        }
    }
}

/// One laid-out line of the preview, already wrapped.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub spans: Vec<Span>,
    /// Leading indent in cells.
    pub indent: usize,
    pub block: Block,
    /// Glyph size, and so the row height. Only headings are ever anything
    /// but [`Scale::Body`].
    pub scale: Scale,
    /// The verbatim source of the fenced block this row belongs to.
    ///
    /// Carried on every row of the block rather than only its first, so
    /// a click anywhere in it can copy it without having to walk back to
    /// find where it started. An `Arc` because every row of one block
    /// shares the same text — cloning a row costs a refcount.
    pub code: Option<Arc<str>>,
}

impl Line {
    fn decorated(self, block: Block) -> Self {
        Self { block, ..self }
    }

    pub fn is_blank(&self) -> bool {
        self.block == Block::Plain && self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Grid rows this line occupies.
    pub fn rows(&self) -> usize {
        self.scale.rows()
    }
}

/// Whether the preview knows how to render this file.
pub fn previewable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown"))
}

/// Highlights a fenced block's contents with the grammar for `language`.
///
/// Returns one line of spans per line of code, or `None` when no grammar
/// matches the fence — an unlabelled or unknown fence stays plain rather
/// than being coloured by a guess.
///
/// The parse is thrown away afterwards. A fence is tens of lines, the
/// result is cached with the rendered preview, and keeping a parser per
/// code block alive would cost more than re-running this on the rare
/// edit that invalidates it.
fn highlight_fence(language: &str, code: &str) -> Option<Vec<Vec<Span>>> {
    use lithe_core::TextBuffer;

    let name = language.trim().to_ascii_lowercase();
    let extension = match name.as_str() {
        // What people actually write on a fence, where it differs from
        // the file extension the language registry keys on.
        "rust" => "rs",
        "python" => "py",
        "javascript" => "js",
        "typescript" => "ts",
        "golang" => "go",
        "shell" | "sh" | "bash" | "zsh" | "console" | "text" | "txt" | "" => return None,
        other => other,
    };
    let probe = std::path::PathBuf::from(format!("fence.{extension}"));
    let mut syntax = lithe_syntax::Syntax::for_path(&probe)?;

    let buffer = TextBuffer::from_text(code);
    syntax.parse(&buffer);
    let mut spans = Vec::new();
    syntax.highlights(&buffer, 0..buffer.len_chars(), &mut spans);

    // Captures come back in byte order and may nest; the innermost one
    // wins, which for these queries is the last to start at a byte.
    let mut kind_at: Vec<Option<lithe_syntax::Kind>> = vec![None; code.len()];
    for span in spans {
        for slot in kind_at
            .get_mut(span.start..span.end.min(code.len()))
            .unwrap_or_default()
        {
            *slot = Some(span.kind);
        }
    }

    Some(
        code.lines()
            .scan(0usize, |offset, text| {
                let start = *offset;
                *offset += text.len() + 1;
                Some((start, text))
            })
            .map(|(start, text)| runs(text, start, &kind_at))
            .collect(),
    )
}

/// Splits one line of code into runs of a single highlight kind.
fn runs(text: &str, start: usize, kind_at: &[Option<lithe_syntax::Kind>]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, ch) in text.char_indices() {
        let kind = kind_at.get(start + i).copied().flatten();
        let style = kind.map_or(Style::Code, Style::Syntax);
        match out.last_mut() {
            Some(last) if last.style == style => last.text.push(ch),
            _ => out.push(Span {
                text: ch.to_string(),
                style,
            }),
        }
    }
    out
}

/// One attribute's value out of a raw inline tag, however it is quoted.
///
/// Markdown passes HTML through, and `<img src=... alt=...>` is how a
/// README puts a picture in the middle of a paragraph, so the one tag
/// that carries information this reader shows still needs reading.
fn attribute(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let mut from = 0;
    // The name as a whole word followed by `=`, so `src` does not match
    // `data-src`.
    let at = loop {
        let at = lower[from..].find(name)? + from;
        let before_ok = at == 0
            || lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = lower[at + name.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            break at;
        }
        from = at + name.len();
    };
    let rest = raw[at + name.len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let value = match rest.chars().next()? {
        quote @ ('"' | '\'') => rest[1..].split(quote).next()?.to_string(),
        _ => rest
            .split(|c: char| c.is_whitespace() || c == '>')
            .next()?
            .to_string(),
    };
    Some(decode(&value))
}

/// The entities that actually appear in hand-written Markdown.
fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        // Last, so an escaped ampersand does not re-decode.
        .replace("&amp;", "&")
}

/// The glyph size a heading is drawn at.
///
/// `h1` and `h2` get real size, the rest get weight. A preview where
/// every heading is bold body text reads as one undifferentiated column,
/// which is most of what made this not look like a document.
fn heading_scale(level: u8) -> Scale {
    match level {
        1 => Scale::Huge,
        2 => Scale::Large,
        _ => Scale::Body,
    }
}

/// The bullet for a list at `depth`, counting from 1.
///
/// Three glyphs, cycling, as a browser does: the shape is how you tell
/// which level you are reading when the indent alone is two cells.
fn bullet(depth: usize) -> char {
    match depth.max(1) % 3 {
        1 => '•',
        2 => '◦',
        _ => '▪',
    }
}

/// An image, which the preview names rather than draws.
///
/// A picture inside flowed text would need the preview to lay out rows of
/// unequal height around it; naming it keeps the one-line-per-row model
/// and still tells you what is there, which for the badges at the top of
/// a README is all there was to say.
fn picture(alt: &str) -> Span {
    Span {
        text: format!("▣ {alt}"),
        style: Style::Link,
    }
}

/// How a column's cells sit in their width.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Left,
    Centre,
    Right,
}

impl Align {
    /// Reads one cell of a `|:---|:--:|---:|` delimiter row.
    fn of(cell: &str) -> Self {
        match (cell.starts_with(':'), cell.ends_with(':')) {
            (true, true) => Self::Centre,
            (false, true) => Self::Right,
            _ => Self::Left,
        }
    }

    /// Spaces to put before and after a cell of `used` cells in `width`.
    fn pad(self, used: usize, width: usize) -> (usize, usize) {
        let slack = width.saturating_sub(used);
        match self {
            Self::Left => (0, slack),
            Self::Right => (slack, 0),
            Self::Centre => (slack / 2, slack - slack / 2),
        }
    }
}

/// A table being collected, and then laid out into a monospace grid.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<Span>>>,
    /// Which of `rows` are headings.
    head: Vec<bool>,
    /// Per column, from the delimiter row. Empty means all left.
    aligns: Vec<Align>,
}

impl Table {
    fn row(&mut self) {
        self.rows.push(Vec::new());
        self.head.push(false);
    }

    /// Marks the row being collected as a heading row.
    fn heading_row(&mut self) {
        if self.rows.is_empty() {
            self.row();
        }
        if let Some(last) = self.head.last_mut() {
            *last = true;
        }
    }

    fn cell(&mut self, spans: Vec<Span>) {
        if self.rows.is_empty() {
            self.row();
        }
        if let Some(row) = self.rows.last_mut() {
            row.push(spans);
        }
    }

    fn align(&self, column: usize) -> Align {
        self.aligns.get(column).copied().unwrap_or_default()
    }

    /// One line per row, inside a drawn frame.
    ///
    /// Emits lines directly rather than through [`wrap`]: the padding is
    /// what holds the columns in line, and re-flowing a row at word
    /// boundaries would take the table apart.
    ///
    /// Widths come from the content, then shrink to fit. A table is the
    /// one construct here whose shape depends on every line of it, which
    /// is why it is collected whole before anything is emitted.
    fn lay_out(self, columns: usize, indent: usize) -> Vec<Line> {
        const MIN: usize = 3;

        let count = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if count == 0 {
            return Vec::new();
        }
        let mut widths = vec![MIN; count];
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width_of(cell));
            }
        }

        // A frame costs one cell per edge and three between columns:
        // `│ a │ b │`.
        let frame = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * count + 1;

        // Shrink the widest column, repeatedly, until the row fits.
        // Taking from the widest keeps a narrow column readable instead
        // of trimming every column into uselessness.
        let room = columns.saturating_sub(indent);
        while frame(&widths) > room {
            let Some(widest) = widths
                .iter()
                .enumerate()
                .max_by_key(|(_, w)| **w)
                .map(|(i, _)| i)
            else {
                break;
            };
            if widths[widest] <= MIN {
                break;
            }
            widths[widest] -= 1;
        }

        let rule = |left: char, join: char, right: char| {
            let mut text = String::from(left);
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    text.push(join);
                }
                text.extend(std::iter::repeat_n('─', width + 2));
            }
            text.push(right);
            Line {
                spans: vec![Span {
                    text,
                    style: Style::Divider,
                }],
                indent,
                block: Block::Plain,
                scale: Scale::Body,
                code: None,
            }
        };

        let mut out = Vec::with_capacity(self.rows.len() + 3);
        out.push(rule('┌', '┬', '┐'));

        // Aligns is read per cell below, so it is taken before the rows
        // move out of `self`.
        let aligns: Vec<Align> = (0..count).map(|i| self.align(i)).collect();
        let mut rows = self.rows.into_iter().zip(self.head).peekable();
        while let Some((row, heading)) = rows.next() {
            let mut spans: Vec<Span> = Vec::new();
            for (i, width) in widths.iter().enumerate() {
                spans.push(Span {
                    text: if i == 0 {
                        "│ ".into()
                    } else {
                        " │ ".into()
                    },
                    style: Style::Divider,
                });
                let cell = row.get(i).cloned().unwrap_or_default();
                let used = width_of(&cell).min(*width);
                let (before, after) = aligns[i].pad(used, *width);
                if before > 0 {
                    spans.push(Span {
                        text: " ".repeat(before),
                        style: Style::Body,
                    });
                }
                for mut span in clip(cell, *width) {
                    if heading && span.style == Style::Body {
                        span.style = Style::Bold;
                    }
                    spans.push(span);
                }
                if after > 0 {
                    spans.push(Span {
                        text: " ".repeat(after),
                        style: Style::Body,
                    });
                }
            }
            spans.push(Span {
                text: " │".into(),
                style: Style::Divider,
            });
            out.push(Line {
                spans,
                indent,
                block: if heading {
                    Block::TableHead
                } else {
                    Block::Plain
                },
                scale: Scale::Body,
                code: None,
            });
            // A heading is separated from the body, but only when there
            // is a body to separate it from.
            if heading && rows.peek().is_some() {
                out.push(rule('├', '┼', '┤'));
            }
        }
        out.push(rule('└', '┴', '┘'));
        out
    }
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

/// Truncates a cell to `width`, marking the cut with an ellipsis.
fn clip(spans: Vec<Span>, width: usize) -> Vec<Span> {
    if width_of(&spans) <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let room = width.saturating_sub(used + 1);
        if room == 0 {
            break;
        }
        let len = span.text.chars().count();
        if len <= room {
            used += len;
            out.push(span);
            continue;
        }
        let text: String = span.text.chars().take(room).collect();
        out.push(Span { text, ..span });
        break;
    }
    out.push(Span {
        text: "…".into(),
        style: Style::Body,
    });
    out
}

/// A row of a pipe table, split on unescaped `|`.
fn table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);

    let mut cells = vec![String::new()];
    let mut escaped = false;
    for ch in trimmed.chars() {
        match ch {
            '\\' if !escaped => escaped = true,
            '|' if !escaped => cells.push(String::new()),
            _ => {
                if escaped && ch != '|' {
                    if let Some(last) = cells.last_mut() {
                        last.push('\\');
                    }
                }
                escaped = false;
                if let Some(last) = cells.last_mut() {
                    last.push(ch);
                }
            }
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Whether `line` is a table's `|---|:--:|` delimiter row.
fn is_table_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('-') || !trimmed.contains('|') {
        return false;
    }
    table_cells(trimmed).iter().all(|cell| {
        !cell.is_empty()
            && cell.starts_with([':', '-'])
            && cell.ends_with([':', '-'])
            && cell.chars().all(|c| c == '-' || c == ':')
    })
}

/// Renders Markdown into lines wrapped to `columns`.
pub fn render(source: &str, columns: usize) -> Vec<Line> {
    let columns = columns.max(20);
    let mut out = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    // Front matter is metadata for whatever publishes the file, not part
    // of the document, so it is hidden the way a renderer hides it rather
    // than shown as a rule and a list of keys.
    let mut i = if lines.first().is_some_and(|l| l.trim_end() == "---") {
        lines
            .iter()
            .skip(1)
            .position(|l| matches!(l.trim_end(), "---" | "..."))
            .map(|at| at + 2)
            .unwrap_or(0)
    } else {
        0
    };

    let refs = reference_labels(&lines);
    // Paragraph being accumulated, and the indent it belongs at — a
    // continuation line inside a list item keeps the item's indent.
    let mut paragraph: Vec<Span> = Vec::new();
    let mut paragraph_indent = 0usize;
    let mut list_pad: Option<usize> = None;

    while i < lines.len() {
        let raw = lines[i];
        i += 1;
        // Two trailing spaces or a trailing backslash is a hard break, so
        // whether the line had them is read before anything trims.
        let hard_break = raw.ends_with("  ") || raw.ends_with('\\');
        let trimmed = raw.trim_end();
        let bare = trimmed.trim();

        // Fenced code wins over everything: inside a fence, nothing is
        // markup.
        if let Some(fence) = fence_open(bare) {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            let mut code: Vec<&str> = Vec::new();
            while i < lines.len() {
                let line = lines[i];
                i += 1;
                if fence_closes(line.trim(), fence.0) {
                    break;
                }
                code.push(line);
            }
            // The source as written, which is what a copy hands back and
            // what a diagram is drawn from.
            let source: Arc<str> = Arc::from(code.join("\n"));
            out.extend(match diagram(&fence.1, &source, columns) {
                Some(drawn) => drawn,
                None => code_block(fence.1, &code, columns),
            });
            for line in out.iter_mut().rev() {
                if !matches!(line.block, Block::Code | Block::Diagram) {
                    break;
                }
                line.code = Some(source.clone());
            }
            list_pad = None;
            continue;
        }

        // A table is recognised by its delimiter row, which is the only
        // thing that tells a row of pipes from a sentence with pipes in it.
        if bare.contains('|') && lines.get(i).is_some_and(|next| is_table_rule(next)) {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            let mut table = Table {
                // The delimiter row is where the alignment is written.
                aligns: lines
                    .get(i)
                    .map(|rule| table_cells(rule).iter().map(|c| Align::of(c)).collect())
                    .unwrap_or_default(),
                ..Table::default()
            };
            table.row();
            table.heading_row();
            for cell in table_cells(bare) {
                table.cell(inline(&cell, &refs));
            }
            i += 1; // the delimiter row
            while let Some(next) = lines.get(i) {
                if !next.contains('|') || next.trim().is_empty() {
                    break;
                }
                i += 1;
                table.row();
                for cell in table_cells(next) {
                    table.cell(inline(&cell, &refs));
                }
            }
            out.extend(table.lay_out(columns, 0));
            out.push(Line::default());
            list_pad = None;
            continue;
        }

        if bare.is_empty() {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            list_pad = None;
            // Never two blanks in a row: a heading already leaves one, and
            // the source usually leaves another.
            if out.last().is_some_and(|l| !l.is_blank()) {
                out.push(Line::default());
            }
            continue;
        }

        // A reference definition is machinery, not text.
        if reference_definition(bare).is_some() {
            continue;
        }

        // Setext heading: the underline belongs to the paragraph above it,
        // and is checked before the horizontal rule because `---` under a
        // paragraph is an underline rather than a break. Not over a list
        // item, though: a rule after a bullet is a rule.
        if !paragraph.is_empty()
            && list_pad.is_none()
            && bare.len() >= 2
            && (bare.chars().all(|c| c == '=') || bare.chars().all(|c| c == '-'))
        {
            let level = if bare.starts_with('=') { 1 } else { 2 };
            let spans = std::mem::take(&mut paragraph);
            push_heading(&mut out, spans, level, columns);
            continue;
        }

        // Horizontal rule. Checked before emphasis so `***` is a rule.
        if bare.len() >= 3
            && bare
                .chars()
                .all(|c| c == '-' || c == '*' || c == '_' || c.is_whitespace())
            && bare.chars().filter(|c| !c.is_whitespace()).count() >= 3
        {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            out.push(Line::default());
            out.push(Line::default().decorated(Block::Rule));
            out.push(Line::default());
            list_pad = None;
            continue;
        }

        // ATX heading, with any closing run of hashes dropped.
        if let Some(rest) = bare.strip_prefix('#') {
            let extra = rest.chars().take_while(|c| *c == '#').count();
            let level = (1 + extra).min(6) as u8;
            let text = rest[extra..].trim_start().trim_end_matches(['#', ' ']);
            if !rest[extra..].is_empty() || extra > 0 {
                flush(&mut paragraph, paragraph_indent, columns, &mut out);
                push_heading(&mut out, inline(text, &refs), level, columns);
                list_pad = None;
                continue;
            }
        }

        // Blockquote, however deeply nested.
        if bare.starts_with('>') {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            let mut depth = 0;
            let mut rest = bare;
            while let Some(after) = rest.strip_prefix('>') {
                depth += 1;
                rest = after.trim_start();
            }
            let mut spans = inline(rest, &refs);
            for span in &mut spans {
                if span.style == Style::Body {
                    span.style = Style::Quote;
                }
            }
            out.extend(
                wrap(spans, depth * 3, columns, Scale::Body)
                    .into_iter()
                    .map(|l| l.decorated(Block::Quote)),
            );
            list_pad = None;
            continue;
        }

        // List item, bulleted or numbered, at whatever depth its indent
        // puts it. Two source spaces is one level, which is what every
        // editor's list continuation produces.
        let source_indent = raw.len() - raw.trim_start().len();
        let bullet_text = bare
            .strip_prefix("- ")
            .or_else(|| bare.strip_prefix("* "))
            .or_else(|| bare.strip_prefix("+ "));
        let numbered = bare
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));

        if let Some(item) = bullet_text.or(numbered.map(|(_, rest)| rest)) {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
            let depth = source_indent / 2 + 1;
            let marker = match numbered {
                Some((n, _)) if bullet_text.is_none() => format!("{n}. "),
                _ => format!("{} ", bullet(depth)),
            };
            let pad = 2 + source_indent;
            let mut spans = vec![Span {
                text: marker.clone(),
                style: Style::Bullet,
            }];
            // A task list is a checkbox, not a literal pair of brackets.
            let item = match task_box(item) {
                Some((box_glyph, rest)) => {
                    spans.push(Span {
                        text: format!("{box_glyph} "),
                        style: Style::Bullet,
                    });
                    rest
                }
                None => item,
            };
            spans.extend(inline(item, &refs));
            paragraph = spans;
            paragraph_indent = pad;
            list_pad = Some(pad + marker.chars().count());
            continue;
        }

        // Otherwise it joins the current paragraph, with a space: a line
        // break in the source is a word break, and `wrap` no longer
        // supplies one of its own, since a run boundary inside a word is
        // the commoner case.
        if paragraph.is_empty() {
            paragraph_indent = list_pad.filter(|_| source_indent > 0).unwrap_or(0);
        } else {
            paragraph.push(Span {
                text: " ".into(),
                style: Style::Body,
            });
        }
        paragraph.extend(inline(bare, &refs));

        // A hard break ends the line without ending the paragraph.
        if hard_break {
            flush(&mut paragraph, paragraph_indent, columns, &mut out);
        }
    }

    flush(&mut paragraph, paragraph_indent, columns, &mut out);
    while out.last().is_some_and(Line::is_blank) {
        out.pop();
    }
    out
}

/// Emits a heading, with the air above and the rule below a browser gives
/// it.
fn push_heading(out: &mut Vec<Line>, spans: Vec<Span>, level: u8, columns: usize) {
    if out.last().is_some_and(|l| !l.is_blank()) {
        out.push(Line::default());
    }
    let scale = heading_scale(level);
    let mut spans = spans;
    for span in &mut spans {
        // Code and links inside a heading keep their own colour; the rest
        // becomes heading text.
        if matches!(span.style, Style::Body | Style::Bold | Style::Emphasis) {
            span.style = Style::Heading(level);
        }
    }
    out.extend(wrap(spans, 0, columns, scale));
    // The top two levels get a rule, as they do in a browser.
    if level <= 2 {
        out.push(Line::default().decorated(Block::HeadingRule));
    }
}

/// A fence's marker character and the language written after it.
fn fence_open(bare: &str) -> Option<(char, String)> {
    for marker in ['`', '~'] {
        let run = bare.chars().take_while(|c| *c == marker).count();
        if run >= 3 {
            let info = bare[run..].trim();
            // The language is the first word; `rust,no_run` is one tag.
            let language = info
                .split([' ', ',', ';', '{'])
                .next()
                .unwrap_or("")
                .to_string();
            return Some((marker, language));
        }
    }
    None
}

fn fence_closes(bare: &str, marker: char) -> bool {
    bare.chars().take_while(|c| *c == marker).count() >= 3
        && bare.chars().all(|c| c == marker || c.is_whitespace())
}

/// A `mermaid` fence drawn as a diagram, when it is one this can draw.
///
/// `None` for every other fence, and for a Mermaid diagram the reader
/// will not lay out — the preview then shows the source, which says
/// what was meant even when the picture cannot be drawn.
fn diagram(language: &str, source: &str, columns: usize) -> Option<Vec<Line>> {
    if !language.trim().eq_ignore_ascii_case("mermaid") {
        return None;
    }
    let drawn = crate::mermaid::render(source, columns.saturating_sub(4))?;
    let mut out = vec![Line::default().decorated(Block::Diagram)];
    out.extend(drawn.into_iter().map(|text| Line {
        spans: vec![Span {
            text,
            style: Style::Body,
        }],
        indent: 2,
        block: Block::Diagram,
        scale: Scale::Body,
        code: None,
    }));
    out.push(Line::default().decorated(Block::Diagram));
    Some(out)
}

/// A fenced block: tinted, padded, and highlighted when the fence names a
/// language the editor already has a grammar for.
fn code_block(language: String, code: &[&str], columns: usize) -> Vec<Line> {
    let mut out = vec![Line::default().decorated(Block::Code)];
    let joined = code.join("\n");
    let highlighted = highlight_fence(&language, &joined);
    let room = columns.saturating_sub(4).max(8);

    for (index, text) in code.iter().enumerate() {
        let trimmed = text.trim_end();
        let spans = highlighted
            .as_ref()
            .and_then(|lines| lines.get(index).cloned())
            .unwrap_or_else(|| {
                vec![Span {
                    text: trimmed.to_string(),
                    style: Style::Code,
                }]
            });
        // Hard-wrapped rather than clipped. Code must not be re-flowed at
        // word boundaries — that changes what it says — but silently
        // cutting the end off is worse than a wrap.
        for chunk in hard_wrap(spans, room) {
            out.push(Line {
                spans: chunk,
                indent: 2,
                block: Block::Code,
                scale: Scale::Body,
                code: None,
            });
        }
    }
    out.push(Line::default().decorated(Block::Code));
    out
}

/// Breaks a line of code at exactly `room` cells, never at a word.
fn hard_wrap(spans: Vec<Span>, room: usize) -> Vec<Vec<Span>> {
    if width_of(&spans) <= room {
        return vec![spans];
    }
    let mut out: Vec<Vec<Span>> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0;
    for span in spans {
        let mut chars = span.text.chars().peekable();
        while chars.peek().is_some() {
            let take = room - used;
            let text: String = chars.by_ref().take(take).collect();
            used += text.chars().count();
            current.push(Span {
                text,
                style: span.style,
            });
            if used == room {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// A task list's checkbox, and the text after it.
fn task_box(item: &str) -> Option<(char, &str)> {
    let rest = item.strip_prefix('[')?;
    let mark = rest.chars().next()?;
    let rest = rest.get(1..)?.strip_prefix(']')?;
    let glyph = match mark {
        ' ' => '☐',
        'x' | 'X' | '✓' => '☑',
        _ => return None,
    };
    Some((glyph, rest.trim_start()))
}

/// The labels of every `[label]: url` definition in the document.
///
/// Collected in a pass of its own because a reference may be defined
/// after it is used, and a link that renders as literal brackets on the
/// first read and as a link on the second would be worse than either.
fn reference_labels(lines: &[&str]) -> HashMap<String, ()> {
    lines
        .iter()
        .filter_map(|line| reference_definition(line.trim()))
        .map(|label| (label, ()))
        .collect()
}

/// The label of a `[label]: url` definition line.
fn reference_definition(bare: &str) -> Option<String> {
    let rest = bare.strip_prefix('[')?;
    let close = rest.find("]:")?;
    let label = &rest[..close];
    let target = rest[close + 2..].trim();
    (!label.is_empty() && !target.is_empty() && !target.contains(char::is_whitespace))
        .then(|| label.to_ascii_lowercase())
}

fn flush(paragraph: &mut Vec<Span>, indent: usize, columns: usize, out: &mut Vec<Line>) {
    if paragraph.is_empty() {
        return;
    }
    out.extend(wrap(
        std::mem::take(paragraph),
        indent,
        columns,
        Scale::Body,
    ));
}

/// Breaks spans into lines at word boundaries, keeping each span's style.
///
/// `scale` is passed because a heading's glyphs are wider than a body
/// cell, so the same number of characters needs more of the column.
fn wrap(spans: Vec<Span>, indent: usize, columns: usize, scale: Scale) -> Vec<Line> {
    let room = ((columns.saturating_sub(indent) as f32 / scale.width()) as usize).max(8);

    // Tokenise into words that may be built from several runs. A run
    // boundary is not a word boundary: `**bold**s` is one word in two
    // styles and `[link](x).` is one word whose last character is not part
    // of the link. Splitting each run on its own spaces and rejoining them
    // with a single space put a gap inside both.
    let mut words: Vec<Vec<Span>> = Vec::new();
    // Whether the last word can still be added to.
    let mut open = false;
    for span in spans {
        let mut first = true;
        for piece in span.text.split(' ') {
            // Only the first piece of a run can continue the open word:
            // every later piece had a space split off before it.
            let continues = open && first;
            first = false;
            if piece.is_empty() {
                // A space, wherever it falls, ends the word.
                open = false;
                continue;
            }
            if !continues {
                words.push(Vec::new());
            }
            let word = words
                .last_mut()
                .expect("a word is open or was just started");
            match word.last_mut() {
                Some(last) if last.style == span.style => last.text.push_str(piece),
                _ => word.push(Span {
                    text: piece.to_string(),
                    style: span.style,
                }),
            }
            open = true;
        }
    }

    let mut lines = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for word in words {
        let len: usize = word.iter().map(|s| s.text.chars().count()).sum();
        if used > 0 && used + len + 1 > room {
            lines.push(Line {
                spans: std::mem::take(&mut current),
                indent,
                block: Block::Plain,
                scale,
                code: None,
            });
            used = 0;
        }
        if used > 0 {
            // The separator joins the two runs it sits between when they
            // agree, so a link's underline and a strikethrough run through
            // the spaces inside the phrase and stop at its edges.
            let before = current.last().map(|s: &Span| s.style);
            let after = word.first().map(|s| s.style);
            let style = match (before, after) {
                (Some(a), Some(b)) if a == b => a,
                _ => Style::Body,
            };
            push_run(&mut current, " ", style);
            used += 1;
        }
        for piece in word {
            used += piece.text.chars().count();
            push_run(&mut current, &piece.text, piece.style);
        }
    }
    if !current.is_empty() {
        lines.push(Line {
            spans: current,
            indent,
            block: Block::Plain,
            scale,
            code: None,
        });
    }
    lines
}

/// Appends text to a line, joining it to the previous run when the style
/// matches so a wrapped line is a handful of runs rather than one per word.
fn push_run(line: &mut Vec<Span>, text: &str, style: Style) {
    match line.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => line.push(Span {
            text: text.to_string(),
            style,
        }),
    }
}

/// The inline HTML a README actually contains, and what it means here.
///
/// Markdown passes HTML through, so a reader that showed the tags as text
/// would be showing something the document never said. Unknown tags are
/// left alone instead of dropped: `Vec<T>` in prose is not markup, and in
/// a code editor's README that case is more common than the other.
fn inline_tag(name: &str) -> Option<Option<Style>> {
    Some(match name {
        "strong" | "b" => Some(Style::Bold),
        "em" | "i" | "cite" | "var" => Some(Style::Emphasis),
        "code" | "kbd" | "samp" | "tt" => Some(Style::Code),
        "a" => Some(Style::Link),
        "s" | "del" | "strike" => Some(Style::Strike),
        // Known, but carries no style of its own.
        "br" | "p" | "div" | "span" | "sub" | "sup" | "small" | "u" | "ins" | "mark"
        | "picture" | "source" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "details"
        | "summary" | "ul" | "ol" | "li" | "table" | "tr" | "td" | "th" | "thead" | "tbody"
        | "blockquote" | "pre" | "center" | "font" | "nobr" => None,
        _ => return None,
    })
}

/// Combines a run's own style with the one it is nested inside.
///
/// Code wins over everything, because inside a code span nothing else is
/// markup; bold and emphasis combine; otherwise the inner one wins.
fn merge(outer: Style, inner: Style) -> Style {
    match (outer, inner) {
        (Style::Code, _) | (_, Style::Code) => Style::Code,
        (Style::Bold, Style::Emphasis) | (Style::Emphasis, Style::Bold) => Style::BoldEmphasis,
        (Style::BoldEmphasis, Style::Bold | Style::Emphasis) => Style::BoldEmphasis,
        (Style::Body, inner) => inner,
        (outer, Style::Body) => outer,
        (_, inner) => inner,
    }
}

/// Splits one line into styled runs.
///
/// Handles `**bold**`, `*em*`, `***both***`, `` `code` ``, `~~struck~~`,
/// `[text](url)`, `[text][ref]`, `![alt](src)`, `<https://autolink>`,
/// bare URLs, backslash escapes, and the inline HTML above. Anything
/// unbalanced is left as written rather than eaten.
fn inline(text: &str, refs: &HashMap<String, ()>) -> Vec<Span> {
    let mut out = Vec::new();
    scan(text, Style::Body, refs, &mut out);
    if out.is_empty() {
        out.push(Span {
            text: String::new(),
            style: Style::Body,
        });
    }
    out
}

fn scan(text: &str, base: Style, refs: &HashMap<String, ()>, out: &mut Vec<Span>) {
    let chars: Vec<char> = text.chars().collect();
    let mut plain = String::new();
    // Styles opened by an inline HTML tag, innermost last.
    let mut tags: Vec<Style> = Vec::new();
    let mut i = 0;

    macro_rules! flush_plain {
        () => {
            if !plain.is_empty() {
                let style = tags.iter().fold(base, |acc, s| merge(acc, *s));
                push(out, std::mem::take(&mut plain), style);
            }
        };
    }

    while i < chars.len() {
        let here = &text[char_byte(&chars, i)..];

        // An escape is the one thing that outranks everything else.
        if chars[i] == '\\' {
            if let Some(next) = chars.get(i + 1).filter(|c| c.is_ascii_punctuation()) {
                plain.push(*next);
                i += 2;
                continue;
            }
        }

        // Code first: inside a span, no other marker is markup.
        if chars[i] == '`' {
            let ticks = chars[i..].iter().take_while(|c| **c == '`').count();
            let marker: String = "`".repeat(ticks);
            if let Some(end) = find_from(&chars, i + ticks, &marker) {
                flush_plain!();
                let body: String = chars[i + ticks..end].iter().collect();
                // A code span's one piece of trim: `` ` `` holds a
                // backtick, and the padding spaces are not content.
                let body = body.trim_matches(' ');
                push(out, body.to_string(), Style::Code);
                i = end + ticks;
                continue;
            }
        }

        // Image before link: the `!` belongs to the construct.
        if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            if let Some((label, consumed)) = bracketed(&chars[i + 1..], refs) {
                flush_plain!();
                let alt = if label.is_empty() { "image" } else { &label };
                out.push(picture(alt));
                i += consumed + 1;
                continue;
            }
        }
        if chars[i] == '[' {
            if let Some((label, consumed)) = bracketed(&chars[i..], refs) {
                flush_plain!();
                let mut inner = Vec::new();
                scan(&label, merge(base, Style::Link), refs, &mut inner);
                // The label's own markup still applies, but a plain label
                // is a link, not body text.
                for mut span in inner {
                    if span.style == Style::Body || span.style == merge(base, Style::Link) {
                        span.style = Style::Link;
                    }
                    out.push(span);
                }
                i += consumed;
                continue;
            }
        }

        // Emphasis markers, longest first so `***` is not read as `**`
        // and then left with a stray marker.
        let mut matched = false;
        for (marker, style) in [
            ("***", Style::BoldEmphasis),
            ("___", Style::BoldEmphasis),
            ("**", Style::Bold),
            ("__", Style::Bold),
            ("~~", Style::Strike),
        ] {
            if !here.starts_with(marker) {
                continue;
            }
            let len = marker.chars().count();
            // No partner on this line: it is punctuation, not a marker,
            // and falls through to be emitted as written.
            let Some(end) = find_from(&chars, i + len, marker) else {
                continue;
            };
            let body: String = chars[i + len..end].iter().collect();
            if body.trim().is_empty() {
                continue;
            }
            flush_plain!();
            scan(&body, merge(base, style), refs, out);
            i = end + len;
            matched = true;
            break;
        }
        if matched {
            continue;
        }

        // Single-character emphasis. `_` only between non-word characters,
        // so `snake_case_name` is a name rather than emphasis.
        if chars[i] == '*' || (chars[i] == '_' && word_boundary(&chars, i)) {
            let marker = chars[i].to_string();
            if let Some(end) = find_from(&chars, i + 1, &marker) {
                let body: String = chars[i + 1..end].iter().collect();
                if !body.trim().is_empty() && !body.contains('\n') {
                    flush_plain!();
                    scan(&body, merge(base, Style::Emphasis), refs, out);
                    i = end + 1;
                    continue;
                }
            }
        }

        // An autolink, or a tag.
        if chars[i] == '<' {
            if let Some(end) = find_from(&chars, i + 1, ">") {
                let raw: String = chars[i + 1..end].iter().collect();
                if raw.starts_with("http://") || raw.starts_with("https://") {
                    flush_plain!();
                    push(out, raw, Style::Link);
                    i = end + 1;
                    continue;
                }
                let closing = raw.starts_with('/');
                let name = raw
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                // An `<img>` written inline in Markdown is still an
                // image, and the one void tag worth naming.
                if name == "img" && !closing {
                    flush_plain!();
                    let alt = attribute(&raw, "alt")
                        .filter(|alt| !alt.trim().is_empty())
                        .or_else(|| attribute(&raw, "src"))
                        .unwrap_or_else(|| "image".to_string());
                    out.push(picture(&alt));
                    i = end + 1;
                    continue;
                }
                if let Some(style) = inline_tag(&name) {
                    flush_plain!();
                    match (closing, style) {
                        (false, Some(style)) => tags.push(style),
                        (true, Some(_)) => {
                            tags.pop();
                        }
                        _ => {}
                    }
                    i = end + 1;
                    continue;
                }
            }
        }

        // A bare URL, which every renderer links and this one should too.
        if here.starts_with("http://") || here.starts_with("https://") {
            let len = chars[i..]
                .iter()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '<' | '>' | ')' | '"'))
                .count();
            let url: String = chars[i..i + len].iter().collect();
            // Trailing punctuation belongs to the sentence, not the link.
            let url = url.trim_end_matches(['.', ',', ';', ':', '!']);
            flush_plain!();
            push(out, url.to_string(), Style::Link);
            i += url.chars().count();
            continue;
        }

        plain.push(chars[i]);
        i += 1;
    }
    flush_plain!();
}

/// Appends a run, joining it to the previous one when the style matches.
fn push(out: &mut Vec<Span>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.style == style => last.text.push_str(&text),
        _ => out.push(Span { text, style }),
    }
}

fn char_byte(chars: &[char], index: usize) -> usize {
    chars[..index].iter().map(|c| c.len_utf8()).sum()
}

/// The index of the next occurrence of `needle` at or after `from`.
fn find_from(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if from >= chars.len() || needle.is_empty() {
        return None;
    }
    (from..=chars.len().saturating_sub(needle.len())).find(|&at| {
        chars[at..at + needle.len()] == needle[..]
            // A marker escaped with a backslash is not a marker.
            && (at == 0 || chars[at - 1] != '\\')
    })
}

/// Whether `_` at `index` sits at a word boundary, which is the only place
/// it means emphasis rather than part of an identifier.
fn word_boundary(chars: &[char], index: usize) -> bool {
    let before = index
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .copied()
        .unwrap_or(' ');
    !before.is_alphanumeric()
}

/// A `[label](target)`, `[label][ref]` or `[label]` shortcut.
///
/// Returns the label and how many characters the whole construct took, or
/// `None` when it is not a link — a bare `[TODO]` in prose stays as it was
/// written.
fn bracketed(chars: &[char], refs: &HashMap<String, ()>) -> Option<(String, usize)> {
    if chars.first() != Some(&'[') {
        return None;
    }
    // Nesting, so `[![badge](img)](link)` finds the outer bracket.
    let mut depth = 0usize;
    let mut close = None;
    for (i, ch) in chars.iter().enumerate() {
        match ch {
            '[' if i == 0 || chars[i - 1] != '\\' => depth += 1,
            ']' if chars[i - 1] != '\\' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let label: String = chars[1..close].iter().collect();

    match chars.get(close + 1) {
        Some('(') => {
            let end = find_from(chars, close + 2, ")")?;
            Some((label, end + 1))
        }
        Some('[') => {
            let end = find_from(chars, close + 2, "]")?;
            let reference: String = chars[close + 2..end].iter().collect();
            let key = if reference.is_empty() {
                label.to_ascii_lowercase()
            } else {
                reference.to_ascii_lowercase()
            };
            refs.contains_key(&key).then_some((label, end + 1))
        }
        // A shortcut reference links only when something defines it.
        _ => refs
            .contains_key(&label.to_ascii_lowercase())
            .then_some((label, close + 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(line: &Line) -> Vec<Style> {
        line.spans.iter().map(|s| s.style).collect()
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// Headings are padded and underlined now, so tests find them rather
    /// than counting rows.
    fn heading_lines(out: &[Line]) -> Vec<(u8, String)> {
        out.iter()
            .filter_map(|l| match l.spans.first().map(|s| s.style) {
                Some(Style::Heading(level)) => Some((level, text_of(l))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn headings_carry_their_level() {
        let out = render("# One\n### Three\n", 60);
        assert_eq!(
            heading_lines(&out),
            vec![(1, "One".to_string()), (3, "Three".to_string())]
        );
    }

    #[test]
    fn inline_markers_become_runs() {
        let out = render("a **bold** and `code` here\n", 80);
        let line = &out[0];
        assert!(
            styles(line).contains(&Style::Bold) && styles(line).contains(&Style::Code),
            "got {:?}",
            styles(line)
        );
        assert!(
            !text_of(line).contains('*') && !text_of(line).contains('`'),
            "markers should not survive into the output: {:?}",
            text_of(line)
        );
    }

    #[test]
    fn an_unmatched_marker_is_left_alone() {
        let out = render("2 * 3 is not emphasis\n", 80);
        assert_eq!(text_of(&out[0]), "2 * 3 is not emphasis");
        assert_eq!(styles(&out[0]), vec![Style::Body]);
    }

    #[test]
    fn a_fence_suspends_all_markup() {
        let out = render("```\n# not a heading\n**not bold**\n```\n", 80);
        let inside: Vec<String> = out
            .iter()
            // The padding rows are tinted too, but empty.
            .filter(|l| l.block == Block::Code)
            .map(text_of)
            .filter(|t| !t.is_empty())
            .collect();
        assert_eq!(inside, vec!["# not a heading", "**not bold**"]);
        assert!(
            heading_lines(&out).is_empty(),
            "nothing inside a fence is a heading"
        );
    }

    #[test]
    fn links_show_their_label_and_hide_the_target() {
        let out = render("see [the docs](https://example.com/x) now\n", 80);
        let line = &out[0];
        assert!(text_of(line).contains("the docs"));
        assert!(
            !text_of(line).contains("example.com"),
            "the url should not be in the body: {:?}",
            text_of(line)
        );
        assert!(styles(line).contains(&Style::Link));
    }

    #[test]
    fn lists_get_a_marker_and_an_indent() {
        let out = render("- first\n- second\n", 80);
        assert_eq!(out[0].indent, 2);
        assert!(text_of(&out[0]).starts_with('•'));

        let numbered = render("1. one\n2. two\n", 80);
        assert!(text_of(&numbered[0]).starts_with("1."));
    }

    #[test]
    fn long_paragraphs_wrap_at_word_boundaries() {
        let source = "word ".repeat(40);
        let out = render(&source, 30);
        assert!(out.len() > 1, "should have wrapped");
        for line in &out {
            let width = line.indent + text_of(line).chars().count();
            assert!(width <= 30, "line too wide: {:?}", text_of(line));
            assert!(
                !text_of(line).starts_with(' '),
                "a wrapped line should not start with a space"
            );
        }
    }

    #[test]
    fn blockquotes_and_rules_are_recognised() {
        let out = render("> quoted\n\n---\n", 60);
        // A quote is styled text with a bar beside it; a rule is drawn
        // rather than typed, so it is a block with no text at all.
        let quote = out
            .iter()
            .find(|l| l.block == Block::Quote)
            .expect("a quote");
        assert_eq!(styles(quote), vec![Style::Quote]);
        assert!(out.iter().any(|l| l.block == Block::Rule));
    }

    #[test]
    fn headings_get_space_and_a_rule_beneath_them() {
        let out = render("intro\n\n# Title\n\nbody\n", 60);
        let heading = out
            .iter()
            .position(|l| matches!(l.spans.first().map(|s| s.style), Some(Style::Heading(1))))
            .expect("a heading");
        assert!(out[heading - 1].is_blank(), "a heading needs air above it");
        assert_eq!(
            out[heading + 1].block,
            Block::HeadingRule,
            "an h1 is underlined"
        );
    }

    #[test]
    fn a_fenced_block_is_tinted_including_its_padding() {
        let out = render("```\ncode here\n```\n", 60);
        let tinted: Vec<_> = out.iter().filter(|l| l.block == Block::Code).collect();
        assert!(
            tinted.len() >= 3,
            "the fence rows pad the block, got {}",
            tinted.len()
        );
        assert!(tinted.iter().any(|l| text_of(l).contains("code here")));
    }

    #[test]
    fn a_run_boundary_is_not_a_word_boundary() {
        // The old wrapper split every run on its own spaces and rejoined
        // them with one, so a link met its full stop across a gap.
        let out = render("see [the docs](http://x), then **stop**.\n", 80);
        assert_eq!(text_of(&out[0]), "see the docs, then stop.");
    }

    #[test]
    fn a_source_line_break_is_still_a_word_break() {
        let out = render("one two\nthree four\n", 80);
        assert_eq!(text_of(&out[0]), "one two three four");
    }

    #[test]
    fn a_top_level_heading_is_drawn_larger() {
        let out = render("# One\n\n## Two\n\n### Three\n", 60);
        let scales: Vec<(u8, Scale)> = out
            .iter()
            .filter_map(|l| match l.spans.first().map(|s| s.style) {
                Some(Style::Heading(level)) => Some((level, l.scale)),
                _ => None,
            })
            .collect();
        assert_eq!(
            scales,
            vec![(1, Scale::Huge), (2, Scale::Large), (3, Scale::Body)]
        );
        // And the taller ones take a second grid row, so the preview can
        // keep counting lines.
        assert_eq!(Scale::Huge.rows(), 2);
        assert_eq!(Scale::Body.rows(), 1);
    }

    #[test]
    fn a_pipe_table_is_drawn_inside_a_frame() {
        let out = render(
            "| Name | Size |\n|---|---:|\n| a.rs | 12 |\n| bb.rs | 3 |\n",
            60,
        );
        let rows: Vec<String> = out.iter().map(text_of).filter(|t| !t.is_empty()).collect();

        assert!(
            rows[0].starts_with('┌') && rows[0].ends_with('┐'),
            "{rows:#?}"
        );
        assert!(rows[1].starts_with("│ Name"), "{rows:#?}");
        assert!(
            rows[2].starts_with('├') && rows[2].ends_with('┤'),
            "{rows:#?}"
        );
        assert!(rows.last().unwrap().starts_with('└'), "{rows:#?}");

        // Every row the same width is what makes the columns line up.
        let widths: Vec<usize> = rows.iter().map(|r| r.chars().count()).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "rows should be padded alike, got {widths:?}"
        );
    }

    #[test]
    fn a_column_is_aligned_the_way_its_delimiter_says() {
        // The markers were being parsed and then thrown away, so every
        // column came out left-aligned however it was written.
        let out = render("| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n", 60);
        let body = out
            .iter()
            .map(text_of)
            .find(|t| t.contains('a'))
            .expect("the body row");
        let cells: Vec<&str> = body
            .trim_matches(|c| c == '│' || c == ' ')
            .split('│')
            .collect();
        assert_eq!(cells.len(), 3, "{body:?}");
        assert!(cells[0].starts_with('a'), "left: {:?}", cells[0]);
        assert!(
            cells[1].starts_with(' ') && cells[1].ends_with(' '),
            "centre: {:?}",
            cells[1]
        );
        assert!(cells[2].ends_with('c'), "right: {:?}", cells[2]);
    }

    #[test]
    fn a_table_with_only_a_header_needs_no_separator() {
        let out = render("| One | Two |\n|---|---|\n", 60);
        let rows: Vec<String> = out.iter().map(text_of).filter(|t| !t.is_empty()).collect();
        assert_eq!(rows.len(), 3, "top, header, bottom: {rows:#?}");
        assert!(rows[2].starts_with('└'), "{rows:#?}");
    }

    #[test]
    fn a_sentence_with_pipes_in_it_is_not_a_table() {
        let out = render("use a | b to pipe\n", 60);
        assert!(out.iter().all(|l| l.block != Block::TableHead));
        assert_eq!(text_of(&out[0]), "use a | b to pipe");
    }

    #[test]
    fn a_wide_table_is_squeezed_rather_than_overflowing() {
        let wide = "x".repeat(200);
        let out = render(&format!("| a | b |\n|---|---|\n| {wide} | y |\n"), 40);
        for line in &out {
            assert!(
                line.indent + text_of(line).chars().count() <= 40,
                "too wide: {:?}",
                text_of(line)
            );
        }
    }

    #[test]
    fn a_fence_is_highlighted_when_it_names_a_language() {
        let out = render("```rust\nfn main() {}\n```\n", 60);
        let kinds: Vec<Style> = out
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.style))
            .collect();
        assert!(
            kinds
                .iter()
                .any(|s| matches!(s, Style::Syntax(lithe_syntax::Kind::Keyword))),
            "`fn` should be a keyword, got {kinds:?}"
        );
    }

    #[test]
    fn every_row_of_a_fence_carries_its_source() {
        // A drawn preview has no selection to drag, so a click has to be
        // able to copy the block from whichever of its rows was hit.
        let out = render(
            "before\n\n```rust\nfn a() {}\nfn b() {}\n```\n\nafter\n",
            60,
        );
        let fenced: Vec<&Line> = out.iter().filter(|l| l.code.is_some()).collect();
        assert!(fenced.len() >= 2, "got {}", fenced.len());
        for line in &fenced {
            assert_eq!(
                line.code.as_deref(),
                Some("fn a() {}\nfn b() {}"),
                "the source as written, not as rendered"
            );
        }
        assert!(
            out.iter()
                .filter(|l| l.code.is_none())
                .any(|l| text_of(l).contains("before")),
            "prose carries none of it"
        );
    }

    #[test]
    fn a_mermaid_fence_is_drawn_and_still_copies_its_source() {
        let src = "```mermaid\nflowchart TB\n  A[One] --> B[Two]\n```\n";
        let out = render(src, 60);
        let drawn: String = out.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(drawn.contains("One") && drawn.contains('▼'), "{drawn}");
        assert!(
            out.iter().any(|l| l.block == Block::Diagram),
            "it should be a diagram, not a code block"
        );
        // Copying gives back the mermaid, not the picture.
        let source = out.iter().find_map(|l| l.code.clone()).expect("a source");
        assert!(source.contains("flowchart TB"), "{source}");
    }

    #[test]
    fn a_mermaid_fence_it_cannot_draw_stays_a_code_block() {
        let src = "```mermaid\npie title Pets\n  \"Dogs\" : 1\n```\n";
        let out = render(src, 60);
        assert!(
            out.iter().all(|l| l.block != Block::Diagram),
            "a pie chart is neither diagram this draws"
        );
        let text: String = out.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("pie title"),
            "the source says what was meant: {text}"
        );
    }

    #[test]
    fn an_unlabelled_fence_stays_plain() {
        let out = render("```\nfn main() {}\n```\n", 60);
        assert!(
            out.iter()
                .flat_map(|l| l.spans.iter())
                .all(|s| !matches!(s.style, Style::Syntax(_))),
            "nothing names a grammar here, so nothing should be coloured"
        );
    }

    #[test]
    fn task_lists_become_checkboxes() {
        let out = render("- [ ] todo\n- [x] done\n", 60);
        assert!(text_of(&out[0]).contains('☐'), "{:?}", text_of(&out[0]));
        assert!(text_of(&out[1]).contains('☑'), "{:?}", text_of(&out[1]));
        assert!(
            !text_of(&out[0]).contains('['),
            "the brackets are the checkbox, not text"
        );
    }

    #[test]
    fn nested_lists_change_bullet_with_depth() {
        let out = render("- one\n  - two\n    - three\n", 60);
        let markers: Vec<char> = out
            .iter()
            .filter_map(|l| text_of(l).trim_start().chars().next())
            .collect();
        assert_eq!(markers, vec!['•', '◦', '▪']);
        assert!(out[0].indent < out[1].indent && out[1].indent < out[2].indent);
    }

    #[test]
    fn an_ordered_list_keeps_its_numbers() {
        let out = render("3. three\n4. four\n", 60);
        assert!(text_of(&out[0]).starts_with("3."));
        assert!(text_of(&out[1]).starts_with("4."));
    }

    #[test]
    fn strikethrough_and_both_markers_are_their_own_styles() {
        let out = render("~~gone~~ and ***loud***\n", 80);
        let styles = styles(&out[0]);
        assert!(styles.contains(&Style::Strike), "got {styles:?}");
        assert!(styles.contains(&Style::BoldEmphasis), "got {styles:?}");
        assert!(!text_of(&out[0]).contains('~'));
        assert!(!text_of(&out[0]).contains('*'));
    }

    #[test]
    fn front_matter_is_metadata_not_text() {
        let out = render("---\ntitle: Hello\ntags: [a, b]\n---\n\n# Body\n", 60);
        let joined: String = out.iter().map(text_of).collect();
        assert!(!joined.contains("title"), "got {joined:?}");
        assert!(joined.contains("Body"));
    }

    #[test]
    fn a_setext_underline_makes_the_line_above_a_heading() {
        let out = render("Title\n=====\n\nSub\n---\n", 60);
        assert_eq!(
            heading_lines(&out),
            vec![(1, "Title".to_string()), (2, "Sub".to_string())]
        );
    }

    #[test]
    fn an_escape_shows_the_character_rather_than_the_markup() {
        let out = render("\\*not emphasis\\* and \\`not code\\`\n", 80);
        assert_eq!(text_of(&out[0]), "*not emphasis* and `not code`");
        assert_eq!(styles(&out[0]), vec![Style::Body]);
    }

    #[test]
    fn a_closing_run_of_hashes_is_not_part_of_the_title() {
        let out = render("## Middle ##\n", 60);
        assert_eq!(heading_lines(&out), vec![(2, "Middle".to_string())]);
    }

    #[test]
    fn a_hard_break_ends_the_line_without_ending_the_paragraph() {
        let out = render("one  \ntwo\n", 80);
        assert_eq!(text_of(&out[0]), "one");
        assert_eq!(text_of(&out[1]), "two");
    }

    #[test]
    fn urls_link_themselves_and_definitions_do_not_show() {
        let out = render(
            "see https://example.com/a and [docs][d] and <https://b.co>\n\n[d]: https://d.co\n",
            90,
        );
        let joined: String = out.iter().map(text_of).collect();
        assert!(
            !joined.contains("[d]:"),
            "a definition is machinery: {joined:?}"
        );
        assert!(joined.contains("docs"));
        let links: Vec<&Span> = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style == Style::Link)
            .collect();
        assert_eq!(links.len(), 3, "two urls and one reference, got {links:?}");
    }

    #[test]
    fn an_undefined_reference_stays_as_written() {
        let out = render("see [nothing][missing] here\n", 80);
        assert_eq!(text_of(&out[0]), "see [nothing][missing] here");
        assert!(!styles(&out[0]).contains(&Style::Link));
    }

    #[test]
    fn snake_case_is_not_emphasis() {
        let out = render("call some_long_name now\n", 80);
        assert_eq!(text_of(&out[0]), "call some_long_name now");
        assert_eq!(styles(&out[0]), vec![Style::Body]);
    }

    #[test]
    fn images_are_named_rather_than_drawn() {
        let out = render("![The logo](logo.png) sits here\n", 80);
        assert!(text_of(&out[0]).contains("The logo"));
        assert!(!text_of(&out[0]).contains("logo.png"));
        assert!(styles(&out[0]).contains(&Style::Link));
    }

    #[test]
    fn inline_html_is_markup_not_text() {
        let out = render("a <b>bold</b> and <kbd>cmd</kbd> word\n", 80);
        assert!(!text_of(&out[0]).contains('<'), "{:?}", text_of(&out[0]));
        assert!(styles(&out[0]).contains(&Style::Bold));
        assert!(styles(&out[0]).contains(&Style::Code));
    }

    #[test]
    fn a_generic_in_prose_is_left_alone() {
        // Not a tag anyone means: in a Rust README this is far commoner
        // than hand-written HTML, so an unknown tag stays as written.
        let out = render("holds a Vec<Thing> inside\n", 80);
        assert_eq!(text_of(&out[0]), "holds a Vec<Thing> inside");
    }

    #[test]
    fn code_wins_over_the_markers_inside_it() {
        let out = render("`**not bold**` here\n", 80);
        let code = out[0]
            .spans
            .iter()
            .find(|s| s.style == Style::Code)
            .expect("a code span");
        assert_eq!(code.text, "**not bold**");
    }

    #[test]
    fn consecutive_lines_join_into_one_paragraph() {
        let out = render("one\ntwo\n\nthree\n", 80);
        assert_eq!(text_of(&out[0]), "one two", "got {:?}", text_of(&out[0]));
    }
}
