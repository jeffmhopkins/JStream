//! A number drag's gesture outside the canvas (#1739, ADR-0213): while a filter's
//! setting, an item's opacity, a fader or a send is dragged, each frame's value goes out
//! as a gesture's `live.set`, so the output and the mix follow it, and the release sends
//! one batch that ends the gesture (`ends_gesture`), one undo step for the whole drag. A
//! drag lost before its release, or a release that changes nothing, cancels it. The
//! calls go one at a time, in order, so the commit never overtakes the `live.set` that
//! opened its gesture, and the values that arrive while one is in flight collapse into
//! the latest. The canvas's own drags keep theirs (`canvas/live.rs`).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0032-live-edits-and-undo.md, docs/adr/0213-number-drags-follow-live.md

use super::number::Scrubbing;
use crate::app::SessionContext;
use crate::store::Studio;
use jstream_control::ControlError;
use jstream_control::protocol::{Action, Batch, Call, CallResult, Edit, GestureId, Rev};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::task::{Poll, Waker};

/// What a commit's caller is told: the batch's revision, or why it failed.
pub type Done = Box<dyn FnOnce(Result<Rev, ControlError>)>;

/// How the gesture ends.
enum End {
    /// Sends these edits as one batch, ending the gesture if the engine holds it.
    Commit(Vec<Edit>, Done),
    /// Drops its values.
    Cancel,
}

/// What waits to be sent, and what has been.
struct State {
    gesture: GestureId,
    /// The newest values not yet sent.
    latest: Option<Vec<Edit>>,
    end: Option<End>,
    /// Whether a call is in flight.
    busy: bool,
    /// Whether a `live.set` succeeded, so the engine holds the gesture.
    opened: bool,
}

/// One drag's calls.
#[derive(Clone)]
struct Pump {
    state: Rc<RefCell<State>>,
    session: SessionContext,
    studio: Studio,
}

