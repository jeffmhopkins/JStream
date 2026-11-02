//! The `shape` kind through the public API: plates, pills, circles and gradients drawn
//! at an exact size.

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::Settings;
use jstream_sources::{Context, Registry, SourceError, needs_restart};
use serde_json::{Value, json};

/// The shape kind's defaults at 100 × 40, with `patch` over them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("shape").expect("the shape kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    settings.insert("width", json!(100));
    settings.insert("height", json!(40));
    for (id, value) in patch {
        settings.insert(*id, value.clone());
    }
    settings
}

fn draw(patch: &[(&str, Value)]) -> VideoFrame {
    let mut source = Registry::builtin()
        .create("shape", &settings(patch), &Context::default())
        .expect("a shape");
    (*source
        .frame(MediaTime::zero(Timebase::MILLIS))
        .expect("a frame"))
    .clone()
}

fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    assert_eq!(frame.info().format, PixelFormat::Rgba8);
    let plane = &frame.planes()[0];
    let at = y as usize * plane.stride() + x as usize * 4;
    plane.data()[at..at + 4].try_into().expect("4 bytes")
}

#[test]
fn a_rectangle_fills_its_exact_size() {
    let frame = draw(&[("color", json!("#c8102eff"))]);
    assert_eq!(
        (frame.info().size.width(), frame.info().size.height()),
        (100, 40)
    );
    assert_eq!(pixel(&frame, 0, 0), [200, 16, 46, 255]);
    assert_eq!(pixel(&frame, 99, 39), [200, 16, 46, 255]);
}

#[test]
fn a_large_radius_makes_a_pill_with_soft_edges() {
    let frame = draw(&[("radius", json!(1000.0))]);
    // The corners are outside the round ends; the middle and the ends' tips are in.
    assert_eq!(pixel(&frame, 0, 0)[3], 0);
    assert_eq!(pixel(&frame, 99, 39)[3], 0);
    assert_eq!(pixel(&frame, 50, 20)[3], 255);
    // The tip's pixel is all but covered: its centre is half a pixel inside.
    assert!(pixel(&frame, 0, 20)[3] > 240);
    // Along the round end's edge, coverage is partial: anti-aliased, not stepped.
    let edge = (0..20).map(|y| pixel(&frame, 3, y)[3]);
    assert!(
        edge.clone().any(|a| a > 0 && a < 255),
        "{:?}",
        edge.collect::<Vec<_>>()
    );
}

#[test]
fn an_ellipse_in_a_square_is_a_circle() {
    let frame = draw(&[
        ("shape", json!("ellipse")),
        ("width", json!(40)),
        ("height", json!(40)),
    ]);
    assert_eq!(pixel(&frame, 0, 0)[3], 0);
    assert_eq!(pixel(&frame, 20, 20)[3], 255);
    // Symmetric: the four points on its axes, just inside the edge, are covered alike.
    let a = pixel(&frame, 1, 20)[3];
    assert!(a > 0);
    assert_eq!(pixel(&frame, 38, 20)[3], a);
    assert_eq!(pixel(&frame, 20, 1)[3], a);
    assert_eq!(pixel(&frame, 20, 38)[3], a);
}

#[test]
fn a_linear_gradient_runs_its_angle_without_banding() {
    let across = draw(&[
        ("fill", json!("linear")),
        ("angle", json!(0.0)),
        ("color", json!("#000000ff")),
        ("color2", json!("#ffffffff")),
    ]);
    let left = pixel(&across, 0, 20)[0];
    let right = pixel(&across, 99, 20)[0];
    // The end pixels' centres are half a pixel in, which linear light lifts off black.
    assert!(left < 30 && right > 245, "{left} → {right}");
    // It rises left to right, never falling by more than the dither.
    let row: Vec<u8> = (0..100).map(|x| pixel(&across, x, 20)[0]).collect();
    assert!(
        row.windows(2)
            .all(|w| u16::from(w[1]) + 2 >= u16::from(w[0])),
        "{row:?}"
    );
    // Mixed in linear light: the middle is brighter than sRGB's halfway 128.
    assert!(row[50] > 160, "{}", row[50]);
    let down = draw(&[
        ("fill", json!("linear")),
        ("angle", json!(90.0)),
        ("color", json!("#000000ff")),
        ("color2", json!("#ffffffff")),
    ]);
    assert!(pixel(&down, 50, 0)[0] < pixel(&down, 50, 39)[0]);
    assert!(pixel(&down, 0, 20)[0].abs_diff(pixel(&down, 99, 20)[0]) <= 2);
}

#[test]
fn a_radial_gradient_is_brightest_in_the_middle() {
    let frame = draw(&[
        ("fill", json!("radial")),
        ("color", json!("#ffffffff")),
        ("color2", json!("#000000ff")),
    ]);
    assert!(pixel(&frame, 50, 20)[0] > 245);
    assert!(pixel(&frame, 2, 20)[0] < pixel(&frame, 25, 20)[0]);
}

#[test]
fn a_border_sits_inside_the_edge() {
    let frame = draw(&[
        ("color", json!("#000000ff")),
        ("border_width", json!(4.0)),
        ("border_color", json!("#ffffffff")),
    ]);
    assert_eq!(pixel(&frame, 1, 20), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 10, 20), [0, 0, 0, 255]);
}

#[test]
fn every_setting_is_live_and_bad_values_name_their_setting() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("shape").expect("the shape kind");
    let old = settings(&[]);
    let mut source = registry
        .create("shape", &old, &Context::default())
        .expect("a shape");
    let new = settings(&[("width", json!(60)), ("radius", json!(8.0))]);
    assert!(!needs_restart(kind, &old, &new));
    source.update(&new).expect("a live update");
    let frame = source
        .frame(MediaTime::zero(Timebase::MILLIS))
        .expect("a frame");
    assert_eq!(frame.info().size.width(), 60);
    assert!(matches!(
        source.update(&settings(&[("fill", json!("conic"))])),
        Err(SourceError::Setting { param, .. }) if param == "fill"
    ));
}
