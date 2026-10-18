//! The engine's end of the browser helper's pipe (ARCH › Browser source): pages opened,
//! restyled and closed by message, and the helper's frames, audio and refused-request
//! counts handed to each page as they arrive. Platform-neutral, over any pair of byte
//! streams, so it is tested here against a scripted helper; the CEF backend runs it over
//! the helper process's named pipes.
//!
//! **Threads.** One reader thread per link reads everything the helper sends. It stamps
//! frames and audio on the media clock as they arrive (the pipe carries no times),
//! imports each frame through the page's [`PageFrames`] before telling the helper the
//! texture is free, and pushes audio into the page's [`PageAudio`]. Requests go out
//! from the caller's thread under a lock.
//!
//! **A helper that stops** (it crashed, or its pipe broke) ends the link: every page's
//! audio fails, so the engine's input sees the stream end, frames stop, and every later
//! request fails. The backend starts a new helper for the next page.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use jstream_media::MediaTime;
use jstream_platform::{GpuFrame, MediaClock, PlatformError};

use crate::wire::{self, ToEngine, ToHelper};
use crate::{BrowserError, BrowserPage, PageAudio, PageSpec};

/// How long the helper may take to answer an open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// One paint the helper made into a shared texture of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Paint {
    /// Which of the page's textures.
    pub(crate) slot: u8,
    /// The texture's NT handle, in the helper's process.
    pub(crate) handle: u64,
    /// Its size in pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Makes each page's frame importer: the D3D12 import on Windows, a recorder in tests.
pub(crate) trait Frames: Send + Sync {
    /// A new page's importer.
    fn page(&self) -> Box<dyn PageFrames>;
}

/// One page's frames on the engine's device.
pub(crate) trait PageFrames: Send + Sync {
    /// Takes `paint`, which arrived at `pts`, as the page's latest frame. Returns once
    /// the engine no longer reads the helper's texture, which the helper then reuses.
    fn paint(&self, paint: &Paint, pts: MediaTime);
    /// The latest frame, if any.
    fn latest(&self) -> Option<GpuFrame>;
}

/// The engine's end of one helper's pipe.
pub(crate) struct Link {
    to_helper: Mutex<Box<dyn Write + Send>>,
    /// Open pages by number. Its lock also orders opening against the link ending, so
    /// no page is added after the reader has failed them all.
    pages: Mutex<Pages>,
    frames: Arc<dyn Frames>,
    clock: Arc<dyn MediaClock>,
    /// Set by [`shutdown`](Self::shutdown): the helper's end is then expected, not a
    /// failure.
    stopping: AtomicBool,
    next_page: AtomicU32,
    /// The helper's answer to [`ToHelper::Start`], taken by [`start`](Self::start).
    ready: Mutex<Option<Receiver<Result<(), String>>>>,
}

struct Pages {
    alive: bool,
    open: HashMap<u32, Arc<PageLink>>,
}

/// What the reader hands one page.
struct PageLink {
    audio: Arc<PageAudio>,
    frames: Box<dyn PageFrames>,
    denied: AtomicU64,
    /// Answered when the helper confirms or refuses the open.
    opened: Mutex<Option<SyncSender<Result<(), String>>>>,
}

