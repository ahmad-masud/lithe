//! Glyph atlas.
//!
//! A code editor is overwhelmingly a monospace ASCII grid, and this is where
//! we cash that in. Printable ASCII is rasterised once at startup into a
//! single-channel coverage texture; drawing a screenful afterwards is pure
//! table lookup plus one instance push per cell — no shaping, no layout, no
//! allocation.
//!
//! Anything outside that set (CJK, emoji, box drawing, combining marks) is
//! rasterised on first sight and cached in the same texture, so the slow path
//! is slow exactly once.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight,
};

/// Which face a glyph is rasterised in. Bold is a second set in the same
/// atlas rather than a second atlas: the cache key carries the weight.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Face {
    #[default]
    Regular,
    Bold,
}

impl Face {
    fn weight(self) -> Weight {
        match self {
            Face::Regular => Weight::NORMAL,
            Face::Bold => Weight::BOLD,
        }
    }
}

/// How large a glyph is rasterised, relative to the editor's own cell.
///
/// The grid is one size, and that is what the fast path rests on. The
/// Markdown preview is the one place that wants more, so a heading can be
/// a heading rather than bold body text. It pays for that by asking for a
/// size by name: three of them, rasterised on first sight, rather than a
/// continuous range that would fill the atlas with near-identical glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Size {
    #[default]
    Body,
    /// Half again, for an `h2`.
    Large,
    /// Nearly double, for an `h1`.
    Huge,
}

impl Size {
    fn factor(self) -> f32 {
        match self {
            Self::Body => 1.0,
            Self::Large => 1.45,
            Self::Huge => 1.9,
        }
    }
}

/// Atlas edge length in texels. R8 single channel, so 1024 x 1024 is 1 MiB —
/// room for ASCII in several styles plus a long tail of incidental glyphs.
const ATLAS_SIZE: u32 = 1024;

/// Padding between packed glyphs, so neighbouring cells cannot bleed.
const PAD: u32 = 1;

/// Candidate monospace faces, best first.
///
/// This list is load-bearing, not a nicety. We build cosmic-text without
/// fontconfig, so fontdb has no generic monospace family configured and
/// `Family::Monospace` resolves to whichever monospaced face happens to sort
/// first — on macOS that is Courier New *Italic*, which renders an entire
/// editor in slanted serif text. Pointing the generic family at a real face
/// fixes it for every lookup, not just ours.
///
/// M3 replaces this with the family named in the config file, falling back
/// to this list.
const MONOSPACE_PREFERENCES: &[&str] = &[
    // macOS
    "Menlo",
    "Monaco",
    // Linux
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    // Windows
    "Cascadia Mono",
    "Consolas",
    // Broadly present, last resorts
    "Andale Mono",
    "Courier New",
];

/// Normalised atlas rectangle.
#[derive(Clone, Copy, Debug, Default)]
pub struct UvRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// A rasterised glyph: where it lives in the atlas, and where to put it
/// relative to the pen position on the baseline.
#[derive(Clone, Copy, Debug, Default)]
pub struct Glyph {
    pub uv: UvRect,
    /// Bitmap size in physical px.
    pub size: [f32; 2],
    /// Offset from pen origin to the bitmap's top-left corner, physical px.
    /// `y` is already measured downward from the baseline.
    pub offset: [f32; 2],
}

/// Grid geometry, in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    /// Horizontal advance of one cell. All ASCII share this in a monospace
    /// face, which is the invariant the fast path rests on.
    pub advance: f32,
    pub line_height: f32,
    /// Baseline offset from the top of the line box.
    pub ascent: f32,
}

pub struct Atlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,

    // Shelf packer. Glyphs are near-uniform in height here, so shelves waste
    // almost nothing and the packer stays ~20 lines instead of 200.
    pen_x: u32,
    pen_y: u32,
    shelf_h: u32,

    cache: HashMap<(char, Face, Size), Option<Glyph>>,
    /// Grid geometry per size, measured on first use of that size.
    sizes: HashMap<Size, CellMetrics>,
    /// Reserved fully-opaque texel. Lets a filled rect reuse the glyph
    /// pipeline rather than needing one of its own.
    pub opaque: UvRect,
    pub metrics: CellMetrics,

    fonts: FontSystem,
    swash: SwashCache,
    font_px: f32,
}

