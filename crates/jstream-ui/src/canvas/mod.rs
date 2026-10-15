//! The preview canvas: the program scene's items as boxes over the preview, with
//! selection, drag, resize, rotate and snapping on their geometry (ADR-0022), each
//! finished gesture sent as one undoable edit, and undo and redo (ADR-0032).
//!
//! The preview itself is a placeholder background until the preview stream is hooked
//! up. Pure logic lives in [`transform`], [`snap`] and [`model`], which are tested
//! natively; this module is the Leptos component that wires them to the page.
//!
//! Docs: crates/jstream-ui/README.md#canvas

pub mod model;
pub mod snap;
pub mod transform;

use crate::app::SessionContext;
use futures_util::StreamExt;
use jstream_control::protocol::{Event, HistoryOp, ItemGeometry, ItemId, Placed};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use model::{CanvasError, Depths, Layout};
use snap::{Guide, SNAP_DISTANCE, SnapTargets};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use transform::{
    Gesture, GestureKind, HANDLE_RADIUS, Handle, Modifiers, Point, RESIZE_HANDLES, Viewport,
};

/// The colour of the selection and its handles.
const ACCENT: &str = "#3b6ef5";
/// The colour of snap guides.
const GUIDE: &str = "#ff3ea5";

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    /// The item dragged.
    item_id: ItemId,
    /// What the drag does, from where.
    gesture: Gesture,
    /// The lines it snaps to, fixed when the drag starts.
    targets: SnapTargets,
}

