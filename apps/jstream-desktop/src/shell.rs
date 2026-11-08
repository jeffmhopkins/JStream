//! The window: a Tauri webview showing the UI from the app's own scheme, with the
//! bootstrap script that hands it the engine, kept on the app's own pages.
//!
//! - The UI is served by [`Ui`] on [`ui::SCHEME`], read from disk at each request.
//! - Navigation is allowed only to pages on that scheme, so no other page is ever
//!   loaded in a webview that runs the bootstrap script. A new window (`window.open`, a
//!   link with a target) opens only for one of the UI's own projector pages
//!   ([`projector`]): a window of the app's own, under the same rules and with the same
//!   script, full screen when the page asks for it. A link to one of the project's own
//!   pages on GitHub, or Cisco's OpenH264 licence ([`links`]), opens in the
//!   system browser instead. Any other is refused.
//! - The page gets one Tauri command, `pick_path` ([`crate::pick`]), and no other.
//!   The UI calls it through the bootstrap global's `browse`, and talks to the engine
//!   only over the WebSocket API. The scheme's one other answer is the update check.
//! - When the app exits, the host stops: the server closes its connections and the
//!   engine stops.
//!
//! Docs: apps/jstream-desktop/README.md#design

use crate::host::Host;
use crate::links;
use crate::ui::{self, Ui};
use crate::updates;
use anyhow::Context;
use std::path::PathBuf;
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

/// Opens `url`, one [`links::opens_in_browser`] accepts, in the system browser:
/// Explorer hands an `https` URL to the default browser on Windows, and `xdg-open` (the
/// desktop's handler, found on `PATH`) on Linux. The URL is one argument, never parsed
/// by a shell.
fn open_in_browser(url: &Url) {
    #[cfg(windows)]
    let opener = std::env::var_os("SystemRoot")
        .map(|root| std::path::Path::new(&root).join("explorer.exe"))
        .unwrap_or_else(|| "explorer.exe".into());
    #[cfg(not(windows))]
    let opener = std::path::PathBuf::from("xdg-open");
    if let Err(error) = std::process::Command::new(opener).arg(url.as_str()).spawn() {
        tracing::warn!(%error, "could not open a link in the browser");
    }
}

/// Opens the projector window `window.open` asked for (`url`, `features`), opens a link
/// to one of the project's pages in the system browser, or refuses it. A projector
/// starts blank, as a window handed to the opener must, then shows `url`.
fn open_projector(
    app: &AppHandle<Wry>,
    script: Option<&str>,
    url: &Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<Wry> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    if url.scheme() == "https" && links::opens_in_browser(url.as_str()) {
        open_in_browser(url);
        return NewWindowResponse::Deny;
    }
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

/// A scheme reply as the response the webview takes. A builder failure is an empty 500.
fn http_response(reply: ui::Reply) -> Response<Vec<u8>> {
    let mut response =
        Response::builder().status(StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK));
    for (name, value) in reply.headers {
        response = response.header(name, value);
    }
    response.body(reply.body).unwrap_or_else(|error| {
        tracing::warn!(%error, "could not build a UI response");
        let mut empty = Response::new(Vec::new());
        *empty.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        empty
    })
}

/// Runs the app until its window closes: `ui` serves the pages, `script` (if the
/// engine started) hands them the engine, `host` is stopped on exit, and
/// `network_file` is where `set_network` saves "Allow devices on my network".
pub fn run(
    ui: Ui,
    script: Option<String>,
    host: Option<Host>,
    network_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    let start = start_url()?;
    let mut builder = tauri::Builder::default();
    if let Some(file) = network_file {
        builder = builder.manage(crate::network::Setting(file));
    }
    let app = builder
        .invoke_handler(tauri::generate_handler![
            crate::pick::pick_path,
            crate::network::set_network
        ])
        // Asynchronous so the update check's request to GitHub doesn't block the
        // window. File requests are answered on this call, as they were before.
        .register_asynchronous_uri_scheme_protocol(
            ui::SCHEME,
            move |_context, request, responder| {
                let method = request.method().as_str().to_owned();
                let path = request.uri().path().to_owned();
                if updates::handles(&method, &path) {
                    std::thread::spawn(move || responder.respond(http_response(updates::reply())));
                    return;
                }
                let reply = ui.respond(&method, &path, ui::new_nonce);
                responder.respond(http_response(reply));
            },
        )
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
        .context(if cfg!(windows) {
            "could not create the window (is WebView2 installed?)"
        } else {
            "could not create the window (is there a display, and WebKitGTK 4.1?)"
        })?;
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
