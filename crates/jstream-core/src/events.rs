//! Event fan-out: one unbounded queue per subscription, so no event is ever dropped.
//!
//! The engine keeps a weak reference to each queue, and the subscription's stream holds
//! the strong one, so dropping the stream unsubscribes. Closing a queue (the session or
//! the engine went away) lets the stream yield what it buffered, then end.

use crate::lock;
use futures_core::Stream;
use jstream_protocol::Event;
use std::collections::VecDeque;
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

/// The subscribers of an engine, in subscription order.
#[derive(Default)]
pub(crate) struct Subscribers {
    queues: Vec<QueueRef>,
}

impl Subscribers {
    /// Opens a new subscription. Returns the stream and the weak handle that closes it.
    pub(crate) fn subscribe(&mut self) -> (Subscription, QueueRef) {
        let queue = Arc::new(Mutex::new(Queue::default()));
        let weak = Arc::downgrade(&queue);
        self.queues.push(weak.clone());
        (Subscription(queue), weak)
    }

    /// Queues `event` for every live subscriber and collects the wakers to wake once the
    /// engine's lock is released. Forgets subscriptions that were dropped or closed, so a
    /// closed stream stays ended.
    pub(crate) fn emit(&mut self, event: &Event, wake: &mut Vec<Waker>) {
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
