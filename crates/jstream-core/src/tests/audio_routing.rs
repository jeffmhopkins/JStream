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
    assert_eq!(done.result, CallResult::Action {});

    // Once the program stops, no meters go out and a reset is still no failure.
    detach();
    drain(&mut events);
    engine.emit_meters_tick();
    assert!(drain(&mut events).is_empty());
    block_on(control.call(reset)).expect("reset with no mixer");
}

/// Feeds `secs` of constant `[v, v]` stereo frames in 10 ms packets stamped exactly from
/// clock time 0.
fn feed(producer: &mut jstream_audio::InputProducer, v: f32, secs: u64) {
    use jstream_media::{MediaTime, Timebase};
    use jstream_platform::{AudioPacket, AudioPacketFlags};
    let frames = 480u64;
    let samples: Vec<f32> = vec![v; 2 * frames as usize];
    for i in 0..secs * 100 {
        producer.push_packet(&AudioPacket {
            samples: &samples,
            device_position: i * frames,
            time: Some(MediaTime::new(
                (i * frames) as i64,
                Timebase::per_second(48_000).expect("timebase"),
            )),
            flags: AudioPacketFlags::default(),
        });
    }
}

/// Short blocks and latency, so the fed second lands in the frames mixed.
fn tap_config() -> MixerConfig {
    MixerConfig {
        block_frames: 480,
        latency_frames: 480,
        ring_frames: 8192,
        ..MixerConfig::default()
    }
}

fn assert_level(block: &[f32], level: f32, what: &str) {
    assert!(!block.is_empty(), "{what}: an empty block");
    for s in block {
        assert!((s - level).abs() < 1e-4, "{what}: {s} is not {level}");
    }
}

fn gain(owner: jstream_protocol::FilterOwner, id: &str, gain_db: f64) -> Edit {
    Edit::FilterAdd {
        owner,
        chain: jstream_protocol::FilterChain::Audio,
        filter_id: jstream_protocol::FilterId::new(id),
        kind: "gain".into(),
        settings: [("gain_db", serde_json::json!(gain_db))]
            .into_iter()
            .collect(),
        enabled: None,
        index: None,
    }
}

fn set_gain(owner: jstream_protocol::FilterOwner, id: &str, gain_db: f64) -> Edit {
    Edit::FilterSetSettings {
        owner,
        filter_id: jstream_protocol::FilterId::new(id),
        patch: [("gain_db", serde_json::json!(gain_db))]
            .into_iter()
            .collect(),
    }
}

/// The master stage's audio chain runs on the mixer's master chain (ADR-0027), and
/// follows live edits and undo.
#[test]
fn the_master_chain_follows_edits_and_undo() {
    use jstream_protocol::FilterOwner;
    let engine = Engine::new();
    let control = connected(&engine);
    let mut mixer = Mixer::new(tap_config()).expect("mixer");
    let block = mixer.config().block_frames;
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    // Not bound to a source, so routing leaves its send alone.
    let (mut producer, input) = mixer.add_input(format).expect("input");
    input.set_send(jstream_audio::BusId::MASTER, 1.0);
    feed(&mut producer, 0.5, 1);
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
    });
    let mut out = vec![0.0; 2 * block];
    let mut frame = 40_000i64;
    let mut mix = |mixer: &mut Mixer| {
        mixer.mix(frame, &mut out).expect("mix");
        frame += block as i64;
        out.clone()
    };
    mix(&mut mixer);
    assert_level(&mix(&mut mixer), 0.5, "no master chain");

    batch(&control, edits(vec![gain(FilterOwner::Master, "g", -6.0)])).expect("added");
    let g6 = 10f32.powf(-6.0 / 20.0);
    assert_level(&mix(&mut mixer), 0.5 * g6, "the master gain");

    batch(
        &control,
        edits(vec![set_gain(FilterOwner::Master, "g", -12.0)]),
    )
    .expect("set");
    // A gain change ramps over one block.
    mix(&mut mixer);
    let g12 = 10f32.powf(-12.0 / 20.0);
    assert_level(&mix(&mut mixer), 0.5 * g12, "a live edit");

    for _ in 0..2 {
        block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    }
    assert_level(&mix(&mut mixer), 0.5, "undone");
}

/// An output with an audio chain gets an output tap on the master bus that runs the
/// output's chain and follows live edits; releasing it removes the tap, and a stale
/// release is harmless.
#[test]
fn an_output_tap_runs_the_output_chain() {
    use jstream_protocol::{FilterOwner, OutputId, OutputKind, RecordContainer};
    let engine = Engine::new();
    let control = connected(&engine);
    let mut mixer = Mixer::new(tap_config()).expect("mixer");
    let block = mixer.config().block_frames;
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let (mut producer, input) = mixer.add_input(format).expect("input");
    input.set_send(jstream_audio::BusId::MASTER, 1.0);
    feed(&mut producer, 0.5, 1);
    let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
    let _detach = engine.attach_mixer(MixerHandles {
        control: mixer.control(),
        master: mixer.bus(),
        meters,
    });
    let rec = OutputId::new("rec");
    let rec_owner = || FilterOwner::Output {
        output_id: rec.clone(),
    };
    batch(
        &control,
        edits(vec![
            Edit::OutputCreate {
                output_id: rec.clone(),
                kind: OutputKind::Record {
                    path: "/tmp/rec.mp4".into(),
                    container: RecordContainer::Mp4,
                },
            },
            gain(rec_owner(), "g", -6.0),
            gain(FilterOwner::Master, "m", -6.0),
        ]),
    )
    .expect("created");
    let (tap, release) = engine.add_output_tap(&rec).expect("a tap");
    let mut out = vec![0.0; 2 * block];
    let mut frame = 40_000i64;
    let mut mix = |mixer: &mut Mixer| {
        mixer.mix(frame, &mut out).expect("mix");
        frame += block as i64;
        out.clone()
    };
    mix(&mut mixer);
    let master = mix(&mut mixer);
    let g6 = 10f32.powf(-6.0 / 20.0);
    assert_level(
        &master,
        0.5 * g6,
        "the master output has only the master chain",
    );
    let (_, tapped) = mixer.output_block(tap).expect("a tapped block");
    assert_level(tapped, 0.5 * g6 * g6, "the output tap adds its own chain");

    batch(&control, edits(vec![set_gain(rec_owner(), "g", -12.0)])).expect("set");
    // A gain change ramps over one block.
    mix(&mut mixer);
    mix(&mut mixer);
    let g12 = 10f32.powf(-12.0 / 20.0);
    let (_, tapped) = mixer.output_block(tap).expect("a tapped block");
    assert_level(tapped, 0.5 * g6 * g12, "a live edit on the output's chain");

    // A second tap for the same output replaces the first, and the first's late
    // release leaves the second alone.
    let (again, release_again) = engine.add_output_tap(&rec).expect("a second tap");
    release();
    mix(&mut mixer);
    mix(&mut mixer);
    assert!(mixer.output_block(tap).is_none(), "replaced");
    let (_, tapped) = mixer.output_block(again).expect("the second tap stays");
    assert_level(tapped, 0.5 * g6 * g12, "the second tap runs the chain too");

    release_again();
    mix(&mut mixer);
    assert!(mixer.output_block(again).is_none(), "released");
}
