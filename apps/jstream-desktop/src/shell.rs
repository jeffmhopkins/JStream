//! The window: a Tauri webview showing the UI from the app's own scheme, with the
//! bootstrap script that hands it the engine, kept on the app's own pages.
//!
//! - The UI is served by [`Ui`] on [`ui::SCHEME`], read from disk at each request.
//! - Navigation is allowed only to pages on that scheme, so no other page is ever
//!   loaded in a webview that runs the bootstrap script. A new window (`window.open`, a
//!   link with a target) opens only for one of the UI's own projector pages
//!   ([`projector`]): a window of the app's own, under the same rules and with the same
//!   script, full screen when the page asks for it. Any other is refused.
//! - The page gets no Tauri IPC commands: the app defines none and grants no
//!   capabilities, so the UI talks to the engine only over the WebSocket API.
//! - When the app exits, the host stops: the server closes its connections and the
//!   engine stops.
//!
//! Docs: apps/jstream-desktop/README.md#design

use crate::host::Host;
use crate::ui::{self, Ui};
use anyhow::Context;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::http::{Response, StatusCode};
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, Wry};

/// The window's label.
const MAIN: &str = "main";

/// The query key of the UI's projector pages (`jstream-ui`'s `projector.rs`).
const PROJECTOR: &str = "projector";

/// The URL the window opens: the UI's index on the app's scheme.
fn start_url() -> anyhow::Result<Url> {
    let url = if cfg!(windows) {
        format!("http://{}.localhost/", ui::SCHEME)
    } else {
        format!("{}://localhost/", ui::SCHEME)
    };
    Url::parse(&url).context("the start URL")
}

/// Whether `url` is one of the app's own pages, the only ones the window may show.
fn is_app_page(url: &Url) -> bool {
    match start_url() {
        Ok(start) => {
            url.scheme() == start.scheme()
                && url.host_str() == start.host_str()
                && url.port_or_known_default() == start.port_or_known_default()
        }
        Err(_) => false,
    }
}

/// What a new window for `url` is: `None` when it isn't one of the UI's projector
/// pages, and refused; otherwise whether the page asks to fill the screen.
fn projector(url: &Url) -> Option<bool> {
    if !is_app_page(url) {
        return None;
    }
    let mut pairs = url.query_pairs();
    pairs
        .clone()
        .any(|(key, _)| key == PROJECTOR)
        .then(|| pairs.any(|(key, value)| key == "full" && value == "1"))
}

/// Whether a webview may navigate to `url`: the UI's own pages, and, for a projector
/// window, the blank page it starts on before the page it was opened for.
fn allowed(url: &Url, blank: bool) -> bool {
    let allowed = is_app_page(url) || (blank && url.as_str() == "about:blank");
    if !allowed {
        tracing::warn!(scheme = url.scheme(), "navigation away from the UI refused");
    }
    allowed
}

/// Opens the projector window `window.open` asked for (`url`, `features`), or refuses
/// it. It starts blank, as a window handed to the opener must, then shows `url`.
fn open_projector(
    app: &AppHandle<Wry>,
    script: Option<&str>,
    url: &Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<Wry> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let Some(full) = projector(url) else {
        tracing::warn!(scheme = url.scheme(), "a new window refused");
        return NewWindowResponse::Deny;
    };
    let label = format!("projector-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let Ok(blank) = Url::parse("about:blank") else {
        return NewWindowResponse::Deny;
    };
    let mut window = WebviewWindowBuilder::new(app, label, WebviewUrl::External(blank))
        .window_features(features)
        .title("JStream projector")
        .min_inner_size(160.0, 90.0)
        .fullscreen(full)
        .on_navigation(|url| allowed(url, true))
        .on_new_window(|_url, _features| NewWindowResponse::Deny)
        .on_document_title_changed(|window, title| {
            let _set = window.set_title(&title);
        });
    if let Some(script) = script {
        window = window.initialization_script(script);
    }
    match window.build() {
        Ok(window) => NewWindowResponse::Create { window },
        Err(error) => {
            tracing::warn!(%error, "could not open a projector window");
            NewWindowResponse::Deny
        }
    }
}

/// Runs the app until its window closes: `ui` serves the pages, `script` (if the
/// engine started) hands them the engine, and `host` is stopped on exit.
pub fn run(ui: Ui, script: Option<String>, host: Option<Host>) -> anyhow::Result<()> {
    let start = start_url()?;
    let app = tauri::Builder::default()
        .register_uri_scheme_protocol(ui::SCHEME, move |_context, request| {
            let reply = ui.respond(
                request.method().as_str(),
                request.uri().path(),
                ui::new_nonce,
            );
            let mut response = Response::builder()
                .status(StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK));
            for (name, value) in reply.headers {
                response = response.header(name, value);
            }
            response.body(reply.body).unwrap_or_else(|error| {
                tracing::warn!(%error, "could not build a UI response");
                let mut empty = Response::new(Vec::new());
                *empty.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                empty
            })
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let projector_script = script.clone();
            let mut window =
                WebviewWindowBuilder::new(app, MAIN, WebviewUrl::CustomProtocol(start))
                    .title("JStream")
                    .inner_size(1280.0, 800.0)
                    .min_inner_size(640.0, 400.0)
                    .on_navigation(|url| allowed(url, false))
                    .on_new_window(move |url, features| {
                        open_projector(&handle, projector_script.as_deref(), &url, features)
                    });
            if let Some(script) = &script {
                window = window.initialization_script(script);
            }
            window.build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("could not create the window (is WebView2 installed?)")?;
    let mut host = host;
    app.run(move |_app, event| {
        if let RunEvent::Exit = event
            && let Some(host) = host.take()
        {
            host.stop();
        }
    });
    Ok(())
}
