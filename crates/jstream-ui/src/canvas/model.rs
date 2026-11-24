//! What the canvas edits and the calls that change it: the selected scene's items'
//! boxes, taken from the shared [`Collection`], the live values a drag shows as it
//! goes (`live.set` and `live.cancel`), `item.set_geometry` (and `item.set_crop` for a
//! crop) as one batch per finished gesture, `scene.set_camera` for a 3D scene's orbit
//! and dolly (ADR-0094), and `history.undo` and `history.redo` (ADR-0032).
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0032-live-edits-and-undo.md

use crate::canvas::camera::Shape;
use crate::store::{Collection, Sizes, SourceDepths};
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, Canvas, Crop, Edit, GestureId, HistoryCall, HistoryOp,
    ItemGeometry, ItemId, ItemInfo, PlacementForm, Rev, SceneCamera, SceneId, SceneRole,
    SolidBounds, SourceKind,
};
use jstream_control::{Control, ControlError};
use std::collections::BTreeSet;
use std::fmt;

/// Why a canvas call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasError {
    /// The engine refused the call, or the connection failed.
    Control(ControlError),
    /// The engine answered with a result of the wrong kind for the call.
    UnexpectedAnswer {
        /// The call that was sent (`item.set_geometry`, `history.undo`, …).
        call: &'static str,
    },
}

impl fmt::Display for CanvasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanvasError::Control(error) => error.fmt(f),
            CanvasError::UnexpectedAnswer { call } => {
                write!(
                    f,
                    "the engine answered {call} with the wrong kind of result"
                )
            }
        }
    }
}

impl From<ControlError> for CanvasError {
    fn from(error: ControlError) -> Self {
        CanvasError::Control(error)
    }
}

/// One item of the scene being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasItem {
    /// The item.
    pub item_id: ItemId,
    /// Its source's name, as a label.
    pub name: String,
    /// Its box. `None` for an item still stored in the 0.3 legacy form, whose box
    /// depends on the source's runtime size, which the protocol doesn't report yet; the
    /// canvas can't draw or edit it.
    pub geometry: Option<ItemGeometry>,
    /// Whether it is drawn.
    pub visible: bool,
    /// Its crop, in source pixels.
    pub crop: Crop,
    /// Its source's own size in pixels, while the engine knows it.
    pub source_size: Option<(u32, u32)>,
    /// Its source's depth in the same pixels, for a 3D model (ADR-0107 decision 8).
    pub source_depth: Option<u32>,
    /// Whether its box is locked (ADR-0096): the canvas selects it but never drags it.
    pub locked: bool,
    /// What a 3D scene outlines and hits it by: its box, its model's bounds
    /// ([`CanvasItem::solid`]), or a 3D group's children (ADR-0164 decision 6).
    pub shape: Shape,
}

/// The scene being edited, at one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The revision of the collection it was taken from.
    pub rev: Rev,
    /// The canvas every scene is laid out on.
    pub canvas: Canvas,
    /// The scene and its name, or `None` when the collection has no scene.
    pub scene: Option<(SceneId, String)>,
    /// The name of the scene's parent, when it is a keyframe scene: its items are the
    /// parent's, and their boxes are its overrides (ADR-0049).
    pub keyframe_of: Option<String>,
    /// Its items, in stacking order: the first is at the bottom.
    pub items: Vec<CanvasItem>,
    /// The camera it is seen through when it is a 3D scene (ADR-0094): its own, or on a
    /// keyframe scene its override or else its parent's.
    pub camera: Option<SceneCamera>,
}

impl CanvasItem {
    /// The bounds of the 3D model it shows, from its source's size and depth, which the
    /// canvas outlines and hits it by in a 3D scene (ADR-0107 decision 8). `None` for
    /// any other item, or while the engine knows no depth for its source.
    pub fn solid(&self) -> Option<SolidBounds> {
        let (width, height) = self.source_size?;
        let depth = self.source_depth?;
        Some(SolidBounds::of_size(
            width as f32,
            height as f32,
            depth as f32,
        ))
    }
}

impl Layout {
    /// The items with a box, in stacking order.
    pub fn boxes(&self) -> impl DoubleEndedIterator<Item = (&ItemId, &ItemGeometry)> {
        self.items
            .iter()
            .filter_map(|item| Some((&item.item_id, item.geometry.as_ref()?)))
    }

