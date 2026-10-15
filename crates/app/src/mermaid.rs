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
    /// `A[..]`, a step.
    Rect,
    /// `A(..)` and `A((..))`, a terminus.
    Round,
    /// `A{..}`, a decision. Drawn with sloped corners, which is as
    /// close to a diamond as a character grid gets — and close enough,
    /// because what matters is that it is not the same shape as a step.
    Diamond,
    /// `A[(..)]`, a store. A disc across the top, which is the symbol
    /// everyone already reads as a database.
    Cylinder,
}

#[derive(Clone, Debug)]
struct Node {
    /// The label, already broken where it said to break.
    ///
    /// Mermaid writes a line break inside a label as a literal `\n` or
    /// as `<br>`, which the source carries through verbatim. Collapsing
    /// those to a space made `Node 0 (leader)\ngRPC + Raft` one very
    /// wide box saying something the author did not write.
    lines: Vec<String>,
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
    /// `A <--> B`: a link rather than a direction.
    ///
    /// It says the two are connected, not that one comes before the
    /// other, so it must not push its target down a layer. Letting it
    /// do that is what spread the three peers of a Raft cluster down
    /// three rows of the page and left the subgraph round them with
    /// nothing it could honestly frame.
    both: bool,
}

/// Renders a `mermaid` fence, or `None` when it is not a flowchart this
/// can lay out honestly.
pub fn render(source: &str, columns: usize) -> Option<Vec<String>> {
    let kind = source
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?
        .split_whitespace()
        .next()?;
    if kind == "sequenceDiagram" {
        return sequence::render(source, columns);
    }
    let (dir, mut nodes, edges, groups) = parse(source)?;
    if nodes.is_empty() {
        return None;
    }
    layer(&mut nodes, &edges, &groups);
    let canvas = if dir.vertical() {
        draw_vertical(dir, &mut nodes, &edges, &groups, columns)
    } else {
        draw_horizontal(dir, &mut nodes, &edges, columns)
    }?;
    Some(canvas)
}

/// Reads the subset of the flowchart grammar that a README uses.
#[allow(clippy::type_complexity)]
fn parse(source: &str) -> Option<(Dir, Vec<Node>, Vec<Edge>, Vec<Group>)> {
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
    let mut groups: Vec<Group> = Vec::new();
    let mut open: Vec<usize> = Vec::new();

    for line in lines {
        let line = line.trim_end_matches(';');
        // Colour, class and click are presentation. This draws in box
        // characters on a monochrome grid, so there is no fill to honour
        // and no node to click: dropping them loses nothing that could
        // have been drawn either way, and a chart whose author tinted one
        // box is still the chart they drew. A direction inside a subgraph
        // goes the same way — one direction per chart is what the layout
        // can do.
        //
        // An edge is the other thing entirely. Anything that names one
        // and cannot be read is refused below, because a diagram quietly
        // missing an arrow says something false.
        let first = line.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "style" | "classDef" | "class" | "click" | "linkStyle" | "direction"
        ) {
            continue;
        }
        if first == "subgraph" {
            let rest = line["subgraph".len()..].trim();
            groups.push(Group {
                title: group_title(rest),
                members: Vec::new(),
            });
            open.push(groups.len() - 1);
            continue;
        }
        if first == "end" {
            open.pop();
            continue;
        }

        match split_edge(line) {
            Some((left, label, right, both)) => {
                // `A & B --> C & D` is Mermaid's shorthand for every edge
                // between the two sides. Reading it as one node called
                // "A & B" found no such node and refused the whole chart.
                let mut sources = Vec::new();
                for token in chain(left) {
                    let from = intern(token, &mut index, &mut nodes)?;
                    enrol(&mut groups, &open, from);
                    sources.push(from);
                }
                for token in chain(right) {
                    let to = intern(token, &mut index, &mut nodes)?;
                    enrol(&mut groups, &open, to);
                    for &from in &sources {
                        edges.push(Edge {
                            from,
                            to,
                            label: label.clone(),
                            both,
                        });
                    }
                }
            }
            // A bare declaration, which is how a node gets a label it is
            // not given at its first edge.
            None => {
                let at = intern(line, &mut index, &mut nodes)?;
                enrol(&mut groups, &open, at);
            }
        }
    }
    Some((dir, nodes, edges, groups))
}

/// A `subgraph` and the nodes declared inside it.
#[derive(Clone, Debug)]
struct Group {
    title: String,
    members: Vec<usize>,
}

/// `subgraph cluster [Raft Cluster]` names the group twice: an id and a
/// title. The title is what anyone reads, so it wins.
fn group_title(rest: &str) -> String {
    match rest.split_once('[') {
        Some((_, title)) => title
            .trim_end_matches([']', '"'])
            .trim_matches('"')
            .trim()
            .to_string(),
        None => rest.trim_matches('"').trim().to_string(),
    }
}

/// Records a node as belonging to whichever subgraphs are open.
fn enrol(groups: &mut [Group], open: &[usize], node: usize) {
    for &g in open {
        if !groups[g].members.contains(&node) {
            groups[g].members.push(node);
        }
    }
}

