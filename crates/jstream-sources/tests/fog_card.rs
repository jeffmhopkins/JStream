//! The `fog_card` kind through the public API: a generated card of fog whose alpha
//! thins out raggedly toward its edges.

use jstream_media::{MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_protocol::Settings;
use jstream_sources::{Context, Registry, SourceError, needs_restart};
use serde_json::{Value, json};

const W: u32 = 320;
const H: u32 = 180;

/// The kind's defaults at a small size, with `patch` over them.
fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("fog_card").expect("the fog_card kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    settings.insert("width", json!(W));
    settings.insert("height", json!(H));
    for (id, value) in patch {
        settings.insert(*id, value.clone());
    }
    settings
}

fn draw(patch: &[(&str, Value)]) -> VideoFrame {
    let mut source = Registry::builtin()
        .create("fog_card", &settings(patch), &Context::default())
        .expect("a fog card");
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

/// The mean alpha, 0 to 255, over the pixels `keep` picks by (x, y).
fn mean_alpha(frame: &VideoFrame, keep: impl Fn(u32, u32) -> bool) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in pixels(frame).iter().enumerate() {
        if keep(i as u32 % W, i as u32 / W) {
            sum += f64::from(p[3]);
            n += 1.0;
        }
    }
    sum / n
}

/// How far a pixel is from the nearest edge.
fn from_edge(x: u32, y: u32) -> u32 {
    x.min(W - 1 - x).min(y).min(H - 1 - y)
}

#[test]
fn the_descriptor_is_live_but_for_the_size() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("fog_card").expect("the kind");
    assert!(kind.video && !kind.audio);
    for p in &kind.params {
        assert_eq!(
            p.live,
            !matches!(p.id.as_str(), "width" | "height"),
            "{}",
            p.id
        );
    }
}

/// Every pixel is the fog's colour, straight, with the fog in its alpha, never above
/// `density`; the same settings draw the same card and another seed another.
#[test]
fn a_card_is_one_colour_with_the_fog_in_its_alpha() {
    let frame = draw(&[("color", json!("#4080c0")), ("density", json!(0.5))]);
    let px = pixels(&frame);
    assert!(px.iter().all(|p| p[..3] == [0x40, 0x80, 0xc0]));
    let most = px.iter().map(|p| p[3]).max().expect("pixels");
    assert!((100..=128).contains(&most), "the thickest fog: {most}");
    assert!(px.iter().any(|p| p[3] == 0), "some of the card is clear");
    let again = draw(&[("color", json!("#4080c0")), ("density", json!(0.5))]);
    assert_eq!(px, pixels(&again));
    let other = draw(&[("color", json!("#4080c0")), ("seed", json!(2))]);
    assert_ne!(px, pixels(&other));
    // No density is a clear card.
    let none = draw(&[("density", json!(0.0))]);
    assert!(pixels(&none).iter().all(|p| p[3] == 0));
}

/// The fog ends before the card does: the outermost pixels are clear, there is less
/// fog near the edges than in the middle, and where it ends is ragged, not a line a
/// fixed distance in.
#[test]
fn the_fog_thins_out_raggedly_toward_the_edges() {
    let frame = draw(&[("density", json!(1.0)), ("coverage", json!(0.8))]);
    assert_eq!(mean_alpha(&frame, |x, y| from_edge(x, y) == 0), 0.0);
    let near = mean_alpha(&frame, |x, y| from_edge(x, y) < 20);
    let middle = mean_alpha(&frame, |x, y| from_edge(x, y) >= 50);
    assert!(
        near * 4.0 < middle,
        "near the edges {near}, the middle {middle}"
    );
    // How far down each column the fog starts, along the top edge away from the corners.
    let px = pixels(&frame);
    let starts: Vec<u32> = (60..W - 60)
        .map(|x| {
            (0..H)
                .find(|&y| px[(y * W + x) as usize][3] > 8)
                .unwrap_or(H)
        })
        .collect();
    let (first, last) = (
        starts.iter().min().expect("columns"),
        starts.iter().max().expect("columns"),
    );
    assert!(last - first >= 10, "the fog's top edge: {first} to {last}");
    // With no edge fade the fog runs to the edges.
    let full = draw(&[
        ("density", json!(1.0)),
        ("coverage", json!(1.0)),
        ("edge", json!(0.0)),
    ]);
    assert!(mean_alpha(&full, |x, y| from_edge(x, y) == 0) > 120.0);
}

#[test]
fn coverage_and_scale_change_the_clouds() {
    let alpha = |coverage: f64| {
        mean_alpha(
            &draw(&[("coverage", json!(coverage)), ("density", json!(1.0))]),
            |_, _| true,
        )
    };
    let (none, some, most) = (alpha(0.0), alpha(0.5), alpha(1.0));
    assert_eq!(none, 0.0);
    assert!(some > 5.0 && most > some * 1.5, "{some} then {most}");
    // Larger clouds change less from one pixel to the next.
    let rough = |scale: f64| {
        let frame = draw(&[("scale", json!(scale)), ("density", json!(1.0))]);
        let px = pixels(&frame);
        let mut sum = 0.0;
        for y in 0..H {
            for x in 0..W - 8 {
                let at = |x: u32| f64::from(px[(y * W + x) as usize][3]);
                sum += (at(x) - at(x + 8)).abs();
            }
        }
        sum
    };
    assert!(rough(0.5) > rough(2.0) * 1.3);
}

#[test]
fn a_look_changes_live_and_a_size_restarts() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("fog_card").expect("the kind");
    let old = settings(&[]);
    assert!(!needs_restart(
        kind,
        &old,
        &settings(&[("density", json!(0.9))])
    ));
    assert!(needs_restart(
        kind,
        &old,
        &settings(&[("width", json!(64))])
    ));
    let mut source = registry
        .create("fog_card", &old, &Context::default())
        .expect("a fog card");
    let at = MediaTime::zero(Timebase::MILLIS);
    let before = pixels(&source.frame(at).expect("a frame"));
    source
        .update(&settings(&[("density", json!(0.9))]))
        .expect("a live update");
    assert_ne!(before, pixels(&source.frame(at).expect("a frame")));
    assert!(matches!(
        source.update(&settings(&[("width", json!(64))])),
        Err(SourceError::NotLive { .. })
    ));
}

#[test]
fn settings_out_of_range_are_refused_by_name() {
    let refused = |patch: &[(&str, Value)]| match Registry::builtin().create(
        "fog_card",
        &settings(patch),
        &Context::default(),
    ) {
        Err(SourceError::Setting { param, .. }) => param,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("accepted {patch:?}"),
    };
    assert_eq!(refused(&[("density", json!(1.5))]), "density");
    assert_eq!(refused(&[("color", json!("fog"))]), "color");
    assert_eq!(refused(&[("seed", json!(-1))]), "seed");
    assert_eq!(refused(&[("width", json!(4097))]), "width");
    assert_eq!(refused(&[("height", json!(5000))]), "height");
}
