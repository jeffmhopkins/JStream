//! Browser sources' engine (ADR-0021, docs/adr/0021-browser-source.md; ADR-0046,
//! docs/adr/0046-browser-crate-and-helper.md; ADR-0048,
//! docs/adr/0048-sandboxed-browser-helper.md): web pages rendered off screen by CEF in a
//! sandboxed helper process, each page's frames as textures on a GPU device the host hands
//! it and its audio as a stream the engine records like a device's.
//!
//! The seam is [`BrowserBackend`], which `jstream-sources` holds injected like every
//! other backend: [`open`](BrowserBackend::open) a [`PageSpec`] and get a
//! [`BrowserPage`]. The platform-neutral parts are built everywhere: the page's location
//! and custom CSS ([`PageLocation`], [`css_script`]), the audio hand-off ([`PageAudio`])
//! and, with the `fake` feature, an in-memory backend for tests. The CEF backend
//! (`CefBackend`), which starts the helper and drives it over pipes, is built on Windows
//! and Linux with the `cef` feature only. The CEF code the helper runs (`run_helper`) is
//! built on Windows and Linux with the `helper` feature only, which
//! `jstream-browser-helper` turns on, so no other build downloads or compiles CEF. On
//! Windows pages' frames cross as shared textures; on Linux the helper paints in
//! software and frames cross in CPU memory (ADR-0095, docs/adr/0095-browser-helper-on-linux.md).
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-browser/README.md#purpose, docs/adr/0021-browser-source.md, docs/adr/0046-browser-crate-and-helper.md, docs/adr/0048-sandboxed-browser-helper.md, docs/adr/0095-browser-helper-on-linux.md, docs/security/threat-model.md#browser

mod audio;
#[cfg(all(any(windows, target_os = "linux"), feature = "cef"))]
mod cef;
#[cfg(any(
    test,
    all(target_os = "linux", any(feature = "cef", feature = "helper"))
))]
mod cpu_frames;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(all(any(windows, target_os = "linux"), feature = "helper"))]
mod helper;
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "cef")))]
mod link;
mod page;
mod page_api;
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
mod permission;
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
mod reload_hold;
mod request;
#[cfg(any(
    test,
    all(
        any(windows, target_os = "linux"),
        any(feature = "cef", feature = "helper")
    )
))]
mod wire;

use std::sync::Arc;

pub use audio::{MAX_CHUNK_FRAMES, PageAudio, PageAudioStream};
#[cfg(all(any(windows, target_os = "linux"), feature = "cef"))]
pub use cef::{CefBackend, CefConfig};
#[cfg(all(any(windows, target_os = "linux"), feature = "helper"))]
pub use helper::run_helper;
pub use page::{
    DEFAULT_CSS, HTML_PAGE_DOMAIN, MAX_CSS_BYTES, MAX_FPS, MAX_HTML_BYTES, PageLocation, PageSpec,
    css_script,
};
pub use page_api::{MAX_PAGE_DATA_BYTES, PageApi, PageData};
pub use request::{RequestPolicy, Verdict};

use jstream_media::VideoFrame;
use jstream_platform::{GpuContext, GpuFrame};

/// Opens off-screen pages. One per process: the CEF backend runs one browser helper, which
/// stops when the backend is dropped, after every page it opened has closed.
///
/// Shared (`Arc<dyn BrowserBackend>`) and called from the engine's sources thread, so it
/// is `Send + Sync`.
pub trait BrowserBackend: Send + Sync {
    /// Opens `spec` off screen. The page loads in the background: the call returns once
    /// the browser is asked for, and frames and audio follow when the page paints and
    /// plays. Dropping the returned page closes it.
    ///
    /// # Errors
    /// [`BrowserError::Start`] when the browser engine can't start (the helper or the
    /// CEF runtime is missing), and [`BrowserError::Open`] when it refuses the page or
    /// has stopped.
    fn open(&self, spec: &PageSpec) -> Result<Box<dyn BrowserPage>, BrowserError>;

