//! WASM automation plugins (ADR-0069, ADR-0075, ADR-0076), run on `jstream-plugin-wasm`'s
//! host.
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
//! A grant never gives more than the package's manifest asks for ([`Request`],
//! R-SEC-003), and a package whose manifest asks for more than it did when a person last
//! granted it is disabled as `needs_grant` until a person grants it again. Once the host
//! names a folder ([`open_dir`]), the installed plugins and their grants are saved in
//! [`PLUGINS_FILE`] there after every change, and loaded again when it is opened.

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
    Action, Call, CallResult, ClientId, EntityKind, Event, Hello, PluginClass,
    PluginDisabledReason, PluginGrant, PluginId, PluginState, ProtocolError, RequestId, Response,
    Welcome,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

/// The file in the plugin folder that holds the installed plugins (ADR-0076).
pub(crate) const PLUGINS_FILE: &str = "plugins.json";

/// The version of [`PLUGINS_FILE`]'s layout this engine reads and writes.
const PLUGINS_FILE_VERSION: u32 = 0;

/// The installed plugins, by ID.
#[derive(Default)]
pub(crate) struct Registry {
    /// The wasmtime host, made by the first install: its ticker thread runs from then on.
    host: Mutex<Option<PluginHost>>,
    plugins: Mutex<BTreeMap<PluginId, Entry>>,
    /// Numbers each run of a plugin, so a run that ends late can't touch a newer one.
    runs: AtomicU64,
    /// Where the installed plugins are saved, once the host names a folder. Held while a
    /// save reads the plugins and writes the file, so saves land in the order they read.
    file: Mutex<Option<PathBuf>>,
    /// For tests: how long [`start`] waits between a plugin's load and marking its entry
    /// `Running`, to widen the window in which events queue for it (#839 review B1). Per
    /// engine, so other tests' installs don't wait too.
    #[cfg(test)]
    pub(crate) running_delay_ms: AtomicU64,
}

struct Entry {
    /// The package directory, read again by each `plugin.enable` and each start.
    package: PathBuf,
    /// What the plugin may do, shared with the [`PluginControl`] of its running instance,
    /// so `plugin.grant` applies to its next call.
    grant: Arc<Mutex<PluginGrant>>,
    /// The name its manifest gives itself, the plugin's own text (`remote_name`).
    name: Option<String>,
    /// What its manifest asks for, as last read.
    request: Request,
    /// What its manifest asked for when a person last granted it (installed it or replaced
    /// its grant). A manifest that asks for more than this is an update that needs a
    /// person's grant.
    granted: Request,
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

/// A package, read from disk: the module's bytes and what the manifest says.
struct Package {
    wasm: Vec<u8>,
    name: Option<String>,
    request: Request,
}

/// What a plugin's manifest asks for (ADR-0076): capability classes, network hosts, and
/// whether it wants folders a person picks. A person's grant may give less, never more,
/// since R-SEC-003 lets a plugin reach only what its manifest declares and the person
/// grants. `read` is every plugin's, asked for or not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Request {
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    classes: BTreeSet<PluginClass>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    folders: bool,
}

impl Request {
    /// The first part of `grant` this request doesn't ask for, as the grant's field and
    /// why, or `None` when it asks for all of it.
    fn excess(&self, grant: &PluginGrant) -> Option<(String, String)> {
        for (i, class) in grant.classes.iter().enumerate() {
            if *class != PluginClass::Read && !self.classes.contains(class) {
                return Some((
                    format!("grant.classes.{i}"),
                    format!("the plugin's manifest doesn't ask for `{class}`"),
                ));
            }
        }
        for (i, host) in grant.hosts.iter().enumerate() {
            if !self.hosts.contains(host) {
                return Some((
                    format!("grant.hosts.{i}"),
                    format!("the plugin's manifest doesn't name `{host}`"),
                ));
            }
        }
        if !grant.folders.is_empty() && !self.folders {
            return Some((
                "grant.folders.0".into(),
                "the plugin's manifest doesn't ask for folders".into(),
            ));
        }
        None
    }

