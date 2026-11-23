//! A nested scene's layer drawn only over what its scene drew (#1070): the same pixels as
//! the whole layer, with fewer of them shaded.

use std::sync::Arc;

use jstream_media::FrameSize;
use jstream_protocol::{BlendMode, Blending};
use serde_json::json;

use super::{Opened, item, near, open, solid};
use crate::filter::testing::{filter, image, read};
use crate::mips::Region;
use crate::{
    Color, Compositor, Crop, FilterClock, Image, Item, LegacyTransform, Placement, Scene, Snapshot,
    VideoFilter,
};

fn at(position: [f32; 2], scale: [f32; 2], rotation_deg: f32) -> Placement {
    Placement::Legacy(LegacyTransform {
        position,
        scale,
        rotation_deg,
    })
}

/// A 32x32 transparent scene with a red square at (5, 6) and a half-opaque green bar at
/// (20, 25), drawn into a layer.
fn sparse(compositor: &mut Compositor) -> Image {
    let red = solid(compositor, 2, 2, [255, 0, 0, 255]);
    let green = solid(compositor, 3, 1, [0, 255, 0, 128]);
    let items = [
        item(&red, at([5.0, 6.0], [1.0, 1.0], 0.0), Crop::NONE, 1.0),
        item(&green, at([20.0, 25.0], [2.0, 2.0], 0.0), Crop::NONE, 1.0),
    ];
    let layer = compositor.layer(32, 32).expect("layer");
    compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(32, 32).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &items,
            },
            &layer,
        )
        .expect("render into the layer");
    layer
}

/// The layer, rotated and scaled, over a varied backdrop in a 48x48 scene, and how many
/// texels it reaches on its own.
fn placed(compositor: &mut Compositor, layer: &Image, blend: BlendMode) -> (Snapshot, u32) {
    let backdrop = compositor
        .upload_rgba8(
            2,
            2,
            &[
                200, 40, 10, 255, 10, 90, 200, 255, //
                30, 160, 60, 255, 240, 220, 30, 255,
            ],
        )
        .expect("backdrop");
    let mut over = item(layer, at([9.0, 2.0], [1.2, 1.1], 17.0), Crop::NONE, 0.8);
    over.blend = blend;
    let items = [
        item(
            &backdrop,
            at([0.0, 0.0], [24.0, 24.0], 0.0),
            Crop::NONE,
            1.0,
        ),
        over,
    ];
    let mut scene = Scene {
        canvas: FrameSize::new(48, 48).expect("invariant: non-zero"),
        background: Color::BLACK,
        items: &items,
    };
    let pixels = compositor
        .render(&scene, true)
        .expect("render")
        .pixels()
        .expect("invariant: readback was requested")
        .clone();
    scene.items = &items[1..];
    compositor
        .render(&scene, false)
        .expect("render the layer alone");
    (pixels, compositor.drawn_texels())
}

/// The layer knows what its scene drew, a pixel past each item, and is drawn only over
/// that, to the same pixels as drawing all of it, in each mode that trims.
#[test]
fn a_layer_is_drawn_only_over_what_its_scene_drew() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let layer = sparse(compositor);
    let content = Region {
        x: 4,
        y: 5,
        width: 23,
        height: 23,
    };
    assert_eq!(layer.content(), Some(content));
    for blend in [BlendMode::Normal, BlendMode::Additive, BlendMode::Screen] {
        let (trimmed, trimmed_reach) = placed(compositor, &layer, blend);
        layer.set_content(None);
        let (whole, whole_reach) = placed(compositor, &layer, blend);
        layer.set_content(Some(content));
        assert!(
            trimmed_reach < whole_reach,
            "{blend:?}: the trimmed layer reaches {trimmed_reach} texels, the whole {whole_reach}"
        );
        for (index, (a, b)) in trimmed.rgba().iter().zip(whole.rgba()).enumerate() {
            assert!(
                a.abs_diff(*b) <= 1,
                "{blend:?}: byte {index} is {a} trimmed and {b} whole"
            );
        }
    }
}

/// An opaque background fills the layer, so nothing is trimmed; a scene that draws
/// nothing leaves a layer that draws nothing; any later write forgets what was drawn.
#[test]
fn a_layer_forgets_its_content_when_it_may_be_anywhere() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let canvas = FrameSize::new(8, 8).expect("invariant: non-zero");
    let layer = compositor.layer(8, 8).expect("layer");
    compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::WHITE,
                items: &[],
            },
            &layer,
        )
        .expect("an opaque layer");
    assert_eq!(layer.content(), None);

    compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::TRANSPARENT,
                items: &[],
            },
            &layer,
        )
        .expect("an empty layer");
    let empty = Region {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    assert_eq!(layer.content(), Some(empty));
    let items = [item(&layer, Placement::IDENTITY, Crop::NONE, 1.0)];
    compositor
        .render(
            &Scene {
                canvas,
                background: Color::BLACK,
                items: &items,
            },
            false,
        )
        .expect("render");
    assert_eq!(compositor.drawn_texels(), 0, "an empty layer draws nothing");

    layer.mark_changed();
    assert_eq!(layer.content(), None, "a write after it forgets it");
}

