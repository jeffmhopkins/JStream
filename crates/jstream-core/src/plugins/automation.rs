//! WASM automation plugins (ADR-0069, ADR-0075), run on `jstream-plugin-wasm`'s host.
//!
//! `plugin.install` loads a package's module and `plugin.enable` loads it again after
//! the engine disabled it; `plugin.grant` replaces what it may do. Each running plugin has
//! a thread of its own (`jstream-plugin`), which loads the module and then hands it the
//! engine's events one at a time, and, when the plugin exports [`EVENT_ENTRY`], a second
//! thread (`jstream-plugin-events`) that moves the events of its subscription into the
//! host's capped queue. A plugin's calls go through a [`PluginControl`]: a session of its
//! own that checks each call against the plugin's grant and answers `forbidden` for one
//! outside it. A plugin that traps or runs past a limit is disabled and its instance
//! dropped, with a warning in the log and `plugin.state_changed`; only a person re-enables
//! it.
//!
//! The installed plugins last for the run: nothing here is saved.

use crate::engine::{Engine, Executed, WeakEngine};
use crate::events::{self, QueueRef, Subscription};
use crate::lock;
use crate::preview::SessionPreviews;
use futures_core::Stream;
use jstream_control::{BoxFuture, Control, ControlError, EventStream};
use jstream_plugin_wasm::{
    Capabilities, Error as PluginError, EventSender, Limits, Plugin, PluginConfig, PluginHost,
    Refusals,
};
use jstream_protocol::{
    Action, Call, CallResult, ClientId, EntityKind, Event, Hello, PluginDisabledReason,
    PluginGrant, PluginId, PluginState, ProtocolError, RequestId, Response, Welcome,
};
use std::collections::BTreeMap;
use std::future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// The export a plugin's events are handed to, one per invocation, with the event as JSON
/// for its input (ABI version 0). A plugin that doesn't export it gets no events.
pub(crate) const EVENT_ENTRY: &str = "on_event";

/// The module in a plugin's package.
pub(crate) const MODULE_FILE: &str = "plugin.wasm";

/// The manifest in a plugin's package, which may be left out.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";

/// The installed plugins, by ID.
#[derive(Default)]
pub(crate) struct Registry {
    /// The wasmtime host, made by the first install: its ticker thread runs from then on.
    host: Mutex<Option<PluginHost>>,
    plugins: Mutex<BTreeMap<PluginId, Entry>>,
    /// Numbers each run of a plugin, so a run that ends late can't touch a newer one.
    runs: AtomicU64,
    /// For tests: how long [`start`] waits between a plugin's load and marking its entry
    /// `Running`, to widen the window in which events queue for it (#839 review B1). Per
    /// engine, so other tests' installs don't wait too.
    #[cfg(test)]
    pub(crate) running_delay_ms: AtomicU64,
}

struct Entry {
    /// The package directory, read again by each `plugin.enable`.
    package: PathBuf,
    /// What the plugin may do, shared with the [`PluginControl`] of its running instance,
    /// so `plugin.grant` applies to its next call.
    grant: Arc<Mutex<PluginGrant>>,
    /// The name its manifest gives itself, the plugin's own text (`remote_name`).
    name: Option<String>,
    state: Run,
}

enum Run {
    /// Being loaded by an install or an enable, which waits for it with no lock held.
    Starting,
    Running(Running),
    Disabled(PluginDisabledReason),
}

/// A running plugin's handles. Dropping them ends its threads: the work channel's last
/// sender goes with the event thread, which ends when the subscription is closed.
struct Running {
    run: u64,
    /// Kept so the thread's `recv` ends once both this and the event thread let go.
    _work: SyncSender<()>,
    /// The plugin's event subscription, closed when it is disabled.
    events: Option<QueueRef>,
}

/// A package, read from disk: the module's bytes and the manifest's name.
struct Package {
    wasm: Vec<u8>,
    name: Option<String>,
}

/// Answers a plugin action, or `None` for any other action.
pub(crate) fn action(
    engine: &Engine,
    action: &Action,
) -> Option<Result<CallResult, ProtocolError>> {
    let result = match action {
        Action::PluginInstall {
            plugin_id,
            path,
            grant,
        } => install(engine, plugin_id, path, grant.clone()),
        Action::PluginGrant { plugin_id, grant } => set_grant(engine, plugin_id, grant.clone()),
        Action::PluginEnable { plugin_id } => enable(engine, plugin_id),
        _ => return None,
    };
    Some(result.map(|()| CallResult::action()))
}

