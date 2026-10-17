//! The preview canvas: the selected scene's items as boxes over the preview, with
//! selection, drag, resize, rotate and snapping on their geometry (ADR-0022), each
//! finished gesture sent as one undoable edit, and undo and redo (ADR-0032) from the
//! keyboard. The scene and the selected item are the shared [`Studio`] selection, so
//! the canvas, the sources list and the properties form show the same ones.
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
use crate::store::Studio;
use jstream_control::protocol::{HistoryOp, ItemGeometry, ItemId, Placed, Rev};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use model::{CanvasError, Layout};
use snap::{Guide, SNAP_DISTANCE, SnapTargets};
use transform::{
    Gesture, GestureKind, HANDLE_SIZE, Handle, KNOB_SIZE, Modifiers, Point, Pointer,
    RESIZE_HANDLES, ROTATE_OFFSET, STEM_LENGTH, Viewport,
};

/// The colour of the selection, its handles and its readout.
const ACCENT: &str = "#4c8dff";
/// The colour of snap guides.
const GUIDE: &str = "#ff4fd8";
/// The outline of an item under the pointer.
const HOVER: &str = "rgba(138,180,255,0.6)";
/// The outline of an item that is neither selected nor under the pointer. The preview
/// stream isn't drawn yet, so the boxes are all there is to see.
const IDLE: &str = "rgba(154,161,172,0.55)";

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    /// The item dragged.
    item_id: ItemId,
    /// What the drag does, from where.
    gesture: Gesture,
    /// The pointer the gesture follows, at half speed with Shift.
    pointer: Pointer,
    /// The lines it snaps to, fixed when the drag starts.
    targets: SnapTargets,
}

