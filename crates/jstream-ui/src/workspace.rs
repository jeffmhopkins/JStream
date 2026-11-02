//! The workspace under the header: the preview canvas in the middle and the eight dock
//! zones around it, drawn from the [`Dock`] model. Panels are grouped and tabbed in the
//! zones, dragged by their tabs to another group, half or zone, resized with the
//! splitters and dividers, collapsed by their title and closed with a second click on
//! their ×. The dock is kept in the browser's storage, so the layout survives a reload,
//! and in the engine's UI settings (ADR-0084), so the desktop app and every browser of
//! one engine show the same layout.
//!
//! Docs: docs/UI-FEATURES.md#docking, crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::canvas::Canvas;
use crate::dock::{Dock, PanelId, Target, Zone};
use crate::pairing::Pairing;
use crate::panels::mixer::MixerFlip;
use crate::panels::{
    AgentActivity, Filters, Inspector, Mixer, Outputs, Scenes, Sources, StreamStats, StudioMode,
    Transform,
};
use crate::plugins::Plugins;
use crate::ui_settings::{self, DOCK_KEY, FirstStep, Kept, Theirs};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::time::Duration;

/// The dock, provided as context by the shell so the header's View and Layout menus can
/// change it.
#[derive(Clone, Copy)]
pub struct DockState(pub RwSignal<Dock>);

/// Where the browser keeps the dock.
const STORAGE_KEY: &str = "jstream.dock";
/// How far, in pixels, a tab moves before a press becomes a drag.
const DRAG_START: f64 = 4.0;
/// How long a close button stays armed.
const ARMED_FOR: Duration = Duration::from_millis(3000);
/// How long the dock stays unchanged before it is sent to the engine: dragging a
/// splitter changes it on every move.
const SHARE_AFTER: Duration = Duration::from_millis(500);

/// The dock the browser kept, or the default one.
pub fn restore() -> Dock {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten())
        .and_then(|text| Dock::from_json(&text))
        .unwrap_or_default()
}

/// Keeps `dock` in the browser's storage. Storage can be off or full; the dock then
/// lasts as long as the page.
fn keep(dock: &Dock) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(STORAGE_KEY, &dock.to_json());
    }
}

/// Whether the engine's server keeps the dock for this session, once it is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shared {
    /// `ui_settings.get` hasn't answered yet: nothing is sent, so the browser's copy never
    /// replaces a newer one the server holds.
    Asking,
    /// The server keeps it: changes are sent.
    Yes,
    /// Nothing keeps it (no server in front, or the call failed): the browser alone does.
    No,
}

/// Where the browser keeps the dock it last read from or sent to the server, so a
/// change the server never received is known on the next load (ADR-0084).
const SHARED_KEY: &str = "jstream.dock.shared";

/// The dock this browser last read from or sent to the server, if it kept one.
fn last_shared() -> Option<Value> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(SHARED_KEY).ok().flatten())
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Records `value` as the dock the server holds now.
fn keep_shared(value: &Value) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(SHARED_KEY, &value.to_string());
    }
}

