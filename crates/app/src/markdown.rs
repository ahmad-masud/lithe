//! The Markdown preview: a document, laid out on the editor's grid.
//!
//! Parsing is pulldown-cmark's, because CommonMark is a standard with a
//! long tail — reference links defined after their use, lazy
//! continuation, what counts as emphasis inside a word — and none of that
//! tail is this editor's product. What is ours is everything after the
//! events: which runs are bold, how a table fits in a column of
//! characters, where a heading earns a second row.
//!
//! Output is a flat list of laid-out lines, so the preview scrolls and
//! draws much as the editor does — a line is one scroll step, and a
//! heading is a line that happens to be two grid rows tall.

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
}

impl Style {
    /// Which face this run is set in.
    ///
    /// Prose for anything that is read as language, the grid's own face
    /// for anything whose meaning is in its alignment: code, a table's
    /// frame, a drawn diagram. The one decision that makes the preview a
    /// document rather than a terminal, and the reason emphasis can be
    /// slanted rather than merely coloured.
    pub fn family(self) -> lithe_gpu::Family {
        match self {
            // A bullet, a checkbox and a table's rule are furniture
            // rather than language: they line up with each other, and
            // with the indents measured in cells around them.
            Self::Code | Self::Syntax(_) | Self::Bullet => lithe_gpu::Family::Mono,
            _ => lithe_gpu::Family::Prose,
        }
    }

    /// The glyph size a run is drawn at, given the line's scale.
    ///
    /// Inline code in body text is set a shade smaller. A monospace
    /// face at the same nominal size reads as larger than the prose
    /// around it, so a word of code stood proud of its own line; a
    /// heading keeps its scale, because there the code is the heading.
    pub fn size(self, scale: Scale) -> lithe_gpu::Size {
        match (self, scale) {
            (Self::Code, Scale::Body) => lithe_gpu::Size::Small,
            (_, Scale::Body) => lithe_gpu::Size::Body,
            (_, Scale::Large) => lithe_gpu::Size::Large,
            (_, Scale::Huge) => lithe_gpu::Size::Huge,
        }
    }

    pub fn face(self) -> lithe_gpu::Face {
        match self {
            Self::Bold | Self::Heading(_) => lithe_gpu::Face::Bold,
            Self::Emphasis => lithe_gpu::Face::Italic,
            Self::BoldEmphasis => lithe_gpu::Face::BoldItalic,
            _ => lithe_gpu::Face::Regular,
        }
    }
}

/// How wide a run of text is, in body cells.
///
/// The layout is in cells because the editor is, but prose is
/// proportional — `iii` is not `MMM` — so it asks rather than counts, and
/// what answers is backed by the same glyph advances the renderer draws
/// with. Without that agreement a line wraps in one place and is drawn in
/// another.
pub trait Measure {
    /// `scale` is the line's, because a heading is drawn larger than the
    /// run inside it says on its own.
    fn width(&mut self, text: &str, style: Style, scale: Scale) -> f32;

    /// How tall a grid row is relative to a cell's width.
    ///
    /// A figure is laid out in cells and drawn as shapes, so it needs
    /// to know the shape of a cell or a circle comes out oval. Two is
    /// about right for a monospace face and is what the grid assumes
    /// when there is no window to ask.
    fn aspect(&mut self) -> f32 {
        2.0
    }
}

/// A measure for a grid: one cell per character, whatever the style.
///
/// What a document is laid out in before there is a window to draw it in,
/// and what the tests below use, so they can go on counting columns.
pub struct Cells;

impl Measure for Cells {
    fn width(&mut self, text: &str, _style: Style, scale: Scale) -> f32 {
        text.chars().count() as f32 * scale.width()
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Where this run goes when it is clicked, for the runs that go
    /// anywhere: a link's text, a bare url, an image's name.
    ///
    /// Per run rather than per line, because a paragraph holds several
    /// and a wrap puts them on different lines; an `Arc` because that
    /// wrap copies the target onto every run it splits the phrase into.
    pub href: Option<Arc<str>>,
}

impl Span {
    /// A run that goes nowhere, which is most of them.
    pub fn plain(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            href: None,
        }
    }
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
    /// The rest of a table — its body rows and its frame.
    Table,
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
    pub fn width(self) -> f32 {
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
    /// A table row this line is, with its cells placed in columns.
    pub row: Option<Row>,
    /// A drawn figure this line stands for.
    ///
    /// A figure is one line of the preview however many rows it takes:
    /// it has no text to wrap, so breaking it into rows would only
    /// mean reassembling it to draw it. [`Line::rows`] reports its
    /// height, which is what scrolling and hit-testing count.
    pub figure: Option<Arc<crate::mermaid::Diagram>>,
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
        match &self.figure {
            Some(figure) => figure.rows,
            None => self.scale.rows(),
        }
    }

