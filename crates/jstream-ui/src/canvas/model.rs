//! What the canvas edits and the calls that change it: the selected scene's items'
//! boxes, taken from the shared [`Collection`], `item.set_geometry` as one batch per
//! finished gesture, and `history.undo` and `history.redo` (ADR-0032).
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0032-live-edits-and-undo.md

use crate::store::Collection;
use jstream_control::protocol::{
    Batch, Call, CallResult, Canvas, Edit, HistoryCall, HistoryOp, ItemGeometry, ItemId,
    PlacementForm, Rev, SceneId,
};
use jstream_control::{Control, ControlError};
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
    /// Its items, in stacking order: the first is at the bottom.
    pub items: Vec<CanvasItem>,
}

impl Layout {
    /// The items with a box, in stacking order.
    pub fn boxes(&self) -> impl DoubleEndedIterator<Item = (&ItemId, &ItemGeometry)> {
        self.items
            .iter()
            .filter_map(|item| Some((&item.item_id, item.geometry.as_ref()?)))
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
}

impl Layout {
    /// The layout of `scene_id` in `collection`, or an empty one with no scene when it
    /// names none or a scene the collection doesn't have.
    pub fn of(collection: &Collection, scene_id: Option<&SceneId>) -> Layout {
        let Some(scene) = scene_id.and_then(|id| collection.scene(id)) else {
            return Layout {
                rev: collection.rev,
                canvas: collection.canvas,
                scene: None,
                items: Vec::new(),
            };
        };
        let items = scene
            .items
            .iter()
            .map(|item| CanvasItem {
                item_id: item.item_id.clone(),
                name: collection.source_name(&item.source_id),
                geometry: match item.placement {
                    PlacementForm::Geometry => item.geometry,
                    _ => None,
                },
                visible: item.visible,
            })
            .collect();
        Layout {
            rev: collection.rev,
            canvas: collection.canvas,
            scene: Some((scene.scene_id.clone(), scene.name.clone())),
            items,
        }
    }
}

/// The batch that places `item_id` in `scene_id` at `geometry`: one edit, so one undo
/// step. It carries no `expected_rev`: a drag's commit is last writer wins per field
/// (ADR-0032).
pub fn set_geometry(scene_id: SceneId, item_id: ItemId, geometry: ItemGeometry) -> Call {
    Call::Batch(Batch::single(Edit::ItemSetGeometry {
        item_id,
        scene_id: Some(scene_id),
        geometry,
    }))
}

/// Sends [`set_geometry`]'s batch and returns the revision it produced.
pub async fn commit(
    control: &dyn Control,
    scene_id: SceneId,
    item_id: ItemId,
    geometry: ItemGeometry,
) -> Result<Rev, CanvasError> {
    match control
        .call(set_geometry(scene_id, item_id, geometry))
        .await?
        .result
    {
        CallResult::Batch { rev, .. } => Ok(rev),
        _ => Err(CanvasError::UnexpectedAnswer {
            call: "item.set_geometry",
        }),
    }
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
        Action, Hello, ItemTransform, PROTOCOL_VERSION, Query, Settings, Size, SourceId,
        SourceKind, Vec2,
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

    #[test]
    fn a_commit_is_one_undoable_step() {
        let control = engine();
        let before = load(&control, "s2");
        let moved = geometry(100.0, 100.0, 320.0, 180.0);
        let rev = now(commit(&control, "s2".into(), "box".into(), moved)).expect("commit");
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
}
