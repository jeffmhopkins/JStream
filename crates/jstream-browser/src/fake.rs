//! An in-memory [`BrowserBackend`] for tests (feature `fake`): it records the pages it
//! opens, and a test plays audio into them and reads back their CSS and whether they
//! closed. Its pages never paint. A test plays a page's requests through its
//! [`RequestPolicy`] with [`FakePageState::request`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use jstream_platform::{GpuContext, GpuFrame};

use crate::{
    BrowserBackend, BrowserError, BrowserPage, PageAudio, PageSpec, RequestPolicy, Verdict,
};

/// A browser that opens pages in memory.
#[derive(Debug, Default)]
pub struct FakeBrowser {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    pages: Vec<Arc<FakePageState>>,
    refuse: Option<BrowserError>,
    device: bool,
}

/// What a test sees of one page the fake opened.
#[derive(Debug)]
pub struct FakePageState {
    spec: PageSpec,
    css: Mutex<Vec<String>>,
    audio: Arc<PageAudio>,
    policy: RequestPolicy,
    closed: AtomicBool,
}

impl FakePageState {
    /// The spec the page was opened with.
    pub fn spec(&self) -> &PageSpec {
        &self.spec
    }

    /// The CSS it was given after it opened, in order.
    pub fn css_updates(&self) -> Vec<String> {
        self.css
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Its audio, to push packets into as CEF would.
    pub fn audio(&self) -> &Arc<PageAudio> {
        &self.audio
    }

    /// Makes a request as the page would, and says whether its policy let it through.
    pub fn request(&self, url: &str) -> bool {
        self.policy.check(url) == Verdict::Allow
    }

    /// Whether the page was dropped.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl FakeBrowser {
    /// A browser with no pages, which opens every page.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every page opened so far, closed ones included, oldest first.
    pub fn pages(&self) -> Vec<Arc<FakePageState>> {
        self.lock().pages.clone()
    }

    /// Makes every later [`open`](BrowserBackend::open) fail with `error`, or succeed
    /// again with `None`.
    pub fn refuse(&self, error: Option<BrowserError>) {
        self.lock().refuse = error;
    }

    /// Whether the host last gave it a device ([`BrowserBackend::use_device`]).
    pub fn has_device(&self) -> bool {
        self.lock().device
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl BrowserBackend for FakeBrowser {
    fn open(&self, spec: &PageSpec) -> Result<Box<dyn BrowserPage>, BrowserError> {
        let mut state = self.lock();
        if let Some(error) = &state.refuse {
            return Err(error.clone());
        }
        let page = Arc::new(FakePageState {
            spec: spec.clone(),
            css: Mutex::new(Vec::new()),
            audio: PageAudio::new(),
            policy: RequestPolicy::for_page(spec),
            closed: AtomicBool::new(false),
        });
        state.pages.push(Arc::clone(&page));
        Ok(Box::new(FakePage(page)))
    }

    fn use_device(&self, gpu: Option<&GpuContext>) {
        self.lock().device = gpu.is_some();
    }
}

/// A page the fake opened.
struct FakePage(Arc<FakePageState>);

impl BrowserPage for FakePage {
    fn frame(&self) -> Option<GpuFrame> {
        None
    }

    fn set_css(&mut self, css: &str) {
        self.0
            .css
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(css.to_owned());
    }

    fn audio(&self) -> &Arc<PageAudio> {
        &self.0.audio
    }

    fn denied_requests(&self) -> u64 {
        self.0.policy.denied()
    }
}

impl Drop for FakePage {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
    }
}
