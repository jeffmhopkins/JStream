//! Live sources (C1b, C3): the engine builds a `jstream-sources` source for each source
//! in the collection, rebuilds it when its settings change, drops it when the source is
//! deleted, and the program mix shows its frames, advancing animations on the clock, or
//! what the pipeline captures for it.

use super::{add, batch, connected, create, edits};
use crate::Engine;
use jstream_media::{FrameSize, Timebase};
use jstream_platform::fake::{FakeClip, FakeClock, FakeMediaDecoders};
use jstream_protocol::{AssetRoot, Batch, Edit, ItemId, Reference, Settings, SourceId, SourceKind};
use jstream_sources::CaptureSpec;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Pillow's 4x4 animated WebP from `jstream-sources`' tests: red for 100 ms, green for
/// 200 ms, then blue at alpha 128 for 300 ms, twice.
const ANIMATED: &[u8] = include_bytes!("../../../jstream-sources/tests/data/anim.webp");

fn settings(pairs: &[(&str, Value)]) -> Settings {
    Settings(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn source(id: &str, kind: &str, settings: Settings) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        settings,
        duplicated_from: None,
    }
}

fn color(hex: &str) -> Settings {
    settings(&[
        ("color", json!(hex)),
        ("width", json!(2)),
        ("height", json!(2)),
    ])
}

/// The first pixel the published program mix shows for `id`, once every build is done.
fn shown(engine: &Engine, id: &str) -> Option<[u8; 4]> {
    assert!(engine.settle_sources(), "the sources were built in time");
    engine
        .render_hub()
        .load()
        .shot()
        .frame(&SourceId::new(id))
        .and_then(|frame| frame.first_pixel())
}

#[test]
fn a_colour_source_is_built_rebuilt_and_dropped() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("fill", "color", color("#ff8000ff")),
            add("a", "i1", "fill"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "fill"), Some([0xff, 0x80, 0x00, 0xff]));

    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("fill"),
            patch: settings(&[("color", json!("#10203040"))]),
        }),
    )
    .expect("changed");
    assert_eq!(
        shown(&engine, "fill"),
        Some([0x10, 0x20, 0x30, 0x40]),
        "a settings change rebuilds the source"
    );

    batch(
        &control,
        Batch::single(Edit::ItemRemove {
            item_id: jstream_protocol::ItemId::new("i1"),
        }),
    )
    .expect("removed");
    assert_eq!(
        shown(&engine, "fill"),
        None,
        "a source no item places is not drawn"
    );
    batch(
        &control,
        Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("fill"),
        }),
    )
    .expect("deleted");
    assert_eq!(shown(&engine, "fill"), None);
}

#[test]
fn a_stored_frame_wins_over_the_live_source() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("fill", "color", color("#ff0000ff")),
            add("a", "i1", "fill"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "fill"), Some([0xff, 0, 0, 0xff]));
    engine
        .set_source_frame(&SourceId::new("fill"), 1, 1, &[1, 2, 3, 4])
        .expect("a frame");
    assert_eq!(shown(&engine, "fill"), Some([1, 2, 3, 4]));
}

#[test]
fn a_text_source_is_built_and_rebuilt() {
    let engine = Engine::new();
    let control = connected(&engine);
    let text = |background: &str| {
        settings(&[
            ("text", json!("Hi")),
            ("background", json!(background)),
            ("padding", json!(4)),
        ])
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("caption", "text", text("#00ff00ff")),
            add("a", "i1", "caption"),
        ]),
    )
    .expect("created");
    // The top left corner is padding, so it shows the background.
    assert_eq!(shown(&engine, "caption"), Some([0, 0xff, 0, 0xff]));

    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("caption"),
            patch: settings(&[("background", json!("#0000ffff"))]),
        }),
    )
    .expect("changed");
    assert_eq!(shown(&engine, "caption"), Some([0, 0, 0xff, 0xff]));
}

