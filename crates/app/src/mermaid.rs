//! A reader for the Mermaid flowcharts people put in a README.
//!
//! Not Mermaid. It draws the one diagram that actually turns up in a
//! project's documentation — a flowchart of boxes and arrows — onto the
//! same monospace grid everything else is drawn on, and refuses
//! everything else rather than drawing it wrong.
//!
//! Refusing is the important half. A sequence diagram rendered as a bad
//! flowchart is worse than the fence it came from: the source at least
//! says what was meant. Anything this cannot lay out returns `None` and
//! the preview shows the code.

use std::collections::HashMap;

/// Which way the layers run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

impl Dir {
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "TB" | "TD" => Self::Down,
            "BT" => Self::Up,
            "LR" => Self::Right,
            "RL" => Self::Left,
            _ => return None,
        })
    }

    /// Whether layers stack vertically.
    fn vertical(self) -> bool {
        matches!(self, Self::Down | Self::Up)
    }

    /// Whether the last layer is drawn first.
    fn reversed(self) -> bool {
        matches!(self, Self::Up | Self::Left)
    }
}

/// A node's outline, which is all this keeps of Mermaid's shapes.
///
/// Mermaid has a dozen; a monospace grid can honestly express two. A
/// rectangle is a step and a rounded box is anything else — a decision,
/// a terminus, a subroutine. Pretending to draw a rhombus out of box
/// characters looks like a mistake rather than a diamond.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Rect,
    Round,
}

#[derive(Clone, Debug)]
struct Node {
    label: String,
    shape: Shape,
    layer: usize,
    /// Column within the layer, and then the cell the box is centred on.
    order: usize,
    centre: usize,
}

#[derive(Clone, Debug)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
}

/// Renders a `mermaid` fence, or `None` when it is not a flowchart this
/// can lay out honestly.
pub fn render(source: &str, columns: usize) -> Option<Vec<String>> {
    let (dir, mut nodes, edges) = parse(source)?;
    if nodes.is_empty() {
        return None;
    }
    layer(&mut nodes, &edges);
    let canvas = if dir.vertical() {
        draw_vertical(dir, &mut nodes, &edges, columns)
    } else {
        draw_horizontal(dir, &mut nodes, &edges, columns)
    }?;
    Some(canvas)
}

/// Reads the subset of the flowchart grammar that a README uses.
#[allow(clippy::type_complexity)]
fn parse(source: &str) -> Option<(Dir, Vec<Node>, Vec<Edge>)> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("%%"));

    let header = lines.next()?;
    let mut words = header.split_whitespace();
    let kind = words.next()?;
    if !matches!(kind, "flowchart" | "graph") {
        return None;
    }
    // A missing direction means top-down, as Mermaid does.
    let dir = match words.next() {
        Some(word) => Dir::parse(word.trim_end_matches(';'))?,
        None => Dir::Down,
    };

    let mut index: HashMap<String, usize> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    for line in lines {
        let line = line.trim_end_matches(';');
        // Everything beyond a flat list of edges — subgraphs, styling,
        // class definitions, click handlers — is refused rather than
        // silently dropped, because dropping it draws a diagram that is
        // missing something without saying so.
        let first = line.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "subgraph" | "end" | "style" | "classDef" | "class" | "click" | "linkStyle"
        ) {
            return None;
        }

        match split_edge(line) {
            Some((left, label, right)) => {
                let from = intern(left, &mut index, &mut nodes)?;
                let to = intern(right, &mut index, &mut nodes)?;
                edges.push(Edge { from, to, label });
            }
            // A bare declaration, which is how a node gets a label it is
            // not given at its first edge.
            None => {
                intern(line, &mut index, &mut nodes)?;
            }
        }
    }
    Some((dir, nodes, edges))
}