/// The preview canvas.
#[component]
pub fn Canvas() -> impl IntoView {
    let session = use_context::<SessionContext>();
    let layout = RwSignal::new(None::<Layout>);
    let problem = RwSignal::new(None::<String>);
    let selected = RwSignal::new(None::<ItemId>);
    // The geometry shown for an item while it is dragged, and until the engine's copy
    // catches up with the committed edit.
    let preview = RwSignal::new(None::<(ItemId, ItemGeometry)>);
    let guides = RwSignal::new(Vec::<Guide>::new());
    let depths = RwSignal::new(Depths::default());
    let snapping = RwSignal::new(true);
    let viewport = RwSignal::new(None::<Viewport>);
    let drag = StoredValue::new(None::<Drag>);
    // Bumped by every load and every commit, so an older load's answer is dropped.
    let loads = StoredValue::new(0_u64);
    let stage = NodeRef::<leptos::html::Div>::new();
    let alive = Arc::new(AtomicBool::new(true));

    let measure = move || {
        let (Some(element), Some(current)) = (stage.get_untracked(), layout.get_untracked()) else {
            return;
        };
        let fit = Viewport::fit(
            current.canvas,
            element.client_width() as f32,
            element.client_height() as f32,
        );
        if viewport.get_untracked() != fit {
            viewport.set(fit);
        }
    };

    let reload = move || {
        let Some(open) = session.and_then(|s| s.get()) else {
            return;
        };
        loads.update_value(|n| *n += 1);
        let mine = loads.get_value();
        spawn_local(async move {
            let result = model::load(&*open.control).await;
            drop(open);
            if loads.try_get_value() != Some(mine) {
                return;
            }
            match result {
                Ok(fresh) => {
                    if selected
                        .get_untracked()
                        .is_some_and(|id| fresh.geometry_of(&id).is_none())
                    {
                        selected.set(None);
                    }
                    if drag.get_value().is_none() {
                        preview.set(None);
                    }
                    layout.set(Some(fresh));
                    measure();
                }
                Err(error) => problem.set(Some(error.to_string())),
            }
        });
    };

    // Follow the engine: re-read on every state change, program switch or dropped
    // events. Only the event stream is kept, so disconnecting still closes the socket.
    if let Some(open) = session.and_then(|s| s.get()) {
        let mut events = open.control.events();
        drop(open);
        let alive = Arc::clone(&alive);
        spawn_local(async move {
            while let Some(event) = events.next().await {
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                if matches!(
                    event,
                    Event::StateChanged { .. }
                        | Event::StudioChanged { .. }
                        | Event::EventsDropped { .. }
                ) {
                    reload();
                }
            }
        });
    }
    reload();
    {
        let alive = Arc::clone(&alive);
        let resize = window_event_listener(ev::resize, move |_| measure());
        on_cleanup(move || {
            alive.store(false, Ordering::Relaxed);
            resize.remove();
        });
    }

    let commit = move |item_id: ItemId, geometry: ItemGeometry| {
        let (Some(open), Some((scene_id, _))) = (
            session.and_then(|s| s.get()),
            layout.with_untracked(|l| l.as_ref().and_then(|l| l.scene.clone())),
        ) else {
            return;
        };
        loads.update_value(|n| *n += 1);
        preview.set(Some((item_id.clone(), geometry)));
        spawn_local(async move {
            let result = model::commit(&*open.control, scene_id, item_id, geometry).await;
            drop(open);
            match result {
                Ok(_) => {
                    depths.update(|d| *d = d.after_batch());
                    problem.set(None);
                }
                Err(error) => {
                    preview.set(None);
                    problem.set(Some(error.to_string()));
                }
            }
            reload();
        });
    };

    let step = move |op: HistoryOp| {
        let Some(open) = session.and_then(|s| s.get()) else {
            return;
        };
        spawn_local(async move {
            let result = model::history(&*open.control, op).await;
            drop(open);
            match result {
                Ok(now) => {
                    depths.set(now);
                    problem.set(None);
                }
                Err(error) if model::is_history_empty(&error) => {
                    depths.update(|d| match op {
                        HistoryOp::Redo => d.redo = Some(0),
                        _ => d.undo = Some(0),
                    });
                }
                Err(error) => problem.set(Some(describe(&error))),
            }
            reload();
        });
    };

    let pointer_at = move |event: &ev::PointerEvent| -> Option<(Viewport, Point)> {
        measure();
        let fit = viewport.get_untracked()?;
        Some((
            fit,
            fit.to_canvas(event.offset_x() as f32, event.offset_y() as f32),
        ))
    };

    let on_down = move |event: ev::PointerEvent| {
        if event.button() != 0 {
            return;
        }
        let Some((fit, point)) = pointer_at(&event) else {
            return;
        };
        let Some(current) = layout.get_untracked() else {
            return;
        };
        if let Some(element) = stage.get_untracked() {
            let _focused = element.focus();
        }
        let on_handle = selected.get_untracked().and_then(|id| {
            let geometry = current.geometry_of(&id)?;
            let handle = transform::handle_at(&geometry, point, fit)?;
            Some((id, geometry, GestureKind::from(handle)))
        });
        let target = on_handle.or_else(|| {
            let id = transform::topmost_at(current.boxes(), point)?.clone();
            let geometry = current.geometry_of(&id)?;
            Some((id, geometry, GestureKind::Move))
        });
        let Some((item_id, start, kind)) = target else {
            selected.set(None);
            return;
        };
        event.prevent_default();
        selected.set(Some(item_id.clone()));
        let targets = SnapTargets::new(
            current.canvas,
            current
                .boxes()
                .filter(|(id, _)| **id != item_id)
                .map(|(_, geometry)| geometry),
        );
        if let Some(element) = stage.get_untracked() {
            let _captured = element.set_pointer_capture(event.pointer_id());
        }
        drag.set_value(Some(Drag {
            item_id,
            gesture: Gesture {
                kind,
                start,
                origin: point,
            },
            targets,
        }));
    };

    let on_move = move |event: ev::PointerEvent| {
        let Some(current) = drag.get_value() else {
            return;
        };
        let Some((fit, point)) = pointer_at(&event) else {
            return;
        };
        let modifiers = Modifiers {
            shift: event.shift_key(),
            alt: event.alt_key(),
        };
        let targets = snapping.get_untracked().then_some(&current.targets);
        if let Some(update) =
            current
                .gesture
                .update(point, modifiers, targets, fit.canvas_len(SNAP_DISTANCE))
        {
            preview.set(Some((current.item_id, update.geometry)));
            guides.set(update.guides);
        }
    };

    let end = move |keep: bool| {
        let Some(finished) = drag.get_value() else {
            return;
        };
        drag.set_value(None);
        guides.set(Vec::new());
        let moved = preview.get_untracked().filter(|(id, geometry)| {
            *id == finished.item_id && *geometry != finished.gesture.start
        });
        match moved {
            Some((item_id, geometry)) if keep => commit(item_id, geometry),
            _ => preview.set(None),
        }
    };

    let on_key = move |event: ev::KeyboardEvent| {
        let key = event.key();
        let command = event.ctrl_key() || event.meta_key();
        match key.as_str() {
            "z" | "Z" if command && event.shift_key() => step(HistoryOp::Redo),
            "z" | "Z" if command => step(HistoryOp::Undo),
            "y" | "Y" if command => step(HistoryOp::Redo),
            "Escape" if drag.get_value().is_some() => end(false),
            "Escape" => selected.set(None),
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" => {
                let Some(id) = selected.get_untracked() else {
                    return;
                };
                let Some(start) = layout.with_untracked(|l| l.as_ref()?.geometry_of(&id)) else {
                    return;
                };
                let distance = if event.shift_key() { 10.0 } else { 1.0 };
                let (dx, dy) = match key.as_str() {
                    "ArrowLeft" => (-distance, 0.0),
                    "ArrowRight" => (distance, 0.0),
                    "ArrowUp" => (0.0, -distance),
                    _ => (0.0, distance),
                };
                if let Some(moved) = transform::nudge(&start, dx, dy) {
                    commit(id, moved);
                }
            }
            _ => return,
        }
        event.prevent_default();
    };

    // The boxes to draw: the engine's, with the dragged or pending one replaced.
    let shown = move || -> Vec<(ItemId, String, ItemGeometry, bool)> {
        let pending = preview.get();
        layout.with(|l| {
            l.iter()
                .flat_map(|l| l.items.iter())
                .filter_map(|item| {
                    let geometry = match &pending {
                        Some((id, geometry)) if *id == item.item_id => *geometry,
                        _ => item.geometry?,
                    };
                    Some((
                        item.item_id.clone(),
                        item.name.clone(),
                        geometry,
                        item.visible,
                    ))
                })
                .collect()
        })
    };

    let view_box = move || {
        layout.with(|l| {
            let canvas = l.as_ref().map(|l| l.canvas).unwrap_or_default();
            format!("0 0 {} {}", canvas.width, canvas.height)
        })
    };
    let canvas_size = move || {
        layout.with(|l| {
            let canvas = l.as_ref().map(|l| l.canvas).unwrap_or_default();
            (canvas.width, canvas.height)
        })
    };
    // Screen pixels in canvas units, for handle sizes and label text.
    let px = move |screen: f32| viewport.get().map_or(screen, |v| v.canvas_len(screen));

    let boxes = move || {
        let chosen = selected.get();
        shown()
            .into_iter()
            .map(|(id, name, geometry, visible)| {
                let points = outline(&geometry);
                let is_selected = chosen.as_ref() == Some(&id);
                let stroke = if is_selected { ACCENT } else { "#d0d0d8" };
                let dash = if visible { "none" } else { "6 4" };
                // The label sits in the top-left of the box's bounds, whatever its rotation.
                let bounds = snap::Bounds::of(&geometry);
                view! {
                    <g>
                        <polygon
                            points=points
                            fill="rgba(255,255,255,0.04)"
                            stroke=stroke
                            stroke-width=if is_selected { "2" } else { "1" }
                            stroke-dasharray=dash
                            vector-effect="non-scaling-stroke"
                        />
                        <text
                            x=bounds.left + px(4.0)
                            y=bounds.top + px(14.0)
                            font-size=px(12.0)
                            fill=stroke
                        >
                            {name}
                        </text>
                    </g>
                }
            })
            .collect_view()
    };

    let handles = move || {
        let id = selected.get()?;
        let fit = viewport.get()?;
        let (_, _, geometry, _) = shown().into_iter().find(|(item, ..)| *item == id)?;
        let side = fit.canvas_len(HANDLE_RADIUS * 2.0);
        let top = transform::handle_point(&geometry, Handle::Resize { x: 0, y: -1 }, fit);
        let knob = transform::handle_point(&geometry, Handle::Rotate, fit);
        let squares = RESIZE_HANDLES
            .iter()
            .map(|handle| {
                let at = transform::handle_point(&geometry, *handle, fit);
                view! {
                    <rect
                        x=at.x - side / 2.0
                        y=at.y - side / 2.0
                        width=side
                        height=side
                        fill="#fff"
                        stroke=ACCENT
                        vector-effect="non-scaling-stroke"
                    />
                }
            })
            .collect_view();
        Some(view! {
            <g>
                <line
                    x1=top.x
                    y1=top.y
                    x2=knob.x
                    y2=knob.y
                    stroke=ACCENT
                    vector-effect="non-scaling-stroke"
                />
                <circle
                    cx=knob.x
                    cy=knob.y
                    r=side / 2.0
                    fill="#fff"
                    stroke=ACCENT
                    vector-effect="non-scaling-stroke"
                />
                {squares}
            </g>
        })
    };

    let guide_lines = move || {
        let (width, height) = canvas_size();
        guides
            .get()
            .into_iter()
            .map(|guide| {
                let (x1, y1, x2, y2) = match guide {
                    Guide::Vertical(x) => (x, 0.0, x, height as f32),
                    Guide::Horizontal(y) => (0.0, y, width as f32, y),
                };
                view! {
                    <line
                        x1=x1
                        y1=y1
                        x2=x2
                        y2=y2
                        stroke=GUIDE
                        vector-effect="non-scaling-stroke"
                    />
                }
            })
            .collect_view()
    };

    let scene_name = move || {
        layout.with(|l| match l {
            None => "Loading…".to_owned(),
            Some(Layout { scene: None, .. }) => "No scene yet".to_owned(),
            Some(Layout {
                scene: Some((_, name)),
                ..
            }) => name.clone(),
        })
    };
    let legacy_note = move || {
        let count = layout.with(|l| l.as_ref().map_or(0, Layout::legacy_count));
        (count > 0).then(|| {
            let items = if count == 1 { "item uses" } else { "items use" };
            view! {
                <span class="placeholder">
                    {format!(
                        "{count} {items} the old 0.3 placement and can't be edited here yet."
                    )}
                </span>
            }
        })
    };

    view! {
        <div style="display:flex;flex-direction:column;gap:0.5rem;width:100%;height:100%;align-items:center">
            <div style="display:flex;gap:0.75rem;align-items:center;width:min(100%, 960px);flex-wrap:wrap">
                <strong>{scene_name}</strong>
                <button
                    title="Undo (Ctrl+Z)"
                    disabled=move || depths.get().undo == Some(0)
                    on:click=move |_| step(HistoryOp::Undo)
                >
                    "Undo"
                </button>
                <button
                    title="Redo (Ctrl+Shift+Z)"
                    disabled=move || depths.get().redo == Some(0)
                    on:click=move |_| step(HistoryOp::Redo)
                >
                    "Redo"
                </button>
                <label title="Hold Alt while dragging to skip snapping">
                    <input
                        type="checkbox"
                        style="width:auto"
                        prop:checked=move || snapping.get()
                        on:change=move |_| snapping.update(|on| *on = !*on)
                    />
                    " Snap"
                </label>
                {legacy_note}
                {move || problem.get().map(|reason| view! { <span class="problem" role="alert">{reason}</span> })}
            </div>
            <div
                class="preview"
                node_ref=stage
                tabindex="0"
                style="position:relative;touch-action:none;outline-offset:2px"
                on:pointerdown=on_down
                on:pointermove=on_move
                on:pointerup=move |_| end(true)
                on:pointercancel=move |_| end(false)
                on:lostpointercapture=move |_| end(true)
                on:keydown=on_key
            >
                <svg
                    viewBox=view_box
                    preserveAspectRatio="xMidYMid meet"
                    style="position:absolute;inset:0;width:100%;height:100%;pointer-events:none"
                >
                    <rect
                        x="0"
                        y="0"
                        width=move || canvas_size().0
                        height=move || canvas_size().1
                        fill="#1d1d21"
                    />
                    <text
                        x=move || canvas_size().0 / 2
                        y=move || canvas_size().1 / 2
                        text-anchor="middle"
                        font-size=move || px(13.0)
                        fill="#77777f"
                    >
                        "Preview stream not connected yet"
                    </text>
                    {boxes}
                    {guide_lines}
                    {handles}
                </svg>
            </div>
        </div>
    }
}

/// A box's outline as SVG `points`, from `jstream-protocol`'s placement formula, so it
/// is exactly what the compositor draws. Empty if a corner is behind the viewer (only
/// possible with 3D, which the engine doesn't accept yet).
fn outline(geometry: &ItemGeometry) -> String {
    let Ok(quad) = geometry.project() else {
        return String::new();
    };
    let mut points = String::new();
    for corner in quad.corners {
        let Placed::At(at) = corner else {
            return String::new();
        };
        points.push_str(&format!("{},{} ", at.x, at.y));
    }
    points
}

/// A failed undo or redo, for the toolbar.
fn describe(error: &CanvasError) -> String {
    match error {
        CanvasError::Control(control)
            if control
                .protocol()
                .is_some_and(|p| p.code() == "undo_conflict") =>
        {
            "Can't undo that: someone else has changed it since.".to_owned()
        }
        other => other.to_string(),
    }
}
