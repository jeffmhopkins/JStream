//! The encode thread and the state it shares with the session's handles. Video and
//! audio sessions are thin wrappers around [`Core`], which is generic over a [`Driver`]
//! that adapts one platform encoder trait.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use jstream_platform::media::Packet;
use jstream_platform::{AudioEncoderConfig, CodecInfo, PlatformError, VideoEncoderConfig};

use crate::fanout::{FanOut, Sub};
use crate::{EncodeError, PushError, StreamEnd, Subscription};

/// The settings a session's encoder was opened with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderSettings {
    /// A video encoder.
    Video(VideoEncoderConfig),
    /// An audio encoder.
    Audio(AudioEncoderConfig),
}

/// What a session's packet stream carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// The codec implementation that was opened (`name`, `codec`, `hardware`).
    pub codec: CodecInfo,
    /// The settings it was opened with.
    pub settings: EncoderSettings,
}

/// How a session queues, polls and drains. The defaults suit one video mix at up to
/// 60 fps; the engine sizes the audio input queue for its mixer's block length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    /// Inputs (frames or sample blocks) queued ahead of the encoder. When it is full the
    /// render or mixer side's push drops the input and counts it (R-RT-004). At least
    /// one; 0 is treated as 1.
    pub input_capacity: usize,
    /// Packets queued for each subscriber, unless it subscribes with its own capacity.
    /// At least one; 0 is treated as 1.
    pub subscriber_capacity: usize,
    /// The longest the encode thread waits before polling the encoder again. Hardware
    /// encoders driven asynchronously need polling every tick, input or not, so keep it
    /// below one frame period. Values under 100 µs are raised to 100 µs.
    pub poll_interval: Duration,
    /// How long [`finish`](crate::VideoSession::finish) waits, in total, for a busy
    /// encoder to accept the inputs still queued. Inputs it can't place by then are
    /// dropped and counted as `dropped_encoder_busy`.
    pub finish_timeout: Duration,
    /// Audio only: the longest gap, left by dropped sample blocks, that is filled with
    /// silence. See the crate README's audio drop policy.
    pub max_audio_gap: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            input_capacity: 8,
            subscriber_capacity: 512,
            poll_interval: Duration::from_millis(4),
            finish_timeout: Duration::from_secs(2),
            max_audio_gap: Duration::from_secs(1),
        }
    }
}

/// A session's counters. Each is a running total since the session started, except
/// `subscribers`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SessionStats {
    /// Inputs (frames or sample blocks) accepted into the input queue.
    pub received: u64,
    /// Inputs dropped by `push` because the input queue was full.
    pub dropped_queue_full: u64,
    /// Inputs dropped because the encoder refused them as busy
    /// ([`PlatformError::Busy`]), or could not take them before `finish_timeout`.
    pub dropped_encoder_busy: u64,
    /// Inputs the encoder accepted.
    pub encoded: u64,
    /// Packets the encoder produced, each offered to every subscriber.
    pub packets: u64,
    /// How many of those packets were keyframes.
    pub keyframes: u64,
    /// Video only: keyframe requests passed to the encoder and accepted by it (one per
    /// frame at most, however many were asked for before it).
    pub keyframes_requested: u64,
    /// Subscribers attached as of the last packet published.
    pub subscribers: u64,
    /// Packets lost by subscribers, summed over all of them
    /// ([`SubscriberStats::dropped`](crate::SubscriberStats::dropped)).
    pub subscriber_drops: u64,
    /// Audio only: sample frames of silence inserted for gaps left by dropped blocks.
    pub silence_frames: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    received: AtomicU64,
    dropped_queue_full: AtomicU64,
    dropped_encoder_busy: AtomicU64,
    encoded: AtomicU64,
    pub(crate) packets: AtomicU64,
    pub(crate) keyframes: AtomicU64,
    keyframes_requested: AtomicU64,
    subscribers: AtomicU64,
    pub(crate) subscriber_drops: AtomicU64,
    pub(crate) silence_frames: AtomicU64,
}