/// Splits `A -->|label| B` into its three parts.
///
/// Returns `None` when the line holds no edge at all, which the caller
/// reads as a bare node declaration.
fn split_edge(line: &str) -> Option<(&str, Option<String>, &str)> {
    let (at, len) = last_arrow(line)?;
    let left = line[..at].trim();
    let mut rest = line[at + len..].trim_start();
    // `--o` and `--x` end an edge with a circle or a cross; the tail
    // letter is part of the arrow, not the start of the target's name.
    if let Some(after) = rest.strip_prefix(['o', 'x']) {
        if after.starts_with(char::is_whitespace) {
            rest = after.trim_start();
        }
    }
    let rest = rest.trim();
    if left.is_empty() || rest.is_empty() {
        return None;
    }

    // `|label|` sits after the arrow.
    let (label, right) = match rest.strip_prefix('|') {
        Some(after) => {
            let end = after.find('|')?;
            (
                Some(after[..end].trim().to_string()),
                after[end + 1..].trim(),
            )
        }
        None => (None, rest),
    };
    if right.is_empty() {
        return None;
    }

    // `A -- text --> B`: the text is left on the near side of the arrow.
    let (left, label) = match label {
        Some(l) => (left, Some(l)),
        None => match last_arrow(left) {
            Some((at, len)) => {
                let before = left[..at].trim();
                let text = left[at + len..].trim();
                if before.is_empty() || text.is_empty() {
                    (left, None)
                } else {
                    (before, Some(text.to_string()))
                }
            }
            None => (left, None),
        },
    };
    Some((left, label, right))
}

/// The last arrow in a line, as a byte offset and length.
///
/// An arrow is a maximal run of `-=.<>`, which is the only way to find
/// one correctly: matching a table of literals longest-first still picks
/// the `->` *inside* `-->` when it scans from the right, and splitting
/// there leaves `A -` as the source node.
///
/// The last one rather than the first, because `A -- text --> B` writes
/// its label between two of them and the real arrow is the second.
fn last_arrow(line: &str) -> Option<(usize, usize)> {
    const ARROW: &[char] = &['-', '=', '.', '<', '>'];
    let bytes: Vec<(usize, char)> = line.char_indices().collect();
    let mut found = None;
    let mut i = 0;
    while i < bytes.len() {
        if !ARROW.contains(&bytes[i].1) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && ARROW.contains(&bytes[i].1) {
            i += 1;
        }
        let run: String = bytes[start..i].iter().map(|(_, c)| *c).collect();
        // Two characters at least, and a line or a double of them: a
        // single `-` is a hyphen in an identifier, and `...` is an
        // ellipsis in a label.
        if run.chars().count() >= 2 && run.contains(['-', '=']) {
            let end = bytes.get(i).map_or(line.len(), |(at, _)| *at);
            found = Some((bytes[start].0, end - bytes[start].0));
        }
    }
    found
}

/// Finds or creates the node a token names, taking its label from the
/// first mention that carries one.
fn intern(token: &str, index: &mut HashMap<String, usize>, nodes: &mut Vec<Node>) -> Option<usize> {
    let (id, label, shape) = node_parts(token)?;
    if let Some(&at) = index.get(&id) {
        if let Some(label) = label {
            nodes[at].label = label;
            nodes[at].shape = shape;
        }
        return Some(at);
    }
    let at = nodes.len();
    index.insert(id.clone(), at);
    nodes.push(Node {
        label: label.unwrap_or(id),
        shape,
        layer: 0,
        order: 0,
        centre: 0,
    });
    Some(at)
}

/// Splits `A[Label]` into its id, its label and its outline.
fn node_parts(token: &str) -> Option<(String, Option<String>, Shape)> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let open = token.find(['[', '(', '{', '>']);
    let Some(open) = open else {
        let id = token.to_string();
        return id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            .then_some((id, None, Shape::Rect));
    };

    let id = token[..open].trim().to_string();
    if id.is_empty() {
        return None;
    }
    let inner = &token[open..];
    // A rectangle is `[..]`; everything else Mermaid offers is drawn
    // rounded. See `Shape`.
    let shape = if inner.starts_with('[') && !inner.starts_with("[[") && !inner.starts_with("[(") {
        Shape::Rect
    } else {
        Shape::Round
    };
    let label = inner
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '>' | '"'))
        .trim()
        .to_string();
    Some((id, (!label.is_empty()).then_some(label), shape))
}