    /// [`Self::boxes`] that the canvas shows and a press can reach: the visible items, and
    /// a hidden one only while it is `selected`. A hidden item draws nothing, so it takes
    /// no clicks and no marquee or snap over what is drawn there (#1732).
    pub fn seen_boxes<'a>(
        &'a self,
        selected: &'a [ItemId],
    ) -> impl DoubleEndedIterator<Item = (&'a ItemId, &'a ItemGeometry)> {
        self.items
            .iter()
            .filter(|item| item.visible || selected.contains(&item.item_id))
            .filter_map(|item| Some((&item.item_id, item.geometry.as_ref()?)))
    }

    /// [`Self::shapes`] of [`Self::seen_boxes`]' items.
    pub fn seen_shapes<'a>(
        &'a self,
        selected: &'a [ItemId],
    ) -> impl DoubleEndedIterator<Item = (&'a ItemId, &'a ItemGeometry, &'a Shape)> {
        self.items
            .iter()
            .filter(|item| item.visible || selected.contains(&item.item_id))
            .filter_map(|item| Some((&item.item_id, item.geometry.as_ref()?, &item.shape)))
    }

    /// [`Self::boxes`], each with what a 3D scene outlines and hits it by
    /// ([`CanvasItem::shape`]).
    pub fn shapes(&self) -> impl DoubleEndedIterator<Item = (&ItemId, &ItemGeometry, &Shape)> {
        self.items
            .iter()
            .filter_map(|item| Some((&item.item_id, item.geometry.as_ref()?, &item.shape)))
    }

    /// The bounds of the model the item shows, if it shows one ([`CanvasItem::solid`]).
    pub fn solid_of(&self, item_id: &ItemId) -> Option<SolidBounds> {
        self.item(item_id)?.solid()
    }

    /// How many items are in the legacy form, which the canvas can't show.
    pub fn legacy_count(&self) -> usize {
        self.items.iter().filter(|i| i.geometry.is_none()).count()
    }

    /// The item's box, if it has one.
    pub fn geometry_of(&self, item_id: &ItemId) -> Option<ItemGeometry> {
        self.items
            .iter()
            .find(|item| &item.item_id == item_id)
            .and_then(|item| item.geometry)
    }

    /// The item, if the scene has it.
    pub fn item(&self, item_id: &ItemId) -> Option<&CanvasItem> {
        self.items.iter().find(|item| &item.item_id == item_id)
    }

    /// Whether the item is locked (ADR-0096), so no drag, handle or arrow key moves it.
    pub fn is_locked(&self, item_id: &ItemId) -> bool {
        self.item(item_id).is_some_and(|item| item.locked)
    }
}

impl Layout {
    /// The layout of `scene_id` in `collection`, or an empty one with no scene when it
    /// names none or a scene the collection doesn't have. It knows no source's size.
    pub fn of(collection: &Collection, scene_id: Option<&SceneId>) -> Layout {
        Layout::sized(collection, scene_id, &Sizes::new(), &SourceDepths::new())
    }

    /// [`Layout::of`], with each item's source size taken from `sizes` and a model's
    /// depth from `depths`.
    pub fn sized(
        collection: &Collection,
        scene_id: Option<&SceneId>,
        sizes: &Sizes,
        depths: &SourceDepths,
    ) -> Layout {
        let Some(scene) = scene_id.and_then(|id| collection.scene(id)) else {
            return Layout {
                rev: collection.rev,
                canvas: collection.canvas,
                scene: None,
                keyframe_of: None,
                items: Vec::new(),
                camera: None,
            };
        };
        let camera = collection.camera_of(&scene.scene_id);
        let mut groups = Groups {
            collection,
            sizes,
            depths,
            path: BTreeSet::from([scene.scene_id.clone()]),
            shared: BTreeSet::new(),
        };
        let items = scene
            .items
            .iter()
            .map(|item| CanvasItem {
                shape: if camera.is_some() {
                    groups.shape(item, 0)
                } else {
                    Shape::Box
                },
                item_id: item.item_id.clone(),
                name: collection.source_name(&item.source_id),
                geometry: match item.placement {
                    PlacementForm::Geometry => item.geometry,
                    _ => None,
                },
                visible: item.visible,
                crop: item.crop,
                source_size: collection.source_size(sizes, &item.source_id),
                source_depth: depths.get(&item.source_id).copied(),
                locked: item.locked,
            })
            .collect();
        Layout {
            rev: collection.rev,
            canvas: collection.canvas,
            scene: Some((scene.scene_id.clone(), scene.name.clone())),
            keyframe_of: collection
                .parent_of(&scene.scene_id)
                .map(|p| p.name.clone()),
            items,
            camera,
        }
    }
}

