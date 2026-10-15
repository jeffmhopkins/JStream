//! Playing one media file: a decode thread that reads frames ahead into a short queue,
//! and the timeline the source maps them onto.
//!
//! Times here are microseconds on the **timeline**: the file's own times plus one file
//! length for each loop already played, so a looping file's timeline keeps rising. The
//! source turns its clock time into a timeline position (see `MediaSource`); the thread
//! decodes in timeline order and the source takes each frame once its position is
//! reached.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use jstream_media::{MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::{MediaDecoder, PlatformError};

/// The frame length assumed when a file declares no frame rate, until two frames show
/// the real one: 1/30 s.
const DEFAULT_PERIOD_US: i64 = 33_333;

/// A decoded frame and where it sits on the timeline.
pub(super) struct Decoded {
    pub(super) at: i64,
    pub(super) frame: Arc<VideoFrame>,
}

/// What the source and the decode thread share, under one lock.
pub(super) struct State {
    /// Frames decoded ahead, in timeline order.
    pub(super) queue: VecDeque<Decoded>,
    /// Frames to keep ahead at most.
    capacity: usize,
    /// Numbers the seeks, so a frame decoded before one is never queued after it.
    generation: u64,
    /// A seek the thread hasn't started: the timeline position to go to.
    seek: Option<i64>,
    /// Set by a seek, cleared by the first frame queued after it.
    pub(super) seeking: bool,
    /// Whether the file plays again from the start after its end.
    looping: bool,
    /// The decoder has nothing more to give until a seek: the file ended and isn't
    /// looping, or it failed.
    pub(super) ended: bool,
    /// Why the decoder stopped, when it failed.
    pub(super) failure: Option<String>,
    /// Set when the source goes away; the thread stops at its next check.
    stop: bool,
}

/// The decode thread's handle: the shared state and the condition both sides wait on.
pub(super) struct Player {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Player {
    /// Starts the decode thread on `decoder`, which has already given up its first
    /// frame, `first`, at the start of the timeline.
    pub(super) fn start(
        decoder: Box<dyn MediaDecoder>,
        first: i64,
        capacity: usize,
        looping: bool,
    ) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(State {
                queue: VecDeque::with_capacity(capacity),
                capacity,
                generation: 0,
                seek: None,
                seeking: false,
                looping,
                ended: false,
                failure: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("jstream-media-file".into())
            .spawn(move || {
                let run = AssertUnwindSafe(|| Decode::new(decoder, first).run(&thread_shared));
                if catch_unwind(run).is_err() {
                    let mut state = lock(&thread_shared.0);
                    state.failure = Some("the decode thread panicked".into());
                    state.ended = true;
                }
            })?;
        Ok(Self { shared })
    }

    /// The shared state, locked.
    pub(super) fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.shared.0)
    }

    /// Tells the thread a frame was taken from the queue, so there is room again.
    pub(super) fn taken(&self) {
        self.shared.1.notify_all();
    }

    /// Asks the thread to go to timeline position `to`. Frames queued before it are
    /// dropped; the frame shown keeps showing until the first one after it arrives.
    pub(super) fn seek(&self, to: i64) {
        let mut state = self.state();
        state.generation += 1;
        state.seek = Some(to);
        state.seeking = true;
        state.queue.clear();
        state.ended = state.failure.is_some();
        drop(state);
        self.shared.1.notify_all();
    }

    /// Sets whether the file loops. Turning it on after the end starts the next play at
    /// `at`, the position now.
    pub(super) fn set_looping(&self, looping: bool, at: i64) {
        let mut state = self.state();
        if state.looping == looping {
            return;
        }
        state.looping = looping;
        let restart = looping && state.ended && state.failure.is_none();
        drop(state);
        if restart {
            self.seek(at);
        } else {
            self.shared.1.notify_all();
        }
    }
}

impl Drop for Player {
    /// Stops the thread without waiting for it: a read in progress finishes first, and
    /// the decoder is dropped on the thread.
    fn drop(&mut self) {
        self.state().stop = true;
        self.shared.1.notify_all();
    }
}