    /// Everything on this line, as one string.
    ///
    /// The spans are a line broken up by style, so a selection that
    /// covers part of one has to index into the whole rather than into
    /// whichever run it happens to start in.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for _ in 0..self.indent {
            out.push(' ');
        }
        for span in &self.spans {
            out.push_str(&span.text);
        }
        out
    }

    /// How many characters [`Line::text`] has.
    pub fn len_chars(&self) -> usize {
        if self.figure.is_some() {
            return 0;
        }
        if self.row.is_some() {
            return self.text().chars().count();
        }
        self.indent
            + self
                .spans
                .iter()
                .map(|s| s.text.chars().count())
                .sum::<usize>()
    }

    /// Whether this line is set on the grid rather than as prose.
    ///
    /// Code and a drawn diagram mean what they mean because of where
    /// their characters sit. A table used to as well; its cells are
    /// now placed in measured columns, so its prose can be prose.
    pub fn is_grid(&self) -> bool {
        matches!(self.block, Block::Code | Block::Diagram)
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
            _ => out.push(Span::plain(ch.to_string(), style)),
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
fn picture(alt: &str, href: Option<Arc<str>>) -> Span {
    Span {
        text: format!("▣ {alt}"),
        style: Style::Link,
        href,
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
    /// What the parser read out of the `|:---|:--:|---:|` row.
    fn of(alignment: &pulldown_cmark::Alignment) -> Self {
        match alignment {
            pulldown_cmark::Alignment::Center => Self::Centre,
            pulldown_cmark::Alignment::Right => Self::Right,
            _ => Self::Left,
        }
    }
}

/// A table being collected, and then laid out into measured columns.
#[derive(Default)]
/// One cell of a table.
#[derive(Clone, Debug)]
pub struct Cell {
    pub spans: Vec<Span>,
    pub align: Align,
    /// Where the column starts and how wide it is, in cells.
    pub x: f32,
    pub width: f32,
}

/// One row of a table, as cells rather than as one padded string.
///
/// A table is the one construct whose columns have to line up down the
/// page, so it is the one that cannot be laid out a line at a time:
/// the widths come from every row, and the painter needs them to put a
/// cell where its column is. Padding a row into a single monospace
/// string did line the columns up, and set the whole table — prose
/// cells included — in the grid's face, which is what made it read as
/// a terminal rather than as a table.
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub heading: bool,
    /// Whether a rule is drawn under this row: under the heading, and
    /// between body rows, but not under the last.
    pub rule: bool,
    /// How far the table runs, in cells, for the rules.
    pub width: f32,
}

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

    /// One line per row, with its cells placed in columns.
    ///
    /// Widths come from the content, measured the way it will be
    /// drawn, and then shrink to fit. A table is the one construct
    /// here whose shape depends on every line of it, which is why it
    /// is collected whole before anything is emitted.
    fn lay_out(self, columns: usize, indent: usize, measure: &mut dyn Measure) -> Vec<Line> {
        /// Cells of air on each side of a cell's text. Generous,
        /// because without vertical rules the gap between columns is
        /// the only thing keeping them apart.
        const PAD: f32 = 1.5;
        const MIN: f32 = 3.0;

        let count = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if count == 0 {
            return Vec::new();
        }
        let cell_width = |spans: &[Span], measure: &mut dyn Measure| -> f32 {
            spans
                .iter()
                .map(|s| measure.width(&s.text, s.style, Scale::Body))
                .sum()
        };
        // A heading's cells are bold, and bold is wider. Promoted
        // before the columns are measured, or the header is measured
        // in one face and drawn in another — `Default` came out as
        // `Def…` in a column sized for `exp`.
        let aligns: Vec<Align> = (0..count).map(|i| self.align(i)).collect();
        let Table { mut rows, head, .. } = self;
        for (row, heading) in rows.iter_mut().zip(&head) {
            if !heading {
                continue;
            }
            for cell in row.iter_mut() {
                for span in cell.iter_mut() {
                    if span.style == Style::Body {
                        span.style = Style::Bold;
                    }
                }
            }
        }

        let mut widths = vec![MIN; count];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell_width(cell, measure));
            }
        }

        // Shrink the widest column, repeatedly, until the row fits.
        // Taking from the widest keeps a narrow column readable
        // instead of trimming every column into uselessness.
        let room = columns.saturating_sub(indent) as f32;
        let total = |widths: &[f32]| widths.iter().sum::<f32>() + PAD * 2.0 * count as f32;
        while total(&widths) > room {
            let widest = widths
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i);
            let Some(widest) = widest else { break };
            if widths[widest] <= MIN {
                break;
            }
            widths[widest] -= 1.0;
        }

        // Where each column starts, once the widths are settled.
        let mut starts = Vec::with_capacity(count);
        let mut at = 0.0;
        for width in &widths {
            starts.push(at);
            at += width + PAD * 2.0;
        }
        let table_width = at;

        let rows: Vec<(Vec<Vec<Span>>, bool)> = rows.into_iter().zip(head).collect();
        let last = rows.len().saturating_sub(1);

        rows.into_iter()
            .enumerate()
            .map(|(index, (row, heading))| {
                let cells = (0..count)
                    .map(|i| {
                        let spans = row.get(i).cloned().unwrap_or_default();
                        Cell {
                            spans: clip_to(spans, widths[i], measure),
                            align: aligns[i],
                            x: starts[i] + PAD,
                            width: widths[i],
                        }
                    })
                    .collect();
                Line {
                    indent,
                    block: if heading {
                        Block::TableHead
                    } else {
                        Block::Table
                    },
                    row: Some(Row {
                        cells,
                        heading,
                        // Under the heading, and between body rows,
                        // but never under the last: a rule along the
                        // bottom closes a box, and this is not one.
                        rule: index < last,
                        width: table_width,
                    }),
                    ..Line::default()
                }
            })
            .collect()
    }
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

/// Truncates a cell to `width` cells, measured the way it is drawn,
/// marking the cut with an ellipsis.
fn clip_to(spans: Vec<Span>, width: f32, measure: &mut dyn Measure) -> Vec<Span> {
    let total: f32 = spans
        .iter()
        .map(|s| measure.width(&s.text, s.style, Scale::Body))
        .sum();
    if total <= width {
        return spans;
    }
    let mut out: Vec<Span> = Vec::new();
    let mut used = 0.0;
    let room = width - measure.width("…", Style::Body, Scale::Body);
    for span in spans {
        let mut kept = String::new();
        for ch in span.text.chars() {
            let next = measure.width(&format!("{kept}{ch}"), span.style, Scale::Body);
            if used + next > room {
                break;
            }
            kept.push(ch);
        }
        let taken = measure.width(&kept, span.style, Scale::Body);
        let whole = kept.chars().count() == span.text.chars().count();
        if !kept.is_empty() {
            used += taken;
            out.push(Span {
                text: kept,
                ..span.clone()
            });
        }
        if !whole {
            break;
        }
    }
    out.push(Span::plain("…", Style::Body));
    out
}

/// Renders Markdown as a page `columns` cells wide, measuring runs with
/// the face they will be drawn in.
///
/// The parsing is pulldown-cmark's: CommonMark, plus the tables, task
/// lists, strikethrough, footnotes and front matter a README actually
/// contains. The layout is ours — events become styled runs, runs become
/// the lines below — and that is the half that decides what a document
/// looks like on a character grid.
///
/// `columns` is the pane, not the column: the margins and the measure
/// come out of it here, so that whatever draws the result can work them
/// out again and arrive at the same numbers.
pub fn render_with(source: &str, columns: usize, measure: &mut dyn Measure) -> Vec<Line> {
    let room = columns.max(20);
    let column = column(room, measure);
    Preview::new(column, figures(room), measure).lines(source)
}

/// As [`render_with`], on the grid, where a character is one cell.
#[cfg(test)]
pub fn render(source: &str, columns: usize) -> Vec<Line> {
    render_with(source, columns, &mut Cells)
}

/// Renders Markdown wrapped to exactly `columns`, with no page around it.
///
/// What a tooltip wants. A popup is a box of that many cells and draws
/// its own padding, so the margins a document keeps would be a second set
/// of them, and the text inside would stop short of its own frame.
pub fn render_boxed(source: &str, columns: usize) -> Vec<Line> {
    let columns = columns.max(20) as f32;
    Preview::new(columns, columns, &mut Cells).lines(source)
}

