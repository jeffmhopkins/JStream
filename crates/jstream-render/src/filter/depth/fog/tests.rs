//! `depth_fog` and the view depth it reads (ADR-0143 decisions 4 to 6): its settings, its
//! fog against a depth image of known distances, and end to end over a 3D scene's view
//! depth from `Compositor::render_view_depth` and `draw_model_depth`.

use std::sync::Arc;

use jstream_media::FrameSize;
use jstream_media::model::Material;
use jstream_media::model::shapes::Shape;
use jstream_protocol::{Canvas, Finite32, ItemGeometry, SceneCamera, Size, Vec2};
use serde_json::{Value, json};

use super::{KIND, Values};
use crate::filter::testing::{filter, read, settings};
use crate::filter::{FilterClock, ViewDepth};
use crate::image;
use crate::tests::{Opened, near, open, solid};
use crate::{Color, Crop, Image, Item, Lighting, ModelDraw, Placement, Scene, Snapshot};

const W: u32 = 160;
const H: u32 = 100;
const WHITE: [u8; 4] = [255; 4];
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn settings_map_to_values() {
    let v = super::values(&settings(KIND, json!({})));
    assert_eq!(
        v,
        Values {
            color: v.color,
            start: 0.0,
            end: 4000.0,
            density: 0.5,
            most: 1.0,
            exponential: true,
            background: true,
        }
    );
    let v = super::values(&settings(
        KIND,
        json!({
            "mode": "linear", "color": "#ffffff", "start_px": -5.0, "end_px": 900.0,
            "density": 50.0, "max": 2.0, "background": false
        }),
    ));
    assert_eq!(v.color, [1.0, 1.0, 1.0, 1.0]);
    assert_eq!((v.start, v.end, v.density, v.most), (0.0, 900.0, 20.0, 1.0));
    assert!(!v.exponential && !v.background);
    assert_eq!(super::uniforms(&v, None).len(), 48);
}

/// White fog over a black picture: the output's grey is the fog amount.
fn white_fog(overrides: Value) -> crate::VideoFilter {
    let mut all = json!({"color": "#ffffff"});
    if let (Value::Object(all), Value::Object(more)) = (&mut all, overrides) {
        all.extend(more);
    }
    filter(KIND, all)
}

fn apply(opened: &mut Opened, fog: &mut crate::VideoFilter, input: &Image) -> Snapshot {
    let output = opened
        .compositor
        .apply_filter(
            fog,
            input,
            FilterClock {
                seconds: 0.0,
                beat: 0.0,
            },
        )
        .expect("apply_filter");
    read(opened, &output)
}

/// sRGB of a linear grey.
fn grey(linear: f32) -> [u8; 4] {
    let v = (crate::color::linear_channel_to_srgb(linear) * 255.0).round() as u8;
    [v, v, v, 255]
}

/// ADR-0143 decision 5: with no view depth (a 2D scene, a chain that isn't a scene's),
/// the fog changes nothing.
#[test]
fn without_a_depth_it_passes_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 8, 8, [40, 90, 200, 255]);
    let mut fog = white_fog(json!({}));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 0, 0, [40, 90, 200, 255], 1);
    near(&shot, 7, 7, [40, 90, 200, 255], 1);
}

/// A 4x1 depth image with `1 / w` of 0.5, 0.25, 0 and 1, with a focal length of 1000:
/// distances 2000, 4000, nothing (infinitely far) and 1000.
fn known_depth(opened: &Opened) -> ViewDepth {
    let texels = [0.5, 0.25, 0.0, 1.0].map(|r| [r, 0.0, 0.0, if r > 0.0 { 1.0 } else { 0.0 }]);
    let image = image::upload_unit_f16(&opened.compositor.maker(), 4, 1, &texels)
        .expect("the depth uploads");
    ViewDepth::new(Arc::new(image), 1000.0)
}

/// ADR-0143 decision 6: linear fog from 1000 to 3000 is half at 2000 and full at 4000;
/// nothing nearer than its start is fogged; the background takes the most fog, or none.
#[test]
fn linear_fog_follows_the_distance() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 4, 1, BLACK);
    let depth = known_depth(&opened);
    let linear = json!({"mode": "linear", "start_px": 1000.0, "end_px": 3000.0});
    let mut fog = white_fog(linear.clone());
    fog.set_depth(Some(depth.clone()));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 0, 0, grey(0.5), 2);
    near(&shot, 1, 0, WHITE, 1);
    near(&shot, 2, 0, WHITE, 1);
    near(&shot, 3, 0, BLACK, 1);
    let mut capped = white_fog(json!({
        "mode": "linear", "start_px": 1000.0, "end_px": 3000.0, "max": 0.25,
        "background": false
    }));
    capped.set_depth(Some(depth.clone()));
    let shot = apply(&mut opened, &mut capped, &input);
    near(&shot, 0, 0, grey(0.25), 2);
    near(&shot, 1, 0, grey(0.25), 2);
    near(&shot, 2, 0, BLACK, 1);
    // Taking the depth away passes through again.
    fog.set_depth(None);
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 0, 0, BLACK, 1);
}