    /// What this request asks for that `earlier` didn't, as a list for a person, or
    /// `None` when it asks for nothing more.
    fn beyond(&self, earlier: &Request) -> Option<String> {
        let mut more: Vec<String> = self
            .classes
            .iter()
            .filter(|class| **class != PluginClass::Read && !earlier.classes.contains(class))
            .map(|class| format!("`{class}`"))
            .collect();
        more.extend(
            self.hosts
                .iter()
                .filter(|host| !earlier.hosts.contains(host))
                .map(|host| format!("`{host}`")),
        );
        if self.folders && !earlier.folders {
            more.push("folders".into());
        }
        (!more.is_empty()).then(|| more.join(", "))
    }

    /// `grant` without what this request doesn't ask for, as after an update that asks for
    /// less.
    fn narrow(&self, grant: &PluginGrant) -> PluginGrant {
        let mut narrowed = grant.clone();
        narrowed
            .classes
            .retain(|class| *class == PluginClass::Read || self.classes.contains(class));
        narrowed.hosts.retain(|host| self.hosts.contains(host));
        if !self.folders {
            narrowed.folders.clear();
        }
        narrowed
    }
}

/// [`PLUGINS_FILE`]: every installed plugin, with what a person granted it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    #[serde(default)]
    plugins: Vec<SavedPlugin>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPlugin {
    plugin_id: PluginId,
    package: PathBuf,
    #[serde(default)]
    grant: PluginGrant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    request: Request,
    #[serde(default)]
    granted: Request,
    /// Why the engine disabled it; absent for a plugin that was enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disabled: Option<PluginDisabledReason>,
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
    if let Some((field, reason)) = read.request.excess(&grant) {
        return Err(invalid(field, reason));
    }
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
                request: read.request.clone(),
                granted: read.request,
                state: Run::Starting,
            },
        );
    }
    let started = start(engine, plugin_id, read.wasm).inspect_err(|_| {
        lock(&registry.plugins).remove(plugin_id);
    });
    save(engine);
    started
}

fn set_grant(
    engine: &Engine,
    plugin_id: &PluginId,
    grant: PluginGrant,
) -> Result<(), ProtocolError> {
    check_grant(&grant)?;
    {
        let mut plugins = lock(&engine.automation().plugins);
        let entry = plugins
            .get_mut(plugin_id)
            .ok_or_else(|| not_found(plugin_id))?;
        if let Some((field, reason)) = entry.request.excess(&grant) {
            return Err(invalid(field, reason));
        }
        *lock(&entry.grant) = grant;
        // The person granted it knowing what it asks for now.
        entry.granted = entry.request.clone();
    }
    save(engine);
    Ok(())
}

fn enable(engine: &Engine, plugin_id: &PluginId) -> Result<(), ProtocolError> {
    let registry = engine.automation();
    let was = {
        let mut plugins = lock(&registry.plugins);
        let entry = plugins
            .get_mut(plugin_id)
            .ok_or_else(|| not_found(plugin_id))?;
        let Run::Disabled(reason) = entry.state else {
            // Running, or being started by another call: already enabled.
            return Ok(());
        };
        entry.state = Run::Starting;
        reason
    };
    let loaded = load(engine, plugin_id, was);
    save(engine);
    loaded
}

