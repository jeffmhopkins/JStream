//! The header bar along the top of the shell (docs/UI-FEATURES.md#header): the brand,
//! the File, Edit, View and Layout menus, the stats readout (frame rate, dropped frames
//! and CPU), Studio mode,
//! Record, and Go Live / End stream. The View and Layout menus change the workspace's
//! dock (`workspace.rs`), and the View menu opens the program and multiview projectors
//! (`projector.rs`). It keeps its own copy of the outputs, like the outputs panel does, and reports
//! a failed call in the shell's problem bar.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header, docs/UI-FEATURES.md#docking

use crate::app::{SessionContext, SessionInfo};
use crate::canvas::model::{history, is_history_empty};
use crate::dock::PanelId;
use crate::menu::{move_focus, step};
use crate::panels::outputs::{
    OutputRow, Vcam, go_live_targets, is_running, live_outputs, load, start_blocker,
    start_or_stop_all,
};
use crate::projector::{self, Request, Target};
use crate::store::Studio;
use crate::workspace::DockState;
use futures_util::StreamExt;
use jstream_control::protocol::{DropCounts, EngineLoad, Event, HistoryOp, OutputId, OutputKind};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;

/// The recording outputs Record starts: every recording that is stopped or failed.
pub fn record_targets<'a>(rows: impl IntoIterator<Item = &'a OutputRow>) -> Vec<OutputId> {
    rows.into_iter()
        .filter(|row| matches!(row.info.kind, OutputKind::Record { .. }))
        .filter(|row| start_blocker(row, Vcam::Unknown).is_none())
        .map(|row| row.info.output_id.clone())
        .collect()
}

/// The recording outputs that run: what "Stop recording" stops.
pub fn recording<'a>(rows: impl IntoIterator<Item = &'a OutputRow>) -> Vec<OutputId> {
    rows.into_iter()
        .filter(|row| matches!(row.info.kind, OutputKind::Record { .. }))
        .filter(|row| is_running(row.state))
        .map(|row| row.info.output_id.clone())
        .collect()
}

/// What the Record button does when pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordButton {
    /// No recording output exists: the button is disabled.
    NoRecording,
    /// Recordings run: pressing stops them.
    Stop(Vec<OutputId>),
    /// None runs: pressing starts these.
    Start(Vec<OutputId>),
}

impl RecordButton {
    /// The button for these outputs.
    pub fn of(rows: &[OutputRow]) -> RecordButton {
        let running = recording(rows);
        if !running.is_empty() {
            return RecordButton::Stop(running);
        }
        let targets = record_targets(rows);
        if targets.is_empty() {
            RecordButton::NoRecording
        } else {
            RecordButton::Start(targets)
        }
    }
}

/// Frames dropped since the header first heard `stats.tick`, from its running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// The totals the count starts from, and the last tick's `seq` and totals.
    base: Option<(DropCounts, u64, DropCounts)>,
}

impl Dropped {
    /// Takes one `stats.tick`. A `seq` that doesn't increase, or a total that shrank,
    /// means the engine restarted: the count starts over from this tick.
    pub fn observe(&mut self, seq: u64, drops: DropCounts) {
        self.base = match self.base {
            Some((base, last_seq, last)) if seq > last_seq && !shrank(drops, last) => {
                Some((base, seq, drops))
            }
            _ => Some((drops, seq, drops)),
        };
    }

    /// Video frames dropped anywhere in the pipeline since the count started, or `None`
    /// before the first tick.
    pub fn frames(&self) -> Option<u64> {
        self.base
            .map(|(base, _, last)| frames(last).saturating_sub(frames(base)))
    }
}

/// The program's frame rate and the engine's CPU use over the last `stats.tick`
/// interval, from the two ticks' loads (protocol 0.48).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// The last tick's `seq` and load.
    last: Option<(u64, EngineLoad)>,
    /// Frames a second the program drew, rounded, or `None` while it drew none.
    fps: Option<u64>,
    /// The share of every logical CPU the engine's process used, in tenths of a
    /// percent, or `None` when the engine doesn't read it.
    cpu: Option<u64>,
}