/// Shares `dock` through the engine's UI settings (ADR-0084) while the workspace is
/// shown. When the server answers, [`ui_settings::first_step`] decides: show its dock,
/// send this browser's (when the server keeps none, or this browser holds a change the
/// server never received), or leave both alone. Each change after that is sent once the
/// dock has stayed the same for [`SHARE_AFTER`].
fn share(dock: RwSignal<Dock>) {
    let Some(session) = use_context::<SessionContext>().and_then(|context| context.get()) else {
        return;
    };
    let shared = StoredValue::new(Shared::Asking);
    // The value last read from or sent to the server, so a dock it sent isn't sent back.
    let last = StoredValue::new(None::<Value>);
    // Bumped by every change, so only the last of a run of changes is sent.
    let changes = StoredValue::new(0u64);
    let control = session.control.clone();
    let send = move |value: Value| {
        let control = control.clone();
        last.try_set_value(Some(value.clone()));
        spawn_local(async move {
            match ui_settings::set(&*control, DOCK_KEY, value.clone()).await {
                Ok(()) => keep_shared(&value),
                Err(error) => leptos::logging::warn!("the dock could not be shared: {error}"),
            }
        });
    };
    let read = session.control.clone();
    spawn_local({
        let send = send.clone();
        async move {
            let kept = match ui_settings::get(&*read, DOCK_KEY).await {
                Ok(Kept::Unserved) => {
                    shared.try_set_value(Shared::No);
                    return;
                }
                Ok(kept) => kept,
                Err(error) => {
                    leptos::logging::warn!("the shared dock could not be read: {error}");
                    shared.try_set_value(Shared::No);
                    return;
                }
            };
            let Some(ours) = dock.try_with_untracked(Dock::to_value) else {
                return;
            };
            let parsed = match &kept {
                Kept::Value(value) => Dock::from_value(value),
                _ => None,
            };
            let theirs = match (&kept, &parsed) {
                (Kept::Value(value), Some(_)) => Theirs::Readable(value),
                (Kept::Value(_), None) => Theirs::Unreadable,
                _ => Theirs::Nothing,
            };
            match ui_settings::first_step(theirs, &ours, last_shared().as_ref()) {
                FirstStep::Adopt => {
                    if let (Kept::Value(value), Some(theirs)) = (kept, parsed) {
                        last.try_set_value(Some(value));
                        let adopted = theirs.to_value();
                        dock.try_set(theirs);
                        keep_shared(&adopted);
                    }
                }
                FirstStep::Send => send(ours),
                FirstStep::Hold => {
                    // An unreadable dock stays until the person changes ours.
                    if matches!(kept, Kept::Value(_)) && parsed.is_some() {
                        keep_shared(&ours);
                    }
                    last.try_set_value(Some(ours));
                }
            }
            shared.try_set_value(Shared::Yes);
        }
    });
    Effect::new(move |_| {
        let value = dock.with(Dock::to_value);
        if shared.get_value() != Shared::Yes {
            return;
        }
        let Some(change) = changes.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        let send = send.clone();
        set_timeout(
            move || {
                let settled = changes.try_get_value() == Some(change);
                let unsent = last.try_with_value(|last| last.as_ref() != Some(&value));
                if settled && unsent == Some(true) {
                    send(value);
                }
            },
            SHARE_AFTER,
        );
    });
}

/// A panel whose engine side isn't there yet: its title and what it waits on.
fn waiting(title: &'static str, note: &'static str) -> AnyView {
    view! {
        <section class="panel waiting-panel">
            <h2>{title}</h2>
            <p class="placeholder">{note}</p>
        </section>
    }
    .into_any()
}

/// A panel's content.
fn panel_view(panel: PanelId) -> AnyView {
    match panel {
        PanelId::Scenes => view! { <Scenes /> }.into_any(),
        PanelId::Objects => view! { <Sources /> }.into_any(),
        PanelId::Inspector => view! { <Inspector /><Pairing /><Plugins /> }.into_any(),
        PanelId::Mixer => view! { <Mixer /> }.into_any(),
        PanelId::Outputs => view! { <Outputs /> }.into_any(),
        PanelId::Transitions => view! { <StudioMode /> }.into_any(),
        PanelId::Transform => view! { <Transform /> }.into_any(),
        PanelId::Filters => view! { <Filters /> }.into_any(),
        PanelId::Chat => waiting(
            "Live chat",
            "Live chat shows a YouTube broadcast's chat once JStream can sign in to YouTube, \
             which this build can't yet. Streaming with a stream key works without it.",
        ),
        PanelId::Stats => view! { <StreamStats /> }.into_any(),
        PanelId::Agent => view! { <AgentActivity /> }.into_any(),
    }
}

