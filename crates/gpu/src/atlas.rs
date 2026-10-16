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

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight};

/// Which face a glyph is rasterised in. Bold is a second set in the same
/// atlas rather than a second atlas: the cache key carries the weight.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Face {
    #[default]
    Regular,
    Bold,
    /// Slanted. Only prose asks for it: the editor has no italic, and a
    /// grid of slanted code is nobody's idea of readable.
    Italic,
    BoldItalic,
}

impl Face {
    fn weight(self) -> Weight {
        match self {
            Face::Regular | Face::Italic => Weight::NORMAL,
            Face::Bold | Face::BoldItalic => Weight::BOLD,
        }
    }

    fn slant(self) -> cosmic_text::Style {
        match self {
            Face::Italic | Face::BoldItalic => cosmic_text::Style::Italic,
            _ => cosmic_text::Style::Normal,
        }
    }
}

/// Which face a run is drawn in: the grid's, or a document's.
///
/// The editor is one monospace face by design — a uniform grid is what
/// lets a screenful cost one instance per cell. The Markdown preview is a
/// document rather than a grid, and prose set in a typewriter face reads
/// like a terminal however well it is coloured, so it gets a proportional
/// one. Code, tables and diagrams inside the preview stay on the grid,
/// because their meaning is in their alignment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Family {
    #[default]
    Mono,
    Prose,
}

impl Family {
    fn resolved(self) -> cosmic_text::Family<'static> {
        match self {
            Self::Mono => cosmic_text::Family::Monospace,
            Self::Prose => cosmic_text::Family::SansSerif,
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
    /// A shade under body, for inline code. A monospace face at the
    /// same nominal size as the prose around it reads as larger than
    /// it, and a word of code in the middle of a sentence should sit
    /// in the line rather than on top of it.
    Small,
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
            Self::Small => 0.88,
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

/// How many shaped runs to keep. Far more than a window holds, so a
/// page being read back and forth never re-shapes, and small enough
/// that a document scrolled end to end does not accumulate one entry
/// per line of it.
const MAX_RUNS: usize = 4096;

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

/// Candidate proportional faces for the preview's prose, best first.
///
/// The same problem as [`MONOSPACE_PREFERENCES`], for the same reason:
/// without fontconfig, `Family::SansSerif` resolves to whichever face
/// happens to sort first, which is how a document ends up set in
/// something nobody chose. Each platform's own interface face comes
/// first, because it is the one guaranteed to be hinted and complete at
/// thirteen pixels.
const PROSE_PREFERENCES: &[&str] = &[
    // macOS
    "SF Pro Text",
    "SF Pro Display",
    "Helvetica Neue",
    // Windows
    "Segoe UI Variable Text",
    "Segoe UI",
    // Linux
    "Inter",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    // Broadly present, last resorts
    "Arial",
    "Helvetica",
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

/// One glyph of a shaped run: its image in the atlas, and where it
/// sits along the run.
///
/// No advance of its own. A shaped glyph's position comes from the
/// run it was shaped in — that is the whole point of shaping — and an
/// advance here would be a second answer to the same question.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub uv: UvRect,
    pub size: [f32; 2],
    /// From the pen origin to the bitmap's top-left, physical px.
    pub offset: [f32; 2],
    /// From the start of the run to this glyph's pen, physical px.
    pub x: f32,
}

/// A run of proportional text, shaped once.
///
/// Shaping the whole run is what gives it kerning: an `AVATAR` set
/// character by character is a tenth wider than the same word shaped
/// together, and headings — most capitals, largest size — are where
/// that shows most.
///
/// Measuring and drawing both read this, so the width reported and the
/// positions drawn come out of one array rather than out of two sums
/// that have to agree. That was the reason the old code shaped each
/// character alone, and it is kept rather than traded away.
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub glyphs: Vec<Placed>,
    pub width: f32,
}

pub struct Atlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,

    // Shelf packer. Glyphs are near-uniform in height here, so shelves waste
    // almost nothing and the packer stays ~20 lines instead of 200.
    pen_x: u32,
    pen_y: u32,
    shelf_h: u32,

    cache: HashMap<(char, Face, Size, Family), Option<Glyph>>,
    /// Shaped runs of proportional text, by what they say and how they
    /// are set. Bounded by what the window has shown: a run is a span
    /// of one line, and the same words come back frame after frame.
    runs: HashMap<(Box<str>, Face, Size, Family), Run>,
    /// Glyph images by the identity shaping gave them, which a
    /// character cannot name: a ligature is one glyph for two letters,
    /// and a contextual alternate is a different glyph for the same
    /// one.
    sprites: HashMap<cosmic_text::CacheKey, Option<Placed>>,
    /// Line geometry per size and face, measured on first use.
    sizes: HashMap<(Size, Family), CellMetrics>,
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
            runs: HashMap::new(),
            sprites: HashMap::new(),
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
        atlas.configure_prose();
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

