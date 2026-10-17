//! Browser sources in the app (ADR-0021, ADR-0046, ADR-0048): the CEF backend the engine
//! opens `browser` sources' pages in, through the sandboxed browser helper, on the engine's
//! media clock, with pages' frames on a GPU device of the media adapter. Windows with the
//! `cef` feature only.
//!
//! Docs: apps/jstream-desktop/README.md#design, docs/adr/0021-browser-source.md, docs/adr/0046-browser-crate-and-helper.md, docs/adr/0048-sandboxed-browser-helper.md

use jstream_browser::{BrowserBackend, CefBackend, CefConfig};
use jstream_platform::{GpuInterop, MediaClock};
use std::path::Path;
use std::sync::Arc;

/// The browser backend for an engine on `clock`, with CEF's cache under `data` (`cef`)
/// and its log in `data`'s `logs` folder (`cef.log`). The browser helper starts when the
/// first page opens, so a missing helper or missing CEF files fail that source, not the
/// app, which doesn't link CEF.
///
/// `None`, logged, when the helper's folder (the exe's) is unknown: the engine then runs
/// without one and `browser` sources draw nothing. Without a GPU device pages still play
/// their audio, but deliver no frames.
pub fn backend(data: &Path, clock: Arc<dyn MediaClock>) -> Option<Arc<dyn BrowserBackend>> {
    let mut config = match CefConfig::beside_exe(data) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "browser sources are off");
            return None;
        }
    };
    config.log_file = Some(data.join("logs").join("cef.log"));
    let backend = CefBackend::new(config, clock);
    // The media adapter's device, as the engine's own media work opens it (ADR-0004),
    // for paints before a program stage; the stage lends its own context in its place.
    match jstream_platform_windows::WindowsGpu::new().open_device() {
        Ok(gpu) => backend.use_device(Some(&gpu)),
        Err(error) => {
            tracing::warn!(%error, "browser sources get no GPU device, so they show no frames")
        }
    }
    Some(Arc::new(backend))
}
