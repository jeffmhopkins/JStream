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
