//! An output captures what its render snapshots place (C3, C4c): one capture per
//! display, window or webcam its program mix shows, started when a source comes on
//! program, stopped when it goes, and started again after a failure.

use super::super::{add, batch, connected, create, drain, edits, output_call, output_state, query};
use super::{Fakes, engine, fakes, pump_until, share, start};
use crate::runtime::{RETRY_MS, cpu_draw};
use crate::{Engine, RecordingPublish};
use jstream_control::Control;
use jstream_media::FrameSize;
use jstream_platform::fake::{FAKE_CAPTURE_CARD, FAKE_WEBCAM, FakeCapture, FakeVideoDevices};
use jstream_platform::{
    Capture, CaptureOptions, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo,
    CursorCapture, DeviceId, MediaClock, PlatformError, VideoSurface,
};
use jstream_protocol::{
    CaptureTargetKind, Edit, Event, ItemId, PixelSize, ProtocolError, Query, QueryResult, Settings,
    SourceId, SourceKind,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// What [`Targets`] was asked and what it is running, shared with the test.
#[derive(Default)]
struct Log {
    /// Every target started, in order, refused ones included.
    started: Vec<CaptureTarget>,
    /// How each start asked for the pointer, in the same order.
    cursors: Vec<CursorCapture>,
    /// Targets whose start is refused with `NotFound`.
    refused: BTreeSet<String>,
    /// Targets whose running streams report `DeviceLost`.
    lost: BTreeSet<String>,
}

/// Lists two displays and a window, and captures each with the fake display's stream.
struct Targets {
    inner: FakeCapture,
    log: Arc<Mutex<Log>>,
    /// Streams running now.
    live: Arc<AtomicUsize>,
}

fn display(id: &str) -> CaptureTarget {
    CaptureTarget::Display(DeviceId::new(id))
}

fn window(id: &str) -> CaptureTarget {
    CaptureTarget::Window(DeviceId::new(id))
}

fn id(target: &CaptureTarget) -> String {
    match target {
        CaptureTarget::Display(id) | CaptureTarget::Window(id) => id.as_str().to_owned(),
        _ => String::new(),
    }
}

impl Capture for Targets {
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        let size = FrameSize::new(64, 36).ok();
        let info = |target, name: &str, primary| CaptureTargetInfo {
            target,
            name: name.into(),
            size,
            primary,
        };
        Ok(vec![
            info(window("w1"), "Editor", false),
            info(display("d2"), "Second", false),
            info(display("d1"), "Main", true),
        ])
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.start_with(target, &CaptureOptions::default())
    }

    fn start_with(
        &self,
        target: &CaptureTarget,
        options: &CaptureOptions,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        let id = id(target);
        {
            let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
            log.started.push(target.clone());
            log.cursors.push(options.cursor);
            if log.refused.contains(&id) {
                return Err(PlatformError::NotFound { kind: "window", id });
            }
        }
        let fake = self.inner.targets()?.remove(0).target;
        let inner = self.inner.start(&fake)?;
        self.live.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(Stream {
            inner,
            id,
            log: Arc::clone(&self.log),
            live: Arc::clone(&self.live),
        }))
    }
}

struct Stream {
    inner: Box<dyn CaptureStream>,
    id: String,
    log: Arc<Mutex<Log>>,
    live: Arc<AtomicUsize>,
}