/// A 96x96 transparent scene with a red square at (10, 12) and a half-opaque green bar
/// at (30, 20), drawn into a layer: most of it is empty.
fn cornered(compositor: &mut Compositor) -> Image {
    let red = solid(compositor, 3, 3, [255, 0, 0, 255]);
    let green = solid(compositor, 4, 1, [0, 255, 0, 128]);
    let items = [
        item(&red, at([10.0, 12.0], [1.0, 1.0], 0.0), Crop::NONE, 1.0),
        item(&green, at([30.0, 20.0], [2.0, 2.0], 0.0), Crop::NONE, 1.0),
    ];
    let layer = compositor.layer(96, 96).expect("layer");
    compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(96, 96).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &items,
            },
            &layer,
        )
        .expect("render into the layer");
    layer
}

/// `filter` over `input` into a new layer, and that layer.
fn filtered(opened: &mut Opened, filter: &mut VideoFilter, input: &Image) -> Image {
    let output = opened
        .compositor
        .layer(input.width(), input.height())
        .expect("layer");
    opened
        .compositor
        .apply_filter_into(
            filter,
            input,
            &output,
            FilterClock {
                seconds: 0.0,
                beat: 0.0,
            },
        )
        .expect("apply the filter");
    output
}

fn same(kind: &str, bounded: &Snapshot, whole: &Snapshot) {
    let pixels = bounded.rgba().chunks(4).zip(whole.rgba().chunks(4));
    for (index, (a, b)) in pixels.enumerate() {
        // A readback un-premultiplies, so where alpha is a code or two the colour is
        // noise: compare the colours as they show, times their alpha.
        let shown =
            |pixel: &[u8], channel: usize| f32::from(pixel[channel]) * f32::from(pixel[3]) / 255.0;
        let near = a[3].abs_diff(b[3]) <= 1
            && (0..3).all(|channel| (shown(a, channel) - shown(b, channel)).abs() <= 1.5);
        let (x, y) = (
            index % bounded.width() as usize,
            index / bounded.width() as usize,
        );
        assert!(
            near,
            "{kind}: pixel ({x}, {y}) is {a:?} bounded and {b:?} whole"
        );
    }
}

/// A blur, a mask, a shadow or a glow over a layer whose content is known draws only near
/// it, to the same pixels as over all of it, and says where that is, so the next filter of a chain and
/// the scene placing the result are bounded too.
#[test]
fn a_filter_over_a_bounded_layer_draws_only_near_its_content() {
    let Some(mut opened) = open() else {
        return;
    };
    let layer = cornered(&mut opened.compositor);
    let content = layer
        .content()
        .expect("a transparent scene's layer is bounded");
    let gradient: Vec<[u8; 4]> = (0..16u8)
        .map(|i| [i * 16, 255 - i * 16, 128, 255])
        .collect();
    let mask = Arc::new(image(&opened, 4, &gradient));
    let kinds = [
        ("blur", json!({ "radius": 4.0 })),
        ("blur", json!({ "radius": 12.0 })),
        (
            "blur",
            json!({ "radius": 6.0, "area": "inside", "area_x": 0.1, "area_width": 0.2 }),
        ),
        (
            "image_mask",
            json!({ "image": {"type": "asset", "root": "app", "path": "m.png"}, "mode": "luma" }),
        ),
        (
            "scene_mask",
            json!({ "scene": {"type": "scene", "scene_id": "scn_mask"}, "mode": "alpha", "invert": true }),
        ),
        (
            "drop_shadow",
            json!({ "color": "#0000ffc0", "offset_x": 6.0, "offset_y": -4.0, "blur": 5.0 }),
        ),
        (
            "glow",
            json!({ "color": "#ffff00", "radius": 10.0, "strength": 2.0 }),
        ),
    ];
    for (kind, settings) in kinds {
        let make = || {
            let mut made = filter(kind, settings.clone());
            made.set_reference(Some(Arc::clone(&mask)));
            made
        };
        let bounded = filtered(&mut opened, &mut make(), &layer);
        let reach = bounded.content().expect("a bounded kind keeps the bound");
        assert!(
            reach.width < 96 && reach.height < 96,
            "{kind}: bounded to {reach:?}"
        );
        assert!(reach.x <= content.x && reach.x + reach.width >= content.x + content.width);
        // The next filter reads the bound the first one left.
        let chained = filtered(&mut opened, &mut make(), &bounded);
        assert!(chained.content().is_some(), "{kind}: a chain stays bounded");
        let (bounded, chained) = (read(&mut opened, &bounded), read(&mut opened, &chained));

        layer.set_content(None);
        let whole = filtered(&mut opened, &mut make(), &layer);
        assert_eq!(whole.content(), None, "{kind}: an unbounded input");
        let whole_chained = filtered(&mut opened, &mut make(), &whole);
        let (whole, whole_chained) = (read(&mut opened, &whole), read(&mut opened, &whole_chained));
        layer.set_content(Some(content));
        same(kind, &bounded, &whole);
        same(kind, &chained, &whole_chained);
    }
}