fn install(
    engine: &Engine,
    plugin_id: &PluginId,
    path: &str,
    grant: PluginGrant,
) -> Result<(), ProtocolError> {
    check_grant(&grant)?;
    let package = PathBuf::from(path);
    let read = read_package(&package)?;
    let registry = engine.automation();
    {
        let mut plugins = lock(&registry.plugins);
        if plugins.contains_key(plugin_id) {
            return Err(ProtocolError::AlreadyExists {
                entity: EntityKind::Plugin,
                id: plugin_id.to_string(),
            });
        }
        plugins.insert(
            plugin_id.clone(),
            Entry {
                package,
                grant: Arc::new(Mutex::new(grant)),
                name: read.name.clone(),
                state: Run::Starting,
            },
        );
    }
    start(engine, plugin_id, read.wasm).inspect_err(|_| {
        lock(&registry.plugins).remove(plugin_id);
    })
}

fn set_grant(
    engine: &Engine,
    plugin_id: &PluginId,
    grant: PluginGrant,
) -> Result<(), ProtocolError> {
    check_grant(&grant)?;
    let plugins = lock(&engine.automation().plugins);
    let entry = plugins.get(plugin_id).ok_or_else(|| not_found(plugin_id))?;
    *lock(&entry.grant) = grant;
    Ok(())
}

fn enable(engine: &Engine, plugin_id: &PluginId) -> Result<(), ProtocolError> {
    let registry = engine.automation();
    let (package, was) = {
        let mut plugins = lock(&registry.plugins);
        let entry = plugins
            .get_mut(plugin_id)
            .ok_or_else(|| not_found(plugin_id))?;
        let Run::Disabled(reason) = entry.state else {
            // Running, or being started by another call: already enabled.
            return Ok(());
        };
        entry.state = Run::Starting;
        (entry.package.clone(), reason)
    };
    let started = read_package(&package)
        .map_err(|error| ProtocolError::ActionFailed {
            reason: error.to_string(),
        })
        .and_then(|read| {
            if let Some(entry) = lock(&registry.plugins).get_mut(plugin_id) {
                entry.name = read.name;
            }
            start(engine, plugin_id, read.wasm)
        });
    match started {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(entry) = lock(&registry.plugins).get_mut(plugin_id) {
                entry.state = Run::Disabled(was);
            }
            Err(match error {
                ProtocolError::InvalidArgument { reason, .. } => {
                    ProtocolError::ActionFailed { reason }
                }
                other => other,
            })
        }
    }
}

fn enabled(engine: &Engine, plugin_id: &PluginId, remote_name: Option<String>) {
    tracing::info!(plugin_id = %plugin_id, "plugin enabled");
    engine.emit(&Event::PluginStateChanged {
        plugin_id: plugin_id.clone(),
        state: PluginState::Enabled,
        reason: None,
        remote_name,
    });
}

/// `invalid_argument` for a grant whose hosts or folders aren't concrete, or that names a
/// folder this OS doesn't read as absolute.
fn check_grant(grant: &PluginGrant) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = grant.problem() {
        return Err(invalid(format!("grant.{field}"), reason));
    }
    for (i, folder) in grant.folders.iter().enumerate() {
        if !Path::new(folder).is_absolute() {
            return Err(invalid(
                format!("grant.folders.{i}"),
                "must be an absolute path on the engine's machine".into(),
            ));
        }
    }
    Ok(())
}