impl Pump {
    fn show(&self, values: Vec<Edit>) {
        let mut state = self.state.borrow_mut();
        if state.end.is_some() {
            return;
        }
        state.latest = Some(values);
        drop(state);
        self.wake();
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
            let (gesture, latest, end, opened) = {
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
                (state.gesture.clone(), latest, end, state.opened)
            };
            let Some(open) = self.session.get() else {
                self.state.borrow_mut().busy = false;
                if let Some(End::Commit(_, done)) = end {
                    done(Err(not_connected()));
                }
                return;
            };
            if let Some(values) = latest {
                let action = Action::LiveSet {
                    gesture_id: gesture,
                    values,
                };
                match open.control.call(Call::Action { action }).await {
                    Ok(_) => self.state.borrow_mut().opened = true,
                    Err(error) => self.studio.report(&error),
                }
                continue;
            }
            match end {
                Some(End::Commit(edits, done)) => {
                    let batch = Batch {
                        edits,
                        expected_rev: None,
                        idempotency_key: None,
                        ends_gesture: opened.then(|| gesture.clone()),
                    };
                    let result = match open.control.call(Call::Batch(batch)).await {
                        Ok(response) => match response.result {
                            CallResult::Batch { rev, .. } => Ok(rev),
                            _ => Err(ControlError::Disconnected {
                                reason: "the engine answered a batch with the wrong kind of \
                                         result"
                                    .into(),
                            }),
                        },
                        Err(error) => Err(error),
                    };
                    if result.is_err() && opened {
                        // The engine still holds the gesture: drop its live values.
                        let action = Action::LiveCancel {
                            gesture_id: gesture,
                        };
                        _ = open.control.call(Call::Action { action }).await;
                    }
                    done(result);
                }
                Some(End::Cancel) if opened => {
                    let action = Action::LiveCancel {
                        gesture_id: gesture,
                    };
                    if let Err(error) = open.control.call(Call::Action { action }).await {
                        self.studio.report(&error);
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

/// Where [`EditDrag::commit_async`]'s outcome waits for its future.
#[derive(Default)]
struct Slot {
    result: Option<Result<Rev, ControlError>>,
    waker: Option<Waker>,
}

/// The error a call gets with no session open.
fn not_connected() -> ControlError {
    ControlError::Disconnected {
        reason: "not connected".into(),
    }
}

/// The gesture of a drag on one of a form's or a strip's number fields (#1739): opened
/// by the drag's first value, which goes out with `live.set` as every later one does,
/// and ended by the release's batch, or cancelled when the release sends nothing. One
/// handle serves every field of the form, since one field is dragged at a time.
#[derive(Clone, Copy)]
pub struct EditDrag {
    live: StoredValue<Option<Pump>, LocalStorage>,
    studio: Studio,
    session: SessionContext,
}

impl Default for EditDrag {
    /// [`EditDrag::new`]: it reads the studio and the session from context.
    fn default() -> Self {
        Self::new()
    }
}

impl EditDrag {
    /// A drag handle for the fields of the component being built. A drag still open
    /// when the component goes leaves no live values behind.
    pub fn new() -> EditDrag {
        let live = StoredValue::new_local(None::<Pump>);
        on_cleanup(move || {
            if let Some(Some(pump)) = live.try_update_value(Option::take) {
                pump.finish(End::Cancel);
            }
        });
        EditDrag {
            live,
            studio: expect_context::<Studio>(),
            session: expect_context::<SessionContext>(),
        }
    }

    /// Shows `values` on output and in the mix, opening the gesture with the first.
    pub fn show(self, values: Vec<Edit>) {
        if self.live.with_value(Option::is_none) {
            let Some(open) = self.session.get() else {
                return;
            };
            let gesture = GestureId::new(self.studio.fresh_id(&open, "drag"));
            self.live.set_value(Some(Pump {
                state: Rc::new(RefCell::new(State {
                    gesture,
                    latest: None,
                    end: None,
                    busy: false,
                    opened: false,
                })),
                session: self.session,
                studio: self.studio,
            }));
        }
        self.live.with_value(|live| {
            if let Some(pump) = live {
                pump.show(values);
            }
        });
    }

    /// Ends an open gesture without a commit: its values show their stored ones again.
    pub fn end(self) {
        if let Some(Some(pump)) = self.live.try_update_value(Option::take) {
            pump.finish(End::Cancel);
        }
    }

    /// Sends `edits` as one batch, ending the open gesture with it once its live values
    /// are out, or alone when no drag is under way; `done` gets the outcome.
    pub fn commit(self, edits: Vec<Edit>, done: impl FnOnce(Result<Rev, ControlError>) + 'static) {
        let done: Done = Box::new(done);
        if let Some(pump) = self.live.try_update_value(Option::take).flatten() {
            pump.finish(End::Commit(edits, done));
            return;
        }
        let Some(open) = self.session.get() else {
            done(Err(not_connected()));
            return;
        };
        spawn_local(async move {
            done(crate::store::apply(&open, edits, None).await);
        });
    }

    /// [`commit`](Self::commit) as a future of its outcome, for a caller that awaits it.
    pub fn commit_async(
        self,
        edits: Vec<Edit>,
    ) -> impl Future<Output = Result<Rev, ControlError>> + 'static {
        let slot: Rc<RefCell<Slot>> = Rc::default();
        let filled = Rc::clone(&slot);
        self.commit(edits, move |result| {
            let mut slot = filled.borrow_mut();
            slot.result = Some(result);
            if let Some(waker) = slot.waker.take() {
                waker.wake();
            }
        });
        std::future::poll_fn(move |cx| {
            let mut slot = slot.borrow_mut();
            match slot.result.take() {
                Some(result) => Poll::Ready(result),
                None => {
                    slot.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
    }

    /// [`commit`](Self::commit), showing a failure in the problem bar.
    pub fn send(self, edits: Vec<Edit>) {
        let studio = self.studio;
        self.commit(edits, move |result| {
            if let Err(error) = result {
                studio.report(&error);
            }
        });
    }

    /// Follows a field's drag: each value through `edit_for` to output, and an end the
    /// release didn't send (an unchanged value, a lost pointer) cancels the gesture. A
    /// value `edit_for` has no edit for leaves the last one showing.
    pub fn follow(self, scrubbing: Scrubbing, edit_for: impl Fn(f64) -> Option<Edit>) {
        match scrubbing {
            Scrubbing::Moved(v) => {
                if let Some(edit) = edit_for(v) {
                    self.show(vec![edit]);
                }
            }
            Scrubbing::Ended | Scrubbing::Cancelled(_) => self.end(),
        }
    }
}