/// A filter that can draw anywhere doesn't keep the bound; one over an empty layer
/// draws nothing and stays empty.
#[test]
fn a_filter_keeps_the_bound_only_when_it_can() {
    let Some(mut opened) = open() else {
        return;
    };
    let layer = cornered(&mut opened.compositor);
    let shifted = filtered(
        &mut opened,
        &mut filter("scroll", json!({ "speed_x": 10.0 })),
        &layer,
    );
    assert_eq!(shifted.content(), None, "scroll moves its input anywhere");

    let empty = opened.compositor.layer(16, 16).expect("layer");
    opened
        .compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(16, 16).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &[],
            },
            &empty,
        )
        .expect("an empty layer");
    let blurred = filtered(&mut opened, &mut filter("blur", json!({})), &empty);
    let nothing = Region {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    assert_eq!(blurred.content(), Some(nothing));
    let pixels = read(&mut opened, &blurred);
    assert!(pixels.rgba().iter().all(|&byte| byte == 0));
}

/// A strong glow of a small square near the right edge of a wide frame, far from its
/// origin, draws the same bounded as whole. Its flood and its light run in the output's
/// frame, so the bound needs no moving; a drop shadow's blur, in a grown frame, moves
/// it there (`Bound::placed`, checked by the kinds above).
#[test]
fn a_glow_near_the_far_edge_of_a_wide_frame_draws_the_same_bounded() {
    let Some(mut opened) = open() else {
        return;
    };
    let square = solid(&opened.compositor, 4, 4, [255, 0, 0, 255]);
    let items = [item(
        &square,
        at([1000.0, 30.0], [1.0, 1.0], 0.0),
        Crop::NONE,
        1.0,
    )];
    let layer = opened.compositor.layer(1024, 64).expect("layer");
    opened
        .compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(1024, 64).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &items,
            },
            &layer,
        )
        .expect("render into the layer");
    let content = layer.content().expect("bounded");
    let make = || {
        filter(
            "glow",
            json!({ "color": "#ffffff", "radius": 16.0, "strength": 8.0 }),
        )
    };
    let bounded = filtered(&mut opened, &mut make(), &layer);
    assert!(bounded.content().is_some(), "a glow keeps the bound");
    let bounded = read(&mut opened, &bounded);
    layer.set_content(None);
    let whole = filtered(&mut opened, &mut make(), &layer);
    let whole = read(&mut opened, &whole);
    layer.set_content(Some(content));
    same("glow", &bounded, &whole);
}

/// Every texel of `image`, not only those its content says can hold anything, which a
/// read draws alone.
fn whole(opened: &mut Opened, image: &Image) -> Snapshot {
    let content = image.content();
    image.set_content(None);
    let pixels = read(opened, image);
    image.set_content(content);
    pixels
}

/// Draws a red square at `at` and a half-opaque green bar below it into `layer`, a
/// 96x96 transparent scene.
fn moved(compositor: &mut Compositor, layer: &Image, at_x: f32, at_y: f32) {
    let red = solid(compositor, 3, 3, [255, 0, 0, 255]);
    let green = solid(compositor, 4, 1, [0, 255, 0, 128]);
    let items = [
        item(&red, at([at_x, at_y], [1.0, 1.0], 0.0), Crop::NONE, 1.0),
        item(
            &green,
            at([at_x, at_y + 9.0], [2.0, 2.0], 0.0),
            Crop::NONE,
            1.0,
        ),
    ];
    compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(96, 96).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &items,
            },
            layer,
        )
        .expect("render into the layer");
}

