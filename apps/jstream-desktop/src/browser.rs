//! Browser sources in the app (ADR-0021, ADR-0046, ADR-0048, ADR-0157): the CEF backend
//! the engine opens `browser` sources' pages in, through the sandboxed browser helper, on
//! the engine's media clock, with pages' frames on a GPU device of the media adapter on
//! Windows and in CPU memory on macOS. Windows and macOS with the `cef` feature only; on
//! macOS the helper is in the app bundle's `Contents/MacOS`, beside the app.
//!
//! Docs: apps/jstream-desktop/README.md#design, docs/adr/0021-browser-source.md, docs/adr/0046-browser-crate-and-helper.md, docs/adr/0048-sandboxed-browser-helper.md, docs/adr/0157-browser-sources-on-macos.md

use jstream_browser::{BrowserBackend, CefBackend, CefConfig};
use jstream_platform::MediaClock;
use std::path::Path;
use std::sync::Arc;

/// The browser backend for an engine on `clock`, with CEF's cache under `data` (`cef`)
/// and its log in `data`'s `logs` folder (`cef.log`). The browser helper starts when the
/// first page opens, so a missing helper or missing CEF files fail that source, not the
/// app, which doesn't link CEF.
///
/// `None`, logged, when the helper's folder (the exe's) is unknown: the engine then runs
/// without one and `browser` sources draw nothing. Without the engine's GPU context pages
/// still play their audio, but deliver no frames.
pub fn backend(data: &Path, clock: Arc<dyn MediaClock>) -> Option<Arc<dyn BrowserBackend>> {
    let mut config = match CefConfig::beside_exe(data) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "browser sources are off");
            return None;
        }
    };
    config.log_file = Some(data.join("logs").join("cef.log"));
    // The engine hands it its GPU context, on the media adapter (ADR-0004, ADR-0117 §5),
    // when the host gives it to the engine (`Engine::with_browser`).
    let backend = CefBackend::new(config, clock);
    Some(Arc::new(backend))
}
