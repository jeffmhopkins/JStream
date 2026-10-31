//! ADR-0081's blend modes, against a CPU reference of the W3C compositing formulas in
//! premultiplied linear light (sRGB-encoded for overlay, difference and exclusion), on
//! opaque and translucent backdrops and sources.

use jstream_media::FrameSize;
use jstream_protocol::{BlendMode, Finite32, ItemGeometry, Size, Vec2};

use super::{item, near, open, solid};
use crate::color::srgb_channel_to_linear;
use crate::{Color, Crop, Image, Item, LegacyTransform, Placement, Scene, Snapshot};

/// The 8-bit sRGB steps a result may be off by: FP16 working texels and the 8-bit encode.
const TOLERANCE: u8 = 2;

fn encode(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn decode(c: f64) -> f64 {
    f64::from(srgb_channel_to_linear(c as f32))
}

/// Whether `mode` mixes sRGB-encoded values (ADR-0081 decision 2).
fn in_srgb(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Overlay | BlendMode::Difference | BlendMode::Exclusion
    )
}

/// B(cs, cb), straight, in the space the mode mixes in.
fn mixed(mode: BlendMode, cs: f64, cb: f64) -> f64 {
    match mode {
        BlendMode::Darken => cs.min(cb),
        BlendMode::Lighten => cs.max(cb),
        BlendMode::Overlay if cb <= 0.5 => 2.0 * cs * cb,
        BlendMode::Overlay => 1.0 - 2.0 * (1.0 - cs) * (1.0 - cb),
        BlendMode::Difference => (cs - cb).abs(),
        BlendMode::Exclusion => cs + cb - 2.0 * cs * cb,
        BlendMode::Multiply => cs * cb,
        BlendMode::Screen => cs + cb - cs * cb,
        _ => cs,
    }
}

/// The resolved straight sRGB pixel of `source` (straight sRGB 8-bit, times `opacity`)
/// composited with `mode` over `backdrop` (straight sRGB in [0, 1], alpha linear).
fn reference(mode: BlendMode, source: [u8; 4], opacity: f32, backdrop: Color) -> [u8; 4] {
    let sa = f64::from(source[3]) / 255.0 * f64::from(opacity);
    let ba = f64::from(backdrop.a);
    let back = [backdrop.r, backdrop.g, backdrop.b].map(|c| decode(f64::from(c)));
    let src = [source[0], source[1], source[2]].map(|c| decode(f64::from(c) / 255.0));
    let alpha = if mode == BlendMode::Additive {
        sa + ba * (1.0 - sa)
    } else {
        sa + ba - sa * ba
    };
    let mut out = [0u8; 4];
    for channel in 0..3 {
        let (s, b) = (src[channel] * sa, back[channel] * ba);
        let premultiplied = match mode {
            BlendMode::Normal => s + b * (1.0 - sa),
            BlendMode::Additive => s + b,
            _ => {
                let (cs, cb) = (src[channel], back[channel]);
                let mix = if in_srgb(mode) {
                    decode(mixed(mode, encode(cs), encode(cb)))
                } else {
                    mixed(mode, cs, cb)
                };
                s * (1.0 - ba) + b * (1.0 - sa) + sa * ba * mix
            }
        };
        let straight = if alpha > 0.0 {
            premultiplied / alpha
        } else {
            0.0
        };
        out[channel] = (encode(straight) * 255.0).round() as u8;
    }
    out[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    out
}

fn blended<'a>(image: &'a Image, placement: Placement, opacity: f32, blend: BlendMode) -> Item<'a> {
    Item {
        blend,
        ..item(image, placement, Crop::NONE, opacity)
    }
}