/// Reads a package: a directory with [`MODULE_FILE`] and, optionally, [`MANIFEST_FILE`].
fn read_package(package: &Path) -> Result<Package, ProtocolError> {
    let path = |reason: String| invalid("path".into(), reason);
    if !package.is_absolute() {
        return Err(path(
            "must be an absolute path on the engine's machine".into(),
        ));
    }
    let wasm = std::fs::read(package.join(MODULE_FILE)).map_err(|e| {
        path(format!(
            "isn't a plugin package: no readable {MODULE_FILE}: {e}"
        ))
    })?;
    let name = match std::fs::read(package.join(MANIFEST_FILE)) {
        // A JSON object; this version reads only its `name`, a string. Fields it doesn't
        // know are ignored, so a later version can add the classes a plugin asks for.
        Ok(bytes) => {
            let manifest: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| path(format!("its {MANIFEST_FILE} isn't valid JSON: {e}")))?;
            let serde_json::Value::Object(fields) = manifest else {
                return Err(path(format!("its {MANIFEST_FILE} isn't a JSON object")));
            };
            match fields.get("name") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(name)) => Some(name.clone()),
                Some(_) => {
                    return Err(path(format!("its {MANIFEST_FILE}'s `name` isn't a string")));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(path(format!("its {MANIFEST_FILE} can't be read: {e}"))),
    };
    Ok(Package { wasm, name })
}

/// Loads `wasm` on a new thread for `plugin_id`, whose entry is `Starting`, and waits for
/// the load. On success the entry is `Running` and `plugin.state_changed` (`enabled`) has
/// gone out, and only then is the plugin handed its first event: so a plugin that faults
/// on it is disabled after it was enabled, never while it is still `Starting`.
fn start(engine: &Engine, plugin_id: &PluginId, wasm: Vec<u8>) -> Result<(), ProtocolError> {
    let registry = engine.automation();
    let host = {
        let mut host = lock(&registry.host);
        match &*host {
            Some(host) => host.clone(),
            None => {
                let made = PluginHost::new().map_err(|e| failed(&e))?;
                *host = Some(made.clone());
                made
            }
        }
    };
    let (grant, name) = {
        let plugins = lock(&registry.plugins);
        let entry = plugins.get(plugin_id).ok_or_else(|| not_found(plugin_id))?;
        (Arc::clone(&entry.grant), entry.name.clone())
    };
    let run = registry.runs.fetch_add(1, Ordering::Relaxed);
    let (work, inbox) = mpsc::sync_channel(1);
    let (loaded_tx, loaded) = mpsc::channel();
    let (go, wait) = mpsc::channel();
    let thread = Instance {
        engine: engine.downgrade(),
        plugin_id: plugin_id.clone(),
        run,
    };
    let channels = Channels {
        nudge: work.clone(),
        inbox,
        loaded: loaded_tx,
        go: wait,
    };
    thread::Builder::new()
        .name("jstream-plugin".into())
        .spawn(move || thread.run(&host, &wasm, grant, channels))
        .map_err(|e| failed(&e))?;
    // The thread answers once the module has loaded (or failed to), within its load
    // deadline; if it panics first, the sender drops and this reports it.
    let events = loaded.recv().unwrap_or_else(|_| {
        Err(ProtocolError::Internal {
            reason: "the plugin's thread ended while it loaded".into(),
        })
    })?;
    #[cfg(test)]
    thread::sleep(std::time::Duration::from_millis(
        registry.running_delay_ms.load(Ordering::Relaxed),
    ));
    if let Some(entry) = lock(&registry.plugins).get_mut(plugin_id) {
        entry.state = Run::Running(Running {
            run,
            _work: work,
            events,
        });
    }
    enabled(engine, plugin_id, name);
    // Its thread starts delivering events now. If it has gone, its run ends as a fault
    // would have ended it.
    let _ = go.send(());
    Ok(())
}

/// One run of a plugin, from its load until it is disabled or the engine ends.
struct Instance {
    engine: WeakEngine,
    plugin_id: PluginId,
    run: u64,
}

/// How a run's thread hears from the engine.
struct Channels {
    /// The sender of `inbox`, for the event thread, if there is one.
    nudge: SyncSender<()>,
    /// One message for each batch of queued events.
    inbox: Receiver<()>,
    /// Where the load's outcome goes: the subscription to keep, or why it failed.
    loaded: mpsc::Sender<Result<Option<QueueRef>, ProtocolError>>,
    /// A message once the entry is `Running`, before which no event is handed over.
    go: Receiver<()>,
}

