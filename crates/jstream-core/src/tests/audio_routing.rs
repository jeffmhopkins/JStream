//! The program mixer follows the collection's audio (ADR-0028), and `meters.tick` and
//! `audio.reset_loudness` reach it, through `InProcessControl`. The mixer is attached
//! by hand, as the program stage does when it opens.

use super::{batch, block_on, connected, create, drain, edits, query};
use crate::Engine;
use crate::audio::{MasterMetering, MixerHandles, ScopeTaps, TapFeed};
use jstream_audio::{Mixer, MixerConfig};
use jstream_control::Control;
use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
use jstream_protocol::{
    Action, BusId, Call, CallResult, Edit, Event, Finite32, HistoryCall, HistoryOp, SceneId,
    SourceId, SourceKind,
};
use std::sync::Arc;

fn db(v: f32) -> Finite32 {
    Finite32::new(v).expect("finite")
}

#[test]
fn the_mixer_follows_edits_and_undo_and_meters_tick() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut events = control.events();
    let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (_producer, input) = mixer.add_input(format).expect("input");
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
        scopes: engine.audio_scopes(),
        taps: Arc::new(ScopeTaps::default()),
    });

    // A nested scene is the only audio source the builtin kinds give for now; it has
    // no fader, mute or sends of its own.
    batch(
        &control,
        edits(vec![
            create("inner"),
            Edit::SourceCreate {
                source_id: SourceId::new("nested"),
                name: "Nested".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("inner"),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
        ]),
    )
    .expect("created");
    engine.set_audio_input(SourceId::new("nested"), input.clone());
    assert_eq!(
        input.send(jstream_audio::BusId::MASTER),
        None,
        "no sends of its own"
    );

    batch(
        &control,
        edits(vec![
            Edit::BusCreate {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            },
            Edit::BusSetFader {
                bus_id: BusId::master(),
                gain_db: Some(db(-20.0)),
                muted: Some(true),
            },
            Edit::SourceSetAudio {
                source_id: SourceId::new("nested"),
                enabled: Some(false),
                follow: None,
                gain_db: None,
                muted: None,
            },
        ]),
    )
    .expect("applied");
    assert!((mixer.bus().gain() - 0.1).abs() < 1e-4);
    assert!(input.muted(), "a disabled source is silent");
    drain(&mut events);
    engine.emit_meters_tick();
    let Some(Event::MetersTick { sources, buses }) = drain(&mut events).pop() else {
        panic!("a meters.tick");
    };
    assert!(sources.is_empty(), "a disabled source has no meter");
    let ids: Vec<&str> = buses.iter().map(|b| b.bus_id.as_str()).collect();
    assert_eq!(ids, ["master", "vod"]);

    // Undo puts the master fader back and removes the bus again.
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    assert!((mixer.bus().gain() - 1.0).abs() < 1e-4);
    assert!(!input.muted());
    drain(&mut events);
    engine.emit_meters_tick();
    let Some(Event::MetersTick { buses, .. }) = drain(&mut events).pop() else {
        panic!("a meters.tick");
    };
    assert_eq!(buses.len(), 1);

    let reset = Call::Action {
        action: Action::AudioResetLoudness {},
    };
    let done = block_on(control.call(reset.clone())).expect("reset");
    assert_eq!(done.result, CallResult::action());

    // Once the program stops, no meters go out and a reset is still no failure.
    detach();
    drain(&mut events);
    engine.emit_meters_tick();
    assert!(drain(&mut events).is_empty());
    block_on(control.call(reset)).expect("reset with no mixer");
}

