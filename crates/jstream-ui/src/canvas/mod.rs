//! The preview canvas: the selected scene's items as boxes over the preview, with
//! selection, drag, resize, rotate and snapping on their geometry (ADR-0022), crop by
//! dragging (in crop mode, or with Alt), each finished gesture sent as one undoable
//! edit, and undo and redo (ADR-0032) from the keyboard. The scene and the selected item are the shared [`Studio`] selection, so
//! the canvas, the sources list and the properties form show the same ones.
//!
//! Under the boxes is the live program preview: one `preview.subscribe` stream on the
//! session's binary channel, decoded into a `<canvas>` ([`preview`], [`screen`]). Pure
//! logic lives in [`transform`], [`crop`], [`snap`], [`model`] and [`preview`], which are tested
//! natively; this module is the Leptos component that wires them to the page.
//!
//! Docs: crates/jstream-ui/README.md#canvas

pub mod crop;
pub mod group;
pub mod live;
pub mod model;
pub mod preview;
pub mod screen;
pub mod snap;
pub mod transform;

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::item_menu;
use crate::menu::{Entry, Menus};
use crate::panels::add_source::AddSourceDialog;
use crate::store::Studio;
use crate::workspace::DockState;
use crop::{CropGesture, Picture};
use futures_util::StreamExt;
use group::{GROUP_ROTATE_OFFSET, GroupGesture, GroupKind};
use jstream_control::protocol::{
    Crop, GestureId, HistoryOp, ItemGeometry, ItemId, Placed, Rev, SceneId,
};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use model::{CanvasError, Layout};
use preview::PreviewState;
use snap::{Bounds, Guide, SNAP_DISTANCE, SnapTargets};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use transform::{
    Gesture, GestureKind, HANDLE_SIZE, Handle, KNOB_SIZE, Modifiers, Point, Pointer,
    RESIZE_HANDLES, ROTATE_OFFSET, STEM_LENGTH, Viewport,
};

/// The colour of the selection, its handles and its readout.
const ACCENT: &str = "#4c8dff";
/// The colour of the selection, its handles and its readout in crop mode.
const CROP: &str = "#e8a33d";
/// The fill of the crop ghost, the uncropped extent.
const CROP_GHOST: &str = "rgba(232,163,61,0.08)";
/// The side of a drawn crop handle, in screen pixels.
const CROP_HANDLE_SIZE: f32 = 12.0;
/// The colour of snap guides.
const GUIDE: &str = "#ff4fd8";
/// The outline of an item under the pointer.
const HOVER: &str = "rgba(138,180,255,0.6)";
/// The outline of an item that is neither selected nor under the pointer. The preview
/// stream isn't drawn yet, so the boxes are all there is to see.
const IDLE: &str = "rgba(154,161,172,0.55)";

/// What a drag in progress does.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    /// Moves, resizes or rotates one item.
    One {
        /// The item dragged.
        item_id: ItemId,
        /// What the drag does, from where.
        gesture: Gesture,
    },
    /// Crops one item by one of its handles.
    Crop {
        /// The item cropped.
        item_id: ItemId,
        /// What the drag does, from where.
        gesture: CropGesture,
        /// The item's crop when the drag started.
        start: Crop,
    },
    /// Moves, scales or rotates every selected item.
    Group(GroupGesture),
    /// Draws a marquee from `from`: the items it touches are selected, with `base`.
    Marquee {
        /// Where the press was.
        from: Point,
        /// Where the pointer is.
        to: Point,
        /// What stays selected whatever the marquee touches: the selection when Shift
        /// was held at the press, else nothing.
        base: Vec<ItemId>,
    },
}

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    /// What it does.
    action: Action,
    /// The pointer the gesture follows, at half speed with Shift. A marquee follows the
    /// real pointer.
    pointer: Pointer,
    /// The lines it snaps to, fixed when the drag starts.
    targets: SnapTargets,
}

/// The commits the preview waits for: how many are in flight, and the latest revision
/// one produced. The preview is dropped once none is in flight and the collection has
/// reached that revision.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Awaiting {
    in_flight: u32,
    last: Option<Rev>,
}

impl Awaiting {
    /// Whether a collection at `shown` reflects every commit sent.
    fn caught_up(self, shown: Option<Rev>) -> bool {
        self.in_flight == 0
            && self
                .last
                .is_none_or(|last| shown.is_some_and(|r| r >= last))
    }
}

/// What the readout under the selection shows while a drag is in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Active {
    One(GestureKind),
    Crop,
    Group(GroupKind),
}

