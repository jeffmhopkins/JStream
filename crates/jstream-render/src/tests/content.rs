//! A nested scene's layer drawn only over what its scene drew (#1070): the same pixels as
//! the whole layer, with fewer of them shaded.

use jstream_media::FrameSize;
use jstream_protocol::BlendMode;

use super::{item, open, solid};
use crate::mips::Region;
use crate::{Color, Compositor, Crop, Image, LegacyTransform, Placement, Scene, Snapshot};

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