fn render(
    compositor: &mut crate::Compositor,
    size: (u32, u32),
    background: Color,
    items: &[Item<'_>],
) -> Snapshot {
    let scene = Scene {
        canvas: FrameSize::new(size.0, size.1).expect("non-zero"),
        background,
        items,
    };
    compositor
        .render(&scene, true)
        .expect("render")
        .pixels()
        .expect("readback was requested")
        .clone()
}

/// Every mode, over every backdrop, from every source, matches the reference: an
/// opaque, a translucent and a fully transparent backdrop, and opaque, faded and
/// translucent sources, including values each side of overlay's midpoint.
#[test]
fn every_mode_matches_the_reference() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let backdrops = [
        Color::rgb(0.8, 0.3, 0.1),
        Color::rgb(0.2, 0.6, 0.9),
        Color {
            r: 0.5,
            g: 0.9,
            b: 0.25,
            a: 0.5,
        },
        Color::TRANSPARENT,
    ];
    let sources: [([u8; 4], f32); 4] = [
        ([200, 40, 120, 255], 1.0),
        ([90, 220, 30, 255], 0.6),
        ([250, 128, 10, 128], 1.0),
        ([30, 30, 230, 200], 0.5),
    ];
    for mode in BlendMode::ALL {
        for backdrop in backdrops {
            for (rgba, opacity) in sources {
                let image = solid(compositor, 2, 2, rgba);
                let items = [blended(&image, Placement::IDENTITY, opacity, mode)];
                let shot = render(compositor, (4, 4), backdrop, &items);
                let expected = reference(mode, rgba, opacity, backdrop);
                let got = shot.pixel(0, 0).expect("inside");
                let close = (0..4).all(|c| got[c].abs_diff(expected[c]) <= TOLERANCE);
                // A fully transparent result has no colour to compare.
                let empty = expected[3] == 0 && got[3] == 0;
                assert!(
                    close || empty,
                    "{} of {rgba:?} at {opacity} over {backdrop:?}: {got:?}, expected \
                     {expected:?}",
                    mode.as_str()
                );
                // Outside the item the backdrop is left as it was.
                let untouched = reference(BlendMode::Normal, [0, 0, 0, 0], 0.0, backdrop);
                let outside = shot.pixel(3, 3).expect("inside");
                if untouched[3] > 0 {
                    near(&shot, 3, 3, untouched, TOLERANCE);
                } else {
                    assert_eq!(outside[3], 0, "{}", mode.as_str());
                }
            }
        }
    }
}

/// Items in a row that read the backdrop each see the ones before: the second difference
/// undoes the first, and a normal item between them is drawn in order. The first item
/// reads the cleared background.
#[test]
fn modes_reading_the_backdrop_stack_in_order() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let white = solid(compositor, 4, 4, [255, 255, 255, 255]);
    let grey = solid(compositor, 2, 2, [128, 128, 128, 255]);
    let background = Color::rgb(0.25, 0.5, 0.75);
    let items = [
        blended(&white, Placement::IDENTITY, 1.0, BlendMode::Difference),
        blended(&grey, Placement::IDENTITY, 1.0, BlendMode::Normal),
        blended(&white, Placement::IDENTITY, 1.0, BlendMode::Difference),
    ];
    let shot = render(compositor, (4, 4), background, &items);
    // White minus the background, then white minus that: the background, in sRGB.
    near(&shot, 3, 3, [64, 128, 191, 255], TOLERANCE);
    // Under the grey: white minus grey.
    near(&shot, 0, 0, [127, 127, 127, 255], TOLERANCE);
}

/// An item partly off the canvas copies only what is on it, and one wholly off draws
/// nothing and fails nothing.
#[test]
fn a_backdrop_item_off_the_canvas_is_clipped() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let white = solid(compositor, 4, 4, [255, 255, 255, 255]);
    let at = |x: f32, y: f32| {
        Placement::Legacy(LegacyTransform {
            position: [x, y],
            ..LegacyTransform::IDENTITY
        })
    };
    let background = Color::rgb(0.0, 0.0, 0.0);
    let items = [
        blended(&white, at(-2.0, -2.0), 1.0, BlendMode::Difference),
        blended(&white, at(50.0, 50.0), 1.0, BlendMode::Exclusion),
        blended(&white, at(4.0, -3.0), 1.0, BlendMode::Lighten),
    ];
    let shot = render(compositor, (6, 6), background, &items);
    near(&shot, 0, 0, [255, 255, 255, 255], 0);
    near(&shot, 1, 1, [255, 255, 255, 255], 0);
    near(&shot, 2, 2, [0, 0, 0, 255], 0);
    near(&shot, 5, 0, [255, 255, 255, 255], 0);
    near(&shot, 5, 1, [0, 0, 0, 255], 0);
}

