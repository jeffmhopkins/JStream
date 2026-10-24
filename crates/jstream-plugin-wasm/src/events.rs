//! The bounded queue of engine events waiting for a plugin (ADR-0069).

use jstream_control::protocol::Event;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

/// The most events a plugin's queue holds (ADR-0069). An event that arrives with the
/// queue full drops every queued event first, and the plugin is next handed one
/// `events.dropped` notice with the count, as a WebSocket client that reads too slowly
/// is.
pub const MAX_QUEUED_EVENTS: usize = 1_000;

/// The events waiting for one plugin, oldest first, and how many were dropped since the
/// plugin was last handed an event.
#[derive(Debug, Default)]
pub(crate) struct EventQueue {
    events: VecDeque<Event>,
    /// Events dropped and not yet reported: the next [`Self::pop`] reports them.
    dropped: u64,
}

impl EventQueue {
    /// Queues `event`. A full queue is emptied first and its events counted as dropped.
    fn push(&mut self, event: Event) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.dropped = self.dropped.saturating_add(self.events.len() as u64);
            self.events.clear();
        }
        self.events.push_back(event);
    }

    /// The next event for the plugin: one `events.dropped` notice when events were
    /// dropped since the last one handed out, else the oldest queued event.
    fn pop(&mut self) -> Option<Event> {
        if self.dropped > 0 {
            let count = std::mem::take(&mut self.dropped);
            return Some(Event::EventsDropped { count });
        }
        self.events.pop_front()
    }

    fn len(&self) -> usize {
        self.events.len() + usize::from(self.dropped > 0)
    }
}

/// Where the engine queues events for one plugin, from any thread, while the plugin's own
/// thread hands them to it with [`Plugin::deliver_event`](crate::Plugin::deliver_event).
/// Clones share the queue. Queueing never blocks on the plugin: it takes a short lock,
/// and a full queue drops its events rather than grow.
#[derive(Clone, Debug, Default)]
pub struct EventSender {
    queue: Arc<Mutex<EventQueue>>,
}

impl EventSender {
    /// Queues `event` for the plugin, dropping the queued events first if it already
    /// holds [`MAX_QUEUED_EVENTS`].
    pub fn send(&self, event: Event) {
        self.lock().push(event);
    }

    /// How many events wait for the plugin, an `events.dropped` notice included.
    pub fn queued(&self) -> usize {
        self.lock().len()
    }

    pub(crate) fn next(&self) -> Option<Event> {
        self.lock().pop()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, EventQueue> {
        // The queue's state is valid between any two of its operations, none of which
        // panics while it holds the lock.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{EventSender, MAX_QUEUED_EVENTS};
    use jstream_control::protocol::Event;

    fn changed(rev: u64) -> Event {
        serde_json::from_value(serde_json::json!({
            "type": "state.changed",
            "rev": rev,
            "client_id": "c1",
        }))
        .expect("a valid event")
    }

    #[test]
    fn events_come_out_in_order() {
        let sender = EventSender::default();
        sender.send(changed(1));
        sender.send(changed(2));
        assert_eq!(sender.queued(), 2);
        assert_eq!(sender.next(), Some(changed(1)));
        assert_eq!(sender.next(), Some(changed(2)));
        assert_eq!(sender.next(), None);
    }

    #[test]
    fn a_full_queue_drops_its_events_for_one_notice() {
        let sender = EventSender::default();
        let full = u64::try_from(MAX_QUEUED_EVENTS).expect("fits");
        for rev in 0..full {
            sender.send(changed(rev));
        }
        assert_eq!(sender.queued(), MAX_QUEUED_EVENTS);
        // One more drops the thousand queued, and keeps the new one.
        sender.send(changed(full));
        assert_eq!(sender.queued(), 2, "the notice and the new event");
        // Another overflow before the plugin reads adds to the same notice: 999 more fill
        // the queue, and the next drops all thousand again.
        for rev in 1..=full {
            sender.send(changed(full + rev));
        }
        assert_eq!(sender.queued(), 2);
        assert_eq!(
            sender.next(),
            Some(Event::EventsDropped { count: 2 * full })
        );
        assert_eq!(sender.next(), Some(changed(2 * full)), "the newest kept");
        assert_eq!(sender.next(), None);
    }
}