/// A `display` or `window` source shows what the pipeline captures for its target:
/// the primary display by default, the one its `target` names otherwise. Sources that
/// capture the same thing share one capture, and a window source with no window chosen
/// captures nothing.
#[test]
fn capture_sources_capture_their_targets() {
    let engine = Engine::new();
    let control = connected(&engine);
    let target = |id: &str| settings(&[("target", json!(id))]);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("screen", "display", Settings::new()),
            source("again", "display", Settings::new()),
            source("second", "display", target("monitor:2")),
            source("editor", "window", target("hwnd:0x2a")),
            source("unset", "window", Settings::new()),
            add("a", "i1", "screen"),
            add("a", "i2", "again"),
            add("a", "i3", "second"),
            add("a", "i4", "editor"),
            add("a", "i5", "unset"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let keys = |engine: &Engine| engine.render_hub().load().shot().captures().clone();
    let captures = |engine: &Engine| {
        keys(engine)
            .into_iter()
            .map(|key| key.spec)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        captures(&engine),
        BTreeSet::from([
            CaptureSpec::Display(None),
            CaptureSpec::Display(Some("monitor:2".into())),
            CaptureSpec::Window(Some("hwnd:0x2a".into())),
        ])
    );
    // Each source has a capture of its own, so a backend that keeps a grant per source
    // keeps them apart: `screen` and `again` show the same display through two.
    let sources: BTreeSet<_> = keys(&engine)
        .into_iter()
        .filter_map(|key| key.source.map(|id| id.as_str().to_owned()))
        .collect();
    assert_eq!(
        sources,
        BTreeSet::from(["again", "editor", "screen", "second"].map(String::from))
    );
    assert_eq!(shown(&engine, "screen"), None, "no frame of its own");

    batch(
        &control,
        edits(vec![Edit::SourceSetSettings {
            source_id: SourceId::new("editor"),
            patch: target("hwnd:0x2b"),
        }]),
    )
    .expect("retargeted");
    assert!(engine.settle_sources(), "the sources were built in time");
    assert!(
        captures(&engine).contains(&CaptureSpec::Window(Some("hwnd:0x2b".into()))),
        "a new target is a new capture"
    );
}

/// A new, empty directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-core-sources-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Which channel of the image's first pixel is lit, and its alpha. The lossless frames
/// come back with a channel a step off (255 as 254), so the test reads the dominant one.
fn colour(engine: &Engine) -> (usize, u8) {
    let pixel = shown(engine, "gif").expect("a frame");
    let lit = (0..3).max_by_key(|&c| pixel[c]).expect("three channels");
    (lit, pixel[3])
}

#[test]
fn an_animated_image_advances_on_the_clock() {
    let dir = temp_dir("animated");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    assert_eq!(colour(&engine), (0, 0xff), "red first");

    clock.advance(99);
    engine.replan();
    assert_eq!(colour(&engine), (0, 0xff), "still red");
    clock.advance(1);
    engine.replan();
    assert_eq!(colour(&engine), (1, 0xff), "green at 100 ms");
    // Past both plays (2 x 600 ms), the last frame stays.
    clock.advance(2_000);
    engine.replan();
    assert_eq!(colour(&engine), (2, 128), "blue at alpha 128");
    let _ = fs::remove_dir_all(dir);
}

/// #224: a re-plan timed on the old clock must not hold an animation back on the new one.
#[test]
fn an_animation_keeps_advancing_after_a_clock_swap() {
    let dir = temp_dir("clock-swap");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    // The old clock is far ahead, so its next frame is due long after the new clock's
    // zero.
    let old = FakeClock::new(Timebase::MILLIS);
    old.advance(1_000_000);
    engine.set_source_clock(Arc::new(old));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    assert_eq!(colour(&engine), (0, 0xff), "red first");

    let new = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(new.clone()));
    new.advance(100);
    // Only the sources' own thread re-plans here; it checks the clock at least once a
    // second.
    let deadline = Instant::now() + Duration::from_secs(10);
    while colour(&engine) != (1, 0xff) {
        assert!(
            Instant::now() < deadline,
            "green at 100 ms on the new clock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_missing_file_shows_nothing() {
    let dir = temp_dir("missing");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "nowhere.png".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("pic", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "pic"),
        ]),
    )
    .expect("an asset that doesn't exist yet is accepted");
    assert_eq!(shown(&engine, "pic"), None);
    let _ = fs::remove_dir_all(dir);
}