/// A layer copies its own texels: a nested scene drawn into one blends with its own
/// (transparent) background, and a layer smaller than the backdrop an earlier frame
/// grew reuses it.
#[test]
fn a_layer_blends_with_its_own_texels() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let red = solid(compositor, 8, 8, [255, 0, 0, 255]);
    let green = solid(compositor, 2, 2, [0, 255, 0, 255]);
    // Grows the backdrop to 8x8 first.
    let big = [
        blended(&red, Placement::IDENTITY, 1.0, BlendMode::Normal),
        blended(&green, Placement::IDENTITY, 1.0, BlendMode::Lighten),
    ];
    let shot = render(compositor, (8, 8), Color::BLACK, &big);
    near(&shot, 0, 0, [255, 255, 0, 255], TOLERANCE);

    let layer = compositor.layer(4, 4).expect("layer");
    let inner = [
        blended(&red, Placement::IDENTITY, 1.0, BlendMode::Normal),
        blended(&green, Placement::IDENTITY, 1.0, BlendMode::Darken),
    ];
    let scene = Scene {
        canvas: FrameSize::new(4, 4).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &inner,
    };
    compositor.render_into(&scene, &layer).expect("render into");
    let outer = [blended(&layer, Placement::IDENTITY, 1.0, BlendMode::Normal)];
    let shot = render(compositor, (4, 4), Color::BLACK, &outer);
    // Darken of green over red is black, inside the layer.
    near(&shot, 0, 0, [0, 0, 0, 255], TOLERANCE);
    near(&shot, 3, 3, [255, 0, 0, 255], TOLERANCE);
}

/// A 3D item reads the backdrop under its projected polygon, edge fringe included.
#[test]
fn a_3d_item_reads_the_backdrop_under_its_polygon() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let white = solid(compositor, 16, 16, [255, 255, 255, 255]);
    let finite = |v: f32| Finite32::new(v).expect("finite");
    let geometry = ItemGeometry {
        turn_deg: finite(40.0),
        perspective_px: Some(finite(200.0)),
        ..ItemGeometry::try_new(
            Vec2::new(16.0, 16.0).expect("finite"),
            Size::try_new(24.0, 24.0).expect("positive"),
        )
        .expect("valid")
    };
    let normal = [blended(
        &white,
        Placement::Geometry(geometry),
        1.0,
        BlendMode::Normal,
    )];
    let difference = [blended(
        &white,
        Placement::Geometry(geometry),
        1.0,
        BlendMode::Difference,
    )];
    let background = Color::rgb(1.0, 1.0, 1.0);
    let over = render(compositor, (32, 32), Color::BLACK, &normal);
    let diff = render(compositor, (32, 32), background, &difference);
    // Difference of white over white is black where the item covers, and the coverage
    // a normal draw gives each pixel shows as how far it went toward black.
    for y in 0..32 {
        for x in 0..32 {
            let covered = over.pixel(x, y).expect("inside")[0];
            let left = diff.pixel(x, y).expect("inside")[0];
            let expected_lin = 1.0 - f64::from(srgb_channel_to_linear(f32::from(covered) / 255.0));
            let expected = (encode(expected_lin) * 255.0).round() as u8;
            assert!(
                left.abs_diff(expected) <= 3,
                "({x}, {y}): {left}, expected {expected} from coverage {covered}"
            );
        }
    }
}