impl CaptureStream for Stream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        let lost = self
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .lost
            .contains(&self.id);
        if lost {
            return Err(PlatformError::DeviceLost {
                what: self.id.clone(),
                reason: "the test closed it".into(),
            });
        }
        self.inner.latest()
    }

    fn stats(&self) -> CaptureStats {
        self.inner.stats()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Setup {
    fakes: Fakes,
    log: Arc<Mutex<Log>>,
    live: Arc<AtomicUsize>,
}

impl Setup {
    fn new() -> Self {
        let mut fakes = fakes();
        let log = Arc::new(Mutex::new(Log::default()));
        let live = Arc::new(AtomicUsize::new(0));
        fakes.backends.capture = Arc::new(Targets {
            inner: FakeCapture::new(fakes.clock.clone(), FrameSize::new(64, 36).expect("size")),
            log: Arc::clone(&log),
            live: Arc::clone(&live),
        });
        Self { fakes, log, live }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn starts(&self, target: &CaptureTarget) -> usize {
        self.log().started.iter().filter(|t| *t == target).count()
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }

    /// Pumps the clock until `done`.
    fn until(&self, done: impl FnMut() -> bool) -> bool {
        pump_until(&self.fakes.clock, &self.fakes.audio, done)
    }
}

fn capture_source(source: &str, kind: &str, target: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(source),
        name: source.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        duplicated_from: None,
        settings: Settings([("target".to_owned(), json!(target))].into_iter().collect()),
    }
}

/// `capture.targets` lists what the backend can capture, displays first, with the IDs a
/// source's `target` takes, whether or not an output runs. With no backend it fails.
#[test]
fn capture_targets_lists_the_displays_then_the_windows() {
    let setup = Setup::new();
    let engine = engine(
        share(&setup.fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    let Ok(QueryResult::CaptureTargets { targets }) = query(&control, Query::CaptureTargets) else {
        panic!("a capture.targets result");
    };
    let listed: Vec<_> = targets
        .iter()
        .map(|info| {
            (
                info.kind,
                info.target.as_str(),
                info.name.as_str(),
                info.primary,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (CaptureTargetKind::Display, "d2", "Second", false),
            (CaptureTargetKind::Display, "d1", "Main", true),
            (CaptureTargetKind::Window, "w1", "Editor", false),
        ]
    );
    assert_eq!(
        targets[0].size,
        Some(PixelSize {
            width: 64,
            height: 36
        })
    );
    assert!(setup.log().started.is_empty(), "listing starts no capture");

    let bare = Engine::new();
    let refused = query(&connected(&bare), Query::CaptureTargets).expect_err("no backend");
    assert!(
        matches!(refused.protocol(), Some(ProtocolError::ActionFailed { .. })),
        "{refused:?}"
    );
}

/// The output captures each display and window its program scene shows, once for each
/// source that shows it (so a backend that keeps a grant per source keeps each source's
/// apart), and nothing else: not the primary display, which only fills an output with no
/// scene. A capture whose source leaves the program stops, and one that comes back
/// starts again.
#[test]
fn an_output_captures_what_its_program_shows() {
    let setup = Setup::new();
    let engine = engine(
        share(&setup.fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            capture_source("second", "display", "d2"),
            capture_source("again", "display", "d2"),
            capture_source("editor", "window", "w1"),
            add("a", "i1", "second"),
            add("a", "i2", "again"),
            add("a", "i3", "editor"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let mut events = control.events();
    start(&control, &mut events);
    assert!(
        setup.until(|| setup.live() == 3),
        "three captures run: {}",
        setup.live()
    );
    assert_eq!(setup.starts(&display("d2")), 2, "one for each source");
    assert_eq!(setup.starts(&window("w1")), 1);
    assert_eq!(
        setup.starts(&display("d1")),
        0,
        "the primary is not on program"
    );

    batch(
        &control,
        edits(vec![Edit::ItemRemove {
            item_id: ItemId::new("i3"),
        }]),
    )
    .expect("removed");
    assert!(
        setup.until(|| setup.live() == 2),
        "the window's capture stopped"
    );
    batch(&control, edits(vec![add("a", "i4", "editor")])).expect("added");
    assert!(
        setup.until(|| setup.live() == 3 && setup.starts(&window("w1")) == 2),
        "the window's capture started again"
    );

    block_on_stop(&control);
    assert_eq!(setup.live(), 0, "stopping the output stops every capture");
}

/// What `source.sizes` reports for each source, by ID.
fn sizes(control: &dyn Control) -> Vec<(String, Option<PixelSize>)> {
    match query(control, Query::SourceSizes).expect("source.sizes") {
        QueryResult::SourceSizes { sources } => sources
            .into_iter()
            .map(|entry| (entry.source_id.as_str().to_owned(), entry.size))
            .collect(),
        other => panic!("not source.sizes: {other:?}"),
    }
}

/// B12 part 2 (#805): a display or window source's size is its capture's frame size,
/// once the render thread has its first frame. It is announced once, and lost again
/// when the source leaves program, which stops its capture, or the output stops.
#[test]
fn capture_sources_report_their_captures_sizes() {
    let setup = Setup::new();
    let engine = engine(
        share(&setup.fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            capture_source("second", "display", "d2"),
            capture_source("editor", "window", "w1"),
            add("a", "i1", "second"),
            add("a", "i2", "editor"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let sized = Some(PixelSize {
        width: 64,
        height: 36,
    });
    assert_eq!(
        sizes(&control),
        vec![("editor".into(), None), ("second".into(), None)],
        "no capture runs before an output starts"
    );
    let mut announced = control.events();
    let mut events = control.events();
    start(&control, &mut events);
    assert!(
        setup.until(|| sizes(&control) == vec![("editor".into(), sized), ("second".into(), sized)]),
        "both captures have a frame: {:?}",
        sizes(&control)
    );
    let changed = |id: &str, size| Event::SourceSizeChanged {
        source_id: SourceId::new(id),
        size,
    };
    let size_events = |events: &mut jstream_control::EventStream| {
        let mut out: Vec<Event> = drain(events)
            .into_iter()
            .filter(|event| matches!(event, Event::SourceSizeChanged { .. }))
            .collect();
        out.sort_by_key(|event| format!("{event:?}"));
        out
    };
    assert_eq!(
        size_events(&mut announced),
        vec![changed("editor", sized), changed("second", sized)],
        "each size is announced once, though every frame repeats it"
    );

    batch(
        &control,
        edits(vec![Edit::ItemRemove {
            item_id: ItemId::new("i2"),
        }]),
    )
    .expect("removed");
    assert!(
        setup.until(|| sizes(&control) == vec![("editor".into(), None), ("second".into(), sized)]),
        "the window's capture stopped: {:?}",
        sizes(&control)
    );
    assert_eq!(size_events(&mut announced), vec![changed("editor", None)]);

    block_on_stop(&control);
    assert!(
        setup.until(|| sizes(&control) == vec![("editor".into(), None), ("second".into(), None)]),
        "the stopped stage forgot its captures: {:?}",
        sizes(&control)
    );
    assert_eq!(size_events(&mut announced), vec![changed("second", None)]);
}

/// A source whose `show_cursor` is off asks for the pointer to be left out; turning it on
/// starts that source's capture again with the pointer drawn in.
#[test]
fn a_capture_asks_for_the_pointer_as_its_source_says() {
    let setup = Setup::new();
    let engine = engine(
        share(&setup.fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    let mut hidden = capture_source("second", "display", "d2");
    if let Edit::SourceCreate { settings, .. } = &mut hidden {
        settings.insert("show_cursor", json!(false));
    }
    batch(
        &control,
        edits(vec![
            create("a"),
            hidden,
            capture_source("editor", "window", "w1"),
            add("a", "i1", "second"),
            add("a", "i2", "editor"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let mut events = control.events();
    start(&control, &mut events);
    assert!(setup.until(|| setup.live() == 2), "both captures run");
    let cursor = |target: &CaptureTarget| {
        let log = setup.log();
        log.started
            .iter()
            .zip(&log.cursors)
            .filter(|(t, _)| *t == target)
            .map(|(_, c)| *c)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        cursor(&display("d2")),
        vec![CursorCapture::Hidden],
        "left out"
    );
    assert_eq!(
        cursor(&window("w1")),
        vec![CursorCapture::Drawn],
        "the default"
    );

    batch(
        &control,
        edits(vec![Edit::SourceSetSettings {
            source_id: SourceId::new("second"),
            patch: Settings(
                [("show_cursor".to_owned(), json!(true))]
                    .into_iter()
                    .collect(),
            ),
        }]),
    )
    .expect("shown");
    assert!(
        setup.until(|| setup.starts(&display("d2")) == 2 && setup.live() == 2),
        "the display's capture started again"
    );
    assert_eq!(
        cursor(&display("d2")),
        vec![CursorCapture::Hidden, CursorCapture::Drawn]
    );
    assert_eq!(
        setup.starts(&window("w1")),
        1,
        "the window's capture kept running"
    );

    block_on_stop(&control);
}

/// A capture that doesn't start, or whose target goes away, is started again
/// [`RETRY_MS`] later on the media clock, while the output stays up.
#[test]
fn a_failed_capture_is_started_again() {
    let setup = Setup::new();
    setup.log().refused.insert("w1".into());
    let engine = engine(
        share(&setup.fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            capture_source("editor", "window", "w1"),
            add("a", "i1", "editor"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let mut events = control.events();
    start(&control, &mut events);
    assert!(
        setup.until(|| setup.starts(&window("w1")) == 1),
        "tried once"
    );
    assert_eq!(setup.live(), 0, "refused");

    setup.log().refused.clear();
    let before = setup.fakes.clock.now().ticks();
    assert!(
        setup.until(|| setup.live() == 1),
        "started again once the target is back"
    );
    assert_eq!(setup.starts(&window("w1")), 2);
    let waited = setup.fakes.clock.now().ticks() - before;
    assert!(
        waited + 100_000 >= RETRY_MS * 1_000,
        "not before the retry interval: {waited} us"
    );

    setup.log().lost.insert("w1".into());
    assert!(
        setup.until(|| setup.live() == 0),
        "a lost target's capture stops"
    );
    setup.log().lost.clear();
    assert!(
        setup.until(|| setup.live() == 1 && setup.starts(&window("w1")) == 3),
        "and starts again"
    );
    block_on_stop(&control);
}

/// The fakes with a video-device backend the test keeps a handle on.
fn with_devices() -> (Fakes, FakeVideoDevices) {
    let mut fakes = fakes();
    let devices = FakeVideoDevices::new(fakes.clock.clone());
    fakes.backends.video_devices = Arc::new(devices.clone());
    (fakes, devices)
}

fn webcam(source: &str, device: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(source),
        name: source.into(),
        kind: SourceKind::Builtin {
            kind: "webcam".into(),
        },
        duplicated_from: None,
        settings: Settings([("device".to_owned(), json!(device))].into_iter().collect()),
    }
}

/// A webcam on program is opened on the video-device backend while an output runs:
/// once for the sources that name the same device, closed when they leave the program
/// mix, and every one closed when the output stops. An empty `device` is the first
/// device listed.
#[test]
fn an_output_opens_the_webcams_its_program_shows() {
    let (fakes, devices) = with_devices();
    let engine = engine(
        share(&fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            webcam("cam", ""),
            webcam("again", ""),
            webcam("card", FAKE_CAPTURE_CARD),
            add("a", "i1", "cam"),
            add("a", "i2", "again"),
            add("a", "i3", "card"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let until = |done: &mut dyn FnMut() -> bool| pump_until(&fakes.clock, &fakes.audio, done);
    let mut events = control.events();
    start(&control, &mut events);
    assert!(
        until(&mut || devices.open_streams() == 2),
        "the webcam, shared by two sources, and the card: {}",
        devices.open_streams()
    );

    batch(
        &control,
        edits(vec![Edit::ItemRemove {
            item_id: ItemId::new("i3"),
        }]),
    )
    .expect("removed");
    assert!(
        until(&mut || devices.open_streams() == 1),
        "the card was closed"
    );

    block_on_stop(&control);
    assert_eq!(
        devices.open_streams(),
        0,
        "stopping the output closes the webcam"
    );
}

/// An unplugged webcam's stream reports `DeviceLost`: it is closed and tried again
/// every [`RETRY_MS`], which fails while the device is gone, and the output stays up.
#[test]
fn an_unplugged_webcam_is_closed_and_the_output_stays_up() {
    let (fakes, devices) = with_devices();
    let engine = engine(
        share(&fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    );
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            webcam("cam", FAKE_WEBCAM),
            add("a", "i1", "cam"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let mut events = control.events();
    start(&control, &mut events);
    assert!(
        pump_until(&fakes.clock, &fakes.audio, || devices.open_streams() == 1),
        "the webcam was opened"
    );

    devices.unplug(&DeviceId::new(FAKE_WEBCAM));
    assert!(
        pump_until(&fakes.clock, &fakes.audio, || devices.open_streams() == 0),
        "the lost webcam was closed"
    );
    let since = fakes.clock.now().ticks();
    assert!(pump_until(&fakes.clock, &fakes.audio, || {
        fakes.clock.now().ticks() - since > 2 * RETRY_MS * 1_000
    }));
    assert_eq!(
        devices.open_streams(),
        0,
        "a gone webcam doesn't open again"
    );
    assert_eq!(
        output_state(&control, "yt"),
        jstream_protocol::OutputState::Active
    );
    block_on_stop(&control);
}

/// A preview stream and an output showing the same webcam share its one open: the
/// preview opens it with no output running, the output's stage reads the same open
/// while it runs, and the preview keeps it once the output stops.
#[test]
fn the_preview_and_an_output_share_a_webcam() {
    let (fakes, devices) = with_devices();
    let engine = engine(
        share(&fakes.backends),
        cpu_draw(),
        Box::new(RecordingPublish::new()),
    )
    .with_preview_draw(crate::preview::cpu_draw());
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            webcam("cam", FAKE_WEBCAM),
            add("a", "i1", "cam"),
        ]),
    )
    .expect("created");
    assert!(engine.settle_sources(), "the sources were built in time");
    let _frames = control.binary();
    super::super::block_on(control.call(jstream_protocol::Call::Action {
        action: jstream_protocol::Action::PreviewSubscribe {
            channel: 1,
            max_width: None,
            max_height: None,
            fps: Some(30),
            codecs: Vec::new(),
            scene_id: None,
            source: None,
        },
    }))
    .expect("preview.subscribe");
    assert!(
        pump_until(&fakes.clock, &fakes.audio, || devices.open_streams() == 1),
        "the preview opened the webcam"
    );

    let mut events = control.events();
    start(&control, &mut events);
    let since = fakes.clock.now().ticks();
    let mut most = 0;
    assert!(pump_until(&fakes.clock, &fakes.audio, || {
        most = most.max(devices.open_streams());
        fakes.clock.now().ticks() - since > RETRY_MS * 1_000
    }));
    assert_eq!(most, 1, "the output's stage opened the webcam again");

    block_on_stop(&control);
    assert_eq!(
        devices.open_streams(),
        1,
        "the preview still shows the webcam"
    );
}

fn block_on_stop(control: &dyn Control) {
    super::super::block_on(control.call(output_call("yt", false))).expect("output.stop");
}