    /// The same, for the proportional face the preview sets prose in.
    fn configure_prose(&mut self) {
        let chosen = PROSE_PREFERENCES.iter().copied().find(|name| {
            self.fonts.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(name))
            })
        });

        match chosen {
            Some(name) => {
                self.fonts.db_mut().set_sans_serif_family(name);
                log::info!("prose face: {name}");
            }
            // The preview then reads in whatever the system calls sans
            // serif, which is still a document rather than a terminal.
            None => log::warn!("no known prose face installed; falling back to the system default"),
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
        let attrs = Attrs::new().family(Family::Mono.resolved());
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
            for face in [Face::Regular, Face::Bold] {
                self.cache
                    .insert((' ', face, size, Family::Mono), Some(Glyph::default()));
            }
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
        self.glyph_in(queue, ch, face, size, Family::Mono)
    }

    /// As [`Atlas::glyph`], in a chosen face.
    ///
    /// A glyph with no coverage of its own — a space — still comes back,
    /// because in a proportional face how wide a space is is a fact about
    /// the font rather than about the grid.
    pub fn glyph_in(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        face: Face,
        size: Size,
        family: Family,
    ) -> Option<Glyph> {
        if let Some(hit) = self.cache.get(&(ch, face, size, family)) {
            return *hit;
        }
        let packed = self.rasterize(queue, ch, face, size, family);
        self.cache.insert((ch, face, size, family), packed);
        packed
    }

    /// How wide `text` is drawn, in physical pixels.
    ///
    /// The same advances [`Atlas::glyph_in`] hands the renderer, so what
    /// is measured here and what is drawn there cannot disagree.
    pub fn measure(
        &mut self,
        queue: &wgpu::Queue,
        text: &str,
        face: Face,
        size: Size,
        family: Family,
    ) -> f32 {
        // Every cell the same width is the whole point of the grid.
        if family == Family::Mono {
            return self.metrics_in(size, family).advance * text.chars().count() as f32;
        }
        // The shaped run, which is also what is drawn: what this
        // reports and where the glyphs land come out of one array, so
        // a line cannot wrap in one place and be painted in another.
        self.run(queue, text, face, size, family).width
    }

    /// Grid geometry at `size`, measured once per size.
    ///
    /// Measured rather than scaled from the body metrics: hinting means a
    /// face at 1.45x the size is not reliably 1.45x the advance, and a
    /// guessed advance shows up immediately as text that drifts out of its
    /// own background.
    pub fn metrics_at(&mut self, size: Size) -> CellMetrics {
        self.metrics_in(size, Family::Mono)
    }

    /// As [`Atlas::metrics_at`], for a chosen face.
    ///
    /// The advance it reports is the grid cell for a monospace face and
    /// an `M` for a proportional one, where it means nothing — prose is
    /// measured glyph by glyph. The line height and ascent are what the
    /// preview lays rows out on, and those are the point.
    pub fn metrics_in(&mut self, size: Size, family: Family) -> CellMetrics {
        if size == Size::Body && family == Family::Mono {
            return self.metrics;
        }
        if let Some(hit) = self.sizes.get(&(size, family)) {
            return *hit;
        }
        let px = self.font_px * size.factor();
        let metrics = Metrics::new(px, (px * 1.4).round());
        let mut buf = Buffer::new(&mut self.fonts, metrics);
        let attrs = Attrs::new().family(family.resolved());
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
        self.sizes.insert((size, family), measured);
        measured
    }

    fn rasterize(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        face: Face,
        size: Size,
        family: Family,
    ) -> Option<Glyph> {
        // Shape the character alone: we want the standalone glyph, not one
        // altered by its neighbours' kerning or ligatures. That costs
        // prose its kerning, and buys something worth more — the width
        // this reports and the width it draws at are the same number, so
        // a line can never wrap one place and be drawn in another.
        let line_height = self.metrics_in(size, family).line_height;
        let metrics = Metrics::new(self.font_px * size.factor(), line_height);
        let mut buf = Buffer::new(&mut self.fonts, metrics);
        let attrs = Attrs::new()
            .family(family.resolved())
            .weight(face.weight())
            .style(face.slant());
        let mut scratch = [0u8; 4];
        buf.set_text(
            ch.encode_utf8(&mut scratch),
            &attrs,
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.fonts, false);

        let laid_out = buf.layout_runs().next()?.glyphs.first()?.clone();
        let key = laid_out.physical((0.0, 0.0), 1.0).cache_key;

        let image = self.swash.get_image_uncached(&mut self.fonts, key)?;
        let (w, h) = (image.placement.width, image.placement.height);
        // No coverage — a space, most often. It still moves the pen.
        if w == 0 || h == 0 {
            return Some(Glyph::default());
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

    /// A run of proportional text, shaped and cached.
    ///
    /// The monospace face is not shaped: every cell is the same width
    /// by definition, and the grid depends on that being exactly true
    /// rather than approximately.
    pub fn run(
        &mut self,
        queue: &wgpu::Queue,
        text: &str,
        face: Face,
        size: Size,
        family: Family,
    ) -> &Run {
        let key = (Box::from(text), face, size, family);
        if !self.runs.contains_key(&key) {
            // Scrolling a long document asks about a different run on
            // every line, so this cannot grow forever. Cleared rather
            // than evicted one at a time: what is on screen is shaped
            // again on the next frame, and a frame only happens when
            // something changed anyway.
            if self.runs.len() >= MAX_RUNS {
                self.runs.clear();
            }
            let built = self.shape(queue, text, face, size, family);
            self.runs.insert(key.clone(), built);
        }
        &self.runs[&key]
    }

    fn shape(
        &mut self,
        queue: &wgpu::Queue,
        text: &str,
        face: Face,
        size: Size,
        family: Family,
    ) -> Run {
        let line_height = self.metrics_in(size, family).line_height;
        let metrics = Metrics::new(self.font_px * size.factor(), line_height);
        let attrs = Attrs::new()
            .family(family.resolved())
            .weight(face.weight())
            .style(face.slant());

        // Collected before anything is rasterised: the buffer borrows
        // the font system, and so does the rasteriser.
        let (places, width) = {
            let mut buf = Buffer::new(&mut self.fonts, metrics);
            // The caller has already decided where this line ends, so
            // the shaper must not decide again.
            buf.set_wrap(cosmic_text::Wrap::None);
            buf.set_size(None, None);
            buf.set_text(text, &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.fonts, false);
            match buf.layout_runs().next() {
                Some(run) => (
                    run.glyphs
                        .iter()
                        .map(|g| (g.physical((0.0, 0.0), 1.0).cache_key, g.x))
                        .collect::<Vec<_>>(),
                    run.line_w,
                ),
                None => (Vec::new(), 0.0),
            }
        };

        let mut glyphs = Vec::with_capacity(places.len());
        for (key, x) in places {
            if let Some(sprite) = self.sprite(queue, key) {
                glyphs.push(Placed { x, ..sprite });
            }
        }
        Run { glyphs, width }
    }

    /// One shaped glyph's image, rasterised and packed on first use.
    ///
    /// `None` for a glyph with no coverage — a space, most often —
    /// which has nothing to draw but still moved the pen when the run
    /// was shaped.
    fn sprite(&mut self, queue: &wgpu::Queue, key: cosmic_text::CacheKey) -> Option<Placed> {
        if let Some(hit) = self.sprites.get(&key) {
            return *hit;
        }
        let packed = self.pack(queue, key);
        self.sprites.insert(key, packed);
        packed
    }

    fn pack(&mut self, queue: &wgpu::Queue, key: cosmic_text::CacheKey) -> Option<Placed> {
        let image = self.swash.get_image_uncached(&mut self.fonts, key)?;
        let (w, h) = (image.placement.width, image.placement.height);
        if w == 0 || h == 0 {
            return None;
        }
        let coverage: Vec<u8> = match image.content {
            SwashContent::Mask => image.data,
            SwashContent::SubpixelMask | SwashContent::Color => {
                image.data.chunks_exact(4).map(|px| px[3]).collect()
            }
        };
        if coverage.len() < (w * h) as usize {
            return None;
        }
        let (x, y) = self.alloc(w, h)?;
        self.upload(queue, x, y, w, h, &coverage);
        let inv = 1.0 / ATLAS_SIZE as f32;
        Some(Placed {
            uv: UvRect {
                min: [x as f32 * inv, y as f32 * inv],
                max: [(x + w) as f32 * inv, (y + h) as f32 * inv],
            },
            size: [w as f32, h as f32],
            offset: [image.placement.left as f32, -image.placement.top as f32],
            x: 0.0,
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