/// Waits until the sources thread, on its own, makes the program mix show `want`. The
/// thread re-reads a clock that is not wall time at least once a second, so the deadline
/// leaves room for a loaded machine.
fn shows_by_itself(engine: &Engine, want: (usize, u8)) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while colour(engine) != want {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}

#[test]
fn the_sources_thread_advances_an_animation_by_itself() {
    let dir = temp_dir("by-itself");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    // Time moves only when the test moves it, so each frame stays shown until the test
    // has seen it, however slow the machine (#293).
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    assert_eq!(colour(&engine), (0, 0xff), "red first");
    // The thread re-plans when each frame is due; no call touches the engine meanwhile.
    clock.advance(100);
    assert!(shows_by_itself(&engine, (1, 0xff)), "green at 100 ms");
    // Past both plays (2 x 600 ms), the last frame stays.
    clock.advance(2_000);
    assert!(shows_by_itself(&engine, (2, 128)), "blue at alpha 128");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_waveform_source_is_painted_by_the_sources_thread() {
    let engine = Engine::new();
    let control = connected(&engine);
    // One column, three rows: silence lights the middle row, full scale the top one.
    batch(
        &control,
        edits(vec![
            create("a"),
            source(
                "wave",
                "waveform",
                settings(&[
                    ("width", json!(1)),
                    ("height", json!(3)),
                    ("color", json!("#ff0000")),
                    ("background", json!("#0000ff")),
                ]),
            ),
            add("a", "i1", "wave"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "wave"), Some([0, 0, 0xff, 0xff]), "silence");
    // The program mix's audio arrives: the thread paints it on its own, off the lock.
    let scope = engine.audio_scope();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && shown(&engine, "wave") != Some([0xff, 0, 0, 0xff]) {
        scope.push(48_000, &[1.0; 960]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        shown(&engine, "wave"),
        Some([0xff, 0, 0, 0xff]),
        "full scale"
    );
}

/// Shows or hides item `i1`.
fn visible(control: &crate::InProcessControl, visible: bool) {
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

/// The fake clip frame `video` shows, re-planning until the decode thread has caught up
/// with the clock and the program mix shows `want`.
fn plays(engine: &Engine, want: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        engine.replan();
        if shown(engine, "video") == Some(FakeClip::pixel(want)) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    false
}

#[test]
fn a_media_file_plays_on_the_clock_and_restarts_when_shown_again() {
    let dir = temp_dir("media");
    fs::write(dir.join("clip.mp4"), b"fake").expect("the file");
    let media = FakeMediaDecoders::new();
    media.add(
        dir.join("clip.mp4").canonicalize().expect("the file"),
        FakeClip {
            size: FrameSize::new(4, 2).expect("a size"),
            frame_period: Timebase::new(1, 10).expect("a time base"),
            frames: 10,
            key_every: 1,
            audio: None,
        },
    );
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    engine.set_source_media(Arc::new(media));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "clip.mp4".into(),
    };
    let video = settings(&[
        ("file", file.to_value()),
        ("loop", json!(true)),
        ("speed", json!(1.0)),
        ("restart_on_show", json!(true)),
    ]);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("video", "media", video),
            add("a", "i1", "video"),
        ]),
    )
    .expect("created");
    assert!(plays(&engine, 0), "the first frame");
    clock.advance(250);
    assert!(plays(&engine, 2), "frame 2 at 250 ms");

    visible(&control, false);
    assert_eq!(shown(&engine, "video"), None, "hidden");
    clock.advance(300);
    visible(&control, true);
    assert!(plays(&engine, 0), "shown again, it plays from the start");
    clock.advance(100);
    assert!(plays(&engine, 1));
    let _ = fs::remove_dir_all(dir);
}

/// B2 of #245's review: a source built while no plan reaches it (here, behind an item
/// created hidden) starts from the beginning when the program first shows it, rather
/// than where its clock has run to.
#[test]
fn a_media_file_first_shown_long_after_it_was_built_plays_from_the_start() {
    let dir = temp_dir("media-hidden");
    fs::write(dir.join("clip.mp4"), b"fake").expect("the file");
    let media = FakeMediaDecoders::new();
    media.add(
        dir.join("clip.mp4").canonicalize().expect("the file"),
        FakeClip {
            size: FrameSize::new(4, 2).expect("a size"),
            frame_period: Timebase::new(1, 10).expect("a time base"),
            frames: 10,
            key_every: 1,
            audio: None,
        },
    );
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    engine.set_source_media(Arc::new(media.clone()));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "clip.mp4".into(),
    };
    let video = settings(&[
        ("file", file.to_value()),
        ("loop", json!(false)),
        ("speed", json!(1.0)),
        ("restart_on_show", json!(true)),
    ]);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("video", "media", video),
            add("a", "i1", "video"),
            Edit::ItemSetVisible {
                item_id: ItemId::new("i1"),
                scene_id: None,
                visible: false,
            },
        ]),
    )
    .expect("created hidden");
    assert_eq!(shown(&engine, "video"), None, "built, but hidden");
    assert_eq!(media.stats().opened, 1, "the file was opened");
    engine.replan();
    // Past the end of the one-second clip: without the restart it would show its last
    // frame.
    clock.advance(5000);
    engine.replan();
    visible(&control, true);
    assert!(plays(&engine, 0), "first shown, it plays from the start");
    clock.advance(100);
    assert!(plays(&engine, 1));
    let _ = fs::remove_dir_all(dir);
}

