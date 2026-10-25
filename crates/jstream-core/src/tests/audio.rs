//! Audio sources (C5): while an output runs, each audio-only source in the collection is
//! one mixer input, added when the source is created and removed when it is deleted. A
//! source that can't be recorded is unavailable and doesn't fail the output; a failed
//! stream is reopened, and an application is found again when it restarts. A media
//! file's sound (C9b) is one more input, fed while the program shows the file, and a
//! `browser` source's page audio is an input too, from when its page opens until it
//! closes.

use super::{add, batch, block_on, connected, create, drain, edits, output_call, output_state};
use crate::audio_sources::AudioStatus;
use crate::runtime::cpu_draw;
use crate::{Engine, RecordingPublish};
use jstream_control::Control;
use jstream_media::{AudioFormat, ChannelLayout, FrameSize, SampleFormat, Timebase};
use jstream_platform::fake::{
    FakeAudio, FakeAudioIo, FakeCapture, FakeClip, FakeClock, FakeCodecs, FakeGpuInterop,
    FakeMediaDecoders, FakeVideoDevices,
};
use jstream_platform::{AudioInputTarget, AudioProcess, Backends};
use jstream_protocol::{
    AssetRoot, Batch, Edit, Event, ItemId, OutputId, OutputState, Reference, Settings, SourceId,
    SourceKind,
};
use serde_json::json;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn engine(audio: &FakeAudioIo, clock: &FakeClock) -> Engine {
    engine_with(audio, clock, FakeMediaDecoders::new())
}

fn engine_with(audio: &FakeAudioIo, clock: &FakeClock, media: FakeMediaDecoders) -> Engine {
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(FakeCapture::new(
            clock.clone(),
            FrameSize::new(64, 36).expect("size"),
        )),
        audio: Arc::new(audio.clone()),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        vcam: None,
        media: Arc::new(media),
    };
    Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_draw(cpu_draw())
        .with_publish(Box::new(RecordingPublish::new()))
}

fn audio_source(id: &str, kind: &str, setting: Option<(&str, &str)>) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        settings: Settings(
            setting
                .map(|(k, v)| (k.to_owned(), json!(v)))
                .into_iter()
                .collect(),
        ),
        duplicated_from: None,
    }
}

fn delete(id: &str) -> Edit {
    Edit::SourceDelete {
        source_id: SourceId::new(id),
    }
}

fn game(pid: u32) -> AudioProcess {
    AudioProcess {
        pid,
        parent_pid: Some(4),
        exe: "game.exe".into(),
    }
}

/// Waits up to five seconds for `done`.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// The fake's open inputs, in a stable order.
fn inputs(audio: &FakeAudioIo) -> Vec<String> {
    let mut targets: Vec<String> = audio
        .input_targets()
        .iter()
        .map(|t| format!("{t:?}"))
        .collect();
    targets.sort();
    targets
}

fn names(targets: &[AudioInputTarget]) -> Vec<String> {
    let mut names: Vec<String> = targets.iter().map(|t| format!("{t:?}")).collect();
    names.sort();
    names
}

fn status(engine: &Engine, id: &str) -> Option<AudioStatus> {
    engine.audio_status(&SourceId::new(id))
}

