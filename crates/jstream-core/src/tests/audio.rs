//! The program mixer follows the collection's audio (ADR-0028), and `meters.tick` and
//! `audio.reset_loudness` reach it, through `InProcessControl`. The mixer is attached
//! by hand, as the program stage does when it opens.

use super::{batch, block_on, connected, create, drain, edits};
use crate::Engine;
use crate::audio::{MasterMetering, MixerHandles};
use jstream_audio::{Mixer, MixerConfig};
use jstream_control::Control;
use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
use jstream_protocol::{
    Action, BusId, Call, CallResult, Edit, Event, Finite32, HistoryCall, HistoryOp, SceneId,
    SourceId, SourceKind,
};

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
    assert_eq!(sources.len(), 1);
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
    assert_eq!(done.result, CallResult::Action {});

    // Once the program stops, no meters go out and a reset is still no failure.
    detach();
    drain(&mut events);
    engine.emit_meters_tick();
    assert!(drain(&mut events).is_empty());
    block_on(control.call(reset)).expect("reset with no mixer");
}
