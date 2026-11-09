//! `floor_reflection` (ADR-0151 decision 4): its settings, and the reflection laid over
//! a floor seen through the framing camera, against depth images of known distances.

use std::sync::Arc;

use jstream_protocol::{Canvas, SceneCamera};
use serde_json::json;

use super::{KIND, Values};
use crate::filter::testing::{filter, read, settings};
use crate::filter::{DepthReflection, FilterClock, ViewDepth};
use crate::image;
use crate::tests::{Opened, near, open, solid};
use crate::{Image, Snapshot};

const W: u32 = 32;
const H: u32 = 16;
const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

#[test]
fn settings_map_to_values() {
    assert_eq!(
        super::values(&settings(KIND, json!({}))),
        Values {
            strength: 0.5,
            fade: 0.0,
            blur: 0.0,
        }
    );
    let v = super::values(&settings(
        KIND,
        json!({"strength": 3.0, "fade_px": -1.0, "blur_px": 500.0}),
    ));
    assert_eq!((v.strength, v.fade, v.blur), (1.0, 0.0, 64.0));
    assert_eq!(
        super::floor(&settings(KIND, json!({"floor_y_px": 12.0}))),
        12.0
    );
}

fn canvas() -> Canvas {
    Canvas {
        width: W,
        height: H,
    }
}

fn view() -> jstream_protocol::CameraView {
    SceneCamera::framing(canvas())
        .view(canvas())
        .expect("a view")
}

/// A depth image whose every pixel holds `inverse_w`, at most 1.
fn even_depth(opened: &Opened, inverse_w: f32) -> Image {
    let texels = vec![[inverse_w, 0.0, 0.0, 1.0]; (W * H) as usize];
    image::upload_unit_f16(&opened.compositor.maker(), W, H, &texels).expect("the depth uploads")
}

/// The framing camera's view depth of a floor at `floor_y` seen past the canvas plane:
/// each row below the principal point at the distance where its ray meets the floor,
/// and nothing above.
fn floor_depth(opened: &Opened, floor_y: f64) -> Image {
    let view = view();
    let f = f64::from(view.focal_px());
    let [_, cy] = view.principal();
    let below = floor_y - view.position()[1];
    let texels: Vec<[f32; 4]> = (0..H)
        .flat_map(|y| {
            let slope = (f64::from(y) + 0.5 - f64::from(cy)) / f;
            let inverse_w = if slope > 0.0 && below > 0.0 {
                (f / (below / slope)) as f32
            } else {
                0.0
            };
            (0..W).map(move |_| [inverse_w, 0.0, 0.0, 1.0])
        })
        .collect();
    image::upload_unit_f16(&opened.compositor.maker(), W, H, &texels).expect("the depth uploads")
}

fn apply(opened: &mut Opened, filter: &mut crate::VideoFilter, input: &Image) -> Snapshot {
    let output = opened
        .compositor
        .apply_filter(
            filter,
            input,
            FilterClock {
                seconds: 0.0,
                beat: 0.0,
            },
        )
        .expect("apply_filter");
    read(opened, &output)
}

/// ADR-0151 decision 4: a black floor takes the white mirrored picture at `strength`
/// where the mirrored depth puts something above the floor, and stays black where it
/// puts something under it, where nothing was mirrored, and off the floor.
#[test]
fn the_floor_takes_the_reflection_of_what_is_above_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, BLACK);
    let view = view();
    let f = view.focal_px();
    // The floor 40 px below the camera's axis: the bottom rows of the canvas see it,
    // farther than the focal length, so every `1 / w` fits the unit depth image.
    let floor_y = view.position()[1] + 40.0;
    let floor = floor_depth(&opened, floor_y);
    let white = solid(&opened.compositor, W, H, WHITE);
    let with = |opened: &Opened, mirrored_at: f32| ViewDepth {
        view: Some(view),
        reflection: Some(DepthReflection {
            image: white.clone(),
            depth: even_depth(opened, mirrored_at),
            floor_y: floor_y as f32,
        }),
        ..ViewDepth::new(Arc::new(floor.clone()), f)
    };
    let mut mirror = filter(KIND, json!({"strength": 1.0}));
    // Far past the floor: the mirror image of something high above it.
    mirror.set_depth(Some(with(&opened, f / 1e5)));
    let shot = apply(&mut opened, &mut mirror, &input);
    near(&shot, W / 2, H - 1, WHITE, 2);
    near(&shot, W / 2, 0, BLACK, 1);
    // Nearer than the floor: under it, so not reflected.
    mirror.set_depth(Some(with(&opened, f / 1.0)));
    let shot = apply(&mut opened, &mut mirror, &input);
    near(&shot, W / 2, H - 1, BLACK, 1);
    // Nothing mirrored.
    mirror.set_depth(Some(with(&opened, 0.0)));
    let shot = apply(&mut opened, &mut mirror, &input);
    near(&shot, W / 2, H - 1, BLACK, 1);
    // Half strength: halfway in linear light.
    let mut half = filter(KIND, json!({}));
    half.set_depth(Some(with(&opened, f / 1e5)));
    let shot = apply(&mut opened, &mut half, &input);
    let v = (crate::color::linear_channel_to_srgb(0.5) * 255.0).round() as u8;
    near(&shot, W / 2, H - 1, [v, v, v, 255], 2);
}

/// ADR-0151 decision 4: with no view, or no reflection drawn, it changes nothing.
#[test]
fn without_a_reflection_it_passes_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, BLACK);
    let view = view();
    let floor = floor_depth(&opened, view.position()[1] + 40.0);
    let mut mirror = filter(KIND, json!({"strength": 1.0}));
    let shot = apply(&mut opened, &mut mirror, &input);
    near(&shot, W / 2, H - 1, BLACK, 0);
    mirror.set_depth(Some(ViewDepth {
        view: Some(view),
        ..ViewDepth::new(Arc::new(floor), view.focal_px())
    }));
    let shot = apply(&mut opened, &mut mirror, &input);
    near(&shot, W / 2, H - 1, BLACK, 0);
}