#[test]
fn audio_sources_follow_the_collection_into_the_mixer() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);

    // A collection without audio sources records the default microphone and desktop.
    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert_eq!(output_state(&control, "yt"), OutputState::Active);
    assert_eq!(
        inputs(&audio),
        names(&[
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback
        ])
    );

    // The first audio source replaces them.
    batch(
        &control,
        Batch::single(audio_source("mic", "audio_input", None)),
    )
    .expect("mic");
    assert!(
        eventually(|| inputs(&audio) == names(&[AudioInputTarget::DefaultInput])),
        "{:?}",
        inputs(&audio)
    );
    assert!(eventually(
        || status(&engine, "mic") == Some(AudioStatus::Recording)
    ));

    // A device that isn't there, and an application that isn't running, are unavailable
    // and leave the output running.
    batch(
        &control,
        edits(vec![
            audio_source("usb", "audio_input", Some(("device", "usb-mic"))),
            audio_source("game", "app_audio", Some(("exe", "Game.exe"))),
            audio_source("desk", "desktop_audio", None),
        ]),
    )
    .expect("more sources");
    assert!(eventually(|| matches!(
        status(&engine, "game"),
        Some(AudioStatus::Unavailable(reason)) if reason == "Game.exe is not running"
    )));
    assert!(matches!(
        status(&engine, "usb"),
        Some(AudioStatus::Unavailable(_))
    ));
    assert_eq!(status(&engine, "desk"), Some(AudioStatus::Recording));
    assert_eq!(output_state(&control, "yt"), OutputState::Active);

    // The application starts: it is found on the next tick.
    audio.set_processes(vec![game(40)]);
    assert!(eventually(
        || status(&engine, "game") == Some(AudioStatus::Recording)
    ));
    assert!(
        audio
            .input_targets()
            .contains(&AudioInputTarget::ProcessLoopback { pid: 40 })
    );

    // It restarts under a new process ID: the input follows it.
    audio.set_processes(vec![game(41)]);
    assert!(
        eventually(|| {
            let targets = audio.input_targets();
            targets.contains(&AudioInputTarget::ProcessLoopback { pid: 41 })
                && !targets.contains(&AudioInputTarget::ProcessLoopback { pid: 40 })
        }),
        "{:?}",
        inputs(&audio)
    );

    // Every device is unplugged: the streams fail and are reopened.
    audio.unplug();
    assert!(
        eventually(|| inputs(&audio)
            == names(&[
                AudioInputTarget::DefaultInput,
                AudioInputTarget::DefaultOutputLoopback,
                AudioInputTarget::ProcessLoopback { pid: 41 },
            ])),
        "{:?}",
        inputs(&audio)
    );

    // Deleting a source removes its input; deleting the last brings the defaults back.
    batch(&control, Batch::single(delete("game"))).expect("delete game");
    assert!(eventually(|| inputs(&audio)
        == names(&[
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback,
        ])));
    assert_eq!(status(&engine, "game"), None);
    batch(
        &control,
        edits(vec![delete("mic"), delete("usb"), delete("desk")]),
    )
    .expect("delete the rest");
    assert!(eventually(|| inputs(&audio)
        == names(&[
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback
        ])));

    // Stopping the output closes them all.
    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
}

#[test]
fn a_settings_change_reopens_the_input_and_stopping_clears_the_status() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    batch(
        &control,
        Batch::single(audio_source("mic", "audio_input", Some(("device", "nope")))),
    )
    .expect("mic");
    // Not recorded while no output runs.
    assert_eq!(status(&engine, "mic"), None);

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(matches!(
        status(&engine, "mic"),
        Some(AudioStatus::Unavailable(_))
    ));
    assert_eq!(audio.input_targets(), vec![]);

    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("mic"),
            patch: Settings(
                [("device".to_owned(), json!(jstream_platform::fake::FAKE_MIC))]
                    .into_iter()
                    .collect(),
            ),
        }),
    )
    .expect("pick the fake microphone");
    assert!(eventually(
        || status(&engine, "mic") == Some(AudioStatus::Recording)
    ));
    assert_eq!(
        audio.input_targets(),
        vec![AudioInputTarget::Input(jstream_platform::DeviceId::new(
            jstream_platform::fake::FAKE_MIC
        ))]
    );

    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
    assert_eq!(status(&engine, "mic"), None);
}

/// Shows or hides the item that places the media file.
fn show_clip(control: &crate::InProcessControl, visible: bool) {
    batch(
        control,
        Batch::single(Edit::ItemSetVisible {
            item_id: ItemId::new("i1"),
            scene_id: None,
            visible,
        }),
    )
    .expect("shown or hidden");
}