/// A tab press that may become a drag.
#[derive(Clone, Copy, Debug)]
struct Press {
    panel: PanelId,
    x: f64,
    y: f64,
}

/// What the workspace's parts share while it is shown.
#[derive(Clone, Copy)]
struct Ui {
    dock: RwSignal<Dock>,
    /// The panel being dragged, once a press has moved far enough.
    dragging: RwSignal<Option<PanelId>>,
    /// Where it would land now.
    over: RwSignal<Option<Target>>,
    /// The press that started it.
    press: StoredValue<Option<Press>>,
    /// Set while the click that ends a drag is still to come, so it isn't taken as a
    /// tab click.
    dragged: StoredValue<bool>,
    /// The panel whose close button was clicked once.
    armed: RwSignal<Option<PanelId>>,
}

/// Where a drop at `(x, y)` lands: the innermost drop area under the pointer, read
/// from its `data-drop` attributes.
fn target_at(x: f64, y: f64) -> Option<Target> {
    let element = document().element_from_point(x as f32, y as f32)?;
    let area = element.closest("[data-drop]").ok().flatten()?;
    let group = || area.get_attribute("data-group")?.parse::<u32>().ok();
    match area.get_attribute("data-drop")?.as_str() {
        "tabs" => Some(Target::Tabs(group()?)),
        "body" => {
            let rect = area.get_bounding_client_rect();
            let first = if area.get_attribute("data-vertical").as_deref() == Some("true") {
                y < rect.top() + rect.height() / 2.0
            } else {
                x < rect.left() + rect.width() / 2.0
            };
            let id = group()?;
            Some(if first {
                Target::Before(id)
            } else {
                Target::After(id)
            })
        }
        "zone" => {
            let key = area.get_attribute("data-zone")?;
            Zone::ALL
                .into_iter()
                .find(|zone| zone.key() == key)
                .map(Target::End)
        }
        _ => None,
    }
}

/// Tells the canvas its room changed: it measures itself on the window's `resize`.
fn nudge_canvas() {
    request_animation_frame(|| {
        if let Ok(event) = web_sys::Event::new("resize") {
            let _ = window().dispatch_event(&event);
        }
    });
}

/// The workspace: the zones around the canvas.
#[component]
pub fn Workspace() -> impl IntoView {
    let dock = expect_context::<DockState>().0;
    let ui = Ui {
        dock,
        dragging: RwSignal::new(None),
        over: RwSignal::new(None),
        press: StoredValue::new(None),
        dragged: StoredValue::new(false),
        armed: RwSignal::new(None),
    };

    // Keep the dock, and let the canvas re-fit, after every change.
    Effect::new(move |_| {
        dock.with(keep);
        nudge_canvas();
    });
    share(dock);

    let end_drag = move |drop: bool| {
        let panel = ui.dragging.get_untracked();
        let target = ui.over.get_untracked();
        ui.press.set_value(None);
        ui.dragging.set(None);
        ui.over.set(None);
        if let Some(panel) = panel {
            ui.dragged.set_value(true);
            set_timeout(move || ui.dragged.set_value(false), Duration::ZERO);
            if drop && let Some(target) = target {
                dock.update(|d| {
                    d.place(panel, target);
                });
            }
        }
    };
    let moves = window_event_listener(leptos::ev::pointermove, move |event| {
        let Some(press) = ui.press.get_value() else {
            return;
        };
        let (x, y) = (f64::from(event.client_x()), f64::from(event.client_y()));
        if ui.dragging.get_untracked().is_none() {
            if (x - press.x).hypot(y - press.y) < DRAG_START {
                return;
            }
            ui.dragging.set(Some(press.panel));
        }
        let target = target_at(x, y);
        if ui.over.get_untracked() != target {
            ui.over.set(target);
        }
    });
    let ups = window_event_listener(leptos::ev::pointerup, move |_| end_drag(true));
    let cancels = window_event_listener(leptos::ev::pointercancel, move |_| end_drag(false));
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        if event.key() == "Escape" && ui.press.get_value().is_some() {
            end_drag(false);
        }
    });
    on_cleanup(move || {
        moves.remove();
        ups.remove();
        cancels.remove();
        keys.remove();
    });

    let zone = move |zone: Zone| view! { <ZoneView ui=ui zone=zone /> };
    view! {
        <div class="workspace" class:dragging=move || ui.dragging.get().is_some()>
            {zone(Zone::OuterLeft)}
            <div class="ws-middle">
                {zone(Zone::Top)}
                <div class="ws-row">
                    {zone(Zone::Left)}
                    <div class="ws-centre">
                        {zone(Zone::CanvasTop)}
                        <section class="region canvas" aria-label="Canvas"><Canvas /></section>
                        {zone(Zone::CanvasBottom)}
                    </div>
                    {zone(Zone::Right)}
                </div>
                {zone(Zone::Bottom)}
            </div>
            {zone(Zone::OuterRight)}
        </div>
    }
}