/// The preview canvas.
#[component]
pub fn Canvas() -> impl IntoView {
    let session = use_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let dock = use_context::<DockState>();
    // The selected scene, laid out from the shared collection.
    let layout = Memo::new(move |_| {
        studio.collection.with(|collection| {
            let collection = collection.as_ref()?;
            let scene = studio.selection.with(|s| s.scene.clone());
            Some(
                studio
                    .sizes
                    .with(|sizes| Layout::sized(collection, scene.as_ref(), sizes)),
            )
        })
    });
    // Crop mode, shared with the item menu; switching scenes leaves it (`Studio`).
    let crop_mode = studio.crop_mode;
    // Every selected item, the primary one first.
    let chosen = Memo::new(move |_| {
        studio
            .selection
            .with(|s| s.items().cloned().collect::<Vec<_>>())
    });
    // The geometries shown for items while they are dragged, and until the collection
    // catches up with the committed edits.
    let preview = RwSignal::new(Vec::<(ItemId, ItemGeometry)>::new());
    // The same for crops.
    let preview_crops = RwSignal::new(Vec::<(ItemId, Crop)>::new());
    let awaiting = StoredValue::new(Awaiting::default());
    let guides = RwSignal::new(Vec::<Guide>::new());
    let snapping = RwSignal::new(true);
    let viewport = RwSignal::new(None::<Viewport>);
    let drag = StoredValue::new(None::<Drag>);
    // The drag's gesture, which shows it live on output.
    let live = StoredValue::new_local(None::<live::Live>);
    // What the drag in progress does, for the readout.
    let active = RwSignal::new(None::<Active>);
    let marquee = RwSignal::new(None::<(Point, Point)>);
    let hovered = RwSignal::new(None::<ItemId>);
    let cursor = RwSignal::new("default");
    let stage = NodeRef::<leptos::html::Div>::new();
    // The canvas area around the frame: it takes the pointer, so a marquee can start on
    // the margin and a handle reaching past the frame can still be grabbed.
    let area = NodeRef::<leptos::html::Div>::new();
    let screen = NodeRef::<leptos::html::Canvas>::new();
    let preview_state = RwSignal::new(PreviewState::Starting);
    let alive = Arc::new(AtomicBool::new(true));

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

    let shown_rev = move || layout.with_untracked(|l| l.as_ref().map(|l| l.rev));
    let drop_preview_if_caught_up = move || {
        if awaiting.get_value().caught_up(shown_rev()) && drag.with_value(Option::is_none) {
            preview.set(Vec::new());
            preview_crops.set(Vec::new());
        }
    };
    // Follow the collection: drop the preview once it shows the committed edits.
    Effect::new(move |_| {
        layout.track();
        drop_preview_if_caught_up();
        measure();
    });
    let resize = window_event_listener(ev::resize, move |_| measure());
    on_cleanup(move || resize.remove());

    let problem = move |error: &CanvasError| studio.problem.set(Some(describe(error)));

    // Shows these items at these geometries, keeping every other item's pending one, so
    // a drag doesn't hide an earlier commit still in flight.
    let show = move |placed: Vec<(ItemId, ItemGeometry)>| {
        preview.update(|shown| {
            shown.retain(|(id, _)| !placed.iter().any(|(p, _)| p == id));
            shown.extend(placed);
        });
    };
    let show_crops = move |cropped: Vec<(ItemId, Crop)>| {
        preview_crops.update(|shown| {
            shown.retain(|(id, _)| !cropped.iter().any(|(c, _)| c == id));
            shown.extend(cropped);
        });
    };

    // The live preview: once the `<canvas>` is in the page, open the session's binary
    // channel and one preview stream on it, and draw its frames.
    {
        let alive = Arc::clone(&alive);
        screen.on_load(move |element| {
            let Some(open) = session.and_then(|s| s.get()) else {
                return;
            };
            let frames = open.control.binary();
            // Only a weak handle is kept, so disconnecting still closes the socket.
            let control = Rc::downgrade(&open.control);
            drop(open);
            let size = stage.get_untracked().map_or((640.0, 360.0), |stage| {
                (stage.client_width() as f32, stage.client_height() as f32)
            });
            let ratio = window().device_pixel_ratio() as f32;
            spawn_local(run_preview(
                frames,
                control,
                screen::Screen::new(element, move || {
                    // Shown once the first picture is drawn, unless the stream has
                    // already ended.
                    if preview_state.try_get_untracked() == Some(PreviewState::Starting) {
                        preview_state.try_set(PreviewState::Live);
                    }
                }),
                size,
                ratio,
                preview_state,
                alive,
            ));
        });
    }
    {
        let alive = Arc::clone(&alive);
        on_cleanup(move || alive.store(false, Ordering::Relaxed));
    }

    // Sends the items' new geometries as one batch, and shows them until the
    // collection has them.
    // A drag's commit names its gesture's scene, and ends the gesture if its live
    // values opened one; a commit that can't be sent or fails cancels the gesture, so
    // the output doesn't keep the dragged geometry (ADR-0032).
    let commit = move |placed: Vec<(ItemId, ItemGeometry)>,
                       crops: Vec<(ItemId, Crop)>,
                       scene: Option<SceneId>,
                       gesture: Option<GestureId>| {
        let Some(open) = session.and_then(|s| s.get()) else {
            return;
        };
        let scene_id = scene.or_else(|| {
            layout.with_untracked(|l| l.as_ref().and_then(|l| l.scene.clone()).map(|(id, _)| id))
        });
        let Some(scene_id) = scene_id.filter(|_| !placed.is_empty() || !crops.is_empty()) else {
            if let Some(gesture) = gesture {
                spawn_local(async move {
                    if let Err(error) = model::live_cancel(&*open.control, gesture).await {
                        problem(&error);
                    }
                });
            }
            return;
        };
        show(placed.clone());
        show_crops(crops.clone());
        awaiting.update_value(|a| a.in_flight += 1);
        spawn_local(async move {
            let result = model::commit(
                &*open.control,
                scene_id,
                placed.clone(),
                crops.clone(),
                gesture.clone(),
            )
            .await;
            awaiting.update_value(|a| a.in_flight = a.in_flight.saturating_sub(1));
            match result {
                Ok(rev) => awaiting.update_value(|a| a.last = a.last.max(Some(rev))),
                Err(error) => {
                    problem(&error);
                    if let Some(gesture) = gesture {
                        // The engine still holds the gesture: drop its live values.
                        let _cancelled = model::live_cancel(&*open.control, gesture).await;
                    }
                    // Stop showing the failed geometries, unless a later gesture has
                    // already replaced them.
                    preview.update(|shown| shown.retain(|entry| !placed.contains(entry)));
                    preview_crops.update(|shown| shown.retain(|entry| !crops.contains(entry)));
                }
            }
            drop(open);
            drop_preview_if_caught_up();
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
        preview
            .with_untracked(|p| p.iter().find(|(p, _)| p == id).map(|(_, g)| *g))
            .or_else(|| current.geometry_of(id))
    };
    // An item's crop as the user last left it, as `latest` does for its box.
    let latest_crop = move |current: &Layout, id: &ItemId| -> Crop {
        preview_crops
            .with_untracked(|p| p.iter().find(|(p, _)| p == id).map(|(_, c)| *c))
            .or_else(|| current.item(id).map(|i| i.crop))
            .unwrap_or(Crop::NONE)
    };
    // An item's picture at `geometry`, with its latest crop, while its source's size is
    // known.
    let picture_of = move |current: &Layout, id: &ItemId, geometry: &ItemGeometry| {
        let size = current.item(id)?.source_size?;
        Picture::of(geometry, size, latest_crop(current, id))
    };
    // The handle of the one selected item under `point`: in crop mode the resize
    // handles sit on its picture and crop, with Alt the box's do; the picture is given
    // when the handle crops. In crop mode with no picture known, only the rotate knob
    // is there.
    let single_handle = move |current: &Layout,
                              id: &ItemId,
                              geometry: &ItemGeometry,
                              fit: Viewport,
                              point: Point,
                              alt: bool|
          -> Option<(Handle, Option<Picture>)> {
        let picture = picture_of(current, id, geometry);
        if crop_mode.get_untracked() {
            if let Some(Handle::Rotate) = transform::handle_at(geometry, point, fit) {
                return Some((Handle::Rotate, None));
            }
            let picture = picture?;
            let handles = picture.geometry(geometry)?;
            return match transform::handle_at(&handles, point, fit)? {
                Handle::Rotate => None,
                handle => Some((handle, Some(picture))),
            };
        }
        match transform::handle_at(geometry, point, fit)? {
            Handle::Resize { .. } if alt => {
                let handle = transform::handle_at(geometry, point, fit)?;
                Some((handle, Some(picture?)))
            }
            handle => Some((handle, None)),
        }
    };
    // The selected items that have a box, with their latest geometries.
    let selected_boxes = move |current: &Layout| -> Vec<(ItemId, ItemGeometry)> {
        chosen
            .get_untracked()
            .into_iter()
            .filter_map(|id| {
                let geometry = latest(current, &id)?;
                Some((id, geometry))
            })
            .collect()
    };

    // The canvas point under a pointer at client coordinates `x`, `y`, measured from
    // the frame wherever the pointer is.
    let canvas_at = move |x: i32, y: i32| -> Option<(Viewport, Point)> {
        measure();
        let fit = viewport.get_untracked()?;
        let frame = stage.get_untracked()?.get_bounding_client_rect();
        Some((
            fit,
            fit.to_canvas(
                (f64::from(x) - frame.left()) as f32,
                (f64::from(y) - frame.top()) as f32,
            ),
        ))
    };
    let pointer_at = move |event: &ev::PointerEvent| canvas_at(event.client_x(), event.client_y());

    // Starts a drag: captures the pointer and remembers what it does.
    let start = move |event: &ev::PointerEvent, point: Point, action: Action, current: &Layout| {
        let moving: Vec<ItemId> = match &action {
            Action::One { item_id, .. } | Action::Crop { item_id, .. } => vec![item_id.clone()],
            Action::Group(gesture) => gesture.starts.iter().map(|(id, _)| id.clone()).collect(),
            Action::Marquee { .. } => Vec::new(),
        };
        let targets = SnapTargets::new(
            current.canvas,
            current
                .boxes()
                .filter(|(id, _)| !moving.contains(id))
                .map(|(_, geometry)| geometry),
        );
        if let Some(element) = area.get_untracked() {
            let _captured = element.set_pointer_capture(event.pointer_id());
        }
        active.set(match &action {
            Action::One { gesture, .. } => Some(Active::One(gesture.kind)),
            Action::Crop { .. } => Some(Active::Crop),
            Action::Group(gesture) => Some(Active::Group(gesture.kind)),
            Action::Marquee { .. } => None,
        });
        // A box drag opens a gesture, so the output follows it before the drop.
        let gesture = match (&action, session, current.scene.as_ref()) {
            (
                Action::One { .. } | Action::Crop { .. } | Action::Group(_),
                Some(session),
                Some((scene_id, _)),
            ) => {
                // Unique for the session, so a remounted canvas never reuses one.
                session.get().map(|open| {
                    let id = GestureId::new(studio.fresh_id(&open, "drag"));
                    live::Live::new(id, scene_id.clone(), session, problem)
                })
            }
            _ => None,
        };
        live.set_value(gesture);
        drag.set_value(Some(Drag {
            action,
            pointer: Pointer::new(point),
            targets,
        }));
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
        event.prevent_default();
        let selection = selected_boxes(&current);
        let shift = event.shift_key();
        // A handle of the selection: the group's when several items are selected, else
        // the one item's.
        if selection.len() > 1 {
            let bounds = Bounds::around(selection.iter().map(|(_, g)| g));
            if let Some(handle) = bounds.and_then(|b| group::handle_at(b, point, fit))
                && let Some(gesture) = GroupGesture::new(handle.into(), selection.clone(), point)
            {
                start(&event, point, Action::Group(gesture), &current);
                return;
            }
        } else if let Some((item_id, geometry)) = selection.first()
            && let Some((handle, picture)) =
                single_handle(&current, item_id, geometry, fit, point, event.alt_key())
        {
            let action = match (handle, picture) {
                (Handle::Resize { x, y }, Some(picture)) => Action::Crop {
                    item_id: item_id.clone(),
                    gesture: CropGesture {
                        x,
                        y,
                        start: *geometry,
                        picture,
                        origin: point,
                    },
                    start: latest_crop(&current, item_id),
                },
                _ => Action::One {
                    item_id: item_id.clone(),
                    gesture: Gesture {
                        kind: handle.into(),
                        start: *geometry,
                        origin: point,
                    },
                },
            };
            start(&event, point, action, &current);
            return;
        }
        let Some(hit) = transform::topmost_at(current.boxes(), point).cloned() else {
            let base = if shift {
                chosen.get_untracked()
            } else {
                Vec::new()
            };
            start(
                &event,
                point,
                Action::Marquee {
                    from: point,
                    to: point,
                    base,
                },
                &current,
            );
            return;
        };
        let was_selected = selection.iter().any(|(id, _)| *id == hit);
        if shift && was_selected {
            // Shift-click on a selected item takes it out, and drags nothing.
            studio.toggle_item(&hit);
            return;
        }
        let moving: Vec<(ItemId, ItemGeometry)> = if was_selected {
            selection
        } else if shift {
            studio.toggle_item(&hit);
            let mut grown = selection;
            grown.extend(latest(&current, &hit).map(|g| (hit.clone(), g)));
            grown
        } else {
            studio.select_item(Some(hit.clone()));
            latest(&current, &hit)
                .map(|g| (hit.clone(), g))
                .into_iter()
                .collect()
        };
        let action = match moving.as_slice() {
            [(item_id, geometry)] => Action::One {
                item_id: item_id.clone(),
                gesture: Gesture {
                    kind: GestureKind::Move,
                    start: *geometry,
                    origin: point,
                },
            },
            _ => match GroupGesture::new(GroupKind::Move, moving, point) {
                Some(gesture) => Action::Group(gesture),
                None => return,
            },
        };
        start(&event, point, action, &current);
    };

    // The cursor and the hover outline for the pointer at `point`, while nothing is
    // dragged.
    let hover = move |fit: Viewport, point: Point, alt: bool| {
        let Some(current) = layout.get_untracked() else {
            return;
        };
        let selection = selected_boxes(&current);
        let handle = if selection.len() > 1 {
            Bounds::around(selection.iter().map(|(_, g)| g))
                .and_then(|b| group::handle_at(b, point, fit))
                .map(|h| (h, false))
        } else {
            selection.first().and_then(|(id, g)| {
                single_handle(&current, id, g, fit, point, alt).map(|(h, p)| (h, p.is_some()))
            })
        };
        let under = transform::topmost_at(current.boxes(), point).cloned();
        let on_selected = under
            .as_ref()
            .is_some_and(|id| selection.iter().any(|(s, _)| s == id));
        let next = match handle {
            Some((_, true)) => "crosshair",
            Some((handle, false)) => handle_cursor(handle),
            None if on_selected => "move",
            None if under.is_some() => "pointer",
            None => "default",
        };
        if cursor.get_untracked() != next {
            cursor.set(next);
        }
        if hovered.get_untracked() != under {
            hovered.set(under);
        }
    };

    let on_move = move |event: ev::PointerEvent| {
        let Some((fit, point)) = pointer_at(&event) else {
            return;
        };
        let Some(mut current) = drag.get_value() else {
            hover(fit, point, event.alt_key());
            return;
        };
        let modifiers = Modifiers {
            ctrl: event.ctrl_key() || event.meta_key(),
        };
        let targets = snapping.get_untracked().then_some(&current.targets);
        let tolerance = fit.canvas_len(SNAP_DISTANCE);
        match &mut current.action {
            Action::Marquee { from, to, base } => {
                *to = point;
                marquee.set(Some((*from, *to)));
                let hits = layout.with_untracked(|l| {
                    l.as_ref()
                        .map(|l| group::marquee(l.boxes(), *from, *to))
                        .unwrap_or_default()
                });
                studio.select_items(base.iter().cloned().chain(hits));
            }
            Action::One { item_id, gesture } => {
                let at = current.pointer.follow(point, event.shift_key());
                if let Some(update) = gesture.update(at, modifiers, targets, tolerance) {
                    let placed = vec![(item_id.clone(), update.geometry)];
                    live.with_value(|l| l.iter().for_each(|l| l.show(placed.clone(), Vec::new())));
                    show(placed);
                    guides.set(update.guides);
                }
            }
            Action::Crop {
                item_id, gesture, ..
            } => {
                let at = current.pointer.follow(point, event.shift_key());
                if let Some((geometry, crop)) = gesture.update(at) {
                    let placed = vec![(item_id.clone(), geometry)];
                    let cropped = vec![(item_id.clone(), crop)];
                    live.with_value(|l| {
                        l.iter()
                            .for_each(|l| l.show(placed.clone(), cropped.clone()));
                    });
                    show(placed);
                    show_crops(cropped);
                }
            }
            Action::Group(gesture) => {
                let at = current.pointer.follow(point, event.shift_key());
                if let Some(update) = gesture.update(at, modifiers, targets, tolerance) {
                    live.with_value(|l| {
                        l.iter()
                            .for_each(|l| l.show(update.geometries.clone(), Vec::new()));
                    });
                    show(update.geometries);
                    guides.set(update.guides);
                }
            }
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
        marquee.set(None);
        let (starts, crop_starts): live::Values = match finished.action {
            Action::Marquee { from, to, base } => {
                if !keep {
                    studio.select_items(base);
                } else if from == to && base.is_empty() {
                    // A click on empty canvas clears the selection.
                    studio.select_item(None);
                }
                return;
            }
            Action::One { item_id, gesture } => (vec![(item_id, gesture.start)], Vec::new()),
            Action::Crop {
                item_id,
                gesture,
                start,
            } => (
                vec![(item_id.clone(), gesture.start)],
                vec![(item_id, start)],
            ),
            Action::Group(gesture) => (gesture.starts, Vec::new()),
        };
        let moved: Vec<(ItemId, ItemGeometry)> = preview.with_untracked(|shown| {
            shown
                .iter()
                .filter(|(id, g)| starts.iter().any(|(s, start)| s == id && start != g))
                .cloned()
                .collect()
        });
        let cropped: Vec<(ItemId, Crop)> = preview_crops.with_untracked(|shown| {
            shown
                .iter()
                .filter(|(id, c)| crop_starts.iter().any(|(s, start)| s == id && start != c))
                .cloned()
                .collect()
        });
        let gesture = live.with_value(Clone::clone);
        live.set_value(None);
        if keep && (!moved.is_empty() || !cropped.is_empty()) {
            match gesture {
                // The commit waits for the live values sent before it.
                Some(gesture) => {
                    gesture.commit(move |scene, ending| {
                        commit(moved, cropped, Some(scene), ending);
                    });
                }
                None => commit(moved, cropped, None, None),
            }
        } else {
            if let Some(gesture) = gesture {
                gesture.cancel();
            }
            // Back to where the gesture started, which is any commit's pending geometry.
            show(starts);
            show_crops(crop_starts);
            drop_preview_if_caught_up();
        }
    };

    let select_all = move || {
        let all = layout.with_untracked(|l| {
            l.iter()
                .flat_map(|l| l.boxes().map(|(id, _)| id.clone()))
                .collect::<Vec<_>>()
        });
        studio.select_items(all);
    };

    let on_key = move |event: ev::KeyboardEvent| {
        let key = event.key();
        let command = event.ctrl_key() || event.meta_key();
        match key.as_str() {
            "z" | "Z" if command && event.shift_key() => step(HistoryOp::Redo),
            "z" | "Z" if command => step(HistoryOp::Undo),
            "y" | "Y" if command => step(HistoryOp::Redo),
            "a" | "A" if command => select_all(),
            "c" | "C" if !command && !event.alt_key() => crop_mode.update(|on| *on = !*on),
            "v" | "V" if !command && !event.alt_key() => crop_mode.set(false),
            "Escape" if drag.with_value(Option::is_some) => end(false),
            "Escape" => {
                crop_mode.set(false);
                studio.select_item(None);
            }
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" => {
                let Some(current) = layout.get_untracked() else {
                    return;
                };
                let distance = if event.shift_key() { 10.0 } else { 1.0 };
                let (dx, dy) = match key.as_str() {
                    "ArrowLeft" => (-distance, 0.0),
                    "ArrowRight" => (distance, 0.0),
                    "ArrowUp" => (0.0, -distance),
                    _ => (0.0, distance),
                };
                let moved: Vec<(ItemId, ItemGeometry)> = selected_boxes(&current)
                    .into_iter()
                    .filter_map(|(id, start)| Some((id, transform::nudge(&start, dx, dy)?)))
                    .collect();
                if moved.is_empty() {
                    return;
                }
                commit(moved, Vec::new(), None, None);
            }
            _ => return,
        }
        event.prevent_default();
    };

    // A double click on an item selects it alone and opens its properties.
    let item_cx = item_menu::Context::from_page();
    let on_double = move |event: ev::MouseEvent| {
        let Some((_, point)) = canvas_at(event.client_x(), event.client_y()) else {
            return;
        };
        let hit = layout.with_untracked(|l| {
            l.as_ref()
                .and_then(|l| transform::topmost_at(l.boxes(), point).cloned())
        });
        match (hit, item_cx) {
            (Some(hit), Some(cx)) => cx.properties(hit),
            (Some(hit), None) => {
                studio.select_item(Some(hit));
                if let Some(dock) = dock {
                    dock.0.update(|d| d.show(PanelId::Inspector));
                }
            }
            _ => {}
        }
    };

    // A right-click opens the canvas menu: the frame's, or the margin's around it.
    let menus = use_context::<Menus>();
    let adder = use_context::<AddSourceDialog>();
    let on_context = move |event: ev::MouseEvent| {
        let Some(menus) = menus else {
            return;
        };
        let at = canvas_at(event.client_x(), event.client_y());
        // On an item: the selected items' menu, after selecting the item alone unless it
        // is already selected.
        let hit = at.and_then(|(_, point)| {
            layout.with_untracked(|l| {
                let l = l.as_ref()?;
                let hit = transform::topmost_at(l.boxes(), point)?.clone();
                Some((l.scene.as_ref()?.0.clone(), hit))
            })
        });
        if let (Some(cx), Some((scene_id, hit))) = (item_cx, hit) {
            if !chosen.get_untracked().contains(&hit) {
                studio.select_item(Some(hit));
            }
            let items = chosen.get_untracked();
            let entries = item_menu::entries(
                cx,
                &scene_id,
                &items,
                item_menu::Scope::Canvas(Rc::new(select_all)),
            );
            if !entries.is_empty() {
                menus.open(&event, entries);
                return;
            }
        }
        let on_frame = at
            .zip(layout.with_untracked(|l| l.as_ref().map(|l| l.canvas)))
            .is_some_and(|((_, p), canvas)| {
                p.x >= 0.0
                    && p.y >= 0.0
                    && p.x <= canvas.width as f32
                    && p.y <= canvas.height as f32
            });
        let scene = studio.selection.with_untracked(|s| s.scene.clone());
        let snap = if snapping.get_untracked() {
            "Turn snapping off"
        } else {
            "Turn snapping on"
        };
        let snap = Entry::item(snap, move || snapping.update(|on| *on = !*on));
        let select = Entry::item("Select all", select_all).hint("Ctrl+A");
        let has_scene = scene.is_some();
        let entries = if on_frame {
            let add = Entry::item("Add source…", move || {
                if let (Some(adder), Some(scene)) = (adder, scene.clone()) {
                    adder.open(scene, false);
                }
            })
            .disabled_if(adder.is_none() || !has_scene);
            vec![
                select,
                snap,
                Entry::Sep,
                add,
                Entry::unavailable("Paste").hint("Ctrl+V"),
            ]
        } else {
            vec![
                Entry::submenu("Pop out", Vec::new()),
                Entry::Sep,
                select,
                snap,
                Entry::Sep,
                Entry::submenu("Transition in", Vec::new()),
                Entry::submenu("Transition out", Vec::new()),
                Entry::Sep,
                Entry::unavailable("Scene properties…"),
                Entry::unavailable("Edit scene filters…"),
            ]
        };
        menus.open(&event, entries);
    };

    // The boxes to draw: the collection's, with the dragged or pending ones replaced.
    let shown = move || -> Vec<(ItemId, String, ItemGeometry, bool)> {
        let pending = preview.get();
        layout.with(|l| {
            l.iter()
                .flat_map(|l| l.items.iter())
                .filter_map(|item| {
                    let geometry = pending
                        .iter()
                        .find(|(id, _)| *id == item.item_id)
                        .map(|(_, g)| *g)
                        .or(item.geometry)?;
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
    // The selected items' shown geometries, the primary one first.
    let shown_selection = move || -> Vec<ItemGeometry> {
        let all = shown();
        chosen
            .get()
            .iter()
            .filter_map(|id| all.iter().find(|(i, ..)| i == id).map(|(_, _, g, _)| *g))
            .collect()
    };

    // The primary selected item's crop as shown, and its picture at `geometry` while its
    // source's size is known.
    let shown_crop = move || -> Option<(Crop, Option<Picture>, ItemGeometry)> {
        let id = chosen.with(|c| c.first().cloned())?;
        let geometry = *shown_selection().first()?;
        let crops = preview_crops.get();
        layout.with(|l| {
            let item = l.as_ref()?.item(&id)?;
            let crop = crops
                .iter()
                .find(|(c, _)| *c == id)
                .map_or(item.crop, |(_, c)| *c);
            let picture = item
                .source_size
                .and_then(|size| Picture::of(&geometry, size, crop));
            Some((crop, picture, geometry))
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
    // The preview is the program mix, so it lies under the boxes only while the canvas
    // shows the program scene; another selected scene gets the placeholder instead.
    let on_program = move || {
        let shown = layout.with(|l| {
            l.as_ref()
                .and_then(|l| l.scene.as_ref().map(|s| s.0.clone()))
        });
        shown.is_some() && studio.program.with(|program| *program == shown)
    };
    let preview_message = move || {
        if on_program() {
            preview_state.get().message()
        } else {
            Some("The preview shows the program scene".to_owned())
        }
    };
    // Where the preview picture goes in the stage, in CSS pixels: the canvas's own fit,
    // so the picture lies exactly under the boxes.
    let screen_box = move || {
        let fit = viewport.get()?;
        let (width, height) = canvas_size();
        Some((
            fit.offset_x,
            fit.offset_y,
            width as f32 * fit.scale,
            height as f32 * fit.scale,
        ))
    };
    // Screen pixels in canvas units, for handle sizes and label text.
    let px = move |screen: f32| viewport.get().map_or(screen, |v| v.canvas_len(screen));

    let boxes = move || {
        let picked = chosen.get();
        let under = hovered.get();
        shown()
            .into_iter()
            .map(|(id, name, geometry, visible)| {
                let points = outline(&geometry);
                let is_selected = picked.contains(&id);
                let (stroke, width) = if is_selected {
                    (selection_colour(picked.len(), crop_mode.get()), "2")
                } else if under.as_ref() == Some(&id) {
                    (HOVER, "1")
                } else {
                    (IDLE, "1")
                };
                let dash = if visible { "none" } else { "6 4" };
                // The label sits in the top-left of the box's bounds, whatever its rotation.
                let bounds = Bounds::of(&geometry);
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
                            fill=if is_selected {
                                selection_colour(picked.len(), crop_mode.get())
                            } else {
                                "#9aa1ac"
                            }
                        >
                            {name}
                        </text>
                    </g>
                }
            })
            .collect_view()
    };

    // Square handles at `squares`, in `colour` (filled, and larger, for crop handles),
    // and a knob on a stem from `top` towards `knob` that stops `stem` short of its full
    // length `reach`.
    let chrome = move |fit: Viewport,
                       squares: Vec<Point>,
                       top: Point,
                       knob: Point,
                       stem: f32,
                       reach: f32,
                       colour: &'static str,
                       cropping: bool| {
        let side = fit.canvas_len(if cropping {
            CROP_HANDLE_SIZE
        } else {
            HANDLE_SIZE
        });
        let corner = fit.canvas_len(2.0);
        let along = stem / reach;
        let end = Point {
            x: top.x + (knob.x - top.x) * along,
            y: top.y + (knob.y - top.y) * along,
        };
        let squares = squares
            .into_iter()
            .map(|at| {
                view! {
                    <rect
                        x=at.x - side / 2.0
                        y=at.y - side / 2.0
                        width=side
                        height=side
                        rx=corner
                        fill=if cropping { CROP } else { "#fff" }
                        stroke=colour
                        stroke-width="2"
                        vector-effect="non-scaling-stroke"
                    />
                }
            })
            .collect_view();
        view! {
            <g>
                <line
                    x1=top.x
                    y1=top.y
                    x2=end.x
                    y2=end.y
                    stroke=colour
                    vector-effect="non-scaling-stroke"
                />
                <circle
                    cx=knob.x
                    cy=knob.y
                    r=fit.canvas_len(KNOB_SIZE / 2.0)
                    fill="#fff"
                    stroke=colour
                    stroke-width="2"
                    vector-effect="non-scaling-stroke"
                />
                {squares}
            </g>
        }
    };

    // A pill of `text` centred under `bottom`, `gap` screen pixels below it.
    let pill = move |fit: Viewport,
                     text: String,
                     middle: f32,
                     bottom: f32,
                     gap: f32,
                     colour: &'static str| {
        // Mono 11px, padded 2px by 7px.
        let height = fit.canvas_len(17.0);
        let width = fit.canvas_len(text.chars().count() as f32 * 6.6 + 14.0);
        let top = bottom + fit.canvas_len(gap);
        view! {
            <g class="readout">
                <rect
                    x=middle - width / 2.0
                    y=top
                    width=width
                    height=height
                    rx=fit.canvas_len(3.0)
                    fill=colour
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
        }
    };

    // The selection's chrome: one item's handles and readout, or the group's dashed box,
    // handles and readout.
    let selection_chrome = move || {
        let fit = viewport.get()?;
        let picked = shown_selection();
        match picked.as_slice() {
            [] => None,
            [geometry] => {
                let cropping = crop_mode.get();
                let colour = selection_colour(1, cropping);
                let (crop, picture) = shown_crop().map_or((Crop::NONE, None), |(c, p, _)| (c, p));
                // In crop mode the handles sit on the picture, and only once its size is
                // known.
                let handle_box = if cropping {
                    picture.and_then(|p| p.geometry(geometry))
                } else {
                    Some(*geometry)
                };
                let squares = handle_box.map_or_else(Vec::new, |b| {
                    RESIZE_HANDLES
                        .iter()
                        .map(|h| transform::handle_point(&b, *h, fit))
                        .collect()
                });
                let top = transform::handle_point(geometry, Handle::Resize { x: 0, y: -1 }, fit);
                let knob = transform::handle_point(geometry, Handle::Rotate, fit);
                let text = match active.get() {
                    Some(Active::Crop) => crop::readout(&crop),
                    Some(Active::One(kind)) => readout_text(geometry, Some(kind), false),
                    _ if cropping && picture.is_none() => {
                        format!(
                            "{}  · no source size yet",
                            readout_text(geometry, None, false)
                        )
                    }
                    _ => readout_text(geometry, None, crop != Crop::NONE),
                };
                // The uncropped extent, while the item is cropped or crop mode is on.
                let ghost = picture.filter(|_| cropping || crop != Crop::NONE).map(|p| {
                    let points: String = p
                        .ghost(geometry)
                        .iter()
                        .map(|c| format!("{},{} ", c.x, c.y))
                        .collect();
                    view! {
                        <polygon
                            class="crop-ghost"
                            points=points
                            fill=CROP_GHOST
                            stroke=CROP
                            stroke-dasharray="4 3"
                            vector-effect="non-scaling-stroke"
                        />
                    }
                });
                let bounds = Bounds::of(geometry);
                let middle = (bounds.left + bounds.right) / 2.0;
                Some(
                    view! {
                        {ghost}
                        {chrome(fit, squares, top, knob, STEM_LENGTH, ROTATE_OFFSET, colour, cropping)}
                        {pill(fit, text, middle, bounds.bottom, 10.0, colour)}
                    }
                    .into_any(),
                )
            }
            many => {
                let bounds = Bounds::around(many)?;
                let o = group::outline(bounds, fit);
                let squares = RESIZE_HANDLES
                    .iter()
                    .map(|h| group::handle_point(bounds, *h, fit))
                    .collect();
                let top = Point {
                    x: (o.left + o.right) / 2.0,
                    y: o.top,
                };
                let knob = group::handle_point(bounds, Handle::Rotate, fit);
                let turned = match (drag.with_value(|d| d.clone()), active.get()) {
                    (
                        Some(Drag {
                            action: Action::Group(g),
                            ..
                        }),
                        Some(Active::Group(GroupKind::Rotate)),
                    ) => g.starts.first().and_then(|(_, start)| {
                        Some(many.first()?.rotation_deg.get() - start.rotation_deg.get())
                    }),
                    _ => None,
                };
                let text = group_readout_text(many.len(), bounds, active.get(), turned);
                Some(
                    view! {
                        <rect
                            x=o.left
                            y=o.top
                            width=o.right - o.left
                            height=o.bottom - o.top
                            fill="none"
                            stroke=ACCENT
                            stroke-dasharray="4 3"
                            vector-effect="non-scaling-stroke"
                        />
                        {chrome(fit, squares, top, knob, STEM_LENGTH, GROUP_ROTATE_OFFSET, ACCENT, false)}
                        {pill(fit, text, (o.left + o.right) / 2.0, o.bottom, 12.0, ACCENT)}
                    }
                    .into_any(),
                )
            }
        }
    };

    let marquee_box = move || {
        let (a, b) = marquee.get()?;
        Some(view! {
            <rect
                class="marquee"
                x=a.x.min(b.x)
                y=a.y.min(b.y)
                width=(a.x - b.x).abs()
                height=(a.y - b.y).abs()
                fill="rgba(76,141,255,0.12)"
                stroke=ACCENT
                vector-effect="non-scaling-stroke"
            />
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
        <div
            class="canvas-view"
            node_ref=area
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
            on:dblclick=on_double
            on:contextmenu=on_context
        >
            <div
                class="preview stage"
                node_ref=stage
                tabindex="0"
                aria-label=move || format!("Scene canvas, {}", scene_name())
                on:keydown=on_key
            >
                <canvas
                    node_ref=screen
                    class="stage-screen"
                    style:left=move || screen_box().map(|b| format!("{}px", b.0))
                    style:top=move || screen_box().map(|b| format!("{}px", b.1))
                    style:width=move || screen_box().map(|b| format!("{}px", b.2))
                    style:height=move || screen_box().map(|b| format!("{}px", b.3))
                    style:visibility=move || {
                        if on_program() && preview_state.get() == PreviewState::Live {
                            "visible"
                        } else {
                            "hidden"
                        }
                    }
                ></canvas>
                <svg
                    viewBox=view_box
                    preserveAspectRatio="xMidYMid meet"
                    class="stage-overlay"
                >
                    {move || {
                        let message = preview_message()?;
                        let (width, height) = canvas_size();
                        Some(view! {
                            <rect x="0" y="0" width=width height=height fill="#1d1d21" />
                            <text
                                x=width / 2
                                y=height / 2
                                text-anchor="middle"
                                font-size=px(13.0)
                                fill="#77777f"
                            >
                                {message}
                            </text>
                        })
                    }}
                    {boxes}
                    {guide_lines}
                    {selection_chrome}
                    {marquee_box}
                </svg>
            </div>
            <div
                class="canvas-tools"
                role="toolbar"
                aria-label="Canvas tools"
                on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()
                on:dblclick=|event: ev::MouseEvent| event.stop_propagation()
                on:contextmenu=|event: ev::MouseEvent| {
                    event.prevent_default();
                    event.stop_propagation();
                }
            >
                <button
                    class="canvas-tool"
                    aria-label="Move and resize"
                    aria-pressed=move || if crop_mode.get() { "false" } else { "true" }
                    title="Move / resize (V) · Shift: half speed or add to selection · Ctrl: no snap while moving, stretch on handles · Alt on a handle: crop"
                    on:click=move |_| crop_mode.set(false)
                >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M5 3l13 8-6 1.5L9 19z" />
                    </svg>
                </button>
                <button
                    class="canvas-tool crop-tool"
                    aria-label="Crop"
                    aria-pressed=move || if crop_mode.get() { "true" } else { "false" }
                    title="Crop (C) · drag a handle to trim"
                    on:click=move |_| crop_mode.update(|on| *on = !*on)
                >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M6 2v14a2 2 0 0 0 2 2h14M2 6h14a2 2 0 0 1 2 2v14" />
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

/// The colour of the selection's outline, handles and readout: the crop colour for one
/// item in crop mode, else the accent.
fn selection_colour(selected: usize, crop_mode: bool) -> &'static str {
    if crop_mode && selected == 1 {
        CROP
    } else {
        ACCENT
    }
}

/// The readout pill's text: while moving `X n  Y n` (the box's top-left), while
/// resizing `W × H`, while rotating `N°`, and at rest `W × H`, with `  · cropped` when
/// the item is `cropped` and `  · N°` when it is rotated.
fn readout_text(geometry: &ItemGeometry, active: Option<GestureKind>, cropped: bool) -> String {
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
        None => {
            let mut text = size;
            if cropped {
                text.push_str("  · cropped");
            }
            if rotation != 0.0 {
                text.push_str(&format!("  · {angle}"));
            }
            text
        }
    }
}

/// The group readout: `N items · W × H` of the selection's bounds, while scaling just
/// `W × H`, and while rotating the angle turned so far.
fn group_readout_text(
    count: usize,
    bounds: Bounds,
    active: Option<Active>,
    turned: Option<f32>,
) -> String {
    let whole = |n: f32| n.round() + 0.0;
    let size = format!(
        "{} × {}",
        whole(bounds.right - bounds.left),
        whole(bounds.bottom - bounds.top)
    );
    match (active, turned) {
        (Some(Active::Group(GroupKind::Scale { .. })), _) => size,
        (Some(Active::Group(GroupKind::Rotate)), Some(turned)) => {
            let turned = transform::normalize_deg(turned);
            format!("{}°", (turned * 10.0).round() / 10.0 + 0.0)
        }
        _ => format!("{count} items · {size}"),
    }
}

/// A box's outline as SVG `points`, from `jstream-protocol`'s placement formula, so it
/// is exactly what the compositor draws. Empty if a corner is behind the viewer (only
/// possible with 3D, which the engine accepts from protocol 0.34).
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

/// Runs the canvas's preview stream: subscribes, then draws each frame of its channel
/// until the session, the stream or the canvas ends. When the browser's H.264 decoder
/// fails, it subscribes again for JPEG only.
async fn run_preview(
    mut frames: jstream_control::BinaryStream,
    control: std::rc::Weak<dyn jstream_control::Control>,
    screen: Option<screen::Screen>,
    (width, height): (f32, f32),
    ratio: f32,
    state: RwSignal<PreviewState>,
    alive: Arc<AtomicBool>,
) {
    let Some(mut screen) = screen else {
        state.set(PreviewState::Unavailable(
            "this browser can't draw on a canvas".into(),
        ));
        return;
    };
    let subscribe = |h264: bool| {
        let control = control.clone();
        async move {
            let control = control.upgrade()?;
            let action = preview::subscribe(width, height, ratio, h264);
            Some(
                control
                    .call(jstream_control::protocol::Call::Action { action })
                    .await,
            )
        }
    };
    let mut h264 = screen::Screen::supports_h264();
    match subscribe(h264).await {
        Some(Ok(_)) => {}
        Some(Err(error)) => {
            state.try_set(PreviewState::Unavailable(error.to_string()));
            return;
        }
        None => return,
    }
    while let Some(frame) = frames.next().await {
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        let header = frame.header;
        if header.channel != preview::CHANNEL {
            continue;
        }
        if header.end {
            let reason = String::from_utf8_lossy(&frame.payload).into_owned();
            state.try_set(PreviewState::Unavailable(reason));
            return;
        }
        if let Err(reason) = screen.show(&header, &frame.payload)
            && !screen.h264_failed()
        {
            state.try_set(PreviewState::Unavailable(reason));
            return;
        }
        if h264 && screen.h264_failed() {
            // Fall back to JPEG once. The channel can only be reused once its stream is
            // closed, so close it first.
            h264 = false;
            let Some(control) = control.upgrade() else {
                return;
            };
            let close = jstream_control::protocol::Action::PreviewUnsubscribe {
                channel: preview::CHANNEL,
            };
            let _closed = control
                .call(jstream_control::protocol::Call::Action { action: close })
                .await;
            drop(control);
            if let Some(Err(error)) = subscribe(false).await {
                state.try_set(PreviewState::Unavailable(error.to_string()));
                return;
            }
        }
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
        assert_eq!(readout_text(&upright, None, false), "640 × 360");
        assert_eq!(
            readout_text(&upright, Some(GestureKind::Move), false),
            "X 640  Y 360"
        );
        assert_eq!(
            readout_text(&upright, Some(GestureKind::Resize { x: 1, y: 1 }), true),
            "640 × 360"
        );
        assert_eq!(readout_text(&upright, None, true), "640 × 360  · cropped");
        let turned = geometry(320.0, 180.0, 640.0, 360.0, -12.34);
        assert_eq!(readout_text(&turned, None, false), "640 × 360  · -12.3°");
        assert_eq!(
            readout_text(&turned, None, true),
            "640 × 360  · cropped  · -12.3°"
        );
        assert_eq!(
            readout_text(&turned, Some(GestureKind::Rotate), false),
            "-12.3°"
        );
        assert_eq!(
            readout_text(&turned, Some(GestureKind::Move), false),
            "X 0  Y 0"
        );
    }

    #[test]
    fn the_group_readout_counts_and_measures() {
        let bounds = Bounds {
            left: 0.0,
            top: 0.0,
            right: 300.4,
            bottom: 100.0,
        };
        assert_eq!(
            group_readout_text(2, bounds, None, None),
            "2 items · 300 × 100"
        );
        let scaling = Some(Active::Group(GroupKind::Scale { x: 1, y: 1 }));
        assert_eq!(group_readout_text(2, bounds, scaling, None), "300 × 100");
        let rotating = Some(Active::Group(GroupKind::Rotate));
        assert_eq!(
            group_readout_text(2, bounds, rotating, Some(-370.04)),
            "-10°"
        );
    }

    #[test]
    fn the_preview_waits_for_every_commit() {
        let mut waiting = Awaiting::default();
        assert!(waiting.caught_up(None));
        // Two commits in flight; the first answers at revision 5.
        waiting.in_flight = 1;
        waiting.last = Some(Rev(5));
        assert!(
            !waiting.caught_up(Some(Rev(5))),
            "the second is still in flight"
        );
        // The second answers at 6: caught up only once the collection reaches 6.
        waiting.in_flight = 0;
        waiting.last = Some(Rev(6));
        assert!(!waiting.caught_up(Some(Rev(5))));
        assert!(waiting.caught_up(Some(Rev(6))));
    }

    #[test]
    fn crop_mode_colours_one_item_only() {
        assert_eq!(selection_colour(1, true), CROP);
        assert_eq!(selection_colour(2, true), ACCENT);
        assert_eq!(selection_colour(1, false), ACCENT);
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
