//! An output captures what its render snapshots place (C3): one capture per display or
//! window its program mix shows, started when a source comes on program, stopped when it
//! goes, and started again after a failure.

use super::super::{add, batch, connected, create, edits, output_call};
use super::{Fakes, engine, fakes, pump_until, share, start};
use crate::RecordingPublish;
use crate::runtime::{RETRY_MS, cpu_draw};
use jstream_control::Control;
use jstream_media::FrameSize;
use jstream_platform::fake::FakeCapture;
use jstream_platform::{
    Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, DeviceId, MediaClock,
    PlatformError, VideoSurface,
};
use jstream_protocol::{Edit, ItemId, Settings, SourceId, SourceKind};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// What [`Targets`] was asked and what it is running, shared with the test.
#[derive(Default)]
struct Log {
    /// Every target started, in order, refused ones included.
    started: Vec<CaptureTarget>,
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
        let id = id(target);
        {
            let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
            log.started.push(target.clone());
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

/// The output captures each display and window its program scene shows, once however
/// many sources show it, and nothing else: not the primary display, which only fills an
/// output with no scene. A capture whose source leaves the program stops, and one that
/// comes back starts again.
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
        setup.until(|| setup.live() == 2),
        "two captures run: {}",
        setup.live()
    );
    assert_eq!(setup.starts(&display("d2")), 1, "shared by two sources");
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
        setup.until(|| setup.live() == 1),
        "the window's capture stopped"
    );
    batch(&control, edits(vec![add("a", "i4", "editor")])).expect("added");
    assert!(
        setup.until(|| setup.live() == 2 && setup.starts(&window("w1")) == 2),
        "the window's capture started again"
    );

    block_on_stop(&control);
    assert_eq!(setup.live(), 0, "stopping the output stops every capture");
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

fn block_on_stop(control: &dyn Control) {
    super::super::block_on(control.call(output_call("yt", false))).expect("output.stop");
}