/// One zone with its splitter, on the canvas side.
#[component]
fn ZoneView(ui: Ui, zone: Zone) -> impl IntoView {
    let dock = ui.dock;
    let ids =
        Memo::new(move |_| dock.with(|d| d.groups(zone).iter().map(|g| g.id).collect::<Vec<_>>()));
    let empty = move || ids.with(Vec::is_empty);
    let zone_ref = NodeRef::<html::Section>::new();
    let size = move || {
        if empty() {
            "44px".to_owned()
        } else {
            format!("{}px", dock.with(|d| d.size(zone)))
        }
    };
    let (width, height) = if zone.vertical() {
        (Some(size), None)
    } else {
        (None, Some(size))
    };
    let section = view! {
        <section
            class="zone"
            class:vertical=zone.vertical()
            class:horizontal=!zone.vertical()
            class:drop-zone=move || ui.over.get() == Some(Target::End(zone))
            aria-label=zone.label()
            data-drop="zone"
            data-zone=zone.key()
            hidden=move || empty() && ui.dragging.get().is_none()
            style:width=move || width.map(|w| w())
            style:height=move || height.map(|h| h())
            node_ref=zone_ref
        >
            <For each=move || ids.get() key=|id| *id let:id>
                <GroupView ui=ui zone=zone id=id zone_ref=zone_ref />
            </For>
            {move || empty().then(|| view! {
                <div class="zone-placeholder">{zone.placeholder()}</div>
            })}
        </section>
    };
    let splitter = view! { <Splitter ui=ui zone=zone hidden=Signal::derive(empty) /> };
    if zone.splitter_after() {
        view! { {section}{splitter} }.into_any()
    } else {
        view! { {splitter}{section} }.into_any()
    }
}

/// The bar that resizes a zone.
#[component]
fn Splitter(ui: Ui, zone: Zone, hidden: Signal<bool>) -> impl IntoView {
    let dock = ui.dock;
    // The pointer's start and the zone's size then, while a drag runs.
    let start = StoredValue::new(None::<(f64, f64, f64)>);
    let moved = StoredValue::new(false);
    let bar = NodeRef::<html::Div>::new();
    let end = move || {
        if start.get_value().is_some() && moved.get_value() {
            dock.update(Dock::resized);
        }
        start.set_value(None);
        moved.set_value(false);
    };
    view! {
        <div
            class="splitter"
            class:cols=zone.vertical()
            class:rows=!zone.vertical()
            role="separator"
            aria-label=zone.splitter_label()
            aria-orientation=if zone.vertical() { "vertical" } else { "horizontal" }
            hidden=move || hidden.get()
            node_ref=bar
            on:pointerdown=move |event| {
                if event.button() != 0 {
                    return;
                }
                event.prevent_default();
                if let Some(bar) = bar.get_untracked() {
                    let _captured = bar.set_pointer_capture(event.pointer_id());
                }
                let from = dock.with_untracked(|d| d.size(zone));
                start.set_value(Some((f64::from(event.client_x()), f64::from(event.client_y()), from)));
            }
            on:pointermove=move |event| {
                let Some((x, y, from)) = start.get_value() else {
                    return;
                };
                if event.buttons() == 0 {
                    end();
                    return;
                }
                moved.set_value(true);
                let (dx, dy) = (f64::from(event.client_x()) - x, f64::from(event.client_y()) - y);
                dock.update(|d| d.resize_zone(zone, from, dx, dy));
            }
            on:pointerup=move |_| end()
            on:pointercancel=move |_| end()
            on:lostpointercapture=move |_| end()
        ></div>
    }
}

