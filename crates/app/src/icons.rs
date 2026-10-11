//! Small bitmap icons.
//!
//! The renderer has two primitives, rectangles and glyphs, and no font we
//! ship has a git logo in it. So these are drawn: a grid per icon, one
//! rectangle per filled cell. At the size a status bar needs them that is
//! a few dozen rectangles, they are crisp at any scale because nothing is
//! resampled, and they cannot go missing the way a font glyph can.

use lithe_gpu::{Frame, Rgba};

/// Which icon. Each is an 9x9 grid, `#` filled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Files,
    Git,
    Search,
    Eye,
}

impl Icon {
    fn grid(self) -> &'static [&'static str; 9] {
        match self {
            // A page with a folded corner.
            Icon::Files => &[
                "######...",
                "#....##..",
                "#....#.#.",
                "#....####",
                "#.......#",
                "#.......#",
                "#.......#",
                "#.......#",
                "#########",
            ],
            // A branch: a trunk on the left, with a branch curving in
            // from the top right. An earlier version ran the right-hand
            // stroke the full height and simply read as a letter H.
            Icon::Git => &[
                "###....##",
                "###....##",
                ".#.....##",
                ".#....###",
                ".#..####.",
                ".####....",
                ".#.......",
                "###......",
                "###......",
            ],
            // A magnifier.
            Icon::Search => &[
                ".#####...",
                "##...##..",
                "#.....#..",
                "#.....#..",
                "##...##..",
                ".#####...",
                "....###..",
                ".....###.",
                "......###",
            ],
            // An eye, open.
            Icon::Eye => &[
                ".........",
                "..#####..",
                ".##...##.",
                "##..#..##",
                "#..###..#",
                "##..#..##",
                ".##...##.",
                "..#####..",
                ".........",
            ],
        }
    }
}

/// Draws `icon` with its top-left at `x`, `y`, scaled so the whole grid is
/// about `size` pixels across.
pub fn draw(f: &mut Frame<'_>, icon: Icon, x: f32, y: f32, size: f32, colour: Rgba) {
    // Whole pixels only: a fractional scale makes a one-pixel stroke
    // blurry, which at this size is most of the icon.
    let px = (size / 9.0).round().max(1.0);
    let grid = icon.grid();

    for (row, line) in grid.iter().enumerate() {
        // Runs of filled cells become one rectangle rather than several.
        let cells: Vec<bool> = line.chars().map(|c| c == '#').collect();
        let mut start = None;
        for column in 0..=cells.len() {
            let filled = cells.get(column).copied().unwrap_or(false);
            match (filled, start) {
                (true, None) => start = Some(column),
                (false, Some(from)) => {
                    f.rect(
                        x + from as f32 * px,
                        y + row as f32 * px,
                        (column - from) as f32 * px,
                        px,
                        colour,
                    );
                    start = None;
                }
                _ => {}
            }
        }
    }
}

/// How wide a drawn icon ends up, so callers can lay out around it.
pub fn width(size: f32) -> f32 {
    (size / 9.0).round().max(1.0) * 9.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_nine_by_nine_grid() {
        for icon in [Icon::Files, Icon::Git, Icon::Search, Icon::Eye] {
            let grid = icon.grid();
            assert_eq!(grid.len(), 9, "{icon:?} has the wrong number of rows");
            for (row, line) in grid.iter().enumerate() {
                assert_eq!(line.chars().count(), 9, "{icon:?} row {row} is {:?}", line);
                assert!(
                    line.chars().all(|c| c == '#' || c == '.'),
                    "{icon:?} row {row} has a stray character: {line:?}"
                );
            }
        }
    }

    #[test]
    fn every_icon_actually_has_something_in_it() {
        for icon in [Icon::Files, Icon::Git, Icon::Search, Icon::Eye] {
            let filled: usize = icon
                .grid()
                .iter()
                .map(|l| l.chars().filter(|c| *c == '#').count())
                .sum();
            assert!(filled > 10, "{icon:?} is nearly blank ({filled} cells)");
            assert!(filled < 70, "{icon:?} is nearly solid ({filled} cells)");
        }
    }

    #[test]
    fn width_is_a_whole_number_of_pixels() {
        // Fractional scaling is what makes one-pixel strokes muddy.
        for size in [9.0, 12.0, 14.0, 18.0, 26.0] {
            let w = width(size);
            assert_eq!(w % 9.0, 0.0, "size {size} gave a fractional scale");
        }
    }
}