/// Moves the clock on by `ms` in 20 ms steps, re-planning at each as the program mix
/// does, so the file's frames are taken and its decoder reads on.
fn run_for(engine: &Engine, clock: &FakeClock, ms: u64) {
    for _ in 0..ms / 20 {
        clock.advance(20_000);
        engine.replan();
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_media_files_sound_is_mixed_while_the_program_shows_it() {
    let dir = std::env::temp_dir().join(format!("jstream-core-media-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a folder");
    let path = dir.join("clip.mp4");
    std::fs::write(&path, b"fake").expect("the file");
    let path = path.canonicalize().expect("the file");
    let media = FakeMediaDecoders::new();
    media.add(
        &path,
        FakeClip {
            size: FrameSize::new(4, 2).expect("a size"),
            frame_period: Timebase::new(1, 25).expect("a time base"),
            frames: 25,
            key_every: 5,
            audio: Some(FakeAudio {
                format: AudioFormat::new(
                    44_100,
                    ChannelLayout::Stereo,
                    SampleFormat::F32Interleaved,
                )
                .expect("a format"),
                block_frames: 1024,
                blocks: 43,
            }),
        },
    );
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine_with(&audio, &clock, media.clone());
    let control = connected(&engine);
    let mut events = control.events();
    // The engine canonicalizes the path itself, so the fake is keyed by the canonical
    // path; the setting drops Windows' verbatim `\\?\` prefix, which an `absolute`
    // asset path refuses as two leading separators.
    let setting = path.to_string_lossy();
    let file = Reference::Asset {
        root: AssetRoot::Absolute,
        path: setting.strip_prefix(r"\\?\").unwrap_or(&setting).to_owned(),
    };
    let video: Settings = Settings(
        [
            ("file", file.to_value()),
            ("loop", json!(true)),
            ("speed", json!(1.0)),
            ("restart_on_show", json!(false)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    );
    batch(
        &control,
        edits(vec![
            create("a"),
            Edit::SourceCreate {
                source_id: SourceId::new("clip"),
                name: "clip".into(),
                kind: SourceKind::Builtin {
                    kind: "media".into(),
                },
                settings: video,
                duplicated_from: None,
            },
            add("a", "i1", "clip"),
        ]),
    )
    .expect("a scene showing the file");
    assert!(engine.settle_sources(), "the file opened");
    // Not mixed while no output runs.
    assert_eq!(status(&engine, "clip"), None);

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(eventually(
        || status(&engine, "clip") == Some(AudioStatus::Recording)
    ));
    // A video doesn't replace the default microphone and desktop audio.
    assert!(
        inputs(&audio)
            == names(&[
                AudioInputTarget::DefaultInput,
                AudioInputTarget::DefaultOutputLoopback
            ]),
        "{:?}",
        inputs(&audio)
    );
    let packets = |engine: &Engine| engine.audio_packets(&SourceId::new("clip")).unwrap_or(0);
    assert!(
        eventually(|| {
            run_for(&engine, &clock, 100);
            packets(&engine) >= 20
        }),
        "the file's sound reaches its mixer input: {}",
        packets(&engine)
    );

    // The count once two reads a reconcile pass apart agree.
    let settled = |engine: &Engine| {
        let mut last = packets(engine);
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(700));
            let now = packets(engine);
            if now == last {
                return now;
            }
            last = now;
        }
        panic!("the count never settled: {last}");
    };

    // Hidden, it is silent: once what was handed on ahead is counted, no more comes.
    show_clip(&control, false);
    run_for(&engine, &clock, 200);
    let hidden = settled(&engine);
    run_for(&engine, &clock, 400);
    assert_eq!(settled(&engine), hidden, "nothing while hidden");

    // Shown again, it is heard again.
    show_clip(&control, true);
    assert!(
        eventually(|| {
            run_for(&engine, &clock, 100);
            packets(&engine) > hidden + 10
        }),
        "heard again: {} after {hidden}",
        packets(&engine)
    );

    // The routing (D5) finds its input as an audio source's: it is metered.
    let metered = |events: &mut jstream_control::EventStream| {
        engine.emit_meters_tick();
        drain(events)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::MetersTick { sources, .. } => Some(
                    sources
                        .into_iter()
                        .map(|s| s.source_id.as_str().to_owned())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .expect("a meters.tick while the program runs")
    };
    assert_eq!(metered(&mut events), ["clip"]);
    // Its audio off, its input closes; on again, it opens again.
    let set_audio = |enabled: bool| {
        batch(
            &control,
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("clip"),
                enabled: Some(enabled),
                follow: None,
                gain_db: None,
                muted: None,
            }),
        )
        .expect("source.set_audio");
    };
    set_audio(false);
    assert!(eventually(|| status(&engine, "clip").is_none()));
    assert!(metered(&mut events).is_empty());
    set_audio(true);
    assert!(eventually(
        || status(&engine, "clip") == Some(AudioStatus::Recording)
    ));
    assert_eq!(metered(&mut events), ["clip"]);

    // Its input runs the source's audio filters (ADR-0026): a gain of -60 dB brings its
    // metered peak down a thousandfold, and removing the filter brings the level back.
    // The mixer runs as the fake device takes blocks, here silence from the microphone
    // and desktop, so the peak is the file's.
    let peak = |events: &mut jstream_control::EventStream| {
        for _ in 0..10 {
            audio.deliver(480, 0.0);
            clock.advance(10_000);
            engine.replan();
            thread::sleep(Duration::from_millis(2));
        }
        engine.emit_meters_tick();
        drain(events)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::MetersTick { sources, .. } => Some(
                    sources
                        .into_iter()
                        .find(|s| s.source_id.as_str() == "clip")
                        .map_or(0.0, |s| s.peak[0].get().max(s.peak[1].get())),
                ),
                _ => None,
            })
            .unwrap_or(0.0)
    };
    // Each fake block holds one level, from 1/128 to 1/2.
    let heard = |peak: f32| peak >= 1.0 / 128.0;
    assert!(
        eventually(|| heard(peak(&mut events))),
        "the file is metered"
    );
    let owner = || jstream_protocol::FilterOwner::Source {
        source_id: SourceId::new("clip"),
    };
    batch(
        &control,
        Batch::single(Edit::FilterAdd {
            owner: owner(),
            chain: jstream_protocol::FilterChain::Audio,
            filter_id: jstream_protocol::FilterId::new("quiet"),
            kind: "gain".into(),
            settings: Settings([("gain_db".to_owned(), json!(-60.0))].into_iter().collect()),
            enabled: None,
            index: None,
        }),
    )
    .expect("filter.add");
    // Two ticks, so the first drops a peak held from before the filter. The loudest
    // block, 1/2, comes out at 0.0005.
    assert!(
        eventually(|| {
            peak(&mut events);
            let quiet = peak(&mut events);
            quiet > 0.0 && quiet < 0.001
        }),
        "the gain filter runs on the file's input"
    );
    batch(
        &control,
        Batch::single(Edit::FilterRemove {
            owner: owner(),
            filter_id: jstream_protocol::FilterId::new("quiet"),
        }),
    )
    .expect("filter.remove");
    assert!(
        eventually(|| {
            peak(&mut events);
            heard(peak(&mut events))
        }),
        "the level comes back once the filter is removed"
    );

    // Deleting the source removes its input.
    batch(
        &control,
        edits(vec![
            Edit::ItemRemove {
                item_id: ItemId::new("i1"),
            },
            delete("clip"),
        ]),
    )
    .expect("deleted");
    assert!(eventually(|| status(&engine, "clip").is_none()));
    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_program_mix_feeds_the_audio_scope_while_an_output_runs() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    let scope = engine.audio_scope();
    let mut window = [0.0f32; 256];
    assert_eq!(
        scope.latest(&mut window),
        (0, 0),
        "no mix before an output runs"
    );

    // The default microphone and desktop inputs each deliver a steady 0.25: the scope
    // sees their mix (the mixer's latency and resampler settle first).
    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            let (_, rate) = scope.latest(&mut window);
            rate == 48_000 && window.iter().any(|&s| s > 0.1)
        }),
        "the scope saw no audio: {window:?}"
    );
    block_on(control.call(output_call("yt", false))).expect("output.stop");
}