/// One group: a divider before it unless it is first, its title or tabs, and its panels.
#[component]
fn GroupView(ui: Ui, zone: Zone, id: u32, zone_ref: NodeRef<html::Section>) -> impl IntoView {
    let dock = ui.dock;
    let group = Memo::new(move |_| dock.with(|d| d.group(id).cloned()));
    let tabs =
        Memo::new(move |_| group.with(|g| g.as_ref().map(|g| g.tabs.clone()).unwrap_or_default()));
    let active = move || group.with(|g| g.as_ref().map(|g| g.active));
    let collapsed = move || group.with(|g| g.as_ref().is_some_and(|g| g.collapsed));
    let multi = move || tabs.with(|t| t.len() > 1);
    let first = Memo::new(move |_| dock.with(|d| d.place_of(id).is_none_or(|p| p.index == 0)));
    let flex = move || {
        if collapsed() {
            "0 0 auto".to_owned()
        } else {
            let weight = group.with(|g| g.as_ref().map_or(1.0, |g| g.weight));
            format!("{weight} 1 0px")
        }
    };
    let over = move |target: Target| ui.over.get() == Some(target);

    let tab_strip = move || {
        view! {
            <For each=move || tabs.get() key=|panel| *panel let:panel>
                <span class="tab-wrap">
                    <button
                        class="dock-tab"
                        class:active=move || active() == Some(panel)
                        class:underline=move || multi() && active() == Some(panel)
                        role="tab"
                        aria-selected=move || if active() == Some(panel) { "true" } else { "false" }
                        title=move || if multi() {
                            "Drag to move · drop on another tab strip to group"
                        } else {
                            "Click to collapse or expand · drag to move · drop on another tab strip to group"
                        }
                        on:pointerdown=move |event| {
                            if event.button() == 0 {
                                ui.press.set_value(Some(Press {
                                    panel,
                                    x: f64::from(event.client_x()),
                                    y: f64::from(event.client_y()),
                                }));
                            }
                        }
                        on:click=move |_| {
                            if !ui.dragged.get_value() {
                                dock.update(|d| d.click_tab(id, panel));
                            }
                        }
                    >
                        {panel.title()}
                    </button>
                    {move || multi().then(|| view! { <CloseButton ui=ui panel=panel small=true /> })}
                </span>
            </For>
        }
    };

    view! {
        {move || (!first.get()).then(|| view! { <Divider ui=ui zone=zone id=id zone_ref=zone_ref /> })}
        <div
            class="group"
            class:collapsed=collapsed
            style:flex=flex
        >
            <div
                class="dock-strip"
                class:drop-target=move || over(Target::Tabs(id))
                role="tablist"
                data-drop="tabs"
                data-group=id.to_string()
            >
                <div class="tabs-row">{tab_strip}</div>
                {move || (active() == Some(PanelId::Mixer)).then(|| view! { <MixerFlip /> })}
                {move || (!multi()).then(|| {
                    active().map(|panel| view! { <CloseButton ui=ui panel=panel small=false /> })
                })}
            </div>
            <div
                class="group-body"
                class:drop-before=move || over(Target::Before(id))
                class:drop-after=move || over(Target::After(id))
                data-drop="body"
                data-group=id.to_string()
                data-vertical=if zone.vertical() { "true" } else { "false" }
            >
                <For each=move || tabs.get() key=|panel| *panel let:panel>
                    <div
                        class=format!("dock-panel panel-{}", panel.key())
                        role="tabpanel"
                        aria-label=panel.title()
                        hidden=move || active() != Some(panel)
                    >
                        {panel_view(panel)}
                    </div>
                </For>
            </div>
        </div>
    }
}