/// How wide the document's column is, in cells, given the pane it sits in.
///
/// The pane, less a margin on each side, and only then capped: a margin
/// is what makes a page, and a cap is what keeps a line readable. The
/// margin is a share of the pane rather than a fixed inset, so a preview
/// beside the code it documents does not spend a tenth of itself on air
/// at one size and half of itself at another.
///
/// The cap is measured in alphabets, which is how type is measured: the
/// lowercase a–z of a face set solid is about thirteen ems, and four of
/// them is a hundred-odd characters. Wider than a book's measure, because
/// a window is read at arm's length rather than held — and lifted again
/// on a pane too wide for it, because a column of that measure centred in
/// a maximised window spends more of the window on air than on the
/// document, which is the complaint this answers.
///
/// Everything in the document shares it. Code set wider than the prose
/// beside it reads as a different page.
pub fn column(columns: usize, measure: &mut dyn Measure) -> f32 {
    const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz";
    const ALPHABETS: f32 = 4.3;
    /// How much of a wide pane the document may use, once a measure of
    /// its own is no longer what is holding it back.
    const SHARE: f32 = 0.72;
    let room = columns as f32;
    let measured = measure.width(ALPHABET, Style::Body, Scale::Body) * ALPHABETS;
    let widest = measured.max(room * SHARE);
    let margin = margin(columns);
    // Never so narrow that a table has nowhere to go, even when that
    // costs the margin.
    (room - 2.0 * margin).min(widest).max(room.min(24.0))
}

/// The margin each side of the document, in cells.
///
/// A share of the pane, so the page looks like a page at any width, and
/// bounded, so a wide window is not mostly margin. Taken out by
/// [`column`] and used again when the preview is drawn, because a figure
/// wider than the column still starts at the margin.
pub fn margin(columns: usize) -> f32 {
    (columns as f32 / 12.0).clamp(2.0, 10.0)
}

/// How wide a figure may be, in cells: the page inside its margins.
///
/// Wider than [`column`], which is capped at what prose reads well at. A
/// drawn diagram is not read line by line, and one refused for want of
/// room is shown as its own source instead, which is the worse outcome.
pub fn figures(columns: usize) -> f32 {
    (columns as f32 - 2.0 * margin(columns)).max(8.0)
}

/// The row a `#fragment` link lands on: the heading with that anchor.
///
/// Matched on the slug rather than on the text, because the slug is what
/// the link carries and what every other renderer of the same file
/// agrees on.
pub fn anchor(lines: &[Line], fragment: &str) -> Option<usize> {
    let want = slug(fragment);
    if want.is_empty() {
        return None;
    }
    let heading = |line: &Line| {
        line.spans
            .iter()
            .any(|s| matches!(s.style, Style::Heading(_)))
    };
    let mut at = 0;
    while at < lines.len() {
        if !heading(&lines[at]) {
            at += 1;
            continue;
        }
        // A long heading wrapped onto a second line is still one
        // heading, and its anchor is made out of the whole of it.
        let end = (at..lines.len())
            .take_while(|i| heading(&lines[*i]))
            .last()
            .unwrap_or(at)
            + 1;
        let text: String = lines[at..end]
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.as_str())
            .collect();
        if slug(&text) == want {
            return Some(at);
        }
        at = end;
    }
    None
}

/// A heading's anchor, as every Markdown renderer makes one: lowercase,
/// spaces hyphenated, and everything that is not a letter, a digit, a
/// hyphen or an underscore dropped.
fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

/// What the event stream is being turned into, while it is being turned.
///
/// A pull parser hands over a document as a flat sequence of starts and
/// ends, so everything that is "where am I" lives here: which inline
/// styles are open, how deep the lists are, whether a table or a fence is
/// being collected.
struct Preview<'a> {
    columns: usize,
    /// How tall a row is against a cell's width, for figures.
    aspect: f32,
    /// How wide a figure may be, in cells. Wider than the column: prose
    /// is capped at what it reads well at, a diagram only by the page.
    room: usize,
    /// Where prose wraps, which is narrower than the column.
    prose: f32,
    measure: &'a mut dyn Measure,
    out: Vec<Line>,
    /// The inline runs of the block being read.
    spans: Vec<Span>,
    /// Inline styles, innermost last, folded together by [`merge`].
    styles: Vec<Style>,
    /// Open lists, innermost last. `Some(n)` numbers the next item.
    lists: Vec<Option<u64>>,
    /// How many block quotes deep, for the bar and the indent.
    quote: usize,
    /// Set while a footnote's definition is being read.
    footnote: bool,
    /// Set while an image's alt text is being collected: an image is
    /// named rather than drawn, and the name is all of it we keep.
    alt: Option<String>,
    /// Open link targets, innermost last: a link, an `<a href>`, or the
    /// image being read. Empty for the text that is only text.
    hrefs: Vec<Arc<str>>,
    /// A code block's language, and the text inside it.
    code: Option<(String, String)>,
    table: Option<Table>,
    /// Set while front matter is being skipped.
    metadata: bool,
}

impl<'a> Preview<'a> {
    /// `column` is where prose wraps and `room` is how wide a figure may
    /// be; a tooltip passes the same number for both.
    fn new(column: f32, room: f32, measure: &'a mut dyn Measure) -> Self {
        let aspect = measure.aspect();
        Self {
            columns: column as usize,
            aspect,
            room: room as usize,
            prose: column,
            measure,
            out: Vec::new(),
            spans: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            quote: 0,
            footnote: false,
            alt: None,
            hrefs: Vec::new(),
            code: None,
            table: None,
            metadata: false,
        }
    }

    fn lines(mut self, source: &str) -> Vec<Line> {
        use pulldown_cmark::{Event, Options, Parser};

        // Only what the layout below can draw. Anything else — maths,
        // definition lists, wikilinks — would arrive as events nothing
        // here knows how to put on the grid, which is worse than leaving
        // it as the text it was written as.
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
        options.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);

