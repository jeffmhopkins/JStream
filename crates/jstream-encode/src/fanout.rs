//! Fan-out: one session's packets offered to every subscriber through its own bounded
//! queue.
//!
//! The encode thread never waits on a subscriber. A full queue drops the packet and
//! counts it against that subscriber alone (R-RT-004). After a video drop the subscriber
//! is resynchronised: it gets nothing more until the next keyframe, since the packets in
//! between can't be decoded without the one it lost. A new subscriber likewise starts at
//! the next keyframe. Every AAC packet is a keyframe, so audio subscribers start and
//! resume at once. A new video subscriber asks the encoder for a keyframe, so with an
//! encoder that can force one it starts at the next frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::time::Duration;

use jstream_platform::media::Packet;

use crate::session::{Counters, Shared};
use crate::{StreamEnd, StreamInfo};

/// What one subscriber has been given, and what it lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubscriberStats {
    /// Packets queued for the subscriber.
    pub delivered: u64,
    /// Packets the subscriber lost: dropped because its queue was full, plus the video
    /// packets skipped afterwards while waiting for a keyframe. Packets skipped before a
    /// new subscriber's first keyframe are not counted.
    pub dropped: u64,
}

#[derive(Debug, Default)]
pub(crate) struct SubCounters {
    delivered: AtomicU64,
    dropped: AtomicU64,
}

impl SubCounters {
    fn snapshot(&self) -> SubscriberStats {
        SubscriberStats {
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

/// Where a subscriber is in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubState {
    /// New: waiting for its first keyframe.
    Joining,
    /// Receiving every packet.
    Live,
    /// Lost a packet: waiting for the next keyframe.
    Resync,
}

/// The encode thread's side of one subscription.
#[derive(Debug)]
pub(crate) struct Sub {
    tx: SyncSender<Packet>,
    state: SubState,
    counters: Arc<SubCounters>,
}

impl Sub {
    /// A new subscriber queue of `capacity` packets (at least one), and its receiving
    /// end.
    pub(crate) fn new(capacity: usize, shared: Arc<Shared>) -> (Self, Subscription) {
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity.max(1));
        let counters = Arc::new(SubCounters::default());
        let sub = Self {
            tx,
            state: SubState::Joining,
            counters: Arc::clone(&counters),
        };
        let subscription = Subscription {
            rx,
            shared,
            counters,
        };
        (sub, subscription)
    }

    /// Offers `packet`. Returns `false` when the subscription has been dropped, so the
    /// subscriber should be removed.
    fn offer(&mut self, packet: &Packet, session: &Counters) -> bool {
        if !packet.is_keyframe() {
            match self.state {
                SubState::Joining => return true,
                SubState::Resync => {
                    self.lost(session);
                    return true;
                }
                SubState::Live => {}
            }
        }
        // Cloning a packet clones an `Arc`, not the payload.
        match self.tx.try_send(packet.clone()) {
            Ok(()) => {
                self.counters.delivered.fetch_add(1, Ordering::Relaxed);
                self.state = SubState::Live;
                true
            }
            Err(TrySendError::Full(_)) => {
                self.lost(session);
                self.state = SubState::Resync;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn lost(&self, session: &Counters) {
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        session.subscriber_drops.fetch_add(1, Ordering::Relaxed);
    }
}

/// Every subscriber of one session. Owned by the encode thread.
#[derive(Debug, Default)]
pub(crate) struct FanOut {
    subs: Vec<Sub>,
}

impl FanOut {
    /// Adds the subscribers that joined since the last call.
    pub(crate) fn admit(&mut self, joins: &Receiver<Sub>, counters: &Counters) {
        while let Ok(sub) = joins.try_recv() {
            self.subs.push(sub);
        }
        counters.set_subscribers(self.subs.len());
    }

    /// Offers every packet in `packets` to every subscriber, in order, and empties
    /// `packets`. Subscribers whose [`Subscription`] was dropped are removed.
    pub(crate) fn publish(&mut self, packets: &mut Vec<Packet>, counters: &Counters) {
        if packets.is_empty() {
            return;
        }
        for packet in packets.drain(..) {
            // Release, so a snapshot that sees the packet also sees every input received
            // before it (`Counters::snapshot`).
            counters.packets.fetch_add(1, Ordering::Release);
            if packet.is_keyframe() {
                counters.keyframes.fetch_add(1, Ordering::Release);
            }
            self.subs.retain_mut(|sub| sub.offer(&packet, counters));
        }
        counters.set_subscribers(self.subs.len());
    }
}

/// One output's view of a session: its packets, in decode order, starting at a
/// keyframe.
///
/// The queue is bounded. When the subscriber falls behind and its queue is full, packets
/// are dropped and counted in [`stats`](Self::stats); after a video drop, delivery
/// resumes at the next keyframe. Dropping the subscription unsubscribes; the session
/// notices on its next packet.
///
/// Every receive method returns the queued packets first, then, once the session has
/// ended and the queue is empty, how it ended ([`StreamEnd`]), on every later call too.
#[derive(Debug)]
pub struct Subscription {
    rx: Receiver<Packet>,
    shared: Arc<Shared>,
    counters: Arc<SubCounters>,
}

impl Subscription {
    /// What the stream carries: the codec that was opened, and the settings it was opened
    /// with. A muxer builds its track header from these (frame size, rate, bit rate, and
    /// the AAC `AudioSpecificConfig`); H.264 parameter sets travel in-band in keyframes
    /// (see the crate README's open questions).
    pub fn info(&self) -> &StreamInfo {
        &self.shared.info
    }

    /// Waits for the next packet.
    ///
    /// # Errors
    /// How the stream ended, once it has and every queued packet has been received.
    pub fn recv(&self) -> Result<Packet, StreamEnd> {
        self.rx.recv().map_err(|_| self.end())
    }

    /// The next packet if one is queued, without waiting.
    ///
    /// # Errors
    /// As for [`recv`](Self::recv).
    pub fn try_recv(&self) -> Result<Option<Packet>, StreamEnd> {
        match self.rx.try_recv() {
            Ok(packet) => Ok(Some(packet)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(self.end()),
        }
    }

    /// Waits up to `timeout` for the next packet; `Ok(None)` when none arrived.
    ///
    /// # Errors
    /// As for [`recv`](Self::recv).
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<Packet>, StreamEnd> {
        match self.rx.recv_timeout(timeout) {
            Ok(packet) => Ok(Some(packet)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(self.end()),
        }
    }

    /// Asks the session for a keyframe, as
    /// [`VideoSession::request_keyframe`](crate::VideoSession::request_keyframe) does,
    /// for a consumer that holds only its subscription. It does nothing on an audio
    /// stream, whose packets are all keyframes.
    pub fn request_keyframe(&self) {
        self.shared.want_keyframe();
    }

    /// What this subscriber has been given and lost so far.
    pub fn stats(&self) -> SubscriberStats {
        self.counters.snapshot()
    }

    fn end(&self) -> StreamEnd {
        // The encode thread records the end before it drops any subscriber queue, so it
        // is set whenever the queue reports a disconnect.
        self.shared.end().unwrap_or(StreamEnd::Closed)
    }
}