/// How many 3D groups deep the canvas follows a group's children, as the engine does
/// (its `MAX_NESTING`): a group placed deeper draws flat, so it is outlined by its box.
const MAX_GROUP_NESTING: usize = 8;

/// Works out the shapes of a 3D scene's items, in stacking order, as the engine plans
/// its pass (ADR-0164 decision 1): an item placing a group with the `group` role and
/// `share_space` on, by a geometry, is drawn as its children, each group shared once per
/// scene (the first visible item placing it), never through itself, and to
/// [`MAX_GROUP_NESTING`] deep. Anything else is drawn as one box.
struct Groups<'a> {
    collection: &'a Collection,
    sizes: &'a Sizes,
    depths: &'a SourceDepths,
    /// The scenes being drawn into one another, the shown one first.
    path: BTreeSet<SceneId>,
    /// The groups already shared in this scene.
    shared: BTreeSet<SceneId>,
}

impl Groups<'_> {
    /// The group `item` places when it is drawn as a 3D group at `depth` groups down.
    fn sharing(&self, item: &ItemInfo, depth: usize) -> Option<SceneId> {
        let SourceKind::Scene { scene_id } = &self.collection.source(&item.source_id)?.kind else {
            return None;
        };
        let group = self.collection.scene(scene_id)?;
        (group.role == SceneRole::Group
            && group.options.share_space
            && item.placement == PlacementForm::Geometry
            && item.geometry.is_some()
            && depth + 1 < MAX_GROUP_NESTING
            && !self.path.contains(scene_id)
            && !self.shared.contains(scene_id))
        .then(|| scene_id.clone())
    }

    /// What `item`, `depth` groups down, is outlined and hit by. A hidden item draws
    /// nothing, so it shares no group with a later item, but the canvas still outlines a
    /// hidden one of the scene's own by its group's children.
    fn shape(&mut self, item: &ItemInfo, depth: usize) -> Shape {
        let Some(group) = self.sharing(item, depth) else {
            let solid = self
                .collection
                .source_size(self.sizes, &item.source_id)
                .zip(self.depths.get(&item.source_id))
                .map(|((width, height), depth)| {
                    SolidBounds::of_size(width as f32, height as f32, *depth as f32)
                });
            return solid.map_or(Shape::Box, Shape::Model);
        };
        if item.visible {
            self.shared.insert(group.clone());
        }
        self.path.insert(group.clone());
        let parts: Vec<(ItemGeometry, Shape)> = self
            .collection
            .scene(&group)
            .into_iter()
            .flat_map(|scene| scene.items.iter())
            .filter(|child| child.visible && child.placement == PlacementForm::Geometry)
            .filter_map(|child| Some((child.geometry?, self.shape(child, depth + 1))))
            .collect();
        self.path.remove(&group);
        if parts.is_empty() {
            // Nothing of it is drawn: its box keeps it on the canvas to pick.
            return Shape::Box;
        }
        Shape::Group {
            canvas: self.collection.canvas,
            parts,
        }
    }
}

/// An `item.set_geometry` for each item in `scene_id`, then an `item.set_crop` for
/// each crop.
fn geometry_edits(
    scene_id: &SceneId,
    placed: Vec<(ItemId, ItemGeometry)>,
    crops: Vec<(ItemId, Crop)>,
) -> Vec<Edit> {
    let geometries = placed
        .into_iter()
        .map(|(item_id, geometry)| Edit::ItemSetGeometry {
            item_id,
            scene_id: Some(scene_id.clone()),
            geometry,
        });
    let crops = crops.into_iter().map(|(item_id, crop)| Edit::ItemSetCrop {
        item_id,
        scene_id: Some(scene_id.clone()),
        crop,
    });
    geometries.chain(crops).collect()
}

