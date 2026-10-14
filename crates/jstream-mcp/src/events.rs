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
//! Docs: crates/jstream-mcp/README.md#design, docs/adr/0036-mcp-events-polling-tool.md

use jstream_control::EventStream;
use jstream_protocol::Event;
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
    ended: bool,
}

impl Buffer {
    pub(crate) fn new(capacity: usize) -> Buffer {
        Buffer {
            capacity: capacity.max(1),
            events: VecDeque::new(),
            next_seq: 1,
            ended: false,
        }
    }

    /// Appends `event`, dropping the oldest one if the buffer is full.
    pub(crate) fn push(&mut self, event: Event) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(Numbered {
            seq: self.next_seq,
            event,
        });
        self.next_seq += 1;
    }

    /// The events after `after` (0 for all), at most `page` of them.
    pub(crate) fn read(&self, after: u64, page: usize) -> Result<Page, CursorAhead> {
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
        Ok(Page {
            more: next < newest,
            events,
            next,
            dropped,
            ended: self.ended,
        })
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
    use jstream_protocol::{ClientId, Event, Rev};

    fn changed(rev: u64) -> Event {
        Event::StateChanged {
            rev: Rev(rev),
            client_id: ClientId::new("c"),
            request_id: None,
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
}