/// ADR-0027 §4: the privacy screen mutes the microphones from the mixer's next block,
/// and only them: the default microphone while the collection has no audio source, then
/// an `audio_input` source, while desktop audio plays on. Turning it off brings them
/// back, with their own mute untouched.
#[test]
fn the_privacy_screen_mutes_the_microphones() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    let scope = engine.audio_scope();
    let mut window = [0.0f32; 256];
    let near = |window: &[f32], level: f32| window.iter().all(|&s| (s - level).abs() < 0.01);
    // Every input delivers a steady 0.25 at unity: the mix is 0.25 per input heard.
    let mixes = |level: f32, window: &mut [f32]| {
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            let (_, rate) = scope.latest(window);
            rate == 48_000 && near(window, level)
        })
    };
    let privacy = |on| block_on(control.call(super::privacy_call(on))).expect("privacy.set");

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(mixes(0.5, &mut window), "the default inputs: {window:?}");
    privacy(true);
    assert!(
        mixes(0.25, &mut window),
        "the default microphone muted: {window:?}"
    );
    privacy(false);
    assert!(mixes(0.5, &mut window), "and back: {window:?}");

    batch(
        &control,
        edits(vec![
            audio_source("mic", "audio_input", None),
            audio_source("desk", "desktop_audio", None),
        ]),
    )
    .expect("created");
    assert!(mixes(0.5, &mut window), "the sources: {window:?}");
    privacy(true);
    assert!(
        mixes(0.25, &mut window),
        "the microphone source muted: {window:?}"
    );
    privacy(false);
    assert!(mixes(0.5, &mut window), "and back: {window:?}");
    block_on(control.call(output_call("yt", false))).expect("output.stop");
}

