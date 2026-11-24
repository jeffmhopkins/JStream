//! Value bindings (ADR-0178): `binding.set_value` and `binding.clear_value`, their
//! checks and inverses, the setter edit a binding gives its target, and the collection
//! file.

use super::{apply_err, apply_ok, fixture, invalid_reason, tone};
use crate::{ValueRange, value_edit};
use jstream_protocol::{
    BusId, Edit, EntityKind, FilterChain, FilterId, FilterOwner, Finite32, MotionCurve, MotionKind,
    MotionMidiControl, ProtocolError, Settings, SourceId, ValueBinding, ValueTarget,
};
use serde_json::json;

fn fader(controller: u8) -> MotionKind {
    MotionKind::Midi {
        control: MotionMidiControl::Cc,
        controller: Some(controller),
        channel: None,
        input: None,
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

fn bind(id: &str, target: ValueTarget) -> Edit {
    Edit::BindingSetValue {
        binding: ValueBinding {
            enabled: true,
            mode: jstream_protocol::MappingMode::Replace,
            binding_id: id.into(),
            input: fader(7),
            target,
            amount: None,
        },
    }
}

fn tint_on_cam() -> Edit {
    Edit::FilterAdd {
        owner: FilterOwner::Source {
            source_id: SourceId::new("cam"),
        },
        chain: FilterChain::Video,
        filter_id: FilterId::new("t1"),
        kind: "tint".into(),
        settings: Settings::new(),
        enabled: None,
        index: None,
    }
}

fn tint_amount() -> ValueTarget {
    ValueTarget::FilterSetting {
        owner: FilterOwner::Source {
            source_id: SourceId::new("cam"),
        },
        filter_id: FilterId::new("t1"),
        param: "amount".into(),
    }
}

#[test]
fn a_binding_is_set_listed_replaced_and_cleared_with_inverses() {
    let mut g = fixture();
    apply_ok(&mut g, vec![tint_on_cam()]);
    let applied = apply_ok(&mut g, vec![bind("knob", tint_amount())]);
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::BindingClearValue {
            binding_id: "knob".into()
        }]
    );
    assert_eq!(g.value_bindings().len(), 1);
    let size = ValueTarget::SourceSetting {
        source_id: SourceId::new("cam"),
        param: "size".into(),
    };
    let applied = apply_ok(&mut g, vec![bind("knob", size.clone())]);
    assert!(matches!(
        &applied.inverse.edits[..],
        [Edit::BindingSetValue { binding }] if binding.target == tint_amount()
    ));
    assert_eq!(g.value_bindings().next().map(|b| &b.target), Some(&size));
    apply_ok(
        &mut g,
        vec![Edit::BindingClearValue {
            binding_id: "knob".into(),
        }],
    );
    assert_eq!(g.value_bindings().len(), 0);
}

#[test]
fn a_binding_needs_a_target_that_takes_a_live_number() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![
            tint_on_cam(),
            tone("hum"),
            Edit::BusCreate {
                bus_id: BusId::new("fx"),
                name: "FX".into(),
            },
        ],
    );
    let missing = apply_err(
        &mut g,
        vec![bind(
            "k",
            ValueTarget::BusGain {
                bus_id: BusId::new("nope"),
            },
        )],
    );
    assert!(matches!(
        missing,
        ProtocolError::NotFound {
            entity: EntityKind::Bus,
            ..
        }
    ));
    let colour = apply_err(
        &mut g,
        vec![bind(
            "k",
            ValueTarget::SourceSetting {
                source_id: SourceId::new("cam"),
                param: "color".into(),
            },
        )],
    );
    assert_eq!(invalid_reason(colour).0, "edits.0.binding.target.param");
    // A `scene` source has no fader: the setter refuses it, so the binding is refused.
    let scene_fader = apply_err(
        &mut g,
        vec![bind(
            "k",
            ValueTarget::SourceGain {
                source_id: SourceId::new("sa"),
            },
        )],
    );
    assert_eq!(invalid_reason(scene_fader).0, "edits.0.binding.target");
    // A send that doesn't exist is refused, though `send.set` would make it.
    let send = apply_err(
        &mut g,
        vec![bind(
            "k",
            ValueTarget::SendGain {
                source_id: SourceId::new("hum"),
                bus_id: BusId::new("fx"),
            },
        )],
    );
    assert_eq!(invalid_reason(send).0, "edits.0.binding.target");
    apply_ok(
        &mut g,
        vec![
            bind(
                "fader",
                ValueTarget::SourceGain {
                    source_id: SourceId::new("hum"),
                },
            ),
            bind(
                "master",
                ValueTarget::BusGain {
                    bus_id: BusId::master(),
                },
            ),
            bind(
                "send",
                ValueTarget::SendGain {
                    source_id: SourceId::new("hum"),
                    bus_id: BusId::master(),
                },
            ),
        ],
    );
}