/// A layer drawn again after its content moved, or after it emptied, is made
/// transparent where it was rather than cleared whole (#1449), and holds the same pixels
/// as a new layer drawn once.
#[test]
fn a_layer_drawn_again_keeps_nothing_of_what_it_held() {
    let Some(mut opened) = open() else {
        return;
    };
    let reused = opened.compositor.layer(96, 96).expect("layer");
    moved(&mut opened.compositor, &reused, 10.0, 12.0);
    assert!(reused.content().is_some());
    moved(&mut opened.compositor, &reused, 60.0, 50.0);
    let fresh = opened.compositor.layer(96, 96).expect("layer");
    moved(&mut opened.compositor, &fresh, 60.0, 50.0);
    let (again, once) = (whole(&mut opened, &reused), whole(&mut opened, &fresh));
    assert_eq!(
        again.rgba(),
        once.rgba(),
        "the moved content leaves nothing behind"
    );

    opened
        .compositor
        .render_into(
            &Scene {
                canvas: FrameSize::new(96, 96).expect("invariant: non-zero"),
                background: Color::TRANSPARENT,
                items: &[],
            },
            &reused,
        )
        .expect("an empty scene");
    let emptied = whole(&mut opened, &reused);
    assert!(
        emptied.rgba().iter().all(|&byte| byte == 0),
        "an emptied layer"
    );
}

/// A filter drawn again into the same output and scratch after its input's content
/// moved keeps nothing of the last frame: the same pixels as a new filter into a new
/// output, for each kind that bounds what it draws.
#[test]
fn a_bounded_filter_drawn_again_keeps_nothing_of_the_last_frame() {
    let Some(mut opened) = open() else {
        return;
    };
    let kinds = [
        ("blur", json!({ "radius": 6.0 })),
        (
            "drop_shadow",
            json!({ "color": "#0000ffc0", "offset_x": 6.0, "offset_y": -4.0, "blur": 5.0 }),
        ),
        (
            "glow",
            json!({ "color": "#ffff00", "radius": 8.0, "strength": 2.0 }),
        ),
    ];
    let input = opened.compositor.layer(96, 96).expect("layer");
    for (kind, settings) in kinds {
        let mut reused = filter(kind, settings.clone());
        moved(&mut opened.compositor, &input, 10.0, 12.0);
        let output = filtered(&mut opened, &mut reused, &input);
        assert!(output.content().is_some(), "{kind}: bounded");
        moved(&mut opened.compositor, &input, 60.0, 50.0);
        opened
            .compositor
            .apply_filter_into(
                &mut reused,
                &input,
                &output,
                FilterClock {
                    seconds: 0.0,
                    beat: 0.0,
                },
            )
            .expect("apply the filter again");
        let fresh = filtered(&mut opened, &mut filter(kind, settings), &input);
        let (again, once) = (whole(&mut opened, &output), whole(&mut opened, &fresh));
        same(kind, &again, &once);
    }
}

/// The 32x32 scene that is `layer` alone, and the same with a transparent texel drawn
/// over it so it is composited the long way: (straight from the layer, composited).
fn alone_and_composited(
    compositor: &mut Compositor,
    layer: &Image,
    background: Color,
) -> (Snapshot, Snapshot) {
    let nothing = solid(compositor, 1, 1, [0, 0, 0, 0]);
    let items = [
        item(layer, Placement::IDENTITY, Crop::NONE, 1.0),
        item(&nothing, Placement::IDENTITY, Crop::NONE, 1.0),
    ];
    let mut draw = |items: &[Item<'_>], straight: bool| {
        let scene = Scene {
            canvas: FrameSize::new(32, 32).expect("invariant: non-zero"),
            background,
            items,
        };
        let output = compositor.render(&scene, true).expect("render");
        let pixels = output.pixels().expect("invariant: read back").clone();
        assert_eq!(compositor.resolved_layer(), straight);
        pixels
    };
    (draw(&items[..1], true), draw(&items, false))
}

/// A scene that is one finished layer over the whole canvas is resolved straight from
/// the layer, to the pixels the composite of it gives, in both blending spaces and over
/// each kind of background.
#[test]
fn a_scene_of_one_whole_layer_is_resolved_straight_from_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    for blending in [Blending::Srgb, Blending::Linear] {
        compositor.set_blending(blending);
        let layer = sparse(compositor);
        for background in [
            Color::TRANSPARENT,
            Color::BLACK,
            Color::rgb(0.2, 0.5, 0.9),
            Color::from_rgba8([200, 40, 10, 128]),
        ] {
            let (straight, composited) = alone_and_composited(compositor, &layer, background);
            for y in 0..32 {
                for x in 0..32 {
                    let expected = composited.pixel(x, y).expect("invariant: inside");
                    near(&straight, x, y, expected, 1);
                }
            }
            // The layer's content is there: its red square and its bar.
            assert_eq!(straight.pixel(5, 6), Some([255, 0, 0, 255]));
            assert_ne!(straight.pixel(21, 25), straight.pixel(0, 0));
        }
    }
}

