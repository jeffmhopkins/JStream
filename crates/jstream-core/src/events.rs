//! Event fan-out: one unbounded queue per subscription, so no event is ever dropped.
//!
//! The engine keeps a weak reference to each queue, and the subscription's stream holds
//! the strong one, so dropping the stream unsubscribes. Closing a queue (the session or
//! the engine went away) lets the stream yield what it buffered, then end.
//!
//! Clients never see the internal show scene (it is not in `state.get` or the scene
//! lists), so an event that names it, or one of its timelines, reaches no one
//! ([`Hidden`]).

use crate::lock;
use futures_core::Stream;
use jstream_protocol::{Event, Rev, SceneId, TimelineId};
use std::collections::{BTreeSet, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

/// The buffer behind one event subscription.
#[derive(Default)]
pub(crate) struct Queue {
    events: VecDeque<Event>,
    waker: Option<Waker>,
    closed: bool,
}

/// A weak handle to a subscription's queue, as the engine and sessions hold it.
pub(crate) type QueueRef = Weak<Mutex<Queue>>;

/// What clients never hear about: the internal show scene and its timelines, as the
/// collection held them at `rev`.
#[derive(Debug, Default, PartialEq)]
struct Hidden {
    rev: Option<Rev>,
    scene: Option<SceneId>,
    timelines: BTreeSet<TimelineId>,
}

impl Hidden {
    /// The show scene of `graph`, and its timelines.
    fn of(graph: &jstream_graph::Graph) -> Self {
        let state = graph.state();
        let scene = state.show_scene().and_then(|id| state.scene(id));
        Hidden {
            rev: Some(graph.rev()),
            scene: scene.map(|scene| scene.id().clone()),
            timelines: scene
                .map(|scene| scene.timelines().keys().cloned().collect())
                .unwrap_or_default(),
        }
    }

    /// Whether `event` names the show scene or one of its timelines.
    fn hides(&self, event: &Event) -> bool {
        let Some(show) = &self.scene else {
            return false;
        };
        match event {
            Event::SceneKeyframeChanged { scene_id, .. }
            | Event::TimelineChanged { scene_id, .. } => scene_id == show,
            Event::TimelineCueFired { timeline_id, .. } => self.timelines.contains(timeline_id),
            _ => false,
        }
    }
}

/// The subscribers of an engine, in subscription order.
#[derive(Default)]
pub(crate) struct Subscribers {
    queues: Vec<QueueRef>,
    /// What no event of theirs may name, as the engine last set it.
    hidden: Hidden,
}

impl Subscribers {
    /// Follows `graph`'s show scene, which no event may name from now on. Cheap when
    /// neither the revision nor the show scene moved since the last call; the engine
    /// calls it as it plans, which every change to either reaches.
    pub(crate) fn follow(&mut self, graph: &jstream_graph::Graph) {
        if self.hidden.rev == Some(graph.rev())
            && self.hidden.scene.as_ref() == graph.state().show_scene()
        {
            return;
        }
        self.hidden = Hidden::of(graph);
    }

    /// Opens a new subscription. Returns the stream and the weak handle that closes it.
    pub(crate) fn subscribe(&mut self) -> (Subscription, QueueRef) {
        let queue = Arc::new(Mutex::new(Queue::default()));
        let weak = Arc::downgrade(&queue);
        self.queues.push(weak.clone());
        (Subscription(queue), weak)
    }

    /// Queues `event` for every live subscriber and collects the wakers to wake once the
    /// engine's lock is released. Forgets subscriptions that were dropped or closed, so a
    /// closed stream stays ended. An event that names the show scene goes nowhere.
    pub(crate) fn emit(&mut self, event: &Event, wake: &mut Vec<Waker>) {
        if self.hidden.hides(event) {
            return;
        }
        self.queues.retain(|weak| {
            let Some(queue) = weak.upgrade() else {
                return false;
            };
            let mut queue = lock(&queue);
            if queue.closed {
                return false;
            }
            queue.events.push_back(event.clone());
            wake.extend(queue.waker.take());
            true
        });
    }

    /// Ends every subscription.
    pub(crate) fn close_all(&mut self) {
        close(self.queues.drain(..));
    }
}

/// Ends the subscriptions still alive among `queues`: each yields what it has buffered,
/// then `None`.
pub(crate) fn close(queues: impl IntoIterator<Item = QueueRef>) {
    for queue in queues.into_iter().filter_map(|weak| weak.upgrade()) {
        let waker = {
            let mut queue = lock(&queue);
            queue.closed = true;
            queue.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// One event subscription, the stream behind [`Control::events`](jstream_control::Control::events).
pub(crate) struct Subscription(Arc<Mutex<Queue>>);

impl Stream for Subscription {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let mut queue = lock(&self.0);
        if let Some(event) = queue.events.pop_front() {
            Poll::Ready(Some(event))
        } else if queue.closed {
            Poll::Ready(None)
        } else {
            queue.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_graph::Graph;
    use jstream_protocol::{ClientId, Edit, TimelineChange};

    fn timeline(scene: &str) -> Event {
        Event::TimelineChanged {
            timeline_id: TimelineId::new("t"),
            scene_id: SceneId::new(scene),
            change: TimelineChange::Started,
            playing: true,
            position_ms: 0,
            pass: 0,
        }
    }

    fn create(id: &str) -> Edit {
        serde_json::from_value(serde_json::json!({
            "type": "scene.create", "scene_id": id, "name": id
        }))
        .expect("an edit")
    }

    /// An event naming the internal show scene reaches no subscriber; one naming another
    /// scene does, and nothing is hidden once it is no longer the show scene.
    #[test]
    fn events_naming_the_show_scene_reach_no_one() {
        let mut graph = Graph::new();
        graph
            .apply(&[create("a"), create("show")], None)
            .expect("created");
        assert!(graph.set_show_scene(Some(SceneId::new("show"))));
        let mut subscribers = Subscribers::default();
        let (subscription, _) = subscribers.subscribe();
        subscribers.follow(&graph);
        let mut wake = Vec::new();
        subscribers.emit(&timeline("show"), &mut wake);
        subscribers.emit(
            &Event::SceneKeyframeChanged {
                scene_id: SceneId::new("show"),
                keyframe_id: None,
                client_id: ClientId::new("c"),
                request_id: None,
            },
            &mut wake,
        );
        subscribers.emit(&timeline("a"), &mut wake);
        let queued = |subscription: &Subscription| lock(&subscription.0).events.clone();
        assert_eq!(queued(&subscription), [timeline("a")]);
        assert!(graph.set_show_scene(None));
        subscribers.follow(&graph);
        subscribers.emit(&timeline("show"), &mut wake);
        assert_eq!(queued(&subscription), [timeline("a"), timeline("show")]);
    }
}
