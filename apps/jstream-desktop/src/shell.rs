//! The window: a Tauri webview showing the UI from the app's own scheme, with the
//! bootstrap script that hands it the engine, kept on the app's own pages.
//!
//! - The UI is served by [`Ui`] on [`ui::SCHEME`], read from disk at each request.
//! - Navigation is allowed only to pages on that scheme, and new windows (`window.open`,
//!   a link with a target) are refused, so no other page is ever loaded in a webview
//!   that runs the bootstrap script.
//! - The page gets no Tauri IPC commands: the app defines none and grants no
//!   capabilities, so the UI talks to the engine only over the WebSocket API.
//! - When the app exits, the host stops: the server closes its connections and the
//!   engine stops.
//!
//! Docs: apps/jstream-desktop/README.md#design

use crate::host::Host;
use crate::ui::{self, Ui};
use anyhow::Context;
use tauri::http::{Response, StatusCode};
use tauri::webview::NewWindowResponse;
use tauri::{RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

/// The window's label.
const MAIN: &str = "main";

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
            let mut window =
                WebviewWindowBuilder::new(app, MAIN, WebviewUrl::CustomProtocol(start))
                    .title("JStream")
                    .inner_size(1280.0, 800.0)
                    .min_inner_size(640.0, 400.0)
                    .on_navigation(|url| {
                        let allowed = is_app_page(url);
                        if !allowed {
                            tracing::warn!(
                                scheme = url.scheme(),
                                "navigation away from the UI refused"
                            );
                        }
                        allowed
                    })
                    .on_new_window(|_url, _features| NewWindowResponse::Deny);
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
