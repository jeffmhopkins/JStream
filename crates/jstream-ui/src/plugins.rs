//! The Plugins section (WA1, ADR-0069, ADR-0076): the WASM automation plugins installed
//! in the engine, and the dialogs that install one and grant it what it may do.
//!
//! - **Calls.** `plugin.list` reads the plugins, `plugin.install` installs a package with
//!   no grant beyond `read`, `plugin.grant` replaces a grant and `plugin.enable` turns a
//!   disabled plugin back on. They are a person's calls: no plugin or agent can make
//!   them. The section re-reads the list after each of its calls and on every
//!   `plugin.state_changed`, except while the grant dialog is open ([`ListGate`]).
//! - **The grant dialog** offers exactly what the plugin's manifest asks for, each
//!   class with what it lets the plugin do, each host, and the folders a person picks
//!   when it asks for folders. `read` is always granted and can't be unticked. It opens
//!   by itself after an install whose manifest asks for more than `read`, and its OK
//!   also enables a plugin disabled as `needs_grant`, since granting is what that plugin
//!   waits for. The engine takes a grant to acknowledge the request its client was last
//!   listed (ADR-0090), and [`ListGate`] makes that the request the dialog shows: while
//!   the dialog is open the section sends no `plugin.list` (a re-read it would have made
//!   waits until the dialog closes), and OK, and so Enter, stays disabled while a list
//!   is on its way. A list sent before the dialog opened that answers a different
//!   request for its plugin draws the dialog again before OK can act. When the enable
//!   after a grant fails because the plugin now asks for more, the dialog opens again on
//!   what it asks for now, with why.
//! - **The plugin's name** comes from its manifest: the plugin's own text, shown as data
//!   and never followed (ADR-0059). The ID and the package path are shown beside it.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0076-saved-wasm-plugins-and-manifest-requests.md, docs/adr/0090-a-grant-acknowledges-the-request-its-client-saw.md

use crate::app::SessionContext;
use crate::dialog::Modal;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Call, CallResult, Event, PluginClass, PluginDisabledReason, PluginGrant, PluginId,
    PluginInfo, PluginRequest, PluginState, Query, QueryResult,
};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::BTreeSet;

/// The classes a person may grant, in the order the dialog shows them, with what each
/// lets a plugin do. `read` comes first and is always granted.
pub const CLASSES: [(PluginClass, &str, &str); 4] = [
    (
        PluginClass::Read,
        "Read",
        "See the scenes, sources, outputs and audio, and get the engine's events. Every plugin has it.",
    ),
    (
        PluginClass::Edit,
        "Edit",
        "Change scenes, sources, filters, transitions, audio and hotkeys, and undo its own changes.",
    ),
    (PluginClass::Live, "Live", "Switch the scene on air."),
    (
        PluginClass::Outputs,
        "Outputs",
        "Start and stop the outputs that exist, such as the stream and the recording.",
    ),
];

/// What to call `plugin` in a list: the name its manifest gives, or else its ID.
pub fn display_name(plugin: &PluginInfo) -> String {
    plugin
        .remote_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| plugin.plugin_id.to_string())
}

/// The plugin's state, as its row says it.
pub fn state_text(plugin: &PluginInfo) -> &'static str {
    match (plugin.state, plugin.reason) {
        (PluginState::Enabled, _) => "On",
        (_, Some(PluginDisabledReason::NeedsGrant)) => "Off: its update asks for more",
        (_, Some(PluginDisabledReason::CpuLimit)) => "Off: it ran too long",
        (_, Some(PluginDisabledReason::MemoryLimit)) => "Off: it used too much memory",
        (_, Some(PluginDisabledReason::Trap)) => "Off: it failed",
        _ => "Off",
    }
}

/// Whether `request` asks for anything beyond `read`, so an install should go on to the
/// grant dialog.
pub fn asks_beyond_read(request: &PluginRequest) -> bool {
    request.classes.iter().any(|c| *c != PluginClass::Read)
        || !request.hosts.is_empty()
        || request.folders
}

/// What the plugin's request asks for that its grant doesn't give, as a list for a
/// person, or `None` when the grant gives all of it.
pub fn not_granted(plugin: &PluginInfo) -> Option<String> {
    let mut missing: Vec<String> = CLASSES
        .iter()
        .filter(|(class, ..)| {
            *class != PluginClass::Read
                && plugin.request.classes.contains(class)
                && !plugin.grant.classes.contains(class)
        })
        .map(|(_, label, _)| (*label).to_owned())
        .collect();
    missing.extend(
        plugin
            .request
            .hosts
            .iter()
            .filter(|host| !plugin.grant.hosts.contains(host))
            .cloned(),
    );
    if plugin.request.folders && plugin.grant.folders.is_empty() {
        missing.push("folders".into());
    }
    (!missing.is_empty()).then(|| missing.join(", "))
}