impl Load {
    /// Takes one `stats.tick`'s load. A tick without one (an engine before 0.48)
    /// clears the readout. A `seq` that doesn't increase, or a clock or total that went
    /// back, means the engine restarted: the next tick gives the next rates.
    pub fn observe(&mut self, seq: u64, load: Option<EngineLoad>) {
        let Some(now) = load else {
            *self = Self::default();
            return;
        };
        (self.fps, self.cpu) = match self.last {
            Some((last_seq, before))
                if seq > last_seq
                    && now.elapsed_ms > before.elapsed_ms
                    && now.program_frames >= before.program_frames =>
            {
                rates(before, now)
            }
            _ => (None, None),
        };
        self.last = Some((seq, now));
    }

    /// "60 fps", while the program draws.
    pub fn fps(&self) -> Option<String> {
        self.fps.map(|fps| format!("{fps} fps"))
    }

    /// "CPU 3%", or "CPU <1%" for a share that rounds to nothing.
    pub fn cpu(&self) -> Option<String> {
        self.cpu.map(|tenths| match tenths {
            1..=4 => "CPU <1%".to_owned(),
            _ => format!("CPU {}%", rounded(tenths, 10)),
        })
    }
}

/// The frame rate and CPU share between two loads, `before` strictly earlier, in
/// whole frames a second and tenths of a percent.
fn rates(before: EngineLoad, now: EngineLoad) -> (Option<u64>, Option<u64>) {
    let ms = now.elapsed_ms - before.elapsed_ms;
    let frames = now.program_frames - before.program_frames;
    let fps = (frames > 0).then(|| rounded(frames.saturating_mul(1_000), ms));
    let cpu = match (before.cpu_ms, now.cpu_ms) {
        (Some(before_ms), Some(now_ms)) if now_ms >= before_ms && now.cpus > 0 => Some(rounded(
            (now_ms - before_ms).saturating_mul(1_000),
            ms.saturating_mul(u64::from(now.cpus)),
        )),
        _ => None,
    };
    (fps, cpu)
}

/// `n / d` rounded to the nearest whole number, `d` above zero.
fn rounded(n: u64, d: u64) -> u64 {
    n.saturating_add(d / 2) / d
}

/// The video frames among `drops`: captured, rendered and encoded frames.
fn frames(drops: DropCounts) -> u64 {
    drops
        .capture_frames
        .saturating_add(drops.render_ticks)
        .saturating_add(drops.encode_frames)
}

/// Whether any total in `now` is below `before`'s.
fn shrank(now: DropCounts, before: DropCounts) -> bool {
    now.capture_frames < before.capture_frames
        || now.audio_frames < before.audio_frames
        || now.render_ticks < before.render_ticks
        || now.encode_frames < before.encode_frames
        || now.encode_blocks < before.encode_blocks
        || now.output_packets < before.output_packets
}

/// Where the browser keeps whether studio mode is on.
const STUDIO_MODE_KEY: &str = "jstream.studio-mode";

/// Whether the browser kept studio mode on. Storage can be off; it is then off.
pub(crate) fn kept_studio_mode() -> bool {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(STUDIO_MODE_KEY).ok().flatten())
        .is_some_and(|value| value == "on")
}

/// Calls `changed` with studio mode each time another window of this browser (the
/// workspace, seen from a projector) keeps a new value, until the calling component is
/// cleaned up.
pub(crate) fn on_kept_studio_mode(changed: impl Fn(bool) + 'static) {
    let listener = window_event_listener(leptos::ev::storage, move |event| {
        if event.key().as_deref() == Some(STUDIO_MODE_KEY) {
            changed(event.new_value().as_deref() == Some("on"));
        }
    });
    on_cleanup(move || listener.remove());
}

/// Keeps studio mode in the browser's storage, when it has storage.
fn keep_studio_mode(on: bool) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(STUDIO_MODE_KEY, if on { "on" } else { "off" });
    }
}

/// A top menu of the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    File,
    Edit,
    View,
    Layout,
}

impl Menu {
    /// The menus along the bar, left to right: View and Layout only with a dock.
    fn bar(dock: bool) -> &'static [Menu] {
        if dock {
            &[Menu::File, Menu::Edit, Menu::View, Menu::Layout]
        } else {
            &[Menu::File, Menu::Edit]
        }
    }

    /// The menu `delta` places along `bar` from this one, wrapping at both ends.
    fn beside(self, bar: &[Menu], delta: isize) -> Menu {
        let at = bar.iter().position(|menu| *menu == self);
        step(bar.len(), at, delta)
            .and_then(|n| bar.get(n).copied())
            .unwrap_or(self)
    }

    /// Its name in the page, which finds its button and dropdown.
    fn key(self) -> &'static str {
        match self {
            Menu::File => "file",
            Menu::Edit => "edit",
            Menu::View => "view",
            Menu::Layout => "layout",
        }
    }
}