/// A panel's ×: the first click arms it, a second closes the panel. It disarms after a
/// few seconds or when the pointer leaves.
#[component]
fn CloseButton(ui: Ui, panel: PanelId, small: bool) -> impl IntoView {
    let armed = move || ui.armed.get() == Some(panel);
    let title = panel.title();
    view! {
        <button
            class=if small { "tab-close" } else { "strip-close" }
            class:armed=armed
            aria-label=format!("Close {title}")
            title=move || if armed() {
                format!("Click again to close {title}")
            } else {
                format!("Close {title} (click twice)")
            }
            on:pointerdown=move |event| event.stop_propagation()
            on:click=move |_| {
                if ui.armed.get_untracked() == Some(panel) {
                    ui.armed.set(None);
                    ui.dock.update(|d| d.close(panel));
                } else {
                    ui.armed.set(Some(panel));
                    set_timeout(
                        move || {
                            if ui.armed.try_get_untracked().flatten() == Some(panel) {
                                ui.armed.try_set(None);
                            }
                        },
                        ARMED_FOR,
                    );
                }
            }
            on:pointerleave=move |_| {
                if ui.armed.get_untracked() == Some(panel) {
                    ui.armed.set(None);
                }
            }
        >
            "×"
        </button>
    }
}

/// The handle between two groups, trading space between them.
#[component]
fn Divider(ui: Ui, zone: Zone, id: u32, zone_ref: NodeRef<html::Section>) -> impl IntoView {
    let dock = ui.dock;
    // The pointer's start, the divider's index, the two weights then and the zone's
    // length, while a drag runs.
    let start = StoredValue::new(None::<(f64, usize, (f64, f64), f64)>);
    let moved = StoredValue::new(false);
    let handle = NodeRef::<html::Button>::new();
    let along = move |event: &leptos::ev::PointerEvent| {
        f64::from(if zone.vertical() {
            event.client_y()
        } else {
            event.client_x()
        })
    };
    let end = move || {
        if start.get_value().is_some() && moved.get_value() {
            dock.update(Dock::resized);
        }
        start.set_value(None);
        moved.set_value(false);
    };
    view! {
        <button
            class="divider"
            class:cols=!zone.vertical()
            class:rows=zone.vertical()
            aria-label="Resize between panels"
            node_ref=handle
            on:pointerdown=move |event| {
                if event.button() != 0 {
                    return;
                }
                let Some(place) = dock.with_untracked(|d| d.place_of(id)) else {
                    return;
                };
                let weights = dock.with_untracked(|d| {
                    let groups = d.groups(zone);
                    (groups[place.index - 1].weight, groups[place.index].weight)
                });
                let length = zone_ref.get_untracked().map_or(0.0, |z| {
                    let rect = z.get_bounding_client_rect();
                    if zone.vertical() { rect.height() } else { rect.width() }
                });
                if let Some(handle) = handle.get_untracked() {
                    let _captured = handle.set_pointer_capture(event.pointer_id());
                }
                start.set_value(Some((along(&event), place.index, weights, length)));
            }
            on:pointermove=move |event| {
                let Some((from, index, weights, length)) = start.get_value() else {
                    return;
                };
                if event.buttons() == 0 {
                    end();
                    return;
                }
                moved.set_value(true);
                let delta = along(&event) - from;
                dock.update(|d| d.resize_between(zone, index, weights, delta, length));
            }
            on:pointerup=move |_| end()
            on:pointercancel=move |_| end()
            on:lostpointercapture=move |_| end()
        ></button>
    }
}
