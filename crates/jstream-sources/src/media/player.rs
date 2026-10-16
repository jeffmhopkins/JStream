//! Playing one media file: a decode thread that reads video frames and audio blocks
//! ahead into short queues, and the timeline the source maps them onto.
//!
//! Times here are microseconds on the **timeline**: the file's own times plus one file
//! length for each loop already played, so a looping file's timeline keeps rising. The
//! source turns its clock time into a timeline position ([`Timing`]); the thread decodes
//! in timeline order, the source takes each frame once its position is reached, and the
//! audio feeder ([`audio`](super::audio)) hands each block on to the mixer shortly
//! before it is due.
//!
//! The thread reads the two streams in step: audio while it is behind the video, and
//! video while its queue has room. So neither stream runs far ahead of the other in the
//! file, and the platform never holds much of one while the other is read. While no
//! feeder takes the audio, the thread still reads it in step, keeping only the latest
//! second, so a feeder that opens later starts with what is due now.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use jstream_media::{
    AudioBuffer, AudioFormat, FrameSize, MediaTime, Rounding, Timebase, VideoFrame,
};
use jstream_platform::{MediaDecoder, PlatformError};

/// The frame length assumed when a file declares no frame rate, until two frames show
/// the real one: 1/30 s.
const DEFAULT_PERIOD_US: i64 = 33_333;
/// How much audio to decode ahead at most while a feeder takes it, in microseconds.
const AUDIO_AHEAD_US: i64 = 1_000_000;
/// The longest block of audio queued, in microseconds: one packet the feeder hands on
/// ([`PACKET_US`](super::audio::PACKET_US)).
const PIECE_US: i64 = super::audio::PACKET_US;

/// A decoded frame and where it sits on the timeline.
pub(super) struct Decoded {
    pub(super) at: i64,
    pub(super) frame: Arc<VideoFrame>,
}

/// A decoded block of audio and where its first frame sits on the timeline.
pub(super) struct Sound {
    pub(super) at: i64,
    /// Interleaved `f32` in the file's audio format.
    pub(super) samples: Vec<f32>,
    /// Its length in microseconds.
    pub(super) length: i64,
}

impl Sound {
    /// Where it ends on the timeline.
    pub(super) fn end(&self) -> i64 {
        self.at + self.length
    }

    /// The block cut into pieces of at most [`PIECE_US`] in `format`, each placed where
    /// its first frame falls. A decoder may hand over a long block at once, and the
    /// feeder hands on a whole block once its start is due: a block longer than a
    /// mixer input's ring then overwrote its own start there before it was heard, so
    /// most of each block was silence (#446, an MP3 file on Windows).
    fn pieces(self, format: AudioFormat) -> Vec<Sound> {
        let rate = i64::from(format.sample_rate()).max(1);
        let channels = format.channels().max(1);
        let per = usize::try_from(rate * PIECE_US / 1_000_000)
            .unwrap_or(1)
            .max(1);
        let Self { at, samples, .. } = self;
        samples
            .chunks(per * channels)
            .enumerate()
            .map(|(index, chunk)| {
                // From the block's start, as the feeder stamps packets, so the pieces
                // stay continuous however many there are.
                let offset = i64::try_from(index * per).unwrap_or(i64::MAX);
                let frames = i64::try_from(chunk.len() / channels).unwrap_or(0);
                Sound {
                    at: at.saturating_add(offset.saturating_mul(1_000_000) / rate),
                    samples: chunk.to_vec(),
                    length: frames * 1_000_000 / rate,
                }
            })
            .collect()
    }
}

/// How the source's clock maps onto the timeline: from `clock` (microseconds since the
/// source started), the timeline runs on from `timeline` at `speed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Timing {
    pub(super) clock: i64,
    pub(super) timeline: i64,
    pub(super) speed: f64,
}

impl Timing {
    /// The timeline position at clock time `at`.
    pub(super) fn position(&self, at: i64) -> i64 {
        let elapsed = (at - self.clock).max(0) as f64 * self.speed;
        self.timeline.saturating_add(elapsed as i64)
    }

    /// The first clock time at which the timeline reaches `to`.
    pub(super) fn clock_at(&self, to: i64) -> i64 {
        let ahead = (to - self.timeline).max(0) as f64 / self.speed;
        self.clock.saturating_add(ahead.ceil() as i64)
    }
}

