//! `screen_space_reflections` (ADR-0170 decision 4): its steps, and what a floor seen
//! through the framing camera reflects of a wall behind it, against hand-made depth and
//! surface images.

use std::sync::Arc;

use jstream_protocol::{Canvas, SceneCamera, Settings};
use serde_json::json;

use super::KIND;
use crate::filter::testing::read;
use crate::filter::{DepthSurface, FilterClock, VideoFilter, ViewDepth};
use crate::image;
use crate::tests::{Opened, open};
use crate::{Image, Snapshot};

const W: u32 = 64;
const H: u32 = 64;
/// How far below the camera's axis the floor is, in the viewing frame's pixels.
const FLOOR: f32 = 20.0;
/// The wall's `1 / w`: past every floor row whose own is less.
const WALL: f32 = 0.4;

#[test]
fn steps_are_one_of_the_switchs_counts() {
    let at = |value: serde_json::Value| {
        let settings: Settings =
            serde_json::from_value(json!({ "steps": value })).expect("settings");
        super::steps(&settings)
    };
    assert_eq!(super::steps(&Settings::default()), 16);
    assert_eq!(at(json!(64)), 64);
    assert_eq!(at(json!(40)), 32, "between counts, the one below");
    assert_eq!(at(json!(1)), 16, "never below the fewest");
    assert_eq!(at(json!(1000)), 64);
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

/// Row `y`'s `1 / w`: the floor where it is nearer than the wall, else the wall.
fn row_depth(y: u32) -> (f32, bool) {
    let [_, cy] = view().principal();
    let floor = (y as f32 + 0.5 - cy) / FLOOR;
    if floor > WALL {
        (floor, true)
    } else {
        (WALL, false)
    }
}

fn upload(opened: &Opened, texel: impl Fn(u32) -> [f32; 4]) -> Image {
    let texels: Vec<[f32; 4]> = (0..H)
        .flat_map(|y| (0..W).map(move |_| y))
        .map(texel)
        .collect();
    image::upload_any_f16(&opened.compositor.maker(), W, H, &texels).expect("the image uploads")
}

/// The scene: a mirror-smooth floor under a green wall, each pixel of the floor
/// replaceable when `replaceable`, showing only the red `env` its environment's
/// reflection added.
fn scene(opened: &Opened, replaceable: bool, env: f32) -> (Image, ViewDepth) {
    let picture = upload(opened, |y| {
        if row_depth(y).1 {
            [env, 0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0, 1.0]
        }
    });
    let depth = upload(opened, |y| [row_depth(y).0, 0.0, 0.0, 1.0]);
    let floor = |y: u32, on: [f32; 4]| if row_depth(y).1 { on } else { [0.0; 4] };
    let surface = DepthSurface {
        // The floor faces up, which is -y.
        normal: upload(opened, |y| floor(y, [0.0, -1.0, 0.0, row_depth(y).0])),
        brdf: upload(opened, |y| floor(y, [1.0, 1.0, 1.0, 0.05])),
        reflected: upload(opened, |y| {
            floor(y, [env, 0.0, 0.0, if replaceable { 1.0 } else { 0.0 }])
        }),
    };
    let view = view();
    let depth = ViewDepth {
        view: Some(view),
        surface: Some(surface),
        ..ViewDepth::new(Arc::new(depth), view.focal_px())
    };
    (picture, depth)
}

fn apply(opened: &mut Opened, filter: &mut VideoFilter, input: &Image) -> Snapshot {
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

/// ADR-0170 decision 4: the floor in front of the wall reflects it, the wall itself is
/// left, a floor whose reflection may not be replaced stays black, and without a
/// surface buffer the picture passes through.
#[test]
fn a_floor_reflects_the_wall_behind_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let mut reflections = VideoFilter::new(KIND, &Settings::default()).expect("a pass");
    let (picture, depth) = scene(&opened, true, 0.0);
    reflections.set_depth(Some(depth.clone()));
    let shot = apply(&mut opened, &mut reflections, &picture);
    let floor = shot.pixel(W / 2, 50).expect("inside");
    assert!(
        floor[1] > 120 && floor[0] < 8,
        "the floor shows the wall: {floor:?}"
    );
    let wall = shot.pixel(W / 2, 10).expect("inside");
    assert!(wall[1] > 250 && wall[0] < 4, "the wall is left: {wall:?}");

    let (picture, kept) = scene(&opened, false, 0.0);
    reflections.set_depth(Some(kept));
    let shot = apply(&mut opened, &mut reflections, &picture);
    let floor = shot.pixel(W / 2, 50).expect("inside");
    assert!(floor[1] < 4, "not replaceable: {floor:?}");

    reflections.set_depth(Some(ViewDepth {
        surface: None,
        ..depth
    }));
    let shot = apply(&mut opened, &mut reflections, &picture);
    let floor = shot.pixel(W / 2, 50).expect("inside");
    assert!(floor[1] < 4, "no surface buffer: {floor:?}");
}

/// ADR-0170 decision 5: what the wall shows takes the place of the environment's
/// reflection, which is taken out as much as the hit is put in.
#[test]
fn a_hit_replaces_the_environments_reflection() {
    let Some(mut opened) = open() else {
        return;
    };
    let mut reflections = VideoFilter::new(KIND, &Settings::default()).expect("a pass");
    let (picture, depth) = scene(&opened, true, 0.5);
    reflections.set_depth(Some(depth));
    let shot = apply(&mut opened, &mut reflections, &picture);
    let floor = shot.pixel(W / 2, 50).expect("inside");
    // 0.5 linear is 188 in sRGB: the wall's green comes in, the red goes out with it.
    assert!(floor[1] > 120, "the wall: {floor:?}");
    assert!(floor[0] < 150, "less of the environment: {floor:?}");
    // The wall seen nowhere: the floor near the bottom edge, whose ray leaves the frame
    // before it reaches the wall, keeps more of its environment.
    let edge = shot.pixel(1, H - 1).expect("inside");
    assert!(edge[0] > floor[0], "{edge:?} against {floor:?}");
}