/// Puts each node one layer below its deepest predecessor.
///
/// Relaxed a bounded number of times rather than sorted topologically,
/// because a flowchart with a loop in it is still a flowchart someone
/// wants to look at — the cap is what keeps a cycle from running
/// forever, and the layering it settles on is good enough to draw.
fn layer(nodes: &mut [Node], edges: &[Edge]) {
    for _ in 0..nodes.len() {
        let mut moved = false;
        for edge in edges {
            if edge.from == edge.to {
                continue;
            }
            let want = nodes[edge.from].layer + 1;
            if nodes[edge.to].layer < want {
                nodes[edge.to].layer = want;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    // Order within a layer is the order the nodes were first mentioned,
    // which is the order they are written in and so the order they are
    // expected in.
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for node in nodes.iter_mut() {
        let slot = seen.entry(node.layer).or_insert(0);
        node.order = *slot;
        *slot += 1;
    }
}

/// Cells a box takes, including its border and one space of padding.
fn box_width(label: &str) -> usize {
    label.chars().count() + 4
}

const BOX_H: usize = 3;
/// Rows between one layer of boxes and the next.
///
/// Three, not two: an arrow needs a row to leave by, a row to run
/// across, and a row to arrive on. With two the arrowhead landed on the
/// connector and came out as `▼─yes───▼`.
const GAP_V: usize = 3;
/// Cells between two boxes in the same layer.
const GAP_H: usize = 2;
/// Columns between one layer of boxes and the next.
const GAP_LR: usize = 4;

struct Canvas {
    cells: Vec<Vec<char>>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![' '; width]; height],
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if let Some(row) = self.cells.get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                *cell = ch;
            }
        }
    }

    /// Draws a character only where the cell is still blank, so a box
    /// is never scribbled over by an arrow routed past it.
    fn under(&mut self, x: usize, y: usize, ch: char) {
        if self.cells.get(y).and_then(|r| r.get(x)) == Some(&' ') {
            self.put(x, y, ch);
        }
    }

    /// Draws `ch`, or `joined` when something is already there — two
    /// edges that branch from one box share a cell, and the second must
    /// not erase the first.
    fn join(&mut self, x: usize, y: usize, ch: char, joined: char) {
        let here = self.cells.get(y).and_then(|r| r.get(x)).copied();
        let occupied = matches!(here, Some(c) if c != ' ' && c != '─' && c != '│');
        self.put(x, y, if occupied { joined } else { ch });
    }

    fn text(&mut self, x: usize, y: usize, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i, y, ch);
        }
    }

    /// A box with its label centred, at `x`,`y`.
    fn boxed(&mut self, x: usize, y: usize, label: &str, shape: Shape) {
        let width = box_width(label);
        let (tl, tr, bl, br) = match shape {
            Shape::Rect => ('┌', '┐', '└', '┘'),
            Shape::Round => ('╭', '╮', '╰', '╯'),
        };
        self.put(x, y, tl);
        self.put(x + width - 1, y, tr);
        self.put(x, y + 2, bl);
        self.put(x + width - 1, y + 2, br);
        for i in 1..width - 1 {
            self.put(x + i, y, '─');
            self.put(x + i, y + 2, '─');
        }
        self.put(x, y + 1, '│');
        self.put(x + width - 1, y + 1, '│');
        self.text(x + 2, y + 1, label);
    }

    fn into_lines(self) -> Vec<String> {
        let mut out: Vec<String> = self
            .cells
            .into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_string())
            .collect();
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }
}

/// Nodes grouped by layer, in drawing order.
fn layers(nodes: &[Node], reversed: bool) -> Vec<Vec<usize>> {
    let depth = nodes.iter().map(|n| n.layer).max().unwrap_or(0) + 1;
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for (i, node) in nodes.iter().enumerate() {
        out[node.layer].push(i);
    }
    for layer in &mut out {
        layer.sort_by_key(|i| nodes[*i].order);
    }
    if reversed {
        out.reverse();
    }
    out
}