/// Loads `plugin_id`'s package again, for an enable or a start of the saved plugins: its
/// entry is `Starting`, and was disabled for `was`. A manifest that asks for more than the
/// person granted leaves it disabled as `needs_grant`; one that asks for less narrows its
/// grant to match. Any other failure leaves it disabled for `was`. A change of reason is
/// reported with `plugin.state_changed`.
fn load(
    engine: &Engine,
    plugin_id: &PluginId,
    was: PluginDisabledReason,
) -> Result<(), ProtocolError> {
    let registry = engine.automation();
    let loaded = (|| {
        let package = lock(&registry.plugins)
            .get(plugin_id)
            .map(|entry| entry.package.clone())
            .ok_or_else(|| (was, not_found(plugin_id)))?;
        let read = read_package(&package).map_err(|error| {
            (
                was,
                ProtocolError::ActionFailed {
                    reason: error.to_string(),
                },
            )
        })?;
        {
            let mut plugins = lock(&registry.plugins);
            let entry = plugins
                .get_mut(plugin_id)
                .ok_or_else(|| (was, not_found(plugin_id)))?;
            entry.name = read.name;
            let more = read.request.beyond(&entry.granted);
            if more.is_none() {
                let mut grant = lock(&entry.grant);
                *grant = read.request.narrow(&grant);
            }
            entry.request = read.request;
            if let Some(more) = more {
                return Err((
                    PluginDisabledReason::NeedsGrant,
                    ProtocolError::ActionFailed {
                        reason: format!(
                            "its update asks for {more}, which a person hasn't granted: \
                             grant it with `plugin.grant`, then enable it"
                        ),
                    },
                ));
            }
        }
        start(engine, plugin_id, read.wasm).map_err(|error| {
            (
                was,
                match error {
                    ProtocolError::InvalidArgument { reason, .. } => {
                        ProtocolError::ActionFailed { reason }
                    }
                    other => other,
                },
            )
        })
    })();
    let Err((reason, error)) = loaded else {
        return Ok(());
    };
    let name = {
        let mut plugins = lock(&registry.plugins);
        let Some(entry) = plugins.get_mut(plugin_id) else {
            return Err(error);
        };
        entry.state = Run::Disabled(reason);
        entry.name.clone()
    };
    if reason != was {
        tracing::warn!(plugin_id = %plugin_id, reason = ?reason, %error, "plugin disabled");
        disabled(engine, plugin_id, reason, name);
    }
    Err(error)
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

fn disabled(
    engine: &Engine,
    plugin_id: &PluginId,
    reason: PluginDisabledReason,
    remote_name: Option<String>,
) {
    engine.emit(&Event::PluginStateChanged {
        plugin_id: plugin_id.clone(),
        state: PluginState::Disabled,
        reason: Some(reason),
        remote_name,
    });
}

/// Opens the plugin folder `dir` for `engine` (ADR-0076): reads the plugins saved in its
/// [`PLUGINS_FILE`], loads each that was enabled, and saves every change from then on.
/// A missing folder or file is created. A file that can't be read or isn't one this
/// version reads is logged and left as it is, and nothing is loaded or saved this run,
/// so a newer version's file isn't overwritten.
pub(crate) fn open_dir(engine: &Engine, dir: PathBuf) {
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::error!(dir = %dir.display(), %error, "can't create the plugin folder: plugins won't be saved");
        return;
    }
    let path = dir.join(PLUGINS_FILE);
    let saved = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) if saved.version == PLUGINS_FILE_VERSION => saved.plugins,
            Ok(saved) => {
                tracing::error!(path = %path.display(), version = saved.version, "the plugins file is a version this engine doesn't read: plugins won't be loaded or saved");
                return;
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "the plugins file can't be read: plugins won't be loaded or saved");
                return;
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "the plugins file can't be read: plugins won't be loaded or saved");
            return;
        }
    };
    let registry = engine.automation();
    *lock(&registry.file) = Some(path);
    let mut enabled = Vec::new();
    {
        let mut plugins = lock(&registry.plugins);
        for plugin in saved {
            if plugins.contains_key(&plugin.plugin_id) {
                tracing::warn!(plugin_id = %plugin.plugin_id, "a plugin is saved twice: the first is kept");
                continue;
            }
            let state = match plugin.disabled {
                None => {
                    enabled.push(plugin.plugin_id.clone());
                    Run::Starting
                }
                // A reason this version doesn't know is a fault, as far as it can tell.
                Some(PluginDisabledReason::Unknown) => Run::Disabled(PluginDisabledReason::Trap),
                Some(reason) => Run::Disabled(reason),
            };
            plugins.insert(
                plugin.plugin_id,
                Entry {
                    package: plugin.package,
                    grant: Arc::new(Mutex::new(plugin.grant)),
                    name: plugin.name,
                    request: plugin.request,
                    granted: plugin.granted,
                    state,
                },
            );
        }
    }
    for plugin_id in enabled {
        // A saved plugin that no longer loads (its package gone or broken, or its `start`
        // trapping) is disabled as a fault; one whose update asks for more, as
        // `needs_grant`.
        if let Err(error) = load(engine, &plugin_id, PluginDisabledReason::Trap) {
            tracing::warn!(plugin_id = %plugin_id, %error, "a saved plugin didn't load");
        }
    }
    save(engine);
}

