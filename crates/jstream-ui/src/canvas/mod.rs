//! The preview canvas: the selected scene's items as boxes over the preview, with
//! selection, drag, resize, rotate and snapping on their geometry (ADR-0022), crop by
//! dragging (in crop mode, or with Alt), each finished gesture sent as one undoable
//! edit, and undo and redo (ADR-0032) from the keyboard. The scene and the selected item
//! are the shared [`Studio`] selection, so the canvas, the sources list and the
//! properties form show the same ones. A keyframe scene shows its parent's items at its
//! overrides under the keyframe banner, and its drags send the keyframe's live values
//! and commit its overrides (#846); Ctrl+D, Ctrl+V and Del, which would change
//! the parent's items, do nothing there. Ctrl+C and Ctrl+V copy and paste items
//! (`clipboard`).
//!
//! Under the boxes is the shown scene's live picture: one `preview.subscribe` stream on
//! the session's binary channel, of the program mix while the canvas shows the program
//! scene and of the shown scene otherwise, decoded into a `<canvas>` ([`preview`], [`screen`]). Pure
//! logic lives in [`transform`], [`crop`], [`snap`], [`model`] and [`preview`], which are tested
//! natively; this module is the Leptos component that wires them to the page.
//!
//! Docs: crates/jstream-ui/README.md#canvas