/// What the source, the decode thread and the audio feeder share, under one lock.
pub(super) struct State {
    /// Frames decoded ahead, in timeline order.
    pub(super) queue: VecDeque<Decoded>,
    /// Frames to keep ahead at most.
    capacity: usize,
    /// Audio decoded ahead, in timeline order.
    pub(super) audio: VecDeque<Sound>,
    /// Whether a feeder takes the audio. Without one, the thread keeps only the latest
    /// [`AUDIO_AHEAD_US`] of what it reads.
    pub(super) audio_open: bool,
    /// Whether the program shows the source, so its audio is heard.
    pub(super) audible: bool,
    /// The source's clock mapping, which the feeder plays the audio by.
    pub(super) timing: Timing,
    /// Numbers the seeks, so a frame decoded before one is never queued after it.
    generation: u64,
    /// A seek the thread hasn't started: the timeline position to go to.
    seek: Option<i64>,
    /// Set by a seek, cleared by the first frame or block queued after it.
    pub(super) seeking: bool,
    /// Whether the file plays again from the start after its end.
    looping: bool,
    /// The decoder has nothing more to give until a seek: the file ended and isn't
    /// looping, or it failed.
    pub(super) ended: bool,
    /// The video ended in this play while its audio plays on: the last frame stays
    /// up, and the source isn't behind. Cleared by a seek and by the next play's first
    /// frame.
    pub(super) video_over: bool,
    /// Why the decoder stopped, when it failed.
    pub(super) failure: Option<String>,
    /// Set when the source goes away; the thread and the feeder stop at their next
    /// check.
    pub(super) stop: bool,
}

impl State {
    /// How much audio is queued, in microseconds.
    fn audio_queued(&self) -> i64 {
        match (self.audio.front(), self.audio.back()) {
            (Some(first), Some(last)) => last.end() - first.at,
            _ => 0,
        }
    }
}

/// The state and the condition the source, the thread and the feeder wait on.
pub(super) type Shared = Arc<(Mutex<State>, Condvar)>;

/// The decode thread's handle.
pub(super) struct Player {
    shared: Shared,
}

/// What the decode thread starts with.
pub(super) struct Start {
    /// The decoder, which has already given up the first frame if the file has video.
    pub(super) decoder: Box<dyn MediaDecoder>,
    /// That frame's position on the timeline, and the size every later frame must
    /// have, which the limits were checked against. `None` for a file with no video.
    pub(super) first: Option<(i64, FrameSize)>,
    /// Frames to keep ahead at most.
    pub(super) capacity: usize,
    pub(super) looping: bool,
    pub(super) timing: Timing,
}

