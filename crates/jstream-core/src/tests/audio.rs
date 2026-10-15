//! Audio sources (C5): while an output runs, each audio-only source in the collection is
//! one mixer input, added when the source is created and removed when it is deleted. A
//! source that can't be recorded is unavailable and doesn't fail the output; a failed
//! stream is reopened, and an application is found again when it restarts. A media
//! file's sound (C9b) is one more input, fed while the program shows the file.

use super::{add, batch, block_on, connected, create, edits, output_call, output_state};
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
    AssetRoot, Batch, Edit, ItemId, OutputId, OutputState, Reference, Settings, SourceId,
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
    let file = Reference::Asset {
        root: AssetRoot::Absolute,
        path: path.to_string_lossy().into_owned(),
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

    // Hidden, it is silent: once what was handed on ahead is counted, no more comes.
    show_clip(&control, false);
    run_for(&engine, &clock, 200);
    thread::sleep(Duration::from_millis(600));
    let hidden = packets(&engine);
    run_for(&engine, &clock, 400);
    thread::sleep(Duration::from_millis(600));
    assert_eq!(packets(&engine), hidden, "nothing while hidden");

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