/// The batch that places each item in `scene_id` at its new geometry, and crops each
/// of `crops`: one batch, so one undo step however many items a gesture moved, ending
/// the drag's gesture if its live values reached the engine. It carries no
/// `expected_rev`: a drag's commit is last writer wins per field (ADR-0032).
pub fn set_geometry(
    scene_id: SceneId,
    placed: Vec<(ItemId, ItemGeometry)>,
    crops: Vec<(ItemId, Crop)>,
    gesture: Option<GestureId>,
) -> Call {
    Call::Batch(Batch {
        edits: geometry_edits(&scene_id, placed, crops),
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: gesture,
    })
}

/// Sends [`set_geometry`]'s batch and returns the revision it produced.
pub async fn commit(
    control: &dyn Control,
    scene_id: SceneId,
    placed: Vec<(ItemId, ItemGeometry)>,
    crops: Vec<(ItemId, Crop)>,
    gesture: Option<GestureId>,
) -> Result<Rev, CanvasError> {
    match control
        .call(set_geometry(scene_id, placed, crops, gesture))
        .await?
        .result
    {
        CallResult::Batch { rev, .. } => Ok(rev),
        _ => Err(CanvasError::UnexpectedAnswer {
            call: "item.set_geometry",
        }),
    }
}

/// The batch that sets `scene_id`'s camera, ending the orbit's gesture if its live
/// values reached the engine: one undo step for the whole drag.
pub fn set_camera(scene_id: SceneId, camera: SceneCamera, gesture: Option<GestureId>) -> Call {
    Call::Batch(Batch {
        edits: vec![Edit::SceneSetCamera {
            scene_id,
            camera: Some(camera),
        }],
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: gesture,
    })
}

/// Sends [`set_camera`]'s batch and returns the revision it produced.
pub async fn commit_camera(
    control: &dyn Control,
    scene_id: SceneId,
    camera: SceneCamera,
    gesture: Option<GestureId>,
) -> Result<Rev, CanvasError> {
    match control
        .call(set_camera(scene_id, camera, gesture))
        .await?
        .result
    {
        CallResult::Batch { rev, .. } => Ok(rev),
        _ => Err(CanvasError::UnexpectedAnswer {
            call: "scene.set_camera",
        }),
    }
}

/// Shows the items at these geometries and crops, and the scene through `camera`, on
/// output while a drag goes on, opening the gesture if it is new (`live.set`): the
/// collection and its revision don't change.
pub async fn live_set(
    control: &dyn Control,
    gesture: GestureId,
    scene_id: SceneId,
    placed: Vec<(ItemId, ItemGeometry)>,
    crops: Vec<(ItemId, Crop)>,
    camera: Option<SceneCamera>,
) -> Result<(), CanvasError> {
    let mut values = geometry_edits(&scene_id, placed, crops);
    values.extend(camera.map(|camera| Edit::SceneSetCamera {
        scene_id,
        camera: Some(camera),
    }));
    let action = Action::LiveSet {
        gesture_id: gesture,
        values,
    };
    control.call(Call::Action { action }).await?;
    Ok(())
}

/// Ends a gesture with no commit (`live.cancel`): the items show their stored geometry.
pub async fn live_cancel(control: &dyn Control, gesture: GestureId) -> Result<(), CanvasError> {
    let action = Action::LiveCancel {
        gesture_id: gesture,
    };
    control.call(Call::Action { action }).await?;
    Ok(())
}

/// How many steps the client can undo and redo, as the engine reports them after each
/// history call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Depths {
    /// Steps that can be undone, or `None` before the engine has said.
    pub undo: Option<u32>,
    /// Steps that can be redone, or `None` before the engine has said.
    pub redo: Option<u32>,
}

/// Undoes or redoes this client's last step (`history.undo` or `history.redo`) and
/// returns the depths the engine reports after it.
pub async fn history(control: &dyn Control, op: HistoryOp) -> Result<Depths, CanvasError> {
    let call = match op {
        HistoryOp::Redo => "history.redo",
        _ => "history.undo",
    };
    match control
        .call(Call::History(HistoryCall::new(op)))
        .await?
        .result
    {
        CallResult::History {
            undo_depth,
            redo_depth,
            ..
        } => Ok(Depths {
            undo: Some(undo_depth),
            redo: Some(redo_depth),
        }),
        _ => Err(CanvasError::UnexpectedAnswer { call }),
    }
}

