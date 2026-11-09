//! A nested scene's layer drawn only over what its scene drew (#1070): the same pixels as
//! the whole layer, with fewer of them shaded.

use std::sync::Arc;

use jstream_media::FrameSize;
use jstream_protocol::BlendMode;
use serde_json::json;

use super::{Opened, item, open, solid};
use crate::filter::testing::{filter, image, read};
use crate::mips::Region;
use crate::{
    Color, Compositor, Crop, FilterClock, Image, LegacyTransform, Placement, Scene, Snapshot,
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
        .apply_filter_into(filter, input, &output, FilterClock { seconds: 0.0 })
        .expect("apply the filter");
    output
}

fn same(kind: &str, bounded: &Snapshot, whole: &Snapshot) {
    let pixels = bounded.rgba().chunks(4).zip(whole.rgba().chunks(4));
    for (index, (a, b)) in pixels.enumerate() {
        // A readback un-premultiplies, so a texel under half a code of alpha, which the
        // whole draw can hold past the reach, reads back as any colour.
        if a[3] == 0 && b[3] == 0 {
            continue;
        }
        let near = a.iter().zip(b).all(|(a, b)| a.abs_diff(*b) <= 1);
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

/// A blur or a mask over a layer whose content is known draws only near it, to the same
/// pixels as over all of it, and says where that is, so the next filter of a chain and
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
    ];
    for (kind, settings) in kinds {
        let make = || {
            let mut made = filter(kind, settings.clone());
            made.set_reference(Some(Arc::clone(&mask)));
            made
        };
        let bounded = filtered(&mut opened, &mut make(), &layer);
        let reach = bounded.content().expect("a blur or a mask keeps the bound");
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