/// The nodes on one side of an edge: `A & B` is two of them.
///
/// Split at the top level only, because a label is free to hold an
/// ampersand of its own — `A[Cut & Paste]` is one node.
fn chain(side: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut start = 0;
    for (at, c) in side.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth = depth.saturating_sub(1),
            '&' if !quoted && depth == 0 => {
                out.push(side[..at][start..].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(side[start..].trim());
    out.retain(|token| !token.is_empty());
    out
}

/// Splits `A -->|label| B` into its three parts.
///
/// Returns `None` when the line holds no edge at all, which the caller
/// reads as a bare node declaration.
fn split_edge(line: &str) -> Option<(&str, Option<String>, &str, bool)> {
    let (at, len) = last_arrow(line)?;
    // `<-->` and `<==>` point both ways, which is a link rather than an
    // order. Read off the arrow itself, before the sides are trimmed.
    let both = line[at..at + len].starts_with('<');
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
    Some((left, label, right, both))
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
            nodes[at].lines = label;
            nodes[at].shape = shape;
        }
        return Some(at);
    }
    let at = nodes.len();
    index.insert(id.clone(), at);
    nodes.push(Node {
        lines: label.unwrap_or_else(|| vec![id]),
        shape,
        layer: 0,
        order: 0,
        centre: 0,
    });
    Some(at)
}

/// Splits `A[Label]` into its id, the rows of its label and its outline.
fn node_parts(token: &str) -> Option<(String, Option<Vec<String>>, Shape)> {
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
    let shape = if inner.starts_with("[(") {
        Shape::Cylinder
    } else if inner.starts_with('{') {
        Shape::Diamond
    } else if inner.starts_with('[') && !inner.starts_with("[[") {
        Shape::Rect
    } else {
        Shape::Round
    };
    let label = inner
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '>' | '"'))
        .trim()
        .replace('"', "");
    // A label breaks where Mermaid says it breaks. The source carries
    // `\n` as two characters and `<br>` as itself, so both are found
    // here rather than in whatever wrote the file.
    let rows: Vec<String> = label
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
        .replace("\\n", "\n")
        .split('\n')
        .map(|row| row.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|row| !row.is_empty())
        .collect();
    Some((id, (!rows.is_empty()).then_some(rows), shape))
}

