//! The root component: the connect form until a session is open, then the shell with its
//! regions, and the event loop that follows the engine's revision.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::canvas::Canvas;
use crate::panels::{Inspector, Mixer, SceneTree};
use crate::session::{Session, engine_url_for_page};
use futures_util::StreamExt;
use jstream_control::ControlError;
use jstream_control::protocol::Event;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Where the connection stands.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    /// No session. Carries why the last one ended or failed, if one did.
    Disconnected(Option<String>),
    /// Connecting and saying `hello`.
    Connecting,
    /// A session is open.
    Connected,
}

/// What the shell shows about the open session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionInfo {
    /// The engine URL the session connected to.
    pub url: String,
    /// The engine's protocol version.
    pub protocol: String,
    /// The client ID the session acts as.
    pub client_id: String,
    /// The last state revision the UI has heard of: `welcome`'s, then each
    /// `state.changed`'s.
    pub rev: String,
}

/// The session the panels work through, provided as context while one is open. `None`
/// otherwise.
#[derive(Clone, Copy)]
pub struct SessionContext(pub StoredValue<Option<Session>, LocalStorage>);

impl SessionContext {
    /// The open session, if there is one.
    pub fn get(&self) -> Option<Session> {
        self.0.get_value()
    }
}

/// A failed connect or `hello`, for the connect form. `Disconnected`'s own message says
/// the connection was "lost", which reads oddly for one that never opened.
fn describe(error: &ControlError) -> String {
    match error {
        ControlError::Disconnected { reason } => reason.clone(),
        other => other.to_string(),
    }
}

/// The engine URL to offer: the page's own host, when the page was served over HTTP.
fn default_engine_url() -> String {
    let location = window().location();
    let protocol = location.protocol().unwrap_or_default();
    let host = location.host().unwrap_or_default();
    engine_url_for_page(&protocol, &host).unwrap_or_default()
}

/// The app: a connect form until a session is open, then the shell.
#[component]
pub fn App() -> impl IntoView {
    let status = RwSignal::new(Status::Disconnected(None));
    let info = RwSignal::new(SessionInfo::default());
    let session = SessionContext(StoredValue::new_local(None));
    provide_context(session);
    // Bumped each time a session ends, so a stale event loop can tell it is stale.
    let generation = StoredValue::new(0_u64);

    let end_session = move |reason: Option<String>| {
        generation.update_value(|g| *g += 1);
        session.0.set_value(None);
        status.set(Status::Disconnected(reason));
    };

    let connect = move |url: String, token: String| {
        status.set(Status::Connecting);
        spawn_local(async move {
            match Session::connect(&url, &token).await {
                Ok(opened) => {
                    let mine = generation.get_value();
                    info.set(SessionInfo {
                        url,
                        protocol: opened.welcome.protocol.to_string(),
                        client_id: opened.welcome.client_id.to_string(),
                        rev: opened.welcome.rev.to_string(),
                    });
                    let mut events = opened.control.events();
                    session.0.set_value(Some(opened));
                    status.set(Status::Connected);
                    while let Some(event) = events.next().await {
                        if generation.get_value() != mine {
                            return;
                        }
                        if let Event::StateChanged { rev, .. } = event {
                            info.update(|info| info.rev = rev.to_string());
                        }
                    }
                    if generation.get_value() == mine {
                        end_session(Some("the connection to the engine closed".into()));
                    }
                }
                Err(error) => status.set(Status::Disconnected(Some(describe(&error)))),
            }
        });
    };

    // In the desktop app, connect to its engine at once (`host.rs`).
    if let Some(engine) = crate::host::desktop() {
        connect(engine.url, engine.token);
    }

    view! {
        <Show
            when=move || status.get() == Status::Connected
            fallback=move || view! { <ConnectForm status=status connect=connect /> }
        >
            <Shell info=info disconnect=move || end_session(None) />
        </Show>
    }
}

/// The form that opens a session: the engine URL and the control token. The token stays
/// in memory: it is never stored by the page or written to the console. Left empty, the
/// connection relies on the credentials the browser already sends to the engine.
#[component]
fn ConnectForm(
    status: RwSignal<Status>,
    connect: impl Fn(String, String) + Copy + 'static,
) -> impl IntoView {
    let url = RwSignal::new(default_engine_url());
    let token = RwSignal::new(String::new());
    let busy = move || status.get() == Status::Connecting;
    let problem = move || match status.get() {
        Status::Disconnected(Some(reason)) => Some(reason),
        _ => None,
    };
    view! {
        <main class="connect">
            <h1>"JStream"</h1>
            <form on:submit=move |event| {
                event.prevent_default();
                connect(url.get_untracked(), token.get_untracked());
            }>
                <label>
                    "Engine URL"
                    <input type="text" placeholder="ws://127.0.0.1:4455" bind:value=url />
                </label>
                <label>
                    "Control token"
                    <input
                        type="password"
                        autocomplete="off"
                        placeholder="Not needed if the browser already signed in"
                        bind:value=token
                    />
                </label>
                <button type="submit" disabled=busy>
                    {move || if busy() { "Connecting…" } else { "Connect" }}
                </button>
            </form>
            {move || problem().map(|reason| view! { <p class="problem" role="alert">{reason}</p> })}
        </main>
    }
}

/// The workspace: a status bar over the scene tree, the preview canvas and the
/// inspector, with the mixer along the bottom. Each region is its own component, so the
/// panels and the canvas are built separately.
#[component]
fn Shell(info: RwSignal<SessionInfo>, disconnect: impl Fn() + 'static) -> impl IntoView {
    view! {
        <div class="shell">
            <header class="status-bar">
                <span class="brand">"JStream"</span>
                <span>{move || info.with(|i| i.url.clone())}</span>
                <span>"protocol " {move || info.with(|i| i.protocol.clone())}</span>
                <span>"client " {move || info.with(|i| i.client_id.clone())}</span>
                <span>"rev " {move || info.with(|i| i.rev.clone())}</span>
                <button on:click=move |_| disconnect()>"Disconnect"</button>
            </header>
            <aside class="region scenes"><SceneTree /></aside>
            <section class="region canvas"><Canvas /></section>
            <aside class="region inspector"><Inspector /></aside>
            <footer class="region mixer"><Mixer /></footer>
        </div>
    }
}