/// ADR-0143 decision 6: exponential fog is 1 - exp(-density (D - start) / 1000).
#[test]
fn exponential_fog_thickens_with_distance() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 4, 1, BLACK);
    let mut fog = white_fog(json!({"density": 0.5, "start_px": 0.0}));
    fog.set_depth(Some(known_depth(&opened)));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 0, 0, grey(1.0 - (-1.0f32).exp()), 2);
    near(&shot, 1, 0, grey(1.0 - (-2.0f32).exp()), 2);
    near(&shot, 3, 0, grey(1.0 - (-0.5f32).exp()), 2);
}

/// ADR-0143 decision 6: fog is a layer over what is there, premultiplied: over a
/// transparent pixel it is the fog's own colour at the fog's opacity.
#[test]
fn fog_covers_a_transparent_pixel_with_its_own_opacity() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 4, 1, [0, 0, 0, 0]);
    let mut fog = filter(
        KIND,
        json!({"color": "#ff0000", "mode": "linear", "start_px": 1000.0, "end_px": 3000.0}),
    );
    fog.set_depth(Some(known_depth(&opened)));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 2, 0, [255, 0, 0, 255], 1);
    near(&shot, 0, 0, [255, 0, 0, 128], 2);
}

/// A depth of another size than the picture doesn't line up with it: pass through.
#[test]
fn a_depth_of_another_size_passes_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 8, 8, BLACK);
    let mut fog = white_fog(json!({}));
    fog.set_depth(Some(known_depth(&opened)));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 3, 3, BLACK, 1);
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

/// A `size` square card centred at `(x, y)`, `depth` along z (toward the viewer).
fn card(x: f32, y: f32, size: f32, depth: f32) -> ItemGeometry {
    let mut geometry = ItemGeometry::try_new(
        Vec2::new(x, y).expect("finite"),
        Size::try_new(size, size).expect("positive"),
    )
    .expect("a box");
    geometry.depth_px = finite(depth);
    geometry
}

fn camera() -> SceneCamera {
    SceneCamera::framing(Canvas {
        width: W,
        height: H,
    })
}

fn focal() -> f32 {
    camera()
        .view(Canvas {
            width: W,
            height: H,
        })
        .expect("a view")
        .focal_px()
}

fn placed<'a>(image: &'a Image, geometry: ItemGeometry, opacity: f32) -> Item<'a> {
    Item {
        camera: Some(camera()),
        ..crate::tests::item(image, Placement::Geometry(geometry), Crop::NONE, opacity)
    }
}

/// `items`' view depth, fogged white with linear fog from `focal` to `3 · focal` over a
/// black canvas, so each grey reads the distance: none at the framing distance (a card at
/// `depth_px` 0), half at twice it.
fn fogged(opened: &mut Opened, items: &[Item<'_>], models: &[ModelDraw<'_>]) -> Snapshot {
    let scene = Scene {
        canvas: FrameSize::new(W, H).expect("non-zero"),
        background: Color::BLACK,
        items,
    };
    let depth = opened.compositor.layer(W, H).expect("a layer");
    opened
        .compositor
        .render_view_depth(&scene, &depth)
        .expect("the view depth draws");
    for model in models {
        opened
            .compositor
            .draw_model_depth(model, &depth)
            .expect("the model's depth draws");
    }
    let black = solid(&opened.compositor, W, H, BLACK);
    let f = focal();
    let mut fog = white_fog(json!({
        "mode": "linear", "start_px": f, "end_px": 3.0 * f, "background": false
    }));
    fog.set_depth(Some(ViewDepth::new(Arc::new(depth), f)));
    apply(opened, &mut fog, &black)
}

/// ADR-0143 decision 4: each card writes its own distance, the nearest wins whatever
/// order the cards draw in, and nothing is written where no card is.
#[test]
fn golden_view_depth_holds_the_nearest_card() {
    let Some(mut opened) = open() else {
        return;
    };
    let f = focal();
    let image = solid(&opened.compositor, 4, 4, WHITE);
    // A card on the canvas plane at the left, one a focal length behind it at the right,
    // and one behind both, overlapping the first, drawn last in stacking order.
    let near_card = placed(&image, card(40.0, 50.0, 30.0, 0.0), 1.0);
    let far_card = placed(&image, card(120.0, 50.0, 60.0, -f), 1.0);
    let behind = Item {
        camera: Some(SceneCamera {
            depth_sort: false,
            ..camera()
        }),
        ..placed(&image, card(40.0, 50.0, 240.0, -2.0 * f), 1.0)
    };
    let shot = fogged(&mut opened, &[near_card, far_card, behind], &[]);
    near(&shot, 40, 50, BLACK, 1);
    // Seen through the camera, a card a focal length back lands half as far from the
    // middle: at 100, not 120.
    near(&shot, 100, 50, grey(0.5), 2);
    // Only the card behind covers here, at three focal lengths: full fog.
    near(&shot, 40, 20, WHITE, 1);
    // Nothing at all, with no background fog.
    near(&shot, 80, 95, BLACK, 1);
}