impl Atlas {
    /// Builds the atlas and rasterises printable ASCII. `font_px` is in
    /// physical pixels — the caller has already applied the display scale.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, font_px: f32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lithe.atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut atlas = Self {
            texture,
            view,
            pen_x: 0,
            pen_y: 0,
            shelf_h: 0,
            cache: HashMap::new(),
            sizes: HashMap::new(),
            opaque: UvRect::default(),
            metrics: CellMetrics {
                advance: font_px * 0.6,
                line_height: (font_px * 1.4).round(),
                ascent: font_px,
            },
            fonts: FontSystem::new(),
            swash: SwashCache::new(),
            font_px,
        };

        atlas.configure_monospace();
        atlas.reserve_opaque_texel(queue);
        atlas.measure_cell();
        atlas.warm_ascii(queue);
        atlas
    }

    /// Points fontdb's generic monospace family at the best face actually
    /// installed, so `Family::Monospace` resolves to something upright and
    /// genuinely fixed-width. See [`MONOSPACE_PREFERENCES`].
    fn configure_monospace(&mut self) {
        let chosen = MONOSPACE_PREFERENCES.iter().copied().find(|name| {
            self.fonts.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(name))
            })
        });

        match chosen {
            Some(name) => {
                self.fonts.db_mut().set_monospace_family(name);
                log::info!("monospace face: {name}");
            }
            // Nothing recognised. fontdb's own pick is then the best we can
            // do, and the editor is still usable.
            None => {
                log::warn!("no known monospace face installed; falling back to the system default")
            }
        }
    }

    /// Writes a single 0xFF texel and hands back its uv, sampled by rects.
    fn reserve_opaque_texel(&mut self, queue: &wgpu::Queue) {
        let (x, y) = (self.pen_x, self.pen_y);
        self.upload(queue, x, y, 1, 1, &[0xFF]);

        // Sample the texel centre so nearest-neighbour cannot drift off it.
        let c = 0.5 / ATLAS_SIZE as f32;
        self.opaque = UvRect {
            min: [c, c],
            max: [c, c],
        };
        self.pen_x += 1 + PAD;
        self.shelf_h = 1;
    }

    /// Derives grid geometry by shaping one reference character. In a
    /// monospace face every ASCII cell shares this advance.
    fn measure_cell(&mut self) {
        let metrics = Metrics::new(self.font_px, (self.font_px * 1.4).round());
        let mut buf = Buffer::new(&mut self.fonts, metrics);
        let attrs = Attrs::new().family(Family::Monospace);
        buf.set_text("M", &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fonts, false);

        if let Some(run) = buf.layout_runs().next() {
            if let Some(glyph) = run.glyphs.first() {
                self.metrics = CellMetrics {
                    advance: glyph.w,
                    line_height: run.line_height,
                    ascent: run.line_y,
                };
            }
        }
    }

    /// Rasterises printable ASCII up front so steady-state drawing never
    /// touches the shaper.
    fn warm_ascii(&mut self, queue: &wgpu::Queue) {
        for byte in 0x21u8..0x7Fu8 {
            self.glyph(queue, byte as char, Face::Regular, Size::Body);
        }
        // Space has no coverage; record it as empty so it is never
        // re-shaped. Bold is left cold: most documents never ask for it,
        // and when one does it rasterises on first sight like any other
        // uncommon glyph.
        for size in [Size::Body, Size::Large, Size::Huge] {
            self.cache.insert((' ', Face::Regular, size), None);
            self.cache.insert((' ', Face::Bold, size), None);
        }
    }

    /// Looks up a glyph, rasterising and packing it on first sight.
    ///
    /// `None` means "nothing to draw" — a space, or a character the font
    /// stack has no coverage for. Both are cached, so a miss costs once.
    pub fn glyph(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        face: Face,
        size: Size,
    ) -> Option<Glyph> {
        if let Some(hit) = self.cache.get(&(ch, face, size)) {
            return *hit;
        }
        let packed = self.rasterize(queue, ch, face, size);
        self.cache.insert((ch, face, size), packed);
        packed
    }

    /// Grid geometry at `size`, measured once per size.
    ///
    /// Measured rather than scaled from the body metrics: hinting means a
    /// face at 1.45x the size is not reliably 1.45x the advance, and a
    /// guessed advance shows up immediately as text that drifts out of its
    /// own background.
    pub fn metrics_at(&mut self, size: Size) -> CellMetrics {
        if size == Size::Body {
            return self.metrics;
        }
        if let Some(hit) = self.sizes.get(&size) {
            return *hit;
        }
        let px = self.font_px * size.factor();
        let metrics = Metrics::new(px, (px * 1.4).round());
        let mut buf = Buffer::new(&mut self.fonts, metrics);
        let attrs = Attrs::new().family(Family::Monospace);
        buf.set_text("M", &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fonts, false);

        let measured = buf
            .layout_runs()
            .next()
            .and_then(|run| {
                let glyph = run.glyphs.first()?;
                Some(CellMetrics {
                    advance: glyph.w,
                    line_height: run.line_height,
                    ascent: run.line_y,
                })
            })
            .unwrap_or(CellMetrics {
                advance: self.metrics.advance * size.factor(),
                line_height: self.metrics.line_height * size.factor(),
                ascent: self.metrics.ascent * size.factor(),
            });
        self.sizes.insert(size, measured);
        measured
    }

    fn rasterize(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        face: Face,
        size: Size,
    ) -> Option<Glyph> {
        // Shape the character alone: we want the standalone glyph for a grid
        // cell, not one altered by its neighbours' kerning or ligatures.
        let line_height = self.metrics_at(size).line_height;
        let metrics = Metrics::new(self.font_px * size.factor(), line_height);
        let mut buf = Buffer::new(&mut self.fonts, metrics);
        let attrs = Attrs::new().family(Family::Monospace).weight(face.weight());
        let mut scratch = [0u8; 4];
        buf.set_text(
            ch.encode_utf8(&mut scratch),
            &attrs,
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.fonts, false);

        let key = buf
            .layout_runs()
            .next()?
            .glyphs
            .first()?
            .physical((0.0, 0.0), 1.0)
            .cache_key;

        let image = self.swash.get_image_uncached(&mut self.fonts, key)?;
        let (w, h) = (image.placement.width, image.placement.height);
        if w == 0 || h == 0 {
            return None;
        }

        // Coverage masks go straight in. Colour glyphs (emoji) need an RGBA
        // atlas and a second pipeline; they are out of scope until the editor
        // earns them, so flatten to the alpha channel rather than drop them.
        let coverage: Vec<u8> = match image.content {
            SwashContent::Mask => image.data,
            SwashContent::SubpixelMask => image.data.chunks_exact(4).map(|px| px[3]).collect(),
            SwashContent::Color => image.data.chunks_exact(4).map(|px| px[3]).collect(),
        };
        if coverage.len() < (w * h) as usize {
            return None;
        }

        let (x, y) = self.alloc(w, h)?;
        self.upload(queue, x, y, w, h, &coverage);

        let inv = 1.0 / ATLAS_SIZE as f32;
        Some(Glyph {
            uv: UvRect {
                min: [x as f32 * inv, y as f32 * inv],
                max: [(x + w) as f32 * inv, (y + h) as f32 * inv],
            },
            size: [w as f32, h as f32],
            offset: [image.placement.left as f32, -image.placement.top as f32],
        })
    }

    /// Shelf allocation. `None` when the atlas is full, which for a code
    /// editor means something pathological is on screen; callers treat it as
    /// "nothing to draw" rather than failing the frame.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w + PAD > ATLAS_SIZE {
            return None;
        }
        if self.pen_x + w > ATLAS_SIZE {
            self.pen_x = 0;
            self.pen_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.pen_y + h > ATLAS_SIZE {
            return None;
        }
        let spot = (self.pen_x, self.pen_y);
        self.pen_x += w + PAD;
        self.shelf_h = self.shelf_h.max(h);
        Some(spot)
    }

    fn upload(&self, queue: &wgpu::Queue, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}