impl Instance {
    fn run(
        &self,
        host: &PluginHost,
        wasm: &[u8],
        grant: Arc<Mutex<PluginGrant>>,
        channels: Channels,
    ) {
        let Channels {
            nudge,
            inbox,
            loaded,
            go,
        } = channels;
        let Some(engine) = self.engine.upgrade() else {
            return;
        };
        let control = PluginControl::open(&engine, &self.plugin_id, grant);
        drop(engine);
        let config = PluginConfig {
            capabilities: Capabilities::every(),
            limits: Limits::default(),
            refusals: Refusals::Forbidden,
        };
        let mut plugin = match Plugin::load(host, wasm, config, Arc::new(control)) {
            Ok(plugin) => plugin,
            Err(error) => {
                let _ = loaded.send(Err(load_error(&error)));
                return;
            }
        };
        // `nudge` goes to the event thread, or is dropped: this thread holds no sender of
        // its own channel, so its `recv` ends when the others let go.
        let events = if plugin.has_entry(EVENT_ENTRY) {
            match self.subscribe(plugin.events(), nudge) {
                Ok(events) => Some(events),
                Err(error) => {
                    let _ = loaded.send(Err(error));
                    return;
                }
            }
        } else {
            drop(nudge);
            None
        };
        // Events queue from the subscription on, but none is handed over until the entry
        // is `Running`: a fault on the first one must find this run to disable.
        if loaded.send(Ok(events)).is_err() || go.recv().is_err() {
            return;
        }
        // Each message is a nudge: hand over every event queued so far. Ends when the
        // engine and the event thread have both let go of the channel.
        while inbox.recv().is_ok() {
            loop {
                match plugin.deliver_event(EVENT_ENTRY) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        self.disable(&error);
                        return;
                    }
                }
            }
        }
    }

    /// Opens the plugin's event subscription and the thread that moves its events into
    /// the plugin's queue, nudging the plugin's thread for each.
    fn subscribe(
        &self,
        queue: EventSender,
        nudge: SyncSender<()>,
    ) -> Result<QueueRef, ProtocolError> {
        let engine = self.engine.upgrade().ok_or_else(ended)?;
        let (mut subscription, events) = engine.subscribe();
        drop(engine);
        thread::Builder::new()
            .name("jstream-plugin-events".into())
            .spawn(move || {
                while let Some(event) = next_blocking(&mut subscription) {
                    queue.send(event);
                    // A nudge already waiting covers this event too.
                    if let Err(TrySendError::Disconnected(())) = nudge.try_send(()) {
                        return;
                    }
                }
            })
            .map_err(|e| failed(&e))?;
        Ok(events)
    }

    /// Disables this run after `error`: the plugin's events stop, the entry says why,
    /// and `plugin.state_changed` reports it. Its instance drops when the thread returns.
    fn disable(&self, error: &PluginError) {
        let reason = match error {
            PluginError::OutOfFuel | PluginError::Deadline => PluginDisabledReason::CpuLimit,
            _ => PluginDisabledReason::Trap,
        };
        tracing::warn!(plugin_id = %self.plugin_id, reason = ?reason, %error, "plugin disabled");
        let Some(engine) = self.engine.upgrade() else {
            return;
        };
        let (events, name) = {
            let mut plugins = lock(&engine.automation().plugins);
            let Some(entry) = plugins.get_mut(&self.plugin_id) else {
                return;
            };
            match &entry.state {
                Run::Running(running) if running.run == self.run => {}
                _ => return,
            }
            let Run::Running(running) = std::mem::replace(&mut entry.state, Run::Disabled(reason))
            else {
                return;
            };
            (running.events, entry.name.clone())
        };
        events::close(events);
        engine.emit(&Event::PluginStateChanged {
            plugin_id: self.plugin_id.clone(),
            state: PluginState::Disabled,
            reason: Some(reason),
            remote_name: name,
        });
    }
}

/// A plugin's session with the engine: the `Control` its calls go through. It checks
/// each call against the plugin's grant, including the hosts and folders a source's or
/// filter's settings reach and a hotkey's bound action, and answers `forbidden` for one
/// outside it without reaching the engine. Its changes are attributed to the plugin's
/// client name, `plugin:<id>`, in the audit log. It holds the engine weakly.
struct PluginControl {
    engine: WeakEngine,
    client_id: ClientId,
    grant: Arc<Mutex<PluginGrant>>,
    next_request: AtomicU64,
    /// Never used: previews are under "never" in ADR-0069's table.
    previews: SessionPreviews,
}