/// #362: a `waveform` drawing a bus gets that bus's audio from the mixer thread, not
/// the program mix's: here the desktop sends only into `vod`, so the bus's scope moves
/// while the program mix stays silent.
#[test]
fn a_bus_a_source_draws_feeds_its_scope_while_an_output_runs() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    let vod = jstream_protocol::BusId::new("vod");
    batch(
        &control,
        edits(vec![
            audio_source("desk", "desktop_audio", None),
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
            Edit::SendSet {
                source_id: SourceId::new("desk"),
                bus_id: vod.clone(),
                gain_db: jstream_protocol::Finite32::ZERO,
            },
            Edit::SendRemove {
                source_id: SourceId::new("desk"),
                bus_id: jstream_protocol::BusId::master(),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("wave"),
                name: "Waveform".into(),
                kind: SourceKind::Builtin {
                    kind: "waveform".into(),
                },
                settings: Settings(
                    [("audio".to_owned(), json!({"type": "bus", "bus_id": "vod"}))]
                        .into_iter()
                        .collect(),
                ),
                duplicated_from: None,
            },
        ]),
    )
    .expect("created");
    let scopes = engine.audio_scopes();
    let bus = scopes.bus(&vod);
    let program = engine.audio_scope();
    let mut window = [0.0f32; 256];

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            let (_, rate) = bus.latest(&mut window);
            rate == 48_000 && window.iter().any(|&s| s > 0.1)
        }),
        "the bus's scope saw no audio: {window:?}"
    );
    let (written, _) = program.latest(&mut window);
    assert!(written > 0, "the program mix is fed too");
    assert!(
        window.iter().all(|&s| s.abs() < 1e-3),
        "the program mix has no send from the desktop: {window:?}"
    );
    block_on(control.call(output_call("yt", false))).expect("output.stop");
}

/// A bus's monitor (D9) plays the bus on its device while an output runs: `vod`'s on
/// the default output hears the desktop, which sends only to `vod`, and the master's on
/// the fake speakers hears nothing from it. A device that isn't there leaves the
/// monitor unavailable, taking it away closes its stream, and stopping closes the rest.
#[test]
fn a_bus_monitor_plays_its_bus_on_its_device_while_an_output_runs() {
    use jstream_protocol::{BusId, BusMonitor, Finite32, MonitorTap};
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    let vod = BusId::new("vod");
    let monitor = |bus_id: &BusId, device: &str, tap| Edit::BusSetMonitor {
        bus_id: bus_id.clone(),
        monitor: Some(BusMonitor {
            device: device.into(),
            tap,
        }),
    };
    batch(
        &control,
        edits(vec![
            audio_source("desk", "desktop_audio", None),
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
            Edit::SendSet {
                source_id: SourceId::new("desk"),
                bus_id: vod.clone(),
                gain_db: Finite32::ZERO,
            },
            Edit::SendRemove {
                source_id: SourceId::new("desk"),
                bus_id: BusId::master(),
            },
            monitor(
                &BusId::master(),
                jstream_platform::fake::FAKE_SPEAKERS,
                MonitorTap::PreFilters,
            ),
            monitor(&vod, "", MonitorTap::PostFilters),
        ]),
    )
    .expect("created");
    // Nothing plays while no output runs.
    assert_eq!(engine.monitor_status(&vod), None);
    assert_eq!(audio.open_streams(), 0);

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(eventually(|| {
        engine.monitor_status(&vod) == Some(AudioStatus::Recording)
            && engine.monitor_status(&BusId::master()) == Some(AudioStatus::Recording)
    }));
    // The desktop input and the two monitors: master's opened first, in bus order.
    assert_eq!(audio.open_streams(), 3);
    let mut heard = Vec::new();
    assert!(
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            heard = audio.render(480);
            heard.len() == 2 && heard[1].iter().any(|&s| s > 0.2)
        }),
        "vod's monitor played no audio"
    );
    assert!(
        heard[0].iter().all(|&s| s.abs() < 1e-3),
        "the master's monitor hears no desktop"
    );

    // A device that isn't there: the monitor is unavailable, and its stream closes.
    batch(
        &control,
        Batch::single(monitor(&vod, "nope", MonitorTap::PreFilters)),
    )
    .expect("another device");
    assert!(eventually(|| matches!(
        engine.monitor_status(&vod),
        Some(AudioStatus::Unavailable(_))
    )));
    assert_eq!(audio.open_streams(), 2);

    // Taken away, it has no status; stopping closes the master's too.
    batch(
        &control,
        Batch::single(Edit::BusSetMonitor {
            bus_id: vod.clone(),
            monitor: None,
        }),
    )
    .expect("no monitor");
    assert!(eventually(|| engine.monitor_status(&vod).is_none()));
    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
    assert_eq!(engine.monitor_status(&BusId::master()), None);
}

