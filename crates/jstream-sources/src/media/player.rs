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

use jstream_media::{FrameSize, MediaTime, Rounding, Timebase, VideoFrame};
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
    /// frame, at `first` on the timeline and of `size`. Every later frame must have that
    /// size, which the limits were checked against.
    pub(super) fn start(
        decoder: Box<dyn MediaDecoder>,
        first: i64,
        size: FrameSize,
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
                let run =
                    AssertUnwindSafe(|| Decode::new(decoder, first, size).run(&thread_shared));
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
    Seek {
        to: i64,
        generation: u64,
        looping: bool,
    },
    Decode {
        looping: bool,
    },
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
    /// The first frame's size, which every frame must have.
    size: FrameSize,
}

impl Decode {
    fn new(decoder: Box<dyn MediaDecoder>, first: i64, size: FrameSize) -> Self {
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
            size,
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
                            looping: state.looping,
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
                // Decoding and seeking run with the lock released, so `frame` never
                // waits on the platform.
                Step::Seek {
                    to,
                    generation,
                    looping,
                } => {
                    self.generation = generation;
                    self.seek(to, looping)
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
        let size = frame.info().size;
        if size != self.size {
            // A stream whose shape changes partway: the queue and the limits were sized
            // for the first frame.
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a picture size change partway through, from {}x{} to {}x{}",
                    self.size.width(),
                    self.size.height(),
                    size.width(),
                    size.height()
                ),
            });
        }
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
        let target = self.skip_to.take();
        let Some(last) = self.last else {
            if let Some(target) = target.filter(|_| looping) {
                // A seek past the end before the length was known (the container
                // didn't say): play from the start at the target instead.
                self.base = target;
                return self.decoder.seek(MediaTime::zero(Timebase::MICROS));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgba_frame;
    use jstream_platform::{MediaInfo, MediaVideoInfo};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant};

    const PERIOD_US: i64 = 40_000;

    fn size(side: u32) -> FrameSize {
        FrameSize::new(side, side).expect("a size")
    }

    /// A decoder for these tests: `frames` frames 40 ms apart, each of `sizes(index)`,
    /// with or without a duration; its seeks can be made to wait for a go-ahead.
    struct Clip {
        info: MediaInfo,
        frames: i64,
        next: i64,
        sizes: fn(i64) -> u32,
        /// Told when a seek starts, then waited on before it finishes.
        gate: Option<(Sender<()>, Receiver<()>)>,
    }

    impl Clip {
        fn new(frames: i64, duration: bool, sizes: fn(i64) -> u32) -> Self {
            Self {
                info: MediaInfo {
                    video: Some(MediaVideoInfo {
                        size: size(sizes(0)),
                        frame_period: None,
                    }),
                    audio: false,
                    duration: duration
                        .then(|| MediaTime::new(frames * PERIOD_US, Timebase::MICROS)),
                },
                frames,
                next: 0,
                sizes,
                gate: None,
            }
        }
    }

    impl MediaDecoder for Clip {
        fn info(&self) -> &MediaInfo {
            &self.info
        }

        fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
            if self.next >= self.frames {
                return Ok(None);
            }
            let side = (self.sizes)(self.next);
            let pixels = (side * side * 4) as usize;
            let pts = MediaTime::new(self.next * PERIOD_US, Timebase::MICROS);
            self.next += 1;
            let frame = rgba_frame(size(side), vec![0; pixels], pts).expect("a frame");
            Ok(Some(frame))
        }

        fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError> {
            if let Some((started, go)) = &self.gate {
                let _ = started.send(());
                let _ = go.recv();
            }
            self.next = micros(to).unwrap_or(0).div_euclid(PERIOD_US).max(0);
            Ok(())
        }
    }

    /// Waits until `done` holds of the state, or fails after five seconds.
    fn wait(player: &Player, what: &str, done: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&player.state()) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// B3 of #245's review: the decode thread seeks with the lock released, so the
    /// source can take frames while the platform seeks.
    #[test]
    fn a_seek_runs_without_the_lock() {
        let mut clip = Clip::new(10, true, |_| 2);
        let (started_tx, started) = channel();
        let (go, go_rx) = channel();
        clip.gate = Some((started_tx, go_rx));
        clip.next = 1;
        let player = Player::start(Box::new(clip), 0, size(2), 4, true).expect("a thread");
        player.seek(200_000);
        started
            .recv_timeout(Duration::from_secs(5))
            .expect("the seek started");
        let free = player.shared.0.try_lock().is_ok();
        go.send(()).expect("the seek goes on");
        assert!(free, "the state is free while the decoder seeks");
        wait(&player, "the frame after the seek", |s| !s.queue.is_empty());
        assert_eq!(player.state().queue[0].at, 200_000);
    }

    /// N5: a frame of another size than the first stops the source, since the limits
    /// and the queue were sized for the first.
    #[test]
    fn a_size_change_partway_is_an_error() {
        let mut clip = Clip::new(10, true, |i| if i < 3 { 2 } else { 4 });
        clip.next = 1;
        let player = Player::start(Box::new(clip), 0, size(2), 8, false).expect("a thread");
        wait(&player, "the failure", |s| s.failure.is_some());
        let state = player.state();
        assert_eq!(
            state.queue.len(),
            2,
            "the frames before the change are kept"
        );
        assert!(
            state
                .failure
                .as_deref()
                .is_some_and(|f| f.contains("2x2 to 4x4")),
            "{:?}",
            state.failure
        );
    }

    /// N7: a looping file whose length isn't known yet, sought past its end, plays from
    /// the start at the target rather than stopping.
    #[test]
    fn a_seek_past_an_unknown_end_plays_on_when_looping() {
        let mut clip = Clip::new(5, false, |_| 2);
        clip.next = 1;
        let player = Player::start(Box::new(clip), 0, size(2), 4, true).expect("a thread");
        wait(&player, "the queue to fill", |s| s.queue.len() == 4);
        player.seek(1_000_000);
        wait(&player, "a frame after the seek", |s| !s.queue.is_empty());
        let state = player.state();
        assert!(!state.ended, "it plays on");
        assert_eq!(
            state.queue[0].at, 1_000_000,
            "the file's start, at the target"
        );
        assert_eq!(state.queue.get(1).map(|d| d.at), Some(1_040_000));
    }
}
