//! The `light` kind: its settings, live changes, and the nothing it draws.

use super::{LightKind, LightType, ShownLight};
use crate::{Context, Source, SourceKind};
use jstream_protocol::Settings;
use serde_json::{Value, json};

fn settings(overrides: Value) -> Settings {
    let mut settings: Settings = LightKind::new()
        .descriptor()
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    if let Value::Object(map) = overrides {
        for (k, v) in map {
            settings.insert(k, v);
        }
    }
    settings
}

fn create(overrides: Value) -> Box<dyn Source> {
    LightKind::new()
        .create(&settings(overrides), &Context::default())
        .expect("a light")
}

#[test]
fn a_default_light_is_a_white_point_that_never_fades() {
    let mut source = create(json!({}));
    assert_eq!(
        source.light(),
        Some(ShownLight {
            kind: LightType::Point,
            color: [1.0; 3],
            intensity: 1.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        })
    );
    let frame = source
        .frame(jstream_media::MediaTime::zero(
            jstream_media::Timebase::MILLIS,
        ))
        .expect("a frame");
    assert_eq!(frame.plane(0).expect("a plane").data(), &[0, 0, 0, 0]);
    assert!(LightKind::new().descriptor().params.iter().all(|p| p.live));
}

#[test]
fn every_setting_changes_live_and_a_bad_one_is_refused() {
    let mut source = create(json!({}));
    source
        .update(&settings(json!({
            "type": "spot",
            "color": "#ff8000",
            "intensity": 4.0,
            "range_px": 900.0,
            "cone_deg": 30.0,
            "softness": 0.5,
        })))
        .expect("live");
    let light = source.light().expect("a light");
    assert_eq!(light.kind, LightType::Spot);
    assert_eq!(light.color, [1.0, 128.0 / 255.0, 0.0]);
    assert_eq!((light.intensity, light.range_px), (4.0, 900.0));
    assert_eq!((light.cone_deg, light.softness), (30.0, 0.5));
    for bad in [
        json!({ "type": "laser" }),
        json!({ "intensity": 65.0 }),
        json!({ "cone_deg": 0.0 }),
        json!({ "softness": -0.1 }),
        json!({ "color": "white" }),
    ] {
        assert!(source.update(&settings(bad.clone())).is_err(), "{bad}");
    }
    assert_eq!(
        source.light(),
        Some(light),
        "a refused change keeps the light"
    );
}

/// ADR-0151 decision 3: a light casts shadows when asked, with its softness; settings
/// saved before shadows existed have none.
#[test]
fn a_light_casts_shadows_when_asked() {
    let light = create(json!({"type": "spot", "shadows": true, "shadow_softness": 0.8}))
        .light()
        .expect("a light");
    assert!(light.shadows);
    assert!((light.shadow_softness - 0.8).abs() < 1e-6);
    let mut older = settings(json!({}));
    older.0.remove("shadows");
    older.0.remove("shadow_softness");
    let light = LightKind::new()
        .create(&older, &Context::default())
        .expect("a light")
        .light()
        .expect("a light");
    assert!(!light.shadows);
    assert!((light.shadow_softness - 0.3).abs() < 1e-6);
}

/// ADR-0151 decision 9: an environment light carries its sky, the defaults when its
/// settings predate it, and the source named as its panorama; another kind has none.
#[test]
fn an_environment_light_carries_its_sky() {
    let source = create(json!({"type": "environment", "sky_color": "#0000ff", "image": "hdri"}));
    let sky = source
        .light()
        .and_then(|light| light.environment)
        .expect("a sky");
    assert_eq!(sky.sky, [0.0, 0.0, 1.0]);
    assert_eq!(sky.horizon, [1.0; 3]);
    assert_eq!(sky.image, Some(jstream_protocol::SourceId::new("hdri")));
    // The parameter's type checks a source reference, which names it too.
    let referenced = create(json!({
        "type": "environment",
        "image": {"type": "source", "source_id": "hdri"},
    }));
    assert_eq!(
        referenced
            .light()
            .and_then(|light| light.environment)
            .and_then(|sky| sky.image),
        Some(jstream_protocol::SourceId::new("hdri"))
    );
    let mut older = settings(json!({"type": "environment"}));
    for id in ["sky_color", "horizon_color", "ground_color", "image"] {
        older.0.remove(id);
    }
    let older = LightKind::new()
        .create(&older, &Context::default())
        .expect("a light");
    let sky = older
        .light()
        .and_then(|light| light.environment)
        .expect("a sky");
    assert_eq!(sky.image, None);
    assert_eq!(sky.ground, [58.0 / 255.0; 3]);
    assert_eq!(create(json!({})).light().and_then(|l| l.environment), None);
    assert!(
        LightKind::new()
            .create(
                &settings(json!({"type": "environment", "image": 3})),
                &Context::default()
            )
            .is_err()
    );
}

