//! Image preview.
//!
//! Decoded once on open and kept on the GPU, not re-decoded per frame. A
//! photograph is the one thing in this editor that is genuinely large, so
//! it is also the one thing with a size limit: past it the file opens as
//! an explanation rather than as a picture.

use std::path::Path;

use lithe_gpu::{Image, Renderer};

/// Largest image that will be decoded.
///
/// Decoding is proportional to pixels, not file size, so the limit is on
/// the dimensions: a 20,000 pixel wide scan is 1.6 GB once expanded to
/// RGBA, and no amount of patience makes that a good idea.
const MAX_PIXELS: u32 = 8192;

pub struct ImageView {
    pub image: Image,
    /// The file's own size, for the caption.
    pub width: u32,
    pub height: u32,
}

/// Whether the preview knows how to show this file.
pub fn is_image(path: &Path) -> bool {
    extension(path).is_some_and(|e| {
        matches!(
            e.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg"
        )
    })
}

pub fn is_svg(path: &Path) -> bool {
    extension(path).is_some_and(|e| e == "svg")
}

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

/// How wide an SVG with no intrinsic size is rasterised.
const SVG_DEFAULT: f32 = 1024.0;

/// Rasterises SVG markup at a given pixel size and uploads it.
///
/// A diagram is drawn as shapes rather than characters, and this is
/// where the shapes become pixels. Sized by the caller rather than by
/// the markup: a figure fills the width it was laid out for, and
/// rasterising at that size is what keeps the strokes crisp instead of
/// scaling a smaller bitmap up.
pub fn rasterise(
    renderer: &Renderer,
    svg: &str,
    width: u32,
    height: u32,
    ink: lithe_gpu::Rgba,
) -> Option<ImageView> {
    let (width, height) = (width.clamp(1, MAX_PIXELS), height.clamp(1, MAX_PIXELS));
    // The shapes are written in `currentColor`, which an SVG outside a
    // document has nothing to resolve against — resvg falls back to
    // black, and black lines on a dark theme are no lines at all. The
    // theme's colour is substituted here, where it is known, rather
    // than baked in where the picture was laid out.
    let svg = svg.replace("currentColor", &hex(ink));
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
        .map_err(|e| log::debug!("diagram will not parse: {e}"))
        .ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(
            width as f32 / size.width(),
            height as f32 / size.height(),
        ),
        &mut pixmap.as_mut(),
    );
    let image = renderer
        .upload_image(&straight(pixmap.take()), width, height)
        .map_err(|e| log::debug!("diagram will not upload: {e}"))
        .ok()?;
    Some(ImageView {
        image,
        width,
        height,
    })
}

/// `#rrggbb` for an SVG attribute. Alpha is left to the stroke's own
/// opacity, which the markup sets.
fn hex(colour: lithe_gpu::Rgba) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b, _] = colour.0;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// tiny-skia gives premultiplied RGBA; the shader blends straight
/// alpha, so this has to be undone or every edge darkens.
fn straight(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for channel in px.iter_mut().take(3) {
                *channel = ((*channel as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    rgba
}

/// Rasterises an SVG and uploads the result.
///
/// Vector art goes down the same path as a photograph: once it is pixels
/// the renderer does not care where they came from, and the alternative
/// is a second drawing pipeline for paths.
fn load_svg(renderer: &Renderer, path: &Path) -> Result<ImageView, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default())
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

    let size = tree.size();
    // Scale up a small icon so it is not a postage stamp, but never past
    // the limit that applies to any other image.
    let scale = (SVG_DEFAULT / size.width().max(size.height())).clamp(
        1.0,
        MAX_PIXELS as f32 / size.width().max(size.height()).max(1.0),
    );
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (size.height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("{width}x{height} is too large to rasterise"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let rgba = straight(pixmap.take());
    let image = renderer
        .upload_image(&rgba, width, height)
        .map_err(|e| format!("{e}"))?;
    Ok(ImageView {
        image,
        width,
        height,
    })
}

/// Decodes a file and uploads it. The error is worth showing: "that is a
/// 30,000 pixel image" is more use than a blank pane.
pub fn load(renderer: &Renderer, path: &Path) -> Result<ImageView, String> {
    if is_svg(path) {
        return load_svg(renderer, path);
    }
    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    // Check the header before decoding, so an absurd image is refused
    // rather than allocated.
    if let Ok((w, h)) = reader.into_dimensions() {
        if w > MAX_PIXELS || h > MAX_PIXELS {
            return Err(format!("image is {w}x{h}, larger than this will show"));
        }
    }

    let decoded = image::ImageReader::open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .decode()
        .map_err(|e| format!("cannot decode {}: {e}", path.display()))?;

    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    let image = renderer
        .upload_image(rgba.as_raw(), width, height)
        .map_err(|e| format!("{e}"))?;

    Ok(ImageView {
        image,
        width,
        height,
    })
}

/// Fits `(w, h)` inside `(max_w, max_h)` without distorting it, and
/// without blowing a small image up past its own size.
pub fn fit(w: u32, h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if w == 0 || h == 0 || max_w <= 0.0 || max_h <= 0.0 {
        return (0.0, 0.0);
    }
    let (w, h) = (w as f32, h as f32);
    let scale = (max_w / w).min(max_h / h).min(1.0);
    ((w * scale).round(), (h * scale).round())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_recognised_whatever_their_case() {
        assert!(is_image(Path::new("a/b/photo.PNG")));
        assert!(is_image(Path::new("x.jpeg")));
        assert!(is_image(Path::new("logo.SVG")));
        assert!(!is_image(Path::new("main.rs")));
        assert!(!is_image(Path::new("noextension")));
    }

    #[test]
    fn svg_is_told_apart_from_a_raster_image() {
        assert!(is_svg(Path::new("logo.svg")));
        assert!(!is_svg(Path::new("logo.png")));
    }

    #[test]
    fn a_large_image_is_scaled_down_to_fit() {
        let (w, h) = fit(4000, 2000, 800.0, 800.0);
        assert_eq!((w, h), (800.0, 400.0), "aspect ratio must hold");
    }

    #[test]
    fn a_small_image_is_left_at_its_own_size() {
        let (w, h) = fit(100, 50, 800.0, 800.0);
        assert_eq!(
            (w, h),
            (100.0, 50.0),
            "an icon should not be blown up to fill the window"
        );
    }

    #[test]
    fn the_tighter_axis_decides() {
        let (w, h) = fit(1000, 1000, 800.0, 200.0);
        assert_eq!((w, h), (200.0, 200.0), "height was the constraint");
    }

    #[test]
    fn degenerate_sizes_do_not_divide_by_zero() {
        assert_eq!(fit(0, 10, 100.0, 100.0), (0.0, 0.0));
        assert_eq!(fit(10, 10, 0.0, 100.0), (0.0, 0.0));
    }
}
