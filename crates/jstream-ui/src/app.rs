//! The root component: the connect form until a session is open, then the shell (the
//! header over the workspace), and the event loop that keeps the shared [`Studio`] state
//! current. Which panel sits where is the workspace's dock (`dock.rs`, `workspace.rs`).
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::header::Header;
use crate::menu::{ContextMenu, Menus};
use crate::panels::AddSource;
use crate::panels::add_source::AddSourceDialog;
use crate::panels::source_properties::{PropertiesDialog, SourceProperties};
use crate::session::{Session, engine_url_for_page};
use crate::store::Studio;
use crate::workspace::{DockState, Workspace, restore};
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
    let studio = Studio::new();
    provide_context(studio);
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
                    studio.reset();
                    studio.load(&opened);
                    // The context holds the only lasting handle: dropping it on Disconnect
                    // drops the connection, which closes the socket and ends `events`.
                    session.0.set_value(Some(opened));
                    status.set(Status::Connected);
                    while let Some(event) = events.next().await {
                        if generation.get_value() != mine {
                            return;
                        }
                        if let Event::StateChanged { rev, .. } = &event {
                            info.update(|info| info.rev = rev.to_string());
                        }
                        if let Some(current) = session.get() {
                            studio.on_event(&current, &event);
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
    // The fields are in no form element: browsers offer to save a password when a form
    // with a password field is submitted. A click or Enter connects instead.
    let submit = move || connect(url.get_untracked(), token.get_untracked());
    let problem = move || match status.get() {
        Status::Disconnected(Some(reason)) => Some(reason),
        _ => None,
    };
    view! {
        <main class="connect">
            <h1>"JStream"</h1>
            <div
                class="fields"
                on:keydown=move |event| {
                    if event.key() == "Enter" && !busy() {
                        event.prevent_default();
                        submit();
                    }
                }
            >
                <label>
                    "Engine URL"
                    <input
                        type="text"
                        name="engine-url"
                        placeholder="ws://127.0.0.1:4455"
                        bind:value=url
                    />
                </label>
                <label>
                    "Control token"
                    <input
                        type="password"
                        name="control-token-entry"
                        autocomplete="off"
                        spellcheck="false"
                        placeholder="Not needed if the browser already signed in"
                        bind:value=token
                    />
                </label>
                <button type="button" class="primary" disabled=busy on:click=move |_| submit()>
                    {move || if busy() { "Connecting…" } else { "Connect" }}
                </button>
            </div>
            {move || problem().map(|reason| view! { <p class="problem" role="alert">{reason}</p> })}
        </main>
    }
}

/// The workspace: the header bar over the dock zones and the preview canvas
/// (`workspace.rs`). The dock is the browser's kept one, or the Editing layout.
#[component]
fn Shell(info: RwSignal<SessionInfo>, disconnect: impl Fn() + 'static) -> impl IntoView {
    let studio = expect_context::<Studio>();
    provide_context(DockState(RwSignal::new(restore())));
    provide_context(Menus::new());
    provide_context(AddSourceDialog::new());
    provide_context(PropertiesDialog::new());
    view! {
        <div class="shell">
            <Header info=info disconnect=disconnect />
            {move || studio.problem.get().map(|problem| view! {
                <div class="problem-bar" role="alert">
                    <span>{problem}</span>
                    <button on:click=move |_| studio.problem.set(None)>"Dismiss"</button>
                </div>
            })}
            <Workspace />
            <AddSource />
            <SourceProperties />
            <ContextMenu />
        </div>
    }
}