        for event in Parser::new_ext(source, options) {
            match event {
                Event::Start(tag) => self.open(tag),
                Event::End(tag) => self.close(tag),
                Event::Text(text) => self.content(&text),
                Event::Code(text) => {
                    let style = merge(self.style(), Style::Code);
                    self.push(&text, style);
                }
                // Raw HTML inside a paragraph is part of it.
                Event::InlineHtml(raw) => self.html(&raw),
                // A block of it is a block: `<summary>x</summary>` and
                // the paragraph under it are two things, and run
                // together they read as one word.
                Event::Html(raw) => {
                    self.flush(Scale::Body);
                    self.html(&raw);
                    self.flush(Scale::Body);
                }
                // A line break in the source is a word break here: the
                // column is ours to wrap to, not the author's.
                Event::SoftBreak => self.push(" ", Style::Body),
                // A hard break ends the line without ending the block.
                Event::HardBreak => self.flush(Scale::Body),
                Event::Rule => {
                    self.blank();
                    self.out.push(Line::default().decorated(Block::Rule));
                    self.blank();
                }
                Event::TaskListMarker(done) => {
                    let glyph = if done { '☑' } else { '☐' };
                    self.push(&format!("{glyph} "), Style::Bullet);
                }
                // Both ends of a footnote: the mark in the text, and the
                // marker on its definition below.
                Event::FootnoteReference(label) => {
                    self.push(&format!("[{label}]"), Style::Link);
                }
                Event::InlineMath(text) | Event::DisplayMath(text) => {
                    let style = merge(self.style(), Style::Code);
                    self.push(&text, style);
                }
            }
        }
        self.flush(Scale::Body);
        while self.out.last().is_some_and(Line::is_blank) {
            self.out.pop();
        }
        self.out
    }

    fn open(&mut self, tag: pulldown_cmark::Tag<'_>) {
        use pulldown_cmark::{CodeBlockKind, Tag};

        // A block opening inside a list item ends that item's own text:
        // the nested list in `- one` / `  - two` starts before the outer
        // item does, and without this the two run together on one line.
        // Not a paragraph, though — in a loose list that *is* the item's
        // text, and the marker in front of it is waiting for it.
        if matches!(
            tag,
            Tag::List(_)
                | Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::Table(_)
                | Tag::FootnoteDefinition(_)
        ) {
            self.flush(Scale::Body);
        }

        match tag {
            Tag::Paragraph | Tag::Heading { .. } => {}
            Tag::BlockQuote(_) => self.quote += 1,
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    // The language is the first word: `rust,no_run` is
                    // one tag, and the rest is for whoever runs it.
                    CodeBlockKind::Fenced(info) => info
                        .split([' ', ',', ';', '{'])
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(first) => self.lists.push(first),
            Tag::Item => {
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    // An ordered list counts from where it said to.
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => format!("{} ", bullet(depth)),
                };
                self.push(&marker, Style::Bullet);
            }
            Tag::FootnoteDefinition(label) => {
                self.footnote = true;
                self.push(&format!("[{label}] "), Style::Bullet);
            }
            Tag::Table(aligns) => {
                self.table = Some(Table {
                    aligns: aligns.iter().map(Align::of).collect(),
                    ..Table::default()
                });
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.row();
                    table.heading_row();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.row();
                }
            }
            Tag::TableCell => {}
            Tag::Emphasis => self.styles.push(Style::Emphasis),
            Tag::Strong => self.styles.push(Style::Bold),
            Tag::Strikethrough => self.styles.push(Style::Strike),
            // The text is what was written; the target is what the run
            // is for, and rides on the spans so that a click can follow
            // it.
            Tag::Link { dest_url, .. } => {
                self.styles.push(Style::Link);
                self.hrefs.push(Arc::from(&*dest_url));
            }
            Tag::Image { dest_url, .. } => {
                self.alt = Some(String::new());
                self.hrefs.push(Arc::from(&*dest_url));
            }
            // Front matter is metadata for whatever publishes the file,
            // not part of the document, so it is hidden the way a
            // renderer hides it rather than shown as a list of keys.
            Tag::MetadataBlock(_) => self.metadata = true,
            _ => {}
        }
    }

    fn close(&mut self, tag: pulldown_cmark::TagEnd) {
        use pulldown_cmark::TagEnd;

        match tag {
            TagEnd::Paragraph => {
                self.flush(Scale::Body);
                self.blank();
            }
            TagEnd::Heading(level) => {
                let spans = std::mem::take(&mut self.spans);
                push_heading(&mut self.out, spans, level as u8, self.prose, self.measure);
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.quote = self.quote.saturating_sub(1);
                self.blank();
            }
            TagEnd::CodeBlock => self.end_code(),
            TagEnd::List(_) => {
                self.lists.pop();
                // Only the outermost: a nested list ending in the
                // middle of its parent is not the end of a block, and
                // a blank line there broke one list into two.
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            // A tight list's items hold text rather than paragraphs, so
            // this is where they are emitted — and without the blank line
            // between blocks, which is what makes a list a list.
            TagEnd::Item => self.flush(Scale::Body),
            TagEnd::FootnoteDefinition => {
                self.flush(Scale::Body);
                self.footnote = false;
                self.blank();
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let laid = table.lay_out(self.columns, 0, self.measure);
                    self.out.extend(laid);
                    self.blank();
                }
            }
            TagEnd::TableCell => {
                let spans = std::mem::take(&mut self.spans);
                if let Some(table) = self.table.as_mut() {
                    table.cell(spans);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {}
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                self.hrefs.pop();
            }
            TagEnd::Image => {
                let alt = self.alt.take().unwrap_or_default();
                let src = self.hrefs.pop();
                // A badge is an image inside a link, and clicking it
                // means the link: the picture is what it is wearing.
                let href = self.href().or(src);
                self.spans.push(picture(alt.trim(), href));
            }
            TagEnd::MetadataBlock(_) => self.metadata = false,
            _ => {}
        }
    }

    /// Text, wherever it lands: a fence's contents, an image's name, or a
    /// run in the paragraph being read.
    fn content(&mut self, text: &str) {
        if self.metadata {
            return;
        }
        if let Some((_, code)) = self.code.as_mut() {
            code.push_str(text);
            return;
        }
        if let Some(alt) = self.alt.as_mut() {
            alt.push_str(text);
            return;
        }
        let style = self.style();
        // CommonMark leaves a bare URL as text; every renderer links it,
        // and a README is mostly bare URLs.
        if style == Style::Link || !text.contains("http") {
            self.push(text, style);
            return;
        }
        let mut rest = text;
        while let Some(at) = rest.find("http") {
            let (before, from) = rest.split_at(at);
            self.push(before, style);
            if !(from.starts_with("http://") || from.starts_with("https://")) {
                let (head, tail) = from.split_at("http".len());
                self.push(head, style);
                rest = tail;
                continue;
            }
            let end = from
                .char_indices()
                .take_while(|(_, c)| !c.is_whitespace() && !matches!(c, '<' | '>' | ')' | '"'))
                .map(|(at, c)| at + c.len_utf8())
                .last()
                .unwrap_or(0);
            // Trailing punctuation belongs to the sentence, not the link.
            let url = from[..end].trim_end_matches(['.', ',', ';', ':', '!']);
            self.push_to(url, merge(style, Style::Link), Some(Arc::from(url)));
            rest = &from[url.len()..];
        }
        self.push(rest, style);
    }

    /// Raw HTML, which Markdown passes through and this reads.
    ///
    /// A reader that showed the tags as text would be showing something
    /// the document never said. Known tags become the style they mean;
    /// unknown ones stay exactly as written, because `Vec<T>` in prose is
    /// commoner in a code editor's README than hand-written HTML.
    fn html(&mut self, raw: &str) {
        let mut rest = raw;
        while let Some(at) = rest.find('<') {
            let (before, from) = rest.split_at(at);
            self.loose_html_text(before);
            let Some(close) = from.find('>') else {
                self.loose_html_text(from);
                return;
            };
            self.tag(&from[..=close]);
            rest = &from[close + 1..];
        }
        self.loose_html_text(rest);
    }

    /// Text between tags in an HTML block, where the newlines are the
    /// author's formatting rather than anything to draw.
    fn loose_html_text(&mut self, text: &str) {
        if text.contains('\n') {
            let flattened = text.replace('\n', " ");
            let trimmed = flattened.trim();
            if !trimmed.is_empty() {
                self.content(trimmed);
            }
            return;
        }
        self.content(text);
    }

    fn tag(&mut self, raw: &str) {
        let inner = raw.trim_start_matches('<').trim_end_matches('>');
        let closing = inner.starts_with('/');
        let name = inner
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        // An `<img>` written in the middle of a paragraph is still an
        // image, and the one void tag worth naming.
        if name == "img" && !closing {
            let alt = attribute(raw, "alt")
                .filter(|alt| !alt.trim().is_empty())
                .or_else(|| attribute(raw, "src"))
                .unwrap_or_else(|| "image".to_string());
            // The link it is wrapped in first, as for a badge written in
            // Markdown; its own source otherwise.
            let href = self
                .href()
                .or_else(|| attribute(raw, "src").map(|src| Arc::from(src.as_str())));
            self.spans.push(picture(&alt, href));
            return;
        }
        match inline_tag(&name) {
            Some(Some(style)) if !closing => {
                // Pushed even when the tag carries no target, so that
                // the closing tag has its own to take back off.
                if name == "a" {
                    let href = attribute(raw, "href").unwrap_or_default();
                    self.hrefs.push(Arc::from(href.as_str()));
                }
                self.styles.push(style);
            }
            Some(Some(style)) => {
                if name == "a" {
                    self.hrefs.pop();
                }
                if let Some(at) = self.styles.iter().rposition(|open| *open == style) {
                    self.styles.remove(at);
                }
            }
            // Known, but carrying no style of its own.
            Some(None) => {}
            None => self.push(raw, self.style()),
        }
    }

    /// The style a run takes here: every open one, folded together.
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::Body, |outer, inner| merge(outer, *inner))
    }

    /// The innermost open link target, if the run being read is inside
    /// one. An `<a>` without an `href` is not a link, and neither is the
    /// empty string it pushed to keep the stack balanced.
    fn href(&self) -> Option<Arc<str>> {
        self.hrefs
            .iter()
            .rev()
            .find(|href| !href.trim().is_empty())
            .cloned()
    }

    fn push(&mut self, text: &str, style: Style) {
        let href = self.href();
        self.push_to(text, style, href);
    }

    /// As [`Self::push`], for a run whose target is not the one the
    /// document has open: a bare url, which is its own.
    fn push_to(&mut self, text: &str, style: Style, href: Option<Arc<str>>) {
        if text.is_empty() {
            return;
        }
        push_run(&mut self.spans, text, style, href);
    }

    /// Where the block being read starts, in cells.
    fn indent(&self) -> usize {
        // Two cells for a quote, not three: the bar is drawn at the
        // column's own edge, so the text only has to clear the bar.
        self.quote * 2 + self.lists.len() * 2 + usize::from(self.footnote) * 2
    }

    /// Wraps what has been read into lines and puts them out.
    fn flush(&mut self, scale: Scale) {
        if self.spans.is_empty() {
            return;
        }
        let mut spans = std::mem::take(&mut self.spans);
        let quoted = self.quote > 0;
        if quoted {
            for span in &mut spans {
                if span.style == Style::Body {
                    span.style = Style::Quote;
                }
            }
        }
        let lines = wrap(spans, self.indent(), self.prose, scale, self.measure);
        self.out.extend(lines.into_iter().map(|line| {
            if quoted {
                line.decorated(Block::Quote)
            } else {
                line
            }
        }));
    }

    /// A blank line between blocks, and never two in a row: a heading
    /// already leaves one, and the source usually leaves another.
    fn blank(&mut self) {
        // A rule — under a heading or across the page — is the air
        // under it already, and a blank row after it is a second
        // helping.
        let spoken_for = self.out.last().is_some_and(|line| {
            line.is_blank() || matches!(line.block, Block::HeadingRule | Block::Rule)
        });
        if !self.out.is_empty() && !spoken_for {
            self.out.push(Line::default());
        }
    }

    /// Emits the fenced or indented block that has just ended.
    fn end_code(&mut self) {
        let Some((language, text)) = self.code.take() else {
            return;
        };
        // The source as written, which is what a copy hands back and what
        // a diagram is drawn from.
        let source: Arc<str> = Arc::from(text.trim_end_matches('\n'));
        let at = self.out.len();
        match diagram(&language, &source, self.room, self.aspect) {
            Some(drawn) => self.out.extend(drawn),
            None => {
                let code: Vec<&str> = source.lines().collect();
                self.out.extend(code_block(language, &code, self.columns));
            }
        }
        // Carried on every row of the block rather than only its first,
        // so a click anywhere in it can copy it.
        for line in &mut self.out[at..] {
            if matches!(line.block, Block::Code | Block::Diagram) {
                line.code = Some(Arc::clone(&source));
            }
        }
        self.blank();
    }
}

