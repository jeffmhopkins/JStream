//! The `events` tool's feed: one engine event subscription, pumped into a bounded buffer
//! that agents page through with a cursor.
//!
//! Each event gets a sequence number from 1 up, local to this server. When the buffer is
//! full, the oldest event is dropped, and a read that asked for it reports how many it
//! missed (`dropped`), so an agent always knows when it has a gap
//! (docs/adr/0036-mcp-events-polling-tool.md).
//!
//! A thread drains the subscription while no tool call reads it, so the
//! [`Control`](jstream_control::Control)'s own (unbounded) stream buffer never grows: the
//! subscription is woken by the `Control`'s machinery, as `serve_stdio` assumes too, and
//! the thread parks in between. A read drains it first, under the same lock and with the
//! thread's waker, so it sees every event the engine has delivered so far.
//!
//! `stats.tick` is not buffered: it arrives about once a second per running pipeline and
//! would soon push `state.changed` and `output.state_changed` out of the buffer
//! (ADR-0036, Consequences). Each tick carries running totals, so the newest one says
//! everything the earlier ones did: the feed keeps only that one, outside the buffer,
//! and every read returns it as `stats`. So an agent still sees the drop counts (ADR-0006:
//! every protocol capability over MCP), and ticks take no sequence number, leave no
//! gap and evict nothing. `meters.tick` comes about ten times a second while the mixer
//! runs and each one is a full reading, so it is kept the same way, as `meters`.
//!
//! `live.changed` comes up to 30 times a second per gesture while someone drags
//! (ADR-0032), so it would do the same. Each carries the newest values of what changed,
//! so a newer one can stand in for an older one of the same gesture: the feed merges the
//! newer values over the older event's, in place, when no read has returned that event
//! yet, only `live.changed` events follow it, and none of those sets a parameter the
//! newer one sets. The merged event keeps its `seq`, so the numbers stay contiguous,
//! nothing a reader has seen changes, and every value keeps its order relative to the
//! others. An unread drag therefore holds one buffered event.
//!
//! `youtube.*` and `plugin.*` events are dropped on arrival: YouTube and plugins are for
//! people, through the UI, so agents see neither YouTube's sign-in state and stream
//! health (ADR-0058 §3) nor a plugin's state (ADR-0069).
//!
//! Docs: crates/jstream-mcp/README.md#design, docs/adr/0036-mcp-events-polling-tool.md

use crate::tools::PEOPLE_ONLY;
use jstream_control::EventStream;
use jstream_protocol::{Edit, Event};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// How many events the buffer keeps; older ones are dropped.
pub(crate) const BUFFER_CAPACITY: usize = 1000;

/// The most events one read returns. The rest wait for the next read (`more`).
pub(crate) const PAGE_SIZE: usize = 100;

/// One buffered event and its sequence number.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Numbered {
    pub(crate) seq: u64,
    pub(crate) event: Event,
}

/// What one read returns: the `events` tool's structured result.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Page {
    /// The buffered events after the cursor, oldest first, at most [`PAGE_SIZE`].
    pub(crate) events: Vec<Numbered>,
    /// The cursor for the next read: the last `seq` returned, else the one passed in.
    pub(crate) next: u64,
    /// Events after the cursor that the buffer dropped before this read.
    pub(crate) dropped: u64,
    /// Whether more events are buffered after `next`.
    pub(crate) more: bool,
    /// Whether the subscription has ended (the session is gone): no event will follow.
    pub(crate) ended: bool,
    /// The newest `stats.tick`, if one has arrived: running totals, so it stands for every
    /// earlier tick. Kept outside the buffer, so ticks never evict other events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stats: Option<Event>,
    /// The newest `meters.tick`, if one has arrived: the audio levels now. Kept outside
    /// the buffer, like `stats`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) meters: Option<Event>,
}

/// A cursor past the newest event: it came from another server, or was made up.
#[derive(Debug, PartialEq)]
pub(crate) struct CursorAhead {
    pub(crate) after: u64,
    pub(crate) newest: u64,
}

/// The bounded, numbered buffer.
#[derive(Debug)]
pub(crate) struct Buffer {
    capacity: usize,
    events: VecDeque<Numbered>,
    /// The sequence number of the next event pushed.
    next_seq: u64,
    /// The newest `seq` any read has returned. Events up to it are never changed.
    read_up_to: u64,
    ended: bool,
    /// The newest `stats.tick`.
    stats: Option<Event>,
    /// The newest `meters.tick`.
    meters: Option<Event>,
}