/// A `when_shown` source is heard only while the Program mix shows it: a scene switch,
/// an item shown or hidden, and undo each move its input's activity gain (ADR-0028).
#[test]
fn when_shown_audio_follows_the_program() {
    let clip = jstream_protocol::KindDescriptor {
        kind_id: "clip".into(),
        category: jstream_protocol::KindCategory::Source,
        name: "Clip".into(),
        version: 1,
        video: true,
        audio: true,
        params: vec![],
    };
    let engine = Engine::new().with_extra_kinds(vec![clip]);
    let control = connected(&engine);
    let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (_producer, input) = mixer.add_input(format).expect("input");
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
        scopes: engine.audio_scopes(),
        taps: Arc::new(ScopeTaps::default()),
    });
    // `a` goes on program as the first scene; the clip is placed in it.
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            Edit::SourceCreate {
                source_id: SourceId::new("clip"),
                name: "Clip".into(),
                kind: SourceKind::Builtin {
                    kind: "clip".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("a"),
                item_id: jstream_protocol::ItemId::new("a1"),
                source_id: SourceId::new("clip"),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            },
        ]),
    )
    .expect("created");
    engine.set_audio_input(SourceId::new("clip"), input.clone());
    assert_eq!(input.activity_gain(), 1.0, "shown on program");

    let switch = |scene: &str| {
        block_on(control.call(Call::Action {
            action: Action::SceneSwitch {
                scene_id: SceneId::new(scene),
                preset_id: None,
            },
        }))
        .expect("switched");
    };
    switch("b");
    assert_eq!(input.activity_gain(), 0.0, "off program");
    batch(
        &control,
        edits(vec![Edit::ItemAdd {
            scene_id: SceneId::new("b"),
            item_id: jstream_protocol::ItemId::new("b1"),
            source_id: SourceId::new("clip"),
            transform: None,
            geometry: None,
            opacity: None,
            blend: None,
        }]),
    )
    .expect("placed in b");
    assert_eq!(input.activity_gain(), 1.0, "placed on program");
    batch(
        &control,
        edits(vec![Edit::ItemSetVisible {
            item_id: jstream_protocol::ItemId::new("b1"),
            scene_id: None,
            visible: false,
        }]),
    )
    .expect("hidden");
    assert_eq!(input.activity_gain(), 0.0, "a hidden item shows nothing");
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    assert_eq!(input.activity_gain(), 1.0, "undo shows it again");
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Redo)))).expect("redo");
    assert_eq!(input.activity_gain(), 0.0, "redo hides it");
    // Deleting the program scene moves program to `a`, which shows the clip.
    batch(
        &control,
        edits(vec![Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }]),
    )
    .expect("deleted");
    assert_eq!(input.activity_gain(), 1.0, "program moved to a");
}