/// Whether `error` is the engine saying there is nothing to undo or redo, so the UI
/// greys the button out instead of showing a problem.
pub fn is_history_empty(error: &CanvasError) -> bool {
    matches!(
        error,
        CanvasError::Control(control)
            if matches!(control.protocol(), Some(jstream_control::protocol::ProtocolError::HistoryEmpty { .. }))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::MockEngine;
    use jstream_control::protocol::{
        Action, Hello, ItemTransform, PROTOCOL_VERSION, Query, QueryResult, Settings, Size,
        SourceId, SourceKind, Vec2,
    };
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    /// The layout of `scene` in the engine's current state.
    fn load(control: &dyn Control, scene: &str) -> Layout {
        let answer = now(control.call(Call::Query {
            query: Query::StateGet,
        }))
        .expect("state.get");
        let CallResult::Query { result } = answer.result else {
            panic!("state.get answers a query result");
        };
        let collection = Collection::from_result(result).expect("a state.get result");
        Layout::of(&collection, Some(&scene.into()))
    }

    fn geometry(cx: f32, cy: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("valid")
    }

    /// A session on an engine with two scenes; the second is on program and holds a
    /// geometry item over a legacy one.
    fn engine() -> impl Control {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        let edits = vec![
            Edit::SceneCreate {
                scene_id: "s1".into(),
                name: "Intro".into(),
                role: None,
            },
            Edit::SceneCreate {
                scene_id: "s2".into(),
                name: "Main".into(),
                role: None,
            },
            Edit::SourceCreate {
                source_id: SourceId::new("cam"),
                name: "Camera".into(),
                kind: SourceKind::Builtin {
                    kind: "color".into(),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            Edit::ItemAdd {
                scene_id: "s2".into(),
                item_id: "old".into(),
                source_id: SourceId::new("cam"),
                transform: Some(ItemTransform::IDENTITY),
                geometry: None,
                opacity: None,
                blend: None,
            },
            Edit::ItemAdd {
                scene_id: "s2".into(),
                item_id: "box".into(),
                source_id: SourceId::new("cam"),
                transform: None,
                geometry: Some(geometry(960.0, 540.0, 640.0, 360.0)),
                opacity: None,
                blend: None,
            },
        ];
        now(control.call(Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        })))
        .expect("setup batch");
        now(control.call(Call::Action {
            action: Action::SceneSwitch {
                scene_id: "s2".into(),
                preset_id: None,
                via: Vec::new(),
            },
        }))
        .expect("switch");
        control
    }

    #[test]
    fn a_scene_is_laid_out_with_its_boxes() {
        let control = engine();
        let layout = load(&control, "s2");
        assert_eq!(layout.canvas, Canvas::DEFAULT);
        assert_eq!(layout.scene, Some(("s2".into(), "Main".into())));
        assert_eq!(layout.items.len(), 2);
        assert_eq!(layout.legacy_count(), 1);
        let boxes: Vec<_> = layout.boxes().map(|(id, _)| id.clone()).collect();
        assert_eq!(boxes, vec![ItemId::from("box")]);
        assert_eq!(layout.items[1].name, "Camera");
    }

    /// A hidden item is left out of what the canvas shows and hits, unless selected (#1732).
    #[test]
    fn a_hidden_item_is_seen_only_while_selected() {
        let control = engine();
        let mut layout = load(&control, "s2");
        let ids = |seen: Vec<(&ItemId, &ItemGeometry)>| -> Vec<ItemId> {
            seen.into_iter().map(|(id, _)| id.clone()).collect()
        };
        let boxed = ItemId::from("box");
        assert_eq!(ids(layout.seen_boxes(&[]).collect()), vec![boxed.clone()]);
        layout.items[1].visible = false;
        assert!(layout.seen_boxes(&[]).next().is_none());
        assert_eq!(layout.seen_shapes(&[]).count(), 0);
        let selected = [boxed.clone()];
        assert_eq!(ids(layout.seen_boxes(&selected).collect()), vec![boxed]);
        assert_eq!(layout.seen_shapes(&selected).count(), 1);
        assert_eq!(layout.boxes().count(), 1, "boxes() still lists every box");
    }

    /// The gestures `live.list` reports open.
    fn open_gestures(control: &dyn Control) -> usize {
        let answer = now(control.call(Call::Query {
            query: Query::LiveList,
        }))
        .expect("live.list");
        match answer.result {
            CallResult::Query {
                result: QueryResult::LiveList { gestures },
            } => gestures.len(),
            _ => panic!("live.list answers a live.list result"),
        }
    }

    #[test]
    fn live_values_change_no_revision_and_the_commit_ends_the_gesture() {
        let control = engine();
        let before = load(&control, "s2");
        let gesture = GestureId::new("drag-1");
        let moving = geometry(200.0, 200.0, 320.0, 180.0);
        now(live_set(
            &control,
            gesture.clone(),
            "s2".into(),
            vec![("box".into(), moving)],
            Vec::new(),
            None,
        ))
        .expect("live.set");
        assert_eq!(open_gestures(&control), 1);
        assert_eq!(
            load(&control, "s2").rev,
            before.rev,
            "live values change no rev"
        );

        let dropped = geometry(220.0, 210.0, 320.0, 180.0);
        let rev = now(commit(
            &control,
            "s2".into(),
            vec![("box".into(), dropped)],
            Vec::new(),
            Some(gesture),
        ))
        .expect("commit");
        assert_eq!(rev.0, before.rev.0 + 1);
        assert_eq!(open_gestures(&control), 0);
        assert_eq!(
            load(&control, "s2").geometry_of(&"box".into()),
            Some(dropped)
        );
    }

    /// ADR-0094: an orbit shows its camera live and commits it in one step, which the
    /// layout then reports as the scene's camera; a keyframe scene shows its parent's.
    #[test]
    fn an_orbit_commits_the_scene_s_camera() {
        let control = engine();
        let before = load(&control, "s2");
        assert_eq!(before.camera, None);
        let canvas = before.canvas;
        let camera = jstream_control::protocol::SceneCamera::framing(canvas);
        let turned = crate::canvas::camera::orbit(camera, 120.0, 40.0);
        let gesture = GestureId::new("orbit-1");
        now(live_set(
            &control,
            gesture.clone(),
            "s2".into(),
            Vec::new(),
            Vec::new(),
            Some(turned),
        ))
        .expect("live.set takes a camera");
        assert_eq!(open_gestures(&control), 1);
        assert_eq!(
            load(&control, "s2").camera,
            None,
            "live values change nothing stored"
        );
        let rev = now(commit_camera(&control, "s2".into(), turned, Some(gesture)))
            .expect("scene.set_camera");
        assert_eq!(rev.0, before.rev.0 + 1);
        assert_eq!(open_gestures(&control), 0);
        assert_eq!(load(&control, "s2").camera, Some(turned));
    }

    #[test]
    fn a_cancel_ends_the_gesture_with_no_edit() {
        let control = engine();
        let before = load(&control, "s2");
        let gesture = GestureId::new("drag-1");
        let moving = geometry(200.0, 200.0, 320.0, 180.0);
        now(live_set(
            &control,
            gesture.clone(),
            "s2".into(),
            vec![("box".into(), moving)],
            Vec::new(),
            None,
        ))
        .expect("live.set");
        now(live_cancel(&control, gesture)).expect("live.cancel");
        assert_eq!(open_gestures(&control), 0);
        let after = load(&control, "s2");
        assert_eq!(after.rev, before.rev);
        assert_eq!(
            after.geometry_of(&"box".into()),
            before.geometry_of(&"box".into())
        );
    }

    #[test]
    fn a_commit_is_one_undoable_step() {
        let control = engine();
        let before = load(&control, "s2");
        let moved = geometry(100.0, 100.0, 320.0, 180.0);
        let rev = now(commit(
            &control,
            "s2".into(),
            vec![("box".into(), moved)],
            Vec::new(),
            None,
        ))
        .expect("commit");
        assert_eq!(rev.0, before.rev.0 + 1);
        let after = load(&control, "s2");
        assert_eq!(after.geometry_of(&"box".into()), Some(moved));

        let depths = now(history(&control, HistoryOp::Undo)).expect("undo");
        assert_eq!(depths.redo, Some(1));
        let undone = load(&control, "s2");
        assert_eq!(
            undone.geometry_of(&"box".into()),
            before.geometry_of(&"box".into())
        );

        let depths = now(history(&control, HistoryOp::Redo)).expect("redo");
        assert_eq!(depths.redo, Some(0));
        let redone = load(&control, "s2");
        assert_eq!(redone.geometry_of(&"box".into()), Some(moved));
    }

    #[test]
    fn a_crop_commits_its_box_and_crop_as_one_step() {
        let control = engine();
        let before = load(&control, "s2");
        let cropped = geometry(1010.0, 540.0, 540.0, 360.0);
        let crop = Crop {
            left: 100,
            top: 0,
            right: 0,
            bottom: 0,
        };
        now(commit(
            &control,
            "s2".into(),
            vec![("box".into(), cropped)],
            vec![("box".into(), crop)],
            None,
        ))
        .expect("commit");
        let after = load(&control, "s2");
        let item = after.item(&"box".into()).expect("the item");
        assert_eq!((item.geometry, item.crop), (Some(cropped), crop));
        assert_eq!(after.rev.0, before.rev.0 + 1, "one batch");
        now(history(&control, HistoryOp::Undo)).expect("undo");
        let undone = load(&control, "s2");
        let item = undone.item(&"box".into()).expect("the item");
        assert_eq!(
            (item.geometry, item.crop),
            (before.geometry_of(&"box".into()), Crop::NONE),
            "one undo restores both"
        );
    }

    #[test]
    fn a_layout_knows_its_sources_sizes() {
        let control = engine();
        let answer = now(control.call(Call::Query {
            query: Query::StateGet,
        }))
        .expect("state.get");
        let CallResult::Query { result } = answer.result else {
            panic!("state.get answers a query result");
        };
        let collection = Collection::from_result(result).expect("a state.get result");
        let sizes = Sizes::from([(SourceId::new("cam"), (1280, 720))]);
        let depths = SourceDepths::from([(SourceId::new("cam"), 300)]);
        let layout = Layout::sized(&collection, Some(&"s2".into()), &sizes, &depths);
        assert_eq!(
            layout.item(&"box".into()).and_then(|i| i.source_size),
            Some((1280, 720))
        );
        assert_eq!(
            layout.solid_of(&"box".into()),
            Some(SolidBounds::of_size(1280.0, 720.0, 300.0)),
            "a source with depth is outlined as a model"
        );
        let plain = Layout::of(&collection, Some(&"s2".into()));
        assert_eq!(plain.item(&"box".into()).and_then(|i| i.source_size), None);
        assert_eq!(plain.solid_of(&"box".into()), None);
    }

    #[test]
    fn clearing_a_crop_gives_the_box_its_uncropped_extent_back() {
        let control = engine();
        let crop = Crop {
            left: 100,
            ..Crop::NONE
        };
        // A 1280×720 source at half size, its left 100 source pixels cropped.
        let cropped = geometry(985.0, 540.0, 590.0, 360.0);
        now(commit(
            &control,
            "s2".into(),
            vec![("box".into(), cropped)],
            vec![("box".into(), crop)],
            None,
        ))
        .expect("commit");
        let answer = now(control.call(Call::Query {
            query: Query::StateGet,
        }))
        .expect("state.get");
        let CallResult::Query { result } = answer.result else {
            panic!("state.get answers a query result");
        };
        let collection = Collection::from_result(result).expect("a state.get result");
        let ids = [ItemId::from("box"), ItemId::from("old")];
        let sizes = Sizes::from([(SourceId::new("cam"), (1280, 720))]);
        let edits = crate::canvas::crop::clear(&collection, &sizes, &"s2".into(), &ids);
        let full = geometry(960.0, 540.0, 640.0, 360.0);
        assert_eq!(
            edits,
            [
                Edit::ItemSetCrop {
                    item_id: "box".into(),
                    scene_id: Some("s2".into()),
                    crop: Crop::NONE,
                },
                Edit::ItemSetGeometry {
                    item_id: "box".into(),
                    scene_id: Some("s2".into()),
                    geometry: full,
                },
            ],
            "only the cropped item, and its box only with a size"
        );
        let unsized_edits =
            crate::canvas::crop::clear(&collection, &Sizes::new(), &"s2".into(), &ids);
        assert_eq!(unsized_edits.len(), 1);
    }

    #[test]
    fn a_locked_item_is_known_to_the_layout() {
        let control = engine();
        assert!(!load(&control, "s2").is_locked(&"box".into()));
        now(control.call(Call::Batch(Batch::single(Edit::ItemSetLocked {
            item_id: "box".into(),
            locked: true,
        }))))
        .expect("lock");
        let layout = load(&control, "s2");
        assert!(layout.is_locked(&"box".into()));
        assert!(!layout.is_locked(&"missing".into()));
    }

    #[test]
    fn nothing_to_redo_is_not_a_problem() {
        let control = engine();
        let error = now(history(&control, HistoryOp::Redo)).expect_err("nothing to redo");
        assert!(is_history_empty(&error));
    }

    #[test]
    fn with_no_scene_there_is_nothing_to_edit() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        let layout = load(&control, "s2");
        assert_eq!(layout.scene, None);
        assert!(layout.items.is_empty());
        let empty = load(&engine(), "s1");
        assert_eq!(empty.scene, Some(("s1".into(), "Intro".into())));
        assert!(empty.items.is_empty());
    }

    /// A 3D scene, `s3`, placing a group, `rig`, over the whole canvas, whose two small
    /// children sit in opposite corners; `share` turns the group's `share_space` on.
    fn rig(share: bool) -> Layout {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        let canvas = Canvas::DEFAULT;
        let (w, h) = (canvas.width as f32, canvas.height as f32);
        let child = |item_id: &str, x: f32, y: f32| Edit::ItemAdd {
            scene_id: "g".into(),
            item_id: item_id.into(),
            source_id: SourceId::new("cam"),
            transform: None,
            geometry: Some(geometry(x, y, 200.0, 200.0)),
            opacity: None,
            blend: None,
        };
        let edits = vec![
            Edit::SceneCreate {
                scene_id: "s3".into(),
                name: "Set".into(),
                role: None,
            },
            Edit::SceneCreate {
                scene_id: "g".into(),
                name: "Rig".into(),
                role: Some(SceneRole::Group),
            },
            Edit::SceneSetOptions {
                scene_id: "g".into(),
                options: jstream_control::protocol::SceneOptions {
                    share_space: share,
                    ..Default::default()
                },
            },
            Edit::SceneSetCamera {
                scene_id: "s3".into(),
                camera: Some(SceneCamera::framing(canvas)),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("cam"),
                name: "Camera".into(),
                kind: SourceKind::Builtin {
                    kind: "color".into(),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            Edit::SourceCreate {
                source_id: SourceId::new("grp"),
                name: "Rig".into(),
                kind: SourceKind::Scene {
                    scene_id: "g".into(),
                },
                settings: Settings::default(),
                duplicated_from: None,
            },
            child("left", 300.0, 300.0),
            child("right", w - 300.0, h - 300.0),
            Edit::ItemAdd {
                scene_id: "s3".into(),
                item_id: "rig".into(),
                source_id: SourceId::new("grp"),
                transform: None,
                geometry: Some(geometry(w * 0.5, h * 0.5, w, h)),
                opacity: None,
                blend: None,
            },
        ];
        now(control.call(Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        })))
        .expect("setup batch");
        load(&control, "s3")
    }

    /// ADR-0164 decision 6 (#1596): in a 3D scene a group that shares the space is
    /// outlined by the hull of its children and hit only inside them, not by its own box
    /// over the whole canvas; a flat group is still one box.
    #[test]
    fn a_3d_group_is_outlined_and_hit_by_its_children() {
        use crate::canvas::camera;
        use crate::canvas::transform::Point;
        let layout = rig(true);
        let view = camera::view(layout.camera, layout.canvas).expect("a 3D scene");
        let gap = Point { x: 960.0, y: 540.0 };
        let on_child = Point { x: 300.0, y: 300.0 };
        assert_eq!(
            camera::topmost_at(layout.shapes(), &view, gap),
            None,
            "the gap between the children is not the group"
        );
        assert_eq!(
            camera::topmost_at(layout.shapes(), &view, on_child),
            Some(&"rig".into()),
            "a child selects the group"
        );
        let (_, g, shape) = layout.shapes().next().expect("the group's item");
        let bounds = camera::screen_box(g, shape, &view).expect("seen");
        // From the left child's left edge to the right child's right edge.
        let width = bounds.size.width.get();
        assert!(
            (width - 1520.0).abs() < 1.0,
            "outlined by its children: {width}"
        );

        let flat = rig(false);
        assert_eq!(
            camera::topmost_at(flat.shapes(), &view, gap),
            Some(&"rig".into()),
            "a flat group is one card"
        );
    }
}