/// The preview canvas.
#[component]
pub fn Canvas() -> impl IntoView {
    let session = use_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    // The selected scene, laid out from the shared collection.
    let layout = Memo::new(move |_| {
        studio.collection.with(|collection| {
            let collection = collection.as_ref()?;
            let scene = studio.selection.with(|s| s.scene.clone());
            Some(Layout::of(collection, scene.as_ref()))
        })
    });
    let selected = Memo::new(move |_| studio.selection.with(|s| s.item.clone()));
    // The geometry shown for an item while it is dragged, and until the collection
    // catches up with the committed edit.
    let preview = RwSignal::new(None::<(ItemId, ItemGeometry)>);
    // The revision the last commit produced, and whether a commit is still in flight:
    // the preview is dropped once the collection has caught up with both.
    let awaiting = StoredValue::new((None::<Rev>, false));
    let guides = RwSignal::new(Vec::<Guide>::new());
    let snapping = RwSignal::new(true);
    let viewport = RwSignal::new(None::<Viewport>);
    let drag = StoredValue::new(None::<Drag>);
    // What the drag in progress does, for the readout.
    let active = RwSignal::new(None::<GestureKind>);
    let hovered = RwSignal::new(None::<ItemId>);
    let cursor = RwSignal::new("default");
    let stage = NodeRef::<leptos::html::Div>::new();

    let measure = move || {
        let (Some(element), Some(canvas)) = (
            stage.get_untracked(),
            layout.with_untracked(|l| l.as_ref().map(|l| l.canvas)),
        ) else {
            return;
        };
        let fit = Viewport::fit(
            canvas,
            element.client_width() as f32,
            element.client_height() as f32,
        );
        if viewport.get_untracked() != fit {
            viewport.set(fit);
        }
    };

    // Follow the collection: drop the preview once it shows the committed edit.
    Effect::new(move |_| {
        let rev = layout.with(|l| l.as_ref().map(|l| l.rev));
        let (committed, in_flight) = awaiting.get_value();
        let caught_up = !in_flight && committed.is_none_or(|c| rev.is_some_and(|r| r >= c));
        if caught_up && drag.with_value(Option::is_none) {
            preview.set(None);
        }
        measure();
    });
    let resize = window_event_listener(ev::resize, move |_| measure());
    on_cleanup(move || resize.remove());

    let problem = move |error: &CanvasError| studio.problem.set(Some(describe(error)));

    let commit = move |item_id: ItemId, geometry: ItemGeometry| {
        let (Some(open), Some((scene_id, _))) = (
            session.and_then(|s| s.get()),
            layout.with_untracked(|l| l.as_ref().and_then(|l| l.scene.clone())),
        ) else {
            return;
        };
        preview.set(Some((item_id.clone(), geometry)));
        awaiting.update_value(|a| a.1 = true);
        spawn_local(async move {
            let result = model::commit(&*open.control, scene_id, item_id, geometry).await;
            drop(open);
            match result {
                Ok(rev) => {
                    awaiting.set_value((Some(rev), false));
                    let shown = layout.with_untracked(|l| l.as_ref().map(|l| l.rev));
                    if shown.is_some_and(|r| r >= rev) && drag.with_value(Option::is_none) {
                        preview.set(None);
                    }
                }
                Err(error) => {
                    awaiting.set_value((None, false));
                    preview.set(None);
                    problem(&error);
                }
            }
        });
    };

    // Undo and redo. With nothing to undo or redo, nothing happens; the Edit menu has
    // the same commands.
    let step = move |op: HistoryOp| {
        let Some(open) = session.and_then(|s| s.get()) else {
            return;
        };
        spawn_local(async move {
            let result = model::history(&*open.control, op).await;
            drop(open);
            match result {
                Err(error) if !model::is_history_empty(&error) => problem(&error),
                _ => {}
            }
        });
    };

    // An item's geometry as the user last left it: the pending or dragged one if there
    // is one, so a nudge or drag started before the engine answers builds on it.
    let latest = move |current: &Layout, id: &ItemId| -> Option<ItemGeometry> {
        match preview.get_untracked() {
            Some((pending, geometry)) if pending == *id => Some(geometry),
            _ => current.geometry_of(id),
        }
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
            let geometry = latest(&current, &id)?;
            let handle = transform::handle_at(&geometry, point, fit)?;
            Some((id, geometry, GestureKind::from(handle)))
        });
        let target = on_handle.or_else(|| {
            let id = transform::topmost_at(current.boxes(), point)?.clone();
            let geometry = latest(&current, &id)?;
            Some((id, geometry, GestureKind::Move))
        });
        let Some((item_id, start, kind)) = target else {
            studio.select_item(None);
            return;
        };
        event.prevent_default();
        if selected.get_untracked().as_ref() != Some(&item_id) {
            studio.select_item(Some(item_id.clone()));
        }
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
            pointer: Pointer::new(point),
            targets,
        }));
        active.set(Some(kind));
    };

    // The cursor and the hover outline for the pointer at `point`, while nothing is
    // dragged.
    let hover = move |fit: Viewport, point: Point| {
        let Some(current) = layout.get_untracked() else {
            return;
        };
        let chosen = selected.get_untracked();
        let handle = chosen.as_ref().and_then(|id| {
            let geometry = latest(&current, id)?;
            transform::handle_at(&geometry, point, fit)
        });
        let under = transform::topmost_at(current.boxes(), point).cloned();
        cursor.set(match handle {
            Some(handle) => handle_cursor(handle),
            None if under.is_some() && under == chosen => "move",
            None if under.is_some() => "pointer",
            None => "default",
        });
        if hovered.get_untracked() != under {
            hovered.set(under);
        }
    };

    let on_move = move |event: ev::PointerEvent| {
        let Some((fit, point)) = pointer_at(&event) else {
            return;
        };
        let Some(mut current) = drag.get_value() else {
            hover(fit, point);
            return;
        };
        let at = current.pointer.follow(point, event.shift_key());
        let modifiers = Modifiers {
            ctrl: event.ctrl_key() || event.meta_key(),
        };
        let targets = snapping.get_untracked().then_some(&current.targets);
        if let Some(update) =
            current
                .gesture
                .update(at, modifiers, targets, fit.canvas_len(SNAP_DISTANCE))
        {
            preview.set(Some((current.item_id.clone(), update.geometry)));
            guides.set(update.guides);
        }
        drag.set_value(Some(current));
    };

    let end = move |keep: bool| {
        let Some(finished) = drag.get_value() else {
            return;
        };
        drag.set_value(None);
        active.set(None);
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
            "Escape" if drag.with_value(Option::is_some) => end(false),
            "Escape" => studio.select_item(None),
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" => {
                let Some(id) = selected.get_untracked() else {
                    return;
                };
                let Some(start) = layout.with_untracked(|l| latest(l.as_ref()?, &id)) else {
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

    // The boxes to draw: the collection's, with the dragged or pending one replaced.
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

    let canvas_size = move || {
        layout.with(|l| {
            let canvas = l.as_ref().map(|l| l.canvas).unwrap_or_default();
            (canvas.width, canvas.height)
        })
    };
    let view_box = move || {
        let (width, height) = canvas_size();
        format!("0 0 {width} {height}")
    };
    // Screen pixels in canvas units, for handle sizes and label text.
    let px = move |screen: f32| viewport.get().map_or(screen, |v| v.canvas_len(screen));

    let boxes = move || {
        let chosen = selected.get();
        let under = hovered.get();
        shown()
            .into_iter()
            .map(|(id, name, geometry, visible)| {
                let points = outline(&geometry);
                let is_selected = chosen.as_ref() == Some(&id);
                let (stroke, width) = if is_selected {
                    (ACCENT, "2")
                } else if under.as_ref() == Some(&id) {
                    (HOVER, "1")
                } else {
                    (IDLE, "1")
                };
                let dash = if visible { "none" } else { "6 4" };
                // The label sits in the top-left of the box's bounds, whatever its rotation.
                let bounds = snap::Bounds::of(&geometry);
                view! {
                    <g>
                        <polygon
                            points=points
                            fill="rgba(255,255,255,0.04)"
                            stroke=stroke
                            stroke-width=width
                            stroke-dasharray=dash
                            vector-effect="non-scaling-stroke"
                        />
                        <text
                            x=bounds.left + px(4.0)
                            y=bounds.top + px(14.0)
                            font-size=px(12.0)
                            fill=if is_selected { ACCENT } else { "#9aa1ac" }
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
        let side = fit.canvas_len(HANDLE_SIZE);
        let corner = fit.canvas_len(2.0);
        let top = transform::handle_point(&geometry, Handle::Resize { x: 0, y: -1 }, fit);
        let knob = transform::handle_point(&geometry, Handle::Rotate, fit);
        // The stem runs from the top edge towards the knob, under it.
        let reach = STEM_LENGTH / ROTATE_OFFSET;
        let stem = Point {
            x: top.x + (knob.x - top.x) * reach,
            y: top.y + (knob.y - top.y) * reach,
        };
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
                        rx=corner
                        fill="#fff"
                        stroke=ACCENT
                        stroke-width="2"
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
                    x2=stem.x
                    y2=stem.y
                    stroke=ACCENT
                    vector-effect="non-scaling-stroke"
                />
                <circle
                    cx=knob.x
                    cy=knob.y
                    r=fit.canvas_len(KNOB_SIZE / 2.0)
                    fill="#fff"
                    stroke=ACCENT
                    stroke-width="2"
                    vector-effect="non-scaling-stroke"
                />
                {squares}
            </g>
        })
    };

    // The pill under the selected item: its size, or what the drag in progress changes.
    let readout = move || {
        let id = selected.get()?;
        let fit = viewport.get()?;
        let (_, _, geometry, _) = shown().into_iter().find(|(item, ..)| *item == id)?;
        let text = readout_text(&geometry, active.get());
        let bounds = snap::Bounds::of(&geometry);
        // Mono 11px, padded 2px by 7px.
        let height = fit.canvas_len(17.0);
        let width = fit.canvas_len(text.chars().count() as f32 * 6.6 + 14.0);
        let middle = (bounds.left + bounds.right) / 2.0;
        let top = bounds.bottom + fit.canvas_len(10.0);
        Some(view! {
            <g class="readout">
                <rect
                    x=middle - width / 2.0
                    y=top
                    width=width
                    height=height
                    rx=fit.canvas_len(3.0)
                    fill=ACCENT
                />
                <text
                    x=middle
                    y=top + height / 2.0
                    text-anchor="middle"
                    dominant-baseline="central"
                    font-size=fit.canvas_len(11.0)
                    fill="#0f1115"
                >
                    {text}
                </text>
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
    let caption = move || {
        let (width, height) = canvas_size();
        format!("{} · {width} × {height}", scene_name())
    };
    let legacy_note = move || {
        let count = layout.with(|l| l.as_ref().map_or(0, Layout::legacy_count));
        (count > 0).then(|| {
            let items = if count == 1 { "item uses" } else { "items use" };
            view! {
                <span class="canvas-note">
                    {format!(
                        "{count} {items} the old 0.3 placement and can't be edited here yet."
                    )}
                </span>
            }
        })
    };
    let snap_title = move || {
        if snapping.get() {
            "Snapping on: edges, centers and other items · click to turn off"
        } else {
            "Snapping off · click to turn on"
        }
    };

    view! {
        <div class="canvas-view">
            <div
                class="preview stage"
                node_ref=stage
                tabindex="0"
                aria-label=move || format!("Scene canvas, {}", scene_name())
                style:cursor=move || cursor.get()
                on:pointerdown=on_down
                on:pointermove=on_move
                on:pointerleave=move |_| {
                    if drag.with_value(Option::is_none) {
                        hovered.set(None);
                    }
                }
                on:pointerup=move |_| end(true)
                on:pointercancel=move |_| end(false)
                on:lostpointercapture=move |_| end(true)
                on:keydown=on_key
            >
                <svg
                    viewBox=view_box
                    preserveAspectRatio="xMidYMid meet"
                    class="stage-overlay"
                >
                    <rect
                        x="0"
                        y="0"
                        width=move || canvas_size().0
                        height=move || canvas_size().1
                        fill="#141a22"
                    />
                    <text
                        x=move || canvas_size().0 / 2
                        y=move || canvas_size().1 / 2
                        text-anchor="middle"
                        font-size=move || px(13.0)
                        fill="#5a606b"
                    >
                        "Preview stream not connected yet"
                    </text>
                    {boxes}
                    {guide_lines}
                    {handles}
                    {readout}
                </svg>
            </div>
            <div
                class="canvas-tools"
                role="toolbar"
                aria-label="Canvas tools"
                on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()
            >
                <button
                    class="canvas-tool"
                    aria-label="Move and resize"
                    aria-pressed="true"
                    title="Move / resize (V) · Shift: half speed · Ctrl: no snap while moving, stretch on handles"
                >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M5 3l13 8-6 1.5L9 19z" />
                    </svg>
                </button>
                <span class="canvas-tool-gap" aria-hidden="true"></span>
                <button
                    class="canvas-tool"
                    aria-label="Snapping"
                    aria-pressed=move || if snapping.get() { "true" } else { "false" }
                    title=snap_title
                    on:click=move |_| snapping.update(|on| *on = !*on)
                >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M6 4v8a6 6 0 0 0 12 0V4M6 8h4M14 8h4" />
                    </svg>
                </button>
            </div>
            <div class="canvas-caption">{caption}</div>
            {legacy_note}
        </div>
    }
}

/// The cursor over a handle: the resize arrows nearest its direction, unrotated.
fn handle_cursor(handle: Handle) -> &'static str {
    match handle {
        Handle::Rotate => "grab",
        Handle::Resize { x: 0, .. } => "ns-resize",
        Handle::Resize { y: 0, .. } => "ew-resize",
        Handle::Resize { x, y } if x == y => "nwse-resize",
        Handle::Resize { .. } => "nesw-resize",
    }
}

/// The readout pill's text: while moving `X n  Y n` (the box's top-left), while
/// resizing `W × H`, while rotating `N°`, and at rest `W × H` with `  · N°` when the
/// item is rotated.
fn readout_text(geometry: &ItemGeometry, active: Option<GestureKind>) -> String {
    // Adding zero turns -0 into 0.
    let whole = |n: f32| n.round() + 0.0;
    let (width, height) = (geometry.size.width.get(), geometry.size.height.get());
    let size = format!("{} × {}", whole(width), whole(height));
    let rotation = (geometry.rotation_deg.get() * 10.0).round() / 10.0 + 0.0;
    let angle = format!("{rotation}°");
    match active {
        Some(GestureKind::Move) => format!(
            "X {}  Y {}",
            whole(geometry.center.x.get() - width / 2.0),
            whole(geometry.center.y.get() - height / 2.0)
        ),
        Some(GestureKind::Resize { .. }) => size,
        Some(GestureKind::Rotate) => angle,
        None if rotation != 0.0 => format!("{size}  · {angle}"),
        None => size,
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

/// A failed call, for the problem bar.
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

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Finite32, Size, Vec2};

    fn geometry(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> ItemGeometry {
        ItemGeometry {
            rotation_deg: Finite32::new(rotation).expect("finite"),
            ..ItemGeometry::try_new(
                Vec2::new(cx, cy).expect("finite"),
                Size::try_new(w, h).expect("positive"),
            )
            .expect("valid")
        }
    }

    #[test]
    fn the_readout_shows_what_the_drag_changes() {
        let upright = geometry(960.0, 540.0, 640.0, 360.0, 0.0);
        assert_eq!(readout_text(&upright, None), "640 × 360");
        assert_eq!(
            readout_text(&upright, Some(GestureKind::Move)),
            "X 640  Y 360"
        );
        assert_eq!(
            readout_text(&upright, Some(GestureKind::Resize { x: 1, y: 1 })),
            "640 × 360"
        );
        let turned = geometry(320.0, 180.0, 640.0, 360.0, -12.34);
        assert_eq!(readout_text(&turned, None), "640 × 360  · -12.3°");
        assert_eq!(readout_text(&turned, Some(GestureKind::Rotate)), "-12.3°");
        assert_eq!(readout_text(&turned, Some(GestureKind::Move)), "X 0  Y 0");
    }

    #[test]
    fn handles_show_the_arrows_of_their_direction() {
        assert_eq!(
            handle_cursor(Handle::Resize { x: -1, y: -1 }),
            "nwse-resize"
        );
        assert_eq!(handle_cursor(Handle::Resize { x: 1, y: -1 }), "nesw-resize");
        assert_eq!(handle_cursor(Handle::Resize { x: 0, y: 1 }), "ns-resize");
        assert_eq!(handle_cursor(Handle::Resize { x: -1, y: 0 }), "ew-resize");
        assert_eq!(handle_cursor(Handle::Rotate), "grab");
    }
}
