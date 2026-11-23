//! The `reflection_probe` kind: its settings, live changes, and the nothing it draws.

use super::{ProbeKind, ShownProbe};
use crate::{Context, Source, SourceKind};
use jstream_protocol::Settings;
use serde_json::{Value, json};

fn settings(overrides: Value) -> Settings {
    let mut settings: Settings = ProbeKind::new()
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
    ProbeKind::new()
        .create(&settings(overrides), &Context::default())
        .expect("a probe")
}

#[test]
fn a_default_probe_captures_once_at_256() {
    let mut source = create(json!({}));
    assert_eq!(
        source.probe(),
        Some(ShownProbe {
            size_px: [2000.0; 3],
            fade_px: 100.0,
            resolution: 256,
            refresh_frames: 0,
        })
    );
    assert_eq!(source.light(), None);
    let frame = source
        .frame(jstream_media::MediaTime::zero(
            jstream_media::Timebase::MILLIS,
        ))
        .expect("a frame");
    assert_eq!(frame.plane(0).expect("a plane").data(), &[0, 0, 0, 0]);
    assert!(ProbeKind::new().descriptor().params.iter().all(|p| p.live));
}

#[test]
fn every_setting_changes_live_and_a_bad_one_is_refused() {
    let mut source = create(json!({}));
    source
        .update(&settings(json!({
            "width_px": 800.0,
            "height_px": 600.0,
            "depth_px": 400.0,
            "fade_px": 0.0,
            "resolution": "512",
            "refresh_frames": 30,
        })))
        .expect("live settings apply");
    assert_eq!(
        source.probe(),
        Some(ShownProbe {
            size_px: [800.0, 600.0, 400.0],
            fade_px: 0.0,
            resolution: 512,
            refresh_frames: 30,
        })
    );
    for bad in [
        json!({"resolution": "300"}),
        json!({"width_px": 0.0}),
        json!({"refresh_frames": 601}),
        json!({"fade_px": -1.0}),
    ] {
        assert!(source.update(&settings(bad.clone())).is_err(), "{bad}");
    }
}
