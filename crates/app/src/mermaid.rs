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

/// A diagram, as shapes to rasterise and labels to typeset.
///
/// The text is deliberately *not* in the SVG. Putting it there would
/// mean handing a font database to the rasteriser, which means scanning
/// every font on the machine at startup for a picture that is mostly
/// lines — and the labels would then be set by a different engine from
/// the prose around them, in a different face, at a different weight.
/// Lithe draws them itself, with the same atlas as everything else.
#[derive(Clone, Debug)]
pub struct Diagram {
    /// Shapes only: boxes, edges, arrowheads, subgraph frames.
    pub svg: String,
    pub labels: Vec<Label>,
    /// Size on the character grid, so scrolling and hit-testing can
    /// count rows without rasterising anything.
    pub cols: usize,
    pub rows: usize,
}

/// One run of text in a diagram, positioned in grid cells.
#[derive(Clone, Debug)]
pub struct Label {
    /// Cells from the left, and rows from the top. `x` is the centre
    /// of the text for a centred label and its left edge otherwise.
    pub x: f32,
    pub row: f32,
    pub text: String,
    pub centred: bool,
    /// Drawn quieter than a node's own name: a subgraph's title and an
    /// edge's label are annotations on the picture rather than things
    /// in it.
    pub faint: bool,
}

/// Builds the shape half of a diagram.
///
/// Coordinates are in cells across and rows down, scaled to square
/// units on the way out so the picture can be scaled uniformly without
/// distorting a corner radius or an arrowhead.
struct Svg {
    body: String,
    aspect: f32,
}

impl Svg {
    /// Stroke width, in cell widths. Thin enough to read as a hairline
    /// at any size the preview will show it at.
    const STROKE: f32 = 0.09;

    fn new(aspect: f32) -> Self {
        Self {
            body: String::new(),
            aspect,
        }
    }

    /// A row index to a y coordinate in square units.
    fn y(&self, row: f32) -> f32 {
        row * self.aspect
    }

    fn rect(&mut self, x: f32, row: f32, w: f32, rows: f32, radius: f32) {
        let (y, h) = (self.y(row), self.y(rows));
        self.body.push_str(&format!(
            "<rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{w:.2}\" height=\"{h:.2}\" \
             rx=\"{radius:.2}\" fill=\"none\" stroke=\"currentColor\" \
             stroke-width=\"{:.3}\"/>",
            Self::STROKE
        ));
    }