pub mod camera;
pub mod crop;
pub mod group;
pub mod live;
pub mod model;
pub mod preview;
pub mod screen;
pub mod snap;
pub mod taps;
pub mod transform;

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::item_menu;
use crate::menu::{Entry, Menus};
use crate::panels::add_source::AddSourceDialog;
use crate::panels::filters::{FilterDialogs, scene_owner, show_filters};
use crate::store::Studio;
use crate::workspace::DockState;
use crop::{CropGesture, Picture};
use futures_util::StreamExt;
use group::{GROUP_ROTATE_OFFSET, GroupGesture, GroupKind};
use jstream_control::protocol::{
    CameraView, Crop, GestureId, HistoryOp, ItemGeometry, ItemId, Placed, Rev, SceneCamera, SceneId,
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
/// The outline of an item that is neither selected nor under the pointer, drawn only
/// while the live picture isn't under the boxes, since then they are all there is to see.
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
    /// Turns a 3D scene's camera about its target, or with `dolly` moves it along its
    /// line of sight (ADR-0094): Alt-drag, with Shift for the dolly.
    Orbit {
        /// The scene whose camera the drag moves: the one shown when it started, which
        /// its preview is kept with and its commit goes to.
        scene: SceneId,
        /// The camera when the drag started.
        start: SceneCamera,
        /// Where the press was.
        origin: Point,
        /// Whether the drag dollies rather than orbits.
        dolly: bool,
    },
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
    /// In a 3D scene, the camera and the box whose depth plane a move follows: the
    /// pointer is taken to that plane before the gesture sees it, and nothing snaps.
    plane: Option<(CameraView, ItemGeometry)>,
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
    // The camera an orbit or dolly shows, and its scene, until the collection has it.
    let camera_preview = RwSignal::new(None::<camera::Preview>);
    let hovered = RwSignal::new(None::<ItemId>);
    let cursor = RwSignal::new("default");
    let stage = NodeRef::<leptos::html::Div>::new();
    // The canvas area around the frame: it takes the pointer, so a marquee can start on
    // the margin and a handle reaching past the frame can still be grabbed.
    let area = NodeRef::<leptos::html::Div>::new();
    let screen = NodeRef::<leptos::html::Canvas>::new();
    let preview_state = RwSignal::new(PreviewState::Starting);
    // What the canvas did with its preview pictures, reported to the engine
    // (`preview.report`, ADR-0132): each time it shows its waiting text or why it has
    // no picture counts once.
    let tally = crate::views::ViewTally::default();
    {
        let tally = tally.clone();
        Effect::new(move |before: Option<PreviewState>| {
            let now = preview_state.get();
            if before.as_ref() != Some(&now) {
                match now {
                    PreviewState::Starting => tally.placeholder(),
                    PreviewState::Unavailable(_) => tally.blank(),
                    PreviewState::Live => {}
                }
            }
            now
        });
    }
    // Kept where a `Copy` closure can reach it.
    let view_tally = StoredValue::new_local(tally.clone());
    let reporting = StoredValue::new_local(None::<IntervalHandle>);
    on_cleanup(move || {
        if let Some(handle) = reporting.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    });
    // What the canvas's preview stream shows: the program mix while the canvas shows
    // the program scene, else the scene it shows.
    let wanted = Memo::new(move |_| {
        let shown = layout.with(|l| {
            l.as_ref()
                .and_then(|l| l.scene.as_ref().map(|s| s.0.clone()))
        });
        studio
            .program
            .with(|program| preview::stream_scene(shown.as_ref(), program.as_ref()))
    });
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
    // channel and one preview stream on it, and draw its frames. Frames of a dialog's
    // own picture go to its tap.
    let stream = StoredValue::new_local(None::<preview::Stream>);
    // The rate the live view asks for: the collection's base rate (ADR-0119).
    let live_fps = Memo::new(move |_| {
        studio
            .collection
            .with(|c| preview::live_fps(c.as_ref().map_or(preview::FPS, |c| c.base_fps)))
    });
    let taps = use_context::<taps::PreviewTaps>();
    {
        let alive = Arc::clone(&alive);
        let tally = tally.clone();
        screen.on_load(move |element| {
            let Some(open) = session.and_then(|s| s.get()) else {
                return;
            };
            let frames = open.control.binary();
            // Only a weak handle is kept, so disconnecting still closes the socket.
            let control = Rc::downgrade(&open.control);
            drop(open);
            let (width, height) = stage.get_untracked().map_or((640.0, 360.0), |stage| {
                (stage.client_width() as f32, stage.client_height() as f32)
            });
            let ratio = window().device_pixel_ratio() as f32;
            let mut first = preview::Stream::first(
                (width, height, ratio),
                screen::Screen::supports_h264(),
                live_fps.get_untracked(),
            );
            // What the canvas shows by now.
            first.scene = wanted.get_untracked();
            stream.set_value(Some(first));
            reporting.set_value(crate::views::report_every_second(
                control.clone(),
                crate::views::label("canvas", None),
                tally.clone(),
            ));
            spawn_local(run_preview(
                frames,
                (control, taps, tally.clone()),
                screen::Screen::new(
                    element,
                    move || {
                        // Shown once the first picture is drawn, unless the stream has
                        // already ended.
                        if preview_state.try_get_untracked() == Some(PreviewState::Starting) {
                            preview_state.try_set(PreviewState::Live);
                        }
                    },
                    tally.clone(),
                ),
                stream,
                preview_state,
                alive,
            ));
        });
    }
    // Closes the current stream and opens `next` in its place.
    let switch = move |current: preview::Stream, next: preview::Stream| {
        let Some(tally) = view_tally.try_get_value() else {
            return;
        };
        stream.set_value(Some(next.clone()));
        // A picture already shown stays until the new stream draws (#1205).
        preview_state.update(|state| *state = state.resubscribed());
        let Some(open) = session.and_then(|s| s.get()) else {
            return;
        };
        let control = Rc::clone(&open.control);
        spawn_local(async move {
            use jstream_control::protocol::{Action, Call};
            let close = Action::PreviewUnsubscribe {
                channel: current.channel,
            };
            let _closed = control.call(Call::Action { action: close }).await;
            // A newer switch has taken over.
            if stream.try_get_value().flatten().map(|s| s.channel) != Some(next.channel) {
                return;
            }
            tally.opened();
            if let Err(error) = control
                .call(Call::Action {
                    action: next.subscribe(),
                })
                .await
            {
                preview_state.try_set(PreviewState::Unavailable(error.to_string()));
            }
        });
    };
    // Another scene shown: close the stream and open one of it (CV-03), once what the
    // canvas wants has held for `preview::SETTLE`. A scene switch changes the selection
    // before the program, so `wanted` goes program, the scene, program again: settling
    // keeps the program stream through it, and its transition plays without a stall
    // (#1205).
    let resubscribe = move |want: Option<SceneId>| {
        let Some(current) = stream.try_get_value().flatten() else {
            return;
        };
        let Some(now) = wanted.try_get_untracked() else {
            return;
        };
        let Some(next) = preview::settled_switch(&current, &want, &now) else {
            return;
        };
        switch(current, next);
    };
    // A new base rate (or the first collection, read after the stream opened): the same
    // picture at the new rate.
    Effect::new(move |_| {
        let fps = live_fps.get();
        let Some(current) = stream.try_get_value().flatten() else {
            return;
        };
        if let Some(next) = current.at_fps(fps) {
            switch(current, next);
        }
    });
    Effect::new(move |_| {
        let want = wanted.get();
        if stream
            .get_value()
            .is_none_or(|current| current.scene == want)
        {
            return;
        }
        set_timeout(move || resubscribe(want), preview::SETTLE);
    });
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

    // Sends an orbit's camera as one undoable `scene.set_camera`, ending its gesture,
    // and shows it until the collection has it; a failure drops it.
    let commit_camera =
        move |camera: SceneCamera, scene_id: SceneId, gesture: Option<GestureId>| {
            let Some(open) = session.and_then(|s| s.get()) else {
                camera_preview.set(None);
                return;
            };
            let sent = camera::Preview {
                scene: scene_id.clone(),
                camera,
            };
            spawn_local(async move {
                let result =
                    model::commit_camera(&*open.control, scene_id, camera, gesture.clone()).await;
                if let Err(error) = result {
                    problem(&error);
                    if let Some(gesture) = gesture {
                        let _cancelled = model::live_cancel(&*open.control, gesture).await;
                    }
                    if camera_preview.try_get_untracked() == Some(Some(sent)) {
                        camera_preview.try_set(None);
                    }
                }
                drop(open);
            });
        };
    // The collection has the orbit's camera, or another scene is shown: stop showing it.
    Effect::new(move |_| {
        let shown = layout.with(|l| l.as_ref().map(|l| (shown_scene(l).cloned(), l.camera)));
        if drag.with_value(|d| {
            matches!(
                d,
                Some(Drag {
                    action: Action::Orbit { .. },
                    ..
                })
            )
        }) {
            return;
        }
        if camera_preview.with_untracked(|p| {
            p.as_ref()
                .is_some_and(|p| p.caught_up(shown.as_ref().map(|(s, c)| (s.as_ref(), *c))))
        }) {
            camera_preview.set(None);
        }
    });

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
    // The camera a 3D scene is seen through as the canvas shows it, the dragged one
    // first (ADR-0094); `None` in a 2D scene.
    let seen_in = move |current: &Layout| -> Option<CameraView> {
        let camera = camera_preview.with_untracked(|p| {
            camera::Preview::camera_in(p.as_ref(), shown_scene(current), current.camera)
        });
        camera::view(camera, current.canvas)
    };
    // The item under `point`: the topmost box, or in a 3D scene the one the engine
    // draws on top through the camera.
    let hit = move |current: &Layout, point: Point| -> Option<ItemId> {
        match seen_in(current) {
            Some(view) => camera::topmost_at(current.boxes(), &view, point).cloned(),
            None => transform::topmost_at(current.boxes(), point).cloned(),
        }
    };
    // The handle of the one selected item under `point`: in crop mode the resize
    // handles sit on its picture and crop, with Alt the box's do; the picture is given
    // when the handle crops. With no picture known there is nothing to crop, so the
    // box's handles resize as usual.
    let single_handle = move |current: &Layout,
                              id: &ItemId,
                              geometry: &ItemGeometry,
                              fit: Viewport,
                              point: Point,
                              alt: bool|
          -> Option<(Handle, Option<Picture>)> {
        // A 3D scene's boxes have no handles: their size, angles and depth are edited in
        // the Transform panel.
        if current.camera.is_some() {
            return None;
        }
        let picture = picture_of(current, id, geometry);
        if crop_mode.get_untracked()
            && let Some(picture) = picture
        {
            if let Some(Handle::Rotate) = transform::handle_at(geometry, point, fit) {
                return Some((Handle::Rotate, None));
            }
            let handles = picture.geometry(geometry)?;
            return match transform::handle_at(&handles, point, fit)? {
                Handle::Rotate => None,
                handle => Some((handle, Some(picture))),
            };
        }
        match transform::handle_at(geometry, point, fit)? {
            handle @ Handle::Resize { .. } if alt => Some((handle, picture)),
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
    let start = move |event: &ev::PointerEvent,
                      point: Point,
                      mut action: Action,
                      current: &Layout| {
        let moving: Vec<ItemId> = match &action {
            Action::One { item_id, .. } | Action::Crop { item_id, .. } => vec![item_id.clone()],
            Action::Group(gesture) => gesture.starts.iter().map(|(id, _)| id.clone()).collect(),
            Action::Marquee { .. } | Action::Orbit { .. } => Vec::new(),
        };
        // In a 3D scene a move follows the pointer on the moved box's depth plane, so
        // the gesture starts from the plane point under the press.
        let seen = seen_in(current);
        let plane = match (&mut action, seen) {
            (Action::One { gesture, .. }, Some(view)) if gesture.kind == GestureKind::Move => {
                let start = gesture.start;
                camera::on_plane(&start, &view, point).map(|origin| {
                    gesture.origin = origin;
                    (view, start)
                })
            }
            (Action::Group(gesture), Some(view)) if gesture.kind == GroupKind::Move => {
                gesture.starts.first().map(|(_, g)| *g).and_then(|start| {
                    camera::on_plane(&start, &view, point).map(|origin| {
                        gesture.origin = origin;
                        (view, start)
                    })
                })
            }
            _ => None,
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
            Action::Marquee { .. } | Action::Orbit { .. } => None,
        });
        // A box drag opens a gesture, so the output follows it before the drop. On a
        // keyframe scene its values are the keyframe's (protocol 0.53, #846): they name
        // the keyframe, as its commit does.
        let gesture = match (&action, session, current.scene.as_ref()) {
            (
                Action::One { .. } | Action::Crop { .. } | Action::Group(_) | Action::Orbit { .. },
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
            plane,
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
        // A locked item in the selection gives it no handles (ADR-0096).
        let held = selection.iter().any(|(id, _)| current.is_locked(id));
        // Alt-drag in a 3D scene orbits its camera, with Shift dollies it.
        if event.alt_key()
            && let Some(scene) = shown_scene(&current).cloned()
            && let Some(camera) = camera_preview.with_untracked(|p| {
                camera::Preview::camera_in(p.as_ref(), Some(&scene), current.camera)
            })
        {
            let orbit = Action::Orbit {
                scene,
                start: camera,
                origin: point,
                dolly: shift,
            };
            start(&event, point, orbit, &current);
            return;
        }
        // A handle of the selection: the group's when several items are selected, else
        // the one item's. A 3D scene's boxes have none.
        let flat = current.camera.is_none() && !held;
        if flat && selection.len() > 1 {
            let bounds = Bounds::around(selection.iter().map(|(_, g)| g));
            if let Some(handle) = bounds.and_then(|b| group::handle_at(b, point, fit))
                && let Some(gesture) = GroupGesture::new(handle.into(), selection.clone(), point)
            {
                start(&event, point, Action::Group(gesture), &current);
                return;
            }
        } else if flat
            && let Some((item_id, geometry)) = selection.first()
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
        let Some(hit) = hit(&current, point) else {
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
        let mut moving: Vec<(ItemId, ItemGeometry)> = if was_selected {
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
        // The click selects a locked item, but a drag moves only the unlocked ones.
        moving.retain(|(id, _)| !current.is_locked(id));
        let action = match moving.as_slice() {
            [] => return,
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
        let held = selection.iter().any(|(id, _)| current.is_locked(id));
        let handle = if held || current.camera.is_some() {
            None
        } else if selection.len() > 1 {
            Bounds::around(selection.iter().map(|(_, g)| g))
                .and_then(|b| group::handle_at(b, point, fit))
                .map(|h| (h, false))
        } else {
            selection.first().and_then(|(id, g)| {
                single_handle(&current, id, g, fit, point, alt).map(|(h, p)| (h, p.is_some()))
            })
        };
        let under = hit(&current, point);
        let on_selected = under
            .as_ref()
            .is_some_and(|id| selection.iter().any(|(s, _)| s == id) && !current.is_locked(id));
        let next = match handle {
            None if alt && current.camera.is_some() => "grab",
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
        let plane = current.plane;
        let targets = (snapping.get_untracked() && plane.is_none()).then_some(&current.targets);
        let tolerance = fit.canvas_len(SNAP_DISTANCE);
        // The pointer as a move follows it: on the moved box's depth plane in a 3D scene.
        let follow = |at: Point| match &plane {
            Some((view, start)) => camera::on_plane(start, view, at),
            None => Some(at),
        };
        match &mut current.action {
            Action::Orbit {
                scene,
                start,
                origin,
                dolly,
            } => {
                let dx = (point.x - origin.x) * fit.scale;
                let dy = (point.y - origin.y) * fit.scale;
                let moved = if *dolly {
                    camera::dolly(*start, dy)
                } else {
                    camera::orbit(*start, dx, dy)
                };
                camera_preview.set(Some(camera::Preview {
                    scene: scene.clone(),
                    camera: moved,
                }));
                live.with_value(|l| l.iter().for_each(|l| l.show_camera(moved)));
            }
            Action::Marquee { from, to, base } => {
                *to = point;
                marquee.set(Some((*from, *to)));
                let hits = layout.with_untracked(|l| {
                    let Some(l) = l.as_ref() else {
                        return Vec::new();
                    };
                    // A 3D scene's boxes are touched where the camera shows them.
                    match seen_in(l) {
                        Some(view) => {
                            let shown: Vec<(ItemId, ItemGeometry)> = l
                                .boxes()
                                .filter_map(|(id, g)| {
                                    Some((id.clone(), camera::screen_box(g, &view)?))
                                })
                                .collect();
                            group::marquee(shown.iter().map(|(id, g)| (id, g)), *from, *to)
                        }
                        None => group::marquee(l.boxes(), *from, *to),
                    }
                });
                studio.select_items(base.iter().cloned().chain(hits));
            }
            Action::One { item_id, gesture } => {
                let at = current.pointer.follow(point, event.shift_key());
                if let Some(at) = follow(at)
                    && let Some(update) = gesture.update(at, modifiers, targets, tolerance)
                {
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
                if let Some(at) = follow(at)
                    && let Some(update) = gesture.update(at, modifiers, targets, tolerance)
                {
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
            Action::Orbit { scene, start, .. } => {
                let gesture = live.with_value(Clone::clone);
                live.set_value(None);
                let shown = layout.with_untracked(|l| l.as_ref().and_then(shown_scene).cloned());
                let moved = camera_preview.with_untracked(|p| {
                    camera::Preview::to_commit(p.as_ref(), &scene, shown.as_ref(), keep, start)
                });
                match (moved, gesture) {
                    (Some(camera), Some(gesture)) => {
                        gesture.commit(move |scene, ending| commit_camera(camera, scene, ending));
                    }
                    (Some(camera), None) => commit_camera(camera, scene, None),
                    (None, gesture) => {
                        if let Some(gesture) = gesture {
                            gesture.cancel();
                        }
                        camera_preview.set(None);
                    }
                }
                return;
            }
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

    let item_cx = item_menu::Context::from_page();
    let pasting = crate::clipboard::Pasting::from_page();
    // The selected items of the scene shown, for the edit keys.
    let chosen_here = move || {
        let scene = layout.with_untracked(|l| l.as_ref()?.scene.as_ref().map(|s| s.0.clone()));
        scene.map(|scene| (scene, chosen.get_untracked()))
    };
    // A keyframe scene's items are its parent's, which its edit keys don't add or remove.
    let in_keyframe =
        move || layout.with_untracked(|l| l.as_ref().is_some_and(|l| l.keyframe_of.is_some()));
    let on_key = move |event: ev::KeyboardEvent| {
        let key = event.key();
        let command = event.ctrl_key() || event.meta_key();
        match key.as_str() {
            // Ctrl+D duplicates as a reference, Ctrl+Shift+D as a clone (KB-04).
            "d" | "D" if command => {
                if !in_keyframe()
                    && let (Some(cx), Some((scene, items))) = (item_cx, chosen_here())
                {
                    cx.duplicate_selected(&scene, &items, event.shift_key());
                }
            }
            "Delete" => {
                if in_keyframe() {
                    return event.prevent_default();
                }
                let Some((cx, (scene, items))) = item_cx.zip(chosen_here()) else {
                    return;
                };
                // Only the shown scene's items: the selection can outlive one, after an
                // undo or another client's delete, and one missing ID fails the batch.
                let items: Vec<ItemId> = studio.collection.with_untracked(|c| {
                    let Some(shown) = c.as_ref().and_then(|c| c.scene(&scene)) else {
                        return Vec::new();
                    };
                    items
                        .into_iter()
                        .filter(|id| shown.items.iter().any(|i| i.item_id == *id))
                        .collect()
                });
                if items.is_empty() {
                    return;
                }
                cx.remove(&items);
            }
            // Ctrl+C copies the selection, Ctrl+V pastes into the shown scene (KB-05).
            "c" | "C" if command => {
                if let (Some(p), Some((scene, items))) = (pasting, chosen_here()) {
                    p.copy_items(&scene, &items);
                }
            }
            "v" | "V" if command => {
                if !in_keyframe()
                    && let (Some(p), Some((scene, _))) = (pasting, chosen_here())
                {
                    event.prevent_default();
                    p.paste(&scene);
                }
            }
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
                    .filter(|(id, _)| !current.is_locked(id))
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
    let on_double = move |event: ev::MouseEvent| {
        let Some((_, point)) = canvas_at(event.client_x(), event.client_y()) else {
            return;
        };
        let hit = layout.with_untracked(|l| l.as_ref().and_then(|l| hit(l, point)));
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
    let scene_dialog = use_context::<crate::panels::scene_properties::SceneDialog>();
    let filters = use_context::<FilterDialogs>();
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
                let hit = hit(l, point)?;
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
            let paste_into = scene.clone();
            let add = Entry::item("Add source…", move || {
                if let (Some(adder), Some(scene)) = (adder, scene.clone()) {
                    adder.open(scene, false);
                }
            })
            .disabled_if(adder.is_none() || !has_scene);
            let paste = Entry::item("Paste", move || {
                if let (Some(p), Some(scene)) = (pasting, paste_into.as_ref()) {
                    p.paste(scene);
                }
            })
            .hint("Ctrl+V")
            .disabled_if(!pasting.is_some_and(|p| p.clipboard.has_items()) || in_keyframe());
            vec![select, snap, Entry::Sep, add, paste]
        } else {
            // The scene's transitions and Properties, for the scene the canvas shows.
            let transitions = |into: bool| match (&scene, session) {
                (Some(scene), Some(session)) => {
                    crate::panels::studio::override_submenu(studio, session, dock, scene, into)
                }
                _ => Entry::submenu(
                    if into {
                        "Transition in"
                    } else {
                        "Transition out"
                    },
                    Vec::new(),
                ),
            };
            let props_scene = scene.clone();
            vec![
                crate::projector::pop_out(studio, scene.clone()),
                crate::screenshot::entry(
                    "Screenshot (scene)",
                    studio,
                    session,
                    scene.clone().map(crate::screenshot::Shot::Scene),
                ),
                Entry::Sep,
                select,
                snap,
                Entry::Sep,
                transitions(true),
                transitions(false),
                Entry::Sep,
                Entry::item("Scene properties…", move || {
                    if let (Some(dialog), Some(scene)) = (scene_dialog, props_scene.clone()) {
                        dialog.open(scene);
                    }
                })
                .disabled_if(scene_dialog.is_none() || !has_scene),
                Entry::item("Edit scene filters…", move || {
                    let owner = scene.as_ref().and_then(|scene| {
                        studio
                            .collection
                            .with_untracked(|c| c.as_ref().map(|c| scene_owner(c, scene)))
                    });
                    if let Some(owner) = owner {
                        show_filters(filters, dock, owner);
                    }
                })
                .disabled_if(!has_scene),
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
    // The preview shows the scene the canvas shows (the program mix for the program
    // scene), so it lies under the boxes once its stream draws.
    let pictured = move || {
        layout.with(|l| l.as_ref().is_some_and(|l| l.scene.is_some()))
            && preview_state.get() == PreviewState::Live
    };
    let preview_message = move || preview_state.get().message();
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
    // The camera the boxes are drawn through, in a 3D scene: the dragged one first.
    let seen_now = move || -> Option<CameraView> {
        camera_preview.with(|p| {
            layout.with(|l| {
                let l = l.as_ref()?;
                camera::view(
                    camera::Preview::camera_in(p.as_ref(), shown_scene(l), l.camera),
                    l.canvas,
                )
            })
        })
    };
    // Screen pixels in canvas units, for handle sizes and label text.
    let px = move |screen: f32| viewport.get().map_or(screen, |v| v.canvas_len(screen));

    // Over the live picture only the selection, the box under the pointer and hidden
    // items are drawn; without it every box is, faintly, with its name.
    let boxes = move || {
        let picked = chosen.get();
        let under = hovered.get();
        let pictured = pictured();
        let seen = seen_now();
        shown()
            .into_iter()
            .filter_map(|(id, name, geometry, visible)| {
                let is_selected = picked.contains(&id);
                let (stroke, width) = if is_selected {
                    (selection_colour(picked.len(), crop_mode.get()), "2")
                } else {
                    (
                        unselected_outline(under.as_ref() == Some(&id), pictured, visible)?,
                        "1",
                    )
                };
                // Through the camera in a 3D scene, so the box lies on its picture.
                let (points, bounds) = match &seen {
                    Some(view) => (
                        camera::outline(&geometry, view),
                        camera::screen_box(&geometry, view).map(|b| Bounds::of(&b))?,
                    ),
                    None => (outline(&geometry), Bounds::of(&geometry)),
                };
                let dash = if visible { "none" } else { "6 4" };
                // Just inside the box's own top-left, running along its top edge.
                let label = name_label(&geometry, seen.as_ref(), bounds, px(4.0), px(14.0));
                let turn = label
                    .rotation
                    .map(|rot| format!("rotate({} {} {})", rot.degrees, rot.cx, rot.cy));
                Some(view! {
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
                            x=label.x
                            y=label.y
                            transform=turn
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
                })
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
        // A 3D scene's selection has no handles; its readout is the camera's.
        if let Some(camera) = camera_preview.with(|p| {
            layout.with(|l| {
                let l = l.as_ref()?;
                camera::Preview::camera_in(p.as_ref(), shown_scene(l), l.camera)
            })
        }) {
            let (width, height) = canvas_size();
            let text = camera::readout(camera);
            return Some(
                pill(
                    fit,
                    text,
                    width as f32 / 2.0,
                    height as f32 - fit.canvas_len(40.0),
                    0.0,
                    ACCENT,
                )
                .into_any(),
            );
        }
        let picked = shown_selection();
        // A locked item in the selection shows its outline and readout, but no handles.
        let locked = layout.with(|l| {
            l.as_ref()
                .is_some_and(|l| chosen.with(|c| c.iter().any(|id| l.is_locked(id))))
        });
        let lock_note = |text: String| {
            if locked {
                format!("{text}  · locked")
            } else {
                text
            }
        };
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
                        {(!locked).then(|| chrome(fit, squares, top, knob, STEM_LENGTH, ROTATE_OFFSET, colour, cropping))}
                        {pill(fit, lock_note(text), middle, bounds.bottom, 10.0, colour)}
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
                        {(!locked).then(|| chrome(fit, squares, top, knob, STEM_LENGTH, GROUP_ROTATE_OFFSET, ACCENT, false))}
                        {pill(fit, lock_note(text), (o.left + o.right) / 2.0, o.bottom, 12.0, ACCENT)}
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
                        if pictured() {
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
            {move || layout.with(|l| l.as_ref()?.keyframe_of.clone()).map(|parent| view! {
                <div class="keyframe-banner" role="status">
                    "◆ Keyframe of “" {parent} "”: only placement and visibility can change"
                </div>
            })}
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

/// The outline of an item that isn't selected: a light one under the pointer, a faint one
/// while no live picture is under the boxes, else none, so a source over the picture
/// shows no box until it is selected or pointed at. A hidden item isn't in the picture
/// but still takes clicks, so it keeps its faint (dashed) outline.
fn unselected_outline(hovered: bool, pictured: bool, visible: bool) -> Option<&'static str> {
    if hovered {
        Some(HOVER)
    } else if pictured && visible {
        None
    } else {
        Some(IDLE)
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

/// Where an item's name is drawn, in canvas units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NameLabel {
    /// `<text>` x before [`NameRotation`] is applied: the corner plus the along-edge inset.
    x: f32,
    /// `<text>` y before the rotation: the corner plus the into-item inset.
    y: f32,
    /// Rotation about the top-left corner, when the name runs along the top edge.
    /// Absent, the name stays upright on the bounds.
    rotation: Option<NameRotation>,
}

/// `rotate(degrees cx cy)` about the box's top-left, in degrees clockwise.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NameRotation {
    /// The top edge's angle, from its top-left toward its top-right.
    degrees: f32,
    /// The corner the rotation is about.
    cx: f32,
    /// The corner the rotation is about.
    cy: f32,
}

/// The name just inside the box's top-left, turned to run along its top edge.
///
/// `pad_along` and `pad_in` are the insets in canvas units (4 and 14 screen pixels,
/// through the viewport). They are added in the unrotated frame and the SVG rotation
/// about the corner carries them onto the edge, which is how an upright box's name,
/// at its bounds' top-left, is drawn. The glyphs grow back toward the edge from that
/// baseline. `flip_h` and `flip_v` mirror the picture, not the box
/// ([`ItemGeometry`]), so the corner stays the box's own top-left. The angle is the
/// edge's own, including past a quarter turn, so the name stays with that corner
/// instead of jumping to the other end to stay upright.
///
/// Corners come from [`ItemGeometry::project_seen`]: the camera's when `view` is set,
/// otherwise the item's own projection, tilt and turn included. When the top-left or
/// the far end of the top edge is behind the viewer, or the edge has no direction (a
/// box turned edge-on), the name stays upright on `bounds`. A mirrored projection (a
/// turn past the side, or a camera behind the item) insets into the box. The glyphs
/// still grow toward the rotated frame's negative y, so on a mirrored box the name
/// sits about one line deeper than on an unmirrored one.
fn name_label(
    geometry: &ItemGeometry,
    view: Option<&CameraView>,
    bounds: Bounds,
    pad_along: f32,
    pad_in: f32,
) -> NameLabel {
    let upright = NameLabel {
        x: bounds.left + pad_along,
        y: bounds.top + pad_in,
        rotation: None,
    };
    let Ok(quad) = geometry.project_seen(view) else {
        return upright;
    };
    let at = |placed: Placed| match placed {
        Placed::At(point) if point.x.is_finite() && point.y.is_finite() => Some(Point {
            x: point.x,
            y: point.y,
        }),
        _ => None,
    };
    let (Some(top_left), Some(top_right)) = (at(quad.corners[0]), at(quad.corners[1])) else {
        return upright;
    };
    let dx = top_right.x - top_left.x;
    let dy = top_right.y - top_left.y;
    // An edge-on top (a 90° turn, drawn flat) has no direction to run along.
    if dx.hypot(dy) < 1.0e-3 {
        return upright;
    }
    // A third corner says which side of the edge the box is on. Positive is clockwise
    // from the edge in this y-down frame, which is the side the +y inset lands on.
    let pad_in = match at(quad.corners[3]).or_else(|| at(quad.corners[2])) {
        Some(inside) => {
            let cross = dx * (inside.y - top_left.y) - dy * (inside.x - top_left.x);
            if cross < 0.0 { -pad_in } else { pad_in }
        }
        None => pad_in,
    };
    NameLabel {
        x: top_left.x + pad_along,
        y: top_left.y + pad_in,
        rotation: Some(NameRotation {
            // Adding zero turns -0 into 0.
            degrees: dy.atan2(dx).to_degrees() + 0.0,
            cx: top_left.x,
            cy: top_left.y,
        }),
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

/// Runs the canvas's preview stream: subscribes, then draws each frame of the current
/// stream's channel until the session, the stream or the canvas ends, handing each
/// frame of another channel to its tap ([`taps`]). When the
/// browser's H.264 decoder fails, it subscribes again for JPEG only. Another scene
/// shown opens a new stream (the canvas's effect does), whose frames it then draws.
async fn run_preview(
    mut frames: jstream_control::BinaryStream,
    (control, taps, tally): (
        std::rc::Weak<dyn jstream_control::Control>,
        Option<taps::PreviewTaps>,
        crate::views::ViewTally,
    ),
    screen: Option<screen::Screen>,
    stream: StoredValue<Option<preview::Stream>, LocalStorage>,
    state: RwSignal<PreviewState>,
    alive: Arc<AtomicBool>,
) {
    let Some(mut screen) = screen else {
        // No stream, so another scene shown opens none either.
        stream.set_value(None);
        state.set(PreviewState::Unavailable(
            "this browser can't draw on a canvas".into(),
        ));
        return;
    };
    let current = move || stream.try_get_value().flatten();
    let subscribe = |action: jstream_control::protocol::Action| {
        let control = control.clone();
        async move {
            let control = control.upgrade()?;
            Some(
                control
                    .call(jstream_control::protocol::Call::Action { action })
                    .await,
            )
        }
    };
    let Some(first) = current() else { return };
    tally.opened();
    match subscribe(first.subscribe()).await {
        Some(Ok(_)) => {}
        // The canvas shows why; the channel is still read, for the taps' frames.
        Some(Err(error)) => {
            state.try_set(PreviewState::Unavailable(error.to_string()));
        }
        None => return,
    }
    while let Some(frame) = frames.next().await {
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        let header = frame.header;
        let Some(now) = current() else { return };
        // A tap's frame, or a late frame of a stream already closed.
        if header.channel != now.channel {
            if let Some(taps) = taps.and_then(|taps| taps.get()) {
                taps.deliver(&frame);
            }
            continue;
        }
        if header.end {
            let reason = String::from_utf8_lossy(&frame.payload).into_owned();
            state.try_set(PreviewState::Unavailable(reason));
            continue;
        }
        if let Err(reason) = screen.show(&header, &frame.payload)
            && !screen.h264_failed()
        {
            state.try_set(PreviewState::Unavailable(reason));
            continue;
        }
        if state.try_get_untracked() == Some(PreviewState::Starting) {
            // The first frame of a stream opened for another scene.
            state.try_set(PreviewState::Live);
        }
        if now.h264 && screen.h264_failed() {
            // Fall back to JPEG once, for this stream and every later one. The channel
            // can only be reused once its stream is closed, so close it first.
            let jpeg = preview::Stream { h264: false, ..now };
            stream.try_set_value(Some(jpeg.clone()));
            let close = jstream_control::protocol::Action::PreviewUnsubscribe {
                channel: jpeg.channel,
            };
            if subscribe(close).await.is_none() {
                return;
            }
            // A switch while the close was on its way has closed this channel and
            // opened the next one: reopening it would leave a stream nobody draws.
            if stream.try_get_value().flatten().map(|s| s.channel) != Some(jpeg.channel) {
                continue;
            }
            tally.opened();
            if let Some(Err(error)) = subscribe(jpeg.subscribe()).await {
                state.try_set(PreviewState::Unavailable(error.to_string()));
            }
        }
    }
}

/// The scene the canvas shows.
fn shown_scene(layout: &Layout) -> Option<&SceneId> {
    layout.scene.as_ref().map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Canvas, Finite32, Size, Vec2};

    /// The baseline after the SVG rotation, where the name is actually drawn.
    fn baseline(label: NameLabel) -> Point {
        let Some(rot) = label.rotation else {
            return Point {
                x: label.x,
                y: label.y,
            };
        };
        let (sin, cos) = rot.degrees.to_radians().sin_cos();
        let dx = label.x - rot.cx;
        let dy = label.y - rot.cy;
        Point {
            x: rot.cx + dx * cos - dy * sin,
            y: rot.cy + dx * sin + dy * cos,
        }
    }

    /// Signed distance of `point` from the edge `from` → `to`: positive on the clockwise
    /// side, in canvas units.
    fn side(from: Point, to: Point, point: Point) -> f32 {
        let dx = to.x - from.x;
        let dy = to.y - from.y;
        (dx * (point.y - from.y) - dy * (point.x - from.x)) / dx.hypot(dy)
    }

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
    fn unselected_boxes_hide_over_the_live_picture() {
        assert_eq!(unselected_outline(false, true, true), None);
        assert_eq!(unselected_outline(true, true, true), Some(HOVER));
        assert_eq!(unselected_outline(false, false, true), Some(IDLE));
        assert_eq!(unselected_outline(true, false, true), Some(HOVER));
        // A hidden item still takes clicks, so it stays outlined over the picture.
        assert_eq!(unselected_outline(false, true, false), Some(IDLE));
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

    #[test]
    fn an_upright_name_stays_on_the_bounds_corner() {
        let g = geometry(100.0, 80.0, 200.0, 100.0, 0.0);
        let bounds = Bounds::of(&g);
        let label = name_label(&g, None, bounds, 4.0, 14.0);
        let rot = label.rotation.expect("the top edge is drawn");
        assert!(rot.degrees.abs() < 1e-4, "{}", rot.degrees);
        let at = baseline(label);
        assert!((at.x - (bounds.left + 4.0)).abs() < 1e-3, "{at:?}");
        assert!((at.y - (bounds.top + 14.0)).abs() < 1e-3, "{at:?}");
    }

    #[test]
    fn the_name_follows_the_rotated_top_left() {
        // The reported case: −16°, where the upright bounds' corner floats off the box.
        let g = geometry(960.0, 540.0, 400.0, 200.0, -16.0);
        let bounds = Bounds::of(&g);
        let label = name_label(&g, None, bounds, 4.0, 14.0);
        let quad = g.project().expect("placed");
        let Placed::At(c0) = quad.corners[0] else {
            panic!("in front")
        };
        let Placed::At(c1) = quad.corners[1] else {
            panic!("in front")
        };
        let Placed::At(c3) = quad.corners[3] else {
            panic!("in front")
        };
        let top_left = Point { x: c0.x, y: c0.y };
        let top_right = Point { x: c1.x, y: c1.y };
        let bottom_left = Point { x: c3.x, y: c3.y };
        let rot = label.rotation.expect("turned with the box");
        assert!((rot.cx - top_left.x).abs() < 1e-3 && (rot.cy - top_left.y).abs() < 1e-3);
        let edge = (top_right.y - top_left.y)
            .atan2(top_right.x - top_left.x)
            .to_degrees();
        assert!(
            (rot.degrees - edge).abs() < 1e-3,
            "{} vs {edge}",
            rot.degrees
        );
        // Not the old place: the upright bounds' top-left, off the rotated corner.
        assert!((bounds.left - top_left.x).abs() > 1.0 || (bounds.top - top_left.y).abs() > 1.0);
        let at = baseline(label);
        let along = {
            let dx = top_right.x - top_left.x;
            let dy = top_right.y - top_left.y;
            let len = dx.hypot(dy);
            ((at.x - top_left.x) * dx + (at.y - top_left.y) * dy) / len
        };
        assert!((along - 4.0).abs() < 1e-2, "{along}");
        let inside = side(top_left, top_right, bottom_left).signum();
        let inset = side(top_left, top_right, at);
        assert!((inset.abs() - 14.0).abs() < 1e-2, "{inset}");
        assert!(
            inset.signum() == inside,
            "the name is outside the box: {inset}"
        );
    }

    #[test]
    fn a_flip_does_not_move_the_name() {
        let mut g = geometry(960.0, 540.0, 400.0, 200.0, -16.0);
        let plain = name_label(&g, None, Bounds::of(&g), 4.0, 14.0);
        g.flip_h = true;
        g.flip_v = true;
        let flipped = name_label(&g, None, Bounds::of(&g), 4.0, 14.0);
        assert_eq!(plain, flipped);
    }

    #[test]
    fn a_mirrored_turn_insets_the_name_into_the_box() {
        let mut g = geometry(100.0, 100.0, 200.0, 100.0, 0.0);
        g.turn_deg = Finite32::new(180.0).expect("finite");
        let label = name_label(&g, None, Bounds::of(&g), 4.0, 14.0);
        let quad = g.project().expect("placed");
        let Placed::At(c0) = quad.corners[0] else {
            panic!("in front")
        };
        let Placed::At(c1) = quad.corners[1] else {
            panic!("in front")
        };
        let Placed::At(c3) = quad.corners[3] else {
            panic!("in front")
        };
        let top_left = Point { x: c0.x, y: c0.y };
        let top_right = Point { x: c1.x, y: c1.y };
        let inside = side(top_left, top_right, Point { x: c3.x, y: c3.y }).signum();
        let inset = side(top_left, top_right, baseline(label));
        assert!(
            (inset.abs() - 14.0).abs() < 1e-2 && inset.signum() == inside,
            "inset {inset}, inside {inside}"
        );
    }

    #[test]
    fn an_edge_on_top_keeps_the_upright_name() {
        let mut g = geometry(100.0, 100.0, 200.0, 100.0, 0.0);
        g.turn_deg = Finite32::new(90.0).expect("finite");
        let bounds = Bounds::of(&g);
        let label = name_label(&g, None, bounds, 4.0, 14.0);
        assert!(label.rotation.is_none());
        assert!((label.x - (bounds.left + 4.0)).abs() < 1e-3);
        assert!((label.y - (bounds.top + 14.0)).abs() < 1e-3);
    }

    /// #1054's item: close to the camera, tilted nearly flat and long enough that one
    /// end reaches behind it. `tilt` positive moves the top away.
    fn partly_behind(tilt: f32) -> (Canvas, CameraView, ItemGeometry) {
        let canvas = Canvas {
            width: 1920,
            height: 1080,
        };
        let seen = camera::view(Some(SceneCamera::framing(canvas)), canvas).expect("camera");
        let camera_z = SceneCamera::framing(canvas).position.z.get();
        let mut g = geometry(960.0, 560.0, 100.0, 2000.0, 0.0);
        g.depth_px = Finite32::new(camera_z - 200.0).expect("finite");
        g.tilt_deg = Finite32::new(tilt).expect("finite");
        (canvas, seen, g)
    }

    #[test]
    fn a_top_edge_behind_the_camera_keeps_the_upright_name() {
        let (_canvas, seen, g) = partly_behind(-80.0);
        let quad = g.project_seen(Some(&seen)).expect("projected");
        assert!(
            matches!(quad.corners[0], Placed::BehindViewer)
                || matches!(quad.corners[1], Placed::BehindViewer),
            "the top edge is in front: {quad:?}"
        );
        let bounds = Bounds::of(&camera::screen_box(&g, &seen).expect("its front is drawn"));
        let label = name_label(&g, Some(&seen), bounds, 4.0, 14.0);
        assert!(label.rotation.is_none());
        assert!((label.x - (bounds.left + 4.0)).abs() < 1e-3);
        assert!((label.y - (bounds.top + 14.0)).abs() < 1e-3);
    }

    #[test]
    fn a_top_edge_in_front_is_named_though_the_bottom_is_behind() {
        let (_canvas, seen, g) = partly_behind(80.0);
        let quad = g.project_seen(Some(&seen)).expect("projected");
        let (Placed::At(c0), Placed::At(c1)) = (quad.corners[0], quad.corners[1]) else {
            panic!("the top edge is behind: {quad:?}");
        };
        assert!(
            matches!(quad.corners[2], Placed::BehindViewer)
                || matches!(quad.corners[3], Placed::BehindViewer)
        );
        let bounds = Bounds::of(&camera::screen_box(&g, &seen).expect("its front is drawn"));
        let label = name_label(&g, Some(&seen), bounds, 4.0, 14.0);
        let rot = label.rotation.expect("the top edge is in front");
        assert!((rot.cx - c0.x).abs() < 1e-3 && (rot.cy - c0.y).abs() < 1e-3);
        let edge = (c1.y - c0.y).atan2(c1.x - c0.x).to_degrees();
        assert!(
            (rot.degrees - edge).abs() < 1e-3,
            "{} vs {edge}",
            rot.degrees
        );
    }

    #[test]
    fn a_flat_box_through_the_framing_camera_names_its_top_left() {
        let canvas = Canvas {
            width: 1920,
            height: 1080,
        };
        let seen = camera::view(Some(SceneCamera::framing(canvas)), canvas).expect("camera");
        let g = geometry(500.0, 300.0, 400.0, 200.0, -16.0);
        let through = name_label(
            &g,
            Some(&seen),
            Bounds::of(&camera::screen_box(&g, &seen).expect("in front")),
            4.0,
            14.0,
        );
        let flat = name_label(&g, None, Bounds::of(&g), 4.0, 14.0);
        let rot_c = through.rotation.expect("edge in front");
        let rot_f = flat.rotation.expect("edge drawn");
        assert!((rot_c.cx - rot_f.cx).abs() < 0.05, "{rot_c:?} vs {rot_f:?}");
        assert!((rot_c.cy - rot_f.cy).abs() < 0.05, "{rot_c:?} vs {rot_f:?}");
        assert!(
            (rot_c.degrees - rot_f.degrees).abs() < 0.05,
            "{} vs {}",
            rot_c.degrees,
            rot_f.degrees
        );
    }
}