/// The element of `menu` that `part` selects, such as its button or its dropdown.
fn menu_part(menu: Menu, part: &str) -> Option<leptos::web_sys::HtmlElement> {
    let selector = format!(
        ".app-header .top-menu[data-menu=\"{}\"] > {part}",
        menu.key()
    );
    document()
        .query_selector(&selector)
        .ok()
        .flatten()?
        .dyn_into()
        .ok()
}

/// Focuses the first (`true`) or last enabled item of `menu`'s dropdown, once it is drawn.
fn focus_item(menu: Menu, first: bool) {
    request_animation_frame(move || {
        if let Some(dropdown) = menu_part(menu, ".dropdown") {
            move_focus(&dropdown, 0, Some(first));
        }
    });
}

/// Focuses `menu`'s button.
fn focus_button(menu: Menu) {
    if let Some(button) = menu_part(menu, ".menu-button") {
        let _ = button.focus();
    }
}

/// The header bar.
#[component]
pub fn Header(
    /// The open session's details, which the File menu shows.
    info: RwSignal<SessionInfo>,
    /// Ends the session.
    disconnect: impl Fn() + 'static,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let rows = RwSignal::new(Vec::<OutputRow>::new());
    let dropped = RwSignal::new(Dropped::default());
    let engine_load = RwSignal::new(Load::default());
    let open = RwSignal::new(None::<Menu>);
    let loads = StoredValue::new(0_u64);
    let disconnect = StoredValue::new_local(disconnect);
    let dock = use_context::<DockState>().map(|d| d.0);
    studio.studio_mode.set(kept_studio_mode());

    let reload = move || {
        let Some(session) = session.get() else {
            return;
        };
        let Some(mine) = loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if loads.try_get_value() != Some(mine) {
                return;
            }
            match loaded {
                Ok(snapshot) => {
                    rows.try_set(snapshot.outputs);
                }
                Err(problem) => {
                    studio.problem.try_set(Some(problem));
                }
            }
        });
    };

    // A stream key changed, which no event reports: re-read whether Go Live can start.
    Effect::new(move |previous: Option<u64>| {
        let now = studio.keys_changed.get();
        if previous.is_some() {
            reload();
        }
        now
    });

    if let Some(current) = session.get() {
        let mut events = current.control.events();
        drop(current);
        reload();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. } | Event::EventsDropped { .. } => reload(),
                    Event::OutputStateChanged { output_id, state } => {
                        let known = rows.try_update(|rows| {
                            rows.iter_mut()
                                .find(|row| row.info.output_id == output_id)
                                .map(|row| row.state = state)
                                .is_some()
                        });
                        if known == Some(false) {
                            reload();
                        }
                    }
                    Event::StatsTick {
                        seq,
                        drops,
                        load: now,
                    } => {
                        dropped.try_update(|d| d.observe(seq, drops));
                        engine_load.try_update(|l| l.observe(seq, now));
                    }
                    _ => {}
                }
            }
        });
    }

    // Starts or stops `outputs`, reporting every failure in the problem bar.
    let start_or_stop = move |outputs: Vec<OutputId>, start: bool| {
        let Some(session) = session.get() else {
            return;
        };
        spawn_local(async move {
            let outcome = start_or_stop_all(&*session.control, outputs, start).await;
            if let Err(problem) = outcome {
                studio.problem.try_set(Some(problem));
            }
        });
    };
    let undo_or_redo = move |op: HistoryOp| {
        open.set(None);
        let Some(session) = session.get() else {
            return;
        };
        spawn_local(async move {
            if let Err(error) = history(&*session.control, op).await {
                let problem = if is_history_empty(&error) {
                    match op {
                        HistoryOp::Redo => "Nothing to redo.".to_owned(),
                        _ => "Nothing to undo.".to_owned(),
                    }
                } else {
                    error.to_string()
                };
                studio.problem.try_set(Some(problem));
            }
        });
    };

    let targets = move || rows.with(|rows| go_live_targets(rows.iter()));
    let live = move || rows.with(|rows| live_outputs(rows.iter()));
    let record = move || rows.with(|rows| RecordButton::of(rows));
    // Escape closes an open menu wherever focus is: WebKit doesn't focus a clicked button,
    // so the key may not reach the header.
    let escape = window_event_listener(leptos::ev::keydown, move |event| {
        if event.key() == "Escape" && open.get_untracked().is_some() {
            open.set(None);
        }
    });
    on_cleanup(move || escape.remove());
    // The bar behaves like a desktop menu bar: a click opens or closes a menu; while one is
    // open, pointing at another opens that one instead; the arrow keys move along the bar
    // and through the open menu.
    let bar = Menu::bar(dock.is_some());
    let toggle =
        move |menu: Menu| open.update(|open| *open = (*open != Some(menu)).then_some(menu));
    // Focus moves to the menu's button at once and to its item once the dropdown is drawn:
    // the dropdown that held focus goes now, and a key pressed before the next frame must
    // still reach the bar rather than the page.
    let open_at = move |menu: Menu, first: bool| {
        open.set(Some(menu));
        focus_button(menu);
        focus_item(menu, first);
    };
    // A key with Ctrl, Alt or Meta is left to the hotkeys: only plain keys drive the bar.
    let modified =
        |event: &leptos::ev::KeyboardEvent| event.ctrl_key() || event.alt_key() || event.meta_key();
    let button_keys = move |menu: Menu| {
        move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
            _ if modified(&event) => {}
            "ArrowDown" | "Enter" | " " => {
                event.prevent_default();
                open_at(menu, true);
            }
            "ArrowUp" => {
                event.prevent_default();
                open_at(menu, false);
            }
            key @ ("ArrowLeft" | "ArrowRight") => {
                event.prevent_default();
                let next = menu.beside(bar, if key == "ArrowLeft" { -1 } else { 1 });
                if open.get_untracked().is_some() {
                    open_at(next, true);
                } else {
                    focus_button(next);
                }
            }
            _ => {}
        }
    };
    let dropdown_keys = move |menu: Menu| {
        move |event: leptos::ev::KeyboardEvent| {
            if modified(&event) {
                return;
            }
            let (delta, to) = match event.key().as_str() {
                "ArrowDown" => (1, None),
                "ArrowUp" => (-1, None),
                "Home" => (0, Some(true)),
                "End" => (0, Some(false)),
                key @ ("ArrowLeft" | "ArrowRight") => {
                    event.prevent_default();
                    open_at(
                        menu.beside(bar, if key == "ArrowLeft" { -1 } else { 1 }),
                        true,
                    );
                    return;
                }
                "Escape" => {
                    event.prevent_default();
                    event.stop_propagation();
                    open.set(None);
                    focus_button(menu);
                    return;
                }
                "Tab" => {
                    open.set(None);
                    return;
                }
                _ => return,
            };
            event.prevent_default();
            if let Some(dropdown) = event
                .current_target()
                .and_then(|target| target.dyn_into::<leptos::web_sys::Element>().ok())
            {
                move_focus(&dropdown, delta, to);
            }
        }
    };
    let menu_button = move |menu: Menu, label: AnyView| {
        view! {
            <button
                class="menu-button"
                class:open=move || open.get() == Some(menu)
                aria-haspopup="menu"
                aria-expanded=move || if open.get() == Some(menu) { "true" } else { "false" }
                on:click=move |_| toggle(menu)
                on:pointerenter=move |_| {
                    if open.get_untracked().is_some_and(|other| other != menu) {
                        open.set(Some(menu));
                        focus_button(menu);
                    }
                }
                on:keydown=button_keys(menu)
            >
                {label}
            </button>
        }
    };

    view! {
        <header
            class="app-header"
            data-rev=move || info.with(|i| i.rev.clone())
        >
            <span class="brand"><span class="brand-mark"></span>"JStream"</span>
            {move || open.get().map(|_| view! {
                <div class="menu-catcher" on:pointerdown=move |_| open.set(None)></div>
            })}
            <div class="top-menu" data-menu=Menu::File.key()>
                {menu_button(Menu::File, "File".into_any())}
                <Show when=move || open.get() == Some(Menu::File)>
                    <div class="dropdown" role="menu" aria-label="File" on:keydown=dropdown_keys(Menu::File)>
                        <p class="menu-head">"CONNECTION"</p>
                        <dl class="connection">
                            <dt>"Engine"</dt>
                            <dd>{move || info.with(|i| i.url.clone())}</dd>
                            <dt>"Protocol"</dt>
                            <dd>{move || info.with(|i| i.protocol.clone())}</dd>
                            <dt>"Client"</dt>
                            <dd>{move || info.with(|i| i.client_id.clone())}</dd>
                            <dt>"Revision"</dt>
                            <dd>{move || info.with(|i| i.rev.clone())}</dd>
                        </dl>
                        <div class="menu-sep"></div>
                        <button class="menuitem" role="menuitem" on:click=move |_| disconnect.with_value(|d| d())>
                            "Disconnect"
                        </button>
                    </div>
                </Show>
            </div>
            <div class="top-menu" data-menu=Menu::Edit.key()>
                {menu_button(Menu::Edit, "Edit".into_any())}
                <Show when=move || open.get() == Some(Menu::Edit)>
                    <div class="dropdown" role="menu" aria-label="Edit" on:keydown=dropdown_keys(Menu::Edit)>
                        <button
                            class="menuitem"
                            role="menuitem"
                            on:click=move |_| undo_or_redo(HistoryOp::Undo)
                        >
                            <span>"Undo"</span>
                            <span class="hint">"Ctrl+Z"</span>
                        </button>
                        <button
                            class="menuitem"
                            role="menuitem"
                            on:click=move |_| undo_or_redo(HistoryOp::Redo)
                        >
                            <span>"Redo"</span>
                            <span class="hint">"Ctrl+Shift+Z"</span>
                        </button>
                    </div>
                </Show>
            </div>
            {dock.map(|dock| view! {
                <div class="top-menu" data-menu=Menu::View.key()>
                    {menu_button(Menu::View, "View".into_any())}
                    <Show when=move || open.get() == Some(Menu::View)>
                        <div class="dropdown" role="menu" aria-label="View" on:keydown=dropdown_keys(Menu::View)>
                            <p class="menu-head">"PANELS"</p>
                            {PanelId::ALL.into_iter().map(|panel| {
                                let shown = move || dock.with(|d| d.shown(panel));
                                view! {
                                    <button
                                        class="menuitem"
                                        role="menuitemcheckbox"
                                        aria-checked=move || if shown() { "true" } else { "false" }
                                        on:click=move |_| dock.update(|d| d.toggle(panel))
                                    >
                                        <span>{panel.title()}</span>
                                        <span class="mark" class:add=move || !shown()>
                                            {move || if shown() { "shown" } else { "add" }}
                                        </span>
                                    </button>
                                }
                            }).collect_view()}
                            <div class="menu-sep"></div>
                            <p class="menu-head">"PROJECTORS"</p>
                            {[
                                ("Program in a window", Target::Program, false),
                                ("Program full screen", Target::Program, true),
                                ("Multiview in a window", Target::Multiview, false),
                                ("Multiview full screen", Target::Multiview, true),
                            ].into_iter().map(|(label, target, full)| {
                                let request = Request { target, full };
                                view! {
                                    <button
                                        class="menuitem"
                                        role="menuitem"
                                        on:click=move |_| {
                                            open.set(None);
                                            projector::open_and_report(studio, &request);
                                        }
                                    >
                                        {label}
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                    </Show>
                </div>
                <div class="top-menu" data-menu=Menu::Layout.key()>
                    {menu_button(Menu::Layout, view! {
                        "Layout: "
                        <span class="layout-name">{move || dock.with(|d| d.current.clone())}</span>
                        <span class="caret">"▾"</span>
                    }.into_any())}
                    <Show when=move || open.get() == Some(Menu::Layout)>
                        <div class="dropdown layouts" role="menu" aria-label="Layout" on:keydown=dropdown_keys(Menu::Layout)>
                            <p class="menu-head">"SAVED LAYOUTS"</p>
                            {move || dock.with(|d| d.saved.iter().map(|l| l.name.clone()).collect::<Vec<_>>())
                                .into_iter()
                                .map(|name| {
                                    let current = {
                                        let name = name.clone();
                                        move || dock.with(|d| d.current == name)
                                    };
                                    let pick = name.clone();
                                    view! {
                                        <button
                                            class="menuitem"
                                            role="menuitem"
                                            on:click=move |_| {
                                                dock.update(|d| d.apply(&pick));
                                                open.set(None);
                                            }
                                        >
                                            <span>{name}</span>
                                            {move || current().then(|| view! { <span class="mark add">"current"</span> })}
                                        </button>
                                    }
                                })
                                .collect_view()}
                            <div class="menu-sep"></div>
                            <button
                                class="menuitem link"
                                role="menuitem"
                                on:click=move |_| {
                                    dock.update(|d| d.save_as_new());
                                    open.set(None);
                                }
                            >
                                "Save current layout as new"
                            </button>
                            <button
                                class="menuitem"
                                role="menuitem"
                                on:click=move |_| {
                                    dock.update(|d| d.reset());
                                    open.set(None);
                                }
                            >
                                "Reset to default"
                            </button>
                        </div>
                    </Show>
                </div>
            })}
            <span class="spacer"></span>
            <span
                class="readout"
                title="The program's frame rate while an output runs, video frames dropped \
                       since this page connected, and the share of all CPUs JStream uses"
            >
                {move || {
                    let (fps, cpu) = engine_load.with(|l| (l.fps(), l.cpu()));
                    let dropped = dropped.with(|d| d.frames()).map(|n| format!("{n} dropped"));
                    [fps, dropped, cpu].into_iter().flatten().collect::<Vec<_>>().join(" · ")
                }}
            </span>
            <button
                class="header-button studio-mode"
                class:on=move || studio.studio_mode.get()
                aria-pressed=move || studio.studio_mode.get().to_string()
                title=move || if studio.studio_mode.get() {
                    "Studio mode is on: a clicked scene goes on preview. Click to turn it off."
                } else {
                    "Turn studio mode on: set up a scene on preview, then take it to program."
                }
                on:click=move |_| {
                    let on = !studio.studio_mode.get_untracked();
                    studio.studio_mode.set(on);
                    keep_studio_mode(on);
                }
            >
                "Studio mode"
            </button>
            <button
                class="header-button"
                class:on=move || matches!(record(), RecordButton::Stop(_))
                disabled=move || record() == RecordButton::NoRecording
                title=move || match record() {
                    RecordButton::NoRecording => "Add a recording output first",
                    RecordButton::Stop(_) => "Stop every recording",
                    RecordButton::Start(_) => "Start every recording output",
                }
                on:click=move |_| match record() {
                    RecordButton::Stop(ids) => start_or_stop(ids, false),
                    RecordButton::Start(ids) => start_or_stop(ids, true),
                    RecordButton::NoRecording => {}
                }
            >
                {move || match record() {
                    RecordButton::Stop(_) => "Stop recording",
                    _ => "Record",
                }}
            </button>
            {move || if live().is_empty() {
                view! {
                    <button
                        class="go-live"
                        disabled=move || targets().is_empty()
                        title="Starts every stopped stream output that has its key set"
                        on:click=move |_| start_or_stop(targets(), true)
                    >
                        "Go Live"
                    </button>
                }
                .into_any()
            } else {
                view! {
                    <button class="go-live live" on:click=move |_| start_or_stop(live(), false)>
                        "End stream"
                    </button>
                }
                .into_any()
            }}
        </header>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{OutputInfo, OutputState, RecordContainer, RtmpTarget};

    fn row(id: &str, kind: OutputKind, state: OutputState) -> OutputRow {
        OutputRow {
            info: OutputInfo {
                output_id: OutputId::new(id),
                kind,
                encoder_profile: None,
                filters: Default::default(),
                video_mix: None,
            },
            state,
            key_set: true,
            cleartext: false,
        }
    }

    fn record(id: &str, state: OutputState) -> OutputRow {
        row(
            id,
            OutputKind::Record {
                path: "/tmp/show.mp4".into(),
                container: RecordContainer::Mp4,
            },
            state,
        )
    }

    fn stream(id: &str, state: OutputState) -> OutputRow {
        row(
            id,
            OutputKind::Rtmp {
                target: RtmpTarget::Generic {
                    url: "rtmp://127.0.0.1/live".into(),
                },
            },
            state,
        )
    }

    #[test]
    fn record_starts_stopped_recordings_and_stops_running_ones() {
        assert_eq!(RecordButton::of(&[]), RecordButton::NoRecording);
        assert_eq!(
            RecordButton::of(&[stream("a", OutputState::Stopped)]),
            RecordButton::NoRecording,
            "a stream is not a recording"
        );
        let rows = [
            record("r1", OutputState::Stopped),
            record("r2", OutputState::Failed),
            stream("s", OutputState::Stopped),
        ];
        let ids =
            |names: &[&str]| -> Vec<OutputId> { names.iter().map(|n| OutputId::new(*n)).collect() };
        assert_eq!(
            RecordButton::of(&rows),
            RecordButton::Start(ids(&["r1", "r2"]))
        );
        let rows = [
            record("r1", OutputState::Active),
            record("r2", OutputState::Stopped),
        ];
        assert_eq!(
            RecordButton::of(&rows),
            RecordButton::Stop(ids(&["r1"])),
            "while one records, the button stops it"
        );
    }

    #[test]
    fn the_arrow_keys_move_along_the_bar_and_wrap() {
        let bar = Menu::bar(true);
        assert_eq!(Menu::File.beside(bar, 1), Menu::Edit);
        assert_eq!(Menu::Layout.beside(bar, 1), Menu::File);
        assert_eq!(Menu::File.beside(bar, -1), Menu::Layout);
        // Without a dock the bar has no View or Layout.
        let bar = Menu::bar(false);
        assert_eq!(Menu::Edit.beside(bar, 1), Menu::File);
        assert_eq!(Menu::File.beside(bar, -1), Menu::Edit);
    }

    #[test]
    fn the_load_gives_the_frame_rate_and_cpu_between_two_ticks() {
        let at = |elapsed_ms, program_frames, cpu_ms| EngineLoad {
            elapsed_ms,
            program_frames,
            cpu_ms,
            cpus: 4,
        };
        let mut l = Load::default();
        l.observe(1, Some(at(1_000, 0, Some(100))));
        assert_eq!((l.fps(), l.cpu()), (None, None), "one tick has no rate");
        // 60 frames and 120 ms of CPU over a second on 4 CPUs.
        l.observe(2, Some(at(2_000, 60, Some(220))));
        assert_eq!(l.fps().as_deref(), Some("60 fps"));
        assert_eq!(l.cpu().as_deref(), Some("CPU 3%"));
        // Idle: no frames drawn, a little CPU, and a host that reads none.
        l.observe(3, Some(at(3_000, 60, Some(230))));
        assert_eq!((l.fps(), l.cpu().as_deref()), (None, Some("CPU <1%")));
        l.observe(4, Some(at(4_000, 60, None)));
        assert_eq!(l.cpu(), None);
        // A missed tick still gives the average over the gap.
        l.observe(6, Some(at(6_000, 120, None)));
        assert_eq!(l.fps().as_deref(), Some("30 fps"));
        // An engine restart, or one without a load, clears the readout.
        l.observe(1, Some(at(10, 0, Some(1))));
        assert_eq!((l.fps(), l.cpu()), (None, None));
        l.observe(2, None);
        assert_eq!(l, Load::default());
    }

    #[test]
    fn dropped_counts_video_frames_since_the_first_tick() {
        let mut d = Dropped::default();
        assert_eq!(d.frames(), None);
        let totals = |capture, render, encode, packets| DropCounts {
            capture_frames: capture,
            render_ticks: render,
            encode_frames: encode,
            output_packets: packets,
            ..DropCounts::default()
        };
        d.observe(4, totals(10, 2, 1, 50));
        assert_eq!(d.frames(), Some(0), "counts from the first tick it sees");
        d.observe(5, totals(12, 3, 1, 90));
        assert_eq!(d.frames(), Some(3), "packets are not frames");
        d.observe(7, totals(12, 3, 4, 90));
        assert_eq!(d.frames(), Some(6), "a missed tick still counts");
        d.observe(1, totals(1, 0, 0, 0));
        assert_eq!(d.frames(), Some(0), "an engine restart starts over");
        d.observe(2, totals(0, 0, 0, 0));
        assert_eq!(d.frames(), Some(0), "a shrinking total starts over");
    }
}
