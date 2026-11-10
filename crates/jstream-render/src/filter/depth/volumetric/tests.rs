//! `volumetric_fog` (ADR-0151 decision 6): its settings, and its fog against depth images
//! of known distances seen through the framing camera: even fog, the height falloff,
//! the drift, and a scene light lighting it.

use std::sync::Arc;

use jstream_protocol::{Canvas, SceneCamera};
use serde_json::{Value, json};

use super::{KIND, Values};
use crate::filter::testing::{filter, read, settings};
use crate::filter::{FilterClock, ViewDepth};
use crate::image;
use crate::tests::{Opened, near, open, solid};
use crate::{Color, Image, LightKind, SceneLight, Snapshot};

const W: u32 = 32;
const H: u32 = 16;
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn settings_map_to_values() {
    let v = super::values(&settings(KIND, json!({})));
    assert_eq!(
        v,
        Values {
            color: v.color,
            density: 0.5,
            base_y: 1080.0,
            falloff: 400.0,
            start: 0.0,
            farthest: 8000.0,
            noise: 0.3,
            noise_scale: 300.0,
            wind: [20.0, 0.0, 0.0],
            scattering: 1.0,
            steps: 48,
        }
    );
    let v = super::values(&settings(
        KIND,
        json!({"density": 99.0, "noise": -1.0, "noise_scale_px": 0.0, "quality": "high",
               "scattering": 50.0}),
    ));
    assert_eq!(
        (v.density, v.noise, v.noise_scale, v.scattering),
        (20.0, 0.0, 1.0, 8.0)
    );
    assert_eq!(v.steps, 96);
    assert_eq!(
        super::values(&settings(KIND, json!({"quality": "low"}))).steps,
        24
    );
}

/// ADR-0151 decision 6: the drift is wind times time in noise features, wrapped so a
/// long run keeps its precision.
#[test]
fn the_drift_wraps_to_the_noise_period() {
    let v = super::values(&settings(
        KIND,
        json!({"wind_x_px": 30.0, "noise_scale_px": 10.0}),
    ));
    let at = |seconds| super::drift(&v, FilterClock { seconds, beat: 0.0 });
    assert_eq!(at(0.0), [0.0; 3]);
    assert!((at(2.0)[0] - 6.0).abs() < 1e-6);
    // 256 features later it is where it was.
    let later = at(2.0 + 256.0 * 10.0 / 30.0);
    assert!((later[0] - 6.0).abs() < 1e-3, "{later:?}");
    assert!(at(1e12)[0] < 256.0);
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

/// A view depth with every pixel at distance `d` (0 for nothing drawn) through the
/// framing camera, lit by `lights`.
fn depth(opened: &Opened, d: f32, lights: &[SceneLight]) -> ViewDepth {
    let view = view();
    let f = view.focal_px();
    let texel = if d > 0.0 {
        [f / d, 0.0, 0.0, 1.0]
    } else {
        [0.0; 4]
    };
    let texels = vec![texel; (W * H) as usize];
    let image = image::upload_unit_f16(&opened.compositor.maker(), W, H, &texels)
        .expect("the depth uploads");
    ViewDepth {
        view: Some(view),
        lights: Arc::from(lights),
        ..ViewDepth::new(Arc::new(image), f)
    }
}

/// Even, still, unlit fog: only density and start, as `extra` changes them.
fn even(extra: Value) -> crate::VideoFilter {
    let mut all = json!({
        "color": "#ffffff", "falloff_px": 0.0, "noise": 0.0, "scattering": 0.0,
        "quality": "high"
    });
    if let (Value::Object(all), Value::Object(more)) = (&mut all, extra) {
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

/// ADR-0151 decision 6: with no view depth, or one without its camera, it changes
/// nothing.
#[test]
fn without_a_scene_view_it_passes_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, [40, 90, 200, 255]);
    let mut fog = even(json!({}));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, 0, 0, [40, 90, 200, 255], 1);
    let mut no_camera = depth(&opened, 2000.0, &[]);
    no_camera.view = None;
    fog.set_depth(Some(no_camera));
    let shot = apply(&mut opened, &mut fog, &input);
    near(&shot, W / 2, H / 2, [40, 90, 200, 255], 1);
}