/// Emits a heading, with the air above and the rule below a browser gives
/// it.
fn push_heading(
    out: &mut Vec<Line>,
    spans: Vec<Span>,
    level: u8,
    columns: f32,
    measure: &mut dyn Measure,
) {
    let scale = heading_scale(level);
    // Air above. A heading set larger takes two rows with its text on
    // the lower one, so that row *is* the air — a blank line as well
    // made the space above an `h2` three rows deep.
    if scale == Scale::Body && out.last().is_some_and(|l| !l.is_blank()) {
        out.push(Line::default());
    }
    let mut spans = spans;
    for span in &mut spans {
        // Code and links inside a heading keep their own colour; the rest
        // becomes heading text.
        if matches!(span.style, Style::Body | Style::Bold | Style::Emphasis) {
            span.style = Style::Heading(level);
        }
    }
    out.extend(wrap(spans, 0, columns, scale, measure));
    // The top two levels get a rule, as they do in a browser.
    if level <= 2 {
        out.push(Line::default().decorated(Block::HeadingRule));
    }
}

/// A `mermaid` fence drawn as a diagram, when it is one this can draw.
///
/// `room` is the page rather than the prose column: a diagram is a
/// figure, and one refused for want of a few cells is shown as its own
/// source, which is worse than a figure wider than the text above it.
///
/// `None` for every other fence, and for a Mermaid diagram the reader
/// will not lay out — the preview then shows the source, which says
/// what was meant even when the picture cannot be drawn.
fn diagram(language: &str, source: &str, room: usize, aspect: f32) -> Option<Vec<Line>> {
    if !language.trim().eq_ignore_ascii_case("mermaid") {
        return None;
    }
    // Two cells of indent and two of margin, which is what the tint
    // behind the block takes up on either side of it.
    let drawn = crate::mermaid::render(source, room.saturating_sub(4), aspect)?;
    // A blank row above and below, so the tint behind the figure has
    // air in it rather than ending flush against the drawing.
    let pad = || Line {
        indent: 2,
        block: Block::Diagram,
        ..Line::default()
    };
    Some(vec![
        pad(),
        Line {
            indent: 2,
            block: Block::Diagram,
            // The fence's own source, so clicking the figure copies
            // the Mermaid back rather than nothing.
            code: Some(Arc::from(source)),
            figure: Some(Arc::new(drawn)),
            ..Line::default()
        },
        pad(),
    ])
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
            .unwrap_or_else(|| vec![Span::plain(trimmed, Style::Code)]);
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
                figure: None,
                row: None,
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
            current.push(Span::plain(text, span.style));
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

/// Breaks spans into lines at word boundaries, keeping each span's style.
///
/// `scale` is passed because a heading's glyphs are wider than a body
/// cell, so the same number of characters needs more of the column.
fn wrap(
    spans: Vec<Span>,
    indent: usize,
    columns: f32,
    scale: Scale,
    measure: &mut dyn Measure,
) -> Vec<Line> {
    // No dividing by the scale: a heading's glyphs are wider, and the
    // measure below already says by how much.
    let room = (columns - indent as f32).max(8.0);

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
                Some(last) if last.style == span.style && last.href == span.href => {
                    last.text.push_str(piece)
                }
                _ => word.push(Span {
                    text: piece.to_string(),
                    style: span.style,
                    href: span.href.clone(),
                }),
            }
            open = true;
        }
    }

    let mut lines = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0.0f32;
    let mut space = 0.0f32;
    for word in words {
        let len: f32 = word
            .iter()
            .map(|span| measure.width(&span.text, span.style, scale))
            .sum();
        if used > 0.0 && used + len + space > room {
            lines.push(Line {
                spans: std::mem::take(&mut current),
                indent,
                block: Block::Plain,
                scale,
                code: None,
                figure: None,
                row: None,
            });
            used = 0.0;
        }
        if used > 0.0 {
            // The separator joins the two runs it sits between when they
            // agree, so a link's underline and a strikethrough run through
            // the spaces inside the phrase and stop at its edges.
            let before = current.last();
            let after = word.first();
            let (style, href) = match (before, after) {
                (Some(a), Some(b)) if a.style == b.style && a.href == b.href => {
                    (a.style, a.href.clone())
                }
                // The gap after a marker is a cell rather than a
                // proportional space, so a checkbox does not sit against
                // the first word of its item.
                (Some(a), _) if a.style == Style::Bullet => (Style::Bullet, None),
                _ => (Style::Body, None),
            };
            used += measure.width(" ", style, scale);
            push_run(&mut current, " ", style, href);
        }
        for piece in word {
            let Span { text, style, href } = piece;
            used += measure.width(&text, style, scale);
            push_run(&mut current, &text, style, href);
        }
        // What the next word will cost to separate from this one.
        space = measure.width(" ", Style::Body, scale);
    }
    if !current.is_empty() {
        lines.push(Line {
            spans: current,
            indent,
            block: Block::Plain,
            scale,
            code: None,
            figure: None,
            row: None,
        });
    }
    lines
}

