//! A drag's live values on their way to the engine (ADR-0032, #747): while a box is
//! dragged, the program output follows it through `live.set`, and the drop commits
//! the gesture in one undoable batch, or Escape cancels it. The calls go one at a time,
//! in order, so the commit never overtakes the `live.set` that opened its gesture, and
//! the values that arrive while one is in flight collapse into the latest.
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0032-live-edits-and-undo.md

use super::model::{self, CanvasError};
use crate::app::SessionContext;
use jstream_control::protocol::{Crop, GestureId, ItemGeometry, ItemId, SceneId};
use leptos::task::spawn_local;
use std::cell::RefCell;
use std::rc::Rc;

/// What a drag shows: each moved item's geometry, and each cropped item's crop.
pub(crate) type Values = (Vec<(ItemId, ItemGeometry)>, Vec<(ItemId, Crop)>);

/// What runs once the gesture's last values are out: the commit, given the gesture's
/// scene and the gesture to end if `live.set` opened one, else `None`.
type Then = Box<dyn FnOnce(SceneId, Option<GestureId>)>;

/// How the gesture ends.
enum End {
    /// Commits it.
    Commit(Then),
    /// Drops its values.
    Cancel,
}

/// What waits to be sent, and what has been.
struct State {
    gesture: GestureId,
    scene_id: SceneId,
    /// The newest values not yet sent.
    latest: Option<Values>,
    end: Option<End>,
    /// Whether a call is in flight.
    busy: bool,
    /// Whether a `live.set` succeeded, so the engine holds the gesture.
    opened: bool,
}

/// One drag's gesture.
#[derive(Clone)]
pub struct Live {
    state: Rc<RefCell<State>>,
    session: SessionContext,
    /// Shows a failed call.
    report: Rc<dyn Fn(&CanvasError)>,
}

impl Live {
    /// A gesture on the items of `scene_id`, opened by its first values.
    pub fn new(
        gesture: GestureId,
        scene_id: SceneId,
        session: SessionContext,
        report: impl Fn(&CanvasError) + 'static,
    ) -> Live {
        Live {
            state: Rc::new(RefCell::new(State {
                gesture,
                scene_id,
                latest: None,
                end: None,
                busy: false,
                opened: false,
            })),
            session,
            report: Rc::new(report),
        }
    }

    /// Shows the items at these geometries, and these crops, on output.
    pub fn show(&self, placed: Vec<(ItemId, ItemGeometry)>, crops: Vec<(ItemId, Crop)>) {
        let mut state = self.state.borrow_mut();
        if state.end.is_some() {
            return;
        }
        state.latest = Some((placed, crops));
        drop(state);
        self.wake();
    }

    /// Ends the gesture with `then`, the commit, once the values before it are out.
    pub fn commit(&self, then: impl FnOnce(SceneId, Option<GestureId>) + 'static) {
        self.finish(End::Commit(Box::new(then)));
    }

    /// Ends the gesture without a commit.
    pub fn cancel(&self) {
        self.finish(End::Cancel);
    }

    fn finish(&self, end: End) {
        let mut state = self.state.borrow_mut();
        if state.end.is_none() {
            state.end = Some(end);
        }
        drop(state);
        self.wake();
    }

    /// Starts sending, unless a call is already in flight: it sends what follows.
    fn wake(&self) {
        {
            let mut state = self.state.borrow_mut();
            if state.busy {
                return;
            }
            state.busy = true;
        }
        spawn_local(self.clone().pump());
    }

    /// Sends the latest values, then the end, one call at a time.
    async fn pump(self) {
        loop {
            let (gesture, scene_id, latest, end, opened) = {
                let mut state = self.state.borrow_mut();
                let latest = state.latest.take();
                // The end waits until no values are left before it.
                let end = if latest.is_none() {
                    state.end.take()
                } else {
                    None
                };
                if latest.is_none() && end.is_none() {
                    state.busy = false;
                    return;
                }
                (
                    state.gesture.clone(),
                    state.scene_id.clone(),
                    latest,
                    end,
                    state.opened,
                )
            };
            let Some(open) = self.session.get() else {
                self.state.borrow_mut().busy = false;
                return;
            };
            if let Some((placed, crops)) = latest {
                match model::live_set(&*open.control, gesture, scene_id, placed, crops).await {
                    Ok(()) => self.state.borrow_mut().opened = true,
                    Err(error) => (self.report)(&error),
                }
                continue;
            }
            match end {
                Some(End::Commit(then)) => then(scene_id, opened.then_some(gesture)),
                Some(End::Cancel) if opened => {
                    if let Err(error) = model::live_cancel(&*open.control, gesture).await {
                        (self.report)(&error);
                    }
                }
                _ => {}
            }
            // Ended: the busy flag stays set, so nothing is sent after the end.
            self.state.borrow_mut().latest = None;
            return;
        }
    }
}
