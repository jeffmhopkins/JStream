//! Audio sources (C5): while an output runs, each audio-only source in the collection is
//! one mixer input, added when the source is created and removed when it is deleted. A
//! source that can't be recorded is unavailable and doesn't fail the output; a failed
//! stream is reopened, and an application is found again when it restarts. A `browser`
//! source's page audio is an input too, from when its page opens until it closes.

use super::{batch, block_on, connected, drain, edits, output_call, output_state};
use crate::audio_sources::AudioStatus;
use crate::runtime::cpu_draw;
use crate::{Engine, RecordingPublish};
use jstream_control::Control;
use jstream_media::{ChannelLayout, FrameSize, Timebase};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
};
use jstream_platform::{AudioInputTarget, AudioProcess, Backends};
use jstream_protocol::{Batch, Edit, Event, OutputId, OutputState, Settings, SourceId, SourceKind};
use serde_json::json;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn engine(audio: &FakeAudioIo, clock: &FakeClock) -> Engine {
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