impl Link {
    /// A link over the helper's two streams. Starts the reader thread, which ends when
    /// the helper's stream does.
    ///
    /// # Errors
    /// The thread can't be started.
    pub(crate) fn connect(
        from_helper: impl Read + Send + 'static,
        to_helper: impl Write + Send + 'static,
        frames: Arc<dyn Frames>,
        clock: Arc<dyn MediaClock>,
    ) -> Result<(Arc<Self>, JoinHandle<()>), BrowserError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let link = Arc::new(Self {
            to_helper: Mutex::new(Box::new(to_helper)),
            pages: Mutex::new(Pages {
                alive: true,
                open: HashMap::new(),
            }),
            frames,
            clock,
            stopping: AtomicBool::new(false),
            next_page: AtomicU32::new(1),
            ready: Mutex::new(Some(ready_rx)),
        });
        let reader = Arc::clone(&link);
        let thread = std::thread::Builder::new()
            .name("jstream-browser-link".into())
            .spawn(move || reader.read(from_helper, ready_tx))
            .map_err(|e| start(format!("starting the browser helper's reader: {e}")))?;
        Ok((link, thread))
    }

    /// Asks the helper to start CEF and waits up to `timeout` for it.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when the helper reports a failure, stops, or doesn't
    /// answer in time.
    pub(crate) fn start(
        &self,
        cache_root: String,
        log_file: Option<String>,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let ready = lock(&self.ready)
            .take()
            .ok_or_else(|| start("the browser helper was already started".into()))?;
        if !self.send(&ToHelper::Start {
            cache_root,
            log_file,
        }) {
            return Err(start("the browser helper's pipe closed".into()));
        }
        match ready.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(start(reason)),
            Err(RecvTimeoutError::Timeout) => Err(start(format!(
                "the browser helper didn't start within {} s",
                timeout.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(start("the browser helper stopped while starting".into()))
            }
        }
    }

    /// Whether the helper is still there.
    pub(crate) fn is_alive(&self) -> bool {
        lock(&self.pages).alive
    }

    /// Copies pages' paints onto the adapter with this LUID from now on, or delivers no
    /// frames (`None`).
    pub(crate) fn use_adapter(&self, luid: Option<u64>) {
        self.send(&ToHelper::Adapter { luid });
    }

    /// Opens `spec` in the helper and waits for it to confirm.
    ///
    /// # Errors
    /// [`BrowserError::Open`] when the helper refuses the page, has stopped, or doesn't
    /// answer within [`OPEN_TIMEOUT`].
    pub(crate) fn open(self: &Arc<Self>, spec: &PageSpec) -> Result<LinkPage, BrowserError> {
        let id = self.next_page.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        let page = Arc::new(PageLink {
            audio: PageAudio::new(),
            frames: self.frames.page(),
            denied: AtomicU64::new(0),
            opened: Mutex::new(Some(tx)),
        });
        {
            let mut pages = lock(&self.pages);
            if !pages.alive {
                return Err(open("the browser helper has stopped".into()));
            }
            pages.open.insert(id, Arc::clone(&page));
        }
        if !self.send(&ToHelper::Open {
            page: id,
            spec: spec.clone(),
        }) {
            lock(&self.pages).open.remove(&id);
            return Err(open("the browser helper's pipe closed".into()));
        }
        match rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(LinkPage {
                link: Arc::clone(self),
                id,
                page,
            }),
            Ok(Err(reason)) => {
                lock(&self.pages).open.remove(&id);
                Err(open(reason))
            }
            Err(RecvTimeoutError::Timeout) => {
                self.close(id);
                Err(open("the browser helper didn't answer".into()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(open(
                "the browser helper stopped while opening the page".into(),
            )),
        }
    }

    /// Asks the helper to close every page and exit. Its end is expected from now on.
    pub(crate) fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.send(&ToHelper::Shutdown);
    }

    fn close(&self, id: u32) {
        lock(&self.pages).open.remove(&id);
        self.send(&ToHelper::Close { page: id });
    }

    /// Sends one message; `false` if the pipe failed, which the reader will see too.
    fn send(&self, message: &ToHelper) -> bool {
        match wire::send(&mut *lock(&self.to_helper), message) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "a message to the browser helper wasn't sent");
                false
            }
        }
    }

    fn page(&self, id: u32) -> Option<Arc<PageLink>> {
        lock(&self.pages).open.get(&id).cloned()
    }

    /// The reader thread: the helper's answer to the start, then everything it sends
    /// until its stream ends or breaks.
    fn read(&self, mut from_helper: impl Read, ready: SyncSender<Result<(), String>>) {
        let started = match wire::receive::<ToEngine>(&mut from_helper) {
            Ok(Some(ToEngine::Ready)) => Ok(()),
            Ok(Some(ToEngine::StartFailed { reason })) => Err(reason),
            Ok(Some(_)) => Err("the browser helper didn't answer the start".into()),
            Ok(None) => Err("the browser helper stopped while starting".into()),
            Err(e) => Err(e.to_string()),
        };
        let ok = started.is_ok();
        let _ = ready.send(started);
        drop(ready);
        let ended = if ok {
            loop {
                match wire::receive::<ToEngine>(&mut from_helper) {
                    Ok(Some(message)) => self.handle(message),
                    Ok(None) => break None,
                    Err(e) => break Some(e.to_string()),
                }
            }
        } else {
            None
        };
        self.end(ended);
    }

    fn handle(&self, message: ToEngine) {
        match message {
            ToEngine::Opened { page } => self.answer(page, Ok(())),
            ToEngine::OpenFailed { page, reason } => self.answer(page, Err(reason)),
            ToEngine::Frame {
                page,
                slot,
                handle,
                width,
                height,
            } => {
                let pts = self.clock.now();
                if let Some(state) = self.page(page) {
                    let paint = Paint {
                        slot,
                        handle,
                        width,
                        height,
                    };
                    state.frames.paint(&paint, pts);
                }
                // Even for a page closed meanwhile: the helper waits for every slot.
                self.send(&ToHelper::Released { page, slot });
            }
            ToEngine::Audio { page, samples } => {
                let arrived = self.clock.now();
                if let Some(state) = self.page(page) {
                    state.audio.push_interleaved(&samples, Some(arrived));
                }
            }
            ToEngine::AudioFailed { page, reason } => {
                if let Some(state) = self.page(page) {
                    tracing::warn!(%reason, "a browser page's audio stream failed");
                    state.audio.fail(&lost(reason));
                }
            }
            ToEngine::Denied { page, count } => {
                if let Some(state) = self.page(page) {
                    // The helper names no URL, which may be a local path: only the count.
                    if state.denied.fetch_max(count, Ordering::Relaxed) == 0 {
                        tracing::warn!(denied = count, "a browser page's request was refused");
                    } else {
                        tracing::debug!(denied = count, "a browser page's request was refused");
                    }
                }
            }
            ToEngine::Warning { message } => {
                tracing::warn!(%message, "the browser helper");
            }
            ToEngine::Closed { page } => {
                tracing::debug!(page, "a browser page closed in the helper");
            }
            ToEngine::Ready | ToEngine::StartFailed { .. } => {
                tracing::debug!("the browser helper repeated its start answer");
            }
        }
    }

    /// Hands the helper's answer to the open waiting for `page`.
    fn answer(&self, page: u32, result: Result<(), String>) {
        if let Some(state) = self.page(page)
            && let Some(tx) = lock(&state.opened).take()
        {
            let _ = tx.send(result);
        }
    }

    /// The helper's stream ended (`error` says how, if it broke): fails every page.
    fn end(&self, error: Option<String>) {
        let pages = {
            let mut pages = lock(&self.pages);
            pages.alive = false;
            std::mem::take(&mut pages.open)
        };
        let stopping = self.stopping.load(Ordering::Acquire);
        if !stopping {
            tracing::warn!(
                error = error.as_deref().unwrap_or("its pipe closed"),
                pages = pages.len(),
                "the browser helper stopped"
            );
        }
        for state in pages.values() {
            lock(&state.opened).take();
            if !stopping {
                state.audio.fail(&lost("the browser helper stopped".into()));
            }
        }
    }
}

