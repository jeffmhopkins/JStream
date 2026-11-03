//! The `backdrop` kind through the public API (#1017's follow-up: generated studio
//! backdrops).

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::Settings;
use jstream_sources::{Context, Registry, SourceError, needs_restart};
use serde_json::{Value, json};

/// The backdrop kind's defaults at a small size, with `patch` over them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("backdrop").expect("the backdrop kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    settings.insert("width", json!(320));
    settings.insert("height", json!(180));
    for (id, value) in patch {
        settings.insert(*id, value.clone());
    }
    settings
}

fn draw(patch: &[(&str, Value)]) -> VideoFrame {
    let mut source = Registry::builtin()
        .create("backdrop", &settings(patch), &Context::default())
        .expect("a backdrop");
    (*source
        .frame(MediaTime::zero(Timebase::MILLIS))
        .expect("a frame"))
    .clone()
}

/// The frame's pixels as packed RGBA, row by row.
fn pixels(frame: &VideoFrame) -> Vec<[u8; 4]> {
    assert_eq!(frame.info().format, PixelFormat::Rgba8);
    let plane = &frame.planes()[0];
    let (w, h) = (frame.info().size.width(), frame.info().size.height());
    (0..h as usize)
        .flat_map(|y| {
            plane.data()[y * plane.stride()..][..w as usize * 4]
                .chunks(4)
                .map(|p| p.try_into().expect("4 bytes"))
                .collect::<Vec<[u8; 4]>>()
        })
        .collect()
}

/// The mean of the three colour channels over the columns `xs` of every row.
fn brightness(frame: &VideoFrame, xs: std::ops::Range<u32>) -> f64 {
    let w = frame.info().size.width();
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in pixels(frame).iter().enumerate() {
        if xs.contains(&(i as u32 % w)) {
            sum += f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]);
            n += 3.0;
        }
    }
    sum / n
}

/// How much the picture varies over a few pixels: the mean absolute difference between
/// pixels 8 apart, after the grain is averaged out over 4×4 blocks.
fn mottle(frame: &VideoFrame) -> f64 {
    let (w, h) = (
        frame.info().size.width() as usize,
        frame.info().size.height() as usize,
    );
    let px = pixels(frame);
    let block = |bx: usize, by: usize| {
        let mut sum = 0.0;
        for y in by * 4..by * 4 + 4 {
            for x in bx * 4..bx * 4 + 4 {
                sum += f64::from(px[y * w + x][1]);
            }
        }
        sum / 16.0
    };
    let (bw, bh) = (w / 4, h / 4);
    let (mut sum, mut n) = (0.0, 0.0);
    for by in 0..bh {
        for bx in 0..bw - 2 {
            sum += (block(bx, by) - block(bx + 2, by)).abs();
            n += 1.0;
        }
    }
    sum / n
}

#[test]
fn the_descriptor_marks_everything_but_the_size_live() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("backdrop").expect("the backdrop kind");
    let ids: Vec<(&str, bool)> = kind
        .params
        .iter()
        .map(|p| (p.id.as_str(), p.live))
        .collect();
    assert_eq!(
        ids,
        [
            ("style", true),
            ("color", true),
            ("accent", true),
            ("texture", true),
            ("scale", true),
            ("light", true),
            ("light_x", true),
            ("light_y", true),
            ("vignette", true),
            ("grain", true),
            ("seed", true),
            ("width", false),
            ("height", false),
        ]
    );
    assert!(kind.video && !kind.audio);
}

/// A backdrop is opaque, the size it is set to, and the same every time for the same
/// settings; another seed draws another one.
#[test]
fn a_backdrop_is_opaque_and_repeatable() {
    let a = draw(&[]);
    assert_eq!((a.info().size.width(), a.info().size.height()), (320, 180));
    assert!(pixels(&a).iter().all(|p| p[3] == 255));
    assert_eq!(pixels(&a), pixels(&draw(&[])));
    assert_ne!(pixels(&a), pixels(&draw(&[("seed", json!(2))])));
}

/// The light falls where `light_x` says, and the vignette darkens the edges.
#[test]
fn the_light_and_vignette_shape_the_brightness() {
    let left = draw(&[("light_x", json!(0.15)), ("texture", json!(0.0))]);
    let right = draw(&[("light_x", json!(0.85)), ("texture", json!(0.0))]);
    assert!(brightness(&left, 0..80) > brightness(&left, 240..320));
    assert!(brightness(&right, 240..320) > brightness(&right, 0..80));

    let flat = draw(&[("vignette", json!(0.0)), ("light", json!(0.0))]);
    let dark = draw(&[("vignette", json!(1.0)), ("light", json!(0.0))]);
    assert!(brightness(&dark, 0..20) < brightness(&flat, 0..20) * 0.8);
}

/// Painted canvas mottles; seamless paper barely does; no texture is smoother still.
#[test]
fn the_style_and_texture_set_how_much_it_mottles() {
    // Without light, falloff or grain, only the mottle varies.
    let only = |patch: &[(&str, Value)]| {
        let mut all = vec![
            ("light", json!(0.0)),
            ("vignette", json!(0.0)),
            ("grain", json!(0.0)),
        ];
        all.extend_from_slice(patch);
        mottle(&draw(&all))
    };
    let canvas = only(&[("style", json!("canvas"))]);
    let paper = only(&[("style", json!("paper"))]);
    let plain = only(&[("style", json!("canvas")), ("texture", json!(0.0))]);
    assert!(canvas > paper * 2.0, "canvas {canvas} vs paper {paper}");
    assert!(paper > plain, "paper {paper} vs none {plain}");
}

/// Every look setting is live; the size needs a restart; a bad value is an error naming
/// it.
#[test]
fn looks_are_live_and_the_size_restarts() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("backdrop").expect("the backdrop kind");
    let old = settings(&[]);
    let mut source = registry
        .create("backdrop", &old, &Context::default())
        .expect("a backdrop");
    let new = settings(&[("color", json!("#884422")), ("style", json!("muslin"))]);
    assert!(!needs_restart(kind, &old, &new));
    source.update(&new).expect("a live update");
    let frame = source
        .frame(MediaTime::zero(Timebase::MILLIS))
        .expect("a frame");
    assert_eq!(
        pixels(&frame),
        pixels(&draw(&[
            ("color", json!("#884422")),
            ("style", json!("muslin")),
        ]))
    );

    let bigger = settings(&[("width", json!(640))]);
    assert!(needs_restart(kind, &old, &bigger));
    assert!(matches!(
        source.update(&bigger),
        Err(SourceError::NotLive { param }) if param == "width"
    ));
    assert!(matches!(
        source.update(&settings(&[("style", json!("velvet"))])),
        Err(SourceError::Setting { param, .. }) if param == "style"
    ));
}