/// #422: a `media` source loaded from a collection file, before the host injects the
/// platform's media files, fails for want of them; injecting them builds it again, so
/// it plays without waiting for an edit.
#[test]
fn a_media_file_loaded_before_the_media_backend_plays_once_it_is_injected() {
    let dir = temp_dir("media-loaded");
    fs::write(dir.join("clip.mp4"), b"fake").expect("the file");
    let media = FakeMediaDecoders::new();
    media.add(
        dir.join("clip.mp4").canonicalize().expect("the file"),
        FakeClip {
            size: FrameSize::new(4, 2).expect("a size"),
            frame_period: Timebase::new(1, 10).expect("a time base"),
            frames: 10,
            key_every: 1,
            audio: None,
        },
    );
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "clip.mp4".into(),
    };
    let video = settings(&[
        ("file", file.to_value()),
        ("loop", json!(true)),
        ("speed", json!(1.0)),
        ("restart_on_show", json!(false)),
    ]);
    {
        // The run that creates the source and saves it to the collection file.
        let engine = Engine::new()
            .with_collection_file(dir.join("scenes.json"))
            .expect("collection file");
        let control = connected(&engine);
        batch(
            &control,
            edits(vec![
                create("a"),
                source("video", "media", video),
                add("a", "i1", "video"),
            ]),
        )
        .expect("created");
    }

    // The next run loads it before the media files are injected, as a host does.
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    assert_eq!(shown(&engine, "video"), None, "no media files yet");
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    engine.set_source_media(Arc::new(media));
    assert!(
        plays(&engine, 0),
        "it plays once the media files are injected"
    );
    clock.advance(150);
    assert!(plays(&engine, 1), "and advances on the clock");
    let _ = fs::remove_dir_all(dir);
}