/// A page open in the helper. Dropping it closes the page.
pub(crate) struct LinkPage {
    link: Arc<Link>,
    id: u32,
    page: Arc<PageLink>,
}

impl BrowserPage for LinkPage {
    fn frame(&self) -> Option<GpuFrame> {
        self.page.frames.latest()
    }

    fn set_css(&mut self, css: &str) {
        self.link.send(&ToHelper::SetCss {
            page: self.id,
            css: css.to_owned(),
        });
    }

    fn audio(&self) -> &Arc<PageAudio> {
        &self.page.audio
    }

    fn denied_requests(&self) -> u64 {
        self.page.denied.load(Ordering::Relaxed)
    }
}

impl Drop for LinkPage {
    fn drop(&mut self) {
        self.link.close(self.id);
    }
}

fn lost(reason: String) -> PlatformError {
    PlatformError::DeviceLost {
        what: "the browser page's audio".into(),
        reason,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}

fn open(reason: String) -> BrowserError {
    BrowserError::Open { reason }
}

#[cfg(test)]
mod tests {
    use std::io::{PipeReader, PipeWriter};
    use std::path::PathBuf;

    use jstream_media::{FrameSize, Timebase};
    use jstream_platform::{AudioInputHandler, AudioPacket};

    use super::*;
    use crate::PageLocation;

    /// A clock that reads one second.
    struct Fixed;

    impl MediaClock for Fixed {
        fn timebase(&self) -> Timebase {
            Timebase::per_second(1000).expect("ms")
        }
        fn now(&self) -> MediaTime {
            MediaTime::new(1000, self.timebase())
        }
    }

    /// Every paint of every page, as it arrived.
    type Paints = Arc<Mutex<Vec<(Paint, MediaTime)>>>;

    /// Records every paint of every page.
    #[derive(Default)]
    struct Recorder(Paints);

    impl Frames for Recorder {
        fn page(&self) -> Box<dyn PageFrames> {
            Box::new(Recorder(Arc::clone(&self.0)))
        }
    }

    impl PageFrames for Recorder {
        fn paint(&self, paint: &Paint, pts: MediaTime) {
            lock(&self.0).push((*paint, pts));
        }
        fn latest(&self) -> Option<GpuFrame> {
            None
        }
    }

    /// A scripted helper: what the engine sent it, and its end of the pipes.
    struct Helper {
        from_engine: PipeReader,
        to_engine: PipeWriter,
    }

    impl Helper {
        fn expect(&mut self) -> ToHelper {
            wire::receive(&mut self.from_engine)
                .expect("receive")
                .expect("a message")
        }
        fn say(&mut self, message: &ToEngine) {
            wire::send(&mut self.to_engine, message).expect("send");
        }
    }

    fn connect() -> (Arc<Link>, JoinHandle<()>, Helper, Paints) {
        let (from_engine, to_helper) = std::io::pipe().expect("pipe");
        let (from_helper, to_engine) = std::io::pipe().expect("pipe");
        let recorder = Recorder::default();
        let paints = Arc::clone(&recorder.0);
        let (link, thread) =
            Link::connect(from_helper, to_helper, Arc::new(recorder), Arc::new(Fixed))
                .expect("connect");
        (
            link,
            thread,
            Helper {
                from_engine,
                to_engine,
            },
            paints,
        )
    }

    /// Starts the link against `helper`, which answers on another thread.
    fn start(link: &Link, helper: Helper) -> Helper {
        let answer = std::thread::spawn(move || {
            let mut helper = helper;
            assert_eq!(
                helper.expect(),
                ToHelper::Start {
                    cache_root: "cache".into(),
                    log_file: None,
                }
            );
            helper.say(&ToEngine::Ready);
            helper
        });
        link.start("cache".into(), None, Duration::from_secs(5))
            .expect("started");
        answer.join().expect("helper")
    }

    fn spec() -> PageSpec {
        PageSpec {
            location: PageLocation::File(PathBuf::from("/overlay/index.html")),
            size: FrameSize::new(640, 360).expect("size"),
            fps: 30,
            css: String::new(),
            local_network: false,
        }
    }

    /// Opens a page, the helper confirming it.
    fn open(link: &Arc<Link>, mut helper: Helper) -> (LinkPage, Helper) {
        let answer = std::thread::spawn(move || {
            let ToHelper::Open { page, spec: got } = helper.expect() else {
                panic!("not an open");
            };
            assert_eq!(got, spec());
            helper.say(&ToEngine::Opened { page });
            helper
        });
        let page = link.open(&spec()).expect("opened");
        (page, answer.join().expect("helper"))
    }

    #[derive(Default)]
    struct Heard {
        samples: Vec<f32>,
        times: Vec<Option<MediaTime>>,
        errors: Vec<String>,
    }

    struct Listener(Arc<Mutex<Heard>>);

    impl AudioInputHandler for Listener {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            let mut heard = lock(&self.0);
            heard.samples.extend_from_slice(packet.samples);
            heard.times.push(packet.time);
        }
        fn on_error(&mut self, error: &PlatformError) {
            lock(&self.0).errors.push(error.to_string());
        }
    }

    /// Waits for `done` to hold, for up to five seconds.
    fn eventually(mut done: impl FnMut() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < end, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_page_gets_its_frames_audio_and_refusals() {
        let (link, thread, helper, paints) = connect();
        let helper = start(&link, helper);
        let (mut page, mut helper) = open(&link, helper);
        link.use_adapter(Some(42));
        assert_eq!(helper.expect(), ToHelper::Adapter { luid: Some(42) });
        let heard = Arc::new(Mutex::new(Heard::default()));
        let _stream = page.audio().open(Box::new(Listener(Arc::clone(&heard))));

        // A frame is imported, then its slot released.
        helper.say(&ToEngine::Frame {
            page: 1,
            slot: 2,
            handle: 0x44,
            width: 640,
            height: 360,
        });
        assert_eq!(helper.expect(), ToHelper::Released { page: 1, slot: 2 });
        assert_eq!(
            lock(&paints).as_slice(),
            &[(
                Paint {
                    slot: 2,
                    handle: 0x44,
                    width: 640,
                    height: 360
                },
                Fixed.now()
            )]
        );
        // Audio is stamped on arrival, less its length: 480 frames, 10 ms.
        helper.say(&ToEngine::Audio {
            page: 1,
            samples: vec![0.5; 960],
        });
        helper.say(&ToEngine::Denied { page: 1, count: 3 });
        // Messages for a page the engine doesn't know are ignored, but a frame's slot
        // is still released.
        helper.say(&ToEngine::Audio {
            page: 9,
            samples: vec![0.5; 2],
        });
        helper.say(&ToEngine::Frame {
            page: 9,
            slot: 0,
            handle: 1,
            width: 1,
            height: 1,
        });
        assert_eq!(helper.expect(), ToHelper::Released { page: 9, slot: 0 });
        eventually(|| page.denied_requests() == 3);
        {
            let heard = lock(&heard);
            assert_eq!(heard.samples, vec![0.5; 960]);
            assert_eq!(
                heard.times,
                vec![Some(MediaTime::new(
                    990,
                    Timebase::per_second(1000).expect("ms")
                ))]
            );
        }
        assert_eq!(lock(&paints).len(), 1);

        page.set_css("body { color: red }");
        assert_eq!(
            helper.expect(),
            ToHelper::SetCss {
                page: 1,
                css: "body { color: red }".into()
            }
        );
        // A failed stream fails the engine's input.
        helper.say(&ToEngine::AudioFailed {
            page: 1,
            reason: "gone".into(),
        });
        eventually(|| !lock(&heard).errors.is_empty());
        drop(page);
        assert_eq!(helper.expect(), ToHelper::Close { page: 1 });

        // Shutting down: the helper's end is expected.
        link.shutdown();
        assert_eq!(helper.expect(), ToHelper::Shutdown);
        drop(helper);
        thread.join().expect("reader");
        assert!(!link.is_alive());
    }

    #[test]
    fn refusals_from_the_helper_are_errors() {
        // A failed start.
        let (link, thread, mut helper, _) = connect();
        let answer = std::thread::spawn(move || {
            helper.expect();
            helper.say(&ToEngine::StartFailed {
                reason: "no libcef.dll".into(),
            });
            helper
        });
        assert_eq!(
            link.start("cache".into(), None, Duration::from_secs(5)),
            Err(BrowserError::Start {
                reason: "no libcef.dll".into()
            })
        );
        drop(answer.join().expect("helper"));
        thread.join().expect("reader");

        // A refused page.
        let (link, thread, helper, _) = connect();
        let mut helper = start(&link, helper);
        let answer = std::thread::spawn(move || {
            let ToHelper::Open { page, .. } = helper.expect() else {
                panic!("not an open");
            };
            helper.say(&ToEngine::OpenFailed {
                page,
                reason: "CEF refused to create the browser".into(),
            });
            helper
        });
        assert!(matches!(
            link.open(&spec()),
            Err(BrowserError::Open { reason }) if reason == "CEF refused to create the browser"
        ));
        drop(answer.join().expect("helper"));
        thread.join().expect("reader");
    }

    #[test]
    fn a_helper_that_stops_fails_its_pages() {
        let (link, thread, helper, _) = connect();
        let helper = start(&link, helper);
        let (page, helper) = open(&link, helper);
        let heard = Arc::new(Mutex::new(Heard::default()));
        let _stream = page.audio().open(Box::new(Listener(Arc::clone(&heard))));
        // The helper crashes: its ends of both pipes close.
        drop(helper);
        thread.join().expect("reader");
        assert!(!link.is_alive());
        assert_eq!(lock(&heard).errors.len(), 1);
        assert!(matches!(link.open(&spec()), Err(BrowserError::Open { .. })));
        // Closing the page afterwards is harmless.
        drop(page);
    }

    #[test]
    fn a_helper_that_never_answers_times_out() {
        let (link, thread, helper, _) = connect();
        assert!(matches!(
            link.start("cache".into(), None, Duration::from_millis(50)),
            Err(BrowserError::Start { .. })
        ));
        drop(helper);
        thread.join().expect("reader");
    }
}