/// Anything but one unfaded, unmoved, canvas-sized layer in `normal` is composited.
#[test]
fn a_layer_that_is_not_the_whole_scene_is_composited() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let layer = sparse(compositor);
    let whole = item(&layer, Placement::IDENTITY, Crop::NONE, 1.0);
    let mut screened = whole;
    screened.blend = BlendMode::Screen;
    let others = [
        // Faded, moved, cropped, scaled and blended.
        item(&layer, Placement::IDENTITY, Crop::NONE, 0.5),
        item(&layer, at([1.0, 0.0], [1.0, 1.0], 0.0), Crop::NONE, 1.0),
        item(
            &layer,
            Placement::IDENTITY,
            Crop {
                left: 1,
                ..Crop::NONE
            },
            1.0,
        ),
        item(&layer, at([0.0, 0.0], [2.0, 2.0], 0.0), Crop::NONE, 1.0),
        screened,
    ];
    for (case, other) in others.into_iter().enumerate() {
        let scene = Scene {
            canvas: FrameSize::new(32, 32).expect("invariant: non-zero"),
            background: Color::BLACK,
            items: &[other],
        };
        compositor.render(&scene, false).expect("render");
        assert!(!compositor.resolved_layer(), "case {case}");
    }
    // And a layer of another size than the canvas.
    let scene = Scene {
        canvas: FrameSize::new(16, 16).expect("invariant: non-zero"),
        background: Color::BLACK,
        items: &[whole],
    };
    compositor.render(&scene, false).expect("render");
    assert!(!compositor.resolved_layer());
}

/// In browser-style blending a layer over a transparent background is decoded only where
/// its items reached, and not at all when it drew nothing: the rest is zero, which
/// decodes to itself. Its pixels are those of a whole decode, and a redraw leaves nothing
/// of what the layer held before.
#[test]
fn a_browser_style_layer_is_decoded_only_where_it_drew() {
    let Some(Opened { mut compositor, .. }) = open() else {
        return;
    };
    compositor.set_blending(jstream_protocol::Blending::Srgb);
    let layer = sparse(&mut compositor);
    let reached = compositor.drawn_texels();
    assert!(reached > 0 && reached < 32 * 32, "{reached} texels reached");
    assert_eq!(compositor.decoded_texels(true, (32, 32)), reached);
    assert_eq!(compositor.decoded_texels(false, (32, 32)), 32 * 32);
    let shown = |compositor: &mut Compositor| {
        let items = [item(&layer, Placement::IDENTITY, Crop::NONE, 1.0)];
        compositor
            .render(
                &Scene {
                    canvas: FrameSize::new(32, 32).expect("invariant: non-zero"),
                    background: Color::BLACK,
                    items: &items,
                },
                true,
            )
            .expect("render")
            .pixels()
            .expect("invariant: readback was requested")
            .clone()
    };
    let shot = shown(&mut compositor);
    super::near(&shot, 5, 6, [255, 0, 0, 255], 1);
    // Half-opaque green over black, mixed encoded: half its code value.
    super::near(&shot, 22, 25, [0, 128, 0, 255], 2);
    super::near(&shot, 15, 15, [0, 0, 0, 255], 0);
    super::near(&shot, 0, 0, [0, 0, 0, 255], 0);

    // Drawn again with one item elsewhere: the old boxes are wiped, and stay zero
    // without a decode over them.
    let blue = solid(&compositor, 2, 2, [0, 0, 255, 255]);
    let items = [item(
        &blue,
        at([12.0, 12.0], [1.0, 1.0], 0.0),
        Crop::NONE,
        1.0,
    )];
    let canvas = FrameSize::new(32, 32).expect("invariant: non-zero");
    compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::TRANSPARENT,
                items: &items,
            },
            &layer,
        )
        .expect("render into the layer again");
    assert!(compositor.decoded_texels(true, (32, 32)) <= 16);
    let shot = shown(&mut compositor);
    super::near(&shot, 12, 12, [0, 0, 255, 255], 1);
    super::near(&shot, 5, 6, [0, 0, 0, 255], 0);
    super::near(&shot, 22, 25, [0, 0, 0, 255], 0);

    // And with nothing: no decode at all.
    compositor
        .render_into(
            &Scene {
                canvas,
                background: Color::TRANSPARENT,
                items: &[],
            },
            &layer,
        )
        .expect("an empty layer");
    assert_eq!(compositor.decoded_texels(true, (32, 32)), 0);
    let shot = shown(&mut compositor);
    super::near(&shot, 12, 12, [0, 0, 0, 255], 0);
}

