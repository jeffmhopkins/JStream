//! Channels (ADR-0183): `channel.set` and `channel.delete`, their checks and inverses,
//! cycles through `channel` inputs, and the collection file.

use super::{apply_err, apply_ok, fixture, invalid_reason};
use jstream_protocol::{
    ChannelInfo, Edit, EntityKind, Finite32, MotionCurve, MotionKind, ProtocolError,
};

fn reading(id: &str) -> MotionKind {
    MotionKind::Channel {
        channel_id: id.into(),
        smoothing_ms: 0,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

fn channel(id: &str, reads: &[&str]) -> Edit {
    let mut channel = ChannelInfo::new(id);
    channel.inputs = reads.iter().map(|r| reading(r)).collect();
    Edit::ChannelSet { channel }
}

#[test]
fn a_channel_is_set_replaced_and_deleted_with_inverses() {
    let mut g = fixture();
    apply_ok(&mut g, vec![channel("power", &[])]);
    assert_eq!(g.channels().len(), 1);
    let mut lights = ChannelInfo::new("lights");
    lights.name = "Lights".into();
    lights.rise_ms = 800;
    lights.inputs = vec![reading("power")];
    apply_ok(
        &mut g,
        vec![Edit::ChannelSet {
            channel: lights.clone(),
        }],
    );
    assert_eq!(g.channel("lights"), Some(&lights));
    lights.on = false;
    apply_ok(
        &mut g,
        vec![Edit::ChannelSet {
            channel: lights.clone(),
        }],
    );
    assert_eq!(g.channel("lights").map(|c| c.on), Some(false));
    // Deleting `power` leaves `lights` reading it: a missing channel reads 0.
    apply_ok(
        &mut g,
        vec![Edit::ChannelDelete {
            channel_id: "power".into(),
        }],
    );
    assert_eq!(g.channel("lights"), Some(&lights));
    let missing = apply_err(
        &mut g,
        vec![Edit::ChannelDelete {
            channel_id: "power".into(),
        }],
    );
    assert!(matches!(
        missing,
        ProtocolError::NotFound {
            entity: EntityKind::Channel,
            ..
        }
    ));
}

#[test]
fn a_channel_input_never_leads_back_to_it() {
    let mut g = fixture();
    let own = apply_err(&mut g, vec![channel("a", &["a"])]);
    assert_eq!(invalid_reason(own).0, "edits.0.channel.inputs.0.channel_id");
    apply_ok(&mut g, vec![channel("a", &["b"]), channel("b", &["c"])]);
    let around = apply_err(&mut g, vec![channel("c", &["a"])]);
    assert_eq!(invalid_reason(around).0, "edits.0.channel.inputs");
    apply_ok(&mut g, vec![channel("c", &[])]);
}

#[test]
fn a_channel_reads_a_controller_a_data_field_or_a_channel_only() {
    let mut g = fixture();
    let mut wiggle = ChannelInfo::new("w");
    wiggle.inputs = vec![MotionKind::Wiggle {
        amplitude: Finite32::ONE,
        frequency_hz: Finite32::ONE,
        seed: 0,
        octaves: 1,
    }];
    let refused = apply_err(&mut g, vec![Edit::ChannelSet { channel: wiggle }]);
    assert_eq!(invalid_reason(refused).0, "edits.0.channel.inputs.0.kind");
    let bad_id = apply_err(&mut g, vec![channel("no spaces", &[])]);
    assert_eq!(invalid_reason(bad_id).0, "edits.0.channel.channel_id");
}

#[test]
fn channels_survive_the_file() {
    let mut g = fixture();
    apply_ok(
        &mut g,
        vec![channel("power", &[]), channel("lights", &["power"])],
    );
    let loaded = crate::Graph::new()
        .with_kinds(super::kinds())
        .with_collection_json(&g.to_collection_json())
        .expect("loads");
    assert_eq!(loaded.state(), g.state());
}

/// ADR-0183 decision 6: a key binds to a channel's actions once the channel exists.
#[test]
fn a_key_binds_to_a_channel_that_exists() {
    let mut g = fixture();
    let toggle = |id: &str| Edit::HotkeySet {
        chord: jstream_protocol::Chord::new("F8"),
        action: jstream_protocol::Action::ChannelSwitch {
            channel_id: id.into(),
            to: jstream_protocol::ChannelSwitch::Toggle,
        },
    };
    let missing = apply_err(&mut g, vec![toggle("lights")]);
    assert!(matches!(
        missing,
        ProtocolError::NotFound {
            entity: EntityKind::Channel,
            ..
        }
    ));
    apply_ok(&mut g, vec![channel("lights", &[]), toggle("lights")]);
}