fn lock(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
    // The state stays consistent at every unlock, so a panic elsewhere doesn't spoil it.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the thread does next.
enum Step {
    Seek { to: i64, generation: u64 },
    Decode { looping: bool },
    Stop,
}

/// The decode thread's own state.
struct Decode {
    decoder: Box<dyn MediaDecoder>,
    /// The generation of the frames it decodes now.
    generation: u64,
    /// Where the current play of the file starts on the timeline.
    base: i64,
    /// One play's length, once known: from the container, then from the frames.
    length: Option<i64>,
    /// The last frame decoded in this play, in file time, and the gap before it.
    last: Option<i64>,
    period: i64,
    /// After a seek: frames shown before this position are skipped, apart from the last
    /// of them, `held`, which is shown at the position itself.
    skip_to: Option<i64>,
    held: Option<Decoded>,
}

impl Decode {
    fn new(decoder: Box<dyn MediaDecoder>, first: i64) -> Self {
        let info = *decoder.info();
        let period = info
            .video
            .and_then(|v| v.frame_period)
            .and_then(|p| micros(MediaTime::new(1, p)))
            .filter(|&p| p > 0)
            .unwrap_or(DEFAULT_PERIOD_US);
        Self {
            length: info.duration.and_then(micros).filter(|&d| d > 0),
            decoder,
            generation: 0,
            base: 0,
            last: Some(first),
            period,
            skip_to: None,
            held: None,
        }
    }

    fn run(mut self, shared: &(Mutex<State>, Condvar)) {
        loop {
            let step = {
                let mut state = lock(&shared.0);
                loop {
                    if state.stop {
                        break Step::Stop;
                    }
                    if let Some(to) = state.seek.take() {
                        break Step::Seek {
                            to,
                            generation: state.generation,
                        };
                    }
                    if !state.ended && state.queue.len() < state.capacity {
                        break Step::Decode {
                            looping: state.looping,
                        };
                    }
                    state = shared.1.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
            };
            let result = match step {
                Step::Stop => return,
                Step::Seek { to, generation } => {
                    self.generation = generation;
                    self.seek(to, lock(&shared.0).looping)
                }
                Step::Decode { looping } => self.decode(shared, looping),
            };
            if let Err(e) = result {
                let mut state = lock(&shared.0);
                state.failure = Some(e.to_string());
                state.ended = true;
                return;
            }
        }
    }

    /// Goes to timeline position `to`: the play that holds it, and the file time in it.
    fn seek(&mut self, to: i64, looping: bool) -> Result<(), PlatformError> {
        let to = to.max(0);
        let (base, time) = match self.length {
            Some(length) if looping => {
                let base = to - to.rem_euclid(length);
                (base, to - base)
            }
            // Not looping: past the end is the last frame, so land just before the end.
            Some(length) => (0, to.min(length - 1)),
            None => (0, to),
        };
        self.base = base;
        self.last = None;
        self.held = None;
        self.skip_to = Some(base + time);
        self.decoder.seek(MediaTime::new(time, Timebase::MICROS))
    }

    /// Decodes the next frame and queues it, or handles the end of the file.
    fn decode(
        &mut self,
        shared: &(Mutex<State>, Condvar),
        looping: bool,
    ) -> Result<(), PlatformError> {
        let Some(frame) = self.decoder.next_video()? else {
            return self.end(shared, looping);
        };
        let time = micros(frame.info().pts).unwrap_or(0);
        if let Some(last) = self.last.filter(|&last| time > last) {
            self.period = time - last;
        }
        self.last = Some(time);
        let decoded = Decoded {
            at: self.base + time,
            frame: Arc::new(frame),
        };
        if let Some(skip_to) = self.skip_to {
            if decoded.at <= skip_to {
                self.held = Some(decoded);
                return Ok(());
            }
            self.skip_to = None;
            if let Some(held) = self.held.take() {
                self.queue(shared, held);
            }
        }
        self.queue(shared, decoded);
        Ok(())
    }

    /// The file ended: queue what a seek held back, learn the play's length, and start
    /// the next play or stop.
    fn end(
        &mut self,
        shared: &(Mutex<State>, Condvar),
        looping: bool,
    ) -> Result<(), PlatformError> {
        if let Some(held) = self.held.take() {
            self.queue(shared, held);
        }
        self.skip_to = None;
        let Some(last) = self.last else {
            // Nothing in this play: a file with no frames left to show.
            return self.stop(shared);
        };
        let length = (last + self.period).max(1);
        self.length = Some(length);
        if !looping {
            return self.stop(shared);
        }
        self.base += length;
        self.last = None;
        self.decoder.seek(MediaTime::zero(Timebase::MICROS))
    }

    /// Nothing more comes until a seek.
    fn stop(&mut self, shared: &(Mutex<State>, Condvar)) -> Result<(), PlatformError> {
        let mut state = lock(&shared.0);
        if state.generation == self.generation {
            state.ended = true;
        }
        Ok(())
    }

    /// Queues a frame, unless a seek has made it stale.
    fn queue(&self, shared: &(Mutex<State>, Condvar), decoded: Decoded) {
        let mut state = lock(&shared.0);
        if state.generation == self.generation && state.seek.is_none() {
            state.queue.push_back(decoded);
            state.seeking = false;
        }
    }
}

/// `t` in whole microseconds.
pub(super) fn micros(t: MediaTime) -> Option<i64> {
    t.rescale(Timebase::MICROS, Rounding::Down)
        .ok()
        .map(MediaTime::ticks)
}
