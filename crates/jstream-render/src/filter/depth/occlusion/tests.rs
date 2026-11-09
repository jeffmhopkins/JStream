//! `ambient_occlusion` (ADR-0151 decision 5): its settings, and its shading against depth
//! images of known shapes: a flat wall, a step and nothing drawn.

use std::sync::Arc;

use serde_json::json;

use super::{KIND, Values};
use crate::filter::testing::{filter, read, settings};
use crate::filter::{FilterClock, ViewDepth};
use crate::image;
use crate::tests::{Opened, near, open, solid};
use crate::{Image, Snapshot};

const W: u32 = 96;
const H: u32 = 48;
const WHITE: [u8; 4] = [255; 4];

#[test]
fn settings_map_to_values() {
    let v = super::values(&settings(KIND, json!({})));
    assert_eq!(
        v,
        Values {
            radius: 48.0,
            strength: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            samples: 8,
        }
    );
    let v = super::values(&settings(
        KIND,
        json!({"radius_px": 0.0, "strength": 9.0, "color": "#ffffff", "quality": "high"}),
    ));
    assert_eq!((v.radius, v.strength, v.samples), (1.0, 2.0, 16));
    assert_eq!(v.color, [1.0, 1.0, 1.0, 1.0]);
}

fn apply(opened: &mut Opened, ao: &mut crate::VideoFilter, input: &Image) -> Snapshot {
    let output = opened
        .compositor
        .apply_filter(
            ao,
            input,
            FilterClock {
                seconds: 0.0,
                beat: 0.0,
            },
        )
        .expect("apply_filter");
    read(opened, &output)
}

/// A `W` by `H` view depth at a focal length of 1000 whose distance at column `x` is
/// `at(x)`, 0 for nothing drawn.
fn depth(opened: &Opened, at: impl Fn(u32) -> f32) -> ViewDepth {
    let texels: Vec<[f32; 4]> = (0..H)
        .flat_map(|_| (0..W).map(&at))
        .map(|d| {
            if d > 0.0 {
                [1000.0 / d, 0.0, 0.0, 1.0]
            } else {
                [0.0; 4]
            }
        })
        .collect();
    let image = image::upload_unit_f16(&opened.compositor.maker(), W, H, &texels)
        .expect("the depth uploads");
    ViewDepth {
        image: Arc::new(image),
        focal_px: 1000.0,
    }
}

fn luma(shot: &Snapshot, x: u32, y: u32) -> u8 {
    shot.pixel(x, y).expect("inside")[1]
}

/// ADR-0151 decision 5: with no view depth it changes nothing.
#[test]
fn without_a_depth_it_passes_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, 8, 8, [40, 90, 200, 255]);
    let mut ao = filter(KIND, json!({}));
    let shot = apply(&mut opened, &mut ao, &input);
    near(&shot, 0, 0, [40, 90, 200, 255], 1);
    near(&shot, 7, 7, [40, 90, 200, 255], 1);
}

/// ADR-0151 decision 5: a flat wall facing the camera has no creases, so nothing on it
/// is shaded.
#[test]
fn a_flat_wall_is_not_shaded() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, WHITE);
    let mut ao = filter(KIND, json!({"quality": "high"}));
    ao.set_depth(Some(depth(&opened, |_| 2000.0)));
    let shot = apply(&mut opened, &mut ao, &input);
    for x in [0, 20, 47, 70, 95] {
        near(&shot, x, H / 2, WHITE, 2);
    }
}

/// ADR-0151 decision 5: at a step, the farther side next to it is shaded by the nearer
/// side, and neither far from the step nor on the nearer side is; where nothing was
/// drawn is unchanged; the colour tints the shade.
#[test]
fn a_step_shades_the_farther_side_beside_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, WHITE);
    let step = |x: u32| match x {
        0..8 => 0.0,
        8..48 => 2000.0,
        _ => 1960.0,
    };
    let mut ao = filter(KIND, json!({"strength": 2.0}));
    ao.set_depth(Some(depth(&opened, step)));
    let shot = apply(&mut opened, &mut ao, &input);
    let (beside, away, nearer) = (
        luma(&shot, 45, H / 2),
        luma(&shot, 16, H / 2),
        luma(&shot, 70, H / 2),
    );
    assert!(beside < 235, "beside the step: {beside}");
    assert!(away > 250, "away from the step: {away}");
    assert!(nearer > 250, "on the nearer side: {nearer}");
    near(&shot, 2, H / 2, WHITE, 1);
    // Toward red, the shade keeps red and loses green.
    let mut red = filter(KIND, json!({"strength": 2.0, "color": "#ff0000"}));
    red.set_depth(Some(depth(&opened, step)));
    let shot = apply(&mut opened, &mut red, &input);
    let pixel = shot.pixel(45, H / 2).expect("inside");
    assert!(pixel[0] > 250 && pixel[1] < 235, "{pixel:?}");
    // No strength changes nothing.
    let mut none = filter(KIND, json!({"strength": 0.0}));
    none.set_depth(Some(depth(&opened, step)));
    let shot = apply(&mut opened, &mut none, &input);
    near(&shot, 45, H / 2, WHITE, 1);
}