    /// A closed shape through the given cell points.
    fn polygon(&mut self, points: &[(f32, f32)], filled: bool) {
        let path: String = points
            .iter()
            .enumerate()
            .map(|(i, (x, row))| {
                let cmd = if i == 0 { 'M' } else { 'L' };
                format!("{cmd}{x:.2} {:.2}", self.y(*row))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let fill = if filled { "currentColor" } else { "none" };
        self.body.push_str(&format!(
            "<path d=\"{path} Z\" fill=\"{fill}\" stroke=\"currentColor\" \
             stroke-width=\"{:.3}\" stroke-linejoin=\"round\"/>",
            Self::STROKE
        ));
    }

    /// An open run through the given cell points, with rounded corners.
    fn polyline(&mut self, points: &[(f32, f32)], dashed: bool) {
        if points.len() < 2 {
            return;
        }
        let path: String = points
            .iter()
            .enumerate()
            .map(|(i, (x, row))| {
                let cmd = if i == 0 { 'M' } else { 'L' };
                format!("{cmd}{x:.2} {:.2}", self.y(*row))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let dash = if dashed {
            " stroke-dasharray=\"0.5 0.45\""
        } else {
            ""
        };
        self.body.push_str(&format!(
            "<path d=\"{path}\" fill=\"none\" stroke=\"currentColor\" \
             stroke-width=\"{:.3}\" stroke-linecap=\"round\" \
             stroke-linejoin=\"round\"{dash}/>",
            Self::STROKE
        ));
    }

    /// A filled triangle at `(x, row)`, pointing the way given.
    fn arrowhead(&mut self, x: f32, row: f32, dx: f32, dy: f32) {
        const LEN: f32 = 0.62;
        const HALF: f32 = 0.34;
        // In square units, so the head is the same shape whichever way
        // it points; converted back to rows for `polygon`.
        let back_row = row - dy * LEN / self.aspect;
        let (sx, sy) = (-dy * HALF, dx * HALF / self.aspect);
        // Written out rather than built in a loop: three points, and
        // the arithmetic reads as what it is.
        let tip = (x, row);
        let left = (x - dx * LEN + sx, back_row + sy);
        let right = (x - dx * LEN - sx, back_row - sy);
        self.polygon(&[tip, left, right], true);
    }

    fn finish(self, cols: usize, rows: usize) -> String {
        let (w, h) = (cols as f32, rows as f32 * self.aspect);
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w:.2} {h:.2}\" \
             width=\"{w:.2}\" height=\"{h:.2}\">{}</svg>",
            self.body
        )
    }
}

/// Renders a `mermaid` fence, or `None` when it is not a flowchart this
/// can lay out honestly.
/// `aspect` is how tall a grid row is relative to a cell's width, so
/// the picture can be laid out in cells and still be scaled uniformly
/// without an arrowhead or a corner radius coming out oval.
pub fn render(source: &str, columns: usize, aspect: f32) -> Option<Diagram> {
    let kind = source
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?
        .split_whitespace()
        .next()?;
    if kind == "sequenceDiagram" {
        return sequence::render(source, columns, aspect);
    }
    let (dir, mut nodes, edges, groups) = parse(source)?;
    if nodes.is_empty() {
        return None;
    }
    layer(&mut nodes, &edges, &groups);
    if dir.vertical() {
        draw_vertical(dir, &mut nodes, &edges, &groups, columns, aspect)
    } else {
        draw_horizontal(dir, &mut nodes, &edges, columns, aspect)
    }
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
    let text = node
        .lines
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0);
    // A diamond is only its full width across the middle, and a label
    // is taller than the one row it sits on, so its corners cut into
    // the text unless the shape is wider than a box would be.
    let padding = match node.shape {
        Shape::Diamond => 8,
        _ => 4,
    };
    text + padding
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

/// Shapes for a top-to-bottom (or bottom-to-top) flowchart.
///
/// The placement is the same grid the character renderer used, and
/// deliberately so: a figure occupies whole rows of the preview, so the
/// rows it takes have to be countable without drawing anything. Only
/// the painting is vector — which is also what lets an edge be one
/// rounded polyline instead of a run of corner characters that have to
/// be tee-joined where two of them meet.
fn draw_vertical(
    dir: Dir,
    nodes: &mut [Node],
    edges: &[Edge],
    groups: &[Group],
    columns: usize,
    aspect: f32,
) -> Option<Diagram> {
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
    // picture needs a gutter for them — sized to the widest, or they
    // are parsed and then run off the edge.
    let gutter = edges
        .iter()
        .filter_map(|e| e.label.as_deref())
        .map(|l| l.chars().count() + 3)
        .max()
        .unwrap_or(0);
    // A subgraph's title is written along its top border, so a group
    // whose boxes are narrower than its name still needs the room.
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
    // holds, so an edge arriving at the layer inside it has somewhere
    // to cross. Only taken when there is a subgraph to spend it on.
    let framed = !groups.is_empty();
    let headroom = if framed { FRAME_PAD } else { 0 };
    let gap = if framed { GAP_V + 3 } else { GAP_V };
    // Layers are not all one height — a box is as tall as its label
    // has lines — so the row each one starts on is the sum of the ones
    // above it rather than its depth times a constant.
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

    let mut svg = Svg::new(aspect);
    let mut labels: Vec<Label> = Vec::new();
    let mut tops: Vec<usize> = vec![0; nodes.len()];
    let mut bottoms: Vec<usize> = vec![0; nodes.len()];

    for (depth, layer) in rows.iter().enumerate() {
        let y = starts[depth];
        let mut x = (content - widths[depth]) / 2;
        for &i in layer {
            let w = box_width(&nodes[i]);
            shape(&mut svg, &mut labels, x, y, &nodes[i]);
            nodes[i].centre = x + w / 2;
            tops[i] = y;
            bottoms[i] = y + box_height(&nodes[i]);
            x += w + GAP_H;
        }
    }

    // A subgraph is drawn as a frame round the boxes it holds, once
    // they have been placed. Flattening the group into the layout and
    // framing it afterwards is what lets the layering stay one simple
    // pass; the frame is skipped when the nodes did not land in a clean
    // rectangle, because a frame that encloses something it does not
    // own says something false.
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
        // such a link is drawn inside the subgraph it belongs to.
        let x0 = left.saturating_sub(2) as f32;
        let x1 = ((right + 2).max(left + group.title.chars().count() + 2)).min(content) as f32;
        let y0 = top.saturating_sub(FRAME_PAD) as f32;
        let y1 = (bottom + FRAME_PAD) as f32;
        frame(&mut svg, &mut labels, x0, y0, x1, y1, &group.title);
    }

    // Edges whose ends landed on one row, which a top-to-bottom route
    // cannot express. Collected here and drawn under the row they
    // share, once everything on it is placed.
    let mut same_layer: Vec<&Edge> = Vec::new();

    for edge in edges {
        if edge.from == edge.to {
            continue;
        }
        let (a, b) = (edge.from, edge.to);
        // Drawn from whichever end is higher, so an edge that runs
        // against the layout direction still points somewhere.
        let (top, bottom) = if tops[a] <= tops[b] { (a, b) } else { (b, a) };
        let from_y = bottoms[top];
        let to_y = tops[bottom];
        if to_y <= from_y {
            same_layer.push(edge);
            continue;
        }
        let mid = from_y as f32 + (to_y - from_y) as f32 / 2.0;
        let (x0, x1) = (nodes[top].centre as f32, nodes[bottom].centre as f32);
        // One run with rounded joins. The character version needed
        // corner glyphs and a rule for what happens where two of them
        // land on one cell; a stroke simply overlaps a stroke.
        let head_at = to_y as f32 - HEAD_ROOM;
        svg.polyline(
            &[(x0, from_y as f32), (x0, mid), (x1, mid), (x1, head_at)],
            false,
        );
        svg.arrowhead(x1, to_y as f32, 0.0, 1.0);

        if let Some(label) = edge.label.as_deref() {
            labels.push(Label {
                x: x1 + 1.0,
                row: mid - 0.5,
                text: label.to_string(),
                centred: false,
                faint: true,
            });
        }
    }

    // Edges between boxes on the same row, routed under it: out of one,
    // across, and back up into the other.
    for edge in same_layer {
        let (a, b) = (edge.from, edge.to);
        if nodes[a].centre == nodes[b].centre {
            continue;
        }
        let base = bottoms[a].max(bottoms[b]) as f32;
        let run = base + 1.0;
        let (ax, bx) = (nodes[a].centre as f32, nodes[b].centre as f32);
        svg.polyline(
            &[
                (ax, base + HEAD_ROOM),
                (ax, run),
                (bx, run),
                (bx, base + HEAD_ROOM),
            ],
            false,
        );
        // Pointing up into the box it arrives at, since it comes from
        // below. A two-headed arrow arrives at both.
        svg.arrowhead(bx, base, 0.0, -1.0);
        if edge.both {
            svg.arrowhead(ax, base, 0.0, -1.0);
        }
        if let Some(label) = edge.label.as_deref() {
            // In the gutter rather than on the run: a same-row link
            // spans the whole distance between its ends, so there is
            // no clear space beside it to write in.
            //
            // Three links between the same three peers usually carry
            // the same word. Written once: three copies of
            // `Raft consensus` stacked on one line is noise, not
            // information.
            let already = labels
                .iter()
                .any(|l| l.text == label && (l.row - run).abs() < 0.01);
            if !already {
                labels.push(Label {
                    x: content as f32 + 1.0,
                    row: run - 0.5,
                    text: label.to_string(),
                    centred: false,
                    faint: true,
                });
            }
        }
    }

    Some(Diagram {
        svg: svg.finish(width, height),
        labels,
        cols: width,
        rows: height,
    })
}

/// How far short of a box an edge stops, so the arrowhead has room to
/// sit against the edge rather than through it.
const HEAD_ROOM: f32 = 0.62;

/// One node: its outline, and its label as text to typeset.
fn shape(svg: &mut Svg, labels: &mut Vec<Label>, x: usize, y: usize, node: &Node) {
    let w = box_width(node) as f32;
    let h = box_height(node) as f32;
    let (x, y) = (x as f32, y as f32);
    // Inset by half a cell on every side, so adjacent boxes have air
    // between them rather than touching outlines.
    let (bx, bw) = (x + 0.5, w - 1.0);
    let (by, bh) = (y + 0.25, h - 0.5);

    match node.shape {
        Shape::Rect => svg.rect(bx, by, bw, bh, 0.0),
        Shape::Round => svg.rect(bx, by, bw, bh, 0.9),
        Shape::Cylinder => {
            svg.rect(bx, by, bw, bh, 0.9);
            // The disc, which is what makes it read as a store.
            svg.polyline(&[(bx, by + 0.9), (bx + bw, by + 0.9)], false);
        }
        Shape::Diamond => svg.polygon(
            &[
                (bx + bw / 2.0, by),
                (bx + bw, by + bh / 2.0),
                (bx + bw / 2.0, by + bh),
                (bx, by + bh / 2.0),
            ],
            false,
        ),
    }

    let lid = f32::from(u8::from(node.shape == Shape::Cylinder)) * 0.5;
    for (row, text) in node.lines.iter().enumerate() {
        labels.push(Label {
            x: x + w / 2.0,
            row: y + lid + 0.5 + row as f32,
            text: text.clone(),
            centred: true,
            faint: false,
        });
    }
}

/// A subgraph's frame, with a gap in the top border for its title.
fn frame(svg: &mut Svg, labels: &mut Vec<Label>, x0: f32, y0: f32, x1: f32, y1: f32, title: &str) {
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let dashed = true;
    // Everything but the top edge, then the top edge in two pieces with
    // the title between them. Breaking the border rather than drawing
    // the title over it is what a cartouche does, and it means the
    // title needs no backing rectangle of its own.
    svg.polyline(&[(x0, y0), (x0, y1), (x1, y1), (x1, y0)], dashed);
    if title.is_empty() {
        svg.polyline(&[(x0, y0), (x1, y0)], dashed);
        return;
    }
    let lead = 1.0;
    let room = title.chars().count() as f32 + 1.0;
    svg.polyline(&[(x0, y0), (x0 + lead, y0)], dashed);
    svg.polyline(&[(x0 + lead + room, y0), (x1, y0)], dashed);
    labels.push(Label {
        x: x0 + lead + 0.5,
        row: y0 - 0.5,
        text: title.to_string(),
        centred: false,
        faint: true,
    });
}

/// Shapes for a left-to-right (or right-to-left) flowchart.
fn draw_horizontal(
    dir: Dir,
    nodes: &mut [Node],
    edges: &[Edge],
    columns: usize,
    aspect: f32,
) -> Option<Diagram> {
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
    // that space has to be wide enough for them.
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

    let mut svg = Svg::new(aspect);
    let mut labels: Vec<Label> = Vec::new();
    let mut lefts: Vec<usize> = vec![0; nodes.len()];
    let mut mids: Vec<f32> = vec![0.0; nodes.len()];
    let mut rights: Vec<usize> = vec![0; nodes.len()];

    let mut x = 0;
    for (depth, layer) in cols.iter().enumerate() {
        // Each column centred against the tallest, for the same reason
        // the vertical layout centres its rows.
        let mut y = (height - spans[depth]) / 2;
        for &i in layer {
            let tall = box_height(&nodes[i]);
            shape(&mut svg, &mut labels, x, y, &nodes[i]);
            lefts[i] = x;
            rights[i] = x + box_width(&nodes[i]);
            // The middle of the box, so an edge arrives beside the
            // label rather than against a border or a store's disc.
            mids[i] = y as f32 + tall as f32 / 2.0;
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
        let from_x = rights[near] as f32 - 0.5;
        let to_x = lefts[far] as f32 + 0.5;
        if to_x <= from_x {
            continue;
        }
        let mid = from_x + (to_x - from_x) / 2.0;
        let (y0, y1) = (mids[near], mids[far]);
        svg.polyline(
            &[(from_x, y0), (mid, y0), (mid, y1), (to_x - HEAD_ROOM, y1)],
            false,
        );
        svg.arrowhead(to_x, y1, 1.0, 0.0);

        if let Some(label) = edge.label.as_deref() {
            // Just past the bend and above the line it belongs to, so
            // two edges leaving one box do not label the same spot.
            labels.push(Label {
                x: mid + 0.5,
                row: y1 - 1.0,
                text: label.to_string(),
                centred: false,
                faint: true,
            });
        }
    }
    Some(Diagram {
        svg: svg.finish(width, height),
        labels,
        cols: width,
        rows: height,
    })
}

/// Sequence diagrams: participants across the top, time down.
///
/// A different drawing entirely from a flowchart, and an easier one —
/// the layout is a column per participant and a row per message, with
/// no layering to work out. It is here because `sequenceDiagram` is the
/// other half of what a project's documentation actually contains.
mod sequence {
    use super::{Label, Svg};

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

    pub fn render(source: &str, columns: usize, aspect: f32) -> Option<super::Diagram> {
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

        // Widened to fit the longest message rather than the text
        // being cut at the next lifeline. A message clipped to
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
        let mut svg = Svg::new(aspect);
        let mut labels: Vec<Label> = Vec::new();

        let mut x = 0;
        for (i, name) in names.iter().enumerate() {
            super::shape(
                &mut svg,
                &mut labels,
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

        // Lifelines run from under the boxes to the foot of the
        // picture, in one stroke each rather than per message row.
        for line in &lifelines {
            svg.polyline(
                &[
                    (*line as f32, head as f32 - 0.5),
                    (*line as f32, height as f32 - 0.5),
                ],
                true,
            );
        }

        for (i, message) in messages.iter().enumerate() {
            let row = head + i * 3;
            let (a, b) = (lifelines[message.from] as f32, lifelines[message.to] as f32);
            let arrow = row as f32 + 1.5;

            let (text_at, text_row) = if message.from == message.to {
                (a + 4.0, arrow)
            } else {
                (a.min(b) + 1.0, row as f32 + 0.5)
            };
            labels.push(Label {
                x: text_at,
                row: text_row,
                text: message.text.clone(),
                centred: false,
                faint: false,
            });

            let dashed = message.line == Line::Dashed;
            if message.from == message.to {
                // A message to itself turns back on its own lifeline.
                svg.polyline(
                    &[
                        (a, arrow - 0.9),
                        (a + 2.5, arrow - 0.9),
                        (a + 2.5, arrow + 0.9),
                        (a + super::HEAD_ROOM, arrow + 0.9),
                    ],
                    dashed,
                );
                svg.arrowhead(a, arrow + 0.9, -1.0, 0.0);
                continue;
            }

            let towards = if b > a { 1.0 } else { -1.0 };
            svg.polyline(
                &[(a, arrow), (b - towards * super::HEAD_ROOM, arrow)],
                dashed,
            );
            svg.arrowhead(b, arrow, towards, 0.0);
        }
        Some(super::Diagram {
            svg: svg.finish(width, height),
            labels,
            cols: width,
            rows: height,
        })
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

    fn diagram(src: &str) -> Diagram {
        render(src, 100, 2.0).expect("a diagram")
    }

    /// What the picture says: one label per line, in drawing order.
    ///
    /// The text is no longer part of the drawing — it is handed back
    /// to be typeset — so this is what used to be read off the
    /// character canvas, and most of these tests read it the same way.
    fn words(d: &Diagram) -> String {
        d.labels
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drawn(src: &str) -> String {
        words(&diagram(src))
    }

    /// The grid row a label sits on, and the cell it starts at.
    fn at(d: &Diagram, text: &str) -> (usize, f32) {
        let label = d
            .labels
            .iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("no label {text:?} among:\n{}", words(d)));
        (label.row as usize, label.x)
    }

    /// Whether anything in the picture is a filled shape, which only an
    /// arrowhead is.
    fn has_arrow(d: &Diagram) -> bool {
        d.svg.contains("fill=\"currentColor\"")
    }

    /// Whether anything is dashed, which only a subgraph frame and a
    /// reply are.
    fn has_dashes(d: &Diagram) -> bool {
        d.svg.contains("stroke-dasharray")
    }

    #[test]
    fn a_chain_of_boxes_is_drawn_top_to_bottom() {
        let d = diagram("flowchart TB\n  A[Start] --> B[Finish]\n");
        assert!(has_arrow(&d), "the edge should point somewhere");
        assert!(at(&d, "Start").0 < at(&d, "Finish").0, "{}", words(&d));
    }

    #[test]
    fn left_to_right_puts_them_side_by_side() {
        let d = diagram("flowchart LR\n  A[One] --> B[Two]\n");
        let (one, two) = (at(&d, "One"), at(&d, "Two"));
        assert_eq!(one.0, two.0, "both should share a row");
        assert!(one.1 < two.1, "and One comes first");
        assert!(has_arrow(&d));
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
        let shapes: Vec<String> = ["A[Step]", "A{Choice}", "A(Done)", "A[(Store)]"]
            .iter()
            .map(|node| diagram(&format!("flowchart TB\n  {node} --> B[Next]\n")).svg)
            .collect();
        for (i, a) in shapes.iter().enumerate() {
            for (j, b) in shapes.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "shapes {i} and {j} are drawn the same way");
            }
        }
        assert!(shapes[0].contains("rx=\"0.00\""), "a step is square");
        assert!(shapes[1].contains("<path"), "a decision is a polygon");
        assert!(shapes[2].contains("rx=\"0.90\""), "a terminus is rounded");
    }

    #[test]
    fn a_store_is_taller_than_the_box_beside_it() {
        // The disc across its top is a row of its own, so the layout
        // has to give it one.
        let node = |token: &str| {
            let (_, lines, shape) = node_parts(token).expect("a node");
            Node {
                lines: lines.expect("a label"),
                shape,
                layer: 0,
                order: 0,
                centre: 0,
            }
        };
        assert!(box_height(&node("A[(Store)]")) > box_height(&node("A[Step]")));
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
        let d = diagram(
            "flowchart TB\n  subgraph clients [Clients]\n    A[One]\n    B[Two]\n  end\n  A --> C[Out]\n",
        );
        let out = words(&d);
        assert!(out.contains("Clients"), "the group is named: {out}");
        assert!(has_dashes(&d), "and framed: {}", d.svg);
        for name in ["One", "Two", "Out"] {
            assert!(out.contains(name), "{name} missing:\n{out}");
        }
    }

    #[test]
    fn a_frame_is_dropped_rather_than_drawn_round_the_wrong_nodes() {
        // `B` is not in the group but shares its layer, so a frame
        // round the group's bounding box would enclose it and say
        // something false. The nodes are still drawn.
        let d = diagram(
            "flowchart TB\n  subgraph g [G]\n    A[Aaa]\n  end\n  B[Bbb]\n  A --> C[Ccc]\n",
        );
        assert!(!has_dashes(&d), "no frame should be drawn:\n{}", d.svg);
        let out = words(&d);
        for name in ["Aaa", "Bbb", "Ccc"] {
            assert!(out.contains(name), "{name} missing:\n{out}");
        }
    }

    #[test]
    fn a_quoted_label_breaks_where_it_says_to_break() {
        let d = diagram("flowchart TB\n  A[\"Node 0 (leader)\\ngRPC\"] --> B[Next]\n");
        let out = words(&d);
        assert!(!out.contains('\\'), "the escape should not survive:\n{out}");
        assert!(!out.contains('"'), "nor the quotes:\n{out}");
        // Two rows of one box, not one row saying something the author
        // did not write.
        let first = at(&d, "Node 0 (leader)");
        let second = at(&d, "gRPC");
        assert_eq!(second.0, first.0 + 1, "the second line is under the first");
        assert_eq!(second.1, first.1, "and in the same box");
    }

    #[test]
    fn a_br_tag_breaks_a_label_too() {
        let d = diagram("flowchart TB\n  A[One<br/>Two] --> B[Next]\n");
        assert_eq!(at(&d, "Two").0, at(&d, "One").0 + 1);
        assert!(!words(&d).contains("<br"), "the tag itself is not text");
    }

    #[test]
    fn a_two_headed_arrow_leaves_its_ends_side_by_side() {
        // `<-->` says two nodes are connected, not that one comes
        // before the other. Ranking it put the peers of a cluster on
        // three separate rows.
        let d = diagram("flowchart TB\n  A[Aaa] <--> B[Bbb]\n");
        assert_eq!(at(&d, "Aaa").0, at(&d, "Bbb").0, "both on one row");
    }

    #[test]
    fn a_subgraph_keeps_its_members_together() {
        // `C` is only reachable by a two-headed arrow, so nothing ranks
        // it and it would sit at the top on its own — leaving the
        // subgraph round it unable to frame anything honestly.
        let d = diagram(
            "flowchart TB\n  subgraph g [Group]\n    A[Aaa]\n    C[Ccc]\n  end\n  \
             Z[Zzz] --> A\n  A <--> C\n",
        );
        assert_eq!(at(&d, "Aaa").0, at(&d, "Ccc").0, "members share a row");
        assert!(words(&d).contains("Group"), "and the frame is named");
        assert!(has_dashes(&d), "and drawn");
    }

    #[test]
    fn a_subgraph_title_is_not_crossed_by_its_own_border() {
        // The border is broken for the title rather than drawn through
        // it, so the title needs no backing of its own and the frame
        // still reads as one shape.
        let d = diagram(
            "flowchart TB\n  Z[Zzz] --> A\n  subgraph g [Raft Cluster]\n    A[Aaa]\n  end\n",
        );
        let title = at(&d, "Raft Cluster");
        assert!(has_dashes(&d), "the frame is drawn");
        assert!(has_arrow(&d), "and the edge still arrives");
        assert!(
            title.0 < at(&d, "Aaa").0,
            "the title sits above what it names"
        );
    }

    #[test]
    fn every_node_is_named_exactly_once() {
        // Three layers, so an edge from the first to the third is
        // routed past the middle one — which on a character grid used
        // to write through its label.
        let d = diagram("flowchart TB\n  A[Aaa] --> B[Bbb]\n  B --> C[Ccc]\n  A --> C\n");
        for name in ["Aaa", "Bbb", "Ccc"] {
            let found = d.labels.iter().filter(|l| l.text == name).count();
            assert_eq!(found, 1, "{name} appears {found} times");
        }
    }

    #[test]
    fn a_sequence_diagram_draws_lifelines_and_messages() {
        let d = diagram(
            "sequenceDiagram\n  participant C as Client\n  participant S as Server\n\
             \n  C->>S: Set(key)\n  S-->>C: ok\n",
        );
        let out = words(&d);
        assert!(out.contains("Client") && out.contains("Server"), "{out}");
        assert!(out.contains("Set(key)") && out.contains("ok"), "{out}");
        assert!(has_arrow(&d), "both messages point somewhere");
        assert!(has_dashes(&d), "a reply is dashed, and so are lifelines");
    }

    #[test]
    fn a_message_is_not_cut_off_at_the_next_lifeline() {
        // The picture widens to the longest message instead:
        // `UNAVAILABLE + l` had lost the part that mattered.
        let long = "UNAVAILABLE plus a leader-address header";
        let out = drawn(&format!(
            "sequenceDiagram\n  participant A\n  participant B\n  A->>B: {long}\n"
        ));
        assert!(out.contains(long), "{out}");
    }

    #[test]
    fn a_message_to_itself_turns_back_on_its_own_lifeline() {
        let straight = diagram("sequenceDiagram\n  participant R\n  participant S\n  R->>S: go\n");
        let loops = diagram("sequenceDiagram\n  participant R\n  R->>R: replicate\n");
        assert!(words(&loops).contains("replicate"));
        // A message to itself leaves and comes back, so its run has
        // corners where a message to someone else is a straight line.
        let bends = |d: &Diagram| d.svg.matches('L').count();
        assert!(
            bends(&loops) > bends(&straight),
            "a self-message should turn back:\n{}",
            loops.svg
        );
    }

    #[test]
    fn participants_are_named_by_their_alias_and_in_order() {
        let d = diagram(
            "sequenceDiagram\n  participant L as Leader\n  participant F as Follower\n\
             \n  F->>L: hello\n",
        );
        assert!(
            at(&d, "Leader").1 < at(&d, "Follower").1,
            "declared order wins"
        );
    }

    #[test]
    fn what_it_cannot_draw_it_refuses() {
        let refuse = |src: &str, columns: usize| render(src, columns, 2.0).is_none();
        // Neither of the two diagrams this draws.
        assert!(refuse("pie title Pets\n  \"Dogs\" : 1\n", 100));
        assert!(refuse(
            "gantt\n  title A\n  section S\n  Task :a1, 2024-01-01, 30d\n",
            100
        ));
        assert!(refuse("erDiagram\n  A ||--o{ B : has\n", 100));
        // Control flow in a sequence diagram is drawn as nested frames;
        // without them the messages inside would read as though they
        // always happen, which is a different diagram.
        assert!(refuse(
            "sequenceDiagram\n  loop every minute\n  A->>B: hi\n  end\n",
            100
        ));
        // And a chart too wide for the column it has.
        assert!(refuse(
            "flowchart LR\n  A[aaaaaaaaaaaa] --> B[bbbbbbbbbbbb]\n",
            20
        ));
    }

    #[test]
    fn a_tinted_box_is_still_a_box() {
        // Styling is dropped rather than refused: there is no palette
        // here to honour it with, and refusing threw away the
        // structure as well — which is most of what made real charts
        // in a real README come out as their own source.
        let d = diagram(
            "flowchart TD\n  A[Client] --> B[Server]\n  B --> C[(Store)]\n\
             \n  style A fill:#f9f,stroke:#333\n  classDef hot fill:#f00\n\
             \n  class B hot\n  click A \"https://example.com\"\n\
             \n  linkStyle 0 stroke:#0f0\n",
        );
        let out = words(&d);
        for name in ["Client", "Server", "Store"] {
            assert!(
                out.contains(name),
                "{name} was dropped with the styling:\n{out}"
            );
        }
        assert!(has_arrow(&d), "the edges survived too");
    }

    #[test]
    fn an_ampersand_is_every_edge_between_the_two_sides() {
        let d = diagram("flowchart TD\n  A & B --> C\n");
        assert_eq!(at(&d, "A").0, at(&d, "B").0, "A and B share a layer");
        assert!(at(&d, "C").0 > at(&d, "A").0, "C is below them");
    }

    #[test]
    fn an_ampersand_inside_a_label_is_not_a_chain() {
        let out = drawn("flowchart LR\n  A[Cut & Paste] --> B[Done]\n");
        assert!(out.contains("Cut & Paste"), "one node, one label:\n{out}");
    }

    #[test]
    fn an_unknown_direction_is_refused_rather_than_guessed() {
        assert!(render("flowchart XY\n  A --> B\n", 100, 2.0).is_none());
        // But a missing one is top-down, as Mermaid says.
        assert!(render("flowchart\n  A --> B\n", 100, 2.0).is_some());
    }

    #[test]
    fn the_picture_fits_the_rows_it_says_it_takes() {
        // The preview counts a figure's rows to scroll past it, so a
        // picture taller than it claims would overlap what follows.
        let d = diagram("flowchart TB\n  A[Aaa] --> B[Bbb]\n  B --> C[Ccc]\n");
        let height: f32 = d.rows as f32 * 2.0;
        let viewbox = d
            .svg
            .split("viewBox=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("a viewBox");
        let parts: Vec<f32> = viewbox
            .split_whitespace()
            .map(|n| n.parse().expect("a number"))
            .collect();
        assert_eq!(parts[2], d.cols as f32, "width in cells");
        assert_eq!(parts[3], height, "height in rows times the aspect");
    }
}