/// In browser-style blending a frame is resolved from its encoded target as it is, to
/// the pixels the same scene gives when it is drawn into a layer, decoded there and
/// shown: over a background and over none, with half-opaque items and a backdrop mode.
#[test]
fn a_browser_style_frame_is_resolved_from_its_encoded_target() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    compositor.set_blending(Blending::Srgb);
    let red = solid(compositor, 2, 2, [255, 0, 0, 255]);
    let green = solid(compositor, 3, 1, [0, 255, 0, 128]);
    let blue = solid(compositor, 2, 2, [40, 90, 255, 200]);
    let mut over = item(&blue, at([10.0, 4.0], [8.0, 8.0], 0.0), Crop::NONE, 0.7);
    over.blend = BlendMode::Overlay;
    let items = [
        item(&red, at([5.0, 6.0], [6.0, 6.0], 0.0), Crop::NONE, 1.0),
        item(&green, at([8.0, 9.0], [6.0, 6.0], 0.0), Crop::NONE, 1.0),
        over,
    ];
    let canvas = FrameSize::new(32, 32).expect("invariant: non-zero");
    for background in [
        Color::TRANSPARENT,
        Color::BLACK,
        Color::rgb(0.2, 0.5, 0.9),
        Color::from_rgba8([200, 40, 10, 128]),
    ] {
        let scene = Scene {
            canvas,
            background,
            items: &items,
        };
        let output = compositor.render(&scene, true).expect("render");
        let straight = output.pixels().expect("invariant: read back").clone();
        assert!(!compositor.resolved_layer());
        let layer = compositor.layer(32, 32).expect("layer");
        compositor
            .render_into(&scene, &layer)
            .expect("into the layer");
        let shown = [item(&layer, Placement::IDENTITY, Crop::NONE, 1.0)];
        let output = compositor
            .render(
                &Scene {
                    canvas,
                    background: Color::TRANSPARENT,
                    items: &shown,
                },
                true,
            )
            .expect("render the layer");
        let decoded = output.pixels().expect("invariant: read back");
        for y in 0..32 {
            for x in 0..32 {
                let expected = decoded.pixel(x, y).expect("invariant: inside");
                near(&straight, x, y, expected, 1);
            }
        }
        // The items are there: the red square, and the green bar mixed over it.
        assert_eq!(straight.pixel(6, 7), Some([255, 0, 0, 255]));
        assert_ne!(straight.pixel(9, 10), straight.pixel(6, 7));
    }
}

/// What a layer shows when it is placed alone on a 32x32 canvas.
fn shown(compositor: &mut Compositor, layer: &Image) -> Snapshot {
    let nothing = solid(compositor, 1, 1, [0, 0, 0, 0]);
    let items = [
        item(layer, Placement::IDENTITY, Crop::NONE, 1.0),
        item(&nothing, Placement::IDENTITY, Crop::NONE, 1.0),
    ];
    let scene = Scene {
        canvas: FrameSize::new(32, 32).expect("invariant: non-zero"),
        background: Color::BLACK,
        items: &items,
    };
    let output = compositor.render(&scene, true).expect("render");
    output.pixels().expect("invariant: read back").clone()
}