/// #362: a source that draws a bus gets that bus's scope, and the mixer thread is
/// pointed at the mixer bus the bus's sends feed; a source on the program mix (the
/// default) adds none, and clearing the reference (as `delete.plan` does before the
/// bus goes) drops the scope.
#[test]
fn a_drawn_bus_gets_a_scope_fed_from_its_mixer_bus() {
    let clip = jstream_protocol::KindDescriptor {
        kind_id: "clip".into(),
        category: jstream_protocol::KindCategory::Source,
        name: "Clip".into(),
        version: 1,
        video: false,
        audio: true,
        params: vec![],
    };
    let engine = Engine::new().with_extra_kinds(vec![clip]);
    let control = connected(&engine);
    let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (_producer, input) = mixer.add_input(format).expect("input");
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let taps = Arc::new(ScopeTaps::default());
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
        scopes: engine.audio_scopes(),
        taps: Arc::clone(&taps),
    });
    let scopes = engine.audio_scopes();
    let wave = |id: &str, audio: serde_json::Value| Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "waveform".into(),
        },
        settings: [
            ("audio", audio),
            ("width", serde_json::json!(4)),
            ("height", serde_json::json!(4)),
        ]
        .into_iter()
        .collect(),
        duplicated_from: None,
    };
    let vod = BusId::new("vod");
    let bus_ref = serde_json::json!({"type": "bus", "bus_id": "vod"});

    // The default draws the program mix: no bus scope, nothing more to feed.
    batch(
        &control,
        edits(vec![
            Edit::SourceCreate {
                source_id: SourceId::new("clip"),
                name: "Clip".into(),
                kind: SourceKind::Builtin {
                    kind: "clip".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            wave("program", serde_json::Value::Null),
        ]),
    )
    .expect("created");
    engine.set_audio_input(SourceId::new("clip"), input.clone());
    assert!(taps.current().is_empty());
    assert!(scopes.buses().is_empty());

    // Drawing `vod`: its scope is fed from the mixer bus the clip's send to `vod` feeds.
    batch(
        &control,
        edits(vec![
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
            Edit::SendSet {
                source_id: SourceId::new("clip"),
                bus_id: vod.clone(),
                gain_db: db(-6.0),
            },
            wave("wave", bus_ref.clone()),
        ]),
    )
    .expect("drawing vod");
    // A bus that doesn't exist is not found, and adds nothing to feed.
    let error = batch(
        &control,
        edits(vec![wave(
            "ghost",
            serde_json::json!({"type": "bus", "bus_id": "ghost"}),
        )]),
    )
    .expect_err("no such bus");
    assert_eq!(
        error.protocol(),
        Some(&jstream_protocol::ProtocolError::NotFound {
            entity: jstream_protocol::EntityKind::Bus,
            id: "ghost".into(),
        })
    );
    let fed = taps.current();
    assert_eq!(fed.len(), 1);
    assert!(Arc::ptr_eq(&fed[0].scope, &scopes.bus(&vod)));
    let TapFeed::Bus(fed_bus) = fed[0].feed else {
        panic!("a bus feeds it: {:?}", fed[0].feed);
    };
    assert_ne!(fed_bus, jstream_audio::BusId::MASTER);
    let gain = input
        .send(fed_bus)
        .expect("the clip sends into the fed bus");
    assert!((gain - 0.501).abs() < 0.01, "-6 dB, got {gain}");

    // The master can be drawn too, before its chain.
    batch(
        &control,
        edits(vec![wave(
            "master",
            serde_json::json!({"type": "bus", "bus_id": "master"}),
        )]),
    )
    .expect("drawing the master");
    let fed: Vec<_> = taps.current().iter().map(|tap| tap.feed).collect();
    assert_eq!(fed.len(), 2);
    assert!(fed.contains(&TapFeed::Bus(jstream_audio::BusId::MASTER)));

    // The bus's delete plan clears the references, so the scope goes with the bus.
    let plan = match query(
        &control,
        jstream_protocol::Query::DeletePlan {
            target: jstream_protocol::DeleteTarget::Bus {
                bus_id: vod.clone(),
            },
        },
    ) {
        Ok(jstream_protocol::QueryResult::DeletePlan { edits: plan, .. }) => plan,
        other => panic!("expected a delete plan, got {other:?}"),
    };
    batch(&control, edits(plan)).expect("the plan applies");
    let fed: Vec<_> = taps.current().iter().map(|tap| tap.feed).collect();
    assert_eq!(fed, [TapFeed::Bus(jstream_audio::BusId::MASTER)]);
    let drawn: Vec<_> = scopes.buses().into_iter().map(|(bus, _)| bus).collect();
    assert_eq!(drawn, [BusId::master()]);
}

/// #459: a source that draws another source gets that source's scope, fed from its
/// mixer input while it has one and is heard (silence while it is muted or has no
/// input); a source with no audio, a scene source or a missing one is refused; and the
/// drawn source's delete is refused until `delete.plan` clears the reference, which
/// drops the scope.
#[test]
fn a_drawn_source_gets_a_scope_fed_from_its_mixer_input() {
    let clip = jstream_protocol::KindDescriptor {
        kind_id: "clip".into(),
        category: jstream_protocol::KindCategory::Source,
        name: "Clip".into(),
        version: 1,
        video: false,
        audio: true,
        params: vec![],
    };
    let engine = Engine::new().with_extra_kinds(vec![clip]);
    let control = connected(&engine);
    let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (_producer, input) = mixer.add_input(format).expect("input");
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let taps = Arc::new(ScopeTaps::default());
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
        scopes: engine.audio_scopes(),
        taps: Arc::clone(&taps),
    });
    let scopes = engine.audio_scopes();
    let wave = |id: &str, drawn: &str| Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "waveform".into(),
        },
        settings: [
            (
                "audio_source",
                serde_json::json!({"type": "source", "source_id": drawn}),
            ),
            ("width", serde_json::json!(4)),
            ("height", serde_json::json!(4)),
        ]
        .into_iter()
        .collect(),
        duplicated_from: None,
    };
    let clip_id = SourceId::new("clip");
    let mute = |muted: bool| Edit::SourceSetAudio {
        source_id: clip_id.clone(),
        enabled: None,
        follow: None,
        gain_db: None,
        muted: Some(muted),
    };
    let feeds = || {
        taps.current()
            .iter()
            .map(|tap| tap.feed)
            .collect::<Vec<_>>()
    };

    // Drawn before its input opens: its scope is fed silence.
    batch(
        &control,
        edits(vec![
            Edit::SourceCreate {
                source_id: clip_id.clone(),
                name: "Clip".into(),
                kind: SourceKind::Builtin {
                    kind: "clip".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            wave("wave", "clip"),
        ]),
    )
    .expect("created");
    assert_eq!(feeds(), [TapFeed::Silence]);
    assert!(Arc::ptr_eq(
        &taps.current()[0].scope,
        &scopes.source(&clip_id)
    ));
    assert!(scopes.buses().is_empty());

    // Its input opens: fed from it. Muted: silence again; unmuted: the input.
    engine.set_audio_input(clip_id.clone(), input.clone());
    assert_eq!(feeds(), [TapFeed::Input(input.id())]);
    batch(&control, edits(vec![mute(true)])).expect("muted");
    assert_eq!(feeds(), [TapFeed::Silence]);
    batch(&control, edits(vec![mute(false)])).expect("unmuted");
    assert_eq!(feeds(), [TapFeed::Input(input.id())]);

    // A source with no audio (a waveform, a scene source) or a missing one is refused.
    let invalid = |drawn: &str| {
        let error = batch(&control, edits(vec![wave("other", drawn)])).expect_err("refused");
        match error.protocol() {
            Some(jstream_protocol::ProtocolError::InvalidArgument { field, reason, .. }) => {
                assert_eq!(field, "edits.0.settings.audio_source");
                assert!(reason.contains("doesn't produce audio"), "{reason}");
            }
            other => panic!("expected invalid_argument, got {other:?}"),
        }
    };
    invalid("wave");
    batch(
        &control,
        edits(vec![
            create("inner"),
            Edit::SourceCreate {
                source_id: SourceId::new("nested"),
                name: "Nested".into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new("inner"),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
        ]),
    )
    .expect("a scene source");
    invalid("nested");
    let error = batch(&control, edits(vec![wave("other", "ghost")])).expect_err("missing");
    assert_eq!(
        error.protocol(),
        Some(&jstream_protocol::ProtocolError::NotFound {
            entity: jstream_protocol::EntityKind::Source,
            id: "ghost".into(),
        })
    );
    assert_eq!(feeds().len(), 1, "nothing more to feed");

    // The drawn source's delete is refused while it is drawn; its plan clears the
    // reference and deletes it, and its scope goes.
    let delete = Edit::SourceDelete {
        source_id: clip_id.clone(),
    };
    let error = batch(&control, edits(vec![delete])).expect_err("drawn");
    assert!(
        matches!(
            error.protocol(),
            Some(jstream_protocol::ProtocolError::InvalidArgument { blockers, .. })
                if blockers.iter().any(|b| b.id == "wave" && b.field == "settings.audio_source")
        ),
        "{error:?}"
    );
    let plan = match query(
        &control,
        jstream_protocol::Query::DeletePlan {
            target: jstream_protocol::DeleteTarget::Source {
                source_id: clip_id.clone(),
            },
        },
    ) {
        Ok(jstream_protocol::QueryResult::DeletePlan { edits: plan, .. }) => plan,
        other => panic!("expected a delete plan, got {other:?}"),
    };
    batch(&control, edits(plan)).expect("the plan applies");
    assert!(feeds().is_empty());
    assert!(scopes.sources().is_empty());
}