/// Sources that open through the browser backend, `browser` and its `youtube_chat`
/// preset, are built again when the host gives the engine its backend after they
/// exist (review B1 on #375).
#[test]
fn browser_presets_open_once_the_backend_arrives() {
    use jstream_browser::PageLocation;
    use jstream_browser::fake::FakeBrowser;

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            audio_source("page", "browser", Some(("url", "https://example.com/"))),
            audio_source("chat", "youtube_chat", Some(("video", "dQw4w9WgXcQ"))),
        ]),
    )
    .expect("sources");
    // Long enough for both builds to fail for want of a backend.
    thread::sleep(Duration::from_millis(200));

    let browser = FakeBrowser::new();
    let _engine = engine.with_browser(browser.clone());
    assert!(
        eventually(|| browser.pages().len() == 2),
        "{:?}",
        browser.pages().len()
    );
    let mut opened: Vec<String> = browser
        .pages()
        .iter()
        .map(|page| match &page.spec().location {
            PageLocation::Web(url) => url.to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    opened.sort();
    assert_eq!(
        opened,
        [
            "https://example.com/",
            "https://www.youtube.com/live_chat?is_popout=1&v=dQw4w9WgXcQ"
        ]
    );
}

#[test]
fn a_browser_page_is_recorded_alongside_the_defaults() {
    use jstream_browser::fake::FakeBrowser;

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let browser = FakeBrowser::new();
    let engine = engine(&audio, &clock).with_browser(browser.clone());
    let control = connected(&engine);
    let mut events = control.events();
    block_on(control.call(output_call("yt", true))).expect("output.start");

    batch(
        &control,
        Batch::single(audio_source(
            "page",
            "browser",
            Some(("url", "https://example.com/")),
        )),
    )
    .expect("page");
    assert!(eventually(
        || status(&engine, "page") == Some(AudioStatus::Recording)
    ));
    let pages = browser.pages();
    assert_eq!(pages.len(), 1);
    // The routing finds the page's input, as it does an audio source's.
    engine.emit_meters_tick();
    let metered = drain(&mut events)
        .into_iter()
        .rev()
        .find_map(|event| match event {
            Event::MetersTick { sources, .. } => Some(sources),
            _ => None,
        });
    assert!(
        metered.is_some_and(|sources| sources.iter().any(|s| s.source_id.as_str() == "page")),
        "the page is metered"
    );
    // A page's audio isn't an audio source: the defaults stay.
    assert_eq!(
        inputs(&audio),
        names(&[
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback
        ])
    );

    // Turning the page's audio off closes its input but keeps the page; turning it on
    // records the same page again.
    let page_metered = |events: &mut jstream_control::EventStream| {
        engine.emit_meters_tick();
        drain(events)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::MetersTick { sources, .. } => Some(sources),
                _ => None,
            })
            .is_some_and(|sources| sources.iter().any(|s| s.source_id.as_str() == "page"))
    };
    let set_audio = |enabled| Edit::SourceSetAudio {
        source_id: SourceId::new("page"),
        enabled: Some(enabled),
        follow: None,
        gain_db: None,
        muted: None,
    };
    batch(&control, Batch::single(set_audio(false))).expect("audio off");
    assert!(eventually(|| status(&engine, "page").is_none()));
    assert!(!page_metered(&mut events), "an off page isn't metered");
    assert_eq!(browser.pages().len(), 1);
    assert!(!pages[0].is_closed());
    batch(&control, Batch::single(set_audio(true))).expect("audio on");
    assert!(eventually(
        || status(&engine, "page") == Some(AudioStatus::Recording)
    ));
    assert!(page_metered(&mut events), "the page is metered again");
    assert_eq!(browser.pages().len(), 1);

    // A new address opens a new page, whose audio replaces the old page's.
    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("page"),
            patch: Settings(
                [("url".to_owned(), json!("https://example.org/"))]
                    .into_iter()
                    .collect(),
            ),
        }),
    )
    .expect("new address");
    assert!(eventually(
        || browser.pages().len() == 2 && pages[0].is_closed()
    ));
    assert!(eventually(
        || status(&engine, "page") == Some(AudioStatus::Recording)
    ));

    // Deleting the source closes its page and its input.
    batch(&control, Batch::single(delete("page"))).expect("delete page");
    assert!(eventually(|| browser.pages().iter().all(|p| p.is_closed())));
    assert!(eventually(|| status(&engine, "page").is_none()));

    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
}

