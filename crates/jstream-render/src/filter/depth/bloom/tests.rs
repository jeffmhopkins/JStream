//! `depth_bloom` (ADR-0151 decision 8): its settings, and its glow against depth images
//! of known distances: a glow spreads where nothing stands in front of it and stops at
//! something nearer.

use std::sync::Arc;

use serde_json::json;

use super::{KIND, Values};
use crate::filter::testing::{filter, read, settings};
use crate::filter::{FilterClock, ViewDepth};
use crate::image;
use crate::tests::{Opened, open};
use crate::{Image, Snapshot};

const W: u32 = 32;
const H: u32 = 8;

#[test]
fn settings_map_to_values() {
    assert_eq!(
        super::values(&settings(KIND, json!({}))),
        Values {
            threshold: 0.8,
            strength: 1.0,
            radius: 24.0,
            depth: 200.0,
            far: 0.0,
        }
    );
    let v = super::values(&settings(
        KIND,
        json!({"threshold": 9.0, "strength": -1.0, "radius_px": 500.0, "depth_px": 0.0}),
    ));
    assert_eq!(
        (v.threshold, v.strength, v.radius, v.depth),
        (4.0, 0.0, 100.0, 1.0)
    );
    assert_eq!(
        crate::filter::outset(KIND, &settings(KIND, json!({"radius_px": 30.0}))),
        Some(30.0)
    );
    // At most 16 taps a side, at least a pixel apart.
    assert_eq!(super::taps(100.0), (50.0, 150.0 / 16.0, 16));
    assert_eq!(super::taps(4.0), (2.0, 1.0, 6));
}

/// A picture black but for a white column at x = 8, its pixels at `left` px for x below
/// 16 and `right` px from 16 on.
fn scene(opened: &Opened, (left, right): (f32, f32)) -> (Image, ViewDepth) {
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..H {
        for x in 0..W {
            rgba.extend_from_slice(if x == 8 {
                &[255, 255, 255, 255]
            } else {
                &[0, 0, 0, 255]
            });
        }
    }
    let picture = opened
        .compositor
        .upload_rgba8(W, H, &rgba)
        .expect("the picture uploads");
    let f = 100.0;
    let texels: Vec<[f32; 4]> = (0..H)
        .flat_map(|_| {
            (0..W).map(|x| {
                let d = if x < 16 { left } else { right };
                [f / d, 0.0, 0.0, 1.0]
            })
        })
        .collect();
    let depth = image::upload_unit_f16(&opened.compositor.maker(), W, H, &texels)
        .expect("the depth uploads");
    (picture, ViewDepth::new(Arc::new(depth), f))
}

fn apply(opened: &mut Opened, bloom: &mut crate::VideoFilter, input: &Image) -> Snapshot {
    let output = opened
        .compositor
        .apply_filter(
            bloom,
            input,
            FilterClock {
                seconds: 0.0,
                beat: 0.0,
            },
        )
        .expect("apply_filter");
    read(opened, &output)
}

/// ADR-0151 decision 8: the white column glows onto pixels as far as it is, and onto
/// none in front of it by more than twice `depth_px`; without a depth it blooms evenly.
#[test]
fn a_glow_stops_at_something_in_front_of_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let glow = |shot: &Snapshot, x| shot.pixel(x, H / 2).expect("inside")[0];
    let settings = json!({"threshold": 0.5, "radius_px": 16.0, "depth_px": 100.0});
    // Everything 1000 px away: the glow reaches both sides alike.
    let (picture, depth) = scene(&opened, (1000.0, 1000.0));
    let mut even = filter(KIND, settings.clone());
    even.set_depth(Some(depth));
    let shot = apply(&mut opened, &mut even, &picture);
    let (near_left, near_right) = (glow(&shot, 4), glow(&shot, 12));
    assert!(near_left > 10, "the glow spreads: {near_left}");
    assert!(
        near_left.abs_diff(near_right) <= 2,
        "{near_left} against {near_right}"
    );
    // The right half 500 px nearer: the glow stops at it.
    let (picture, depth) = scene(&opened, (1000.0, 500.0));
    let mut blocked = filter(KIND, settings.clone());
    blocked.set_depth(Some(depth));
    let shot = apply(&mut opened, &mut blocked, &picture);
    assert!(glow(&shot, 4) > 10, "it still glows on its own side");
    assert!(glow(&shot, 20) <= 1, "none crosses: {}", glow(&shot, 20));
    // No depth: plain bloom crosses.
    let mut plain = filter(KIND, settings);
    let shot = apply(&mut opened, &mut plain, &picture);
    assert!(
        glow(&shot, 20) > 2,
        "plain bloom reaches it: {}",
        glow(&shot, 20)
    );
}