/// An ID for a plugin installed from `path`: `plg_` and the package folder's name, in
/// lowercase letters, digits and underscores, made unique among `taken`.
pub fn suggested_id(path: &str, taken: &[PluginId]) -> String {
    let folder = path
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let mut slug = String::new();
    for c in folder.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('_') && !slug.is_empty() {
            slug.push('_');
        }
    }
    let slug = slug.trim_end_matches('_');
    let base = if slug.is_empty() {
        "plg_plugin".to_owned()
    } else {
        format!("plg_{slug}")
    };
    let free = |id: &str| !taken.iter().any(|t| t.as_str() == id);
    if free(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| free(id))
        .unwrap_or(base)
}

/// The grant dialog's choices, made from what the manifest asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantForm {
    /// The classes ticked, besides `read`.
    pub classes: BTreeSet<PluginClass>,
    /// The hosts ticked.
    pub hosts: BTreeSet<String>,
    /// The folders, one absolute path a line.
    pub folders: String,
}

impl GrantForm {
    /// The dialog as it opens for `plugin`: what it is granted now, within what it asks
    /// for.
    pub fn of(plugin: &PluginInfo) -> GrantForm {
        GrantForm {
            classes: plugin
                .grant
                .classes
                .iter()
                .filter(|c| **c != PluginClass::Read && plugin.request.classes.contains(c))
                .copied()
                .collect(),
            hosts: plugin
                .grant
                .hosts
                .iter()
                .filter(|h| plugin.request.hosts.contains(h))
                .cloned()
                .collect(),
            folders: if plugin.request.folders {
                plugin.grant.folders.join("\n")
            } else {
                String::new()
            },
        }
    }