/// #459: a `waveform` drawing a source gets that source's audio after its own fader,
/// before any bus, from the mixer thread: here the desktop is at -6 dB and sends to no
/// bus, so its scope reads half the delivered level while the program mix carries only
/// the microphone at full level. Muted, the source draws silence.
#[test]
fn a_source_a_source_draws_feeds_its_scope_after_its_fader() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            audio_source("mic", "audio_input", None),
            audio_source("desk", "desktop_audio", None),
            Edit::SourceSetAudio {
                source_id: SourceId::new("desk"),
                enabled: None,
                follow: None,
                gain_db: Some(jstream_protocol::Finite32::new(-6.0).expect("finite")),
                muted: None,
            },
            Edit::SendRemove {
                source_id: SourceId::new("desk"),
                bus_id: jstream_protocol::BusId::master(),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("wave"),
                name: "Waveform".into(),
                kind: SourceKind::Builtin {
                    kind: "waveform".into(),
                },
                settings: Settings(
                    [(
                        "audio_source".to_owned(),
                        json!({"type": "source", "source_id": "desk"}),
                    )]
                    .into_iter()
                    .collect(),
                ),
                duplicated_from: None,
            },
        ]),
    )
    .expect("created");
    let scopes = engine.audio_scopes();
    let desk = scopes.source(&SourceId::new("desk"));
    let program = engine.audio_scope();
    let mut window = [0.0f32; 256];
    let near = |window: &[f32], level: f32| window.iter().all(|&s| (s - level).abs() < 0.01);

    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            let (_, rate) = desk.latest(&mut window);
            rate == 48_000 && near(&window, 0.125)
        }),
        "the source's scope didn't read it after its -6 dB fader: {window:?}"
    );
    program.latest(&mut window);
    assert!(
        near(&window, 0.25),
        "the program mix carries the microphone only: {window:?}"
    );

    batch(
        &control,
        edits(vec![Edit::SourceSetAudio {
            source_id: SourceId::new("desk"),
            enabled: None,
            follow: None,
            gain_db: None,
            muted: Some(true),
        }]),
    )
    .expect("muted");
    assert!(
        eventually(|| {
            audio.deliver(480, 0.25);
            clock.advance(10_000);
            let (_, rate) = desk.latest(&mut window);
            rate == 48_000 && near(&window, 0.0)
        }),
        "a muted source draws silence: {window:?}"
    );
    block_on(control.call(output_call("yt", false))).expect("output.stop");
}

/// The routing (D5) finds the inputs the program stage opens: an audio source that
/// records is metered in `meters.tick`. Turning its audio off closes its input and drops
/// its meter, and doesn't bring the default microphone and desktop back; turning it on
/// opens it again.
#[test]
fn recording_audio_sources_are_routed_and_metered() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    let mut events = control.events();
    batch(
        &control,
        Batch::single(audio_source("desk", "desktop_audio", None)),
    )
    .expect("desk");
    block_on(control.call(output_call("yt", true))).expect("output.start");
    assert!(eventually(
        || status(&engine, "desk") == Some(AudioStatus::Recording)
    ));
    let metered = |events: &mut jstream_control::EventStream| {
        engine.emit_meters_tick();
        drain(events)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::MetersTick { sources, .. } => Some(
                    sources
                        .into_iter()
                        .map(|s| s.source_id.as_str().to_owned())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .expect("a meters.tick while the program runs")
    };
    assert_eq!(metered(&mut events), ["desk"]);

    batch(
        &control,
        Batch::single(Edit::SourceSetAudio {
            source_id: SourceId::new("desk"),
            enabled: Some(false),
            follow: None,
            gain_db: None,
            muted: None,
        }),
    )
    .expect("audio off");
    assert!(
        eventually(|| inputs(&audio).is_empty()),
        "{:?}",
        inputs(&audio)
    );
    assert_eq!(status(&engine, "desk"), None);
    assert!(metered(&mut events).is_empty());

    batch(
        &control,
        Batch::single(Edit::SourceSetAudio {
            source_id: SourceId::new("desk"),
            enabled: Some(true),
            follow: None,
            gain_db: None,
            muted: None,
        }),
    )
    .expect("audio on");
    assert!(eventually(
        || status(&engine, "desk") == Some(AudioStatus::Recording)
    ));
    assert_eq!(
        inputs(&audio),
        names(&[AudioInputTarget::DefaultOutputLoopback])
    );
    assert_eq!(metered(&mut events), ["desk"]);
    block_on(control.call(output_call("yt", false))).expect("output.stop");
}