/// A capture backend that records what it was asked, around the fake capture.
struct Recording {
    inner: jstream_platform::fake::FakeCapture,
    started: std::sync::Mutex<Vec<(Option<String>, jstream_platform::CursorCapture)>>,
    forgot: std::sync::Mutex<Vec<String>>,
}

impl jstream_platform::Capture for Recording {
    fn targets(
        &self,
    ) -> Result<Vec<jstream_platform::CaptureTargetInfo>, jstream_platform::PlatformError> {
        self.inner.targets()
    }

    fn start(
        &self,
        target: &jstream_platform::CaptureTarget,
    ) -> Result<Box<dyn jstream_platform::CaptureStream>, jstream_platform::PlatformError> {
        self.start_with(target, &jstream_platform::CaptureOptions::default())
    }

    fn start_with(
        &self,
        target: &jstream_platform::CaptureTarget,
        options: &jstream_platform::CaptureOptions,
    ) -> Result<Box<dyn jstream_platform::CaptureStream>, jstream_platform::PlatformError> {
        crate::lock(&self.started).push((options.source.clone(), options.cursor));
        self.inner.start(target)
    }

    fn forget_source(&self, source: &str) {
        crate::lock(&self.forgot).push(source.to_owned());
    }
}

fn recording() -> Arc<Recording> {
    Arc::new(Recording {
        inner: jstream_platform::fake::FakeCapture::new(
            FakeClock::new(Timebase::MICROS),
            FrameSize::new(64, 36).expect("size"),
        ),
        started: std::sync::Mutex::new(Vec::new()),
        forgot: std::sync::Mutex::new(Vec::new()),
    })
}

#[test]
fn a_sources_capture_is_started_for_that_source() {
    let capture = recording();
    let backends = crate::runtime::CaptureBackends {
        cpu: Arc::clone(&capture) as Arc<dyn jstream_platform::Capture>,
        gpu: None,
    };
    let key = crate::snapshot::CaptureKey::for_source(
        CaptureSpec::Display(None),
        SourceId::new("screen"),
    );
    crate::runtime::start_capture(&backends, &key).expect("the capture starts");
    crate::runtime::start_capture(&backends, &crate::snapshot::CaptureKey::primary())
        .expect("the primary display starts");
    crate::runtime::start_capture(&backends, &key.clone().with_cursor(false))
        .expect("the capture without the pointer starts");
    use jstream_platform::CursorCapture::{Drawn, Hidden};
    assert_eq!(
        *crate::lock(&capture.started),
        vec![
            (Some("screen".to_owned()), Drawn),
            (None, Drawn),
            (Some("screen".to_owned()), Hidden)
        ],
        "a source's capture names it, the primary display without a scene names none, \
         and a capture without the pointer asks for it to be left out"
    );
}

#[test]
fn deleting_a_source_forgets_its_capture_grant() {
    use jstream_media::ChannelLayout;
    use jstream_platform::Backends;
    use jstream_platform::fake::{
        FakeAudioIo, FakeCodecs, FakeGpuInterop, FakeMediaDecoders, FakeVideoDevices,
    };

    let capture = recording();
    let clock = FakeClock::new(Timebase::MICROS);
    let backends = Backends {
        clock: Arc::new(clock.clone()),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::clone(&capture) as Arc<dyn jstream_platform::Capture>,
        audio: Arc::new(FakeAudioIo::new(clock, 48_000, ChannelLayout::Stereo).expect("audio")),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new().with_backends(backends);
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            source("screen", "display", Settings::new()),
            source("other", "display", Settings::new()),
        ]),
    )
    .expect("created");
    batch(
        &control,
        edits(vec![Edit::SourceDelete {
            source_id: SourceId::new("screen"),
        }]),
    )
    .expect("deleted");
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::lock(&capture.forgot).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*crate::lock(&capture.forgot), vec!["screen".to_owned()]);
}