/// ADR-0143 decision 4: a card whose alpha with its opacity is under one half writes no
/// depth, so what is behind it keeps its own; a hidden card writes none either.
#[test]
fn golden_view_depth_skips_mostly_transparent_cards() {
    let Some(mut opened) = open() else {
        return;
    };
    let f = focal();
    let image = solid(&opened.compositor, 4, 4, WHITE);
    let back = placed(&image, card(80.0, 50.0, 120.0, -f), 1.0);
    let faint = placed(&image, card(80.0, 50.0, 30.0, 0.0), 0.4);
    let hidden = Item {
        visible: false,
        ..placed(&image, card(60.0, 50.0, 10.0, 0.0), 1.0)
    };
    let shot = fogged(&mut opened, &[back, faint, hidden], &[]);
    near(&shot, 80, 50, grey(0.5), 2);
    near(&shot, 60, 50, grey(0.5), 2);
    let solid_front = placed(&image, card(80.0, 50.0, 30.0, 0.0), 0.6);
    let shot = fogged(&mut opened, &[back, solid_front], &[]);
    near(&shot, 80, 50, BLACK, 1);
}

/// ADR-0143 decision 4: a scene with no camera has no depth.
#[test]
fn golden_view_depth_is_empty_without_a_camera() {
    let Some(mut opened) = open() else {
        return;
    };
    let image = solid(&opened.compositor, 4, 4, WHITE);
    let flat = crate::tests::item(
        &image,
        Placement::Geometry(card(80.0, 50.0, 80.0, -500.0)),
        Crop::NONE,
        1.0,
    );
    let scene = Scene {
        canvas: FrameSize::new(W, H).expect("non-zero"),
        background: Color::BLACK,
        items: &[flat],
    };
    let depth = opened.compositor.layer(W, H).expect("a layer");
    opened
        .compositor
        .render_view_depth(&scene, &depth)
        .expect("the view depth draws");
    let shot = read(&mut opened, &depth);
    near(&shot, 80, 50, [0, 0, 0, 0], 0);
}

/// ADR-0143 decision 4: a model writes its own depth per fragment, nearer than a card
/// behind it, and its layer's placement writes none of its own.
#[test]
fn golden_view_depth_takes_a_models_own_depth() {
    let Some(mut opened) = open() else {
        return;
    };
    let f = focal();
    let cube = Shape::Cube
        .model(Material::default())
        .expect("the cube builds");
    let uploaded = opened.compositor.upload_model(&cube).expect("it uploads");
    // A 40 px cube a focal length back, in front of a card two focal lengths back.
    let pose = card(80.0, 50.0, 40.0, -f);
    let draw = ModelDraw {
        model: &uploaded,
        geometry: pose,
        camera: Some(camera()),
        lighting: Lighting::Unlit,
        lights: None,
        shadows: &[],
        environment: None,
    };
    let layer = opened.compositor.layer(W, H).expect("a layer");
    let image = solid(&opened.compositor, 4, 4, WHITE);
    let back = placed(&image, card(80.0, 50.0, 120.0, -2.0 * f), 1.0);
    let model_item = Item {
        camera: Some(camera()),
        ..crate::tests::item(&layer, Placement::Layer(pose), Crop::NONE, 1.0)
    };
    let shot = fogged(&mut opened, &[back, model_item], &[draw]);
    // The cube's front face is 20 px nearer than its centre: (2f - 20 - f) / 2f of fog.
    let at_front = (f - 20.0) / (2.0 * f);
    near(&shot, 80, 50, grey(at_front), 3);
    // Beside the cube, the card: (3f - f) / 2f, full fog.
    near(&shot, 64, 50, WHITE, 1);
}

/// ADR-0143 decision 6: a translucent fog colour is laid over the picture, source-over,
/// so an opaque picture stays opaque. Half-alpha white fog at full amount over black
/// gives linear 0.5 grey, not a half-transparent picture; fog at alpha 0 changes nothing.
#[test]
fn translucent_fog_covers_an_opaque_picture() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 4, 1, BLACK);
    let depth = known_depth(&opened);
    let mut half = filter(
        KIND,
        json!({"color": "#ffffff80", "mode": "linear", "start_px": 1000.0, "end_px": 3000.0}),
    );
    half.set_depth(Some(depth.clone()));
    let shot = apply(&mut opened, &mut half, &input);
    near(&shot, 2, 0, grey(128.0 / 255.0), 2);
    near(&shot, 0, 0, grey(0.5 * 128.0 / 255.0), 2);
    let mut clear = filter(KIND, json!({"color": "#ffffff00"}));
    clear.set_depth(Some(depth));
    let shot = apply(&mut opened, &mut clear, &input);
    near(&shot, 2, 0, BLACK, 0);
}