/// Writes the installed plugins to the plugin folder's [`PLUGINS_FILE`], if the host named
/// one. A plugin being started is saved as enabled. A failed save is logged at `error`;
/// the next change saves again.
fn save(engine: &Engine) {
    let registry = engine.automation();
    let file = lock(&registry.file);
    let Some(path) = file.as_ref() else {
        return;
    };
    let plugins = lock(&registry.plugins)
        .iter()
        .map(|(plugin_id, entry)| SavedPlugin {
            plugin_id: plugin_id.clone(),
            package: entry.package.clone(),
            grant: lock(&entry.grant).clone(),
            name: entry.name.clone(),
            request: entry.request.clone(),
            granted: entry.granted.clone(),
            disabled: match entry.state {
                Run::Disabled(reason) => Some(reason),
                Run::Starting | Run::Running(_) => None,
            },
        })
        .collect();
    let saved = Saved {
        version: PLUGINS_FILE_VERSION,
        plugins,
    };
    let written = serde_json::to_vec_pretty(&saved)
        .map_err(std::io::Error::other)
        .and_then(|bytes| crate::collection::write_atomically(path, &bytes));
    if let Err(error) = written {
        tracing::error!(path = %path.display(), %error, "the installed plugins couldn't be saved");
    }
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
    let (name, request) = match std::fs::read(package.join(MANIFEST_FILE)) {
        Ok(bytes) => {
            read_manifest(&bytes).map_err(|reason| path(format!("its {MANIFEST_FILE} {reason}")))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, Request::default()),
        Err(e) => return Err(path(format!("its {MANIFEST_FILE} can't be read: {e}"))),
    };
    Ok(Package {
        wasm,
        name,
        request,
    })
}

/// A manifest's name and what it asks for (ADR-0076): a JSON object whose `name` is a
/// string, `classes` capability classes, `hosts` hosts as `scheme://host:port`, and
/// `folders` whether it wants folders a person picks, each optional. Fields it doesn't
/// know are ignored, so a later version can add some. The error completes "its manifest
/// …".
fn read_manifest(bytes: &[u8]) -> Result<(Option<String>, Request), String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("isn't valid JSON: {e}"))?;
    let serde_json::Value::Object(fields) = manifest else {
        return Err("isn't a JSON object".into());
    };
    let field = |name: &str| fields.get(name).filter(|value| !value.is_null());
    let name = match field("name") {
        None => None,
        Some(serde_json::Value::String(name)) => Some(name.clone()),
        Some(_) => return Err("has a `name` that isn't a string".into()),
    };
    let classes = match field("classes") {
        None => BTreeSet::new(),
        Some(classes) => serde_json::from_value(classes.clone())
            .map_err(|e| format!("has `classes` that aren't a list of capability classes: {e}"))?,
    };
    let hosts: Vec<String> = match field("hosts") {
        None => Vec::new(),
        Some(hosts) => serde_json::from_value(hosts.clone())
            .map_err(|e| format!("has `hosts` that aren't a list of strings: {e}"))?,
    };
    let mut asked = PluginGrant::default();
    asked.hosts.clone_from(&hosts);
    if let Some((field, reason)) = asked.problem() {
        return Err(format!("has a `{field}` that {reason}"));
    }
    let folders = match field("folders") {
        None => false,
        Some(serde_json::Value::Bool(folders)) => *folders,
        Some(_) => return Err("has a `folders` that isn't true or false".into()),
    };
    Ok((
        name,
        Request {
            classes,
            hosts,
            folders,
        },
    ))
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
            PluginError::MemoryLimit(_) => PluginDisabledReason::MemoryLimit,
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
        disabled(&engine, &self.plugin_id, reason, name);
        save(&engine);
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