fn draw_vertical(
    dir: Dir,
    nodes: &mut [Node],
    edges: &[Edge],
    columns: usize,
) -> Option<Vec<String>> {
    let rows = layers(nodes, dir.reversed());

    // Width is set by the widest layer; every layer is then centred in
    // it, which is what makes a chart read as one shape rather than as
    // a left-aligned list of rows.
    let widths: Vec<usize> = rows
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|i| box_width(&nodes[*i].label))
                .sum::<usize>()
                + GAP_H * layer.len().saturating_sub(1)
        })
        .collect();
    // Edge labels sit to the right of the arrow they belong to, so the
    // canvas needs a gutter for them — sized to the widest, or they are
    // parsed and then silently dropped for want of room.
    let gutter = edges
        .iter()
        .filter_map(|e| e.label.as_deref())
        .map(|l| l.chars().count() + 3)
        .max()
        .unwrap_or(0);
    let content = *widths.iter().max()? + 1;
    let width = content + gutter;
    if width > columns {
        return None;
    }
    let height = rows.len() * BOX_H + rows.len().saturating_sub(1) * GAP_V;

    let mut canvas = Canvas::new(width, height);
    let mut tops: Vec<usize> = vec![0; nodes.len()];

    for (depth, layer) in rows.iter().enumerate() {
        let y = depth * (BOX_H + GAP_V);
        let mut x = (content - widths[depth]) / 2;
        for &i in layer {
            let w = box_width(&nodes[i].label);
            canvas.boxed(x, y, &nodes[i].label, nodes[i].shape);
            nodes[i].centre = x + w / 2;
            tops[i] = y;
            x += w + GAP_H;
        }
    }

    for edge in edges {
        if edge.from == edge.to {
            continue;
        }
        let (a, b) = (edge.from, edge.to);
        // Drawn from whichever end is higher on the canvas, so an edge
        // that runs against the layout direction still points somewhere.
        let (top, bottom) = if tops[a] <= tops[b] { (a, b) } else { (b, a) };
        let from_y = tops[top] + BOX_H;
        let to_y = tops[bottom];
        if to_y <= from_y {
            continue;
        }
        let mid = from_y + (to_y - from_y) / 2;
        let (x0, x1) = (nodes[top].centre, nodes[bottom].centre);

        for y in from_y..mid {
            canvas.under(x0, y, '│');
        }
        for y in mid + 1..to_y {
            canvas.under(x1, y, '│');
        }
        if x0 != x1 {
            let (lo, hi) = (x0.min(x1), x0.max(x1));
            for x in lo..=hi {
                canvas.under(x, mid, '─');
            }
            // Two edges leaving the same box meet here, so a corner that
            // lands on one already drawn becomes a tee rather than
            // replacing it and losing the other branch.
            canvas.join(x0, mid, if x1 > x0 { '╰' } else { '╯' }, '┴');
            canvas.join(x1, mid, if x1 > x0 { '╮' } else { '╭' }, '┬');
        } else {
            canvas.under(x0, mid, '│');
        }
        let head = to_y.saturating_sub(1);
        canvas.put(x1, head, '▼');

        if let Some(label) = edge.label.as_deref() {
            // On the arrowhead's row, beside it. On the connector row it
            // was written over the line it belonged to.
            let at = x1 + 2;
            if at + label.chars().count() < width {
                canvas.text(at, head, label);
            }
        }
    }
    Some(canvas.into_lines())
}