/// A layer that still holds the very draw it is asked for is left as it is
/// (ADR-0194), and is drawn again when anything it shows has changed: an image's
/// texels, an item's place or opacity, the background, the blending space, or the
/// layer itself.
#[test]
fn a_layer_is_drawn_again_only_when_what_it_draws_has_changed() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let canvas = FrameSize::new(32, 32).expect("invariant: non-zero");
    let red = solid(compositor, 2, 2, [255, 0, 0, 255]);
    let layer = compositor.layer(32, 32).expect("layer");
    let mut kept = 0;
    // Draws the red square at `x` with `opacity` over `background`, and says whether
    // the layer was left as it was.
    let mut draw = |compositor: &mut Compositor, x: f32, opacity: f32, background: Color| {
        let items = [item(
            &red,
            at([x, 6.0], [4.0, 4.0], 0.0),
            Crop::NONE,
            opacity,
        )];
        let scene = Scene {
            canvas,
            background,
            items: &items,
        };
        compositor
            .render_into(&scene, &layer)
            .expect("into the layer");
        let was = kept;
        kept = compositor.kept_layers();
        kept > was
    };
    let clear = Color::TRANSPARENT;
    assert!(!draw(compositor, 4.0, 1.0, clear), "the first draw");
    assert!(draw(compositor, 4.0, 1.0, clear), "the same again");
    assert!(draw(compositor, 4.0, 1.0, clear), "and again");
    super::near(&shown(compositor, &layer), 6, 8, [255, 0, 0, 255], 0);
    // Moved: drawn, and nothing is left where it was.
    assert!(!draw(compositor, 20.0, 1.0, clear), "moved");
    let moved = shown(compositor, &layer);
    super::near(&moved, 22, 8, [255, 0, 0, 255], 0);
    super::near(&moved, 6, 8, [0, 0, 0, 255], 0);
    assert!(draw(compositor, 20.0, 1.0, clear), "the same again");
    assert!(!draw(compositor, 20.0, 0.5, clear), "faded");
    assert!(
        !draw(compositor, 20.0, 0.5, Color::BLACK),
        "another background"
    );
    assert!(draw(compositor, 20.0, 0.5, Color::BLACK), "the same again");
    // The image's texels changed.
    compositor
        .rewrite_rgba8(&red, &[0, 0, 255, 255].repeat(4))
        .expect("rewrite");
    assert!(!draw(compositor, 20.0, 0.5, Color::BLACK), "new texels");
    assert!(!draw(compositor, 20.0, 1.0, clear), "opaque again");
    super::near(&shown(compositor, &layer), 22, 8, [0, 0, 255, 255], 0);
    // The blending space changed.
    compositor.set_blending(Blending::Srgb);
    assert!(!draw(compositor, 20.0, 1.0, clear), "another space");
    assert!(draw(compositor, 20.0, 1.0, clear), "the same again");
    // Something else drew into the layer since.
    let scene = Scene {
        canvas,
        background: Color::rgb(0.0, 1.0, 0.0),
        items: &[],
    };
    compositor.render_into(&scene, &layer).expect("green");
    assert!(!draw(compositor, 20.0, 1.0, clear), "after another draw");
    let again = shown(compositor, &layer);
    super::near(&again, 22, 8, [0, 0, 255, 255], 0);
    super::near(&again, 6, 8, [0, 0, 0, 255], 0);
}

/// A scene that places a layer is drawn again when that layer was, and kept when it
/// was kept: a nested scene that did not change costs its parents nothing either.
#[test]
fn a_parent_is_kept_while_the_layers_it_places_are() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let canvas = FrameSize::new(32, 32).expect("invariant: non-zero");
    let red = solid(compositor, 2, 2, [255, 0, 0, 255]);
    let child = compositor.layer(32, 32).expect("child");
    let parent = compositor.layer(32, 32).expect("parent");
    let frame = |compositor: &mut Compositor, x: f32| {
        let before = compositor.kept_layers();
        let items = [item(&red, at([x, 6.0], [4.0, 4.0], 0.0), Crop::NONE, 1.0)];
        let scene = Scene {
            canvas,
            background: Color::TRANSPARENT,
            items: &items,
        };
        compositor.render_into(&scene, &child).expect("child");
        let placed = [item(
            &child,
            at([0.0, 8.0], [1.0, 1.0], 0.0),
            Crop::NONE,
            1.0,
        )];
        let scene = Scene {
            canvas,
            background: Color::BLACK,
            items: &placed,
        };
        compositor.render_into(&scene, &parent).expect("parent");
        compositor.kept_layers() - before
    };
    assert_eq!(frame(compositor, 4.0), 0, "the first frame draws both");
    assert_eq!(
        frame(compositor, 4.0),
        2,
        "an unchanged frame draws neither"
    );
    assert_eq!(frame(compositor, 12.0), 0, "a moved item draws both");
    assert_eq!(frame(compositor, 12.0), 2);
    let seen = shown(compositor, &parent);
    super::near(&seen, 14, 16, [255, 0, 0, 255], 0);
    super::near(&seen, 6, 16, [0, 0, 0, 255], 0);
}

/// The cells of a 64x64 scene: four squares, the third half-opaque over the second.
fn cells<'a>(images: &'a [Image; 4], moved: f32, faded: f32) -> [Item<'a>; 4] {
    [
        item(&images[0], at([2.0, 2.0], [5.0, 5.0], 0.0), Crop::NONE, 1.0),
        item(
            &images[1],
            at([40.0, 4.0], [6.0, 6.0], 0.0),
            Crop::NONE,
            faded,
        ),
        item(
            &images[2],
            at([44.0, 8.0], [6.0, 6.0], 0.0),
            Crop::NONE,
            1.0,
        ),
        item(
            &images[3],
            at([moved, 44.0], [4.0, 4.0], 0.0),
            Crop::NONE,
            1.0,
        ),
    ]
}

