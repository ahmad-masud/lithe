//! Window, GPU surface and the two drawing primitives the rest of the editor
//! is built from.
//!
//! The public surface is deliberately tiny: `rect` and `text`. A code editor
//! is a character grid, so a file tree, a command palette, a search result
//! list and the editor body are all the same two calls with different
//! coordinates. There is no widget tree and no layout engine to pay for.

mod atlas;
mod renderer;

pub use atlas::{CellMetrics, Face, Size};
pub use renderer::{Frame, Image, Renderer};

/// A colour, as the surface wants it.
///
/// Values are sRGB-encoded and passed through unconverted, because the
/// surface is a UNORM target on purpose — see the format choice in
/// `Renderer::new`. Blending glyph coverage in gamma space is what keeps text
/// at the weight the rasteriser intended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const BLACK: Self = Self([0.0, 0.0, 0.0, 1.0]);
    pub const TRANSPARENT: Self = Self([0.0, 0.0, 0.0, 0.0]);

    /// From `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self([
            ((rgb >> 16) & 0xFF) as f32 / 255.0,
            ((rgb >> 8) & 0xFF) as f32 / 255.0,
            (rgb & 0xFF) as f32 / 255.0,
            1.0,
        ])
    }

    pub const fn alpha(self, a: f32) -> Self {
        Self([self.0[0], self.0[1], self.0[2], a])
    }
}