impl Player {
    /// Starts the decode thread.
    pub(super) fn start(start: Start) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(State {
                queue: VecDeque::with_capacity(start.capacity),
                capacity: start.capacity,
                audio: VecDeque::new(),
                audio_open: false,
                audible: false,
                timing: start.timing,
                generation: 0,
                seek: None,
                seeking: false,
                video_over: false,
                looping: start.looping,
                ended: false,
                failure: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let thread_shared = Arc::clone(&shared);
        let Start { decoder, first, .. } = start;
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

    /// The state, for the audio feeder.
    pub(super) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// Tells the thread a frame was taken from the queue, so there is room again.
    pub(super) fn taken(&self) {
        self.shared.1.notify_all();
    }

    /// Asks the thread to go to timeline position `to`. Frames and audio queued before
    /// it are dropped; the frame shown keeps showing until the first one after it
    /// arrives.
    pub(super) fn seek(&self, to: i64) {
        seek(&self.shared, to);
    }

    /// Sets how the source's clock maps onto the timeline, for the audio feeder.
    pub(super) fn set_timing(&self, timing: Timing) {
        self.state().timing = timing;
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

/// Asks the decode thread to go to timeline position `to`, as [`Player::seek`].
fn seek(shared: &Shared, to: i64) {
    seek_locked(&mut lock(&shared.0), to);
    shared.1.notify_all();
}

/// [`seek`], with the state already locked. The caller notifies the condition once it
/// lets go of the lock.
pub(super) fn seek_locked(state: &mut State, to: i64) {
    state.generation += 1;
    state.seek = Some(to);
    state.seeking = true;
    state.queue.clear();
    state.audio.clear();
    state.video_over = false;
    state.ended = state.failure.is_some();
}

impl Drop for Player {
    /// Stops the thread without waiting for it: a read in progress finishes first, and
    /// the decoder is dropped on the thread.
    fn drop(&mut self) {
        self.state().stop = true;
        self.shared.1.notify_all();
    }
}

pub(super) fn lock(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
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
    Video {
        looping: bool,
    },
    Audio {
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
    /// One play's length, once known: from the container, then from the streams.
    length: Option<i64>,
    /// The last frame decoded in this play, in file time, and the gap before it.
    last: Option<i64>,
    period: i64,
    /// After a seek: frames shown before this position are skipped, apart from the last
    /// of them, `held`, which is shown at the position itself. Audio before it is
    /// skipped too.
    skip_to: Option<i64>,
    held: Option<Decoded>,
    /// The first frame's size, which every frame must have; `None` with no video.
    size: Option<FrameSize>,
    /// The audio format every block must have; `None` with no audio.
    audio: Option<AudioFormat>,
    /// Where the last audio block decoded in this play ends, in file time.
    audio_end: Option<i64>,
    /// The streams that have ended in this play.
    video_done: bool,
    audio_done: bool,
}

impl Decode {
    fn new(decoder: Box<dyn MediaDecoder>, first: Option<(i64, FrameSize)>) -> Self {
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
            last: first.map(|(at, _)| at),
            period,
            skip_to: None,
            held: None,
            size: first.map(|(_, size)| size),
            audio: info.audio.map(|a| a.format),
            audio_end: None,
            video_done: false,
            audio_done: false,
        }
    }

    /// Whether this play still has video to read.
    fn video_live(&self) -> bool {
        self.size.is_some() && !self.video_done
    }

    /// Whether this play still has audio to read.
    fn audio_live(&self) -> bool {
        self.audio.is_some() && !self.audio_done
    }

    /// The next step, with the state locked: a seek first, then whichever stream is
    /// behind and has room, or nothing yet.
    fn next(&self, state: &State) -> Option<Step> {
        if state.stop {
            return Some(Step::Stop);
        }
        if let Some(to) = state.seek {
            return Some(Step::Seek {
                to,
                generation: state.generation,
                looping: state.looping,
            });
        }
        if state.ended {
            return None;
        }
        let looping = state.looping;
        let (video, audio) = (self.video_live(), self.audio_live());
        let audio_room = !state.audio_open || state.audio_queued() < AUDIO_AHEAD_US;
        let behind = self.audio_end.unwrap_or(i64::MIN) <= self.last.unwrap_or(i64::MIN);
        if video && audio && behind && audio_room {
            Some(Step::Audio { looping })
        } else if video && state.queue.len() < state.capacity {
            Some(Step::Video { looping })
        } else if audio
            && !video
            && (if state.audio_open {
                audio_room
            } else {
                self.size.is_some()
            })
        {
            // Audio past the end of the video. With no feeder, an audio-only file waits
            // for one rather than racing through the file.
            Some(Step::Audio { looping })
        } else {
            None
        }
    }

    fn run(mut self, shared: &(Mutex<State>, Condvar)) {
        loop {
            let step = {
                let mut state = lock(&shared.0);
                loop {
                    if let Some(step) = self.next(&state) {
                        if matches!(step, Step::Seek { .. }) {
                            state.seek = None;
                        }
                        break step;
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
                Step::Video { looping } => self.video(shared, looping),
                Step::Audio { looping } => self.audio(shared, looping),
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
        self.new_play();
        self.skip_to = Some(base + time);
        self.decoder.seek(MediaTime::new(time, Timebase::MICROS))
    }

    /// Forgets what this play decoded, for a seek or the next loop.
    fn new_play(&mut self) {
        self.last = None;
        self.held = None;
        self.audio_end = None;
        self.video_done = false;
        self.audio_done = false;
    }

    /// Decodes the next frame and queues it, or handles the end of the video.
    fn video(
        &mut self,
        shared: &(Mutex<State>, Condvar),
        looping: bool,
    ) -> Result<(), PlatformError> {
        let Some(frame) = self.decoder.next_video()? else {
            self.video_done = true;
            if self.audio_live() {
                self.video_over(shared);
            }
            return self.ended(shared, looping);
        };
        let size = frame.info().size;
        if let Some(first) = self.size.filter(|&first| first != size) {
            // A stream whose shape changes partway: the queue and the limits were sized
            // for the first frame.
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a picture size change partway through, from {}x{} to {}x{}",
                    first.width(),
                    first.height(),
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

    /// Decodes the next block of audio and queues it, or handles the end of the audio.
    fn audio(
        &mut self,
        shared: &(Mutex<State>, Condvar),
        looping: bool,
    ) -> Result<(), PlatformError> {
        let Some(format) = self.audio else {
            return Ok(());
        };
        let Some(block) = self.decoder.next_audio()? else {
            self.audio_done = true;
            return self.ended(shared, looping);
        };
        if block.format() != format {
            // The mixer input was made for the first format.
            return Err(PlatformError::Unsupported {
                what: format!(
                    "an audio format change partway through, from {format:?} to {:?}",
                    block.format()
                ),
            });
        }
        let Some(sound) = self.sound(block) else {
            return Ok(());
        };
        let mut state = lock(&shared.0);
        if state.generation == self.generation && state.seek.is_none() {
            state.audio.extend(sound.pieces(format));
            while !state.audio_open && state.audio_queued() > AUDIO_AHEAD_US {
                state.audio.pop_front();
            }
            // With video, a seek is over once a frame comes: the source waits for one
            // rather than seeking again.
            if self.size.is_none() {
                state.seeking = false;
            }
        }
        Ok(())
    }

    /// A block on the timeline, cut to start at a seek's target; `None` when it ends
    /// before it.
    fn sound(&mut self, block: AudioBuffer) -> Option<Sound> {
        let rate = i64::from(block.format().sample_rate());
        let channels = block.format().channels();
        let start = micros(block.pts()).unwrap_or(0);
        let frames = i64::try_from(block.frames()).unwrap_or(i64::MAX);
        let end = start.saturating_add(frames.saturating_mul(1_000_000) / rate);
        self.audio_end = Some(end);
        let mut at = self.base + start;
        let mut samples = block.samples().to_vec();
        if let Some(skip_to) = self.skip_to.filter(|&skip_to| at < skip_to) {
            if self.base + end <= skip_to {
                return None;
            }
            let cut = ((skip_to - at) * rate)
                .div_euclid(1_000_000)
                .clamp(0, frames);
            let cut = usize::try_from(cut).unwrap_or(0);
            samples.drain(..cut * channels);
            at = skip_to;
        }
        let left = i64::try_from(samples.len() / channels.max(1)).unwrap_or(0);
        Some(Sound {
            at,
            samples,
            length: left * 1_000_000 / rate,
        })
    }

    /// One stream ended: once every stream has, the play has.
    fn ended(
        &mut self,
        shared: &(Mutex<State>, Condvar),
        looping: bool,
    ) -> Result<(), PlatformError> {
        if self.video_live() || self.audio_live() {
            return Ok(());
        }
        self.end(shared, looping)
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
        let video_end = self.last.map(|last| last + self.period);
        let Some(last) = video_end.max(self.audio_end) else {
            if let Some(target) = target.filter(|_| looping) {
                // A seek past the end before the length was known (the container
                // didn't say): play from the start at the target instead.
                self.base = target;
                self.new_play();
                return self.decoder.seek(MediaTime::zero(Timebase::MICROS));
            }
            // Nothing in this play: a file with nothing left to play.
            return self.stop(shared);
        };
        let length = last.max(1);
        self.length = Some(length);
        if !looping {
            return self.stop(shared);
        }
        self.base += length;
        self.new_play();
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
            state.video_over = false;
        }
    }

    /// The video ended before the audio: shows the frame a seek held back, since no
    /// later one comes, and tells the source its picture is over rather than behind.
    fn video_over(&mut self, shared: &(Mutex<State>, Condvar)) {
        if let Some(held) = self.held.take() {
            self.queue(shared, held);
        }
        let mut state = lock(&shared.0);
        if state.generation == self.generation && state.seek.is_none() {
            state.video_over = true;
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
                    audio: None,
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

        fn next_audio(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
            Ok(None)
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

    /// Starts a player on `clip`, whose first frame (at 0, 2x2) has been taken.
    fn start(clip: Clip, capacity: usize, looping: bool) -> Player {
        Player::start(Start {
            decoder: Box::new(clip),
            first: Some((0, size(2))),
            capacity,
            looping,
            timing: Timing {
                clock: 0,
                timeline: 0,
                speed: 1.0,
            },
        })
        .expect("a thread")
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
        let player = start(clip, 4, true);
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
        let player = start(clip, 8, false);
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
        let player = start(clip, 4, true);
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