/// Puts each node one layer below its deepest predecessor.
///
/// Relaxed a bounded number of times rather than sorted topologically,
/// because a flowchart with a loop in it is still a flowchart someone
/// wants to look at — the cap is what keeps a cycle from running
/// forever, and the layering it settles on is good enough to draw.
fn layer(nodes: &mut [Node], edges: &[Edge], groups: &[Group]) {
    for _ in 0..nodes.len() {
        let mut moved = false;
        for edge in edges {
            // A two-headed arrow says the ends are connected, not that
            // one comes before the other, so it ranks neither.
            if edge.from == edge.to || edge.both {
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

    // A node in a subgraph with nothing pointing at it has no layer of
    // its own, so it sits at the top while its peers are further down
    // and the subgraph cannot be framed without enclosing strangers.
    // It joins its peers instead — a subgraph is drawn as one region,
    // which is the whole of what declaring one asks for.
    let ranked: Vec<bool> = {
        let mut has = vec![false; nodes.len()];
        for edge in edges {
            if edge.from != edge.to && !edge.both {
                has[edge.to] = true;
            }
        }
        has
    };
    for group in groups {
        let settled = group
            .members
            .iter()
            .filter(|i| ranked[**i])
            .map(|i| nodes[*i].layer)
            .max();
        let Some(settled) = settled else { continue };
        for &member in &group.members {
            if !ranked[member] {
                nodes[member].layer = settled;
            }
        }
    }

    // Pulling a node down can leave an edge from it pointing upwards,
    // so what follows it comes down too. Bounded, like the relaxation
    // above, and for the same reason.
    for _ in 0..nodes.len() {
        let mut moved = false;
        for edge in edges {
            if edge.from == edge.to || edge.both {
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
fn box_width(node: &Node) -> usize {
    node.lines
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0)
        + 4
}

/// Rows a box takes: one per line of its label, and its two borders.
///
/// A store has one more, for the disc across its top.
fn box_height(node: &Node) -> usize {
    node.lines.len().max(1) + 2 + usize::from(node.shape == Shape::Cylinder)
}

/// Rows the tallest box in a layer takes, which is the layer's height.
fn layer_height(nodes: &[Node], layer: &[usize]) -> usize {
    layer
        .iter()
        .map(|i| box_height(&nodes[*i]))
        .max()
        .unwrap_or(BOX_H)
}

/// A box with a one-line label, which is most of them.
const BOX_H: usize = 3;
/// Rows a subgraph frame keeps between its border and the boxes in it.
///
/// Two: one for the border itself, and one for whatever an edge is
/// doing on its way in or out.
const FRAME_PAD: usize = 2;
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
    /// Cells a box owns. Nothing else may write to them.
    ///
    /// Without this an arrow routed past a box wrote its corners and
    /// arrowheads straight through the label — `Node 0 (leader)` came
    /// out as `N┴de╮0 (leader)`. Checking for a blank cell is not
    /// enough, because a box's own characters are not blank.
    locked: Vec<Vec<bool>>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![' '; width]; height],
            locked: vec![vec![false; width]; height],
        }
    }

    /// Writes regardless of who owns the cell. Only box drawing may.
    fn set(&mut self, x: usize, y: usize, ch: char) {
        if let Some(row) = self.cells.get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                *cell = ch;
            }
        }
        if let Some(row) = self.locked.get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                *cell = true;
            }
        }
    }

    fn owned(&self, x: usize, y: usize) -> bool {
        self.locked
            .get(y)
            .and_then(|r| r.get(x))
            .copied()
            .unwrap_or(false)
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if self.owned(x, y) {
            return;
        }
        if let Some(row) = self.cells.get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                *cell = ch;
            }
        }
    }

    /// Draws a character only where the cell is still blank, so a box
    /// is never scribbled over by an arrow routed past it.
    fn under(&mut self, x: usize, y: usize, ch: char) {
        if !self.owned(x, y) && self.cells.get(y).and_then(|r| r.get(x)) == Some(&' ') {
            self.put(x, y, ch);
        }
    }

    /// Draws `ch`, or `joined` when something is already there — two
    /// edges that branch from one box share a cell, and the second must
    /// not erase the first.
    fn join(&mut self, x: usize, y: usize, ch: char, joined: char) {
        if self.owned(x, y) {
            return;
        }
        let here = self.cells.get(y).and_then(|r| r.get(x)).copied();
        let occupied = matches!(here, Some(c) if c != ' ' && c != '─' && c != '│');
        self.put(x, y, if occupied { joined } else { ch });
    }

    fn text(&mut self, x: usize, y: usize, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i, y, ch);
        }
    }

    /// Text that gives way to anything already there, so a label never
    /// lands on a box or a line it does not own.
    fn label(&mut self, x: usize, y: usize, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            self.under(x + i, y, ch);
        }
    }

    /// A box with its label inside, at `x`,`y`.
    ///
    /// As many rows as the label has lines, each centred in the box:
    /// a two-line label in a left-aligned box reads as one line that
    /// ran out of room.
    fn boxed(&mut self, x: usize, y: usize, node: &Node) {
        let width = box_width(node);
        let height = box_height(node);
        let (tl, tr, bl, br) = match node.shape {
            Shape::Rect => ('┌', '┐', '└', '┘'),
            Shape::Round | Shape::Cylinder => ('╭', '╮', '╰', '╯'),
            Shape::Diamond => ('╱', '╲', '╲', '╱'),
        };
        for row in 0..height {
            for i in 0..width {
                self.set(x + i, y + row, ' ');
            }
        }
        let last = height - 1;
        self.set(x, y, tl);
        self.set(x + width - 1, y, tr);
        self.set(x, y + last, bl);
        self.set(x + width - 1, y + last, br);
        for i in 1..width - 1 {
            self.set(x + i, y, '─');
            self.set(x + i, y + last, '─');
        }
        for row in 1..last {
            self.set(x, y + row, '│');
            self.set(x + width - 1, y + row, '│');
        }
        // A store's disc: the line that makes a rounded box read as a
        // cylinder rather than as a terminus.
        let lid = usize::from(node.shape == Shape::Cylinder);
        if lid == 1 {
            self.set(x, y + 1, '├');
            self.set(x + width - 1, y + 1, '┤');
            for i in 1..width - 1 {
                self.set(x + i, y + 1, '─');
            }
        }
        for (row, text) in node.lines.iter().enumerate() {
            let inset = (width - text.chars().count()) / 2;
            for (i, ch) in text.chars().enumerate() {
                self.set(x + inset + i, y + 1 + lid + row, ch);
            }
        }
    }

    /// A light frame with a title on its top edge, drawn only where the
    /// cells are free.
    fn frame(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, title: &str) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for x in x0 + 1..x1 {
            self.under(x, y0, '╌');
            self.under(x, y1, '╌');
        }
        for y in y0 + 1..y1 {
            self.under(x0, y, '╎');
            self.under(x1, y, '╎');
        }
        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            self.under(x, y, '┼');
        }
        if !title.is_empty() {
            let at = x0 + 2;
            if at + title.chars().count() < x1 {
                self.text(at, y0, title);
            }
        }
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
    groups: &[Group],
    columns: usize,
) -> Option<Vec<String>> {
    let rows = layers(nodes, dir.reversed());

    // Width is set by the widest layer; every layer is then centred in
    // it, which is what makes a chart read as one shape rather than as
    // a left-aligned list of rows.
    let widths: Vec<usize> = rows
        .iter()
        .map(|layer| {
            layer.iter().map(|i| box_width(&nodes[*i])).sum::<usize>()
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
    // A subgraph's title is written on its top border, so a group
    // whose boxes are narrower than its name still needs the room:
    // without this the title did not fit and was dropped, and the
    // subgraph was drawn as an unlabelled box.
    let titles = groups
        .iter()
        .filter(|g| !g.members.is_empty())
        .map(|g| g.title.chars().count() + 6)
        .max()
        .unwrap_or(0);
    let content = (*widths.iter().max()?).max(titles) + 1;
    let width = content + gutter;
    if width > columns {
        return None;
    }
    // A frame needs rows of its own, above and below the boxes it
    // holds, or an arrowhead arriving at the layer inside it lands on
    // the border and eats the title: `Raft Cluster` came out as
    // `Raft Cl▼ster`. Three more rows between layers buys them, and
    // the room is only taken when there is a subgraph to spend it on.
    let framed = !groups.is_empty();
    let headroom = if framed { FRAME_PAD } else { 0 };
    let gap = if framed { GAP_V + 3 } else { GAP_V };
    // Layers are no longer all one height — a box is as tall as its
    // label has lines — so the row each one starts on is the sum of
    // the ones above it rather than its depth times a constant.
    let heights: Vec<usize> = rows.iter().map(|l| layer_height(nodes, l)).collect();
    let mut starts: Vec<usize> = Vec::with_capacity(rows.len());
    let mut at = headroom;
    for height in &heights {
        starts.push(at);
        at += height + gap;
    }
    let height = at.saturating_sub(gap) + headroom;
    // The order layers were drawn in, so a group frame can find the row
    // a layer landed on even when the direction reversed them.
    let rows_order: Vec<usize> = rows
        .iter()
        .map(|layer| layer.first().map_or(0, |i| nodes[*i].layer))
        .collect();

    let mut canvas = Canvas::new(width + 4, height);
    let mut tops: Vec<usize> = vec![0; nodes.len()];
    let mut bottoms: Vec<usize> = vec![0; nodes.len()];

    for (depth, layer) in rows.iter().enumerate() {
        let y = starts[depth];
        let mut x = (content - widths[depth]) / 2;
        for &i in layer {
            let w = box_width(&nodes[i]);
            canvas.boxed(x, y, &nodes[i]);
            nodes[i].centre = x + w / 2;
            tops[i] = y;
            bottoms[i] = y + box_height(&nodes[i]);
            x += w + GAP_H;
        }
    }

    // A subgraph is drawn as a frame round the boxes it holds, once they
    // have been placed. Flattening the group into the layout and framing
    // it afterwards is what lets the layering stay one simple pass; the
    // frame is skipped when the nodes did not land in a clean rectangle,
    // because a frame that encloses something it does not own says
    // something false.
    for group in groups {
        if group.members.is_empty() {
            continue;
        }
        let rows: Vec<usize> = group.members.iter().map(|i| nodes[*i].layer).collect();
        let (first, last) = (*rows.iter().min()?, *rows.iter().max()?);
        let foreign = nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.layer >= first && n.layer <= last && !group.members.contains(&i));
        if foreign {
            continue;
        }
        let left = group
            .members
            .iter()
            .map(|i| nodes[*i].centre - box_width(&nodes[*i]) / 2)
            .min()?;
        let right = group
            .members
            .iter()
            .map(|i| nodes[*i].centre + box_width(&nodes[*i]) / 2)
            .max()?;
        let depth = |layer: usize| rows_order.iter().position(|l| *l == layer).unwrap_or(layer);
        let (first_depth, last_depth) = (depth(first), depth(last));
        let top = starts.get(first_depth).copied().unwrap_or(headroom);
        let bottom = starts.get(last_depth).copied().unwrap_or(headroom)
            + heights.get(last_depth).copied().unwrap_or(BOX_H);
        // Below the row a link between two members is routed on, so
        // such a link is drawn inside the subgraph it belongs to. Wide
        // enough for the title, which is written along the top border
        // and is otherwise dropped for want of room.
        let x0 = left.saturating_sub(2);
        let x1 = (right + 2).max(x0 + group.title.chars().count() + 4);
        canvas.frame(
            x0,
            top.saturating_sub(FRAME_PAD),
            x1.min(content),
            bottom + FRAME_PAD,
            &group.title,
        );
    }

    // Edges whose ends landed on one row, which a top-to-bottom route
    // cannot express. Collected here and drawn below the row they
    // share, once everything on it is placed.
    let mut same_layer: Vec<&Edge> = Vec::new();
    // Labels already placed in the gutter, so two do not land on each
    // other and one of them come out half-written.
    let mut taken: Vec<(usize, String)> = Vec::new();

    for edge in edges {
        if edge.from == edge.to {
            continue;
        }
        let (a, b) = (edge.from, edge.to);
        // Drawn from whichever end is higher on the canvas, so an edge
        // that runs against the layout direction still points somewhere.
        let (top, bottom) = if tops[a] <= tops[b] { (a, b) } else { (b, a) };
        let from_y = bottoms[top];
        let to_y = tops[bottom];
        if to_y <= from_y {
            same_layer.push(edge);
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
                canvas.label(at, head, label);
            }
        }
    }
    // Edges between boxes on the same row, routed under it: down out
    // of one, across, and up into the other. Drawn last so they give
    // way to everything already placed.
    for edge in same_layer {
        let (a, b) = (edge.from, edge.to);
        if nodes[a].centre == nodes[b].centre {
            continue;
        }
        let base = bottoms[a].max(bottoms[b]);
        let run = base + 1;
        if run >= height {
            continue;
        }
        let (lo, hi) = (
            nodes[a].centre.min(nodes[b].centre),
            nodes[a].centre.max(nodes[b].centre),
        );
        for x in lo..=hi {
            canvas.under(x, run, '─');
        }
        canvas.join(lo, run, '╰', '┴');
        canvas.join(hi, run, '╯', '┴');
        // Pointing up into the box it arrives at, since it comes from
        // below. A two-headed arrow arrives at both.
        canvas.put(nodes[b].centre, base, '▲');
        if edge.both {
            canvas.put(nodes[a].centre, base, '▲');
        } else {
            canvas.under(nodes[a].centre, base, '│');
        }
        if let Some(label) = edge.label.as_deref() {
            // In the gutter, not beside the arrow: a same-row link runs
            // across the whole width between its ends, so there is no
            // clear cell next to it — the label landed on the line and
            // came out as `Raft─consensus────`.
            //
            // Three links between the same three peers usually carry
            // the same word, so an identical label on a row it is
            // already on is the one already there; a different one
            // steps down until it finds a row of its own.
            let here = |taken: &[(usize, String)], row: usize| {
                taken.iter().any(|(r, l)| *r == row && l == label)
            };
            let used = |taken: &[(usize, String)], row: usize| taken.iter().any(|(r, _)| *r == row);
            let mut row = run;
            while !here(&taken, row) && used(&taken, row) && row + 1 < height {
                row += 1;
            }
            if here(&taken, row) {
                continue;
            }
            taken.push((row, label.to_string()));
            canvas.label(content + 1, row, label);
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
                .map(|i| box_width(&nodes[*i]))
                .max()
                .unwrap_or(0)
        })
        .collect();
    // Edge labels are written in the space between two columns, so
    // that space has to be wide enough for them. Without this a label
    // was centred on the connector it belonged to and cut in half by
    // it: `no` came out as `n`.
    let lane = edges
        .iter()
        .filter_map(|e| e.label.as_deref())
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let gap_lr = GAP_LR + if lane > 0 { lane + 1 } else { 0 };
    let width: usize = col_widths.iter().sum::<usize>() + gap_lr * cols.len().saturating_sub(1) + 1;
    if width > columns {
        return None;
    }
    // Boxes are not all one height — a label can have more than one
    // line, and a store has a disc across its top — so a column is as
    // tall as the boxes in it rather than as its count times a
    // constant.
    let spans: Vec<usize> = cols
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|i| box_height(&nodes[*i]) + 1)
                .sum::<usize>()
        })
        .collect();
    let height = *spans.iter().max()?;

    let mut canvas = Canvas::new(width, height);
    let mut lefts: Vec<usize> = vec![0; nodes.len()];
    let mut mids: Vec<usize> = vec![0; nodes.len()];
    let mut rights: Vec<usize> = vec![0; nodes.len()];

    let mut x = 0;
    for (depth, layer) in cols.iter().enumerate() {
        // Each column centred against the tallest, for the same reason
        // the vertical layout centres its rows.
        let mut y = (height - spans[depth]) / 2;
        for &i in layer {
            let tall = box_height(&nodes[i]);
            canvas.boxed(x, y, &nodes[i]);
            lefts[i] = x;
            rights[i] = x + box_width(&nodes[i]);
            // The middle of the box, so an arrow arrives beside the
            // label rather than on a border or on a store's disc.
            mids[i] = y + tall / 2;
            y += tall + 1;
        }
        x += col_widths[depth] + gap_lr;
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
            // Beside the arrowhead and one row above it: clear of the
            // connector, which cut a centred label in half, and on a
            // row of its own, which two edges leaving the same box
            // were not — both labels landed on one row and the second
            // was written over the first.
            let at = to_x.saturating_sub(1 + label.chars().count());
            canvas.label(at, y1.saturating_sub(1), label);
        }
    }
    Some(canvas.into_lines())
}