/// Appends text to a line, joining it to the previous run when the style
/// and the target match, so a wrapped line is a handful of runs rather
/// than one per word.
fn push_run(line: &mut Vec<Span>, text: &str, style: Style, href: Option<Arc<str>>) {
    match line.last_mut() {
        Some(last) if last.style == style && last.href == href => last.text.push_str(text),
        _ => line.push(Span {
            text: text.to_string(),
            style,
            href,
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
    fn every_row_of_a_table_puts_its_cells_in_the_same_columns() {
        // Columns lining up down the page is the whole of what makes a
        // table a table, and it is now a shared set of positions
        // rather than a shared amount of padding.
        let out = render(
            "| Name | Size |\n|---|---:|\n| a.rs | 12 |\n| bb.rs | 3 |\n",
            60,
        );
        let rows: Vec<&Row> = out.iter().filter_map(|l| l.row.as_ref()).collect();
        assert_eq!(rows.len(), 3, "a header and two body rows");

        let columns = |r: &Row| -> Vec<(u32, u32)> {
            r.cells
                .iter()
                .map(|c| (c.x.to_bits(), c.width.to_bits()))
                .collect()
        };
        let first = columns(rows[0]);
        for row in &rows[1..] {
            assert_eq!(columns(row), first, "every row shares the columns");
        }

        assert!(rows[0].heading, "the first row is the header");
        assert!(rows[0].rule, "with a rule under it");
        assert!(!rows[2].rule, "and none under the last row");
        assert!(
            rows[0].cells[0]
                .spans
                .iter()
                .all(|s| s.style == Style::Bold),
            "a header is set bold"
        );
    }

    #[test]
    fn a_column_is_aligned_the_way_its_delimiter_says() {
        // The markers were being parsed and then thrown away, so every
        // column came out left-aligned however it was written.
        let out = render("| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n", 60);
        let row = out
            .iter()
            .find_map(|l| l.row.as_ref().filter(|r| !r.heading))
            .expect("a body row");
        let aligns: Vec<Align> = row.cells.iter().map(|c| c.align).collect();
        assert_eq!(aligns, vec![Align::Left, Align::Centre, Align::Right]);
    }

    #[test]
    fn a_table_with_only_a_header_needs_no_separator() {
        let out = render("| One | Two |\n|---|---|\n", 60);
        let rows: Vec<&Row> = out.iter().filter_map(|l| l.row.as_ref()).collect();
        assert_eq!(rows.len(), 1, "one row and nothing to separate it from");
        assert!(!rows[0].rule, "so no rule under it");
    }

    /// A header is measured in the face it is drawn in. Measuring the
    /// plain text and drawing it bold made `Default` come out as
    /// `Def…` in a column sized for `exp`.
    #[test]
    fn a_header_is_not_clipped_by_its_own_weight() {
        let out = render("| Default |\n|---|\n| exp |\n", 60);
        let header = out
            .iter()
            .find_map(|l| l.row.as_ref().filter(|r| r.heading))
            .expect("a header");
        let text: String = header.cells[0].spans.iter().map(|s| &*s.text).collect();
        assert_eq!(text, "Default", "the whole word fits");
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
        let figure = out
            .iter()
            .find_map(|l| l.figure.clone())
            .expect("a drawn figure");
        let said: Vec<&str> = figure.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(said.contains(&"One") && said.contains(&"Two"), "{said:?}");
        assert!(!figure.svg.is_empty(), "and shapes to draw");
        assert!(
            out.iter().any(|l| l.block == Block::Diagram),
            "it should be a diagram, not a code block"
        );
        // Copying gives back the mermaid, not the picture.
        let source = out.iter().find_map(|l| l.code.clone()).expect("a source");
        assert!(source.contains("flowchart TB"), "{source}");
    }

    /// A figure is one line of the preview however tall it is, and
    /// says so — the rows it reports are what scrolling counts.
    #[test]
    fn a_figure_is_one_line_as_tall_as_it_says() {
        let src = "```mermaid\nflowchart TB\n  A[One] --> B[Two]\n```\n";
        let out = render(src, 60);
        let line = out.iter().find(|l| l.figure.is_some()).expect("a figure");
        let figure = line.figure.as_deref().expect("a figure");
        assert!(figure.rows > 1, "a diagram is more than one row tall");
        assert_eq!(line.rows(), figure.rows, "and the line reports them");
        assert_eq!(line.len_chars(), 0, "it has no text to select");
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

    /// Four spaces is a code block in CommonMark, and this used to be
    /// wrapped as prose — which is what every `cargo run` line in our own
    /// README was being drawn as.
    #[test]
    fn an_indented_block_is_code_rather_than_prose() {
        let out = render(
            "text:\n\n    cargo run -- a b\n    second line\n\nafter\n",
            60,
        );
        let code: Vec<String> = out
            .iter()
            .filter(|l| l.block == Block::Code)
            .map(text_of)
            .filter(|t| !t.is_empty())
            .collect();
        assert_eq!(code, vec!["cargo run -- a b", "second line"]);
        // And it is copyable, like any other block of code.
        assert!(out.iter().any(|l| l.code.is_some()));
    }

    #[test]
    fn a_footnote_is_a_mark_in_the_text_and_a_block_below() {
        let out = render("A claim[^1] stands.\n\n[^1]: Because it does.\n", 60);
        let joined: String = out.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains("[1] stands"),
            "the mark reads inline: {joined:?}"
        );
        assert!(joined.contains("Because it does."), "and the note is kept");
        assert!(
            !joined.contains("[^1]"),
            "the marker is markup, not text: {joined:?}"
        );
    }

    /// A tight list is a list; a loose one is a sequence of paragraphs
    /// that happen to be bulleted, and reads like one.
    #[test]
    fn a_loose_list_is_spaced_and_a_tight_one_is_not() {
        let tight = render("- one\n- two\n", 60);
        assert!(
            tight.iter().all(|l| !l.is_blank()),
            "nothing between the items of a tight list"
        );

        let loose = render("- one\n\n- two\n", 60);
        assert!(
            loose.iter().any(Line::is_blank),
            "a loose list keeps the air its author put there"
        );
    }

    /// A sublist ending is not a block ending. Treating it as one put
    /// a blank line between the nested items and whatever came next,
    /// and one list read as three.
    #[test]
    fn a_nested_list_does_not_break_the_one_it_is_in() {
        let out = render("- one\n- two\n  - inner\n- three\n", 60);
        assert!(
            out.iter().all(|l| !l.is_blank()),
            "a tight list stays tight through its sublists: {:?}",
            out.iter().map(text_of).collect::<Vec<_>>()
        );
        let joined: String = out.iter().map(text_of).collect::<Vec<_>>().join("|");
        assert!(joined.contains("inner"), "the sublist is still there");
        assert!(joined.contains("three"), "and so is what follows it");
    }

    #[test]
    fn a_nested_list_is_indented_further_than_its_parent() {
        let out = render("- one\n  - inner\n", 60);
        let outer = out
            .iter()
            .find(|l| text_of(l).contains("one"))
            .expect("the outer item");
        let inner = out
            .iter()
            .find(|l| text_of(l).contains("inner"))
            .expect("the inner item");
        assert!(
            inner.indent > outer.indent,
            "depth is what says which list an item is in: {} vs {}",
            inner.indent,
            outer.indent
        );
    }

    /// `<summary>` and the paragraph under it are two blocks. Run
    /// together they came out as one word with no space in it.
    #[test]
    fn an_html_block_is_a_block_of_its_own() {
        let out = render(
            "<details>\n<summary>Heading</summary>\n\nBody text.\n\n</details>\n",
            60,
        );
        let rows: Vec<String> = out.iter().map(text_of).filter(|t| !t.is_empty()).collect();
        assert_eq!(rows, vec!["Heading", "Body text."], "got {rows:#?}");
    }

    /// The definition may come after the use, which is why this used to
    /// need a pass of its own over the document.
    #[test]
    fn a_reference_defined_later_is_still_a_link() {
        let out = render("see [the docs][d] here\n\n[d]: https://example.com\n", 60);
        assert_eq!(text_of(&out[0]), "see the docs here");
        assert!(styles(&out[0]).contains(&Style::Link));
    }

    #[test]
    fn an_entity_becomes_the_character_it_names() {
        let out = render("a &amp; b &lt;c&gt; d\n", 60);
        assert_eq!(text_of(&out[0]), "a & b <c> d");
    }

    /// Emphasis inside emphasis is both at once, which is a rule about
    /// delimiter runs rather than about the characters.
    #[test]
    fn emphasis_inside_emphasis_is_both_at_once() {
        let out = render("**bold with *more* inside**\n", 60);
        let styles = styles(&out[0]);
        assert!(styles.contains(&Style::Bold), "got {styles:?}");
        assert!(styles.contains(&Style::BoldEmphasis), "got {styles:?}");
        assert_eq!(text_of(&out[0]), "bold with more inside");
    }

    /// Prose is set in a proportional face and everything whose meaning
    /// is its alignment is not. Set a table's frame proportionally and it
    /// comes apart; set a diagram's box characters proportionally and it
    /// stops being a picture.
    #[test]
    fn what_is_set_on_the_grid_and_what_is_set_as_prose() {
        use lithe_gpu::{Face, Family};

        assert_eq!(Style::Body.family(), Family::Prose);
        assert_eq!(Style::Heading(1).family(), Family::Prose);
        assert_eq!(Style::Code.family(), Family::Mono);
        // A marker lines up with the indents measured in cells around it.
        assert_eq!(Style::Bullet.family(), Family::Mono);

        // Weight and slant carry what they carry in a document.
        assert_eq!(Style::Emphasis.face(), Face::Italic);
        assert_eq!(Style::Bold.face(), Face::Bold);
        assert_eq!(Style::BoldEmphasis.face(), Face::BoldItalic);
        assert_eq!(Style::Heading(3).face(), Face::Bold);

        // A diagram is on the grid; a table no longer is, because its
        // cells are placed in measured columns and its prose is prose.
        let table = render("| a | b |\n|---|---|\n| 1 | 2 |\n", 60);
        assert!(
            table
                .iter()
                .filter(|l| l.row.is_some())
                .all(|l| !l.is_grid()),
            "a table row is not grid"
        );
    }

    /// Inline code is set a shade smaller than the prose around it: a
    /// monospace face at the same nominal size reads as larger, and a
    /// word of code stood proud of its own line.
    #[test]
    fn inline_code_is_set_smaller_than_the_line_it_is_in() {
        use lithe_gpu::Size;
        assert_eq!(Style::Code.size(Scale::Body), Size::Small);
        assert_eq!(Style::Body.size(Scale::Body), Size::Body);
        // In a heading the code *is* the heading, so it keeps the size.
        assert_eq!(Style::Code.size(Scale::Huge), Size::Huge);
    }

    /// A line of prose is as long as what is in it, not as long as the
    /// characters it happens to be spelled with.
    #[test]
    fn a_measured_column_wraps_on_what_it_measures() {
        /// Half a cell per character, as a proportional face is.
        struct Narrow;
        impl Measure for Narrow {
            fn width(&mut self, text: &str, _style: Style, scale: Scale) -> f32 {
                text.chars().count() as f32 * 0.5 * scale.width()
            }
        }

        let source = "word ".repeat(40);
        let wide = render_with(&source, 40, &mut Narrow);
        let grid = render(&source, 40);
        assert!(
            wide.len() < grid.len(),
            "narrower glyphs fit more words in a line: {} vs {}",
            wide.len(),
            grid.len()
        );
    }

    #[test]
    fn consecutive_lines_join_into_one_paragraph() {
        let out = render("one\ntwo\n\nthree\n", 80);
        assert_eq!(text_of(&out[0]), "one two", "got {:?}", text_of(&out[0]));
    }

    /// Where the run holding `text` goes, if anywhere.
    fn target(out: &[Line], text: &str) -> Option<String> {
        out.iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.text.contains(text))
            .and_then(|s| s.href.as_deref().map(str::to_string))
    }

    #[test]
    fn the_margins_are_margins_rather_than_half_the_page() {
        // The complaint this answers: a column measured for a book left
        // more air on either side of it than it used itself.
        let room = 117;
        let column = column(room, &mut Cells);
        let margin = (room as f32 - column) / 2.0;
        assert!(margin >= 2.0, "there is still a margin: {margin}");
        assert!(
            margin <= column / 4.0,
            "and it is a margin, not the page: {margin} either side of {column}"
        );
    }

    #[test]
    fn the_page_keeps_its_margins_at_every_width() {
        for room in [30usize, 60, 90, 117, 160, 225, 400] {
            let column = column(room, &mut Cells);
            let air = room as f32 - column;
            assert!(air >= 3.0, "{room}: a page has margins, got {air}");
            assert!(
                air <= room as f32 * 0.32,
                "{room}: and is not mostly margin: {air} of it"
            );
        }
    }

    #[test]
    fn a_figure_may_be_wider_than_the_prose_beside_it() {
        // Six nodes into one, which needs more cells than prose is set
        // to. Refusing it would have shown the fence as its own source.
        let source = "```mermaid\nflowchart TD\n    \
             one[the first step] --> hub[the one below]\n    \
             two[the second step] --> hub\n    \
             three[the third step] --> hub\n    \
             four[the fourth step] --> hub\n    \
             five[the fifth step] --> hub\n    \
             six[the sixth step] --> hub\n```\n";
        let out = render(source, 160);
        let widest = out
            .iter()
            .filter_map(|l| Some(l.indent + l.figure.as_deref()?.cols))
            .max()
            .expect("a drawn figure, not a fence");
        assert!(
            widest as f32 > column(160, &mut Cells),
            "the figure used the page: {widest}"
        );
        assert!(
            widest as f32 <= figures(160),
            "and stayed inside it: {widest}"
        );
    }

    #[test]
    fn a_figures_rows_are_all_one_width() {
        // The tint behind a figure is sized from the row it is drawn on,
        // so a ragged figure would stair-step down the page.
        let out = render("```mermaid\nflowchart TD\n    a[one] --> b[two]\n```\n", 80);
        let widths: Vec<usize> = out
            .iter()
            .filter(|l| l.block == Block::Diagram)
            .map(|l| l.spans.iter().map(|s| s.text.chars().count()).sum())
            .collect();
        assert!(widths.len() > 2, "a drawn figure: {widths:?}");
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
    }

    #[test]
    fn a_link_carries_where_it_goes() {
        let out = render(
            "see [the docs](https://example.com/a?b=1&c=2) for more\n",
            80,
        );
        assert_eq!(
            target(&out, "the docs").as_deref(),
            Some("https://example.com/a?b=1&c=2")
        );
        // And the prose around it goes nowhere.
        let prose = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.text.contains("for more"))
            .expect("the words after the link");
        assert!(prose.href.is_none(), "{prose:?}");
    }

    #[test]
    fn a_bare_url_is_its_own_target() {
        let out = render("read https://example.com/x today\n", 80);
        assert_eq!(
            target(&out, "https://example.com/x").as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn a_badge_goes_where_its_link_goes() {
        // An image inside a link, which is how every README begins. The
        // picture is what it is wearing; the click means the link.
        let out = render(
            "[![build](https://img.example/b.svg)](https://ci.example/job)\n",
            80,
        );
        assert_eq!(
            target(&out, "build").as_deref(),
            Some("https://ci.example/job")
        );
    }

    #[test]
    fn a_picture_on_its_own_points_at_itself() {
        let out = render("![a diagram](docs/diagram.png)\n", 80);
        assert_eq!(
            target(&out, "a diagram").as_deref(),
            Some("docs/diagram.png")
        );
        // Written as HTML, which a README does when it wants a width.
        let out = render(
            "<img src=\"docs/d.png\" alt=\"a picture\" width=\"40\">\n",
            80,
        );
        assert_eq!(target(&out, "a picture").as_deref(), Some("docs/d.png"));
    }

    #[test]
    fn a_wrapped_link_keeps_its_target_on_every_line() {
        let out = render(
            "[a link whose text is long enough to need two lines of a narrow column](x.md)\n",
            24,
        );
        let runs: Vec<&Span> = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style == Style::Link)
            .collect();
        assert!(runs.len() > 1, "it wrapped: {runs:?}");
        assert!(
            runs.iter().all(|s| s.href.as_deref() == Some("x.md")),
            "{runs:?}"
        );
    }

    #[test]
    fn an_html_anchor_carries_its_href() {
        let out = render("<a href=\"https://example.com\">home</a>\n", 80);
        assert_eq!(target(&out, "home").as_deref(), Some("https://example.com"));
        // One without a target is not a link, and its closing tag takes
        // back what it pushed rather than the one around it.
        let out = render("<a name=\"top\">here</a> and [there](t.md)\n", 80);
        assert_eq!(target(&out, "here"), None);
        assert_eq!(target(&out, "there").as_deref(), Some("t.md"));
    }

    #[test]
    fn a_relative_target_is_kept_as_written() {
        let out = render("[the guide](docs/guide.md#writing)\n", 80);
        assert_eq!(
            target(&out, "the guide").as_deref(),
            Some("docs/guide.md#writing")
        );
    }

    #[test]
    fn an_anchor_finds_the_heading_it_names() {
        let out = render("# Top\n\nwords\n\n## The Second Part!\n\nmore\n", 80);
        let at = anchor(&out, "the-second-part").expect("the heading");
        assert!(
            text_of(&out[at]).contains("The Second Part"),
            "landed on {:?}",
            text_of(&out[at])
        );
        assert_eq!(anchor(&out, "nowhere"), None);
    }
}