/// #716: a `webcam` source records its device's paired audio input, alongside the
/// defaults (a webcam's microphone isn't an audio source for that rule). A device with
/// no paired input, or one that isn't connected, is unavailable; turning the source's
/// audio off, removing its last send or deleting it closes its input.
#[test]
fn a_webcam_records_its_paired_audio_input() {
    use jstream_platform::fake::{FAKE_CAPTURE_CARD, FAKE_MIC, FAKE_WEBCAM};

    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let engine = engine(&audio, &clock);
    let control = connected(&engine);
    block_on(control.call(output_call("yt", true))).expect("output.start");
    let mic = AudioInputTarget::Input(jstream_platform::DeviceId::new(FAKE_MIC));

    // The first device listed (the fake webcam) when `device` is empty, and by its ID.
    batch(
        &control,
        edits(vec![
            audio_source("cam", "webcam", None),
            audio_source("named", "webcam", Some(("device", FAKE_WEBCAM))),
            audio_source("card", "webcam", Some(("device", FAKE_CAPTURE_CARD))),
            audio_source("gone", "webcam", Some(("device", "unplugged"))),
        ]),
    )
    .expect("webcams");
    assert!(eventually(|| status(&engine, "cam")
        == Some(AudioStatus::Recording)
        && status(&engine, "named") == Some(AudioStatus::Recording)));
    assert_eq!(
        status(&engine, "card"),
        Some(AudioStatus::Unavailable(
            "Fake capture card has no paired audio input".into()
        ))
    );
    assert_eq!(
        status(&engine, "gone"),
        Some(AudioStatus::Unavailable(
            "the video device unplugged is not connected".into()
        ))
    );
    assert_eq!(
        inputs(&audio),
        names(&[
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback,
            mic.clone(),
            mic.clone(),
        ])
    );
    assert_eq!(output_state(&control, "yt"), OutputState::Active);

    // Its audio off, the webcam's input closes; on again, it opens again.
    let set_audio = |enabled: bool| {
        batch(
            &control,
            Batch::single(Edit::SourceSetAudio {
                source_id: SourceId::new("named"),
                enabled: Some(enabled),
                follow: None,
                gain_db: None,
                muted: None,
            }),
        )
        .expect("source.set_audio");
    };
    set_audio(false);
    assert!(eventually(|| status(&engine, "named").is_none()
        && audio.input_targets().iter().filter(|t| **t == mic).count()
            == 1));
    set_audio(true);
    assert!(eventually(
        || status(&engine, "named") == Some(AudioStatus::Recording)
    ));

    // The microphone opens only while the source has a send: without its master send it
    // closes, whatever its audio settings say, and a send opens it again.
    let mics = || audio.input_targets().iter().filter(|t| **t == mic).count();
    batch(
        &control,
        Batch::single(Edit::SendRemove {
            source_id: SourceId::new("named"),
            bus_id: jstream_protocol::BusId::master(),
        }),
    )
    .expect("send.remove");
    assert!(eventually(
        || status(&engine, "named").is_none() && mics() == 1
    ));
    batch(
        &control,
        Batch::single(Edit::SendSet {
            source_id: SourceId::new("named"),
            bus_id: jstream_protocol::BusId::master(),
            gain_db: jstream_protocol::Finite32::ZERO,
        }),
    )
    .expect("send.set");
    assert!(eventually(|| status(&engine, "named")
        == Some(AudioStatus::Recording)
        && mics() == 2));

    // Deleting the webcam closes its microphone.
    batch(
        &control,
        Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("named"),
        }),
    )
    .expect("source.delete");
    assert!(eventually(
        || status(&engine, "named").is_none() && mics() == 1
    ));
    assert_eq!(output_state(&control, "yt"), OutputState::Active);

    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(eventually(|| audio.open_streams() == 0));
}