    /// The grant to send for `request`: what is ticked and asked for, and each non-empty
    /// folder line, trimmed, when it asks for folders. The engine checks the rest.
    pub fn grant(&self, request: &PluginRequest) -> PluginGrant {
        PluginGrant {
            classes: self
                .classes
                .iter()
                .filter(|c| request.classes.contains(c))
                .copied()
                .collect(),
            hosts: request
                .hosts
                .iter()
                .filter(|h| self.hosts.contains(*h))
                .cloned()
                .collect(),
            folders: if request.folders {
                self.folders
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// The installed plugins (`plugin.list`).
pub async fn list(control: &dyn Control) -> Result<Vec<PluginInfo>, String> {
    let call = Call::Query {
        query: Query::PluginList,
    };
    match control.call(call).await.map_err(|e| e.to_string())?.result {
        CallResult::Query {
            result: QueryResult::PluginList { plugins },
        } => Ok(plugins),
        _ => Err("the engine answered `plugin.list` with something else".into()),
    }
}

/// Runs a plugin action.
async fn act(control: &dyn Control, action: Action) -> Result<(), ControlError> {
    control.call(Call::Action { action }).await.map(drop)
}

/// Installs the package at `path` as `plugin_id`, granting it nothing beyond `read`.
pub async fn install(
    control: &dyn Control,
    plugin_id: PluginId,
    path: String,
) -> Result<(), String> {
    act(
        control,
        Action::PluginInstall {
            plugin_id,
            path,
            grant: PluginGrant::default(),
        },
    )
    .await
    .map_err(|e| e.to_string())
}

/// Replaces `plugin_id`'s grant, then enables it when `enable` (a plugin disabled as
/// `needs_grant`).
pub async fn grant(
    control: &dyn Control,
    plugin_id: PluginId,
    grant: PluginGrant,
    enable: bool,
) -> Result<(), String> {
    act(
        control,
        Action::PluginGrant {
            plugin_id: plugin_id.clone(),
            grant,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    if enable {
        act(control, Action::PluginEnable { plugin_id })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Enables a disabled plugin.
pub async fn enable(control: &dyn Control, plugin_id: PluginId) -> Result<(), String> {
    act(control, Action::PluginEnable { plugin_id })
        .await
        .map_err(|e| e.to_string())
}

/// When the section may send `plugin.list`, and when the grant dialog's OK may act
/// (#978, ADR-0090). The engine notes each list as what this client was shown, and a
/// grant acknowledges the last one. So no list goes out while the grant dialog shows a
/// request: one wanted then waits, and is sent once when the dialog closes. And OK waits
/// while a list is on its way, so a list sent before the dialog opened has drawn it again
/// with what it answers before a grant can go out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListGate {
    /// The grant dialog is open.
    open: bool,
    /// A list was wanted while it was open.
    deferred: bool,
    /// Lists sent and not answered yet.
    in_flight: u32,
}

impl ListGate {
    /// The grant dialog opens.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Whether a wanted list may be sent now; if not, it is sent when the dialog closes.
    pub fn may_list(&mut self) -> bool {
        if self.open {
            self.deferred = true;
        }
        !self.open
    }

    /// The grant dialog closes: whether the list it held back is due now.
    pub fn close(&mut self) -> bool {
        self.open = false;
        std::mem::take(&mut self.deferred)
    }

    /// A list goes out.
    pub fn sent(&mut self) {
        self.in_flight += 1;
    }

    /// A list was answered, or failed.
    pub fn answered(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Whether the grant dialog's OK may act: no list is on its way.
    pub fn may_grant(&self) -> bool {
        self.in_flight == 0
    }
}

/// The section's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Section {
    session: SessionContext,
    plugins: RwSignal<Vec<PluginInfo>>,
    problem: RwSignal<Option<String>>,
    /// The install dialog, open while set.
    installing: RwSignal<bool>,
    /// The plugin whose grant dialog is open, as the dialog shows it. Set and cleared
    /// through [`Section::open_grant`] and [`Section::close_grant`], which keep `gate`.
    granting: RwSignal<Option<PluginInfo>>,
    /// What the grant dialog shows went wrong.
    grant_problem: RwSignal<Option<String>>,
    gate: RwSignal<ListGate>,
}

impl Section {
    /// Re-reads the plugins. Only for when the grant dialog is closed: otherwise
    /// [`Section::refresh`]. While it is on its way the grant dialog's OK waits.
    async fn reload(self) -> Option<Vec<PluginInfo>> {
        let session = self.session.get()?;
        self.gate.try_update(ListGate::sent);
        let listed = list(&*session.control).await;
        let read = match listed {
            Ok(plugins) => {
                // A list sent before the dialog opened, answering another request for its
                // plugin, draws the dialog again, so it shows what the engine noted.
                let redraw = self
                    .granting
                    .try_get_untracked()
                    .flatten()
                    .and_then(|shown| {
                        plugins
                            .iter()
                            .find(|p| p.plugin_id == shown.plugin_id)
                            .filter(|p| (&p.request, p.reason) != (&shown.request, shown.reason))
                            .cloned()
                    });
                self.plugins.try_set(plugins.clone());
                if let Some(plugin) = redraw {
                    self.granting.try_set(Some(plugin));
                }
                Some(plugins)
            }
            Err(problem) => {
                self.problem.try_set(Some(problem));
                None
            }
        };
        self.gate.try_update(ListGate::answered);
        read
    }

    /// Re-reads the plugins now, or once the grant dialog closes if it is open.
    async fn refresh(self) {
        if self.gate.try_update(ListGate::may_list).unwrap_or(false) {
            self.reload().await;
        }
    }

    /// What the section does with an engine event.
    async fn on_event(self, event: &Event) {
        if matches!(
            event,
            Event::PluginStateChanged { .. } | Event::EventsDropped { .. }
        ) {
            self.refresh().await;
        }
    }

    /// Opens the grant dialog for `plugin_id`, as the plugins were last read, showing
    /// `problem` if given.
    fn open_grant(self, plugin_id: &PluginId, problem: Option<String>) {
        let Some(plugin) = self
            .plugins
            .try_with_untracked(|all| all.iter().find(|p| p.plugin_id == *plugin_id).cloned())
            .flatten()
        else {
            return;
        };
        self.gate.try_update(ListGate::open);
        self.grant_problem.try_set(problem);
        self.granting.try_set(Some(plugin));
    }

    /// Closes the grant dialog, then re-reads the plugins if `reload` or if a re-read was
    /// held back while it was open.
    async fn close_grant(self, reload: bool) {
        self.granting.try_set(None);
        let due = self.gate.try_update(ListGate::close).unwrap_or(false);
        if reload || due {
            self.reload().await;
        }
    }

    /// The grant dialog's OK: sends `grant` for the plugin it shows and, for one disabled
    /// as `needs_grant`, enables it. Does nothing while a list is on its way. A failed
    /// grant keeps the dialog open, its re-read held back. A failed enable after a grant
    /// (an update that asks for still more) closes it, re-reads, and opens it again on
    /// what the plugin asks for now, with why.
    async fn submit_grant(self, grant_to: PluginGrant) {
        let Some(shown) = self.granting.try_get_untracked().flatten() else {
            return;
        };
        if !self
            .gate
            .try_with_untracked(ListGate::may_grant)
            .unwrap_or(false)
        {
            return;
        }
        let Some(session) = self.session.get() else {
            return;
        };
        let control = &*session.control;
        if let Err(error) = grant(control, shown.plugin_id.clone(), grant_to, false).await {
            self.grant_problem.try_set(Some(error));
            self.refresh().await;
            return;
        }
        if shown.reason == Some(PluginDisabledReason::NeedsGrant)
            && let Err(error) = enable(control, shown.plugin_id.clone()).await
        {
            self.close_grant(true).await;
            self.open_grant(&shown.plugin_id, Some(error));
            return;
        }
        self.close_grant(true).await;
    }

    fn enable(self, plugin_id: PluginId) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let done = enable(&*session.control, plugin_id).await;
            self.problem.try_set(done.err());
            self.refresh().await;
        });
    }
}

/// The Plugins section. Shows nothing until `plugin.list` answers.
#[component]
pub fn Plugins() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return ().into_any();
    };
    let section = Section {
        session,
        plugins: RwSignal::new(Vec::new()),
        problem: RwSignal::new(None),
        installing: RwSignal::new(false),
        granting: RwSignal::new(None),
        grant_problem: RwSignal::new(None),
        gate: RwSignal::new(ListGate::default()),
    };
    let read = RwSignal::new(false);
    spawn_local(async move {
        if section.reload().await.is_some() {
            read.try_set(true);
        }
    });
    if let Some(current) = session.get() {
        let mut events = current.control.events();
        drop(current);
        spawn_local(async move {
            while let Some(event) = events.next().await {
                // The section is gone: stop reading.
                if section.plugins.try_with_untracked(|_| ()).is_none() {
                    break;
                }
                section.on_event(&event).await;
            }
        });
    }

    let ids = move || {
        section.plugins.with(|plugins| {
            plugins
                .iter()
                .map(|p| p.plugin_id.clone())
                .collect::<Vec<_>>()
        })
    };
    let row = move |plugin_id: PluginId| {
        let info = move || {
            section
                .plugins
                .with(|plugins| plugins.iter().find(|p| p.plugin_id == plugin_id).cloned())
        };
        move || {
            info().map(|plugin| {
                let enabled = plugin.state == PluginState::Enabled;
                let grant_id = plugin.plugin_id.clone();
                let enable_id = plugin.plugin_id.clone();
                let missing = not_granted(&plugin);
                view! {
                    <li class="plugin" class:off=!enabled data-plugin=plugin.plugin_id.to_string()>
                        <div class="plugin-head">
                            <span class="plugin-name">{display_name(&plugin)}</span>
                            <span class="plugin-state" class:on=enabled>{state_text(&plugin)}</span>
                        </div>
                        <span class="plugin-meta mono" title=plugin.path.clone()>
                            {format!("{} · {}", plugin.plugin_id, plugin.path)}
                        </span>
                        {missing.map(|m| view! {
                            <span class="plugin-meta">"Asks for, not granted: " {m}</span>
                        })}
                        <div class="plugin-actions">
                            <button on:click=move |_| section.open_grant(&grant_id, None)>
                                "Permissions…"
                            </button>
                            {(!enabled).then(|| view! {
                                <button on:click=move |_| section.enable(enable_id.clone())>"Turn on"</button>
                            })}
                        </div>
                    </li>
                }
            })
        }
    };

    view! {
        <Show when=move || read.get()>
            <section class="plugins">
                <h2>"Plugins"</h2>
                <button class="primary" on:click=move |_| section.installing.set(true)>
                    "Install plugin"
                </button>
                {move || section.problem.get().map(|problem| view! {
                    <p class="problem" role="alert">{problem}</p>
                })}
                <Show
                    when=move || section.plugins.with(|plugins| !plugins.is_empty())
                    fallback=|| view! { <p class="placeholder">"No plugins installed."</p> }
                >
                    <ul class="plugin-list">
                        <For each=ids key=|id| id.clone() let:plugin_id>
                            {row(plugin_id)}
                        </For>
                    </ul>
                </Show>
            </section>
            <Show when=move || section.installing.get()>
                <InstallDialog section=section />
            </Show>
            {move || section.granting.get().map(|plugin| view! {
                <GrantDialog section=section plugin=plugin />
            })}
        </Show>
    }
    .into_any()
}

/// The install dialog: the package folder's path and the plugin's ID.
#[component]
fn InstallDialog(section: Section) -> impl IntoView {
    let path = RwSignal::new(String::new());
    let id = RwSignal::new(String::new());
    let id_edited = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let cancel = move || section.installing.set(false);
    let suggest = move || {
        if !id_edited.get_untracked() {
            let taken = section.plugins.with_untracked(|plugins| {
                plugins
                    .iter()
                    .map(|p| p.plugin_id.clone())
                    .collect::<Vec<_>>()
            });
            id.set(suggested_id(&path.get_untracked(), &taken));
        }
    };
    let ok = move |_| {
        if busy.get_untracked() {
            return;
        }
        let (path, plugin_id) = (
            path.get_untracked().trim().to_owned(),
            id.get_untracked().trim().to_owned(),
        );
        if path.is_empty() {
            problem.set(Some("Type the path of the plugin's folder.".into()));
            return;
        }
        if plugin_id.is_empty() {
            problem.set(Some("Give the plugin an ID.".into()));
            return;
        }
        let Some(session) = section.session.get() else {
            return;
        };
        busy.set(true);
        let plugin_id = PluginId::new(plugin_id);
        spawn_local(async move {
            let installed = install(&*session.control, plugin_id.clone(), path).await;
            busy.try_set(false);
            if let Err(error) = installed {
                problem.try_set(Some(error));
                return;
            }
            section.installing.try_set(false);
            let plugins = section.reload().await.unwrap_or_default();
            // A plugin that asks for more than `read` goes on to its grant.
            if plugins
                .iter()
                .any(|p| p.plugin_id == plugin_id && asks_beyond_read(&p.request))
            {
                section.open_grant(&plugin_id, None);
            }
        });
    };
    view! {
        <Modal label="Install plugin" class="plugin-dialog" cancel=cancel>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Install plugin"</h2>
                    <span class="modal-sub">"A WASM automation plugin, from a folder on the engine's machine"</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| cancel()>"×"</button>
            </div>
            <div class="plugin-dialog-body">
                <label class="field">
                    <span>"Folder"</span>
                    <input
                        type="text"
                        class="mono"
                        placeholder="/path/to/plugin"
                        prop:value=move || path.get()
                        on:input=move |ev| {
                            path.set(event_target_value(&ev));
                            suggest();
                        }
                    />
                </label>
                <label class="field">
                    <span>"ID"</span>
                    <input
                        type="text"
                        class="mono"
                        prop:value=move || id.get()
                        on:input=move |ev| {
                            id_edited.set(true);
                            id.set(event_target_value(&ev));
                        }
                    />
                </label>
                <p class="plugin-note">
                    "The folder holds " <code>"plugin.wasm"</code> " and may hold "
                    <code>"manifest.json"</code> ". The plugin starts with read access only; "
                    "you choose what else it may do next."
                </p>
                {move || problem.get().map(|e| view! { <p class="problem" role="alert">{e}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary" on:click=move |_| cancel()>"Cancel"</button>
                <button type="button" class="primary" data-primary disabled=move || busy.get() on:click=ok>
                    "Install"
                </button>
            </div>
        </Modal>
    }
}

/// The grant dialog for `plugin`: what its manifest asks for, to tick.
#[component]
fn GrantDialog(section: Section, plugin: PluginInfo) -> impl IntoView {
    let form = RwSignal::new(GrantForm::of(&plugin));
    let problem = section.grant_problem;
    let busy = RwSignal::new(false);
    let request = StoredValue::new(plugin.request.clone());
    let needs_grant = plugin.reason == Some(PluginDisabledReason::NeedsGrant);
    // OK, and so Enter, waits while a list is on its way (`ListGate::may_grant`).
    let waiting = move || busy.get() || !section.gate.with(ListGate::may_grant);
    let cancel = move || spawn_local(section.close_grant(false));
    let ok = move |_| {
        if busy.get_untracked() {
            return;
        }
        let granted = form.with_untracked(|f| request.with_value(|r| f.grant(r)));
        busy.set(true);
        spawn_local(async move {
            section.submit_grant(granted).await;
            busy.try_set(false);
        });
    };

    let classes = CLASSES
        .iter()
        .filter(|(class, ..)| *class == PluginClass::Read || plugin.request.classes.contains(class))
        .map(|&(class, label, what)| {
            let read = class == PluginClass::Read;
            let checked = move || read || form.with(|f| f.classes.contains(&class));
            view! {
                <label class="plugin-choice">
                    <input
                        type="checkbox"
                        disabled=read
                        prop:checked=checked
                        on:change=move |ev| {
                            let on = event_target_checked(&ev);
                            form.update(|f| {
                                if on {
                                    f.classes.insert(class);
                                } else {
                                    f.classes.remove(&class);
                                }
                            });
                        }
                    />
                    <span class="plugin-choice-text">
                        <span class="plugin-choice-name">{label}</span>
                        <span class="plugin-choice-what">{what}</span>
                    </span>
                </label>
            }
        })
        .collect_view();
    let hosts = (!plugin.request.hosts.is_empty()).then(|| {
        let rows = plugin
            .request
            .hosts
            .iter()
            .map(|host| {
                let host = host.clone();
                let mine = host.clone();
                let shown = host.clone();
                view! {
                    <label class="plugin-choice">
                        <input
                            type="checkbox"
                            prop:checked=move || form.with(|f| f.hosts.contains(&mine))
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                let host = host.clone();
                                form.update(|f| {
                                    if on {
                                        f.hosts.insert(host);
                                    } else {
                                        f.hosts.remove(&host);
                                    }
                                });
                            }
                        />
                        <span class="plugin-choice-name mono">{shown}</span>
                    </label>
                }
            })
            .collect_view();
        view! {
            <p class="menu-head">"NETWORK"</p>
            <p class="plugin-note">"Hosts the plugin may reach. It reaches no other."</p>
            {rows}
        }
    });
    let folders = plugin.request.folders.then(|| {
        view! {
            <p class="menu-head">"FOLDERS"</p>
            <label class="field">
                <span class="plugin-note">"Folders its settings may name, one absolute path a line. Leave it empty for none."</span>
                <textarea
                    class="mono"
                    rows="3"
                    prop:value=move || form.with(|f| f.folders.clone())
                    on:input=move |ev| {
                        let text = event_target_value(&ev);
                        form.update(|f| f.folders = text);
                    }
                ></textarea>
            </label>
        }
    });
    // The title is the app's own; the plugin's name, its own text, is quoted under it
    // with its ID and path, so a name can't pass for the dialog's chrome (#974 N1).
    let sub = format!(
        "“{}” · {} · {}",
        display_name(&plugin),
        plugin.plugin_id,
        plugin.path
    );
    view! {
        <Modal label="Plugin permissions" class="plugin-dialog" cancel=cancel>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Plugin permissions"</h2>
                    <span class="modal-sub">{sub}</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| cancel()>"×"</button>
            </div>
            <div class="plugin-dialog-body">
                {needs_grant.then(|| view! {
                    <p class="plugin-note">
                        "An update of this plugin asks for more than you granted it. It stays off "
                        "until you grant it again."
                    </p>
                })}
                <p class="plugin-note">
                    "You can grant only what the plugin's manifest asks for. Its name and "
                    "everything it writes are its own text."
                </p>
                <p class="menu-head">"ACCESS"</p>
                {classes}
                {hosts}
                {folders}
                {move || problem.get().map(|e| view! { <p class="problem" role="alert">{e}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary" on:click=move |_| cancel()>"Cancel"</button>
                <button type="button" class="primary" data-primary disabled=waiting on:click=ok>
                    {if needs_grant { "Grant and turn on" } else { "Save" }}
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLASSES, GrantForm, ListGate, asks_beyond_read, display_name, not_granted, state_text,
        suggested_id,
    };
    use jstream_control::protocol::{
        PluginClass, PluginDisabledReason, PluginGrant, PluginId, PluginInfo, PluginRequest,
        PluginState,
    };

    fn plugin(request: PluginRequest, grant: PluginGrant) -> PluginInfo {
        PluginInfo {
            plugin_id: PluginId::new("plg_timer"),
            state: PluginState::Enabled,
            reason: None,
            remote_name: Some("Scene timer".into()),
            path: "/home/me/plugins/timer".into(),
            request,
            grant,
        }
    }

    fn asks_all() -> PluginRequest {
        PluginRequest {
            classes: [PluginClass::Edit, PluginClass::Live].into(),
            hosts: vec!["https://a.example:443".into(), "wss://b.example:443".into()],
            folders: true,
        }
    }

    #[test]
    fn every_class_is_offered_once_with_read_first() {
        assert_eq!(CLASSES[0].0, PluginClass::Read);
        let mut seen: Vec<PluginClass> = CLASSES.iter().map(|c| c.0).collect();
        seen.dedup();
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn a_plugin_is_named_by_its_manifest_or_its_id() {
        let mut p = plugin(PluginRequest::default(), PluginGrant::default());
        assert_eq!(display_name(&p), "Scene timer");
        p.remote_name = Some("  ".into());
        assert_eq!(display_name(&p), "plg_timer");
        p.remote_name = None;
        assert_eq!(display_name(&p), "plg_timer");
    }

    #[test]
    fn the_state_says_why_a_plugin_is_off() {
        let mut p = plugin(PluginRequest::default(), PluginGrant::default());
        assert_eq!(state_text(&p), "On");
        p.state = PluginState::Disabled;
        assert_eq!(state_text(&p), "Off");
        p.reason = Some(PluginDisabledReason::NeedsGrant);
        assert_eq!(state_text(&p), "Off: its update asks for more");
        p.reason = Some(PluginDisabledReason::Trap);
        assert_eq!(state_text(&p), "Off: it failed");
    }

    #[test]
    fn only_a_request_beyond_read_opens_the_grant_dialog() {
        assert!(!asks_beyond_read(&PluginRequest::default()));
        assert!(!asks_beyond_read(&PluginRequest {
            classes: [PluginClass::Read].into(),
            ..PluginRequest::default()
        }));
        assert!(asks_beyond_read(&PluginRequest {
            folders: true,
            ..PluginRequest::default()
        }));
        assert!(asks_beyond_read(&asks_all()));
    }

    #[test]
    fn what_is_asked_for_and_not_granted_is_listed() {
        let p = plugin(
            asks_all(),
            PluginGrant {
                classes: [PluginClass::Edit].into(),
                hosts: vec!["https://a.example:443".into()],
                folders: Vec::new(),
            },
        );
        assert_eq!(
            not_granted(&p).as_deref(),
            Some("Live, wss://b.example:443, folders")
        );
        let all = plugin(
            asks_all(),
            GrantForm {
                classes: [PluginClass::Edit, PluginClass::Live].into(),
                hosts: asks_all().hosts.into_iter().collect(),
                folders: "/data".into(),
            }
            .grant(&asks_all()),
        );
        assert_eq!(not_granted(&all), None);
    }

    #[test]
    fn the_form_opens_with_the_grant_within_the_request() {
        // An update dropped `live` and a host: the form doesn't offer them ticked.
        let p = plugin(
            PluginRequest {
                classes: [PluginClass::Edit].into(),
                hosts: vec!["https://a.example:443".into()],
                folders: true,
            },
            PluginGrant {
                classes: [PluginClass::Edit, PluginClass::Live].into(),
                hosts: vec![
                    "https://a.example:443".into(),
                    "https://gone.example:443".into(),
                ],
                folders: vec!["/a".into(), "/b".into()],
            },
        );
        let form = GrantForm::of(&p);
        assert_eq!(form.classes, [PluginClass::Edit].into());
        assert_eq!(form.hosts, ["https://a.example:443".to_owned()].into());
        assert_eq!(form.folders, "/a\n/b");
    }

    #[test]
    fn the_grant_sent_never_exceeds_the_request() {
        let form = GrantForm {
            classes: [PluginClass::Edit, PluginClass::Outputs].into(),
            hosts: [
                "wss://b.example:443".to_owned(),
                "https://x.example:443".to_owned(),
            ]
            .into(),
            folders: "  /data/in \n\n/data/out\n".into(),
        };
        assert_eq!(
            form.grant(&asks_all()),
            PluginGrant {
                classes: [PluginClass::Edit].into(),
                hosts: vec!["wss://b.example:443".into()],
                folders: vec!["/data/in".into(), "/data/out".into()],
            }
        );
        let no_folders = PluginRequest {
            folders: false,
            ..asks_all()
        };
        assert!(form.grant(&no_folders).folders.is_empty());
    }

    #[test]
    fn an_id_is_suggested_from_the_folder() {
        assert_eq!(
            suggested_id("/home/me/Scene Timer/", &[]),
            "plg_scene_timer"
        );
        assert_eq!(
            suggested_id(r"C:\plugins\chat-switch", &[]),
            "plg_chat_switch"
        );
        assert_eq!(suggested_id("", &[]), "plg_plugin");
        let taken = [PluginId::new("plg_timer"), PluginId::new("plg_timer_2")];
        assert_eq!(suggested_id("/p/timer", &taken), "plg_timer_3");
    }

    use super::{Event, PluginDisabledReason as Reason, Section};
    use crate::app::SessionContext;
    use crate::session::Session;
    use jstream_control::protocol::{
        Action, Call, CallResult, Hello, PROTOCOL_VERSION, ProtocolError, Query, QueryResult,
        RequestId, Response, Welcome,
    };
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockControl, MockEngine};
    use leptos::prelude::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    /// An engine with one plugin, as far as a grant goes (ADR-0090): each `plugin.list`
    /// notes, when it is handled, the request it shows, and a grant acknowledges the last
    /// one noted. An enable fails while the plugin asks for more than was acknowledged.
    /// While `hold` is set, a list is handled but its answer doesn't arrive.
    struct NotingEngine {
        inner: MockControl,
        request: Mutex<PluginRequest>,
        reason: Mutex<Option<Reason>>,
        noted: Mutex<Option<PluginRequest>>,
        acknowledged: Mutex<Option<PluginRequest>>,
        calls: Mutex<Vec<&'static str>>,
        hold: Arc<AtomicBool>,
    }

    impl NotingEngine {
        fn new() -> Rc<Self> {
            Rc::new(NotingEngine {
                inner: MockEngine::new().connect(),
                request: Mutex::new(PluginRequest::default()),
                reason: Mutex::new(None),
                noted: Mutex::new(None),
                acknowledged: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
                hold: Arc::new(AtomicBool::new(false)),
            })
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("not poisoned").clone()
        }

        fn acknowledged(&self) -> Option<PluginRequest> {
            self.acknowledged.lock().expect("not poisoned").clone()
        }

        fn ask(&self, request: PluginRequest) {
            *self.request.lock().expect("not poisoned") = request;
        }
    }

    impl Control for NotingEngine {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            let request = self.request.lock().expect("not poisoned").clone();
            let answer = |result| {
                Ok(Response {
                    id: RequestId::new("engine_1"),
                    result,
                })
            };
            let mut calls = self.calls.lock().expect("not poisoned");
            let result = match call {
                Call::Query {
                    query: Query::PluginList,
                } => {
                    calls.push("plugin.list");
                    *self.noted.lock().expect("not poisoned") = Some(request.clone());
                    let mut listed = plugin(request, PluginGrant::default());
                    listed.state = PluginState::Disabled;
                    listed.reason = *self.reason.lock().expect("not poisoned");
                    let mut result = Some(answer(CallResult::Query {
                        result: QueryResult::PluginList {
                            plugins: vec![listed],
                        },
                    }));
                    let hold = Arc::clone(&self.hold);
                    return Box::pin(std::future::poll_fn(move |_| {
                        if hold.load(Ordering::SeqCst) {
                            Poll::Pending
                        } else {
                            Poll::Ready(result.take().expect("polled once ready"))
                        }
                    }));
                }
                Call::Action {
                    action: Action::PluginGrant { .. },
                } => {
                    calls.push("plugin.grant");
                    let noted = self.noted.lock().expect("not poisoned").clone();
                    *self.acknowledged.lock().expect("not poisoned") =
                        Some(noted.unwrap_or(request));
                    answer(CallResult::action())
                }
                Call::Action {
                    action: Action::PluginEnable { .. },
                } => {
                    calls.push("plugin.enable");
                    if self.acknowledged().as_ref() == Some(&request) {
                        answer(CallResult::action())
                    } else {
                        Err(ControlError::from(ProtocolError::ActionFailed {
                            reason: "its update asks for `edit`".into(),
                        }))
                    }
                }
                other => {
                    drop(calls);
                    return self.inner.call(other);
                }
            };
            Box::pin(std::future::ready(result))
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    /// Polls a future that is ready at once, as the fake engine's are unless held.
    fn now<F: Future>(future: F) -> F::Output {
        match std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the futures here are ready at once"),
        }
    }

    /// The Plugins section's state, on a session with `engine`, its plugins read once.
    fn section(engine: &Rc<NotingEngine>) -> Section {
        let welcome = now(engine.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("handshake");
        let control: Rc<dyn Control> = engine.clone();
        let section = Section {
            session: SessionContext(StoredValue::new_local(Some(Session { control, welcome }))),
            plugins: RwSignal::new(Vec::new()),
            problem: RwSignal::new(None),
            installing: RwSignal::new(false),
            granting: RwSignal::new(None),
            grant_problem: RwSignal::new(None),
            gate: RwSignal::new(ListGate::default()),
        };
        now(section.reload()).expect("lists");
        section
    }

    fn shown(section: Section) -> PluginInfo {
        section
            .granting
            .get_untracked()
            .expect("the dialog is open")
    }

    fn ok_waits(section: Section) -> bool {
        !section.gate.with_untracked(ListGate::may_grant)
    }

    fn state_changed() -> Event {
        Event::PluginStateChanged {
            plugin_id: PluginId::new("plg_timer"),
            state: PluginState::Disabled,
            reason: Some(Reason::NeedsGrant),
            remote_name: None,
        }
    }

    /// #1004 review B1: a `plugin.state_changed` that arrives while the grant dialog is
    /// open sends no `plugin.list`, so the grant acknowledges the request the dialog
    /// shows, and the held-back list goes out once the dialog closes.
    #[test]
    fn a_state_change_while_the_grant_dialog_is_open_sends_no_list() {
        let engine = NotingEngine::new();
        let section = section(&engine);
        section.open_grant(&PluginId::new("plg_timer"), None);
        // Another client loads an update that asks for more, and its state change
        // arrives while the dialog is open.
        engine.ask(asks_all());
        now(section.on_event(&state_changed()));
        now(section.on_event(&Event::EventsDropped { count: 3 }));
        assert_eq!(
            engine.calls(),
            ["plugin.list"],
            "no list while the dialog is open"
        );

        let shown = shown(section);
        now(section.submit_grant(GrantForm::of(&shown).grant(&shown.request)));
        assert_eq!(engine.acknowledged(), Some(PluginRequest::default()));
        assert_eq!(section.granting.get_untracked(), None, "the dialog closed");
        assert_eq!(
            engine.calls(),
            ["plugin.list", "plugin.grant", "plugin.list"],
            "then one list"
        );
    }

    /// #1004 review B3: a list sent before the grant dialog opened, answered after, holds
    /// OK until its answer has drawn the dialog again with the request it noted, so a
    /// grant acknowledges what the dialog shows.
    #[test]
    fn a_list_in_flight_when_the_dialog_opens_holds_ok_until_it_redraws() {
        let engine = NotingEngine::new();
        let section = section(&engine);
        engine.ask(asks_all());
        engine.hold.store(true, Ordering::SeqCst);
        let event = state_changed();
        let mut refresh: Pin<Box<dyn Future<Output = ()>>> = Box::pin(section.on_event(&event));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            refresh.as_mut().poll(&mut cx).is_pending(),
            "the list is on its way"
        );

        // The person opens the dialog on the plugins as last read.
        section.open_grant(&PluginId::new("plg_timer"), None);
        assert_eq!(shown(section).request, PluginRequest::default());
        assert!(ok_waits(section), "OK waits for the list");
        // Pressing it now does nothing.
        now(section.submit_grant(PluginGrant::default()));
        assert_eq!(engine.calls(), ["plugin.list", "plugin.list"]);

        engine.hold.store(false, Ordering::SeqCst);
        assert!(refresh.as_mut().poll(&mut cx).is_ready());
        assert_eq!(shown(section).request, asks_all(), "drawn again");
        assert!(!ok_waits(section));
        let shown = shown(section);
        now(section.submit_grant(GrantForm::of(&shown).grant(&shown.request)));
        assert_eq!(
            engine.acknowledged(),
            Some(asks_all()),
            "what the dialog showed"
        );
        assert_eq!(
            engine.calls(),
            ["plugin.list", "plugin.list", "plugin.grant", "plugin.list"]
        );
    }

    /// #1004 review N3: when the enable after a grant fails because the plugin now asks
    /// for more than the dialog showed, the dialog opens again on what it asks for now,
    /// with why.
    #[test]
    fn a_failed_enable_after_a_grant_opens_the_dialog_on_the_update() {
        let engine = NotingEngine::new();
        *engine.reason.lock().expect("not poisoned") = Some(Reason::NeedsGrant);
        let section = section(&engine);
        section.open_grant(&PluginId::new("plg_timer"), None);
        // The update lands with no event reaching this client.
        engine.ask(asks_all());
        now(section.submit_grant(PluginGrant::default()));
        assert_eq!(
            engine.calls(),
            [
                "plugin.list",
                "plugin.grant",
                "plugin.enable",
                "plugin.list"
            ]
        );
        assert_eq!(shown(section).request, asks_all(), "open on the update");
        let problem = section.grant_problem.get_untracked().expect("why");
        assert!(problem.contains("`edit`"), "{problem}");
    }

    #[test]
    fn a_closed_dialog_holds_nothing_back() {
        let mut gate = ListGate::default();
        assert!(gate.may_list());
        gate.open();
        assert!(!gate.close(), "nothing was wanted while it was open");
        gate.sent();
        gate.sent();
        gate.answered();
        assert!(!gate.may_grant(), "one list is still on its way");
        gate.answered();
        assert!(gate.may_grant());
    }
}
