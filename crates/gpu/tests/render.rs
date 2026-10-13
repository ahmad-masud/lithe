//! Render tests against the offscreen target.
//!
//! A hand-rolled text renderer can fail in ways a type checker cannot see:
//! glyphs upside down, off by a baseline, the wrong face, nothing drawn at
//! all. These read the actual pixels back.
//!
//! They need a GPU. Where there is no adapter — a CI container, say — they
//! skip rather than fail, since a missing GPU is not a broken renderer.

use lithe_gpu::{Renderer, Rgba};

const W: u32 = 240;
const H: u32 = 96;
const BG: Rgba = Rgba::hex(0x102030);
const FG: Rgba = Rgba::hex(0xF0E0D0);

fn renderer() -> Option<Renderer> {
    match Renderer::offscreen(W, H, 1.0, 16.0) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("skipping render test, no usable GPU: {e}");
            None
        }
    }
}

fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn close(got: [u8; 4], want: Rgba) -> bool {
    // One step of tolerance for float-to-unorm rounding.
    (0..3).all(|c| {
        let expected = (want.0[c] * 255.0).round() as i32;
        (got[c] as i32 - expected).abs() <= 1
    })
}

#[test]
fn clear_fills_the_whole_target() {
    let Some(mut r) = renderer() else { return };
    r.frame(BG).present();
    let buf = r.read_rgba().expect("readback");

    assert_eq!(buf.len(), (W * H * 4) as usize);
    for (x, y) in [
        (0, 0),
        (W - 1, 0),
        (0, H - 1),
        (W - 1, H - 1),
        (W / 2, H / 2),
    ] {
        assert!(
            close(pixel(&buf, x, y), BG),
            "pixel ({x},{y}) is {:?}, expected the clear colour",
            pixel(&buf, x, y)
        );
    }
}

#[test]
fn rect_lands_where_it_was_asked_to() {
    let Some(mut r) = renderer() else { return };
    {
        let mut f = r.frame(BG);
        f.rect(20.0, 10.0, 40.0, 30.0, FG);
        f.present();
    }
    let buf = r.read_rgba().expect("readback");

    // Inside.
    for (x, y) in [(20, 10), (59, 39), (40, 25)] {
        assert!(
            close(pixel(&buf, x, y), FG),
            "({x},{y}) should be inside the rect, got {:?}",
            pixel(&buf, x, y)
        );
    }
    // Just outside, on every edge.
    for (x, y) in [(19, 25), (60, 25), (40, 9), (40, 40)] {
        assert!(
            close(pixel(&buf, x, y), BG),
            "({x},{y}) should be outside the rect, got {:?}",
            pixel(&buf, x, y)
        );
    }
}

#[test]
fn text_puts_ink_on_its_own_baseline_and_nowhere_else() {
    let Some(mut r) = renderer() else { return };
    let m = r.cell();

    // A monospace face must report a sane advance; a zero or absurd value
    // means family resolution fell through.
    assert!(
        m.advance > 2.0 && m.advance < 64.0,
        "implausible cell advance {}",
        m.advance
    );
    assert!(m.line_height >= m.ascent, "ascent exceeds the line box");

    let baseline = m.ascent;
    {
        let mut f = r.frame(BG);
        f.text(0.0, baseline, "Xy", FG);
        f.present();
    }
    let buf = r.read_rgba().expect("readback");

    let ink_in = |y0: u32, y1: u32, x0: u32, x1: u32| {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| !close(pixel(&buf, x, y), BG))
            .count()
    };

    // Ink exists, inside the first two cells.
    let cells = (m.advance * 2.0).ceil() as u32;
    let drawn = ink_in(0, m.line_height.ceil() as u32, 0, cells);
    assert!(
        drawn > 10,
        "expected glyph coverage, found {drawn} lit pixels"
    );

    // And the rest of the target is untouched: nothing below the line box,
    // nothing to the right of the two cells drawn.
    let below = ink_in(m.line_height.ceil() as u32 + 2, H, 0, W);
    assert_eq!(below, 0, "{below} lit pixels below the text's line box");

    let right = ink_in(0, H, cells + 2, W);
    assert_eq!(right, 0, "{right} lit pixels past the end of the text");
}

#[test]
fn an_image_is_drawn_in_its_own_colours() {
    let Some(mut r) = renderer() else { return };

    // Four distinct, saturated quadrants: a channel being dropped or a
    // coverage path being taken instead shows up immediately.
    const N: u32 = 4;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (top, left) = (y < N / 2, x < N / 2);
            let colour: [u8; 4] = match (top, left) {
                (true, true) => [200, 30, 40, 255],
                (true, false) => [30, 200, 40, 255],
                (false, true) => [30, 40, 200, 255],
                (false, false) => [20, 22, 27, 255],
            };
            rgba.extend_from_slice(&colour);
        }
    }
    let image = r.upload_image(&rgba, N, N).expect("upload");

    {
        let mut f = r.frame(BG);
        // Draw text too, so the batching has to switch textures and
        // switch back rather than only ever drawing one thing.
        f.text(0.0, 20.0, "before", FG);
        f.image(&image, 40.0, 40.0, 40.0, 40.0);
        f.text(0.0, 90.0, "after", FG);
        f.present();
    }
    let buf = r.read_rgba().expect("readback");

    // The image's own colours, unmodified: not tinted, not reduced to
    // coverage, not gamma-shifted.
    for (x, y, want) in [
        (45, 45, [200u8, 30, 40]),
        (75, 45, [30, 200, 40]),
        (45, 75, [30, 40, 200]),
        (75, 75, [20, 22, 27]),
    ] {
        let got = pixel(&buf, x, y);
        assert_eq!(
            &got[..3],
            &want[..],
            "image pixel at ({x},{y}) came out as {got:?}, wanted {want:?}"
        );
    }

    // And the text either side of it still drew.
    let ink = (0..W)
        .flat_map(|x| (0..30u32).map(move |y| (x, y)))
        .filter(|&(x, y)| !close(pixel(&buf, x, y), BG))
        .count();
    assert!(ink > 10, "the text before the image did not draw");
}

/// Prose is proportional and the grid is not, which is the whole
/// difference between the preview reading as a document and reading as a
/// terminal. An `i` must be narrower than an `M` in one and exactly as
/// wide in the other.
#[test]
fn a_prose_face_is_proportional_and_the_grid_is_not() {
    use lithe_gpu::{Face, Family, Size};

    let Some(r) = renderer() else { return };

    let mono = |text: &str| r.measure(text, Face::Regular, Size::Body, Family::Mono);
    let prose = |text: &str| r.measure(text, Face::Regular, Size::Body, Family::Prose);

    assert_eq!(mono("i"), mono("M"), "a grid cell is a grid cell");
    assert!(
        prose("i") > 0.0 && prose("i") < prose("M"),
        "an i is narrower than an M: {} vs {}",
        prose("i"),
        prose("M")
    );
    // A space has no ink and still moves the pen; without this every
    // proportional line would be drawn with its words run together.
    assert!(prose(" ") > 0.0, "a space has a width");
    assert_eq!(
        prose("a b"),
        prose("a") + prose(" ") + prose("b"),
        "measuring a run is measuring its glyphs"
    );

    // Italic exists as its own face rather than as slanted body text.
    let italic = r.measure("word", Face::Italic, Size::Body, Family::Prose);
    assert!(italic > 0.0);
}