#[test]
fn a_binding_reads_a_controller_or_a_data_field_only() {
    let mut g = fixture();
    apply_ok(&mut g, vec![tint_on_cam()]);
    let wiggle = apply_err(
        &mut g,
        vec![Edit::BindingSetValue {
            binding: ValueBinding {
                enabled: true,
                mode: jstream_protocol::MappingMode::Replace,
                binding_id: "k".into(),
                input: MotionKind::Drift {
                    rate_per_s: Finite32::ONE,
                    wrap: None,
                },
                target: tint_amount(),
                amount: None,
            },
        }],
    );
    assert_eq!(invalid_reason(wiggle).0, "edits.0.binding.kind");
    let bad_id = apply_err(&mut g, vec![bind("no spaces", tint_amount())]);
    assert_eq!(invalid_reason(bad_id).0, "edits.0.binding.binding_id");
}

#[test]
fn a_binding_stays_when_its_target_goes_and_survives_the_file() {
    let mut g = fixture();
    apply_ok(&mut g, vec![tint_on_cam(), bind("knob", tint_amount())]);
    apply_ok(
        &mut g,
        vec![Edit::FilterRemove {
            owner: FilterOwner::Source {
                source_id: SourceId::new("cam"),
            },
            filter_id: FilterId::new("t1"),
        }],
    );
    assert_eq!(g.value_bindings().len(), 1);
    assert_eq!(g.value_range(&tint_amount()), None);
    let loaded = crate::Graph::new()
        .with_kinds(super::kinds())
        .with_collection_json(&g.to_collection_json())
        .expect("loads");
    assert_eq!(loaded.state(), g.state());
}

#[test]
fn the_setter_keeps_the_value_within_the_range() {
    let whole = ValueRange::Number {
        min: 1.0,
        max: 100.0,
        whole: true,
    };
    let Some(Edit::SourceSetSettings { patch, .. }) = value_edit(
        &ValueTarget::SourceSetting {
            source_id: SourceId::new("cam"),
            param: "size".into(),
        },
        whole,
        250.4,
    ) else {
        panic!("a setting's setter");
    };
    assert_eq!(patch.get("size"), Some(&json!(100)));
    assert_eq!(ValueRange::Switch.json(0.6), json!(true));
    assert_eq!(ValueRange::Switch.json(0.4), json!(false));
    let fader = ValueRange::Number {
        min: -100.0,
        max: 24.0,
        whole: false,
    };
    let Some(Edit::BusSetFader { gain_db, .. }) = value_edit(
        &ValueTarget::BusGain {
            bus_id: BusId::master(),
        },
        fader,
        -6.0,
    ) else {
        panic!("a fader's setter");
    };
    assert_eq!(gain_db.map(Finite32::get), Some(-6.0));
}

/// ADR-0209: a PTZ axis takes whole head counts over its protocol range, names a
/// camera the collection can't look up (it belongs to the machine), and gives no setter
/// edit, since the engine aims the head instead.
#[test]
fn a_ptz_axis_is_bound_without_a_lookup_and_gives_no_edit() {
    use jstream_protocol::PtzAxis;
    let mut g = fixture();
    let zoom = ValueTarget::PtzAxis {
        camera: "any camera".into(),
        axis: PtzAxis::Zoom,
    };
    let applied = apply_ok(&mut g, vec![bind("zoom", zoom.clone())]);
    assert_eq!(
        applied.inverse.edits,
        vec![Edit::BindingClearValue {
            binding_id: "zoom".into()
        }]
    );
    let range = g.value_range(&zoom).expect("a range");
    assert_eq!(
        range,
        ValueRange::Number {
            min: 0.0,
            max: 65535.0,
            whole: true
        }
    );
    assert_eq!(value_edit(&zoom, range, 100.0), None);
    let pan = ValueTarget::PtzAxis {
        camera: "any camera".into(),
        axis: PtzAxis::Pan,
    };
    assert!(matches!(
        g.value_range(&pan),
        Some(ValueRange::Number { min, .. }) if min == -524_287.0
    ));
    let unnamed = apply_err(
        &mut g,
        vec![bind(
            "bad",
            ValueTarget::PtzAxis {
                camera: String::new(),
                axis: PtzAxis::Tilt,
            },
        )],
    );
    assert!(
        matches!(&unnamed, ProtocolError::InvalidArgument { field, .. } if field.ends_with("binding.target.camera")),
        "{unnamed:?}"
    );
}