impl Counters {
    pub(crate) fn set_subscribers(&self, n: usize) {
        self.subscribers
            .store(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> SessionStats {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        SessionStats {
            received: get(&self.received),
            dropped_queue_full: get(&self.dropped_queue_full),
            dropped_encoder_busy: get(&self.dropped_encoder_busy),
            encoded: get(&self.encoded),
            packets: get(&self.packets),
            keyframes: get(&self.keyframes),
            keyframes_requested: get(&self.keyframes_requested),
            subscribers: get(&self.subscribers),
            subscriber_drops: get(&self.subscriber_drops),
            silence_frames: get(&self.silence_frames),
        }
    }
}

/// State shared by the encode thread, the session, its inputs and its subscriptions.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) info: StreamInfo,
    pub(crate) counters: Counters,
    /// `finish` was called: drain, then end.
    finishing: AtomicBool,
    /// The session was dropped: end without draining.
    aborting: AtomicBool,
    /// An input was dropped since the last one queued, so the next one queued follows a
    /// gap.
    gap: AtomicBool,
    /// A keyframe was asked for: request one before the next input is encoded.
    keyframe_wanted: AtomicBool,
    end: OnceLock<StreamEnd>,
}

impl Shared {
    fn new(info: StreamInfo) -> Arc<Self> {
        Arc::new(Self {
            info,
            counters: Counters::default(),
            finishing: AtomicBool::new(false),
            aborting: AtomicBool::new(false),
            gap: AtomicBool::new(false),
            keyframe_wanted: AtomicBool::new(false),
            end: OnceLock::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests(info: StreamInfo) -> Arc<Self> {
        Self::new(info)
    }

    pub(crate) fn end(&self) -> Option<StreamEnd> {
        self.end.get().cloned()
    }

    /// Asks the encode thread to request a keyframe before it encodes the next input.
    /// Never blocks; requests made before that input coalesce into one.
    pub(crate) fn want_keyframe(&self) {
        self.keyframe_wanted.store(true, Ordering::Release);
    }
}

/// A message to the encode thread.
pub(crate) enum Msg<I> {
    /// One input. `after_drop` says an input was dropped just before it.
    Item { input: I, after_drop: bool },
    /// Re-check the session's flags now.
    Wake,
}

/// Adapts one platform encoder trait for the encode loop.
pub(crate) trait Driver: Send + 'static {
    type Input: Send + 'static;

    /// Whether `encode` would take an input now.
    fn can_accept(&self) -> bool;

    /// Encodes one input. [`PlatformError::Busy`] means it was dropped; any other error
    /// stops the session.
    fn encode(
        &mut self,
        input: Self::Input,
        after_drop: bool,
        counters: &Counters,
    ) -> Result<(), PlatformError>;

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;

    /// Asks the encoder to make the next input a keyframe; `true` when it agreed. An
    /// encoder that refuses still places its regular keyframes, so a refusal is not an
    /// error. Audio packets are all keyframes, so the default does nothing.
    fn request_keyframe(&mut self) -> bool {
        false
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;
}

/// The thread-safe handle the render or mixer side pushes into.
pub(crate) struct InputHandle<I> {
    tx: SyncSender<Msg<I>>,
    shared: Arc<Shared>,
}

impl<I> Clone for InputHandle<I> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<I> InputHandle<I> {
    /// Queues `input` without blocking, or drops it.
    pub(crate) fn push(&self, input: I) -> Result<(), PushError> {
        let s = &self.shared;
        if s.finishing.load(Ordering::Acquire) || s.aborting.load(Ordering::Acquire) {
            return Err(PushError::Closed);
        }
        let after_drop = s.gap.swap(false, Ordering::AcqRel);
        match self.tx.try_send(Msg::Item { input, after_drop }) {
            Ok(()) => {
                s.counters.received.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                s.gap.store(true, Ordering::Release);
                s.counters
                    .dropped_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                Err(PushError::Full)
            }
            Err(TrySendError::Disconnected(_)) => Err(PushError::Closed),
        }
    }
}

/// How the encode thread paces itself.
#[derive(Clone, Copy, Debug)]
struct Timing {
    poll_interval: Duration,
    finish_timeout: Duration,
}

/// One running session: the encode thread plus the channels into it.
pub(crate) struct Core<D: Driver> {
    input: InputHandle<D::Input>,
    joins: Sender<Sub>,
    shared: Arc<Shared>,
    subscriber_capacity: usize,
    waker: Thread,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl<D: Driver> Core<D> {
    /// Starts the encode thread, named `name`, driving `driver`.
    pub(crate) fn start(
        name: &str,
        driver: D,
        info: StreamInfo,
        options: &SessionOptions,
    ) -> Result<Self, EncodeError> {
        let shared = Shared::new(info);
        let (tx, rx) = std::sync::mpsc::sync_channel(options.input_capacity.max(1));
        let (joins, joined) = std::sync::mpsc::channel();
        let timing = Timing {
            poll_interval: options.poll_interval.max(Duration::from_micros(100)),
            finish_timeout: options.finish_timeout,
        };
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name(name.into())
            .spawn(move || encode_thread(driver, rx, joined, thread_shared, timing))
            .map_err(|e| EncodeError::Spawn {
                message: e.to_string(),
            })?;
        Ok(Self {
            input: InputHandle {
                tx,
                shared: Arc::clone(&shared),
            },
            joins,
            shared,
            subscriber_capacity: options.subscriber_capacity,
            waker: handle.thread().clone(),
            thread: Mutex::new(Some(handle)),
        })
    }

    pub(crate) fn input(&self) -> InputHandle<D::Input> {
        self.input.clone()
    }

    pub(crate) fn info(&self) -> &StreamInfo {
        &self.shared.info
    }

    pub(crate) fn stats(&self) -> SessionStats {
        self.shared.counters.snapshot()
    }

    pub(crate) fn subscribe(&self, capacity: Option<usize>) -> Subscription {
        let capacity = capacity.unwrap_or(self.subscriber_capacity);
        let (sub, subscription) = Sub::new(capacity, Arc::clone(&self.shared));
        // If the thread has ended, the queue is dropped here and the subscription
        // reports how the session ended.
        let _ = self.joins.send(sub);
        // The subscriber starts at the next keyframe, so ask for one now rather than
        // leaving it to wait out the keyframe interval. The request is applied before the
        // next input is encoded, and the subscriber is admitted before that input's packet
        // is published, so the forced keyframe is its first packet.
        self.shared.want_keyframe();
        subscription
    }

    pub(crate) fn request_keyframe(&self) {
        self.shared.want_keyframe();
    }

    pub(crate) fn finish(&self) -> Result<SessionStats, EncodeError> {
        self.shared.finishing.store(true, Ordering::Release);
        self.wake();
        self.join();
        match self.shared.end() {
            Some(StreamEnd::Finished) => Ok(self.stats()),
            Some(StreamEnd::Failed(e)) => Err(e),
            // `Closed` comes only from dropping the session, which can't happen while
            // `finish` borrows it; and `join` records an end for a thread that died.
            Some(StreamEnd::Closed) | None => Err(EncodeError::Panicked {
                message: "the encode thread ended without a result".into(),
            }),
        }
    }

    fn wake(&self) {
        // A full queue needs no wake-up: the thread is about to take an input anyway.
        let _ = self.input.tx.try_send(Msg::Wake);
        self.waker.unpark();
    }

    /// Joins the encode thread, once. Concurrent callers wait for the first.
    fn join(&self) {
        let mut thread = lock(&self.thread);
        if let Some(handle) = thread.take()
            && let Err(payload) = handle.join()
        {
            // Only a panic outside the caught region (dropping the encoder) lands here.
            let _ = self
                .shared
                .end
                .set(StreamEnd::Failed(EncodeError::Panicked {
                    message: panic_message(payload.as_ref()),
                }));
        }
    }
}

impl<D: Driver> Drop for Core<D> {
    fn drop(&mut self) {
        self.shared.aborting.store(true, Ordering::Release);
        self.wake();
        self.join();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded value is an `Option<JoinHandle>`, valid whatever a panicking holder did.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "a non-string panic payload".into()
    }
}

/// The encode thread's body. Owns the driver, the input queue and every subscriber
/// queue; records how the session ended before it drops any of them, so subscribers
/// that see their queue disconnect always find the end.
fn encode_thread<D: Driver>(
    mut driver: D,
    rx: Receiver<Msg<D::Input>>,
    joined: Receiver<Sub>,
    shared: Arc<Shared>,
    timing: Timing,
) {
    let mut fan = FanOut::default();
    let end = catch_unwind(AssertUnwindSafe(|| {
        run(&mut driver, &rx, &joined, &mut fan, &shared, timing)
    }))
    .unwrap_or_else(|payload| {
        StreamEnd::Failed(EncodeError::Panicked {
            message: panic_message(payload.as_ref()),
        })
    });
    let _ = shared.end.set(end);
    drop(fan);
    drop(joined);
    drop(rx);
    drop(driver);
}

fn run<D: Driver>(
    driver: &mut D,
    rx: &Receiver<Msg<D::Input>>,
    joined: &Receiver<Sub>,
    fan: &mut FanOut,
    shared: &Shared,
    timing: Timing,
) -> StreamEnd {
    let mut out = Vec::with_capacity(16);
    let result = (|| -> Result<StreamEnd, EncodeError> {
        loop {
            if shared.aborting.load(Ordering::Acquire) {
                return Ok(StreamEnd::Closed);
            }
            if shared.finishing.load(Ordering::Acquire) {
                break;
            }
            if driver.can_accept() {
                // Inputs are taken only when the encoder can take them, so a slow
                // encoder fills the input queue and `push` drops at the source.
                match rx.recv_timeout(timing.poll_interval) {
                    Ok(Msg::Item { input, after_drop }) => {
                        encode_one(driver, input, after_drop, shared)?;
                    }
                    Ok(Msg::Wake) | Err(RecvTimeoutError::Timeout) => {}
                    // The session holds a sender until it has set `aborting`.
                    Err(RecvTimeoutError::Disconnected) => return Ok(StreamEnd::Closed),
                }
            } else {
                // An asynchronous encoder asks for input only after a poll pumps its
                // events, so keep polling every interval.
                thread::park_timeout(timing.poll_interval);
            }
            step(driver, &mut out, joined, fan, shared)?;
        }

        // Finishing: encode what is still queued, then drain the encoder.
        // `None`: a timeout too long to represent, so no limit.
        let deadline = Instant::now().checked_add(timing.finish_timeout);
        while let Ok(msg) = rx.try_recv() {
            let Msg::Item { input, after_drop } = msg else {
                continue;
            };
            while !driver.can_accept() && deadline.is_none_or(|d| Instant::now() < d) {
                thread::sleep(timing.poll_interval);
                // Polling pumps an asynchronous encoder, which may then ask for input.
                step(driver, &mut out, joined, fan, shared)?;
            }
            if driver.can_accept() {
                encode_one(driver, input, after_drop, shared)?;
            } else {
                shared
                    .counters
                    .dropped_encoder_busy
                    .fetch_add(1, Ordering::Relaxed);
            }
            step(driver, &mut out, joined, fan, shared)?;
        }
        driver.finish(&mut out).map_err(EncodeError::Encoder)?;
        fan.admit(joined, &shared.counters);
        fan.publish(&mut out, &shared.counters);
        Ok(StreamEnd::Finished)
    })();
    result.unwrap_or_else(StreamEnd::Failed)
}

fn encode_one<D: Driver>(
    driver: &mut D,
    input: D::Input,
    after_drop: bool,
    shared: &Shared,
) -> Result<(), EncodeError> {
    let c = &shared.counters;
    // A load first, so the common case (no request) is a plain read, not a swap.
    if shared.keyframe_wanted.load(Ordering::Relaxed)
        && shared.keyframe_wanted.swap(false, Ordering::Acquire)
        && driver.request_keyframe()
    {
        c.keyframes_requested.fetch_add(1, Ordering::Relaxed);
    }
    match driver.encode(input, after_drop, c) {
        Ok(()) => {
            c.encoded.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        Err(PlatformError::Busy { .. }) => {
            c.dropped_encoder_busy.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        Err(e) => Err(EncodeError::Encoder(e)),
    }
}

/// Polls the encoder and publishes what it produced. New subscribers are admitted first,
/// so one that subscribed before a packet was published is offered that packet.
fn step<D: Driver>(
    driver: &mut D,
    out: &mut Vec<Packet>,
    joined: &Receiver<Sub>,
    fan: &mut FanOut,
    shared: &Shared,
) -> Result<(), EncodeError> {
    driver.poll(out).map_err(EncodeError::Encoder)?;
    fan.admit(joined, &shared.counters);
    fan.publish(out, &shared.counters);
    Ok(())
}