fn draw_horizontal(
    dir: Dir,
    nodes: &mut [Node],
    edges: &[Edge],
    columns: usize,
) -> Option<Vec<String>> {
    let cols = layers(nodes, dir.reversed());

    let col_widths: Vec<usize> = cols
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|i| box_width(&nodes[*i].label))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let width: usize = col_widths.iter().sum::<usize>() + GAP_LR * cols.len().saturating_sub(1) + 1;
    if width > columns {
        return None;
    }
    let tallest = cols.iter().map(Vec::len).max()?;
    let height = tallest * (BOX_H + 1);

    let mut canvas = Canvas::new(width, height);
    let mut lefts: Vec<usize> = vec![0; nodes.len()];
    let mut mids: Vec<usize> = vec![0; nodes.len()];
    let mut rights: Vec<usize> = vec![0; nodes.len()];

    let mut x = 0;
    for (depth, layer) in cols.iter().enumerate() {
        // Each column centred against the tallest, for the same reason
        // the vertical layout centres its rows.
        let span = layer.len() * (BOX_H + 1);
        let mut y = (height - span) / 2;
        for &i in layer {
            canvas.boxed(x, y, &nodes[i].label, nodes[i].shape);
            lefts[i] = x;
            rights[i] = x + box_width(&nodes[i].label);
            mids[i] = y + 1;
            y += BOX_H + 1;
        }
        x += col_widths[depth] + GAP_LR;
    }

    for edge in edges {
        if edge.from == edge.to {
            continue;
        }
        let (a, b) = (edge.from, edge.to);
        let (near, far) = if lefts[a] <= lefts[b] { (a, b) } else { (b, a) };
        let from_x = rights[near];
        let to_x = lefts[far];
        if to_x <= from_x {
            continue;
        }
        let mid = from_x + (to_x - from_x) / 2;
        let (y0, y1) = (mids[near], mids[far]);

        for x in from_x..mid {
            canvas.under(x, y0, '─');
        }
        for x in mid + 1..to_x {
            canvas.under(x, y1, '─');
        }
        if y0 != y1 {
            let (lo, hi) = (y0.min(y1), y0.max(y1));
            for y in lo..=hi {
                canvas.under(mid, y, '│');
            }
            canvas.join(mid, y0, if y1 > y0 { '╮' } else { '╯' }, '┤');
            canvas.join(mid, y1, if y1 > y0 { '╰' } else { '╭' }, '├');
        } else {
            canvas.under(mid, y0, '─');
        }
        canvas.put(to_x.saturating_sub(1), y1, '▶');

        if let Some(label) = edge.label.as_deref() {
            let at = mid.saturating_sub(label.chars().count() / 2);
            let row = y0.min(y1).saturating_sub(1);
            canvas.text(at, row, label);
        }
    }
    Some(canvas.into_lines())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(src: &str) -> String {
        render(src, 100).expect("a flowchart").join("\n")
    }

    #[test]
    fn a_chain_of_boxes_is_drawn_top_to_bottom() {
        let out = drawn("flowchart TB\n  A[Start] --> B[Finish]\n");
        assert!(out.contains("Start") && out.contains("Finish"), "{out}");
        assert!(out.contains('▼'), "the edge should point somewhere:\n{out}");
        // Start is above Finish.
        let lines: Vec<&str> = out.lines().collect();
        let start = lines.iter().position(|l| l.contains("Start")).unwrap();
        let finish = lines.iter().position(|l| l.contains("Finish")).unwrap();
        assert!(start < finish, "{out}");
    }

    #[test]
    fn left_to_right_puts_them_side_by_side() {
        let out = drawn("flowchart LR\n  A[One] --> B[Two]\n");
        let row = out
            .lines()
            .find(|l| l.contains("One"))
            .expect("a row with One");
        assert!(row.contains("Two"), "both should share a row:\n{out}");
        assert!(out.contains('▶'), "{out}");
    }

    #[test]
    fn an_edge_label_is_drawn_beside_its_arrow() {
        let out = drawn("flowchart TB\n  A{Ok?} -->|yes| B[Go]\n  A -->|no| C[Stop]\n");
        assert!(out.contains("yes") && out.contains("no"), "{out}");
        assert!(out.contains("Ok?"), "{out}");
    }

    #[test]
    fn the_dash_text_dash_form_is_a_label_too() {
        let out = drawn("flowchart TB\n  A -- retry --> B\n");
        assert!(out.contains("retry"), "{out}");
        assert!(out.contains('A') && out.contains('B'), "{out}");
    }

    #[test]
    fn a_decision_is_drawn_differently_from_a_step() {
        let out = drawn("flowchart TB\n  A[Step] --> B{Choice}\n");
        assert!(out.contains('┌'), "a step is square: {out}");
        assert!(out.contains('╭'), "a decision is rounded: {out}");
    }

    #[test]
    fn a_node_mentioned_before_it_is_labelled_still_gets_its_label() {
        let out = drawn("flowchart TB\n  A --> B\n  B[Named]\n");
        assert!(out.contains("Named"), "{out}");
    }

    #[test]
    fn a_cycle_terminates_and_still_draws() {
        let out = drawn("flowchart TB\n  A --> B\n  B --> C\n  C --> A\n");
        for name in ['A', 'B', 'C'] {
            assert!(out.contains(name), "{out}");
        }
    }

    #[test]
    fn what_it_cannot_draw_it_refuses() {
        // Not a flowchart at all.
        assert!(render("sequenceDiagram\n  Alice->>Bob: hi\n", 100).is_none());
        assert!(render("pie title Pets\n  \"Dogs\" : 1\n", 100).is_none());
        // A flowchart with parts this does not lay out. Drawing it
        // without the subgraph would be a diagram missing something.
        assert!(render("flowchart TB\n  subgraph one\n  A --> B\n  end\n", 100).is_none());
        assert!(render("flowchart TB\n  A --> B\n  style A fill:#f9f\n", 100).is_none());
        // And a chart too wide for the column it has.
        assert!(render("flowchart LR\n  A[aaaaaaaaaaaa] --> B[bbbbbbbbbbbb]\n", 20).is_none());
    }

    #[test]
    fn an_unknown_direction_is_refused_rather_than_guessed() {
        assert!(render("flowchart XY\n  A --> B\n", 100).is_none());
        // But a missing one is top-down, as Mermaid says.
        assert!(render("flowchart\n  A --> B\n", 100).is_some());
    }

    #[test]
    fn a_box_is_never_drawn_over_by_an_arrow() {
        // Three layers, so an edge from the first to the third has to be
        // routed past the middle one.
        let out = drawn("flowchart TB\n  A[Aaa] --> B[Bbb]\n  B --> C[Ccc]\n  A --> C\n");
        for name in ["Aaa", "Bbb", "Ccc"] {
            assert!(out.contains(name), "{name} was drawn over:\n{out}");
        }
    }
}