impl Buffer {
    pub(crate) fn new(capacity: usize) -> Buffer {
        Buffer {
            capacity: capacity.max(1),
            events: VecDeque::new(),
            next_seq: 1,
            read_up_to: 0,
            ended: false,
            stats: None,
            meters: None,
        }
    }

    /// Appends `event`, dropping the oldest one if the buffer is full. A `stats.tick` or
    /// a `meters.tick` replaces the kept one of its type instead and takes no sequence
    /// number, and a `live.changed` may be merged into an unread one instead
    /// ([`Buffer::supersede`]). A people-only event (a `youtube.*` or `plugin.*`
    /// one) is not kept: agents don't see it (ADR-0058 §3, ADR-0069).
    pub(crate) fn push(&mut self, event: Event) {
        if is_people_only(&event) {
            return;
        }
        match event {
            Event::StatsTick { .. } => {
                self.stats = Some(event);
                return;
            }
            Event::MetersTick { .. } => {
                self.meters = Some(event);
                return;
            }
            _ => {}
        }
        let Some(event) = self.supersede(event) else {
            return;
        };
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(Numbered {
            seq: self.next_seq,
            event,
        });
        self.next_seq += 1;
    }

    /// For a `live.changed`, finds the newest buffered `live.changed` of the same gesture
    /// that no read has returned yet and that only `live.changed` events follow. If none
    /// of those later events sets a parameter `event` sets, merges `event`'s values over
    /// that one's in place, keeping its `seq`, and returns `None`. Otherwise it returns
    /// `event` to be buffered as usual. Merging moves nothing a reader has seen and
    /// leaves no gap in the numbers, so `dropped` stays exact.
    fn supersede(&mut self, event: Event) -> Option<Event> {
        let Event::LiveChanged {
            gesture_id,
            client_id,
            values,
        } = event
        else {
            return Some(event);
        };
        let read_up_to = self.read_up_to;
        let mut clashes = false;
        let older = self
            .events
            .iter_mut()
            .rev()
            .take_while(|e| e.seq > read_up_to && matches!(e.event, Event::LiveChanged { .. }))
            .find(|e| {
                let Event::LiveChanged {
                    gesture_id: g,
                    client_id: c,
                    values: theirs,
                } = &e.event
                else {
                    return false;
                };
                if *g == gesture_id && *c == client_id {
                    return true;
                }
                clashes |= theirs
                    .iter()
                    .any(|t| param(t).is_some_and(|p| values.iter().any(|v| param(v) == Some(p))));
                false
            });
        match older {
            Some(Numbered {
                event: Event::LiveChanged { values: old, .. },
                ..
            }) if !clashes => {
                old.retain(|o| param(o).is_none_or(|p| values.iter().all(|v| param(v) != Some(p))));
                old.extend(values);
                None
            }
            _ => Some(Event::LiveChanged {
                gesture_id,
                client_id,
                values,
            }),
        }
    }

    /// The events after `after` (0 for all), at most `page` of them.
    pub(crate) fn read(&mut self, after: u64, page: usize) -> Result<Page, CursorAhead> {
        let newest = self.next_seq - 1;
        if after > newest {
            return Err(CursorAhead { after, newest });
        }
        let oldest = self.events.front().map_or(self.next_seq, |e| e.seq);
        // Sequence numbers are contiguous, so the ones between the cursor and the oldest
        // kept event are exactly those dropped.
        let dropped = oldest.saturating_sub(after.saturating_add(1));
        let events: Vec<Numbered> = self
            .events
            .iter()
            .filter(|e| e.seq > after)
            .take(page)
            .cloned()
            .collect();
        let next = events.last().map_or(after, |e| e.seq);
        self.read_up_to = self.read_up_to.max(next);
        Ok(Page {
            more: next < newest,
            events,
            next,
            dropped,
            ended: self.ended,
            stats: self.stats.clone(),
            meters: self.meters.clone(),
        })
    }
}

/// The parameter a live value sets: its item, and whether it is the crop (else the
/// placement, which `item.set_transform` and `item.set_geometry` both set).
fn param(value: &Edit) -> Option<(&jstream_protocol::ItemId, bool)> {
    match value {
        Edit::ItemSetTransform { item_id, .. } | Edit::ItemSetGeometry { item_id, .. } => {
            Some((item_id, false))
        }
        Edit::ItemSetCrop { item_id, .. } => Some((item_id, true)),
        _ => None,
    }
}