impl PluginControl {
    fn open(engine: &Engine, plugin_id: &PluginId, grant: Arc<Mutex<PluginGrant>>) -> Self {
        let (client_id, _) = engine.register_client(&format!("plugin:{plugin_id}"));
        PluginControl {
            engine: engine.downgrade(),
            client_id,
            grant,
            next_request: AtomicU64::new(1),
            previews: SessionPreviews::default(),
        }
    }
}

impl Control for PluginControl {
    fn hello(&self, _: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        // The host never sends one: the plugin is lent this session already open.
        Box::pin(future::ready(Err(ProtocolError::Unsupported {
            call: "hello".into(),
        }
        .into())))
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let checked = lock(&self.grant).check(&call);
        if let Err(error) = checked {
            return Box::pin(future::ready(Err(error.into())));
        }
        let Some(engine) = self.engine.upgrade() else {
            return Box::pin(future::ready(Err(ControlError::Disconnected {
                reason: "the engine has ended".into(),
            })));
        };
        let id = RequestId::new(format!(
            "req_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        ));
        match engine.execute(&self.client_id, &id, call, &self.previews) {
            Executed::Ready(result) => Box::pin(future::ready(
                (*result)
                    .map(|result| Response { id, result })
                    .map_err(ControlError::from),
            )),
            Executed::Pending(reply, _span) => Box::pin(async move {
                reply
                    .await
                    .map(|result| Response {
                        id,
                        result: CallResult::Query { result },
                    })
                    .map_err(ControlError::from)
            }),
        }
    }

    fn events(&self) -> EventStream {
        match self.engine.upgrade() {
            Some(engine) => Box::pin(engine.subscribe().0),
            None => Box::pin(Ended),
        }
    }
}

/// A plugin's session holding `grant`, for the tests of [`PluginControl`].
#[cfg(test)]
pub(crate) fn tests_control(engine: &Engine, plugin_id: &str, grant: PluginGrant) -> impl Control {
    PluginControl::open(
        engine,
        &PluginId::new(plugin_id),
        Arc::new(Mutex::new(grant)),
    )
}

impl Drop for PluginControl {
    /// Ends the plugin's session: its history and open gestures go, as when a client
    /// disconnects (ADR-0032).
    fn drop(&mut self) {
        if let Some(engine) = self.engine.upgrade() {
            engine.forget_client(&self.client_id);
        }
    }
}

/// An event stream that has ended.
struct Ended;

impl Stream for Ended {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Event>> {
        Poll::Ready(None)
    }
}

/// Wakes a parked thread.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// The subscription's next event, parking this thread until it comes; `None` once the
/// subscription is closed.
fn next_blocking(subscription: &mut Subscription) -> Option<Event> {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match Pin::new(&mut *subscription).poll_next(&mut cx) {
            Poll::Ready(event) => return event,
            Poll::Pending => thread::park(),
        }
    }
}

/// How a failed load is reported: a module that isn't a plugin is the package's fault, a
/// `start` function that traps or runs past its limits is `action_failed`.
fn load_error(error: &PluginError) -> ProtocolError {
    match error {
        PluginError::Compile(_) | PluginError::ForbiddenImport { .. } | PluginError::NoMemory => {
            invalid(
                "path".into(),
                format!("{MODULE_FILE} isn't a plugin: {}", describe(error)),
            )
        }
        _ => ProtocolError::ActionFailed {
            reason: format!("the plugin failed to load: {}", describe(error)),
        },
    }
}

/// `error` with its sources, as one line.
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn invalid(field: String, reason: String) -> ProtocolError {
    ProtocolError::InvalidArgument {
        field,
        reason,
        blockers: Vec::new(),
    }
}

fn not_found(plugin_id: &PluginId) -> ProtocolError {
    ProtocolError::NotFound {
        entity: EntityKind::Plugin,
        id: plugin_id.to_string(),
    }
}

fn failed(error: &dyn std::error::Error) -> ProtocolError {
    ProtocolError::ActionFailed {
        reason: describe(error),
    }
}

fn ended() -> ProtocolError {
    ProtocolError::Internal {
        reason: "the engine has ended".into(),
    }
}