/// ADR-0167: an environment light carries whether it is drawn as a backdrop and its
/// stars; settings that predate them read as no backdrop and no stars, and a value out
/// of range is refused.
#[test]
fn an_environment_light_carries_its_backdrop_and_stars() {
    let source = create(json!({
        "type": "environment",
        "backdrop": true,
        "stars": 0.5,
        "star_brightness": 2.0,
    }));
    let sky = source
        .light()
        .and_then(|light| light.environment)
        .expect("a sky");
    assert!(sky.backdrop);
    assert!((sky.stars - 0.5).abs() < 1e-6);
    assert!((sky.star_brightness - 2.0).abs() < 1e-6);
    let mut older = settings(json!({"type": "environment"}));
    for id in ["backdrop", "stars", "star_brightness"] {
        older.0.remove(id);
    }
    let sky = LightKind::new()
        .create(&older, &Context::default())
        .expect("a light")
        .light()
        .and_then(|light| light.environment)
        .expect("a sky");
    assert!(!sky.backdrop);
    assert_eq!(sky.stars, 0.0);
    assert!((sky.star_brightness - 1.0).abs() < 1e-6);
    for bad in [
        json!({"type": "environment", "stars": 2.0}),
        json!({"type": "environment", "star_brightness": -1.0}),
        json!({"type": "environment", "backdrop": "yes"}),
    ] {
        assert!(
            LightKind::new()
                .create(&settings(bad), &Context::default())
                .is_err()
        );
    }
}

/// ADR-0167: an environment light carries its backdrop's deep sky and warp; settings
/// that predate them read as none, still, seed 0 and the default colours.
#[test]
fn an_environment_light_carries_its_deep_sky_and_warp() {
    let sky = create(json!({
        "type": "environment",
        "nebula": 0.6,
        "nebula_color": "#ff0000",
        "galaxy": 0.4,
        "sky_seed": 42,
        "warp": 0.75,
        "warp_ramp_s": 3.0,
    }))
    .light()
    .and_then(|light| light.environment)
    .expect("a sky");
    assert!((sky.nebula - 0.6).abs() < 1e-6);
    assert_eq!(sky.nebula_colors[0], [1.0, 0.0, 0.0]);
    assert!((sky.galaxy - 0.4).abs() < 1e-6);
    assert_eq!(sky.sky_seed, 42);
    assert!((sky.warp - 0.75).abs() < 1e-6);
    assert!((sky.warp_ramp_s - 3.0).abs() < 1e-6);
    let mut older = settings(json!({"type": "environment"}));
    for id in [
        "nebula",
        "nebula_color",
        "nebula_color_2",
        "galaxy",
        "sky_seed",
        "warp",
        "warp_ramp_s",
    ] {
        older.0.remove(id);
    }
    let sky = LightKind::new()
        .create(&older, &Context::default())
        .expect("a light")
        .light()
        .and_then(|light| light.environment)
        .expect("a sky");
    assert_eq!(
        (sky.nebula, sky.galaxy, sky.sky_seed, sky.warp),
        (0.0, 0.0, 0, 0.0)
    );
    assert!((sky.warp_ramp_s - 1.5).abs() < 1e-6);
    assert_eq!(
        sky.nebula_colors[1],
        [36.0 / 255.0, 113.0 / 255.0, 168.0 / 255.0]
    );
    for bad in [
        json!({"type": "environment", "warp": 1.5}),
        json!({"type": "environment", "warp_ramp_s": 11.0}),
        json!({"type": "environment", "sky_seed": 10000}),
        json!({"type": "environment", "sky_seed": 1.5}),
        json!({"type": "environment", "nebula_color": "violet"}),
    ] {
        assert!(
            LightKind::new()
                .create(&settings(bad), &Context::default())
                .is_err()
        );
    }
}