/// The subscription and its buffer, behind one lock.
struct Shared {
    stream: Option<EventStream>,
    buffer: Buffer,
}

impl Shared {
    /// Moves every event the subscription has ready into the buffer. Polled with the pump
    /// thread's waker, so the thread is woken when more arrive, whoever polled last.
    fn drain(&mut self, waker: &Waker) {
        let mut cx = Context::from_waker(waker);
        while let Some(stream) = self.stream.as_mut() {
            match stream.as_mut().poll_next(&mut cx) {
                Poll::Ready(Some(event)) => self.buffer.push(event),
                Poll::Ready(None) => {
                    self.stream = None;
                    self.buffer.ended = true;
                }
                Poll::Pending => break,
            }
        }
    }
}

/// Whether `event` belongs to a [`PEOPLE_ONLY`] noun, so agents never see it.
fn is_people_only(event: &Event) -> bool {
    event.name().is_some_and(|name| {
        PEOPLE_ONLY
            .iter()
            .any(|noun| name.split_once('.').is_some_and(|(n, _)| n == *noun))
    })
}

fn lock(mutex: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Unparks the pump thread.
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// One event subscription, buffered for the `events` tool.
pub(crate) struct Feed {
    shared: Arc<Mutex<Shared>>,
    /// The pump thread's waker, or a no-op one if the thread could not be started.
    waker: Waker,
}

impl Feed {
    /// Starts buffering `stream` into a buffer of `capacity` events.
    ///
    /// The pump thread ends with the stream, which ends when the session goes away. If
    /// the thread can't be started, events are still buffered, but only when a read
    /// drains them.
    pub(crate) fn start(stream: EventStream, capacity: usize) -> Feed {
        let shared = Arc::new(Mutex::new(Shared {
            stream: Some(stream),
            buffer: Buffer::new(capacity),
        }));
        let pumped = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("jstream-mcp-events".into())
            .spawn(move || pump(&pumped));
        let waker = match spawned {
            Ok(handle) => Waker::from(Arc::new(ThreadWaker(handle.thread().clone()))),
            Err(_) => Waker::noop().clone(),
        };
        Feed { shared, waker }
    }

    /// The events after `after`: drains what has arrived, then reads.
    pub(crate) fn read(&self, after: u64) -> Result<Page, CursorAhead> {
        let mut shared = lock(&self.shared);
        shared.drain(&self.waker);
        shared.buffer.read(after, PAGE_SIZE)
    }
}

/// The pump thread: drain, park until woken, repeat until the stream ends.
fn pump(shared: &Mutex<Shared>) {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    loop {
        {
            let mut shared = lock(shared);
            shared.drain(&waker);
            if shared.stream.is_none() {
                return;
            }
        }
        // A spurious unpark just drains again.
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::{Buffer, CursorAhead, Numbered};
    use jstream_protocol::{ClientId, Crop, Edit, Event, GestureId, ItemId, Rev, YoutubeAuth};

    fn changed(rev: u64) -> Event {
        Event::StateChanged {
            rev: Rev(rev),
            client_id: ClientId::new("c"),
            request_id: None,
            cause: None,
        }
    }

    fn seqs(events: &[Numbered]) -> Vec<u64> {
        events.iter().map(|e| e.seq).collect()
    }

    #[test]
    fn reads_page_through_the_buffer_with_a_cursor() {
        let mut buffer = Buffer::new(10);
        let empty = buffer.read(0, 2).expect("a valid cursor");
        assert_eq!((empty.events.len(), empty.next, empty.more), (0, 0, false));
        for rev in 1..=5 {
            buffer.push(changed(rev));
        }
        let first = buffer.read(0, 2).expect("a valid cursor");
        assert_eq!(seqs(&first.events), [1, 2]);
        assert_eq!(first.events[1].event, changed(2));
        assert_eq!((first.next, first.dropped, first.more), (2, 0, true));
        let rest = buffer.read(first.next, 10).expect("a valid cursor");
        assert_eq!(seqs(&rest.events), [3, 4, 5]);
        assert_eq!((rest.next, rest.more), (5, false));
        let none = buffer.read(5, 10).expect("a valid cursor");
        assert_eq!((none.events.len(), none.next, none.more), (0, 5, false));
    }

    #[test]
    fn overflow_drops_the_oldest_and_reports_the_gap() {
        let mut buffer = Buffer::new(3);
        for rev in 1..=7 {
            buffer.push(changed(rev));
        }
        // 1 to 4 were dropped; a reader that saw 2 missed 3 and 4.
        let page = buffer.read(2, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [5, 6, 7]);
        assert_eq!(page.dropped, 2);
        let from_start = buffer.read(0, 10).expect("a valid cursor");
        assert_eq!(from_start.dropped, 4);
        // A reader that kept up missed nothing.
        assert_eq!(buffer.read(4, 10).map(|p| p.dropped), Ok(0));
        assert_eq!(buffer.read(6, 10).map(|p| p.dropped), Ok(0));
    }

    fn live(gesture: &str, values: Vec<Edit>) -> Event {
        Event::LiveChanged {
            gesture_id: GestureId::new(gesture),
            client_id: ClientId::new("c"),
            values,
        }
    }

    fn crop(item: &str, left: u32) -> Edit {
        Edit::ItemSetCrop {
            item_id: ItemId::new(item),
            scene_id: None,
            crop: Crop { left, ..Crop::NONE },
        }
    }

    /// A drag holds one buffered `live.changed`, with the newest value of each parameter,
    /// and every other event keeps its place.
    #[test]
    fn a_newer_live_changed_merges_into_an_unread_one_of_its_gesture() {
        let mut buffer = Buffer::new(10);
        buffer.push(changed(1));
        buffer.push(live("g", vec![crop("a", 1), crop("b", 1)]));
        buffer.push(live("h", vec![crop("c", 1)]));
        buffer.push(live("g", vec![crop("a", 2)]));
        let page = buffer.read(0, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [1, 2, 3]);
        assert_eq!(
            page.events[1].event,
            live("g", vec![crop("b", 1), crop("a", 2)])
        );

        // Not into an event a read has returned: the reader would never see the change.
        buffer.push(live("g", vec![crop("a", 3)]));
        let page = buffer.read(3, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [4]);
        assert_eq!(page.events[0].event, live("g", vec![crop("a", 3)]));

        // Not across another kind of event: its order relative to them matters.
        buffer.push(changed(2));
        buffer.push(live("g", vec![crop("a", 4)]));
        let page = buffer.read(4, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [5, 6]);

        // Nor across a `live.changed` that sets the same parameter: another gesture took
        // it over, and moving this value before that one would undo the takeover.
        let mut buffer = Buffer::new(10);
        buffer.push(live("g", vec![crop("a", 1)]));
        buffer.push(live("h", vec![crop("a", 2)]));
        buffer.push(live("g", vec![crop("a", 3)]));
        assert_eq!(seqs(&buffer.read(0, 10).expect("valid").events), [1, 2, 3]);
    }

    /// Merging leaves no gap in the numbers, so eviction still counts only real drops.
    #[test]
    fn merging_then_evicting_reports_only_the_evicted_events() {
        let mut buffer = Buffer::new(3);
        buffer.push(changed(1));
        buffer.push(live("g", vec![crop("a", 1)]));
        buffer.push(live("g", vec![crop("a", 2)]));
        buffer.push(changed(2));
        buffer.push(changed(3));
        let page = buffer.read(0, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [2, 3, 4]);
        assert_eq!(page.dropped, 1, "only seq 1 was dropped");
        assert_eq!(page.events[0].event, live("g", vec![crop("a", 2)]));
    }

    #[test]
    fn a_cursor_past_the_newest_event_is_refused() {
        let mut buffer = Buffer::new(3);
        buffer.push(changed(1));
        assert_eq!(
            buffer.read(2, 10),
            Err(CursorAhead {
                after: 2,
                newest: 1
            })
        );
    }

    /// YouTube and plugins are for people, through the UI (ADR-0058 §3, ADR-0069): their
    /// events never reach the feed, and take no sequence number.
    #[test]
    fn youtube_events_are_not_kept() {
        let mut buffer = Buffer::new(10);
        buffer.push(changed(1));
        buffer.push(Event::YoutubeStatusChanged {
            auth: YoutubeAuth::SignedIn,
        });
        buffer.push(Event::PluginStateChanged {
            plugin_id: jstream_protocol::PluginId::new("plg_1"),
            state: jstream_protocol::PluginState::Enabled,
            reason: None,
            remote_name: None,
        });
        buffer.push(changed(2));
        let page = buffer.read(0, 10).expect("a valid cursor");
        assert_eq!(seqs(&page.events), [1, 2]);
        assert_eq!(page.events[1].event, changed(2));
    }
}