/// Sequence diagrams: participants across the top, time down.
///
/// A different drawing entirely from a flowchart, and an easier one —
/// the layout is a column per participant and a row per message, with
/// no layering to work out. It is here because `sequenceDiagram` is the
/// other half of what a project's documentation actually contains.
mod sequence {
    use super::Canvas;

    /// How a message's line is drawn.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Line {
        Solid,
        /// A reply, which Mermaid writes `-->>` and draws dashed.
        Dashed,
    }

    struct Message {
        from: usize,
        to: usize,
        text: String,
        line: Line,
    }

    /// Cells of air either side of a participant's name in its box.
    const PAD: usize = 2;
    /// Cells between one participant's box and the next.
    const GAP: usize = 3;

    pub fn render(source: &str, columns: usize) -> Option<Vec<String>> {
        let (names, messages) = parse(source)?;
        if names.is_empty() || messages.is_empty() {
            return None;
        }

        let widths: Vec<usize> = names.iter().map(|n| n.chars().count() + PAD * 2).collect();
        let columns_wide: usize = widths.iter().sum::<usize>() + GAP * (names.len() - 1) + 1;

        // Lifelines first, because how much room a message's text needs
        // depends on where its own lifeline is.
        let mut lifelines = Vec::with_capacity(names.len());
        let mut x = 0;
        for width in &widths {
            lifelines.push(x + width / 2);
            x += width + GAP;
        }

        // The canvas is widened to fit the longest message rather than
        // the text being cut at the next lifeline. A message clipped to
        // `UNAVAILABLE + l` has lost the part that mattered.
        let needed = messages
            .iter()
            .map(|m| {
                let start = if m.from == m.to {
                    lifelines[m.from] + 4
                } else {
                    lifelines[m.from].min(lifelines[m.to]) + 2
                };
                start + m.text.chars().count() + 1
            })
            .max()
            .unwrap_or(0);
        let width = columns_wide.max(needed);
        if width > columns {
            return None;
        }

        // Three rows per message: its text, its arrow, and air.
        let head = 3;
        let height = head + messages.len() * 3 + 1;
        let mut canvas = Canvas::new(width, height);

        let mut x = 0;
        for (i, name) in names.iter().enumerate() {
            canvas.boxed(
                x,
                0,
                &super::Node {
                    lines: vec![name.clone()],
                    shape: super::Shape::Rect,
                    layer: 0,
                    order: 0,
                    centre: 0,
                },
            );
            x += widths[i] + GAP;
        }

        for (i, message) in messages.iter().enumerate() {
            let row = head + i * 3;
            let (a, b) = (lifelines[message.from], lifelines[message.to]);
            let arrow = row + 1;

            // The text goes down before the lifelines, which are then
            // drawn only where it is not. The other order put a `│`
            // through every message that reached past one lifeline:
            // `Set(key, value│`.
            let (text_at, text_row) = if message.from == message.to {
                (a + 4, arrow)
            } else {
                (a.min(b) + 2, row)
            };
            canvas.text(text_at, text_row, &message.text);

            // Lifelines run the whole height, under everything, so a
            // message crossing one stays readable.
            for line in &lifelines {
                for y in row..row + 3 {
                    canvas.under(*line, y, '│');
                }
            }

            if message.from == message.to {
                // A message to itself turns back on its own lifeline.
                canvas.put(a + 1, row, '─');
                canvas.put(a + 2, row, '╮');
                canvas.put(a + 2, arrow, '│');
                canvas.put(a + 2, arrow + 1, '╯');
                canvas.put(a + 1, arrow + 1, '─');
                canvas.put(a, arrow + 1, '◀');
                continue;
            }

            let (lo, hi) = (a.min(b), a.max(b));
            for x in lo + 1..hi {
                canvas.put(
                    x,
                    arrow,
                    match message.line {
                        Line::Solid => '─',
                        // A hyphen rather than a box-drawing dash: it is
                        // in every font, and a reply that renders as a
                        // missing glyph is worse than one that is plain.
                        Line::Dashed => '-',
                    },
                );
            }
            // The tail is on the sender's side whichever way the
            // message runs; `├` on the right end of a leftward arrow
            // pointed the wrong way.
            canvas.put(a, arrow, if b > a { '├' } else { '┤' });
            canvas.put(b, arrow, if b > a { '▶' } else { '◀' });
        }
        Some(canvas.into_lines())
    }

    /// Reads participants and messages, and refuses the rest.
    ///
    /// `loop`, `alt`, `opt` and `par` are control flow drawn as nested
    /// frames; without them the messages inside would be shown as though
    /// they always happen, which is a different diagram.
    fn parse(source: &str) -> Option<(Vec<String>, Vec<Message>)> {
        let mut names: Vec<String> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut messages: Vec<Message> = Vec::new();

        let id_of = |ids: &mut Vec<String>, names: &mut Vec<String>, token: &str| {
            let token = token.trim();
            match ids.iter().position(|i| i == token) {
                Some(at) => Some(at),
                None if token.is_empty() => None,
                None => {
                    ids.push(token.to_string());
                    names.push(token.to_string());
                    Some(ids.len() - 1)
                }
            }
        };

        for line in source.lines().map(str::trim) {
            if line.is_empty() || line.starts_with("%%") || line == "sequenceDiagram" {
                continue;
            }
            let first = line.split_whitespace().next().unwrap_or("");
            if matches!(
                first,
                "loop"
                    | "alt"
                    | "else"
                    | "opt"
                    | "par"
                    | "and"
                    | "end"
                    | "rect"
                    | "critical"
                    | "break"
                    | "activate"
                    | "deactivate"
                    | "autonumber"
            ) {
                return None;
            }
            if matches!(first, "Note" | "note") {
                continue;
            }

            if let Some(rest) = line
                .strip_prefix("participant ")
                .or_else(|| line.strip_prefix("actor "))
            {
                let (id, shown) = match rest.split_once(" as ") {
                    Some((id, shown)) => (id.trim(), shown.trim()),
                    None => (rest.trim(), rest.trim()),
                };
                if ids.iter().all(|i| i != id) {
                    ids.push(id.to_string());
                    names.push(shown.to_string());
                }
                continue;
            }

            // `A->>B: text`. Longest arrow first, so `->>` is not read
            // as `->` with a stray `>` on the target.
            const ARROWS: &[(&str, Line)] = &[
                ("-->>", Line::Dashed),
                ("--)", Line::Dashed),
                ("-->", Line::Dashed),
                ("->>", Line::Solid),
                ("-)", Line::Solid),
                ("->", Line::Solid),
            ];
            let found = ARROWS
                .iter()
                .find_map(|(arrow, line_kind)| line.find(arrow).map(|at| (at, *arrow, *line_kind)));
            let Some((at, arrow, line_kind)) = found else {
                // Not a message and not a participant: something this
                // does not read, so the whole diagram is refused.
                return None;
            };
            let from = &line[..at];
            let rest = &line[at + arrow.len()..];
            let (to, text) = rest.split_once(':').unwrap_or((rest, ""));
            let from = id_of(&mut ids, &mut names, from)?;
            let to = id_of(&mut ids, &mut names, to)?;
            messages.push(Message {
                from,
                to,
                text: text.trim().to_string(),
                line: line_kind,
            });
        }
        Some((names, messages))
    }
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
    fn every_shape_is_drawn_differently_from_the_others() {
        // A flowchart says what a thing is by the shape it is drawn
        // as, so a decision that looks like a step has thrown the
        // information away.
        let out =
            drawn("flowchart TB\n  A[Step] --> B{Choice}\n  B --> C(Done)\n  B --> D[(Store)]\n");
        assert!(out.contains('┌'), "a step is square: {out}");
        assert!(out.contains('╱'), "a decision is sloped: {out}");
        assert!(out.contains('╭'), "a terminus is rounded: {out}");
        assert!(
            out.contains('├') && out.contains('┤'),
            "a store has a disc across its top: {out}"
        );
    }

    #[test]
    fn a_store_is_taller_than_the_box_beside_it() {
        // The disc is a row of its own, so the layout has to give it
        // one — a box drawn taller than the row it was placed in
        // writes through whatever is under it.
        let out = drawn("flowchart LR\n  A[Step] --> B[(Store)]\n");
        for row in out.lines() {
            assert!(
                !row.contains("Step") || !row.contains("Store"),
                "the two sit on different rows of their boxes: {out}"
            );
        }
        assert!(out.contains("Step") && out.contains("Store"), "{out}");
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
    fn a_subgraph_groups_its_nodes_inside_a_frame() {
        let out = drawn(
            "flowchart TB\n  subgraph clients [Clients]\n    A[One]\n    B[Two]\n  end\n  A --> C[Out]\n",
        );
        assert!(out.contains("Clients"), "the group is named: {out}");
        assert!(out.contains('╎'), "and framed: {out}");
        for name in ["One", "Two", "Out"] {
            assert!(out.contains(name), "{name} missing:\n{out}");
        }
    }

    #[test]
    fn a_frame_is_dropped_rather_than_drawn_round_the_wrong_nodes() {
        // `B` is not in the group but shares its layer, so a frame round
        // the group's bounding box would enclose it and say something
        // false. The nodes are still drawn.
        let out =
            drawn("flowchart TB\n  subgraph g [G]\n    A[Aaa]\n  end\n  B[Bbb]\n  A --> C[Ccc]\n");
        assert!(!out.contains('╎'), "no frame should be drawn:\n{out}");
        for name in ["Aaa", "Bbb", "Ccc"] {
            assert!(out.contains(name), "{name} missing:\n{out}");
        }
    }

    #[test]
    fn a_quoted_label_breaks_where_it_says_to_break() {
        let out = drawn("flowchart TB\n  A[\"Node 0 (leader)\\ngRPC\"] --> B[Next]\n");
        assert!(!out.contains('\\'), "the escape should not survive:\n{out}");
        assert!(!out.contains('"'), "nor the quotes:\n{out}");
        // Two rows of one box, not one row saying something the author
        // did not write.
        let rows: Vec<&str> = out.lines().collect();
        let first = rows
            .iter()
            .position(|r| r.contains("Node 0 (leader)"))
            .expect("the first line of the label");
        assert!(
            rows[first + 1].contains("gRPC"),
            "the second line belongs under the first:\n{out}"
        );
        assert!(!rows[first].contains("gRPC"), "and not beside it:\n{out}");
    }

    #[test]
    fn a_br_tag_breaks_a_label_too() {
        let out = drawn("flowchart TB\n  A[One<br/>Two] --> B[Next]\n");
        let rows: Vec<&str> = out.lines().collect();
        let first = rows
            .iter()
            .position(|r| r.contains("One"))
            .expect("the first line");
        assert!(rows[first + 1].contains("Two"), "{out}");
        assert!(!out.contains("<br"), "the tag itself is not text:\n{out}");
    }

    #[test]
    fn a_two_headed_arrow_leaves_its_ends_side_by_side() {
        // `<-->` says two nodes are connected, not that one comes
        // before the other. Ranking it put the peers of a cluster on
        // three separate rows.
        let out = drawn("flowchart TB\n  A[Aaa] <--> B[Bbb]\n");
        let rows: Vec<&str> = out.lines().collect();
        let a = rows.iter().position(|r| r.contains("Aaa")).expect("Aaa");
        let b = rows.iter().position(|r| r.contains("Bbb")).expect("Bbb");
        assert_eq!(a, b, "both on one row:\n{out}");
    }

    #[test]
    fn a_subgraph_keeps_its_members_together() {
        // `C` is only reachable by a two-headed arrow, so nothing ranks
        // it and it would sit at the top on its own — leaving the
        // subgraph round it unable to frame anything honestly.
        let source = "flowchart TB\n               subgraph g [Group]\n    A[Aaa]\n    C[Ccc]\n  end\n               Z[Zzz] --> A\n  A <--> C\n";
        let out = drawn(source);
        let rows: Vec<&str> = out.lines().collect();
        let a = rows.iter().position(|r| r.contains("Aaa")).expect("Aaa");
        let c = rows.iter().position(|r| r.contains("Ccc")).expect("Ccc");
        assert_eq!(a, c, "members share a row:\n{out}");
        assert!(out.contains("Group"), "and the frame is drawn:\n{out}");
        assert!(out.contains('╎'), "with sides:\n{out}");
    }

    #[test]
    fn an_arrow_into_a_subgraph_does_not_land_on_its_title() {
        // The arrowhead used to arrive on the frame's top border:
        // `Raft Cluster` came out as `Raft Cl▼ster`.
        let source = "flowchart TB\n               Z[Zzz] --> A\n  subgraph g [Raft Cluster]\n    A[Aaa]\n  end\n";
        let out = drawn(source);
        assert!(out.contains("Raft Cluster"), "the title survives:\n{out}");
        assert!(out.contains('▼'), "and the arrow still arrives:\n{out}");
    }

    #[test]
    fn a_label_never_lands_on_top_of_a_box() {
        // An arrow routed past a box used to write its corners through
        // the label: `Node 0 (leader)` came out as `N┴de╮0 (leader)`.
        let out = drawn(
            "flowchart TB\n  A[Aaaaaaaaaa] --> B[Bbbbbbbbbb]\n  B --> C[Cccccccccc]\n  A --> C\n",
        );
        for name in ["Aaaaaaaaaa", "Bbbbbbbbbb", "Cccccccccc"] {
            assert!(out.contains(name), "{name} was written through:\n{out}");
        }
    }

    #[test]
    fn a_sequence_diagram_draws_lifelines_and_messages() {
        let out = drawn(
            "sequenceDiagram\n  participant C as Client\n  participant S as Server\n\
             \n  C->>S: Set(key)\n  S-->>C: ok\n",
        );
        assert!(out.contains("Client") && out.contains("Server"), "{out}");
        assert!(out.contains("Set(key)") && out.contains("ok"), "{out}");
        assert!(
            out.contains('▶') && out.contains('◀'),
            "both directions: {out}"
        );
        // A reply is dashed, a call is not.
        assert!(out.contains('-'), "{out}");
    }

    #[test]
    fn a_message_is_not_cut_off_at_the_next_lifeline() {
        // The canvas widens to the longest message instead:
        // `UNAVAILABLE + l` had lost the part that mattered.
        let long = "UNAVAILABLE plus a leader-address header";
        let out = drawn(&format!(
            "sequenceDiagram\n  participant A\n  participant B\n  A->>B: {long}\n"
        ));
        assert!(out.contains(long), "{out}");
    }

    #[test]
    fn a_message_to_itself_turns_back_on_its_own_lifeline() {
        let out = drawn("sequenceDiagram\n  participant R\n  R->>R: replicate\n");
        assert!(out.contains("replicate"), "{out}");
        assert!(out.contains('╮') && out.contains('╯'), "{out}");
    }

    #[test]
    fn participants_are_named_by_their_alias_and_in_order() {
        let out = drawn(
            "sequenceDiagram\n  participant L as Leader\n  participant F as Follower\n\
             \n  F->>L: hello\n",
        );
        let header = out.lines().nth(1).expect("the name row");
        let leader = header.find("Leader").expect("Leader");
        let follower = header.find("Follower").expect("Follower");
        assert!(leader < follower, "declared order wins: {header:?}");
    }

    #[test]
    fn what_it_cannot_draw_it_refuses() {
        // Neither of the two diagrams this draws.
        assert!(render("pie title Pets\n  \"Dogs\" : 1\n", 100).is_none());
        assert!(render(
            "gantt\n  title A\n  section S\n  Task :a1, 2024-01-01, 30d\n",
            100
        )
        .is_none());
        assert!(render("erDiagram\n  A ||--o{ B : has\n", 100).is_none());
        // Control flow in a sequence diagram is drawn as nested frames;
        // without them the messages inside would read as though they
        // always happen, which is a different diagram.
        assert!(render(
            "sequenceDiagram\n  loop every minute\n  A->>B: hi\n  end\n",
            100
        )
        .is_none());
        // And a chart too wide for the column it has.
        assert!(render("flowchart LR\n  A[aaaaaaaaaaaa] --> B[bbbbbbbbbbbb]\n", 20).is_none());
    }

    #[test]
    fn a_tinted_box_is_still_a_box() {
        // Styling is dropped rather than refused: there is no colour on
        // this grid to honour it with, and refusing threw away the
        // structure as well — which is most of what made real charts
        // in a real README come out as their own source.
        let out = drawn(
            "flowchart TD\n  A[Client] --> B[Server]\n  B --> C[(Store)]\n\
             \n  style A fill:#f9f,stroke:#333\n  classDef hot fill:#f00\n\
             \n  class B hot\n  click A \"https://example.com\"\n\
             \n  linkStyle 0 stroke:#0f0\n",
        );
        for name in ["Client", "Server", "Store"] {
            assert!(
                out.contains(name),
                "{name} was dropped with the styling:\n{out}"
            );
        }
        assert!(out.contains('▼'), "the edges survived too:\n{out}");
    }

    #[test]
    fn an_ampersand_is_every_edge_between_the_two_sides() {
        let out = drawn("flowchart TD\n  A & B --> C\n");
        for name in ["A", "B", "C"] {
            assert!(out.contains(name), "{name} is a node:\n{out}");
        }
        // Two boxes on the top layer and one underneath, which is what
        // two edges into one node looks like.
        let rows: Vec<&str> = out.lines().collect();
        let top = rows
            .iter()
            .position(|r| r.contains('A'))
            .expect("the first layer");
        assert!(rows[top].contains('B'), "A and B share a layer:\n{out}");
        assert!(
            rows.iter().skip(top).any(|r| r.contains('C')),
            "C is below them:\n{out}"
        );
    }

    #[test]
    fn an_ampersand_inside_a_label_is_not_a_chain() {
        let out = drawn("flowchart LR\n  A[Cut & Paste] --> B[Done]\n");
        assert!(out.contains("Cut & Paste"), "one node, one label:\n{out}");
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