    /// The GPU device pages deliver their frames on: one on the media adapter, so the
    /// compositor can sample them where they are (ADR-0004). The host gives one it opens,
    /// for paints before a program stage; while a stage runs the engine replaces it with
    /// the stage's own context, and passes `None` when the stage stops. With `None`
    /// (the default) pages keep painting but their frames are dropped. Pages open before
    /// the call follow it from their next paint.
    fn use_device(&self, gpu: Option<&GpuContext>);
}

/// One open page. Dropping it closes the browser.
pub trait BrowserPage: Send {
    /// The page's latest frame, or `None` before its first paint and while the backend
    /// has no device.
    ///
    /// The texture is BGRA8 with **premultiplied** alpha (CEF's own layout), at the
    /// page's size. It is the page's own copy on the engine's device, so it stays valid
    /// after CEF reuses its texture; a frame held across later paints keeps showing the
    /// page as it was, since each paint writes another texture of a small ring.
    fn frame(&self) -> Option<GpuFrame>;

    /// Whether the page's frames arrive in CPU memory ([`cpu_frame`](Self::cpu_frame))
    /// instead of on the GPU ([`frame`](Self::frame)): the CEF backend on Linux, whose
    /// helper paints in software (ADR-0095). The same for as long as the page is open.
    /// `false`, the default, for a backend that paints on the GPU.
    fn paints_on_cpu(&self) -> bool {
        false
    }

    /// The page's latest frame in CPU memory, for a page that
    /// [`paints_on_cpu`](Self::paints_on_cpu): straight-alpha sRGB RGBA8 at the page's
    /// size, or `None` before its first paint. `None`, the default, for a backend that
    /// paints on the GPU.
    fn cpu_frame(&self) -> Option<Arc<VideoFrame>> {
        None
    }

    /// Replaces the page's custom CSS ([`css_script`]): it applies now if the page has
    /// loaded, and on every load after.
    fn set_css(&mut self, css: &str);

    /// Loads the page again from its location, as if it had just opened: its scripts and
    /// CSS animations start over. It is transparent from the reload until the new
    /// document paints, never showing the end of the old one.
    fn reload(&mut self);

    /// Tells the page whether anything draws it (#1071). A hidden page stops painting
    /// and its animation frames stop (CEF's `WasHidden`), so it costs next to nothing
    /// while no mix shows it; its scripts, timers and audio keep running, throttled as
    /// Chromium throttles a background tab. Shown again, it paints at once, and its
    /// frames keep showing its last paint until then. Pages open shown.
    fn set_hidden(&mut self, hidden: bool);

    /// The page's audio, which the engine records into a mixer input with
    /// [`PageAudio::open`]. It is silent until the page plays something.
    fn audio(&self) -> &Arc<PageAudio>;

    /// How many of the page's requests were refused under its [`RequestPolicy`]: a
    /// file page reaching outside its folder, a web page reaching for a local file, or
    /// a scheme no page may load.
    fn denied_requests(&self) -> u64;

    /// Sends the page an update to its `window.jstream` (ADR-0102), split as
    /// [`PageData::split`] does. Nothing for a page opened without a
    /// [`page_api`](PageSpec::page_api), nor for a backend without pages of its own
    /// (the default).
    fn send_data(&mut self, data: PageData) {
        let _ = data;
    }
}

/// Why a page could not be opened.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BrowserError {
    /// The browser engine could not start. Every later [`BrowserBackend::open`] fails
    /// the same way.
    #[error("the browser engine could not start: {reason}")]
    Start {
        /// What failed, in words.
        reason: String,
    },
    /// The engine is running but refused this page.
    #[error("the browser could not open the page: {reason}")]
    Open {
        /// What failed, in words.
        reason: String,
    },
    /// The page's URL is not one a browser source may load.
    #[error("{reason}")]
    Url {
        /// What is wrong with it.
        reason: String,
    },
}