/// ADR-0151 decision 6: even fog lets through exp(−density · ray length / 1000), from
/// its start to the depth, or to its farthest where nothing was drawn.
#[test]
fn even_fog_follows_the_ray_length() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, BLACK);
    let mut fog = even(json!({"density": 0.5}));
    fog.set_depth(Some(depth(&opened, 2000.0, &[])));
    let shot = apply(&mut opened, &mut fog, &input);
    // Through the centre the ray runs along the view: 2000 px.
    near(&shot, W / 2, H / 2, grey(1.0 - (-1.0f32).exp()), 2);
    let mut started = even(json!({"density": 0.5, "start_px": 1000.0}));
    started.set_depth(Some(depth(&opened, 2000.0, &[])));
    let shot = apply(&mut opened, &mut started, &input);
    near(&shot, W / 2, H / 2, grey(1.0 - (-0.5f32).exp()), 2);
    // Where nothing was drawn it marches to its farthest.
    let mut empty = even(json!({"density": 0.5, "max_px": 4000.0}));
    empty.set_depth(Some(depth(&opened, 0.0, &[])));
    let shot = apply(&mut opened, &mut empty, &input);
    near(&shot, W / 2, H / 2, grey(1.0 - (-2.0f32).exp()), 2);
    // Nearer than its start nothing is fogged.
    let mut late = even(json!({"start_px": 3000.0}));
    late.set_depth(Some(depth(&opened, 2000.0, &[])));
    let shot = apply(&mut opened, &mut late, &input);
    near(&shot, W / 2, H / 2, BLACK, 1);
}

/// ADR-0151 decision 6: fog thins above its base by the falloff, so a sharp falloff far
/// below the camera's rays leaves them nearly clear.
#[test]
fn the_falloff_thins_fog_above_its_base() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, BLACK);
    let mut low = even(json!({"density": 2.0, "base_y_px": 5000.0, "falloff_px": 50.0}));
    low.set_depth(Some(depth(&opened, 2000.0, &[])));
    let shot = apply(&mut opened, &mut low, &input);
    near(&shot, W / 2, H / 2, BLACK, 2);
    // A base above everything makes it as dense everywhere.
    let mut high = even(json!({"density": 2.0, "base_y_px": -5000.0, "falloff_px": 50.0}));
    high.set_depth(Some(depth(&opened, 2000.0, &[])));
    let shot = apply(&mut opened, &mut high, &input);
    near(&shot, W / 2, H / 2, grey(1.0 - (-4.0f32).exp()), 2);
}

/// ADR-0151 decision 6: a scene light lights the fog, black on its own, that its light
/// reaches; one out of reach doesn't.
#[test]
fn a_scene_light_lights_the_fog() {
    let Some(mut opened) = open() else {
        return;
    };
    let input = solid(&opened.compositor, W, H, BLACK);
    let camera = view().position();
    // On the centre ray, halfway to the depth.
    let f = f64::from(view().focal_px());
    let light = |range_px| SceneLight {
        kind: LightKind::Point,
        position: [camera[0], camera[1], camera[2] - f - 500.0],
        aim: [0.0, 0.0, -1.0],
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
        intensity: 4.0,
        range_px,
        cone_deg: 60.0,
        softness: 0.3,
    };
    let black = json!({"color": "#000000ff", "density": 0.5, "scattering": 1.0});
    let mut lit = even(black.clone());
    lit.set_depth(Some(depth(&opened, (f + 1000.0) as f32, &[light(0.0)])));
    let shot = apply(&mut opened, &mut lit, &input);
    let pixel = shot.pixel(W / 2, H / 2).expect("inside");
    assert!(pixel[0] > 40, "the light reaches the fog: {pixel:?}");
    let mut dark = even(black);
    dark.set_depth(Some(depth(&opened, (f + 1000.0) as f32, &[])));
    let shot = apply(&mut opened, &mut dark, &input);
    near(&shot, W / 2, H / 2, BLACK, 1);
}