/// A layer in which a few items changed is drawn again only over their boxes
/// (ADR-0201), to the very pixels a whole redraw gives, in both blending spaces: an
/// item moved, one faded under another, an image's texels changed. A change over more
/// than half of it, an added item, or a background draws it whole.
#[test]
fn a_layer_is_redrawn_only_where_its_items_changed() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let canvas = FrameSize::new(64, 64).expect("invariant: non-zero");
    for blending in [Blending::Linear, Blending::Srgb] {
        compositor.set_blending(blending);
        let images = [
            solid(compositor, 2, 2, [255, 0, 0, 255]),
            solid(compositor, 2, 2, [0, 255, 0, 255]),
            solid(compositor, 2, 2, [40, 90, 255, 128]),
            solid(compositor, 2, 2, [255, 255, 0, 255]),
        ];
        let kept = compositor.layer(64, 64).expect("layer");
        // Draws the cells into `kept`, and afresh into a new layer, and holds the two
        // to the same pixels; returns how many texels of `kept` were redrawn as boxes.
        let frame = |compositor: &mut Compositor, items: &[Item<'_>], background: Color| {
            let scene = Scene {
                canvas,
                background,
                items,
            };
            compositor.render_into(&scene, &kept).expect("kept");
            let patched = compositor.patched_texels();
            let fresh = compositor.layer(64, 64).expect("fresh");
            compositor.render_into(&scene, &fresh).expect("fresh");
            assert_eq!(compositor.patched_texels(), 0, "a new layer is drawn whole");
            let read = |compositor: &mut Compositor, layer: &Image| {
                let shown = [
                    item(layer, Placement::IDENTITY, Crop::NONE, 1.0),
                    item(
                        &images[0],
                        at([70.0, 70.0], [1.0, 1.0], 0.0),
                        Crop::NONE,
                        1.0,
                    ),
                ];
                let scene = Scene {
                    canvas,
                    background: Color::BLACK,
                    items: &shown,
                };
                let output = compositor.render(&scene, true).expect("render");
                output.pixels().expect("invariant: read back").clone()
            };
            let (patched_pixels, fresh_pixels) =
                (read(compositor, &kept), read(compositor, &fresh));
            assert_eq!(patched_pixels.rgba(), fresh_pixels.rgba());
            patched
        };
        let clear = Color::TRANSPARENT;
        assert_eq!(frame(compositor, &cells(&images, 10.0, 1.0), clear), 0);
        // The last cell moves: its old box and its new one, and nothing else.
        let moved = frame(compositor, &cells(&images, 30.0, 1.0), clear);
        assert!(moved > 0 && moved < 64 * 64 / 4, "{moved} texels");
        // The second fades under the third.
        let faded = frame(compositor, &cells(&images, 30.0, 0.4), clear);
        assert!(faded > 0 && faded < 64 * 64 / 4, "{faded} texels");
        // The first cell's texels change.
        compositor
            .rewrite_rgba8(&images[0], &[255, 0, 255, 255].repeat(4))
            .expect("rewrite");
        let rewritten = frame(compositor, &cells(&images, 30.0, 0.4), clear);
        assert!(
            rewritten > 0 && rewritten < 64 * 64 / 4,
            "{rewritten} texels"
        );
        // Two cells change at once, far apart: two boxes.
        let both = frame(compositor, &cells(&images, 12.0, 0.9), clear);
        assert!(both > 0 && both < 64 * 64 / 4, "{both} texels");
        // Whole draws: an item added, a background, and a change over most of it.
        let mut more = cells(&images, 12.0, 0.9).to_vec();
        more.push(item(
            &images[3],
            at([20.0, 20.0], [2.0, 2.0], 0.0),
            Crop::NONE,
            1.0,
        ));
        assert_eq!(frame(compositor, &more, clear), 0, "an item added");
        assert_eq!(frame(compositor, &more, Color::BLACK), 0, "a background");
        assert_eq!(
            frame(compositor, &more, Color::BLACK),
            0,
            "over a background"
        );
        let mut wide = cells(&images, 12.0, 0.9);
        assert_eq!(frame(compositor, &wide, clear), 0, "an item removed");
        wide[0] = item(
            &images[0],
            at([0.0, 0.0], [30.0, 30.0], 0.0),
            Crop::NONE,
            1.0,
        );
        assert_eq!(frame(compositor, &wide, clear), 0, "most of the layer");
    }
}
