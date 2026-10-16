//! A media file's audio, handed on to the engine's mixer.
//!
//! [`MediaAudio`] is the source's audio as the engine sees it: its format, and a stream
//! it opens on a mixer input's producer ([`MediaAudio::open`]), as it opens a device.
//! The stream's `jstream-media-audio` thread takes the blocks the decode thread queued
//! and hands each on shortly before it is due, as a packet stamped with the media-clock
//! time the source's timeline reaches it. So the mixer places it as it places a
//! device's packet, and the audio lines up with the frames the source shows.
//!
//! It is heard only while the program shows the source ([`MediaAudio::set_audible`])
//! and plays at `speed` 1: at any other speed the source is silent, since the mixer
//! steers an input's rate by half a percent at most. Audio the timeline has passed is
//! dropped, and a jump in the timeline (a restart, a seek, the source shown again) is
//! a discontinuity the mixer re-anchors at. Silence in the file itself, such as where a
//! looping file's sound ends before its picture does, is a gap in the device position
//! instead, which the mixer fills with silence where it falls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jstream_media::{AudioFormat, MediaTime, Timebase};
use jstream_platform::{AudioInputHandler, AudioPacket, AudioPacketFlags, AudioStream, MediaClock};

use super::player::{self, Shared, Sound, lock, micros};

/// How often the thread hands audio on.
const TICK: Duration = Duration::from_millis(10);
/// How far ahead of the timeline audio is handed on, in microseconds: enough to ride
/// out a late tick, well inside what a mixer input's ring holds.
const LEAD_US: i64 = 100_000;
/// Audio that ends this far behind the timeline is dropped rather than handed on, in
/// microseconds: the mixer has mixed past it.
const LATE_US: i64 = 20_000;
/// With no video to catch the timeline up, audio this far behind it is skipped by a
/// seek rather than decoded through, in microseconds.
const CATCH_UP_US: i64 = 1_000_000;
/// The longest packet handed on, in microseconds: one device period.
pub(super) const PACKET_US: i64 = 10_000;
/// Two packets whose stamps are further apart than their length by more than this are
/// not continuous, in microseconds.
const JUMP_US: i64 = 1_000;
/// The longest silence in the file itself the device position steps over, rather than
/// flagging a discontinuity, in microseconds: well inside the 2 s gap a mixer input
/// fills with silence.
const GAP_US: i64 = 1_000_000;

/// A `media` source's audio. Clones share it.
#[derive(Clone)]
pub struct MediaAudio {
    shared: Shared,
    format: AudioFormat,
    /// Whether the file has video, which catches the timeline up after a jump.
    video: bool,
}

impl std::fmt::Debug for MediaAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaAudio")
            .field("format", &self.format)
            .field("video", &self.video)
            .finish_non_exhaustive()
    }
}

impl MediaAudio {
    pub(super) fn new(shared: Shared, format: AudioFormat, video: bool) -> Self {
        Self {
            shared,
            format,
            video,
        }
    }

    /// The format of every packet: the file's sample rate and channel layout,
    /// interleaved `f32`.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Sets whether the program shows the source, so its audio is heard. Until it is
    /// set, it isn't.
    pub fn set_audible(&self, audible: bool) {
        lock(&self.shared.0).audible = audible;
    }

    /// Starts handing the audio on to `handler` (a mixer input's producer), stamped on
    /// `clock`, where the source's time zero is `start`. Dropping the stream stops it,
    /// and the handler is dropped once its last call has returned.
    ///
    /// # Errors
    /// The thread could not be started, or a stream is already open on this audio.
    pub fn open(
        &self,
        handler: Box<dyn AudioInputHandler>,
        clock: Arc<dyn MediaClock>,
        start: MediaTime,
    ) -> Result<Box<dyn AudioStream>, String> {
        {
            let mut state = lock(&self.shared.0);
            if state.audio_open {
                return Err("the media file's audio is already playing".into());
            }
            state.audio_open = true;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let feed = Feed {
            shared: Arc::clone(&self.shared),
            stop: Arc::clone(&stop),
            handler,
            clock,
            start: micros(start).unwrap_or(0),
            format: self.format,
            video: self.video,
            position: 0,
            next: None,
            jumped: false,
        };
        let thread = thread::Builder::new()
            .name("jstream-media-audio".into())
            .spawn(move || feed.run());
        match thread {
            Ok(thread) => Ok(Box::new(MediaAudioStream {
                shared: Arc::clone(&self.shared),
                stop,
                thread: Some(thread),
                format: self.format,
            })),
            Err(err) => {
                lock(&self.shared.0).audio_open = false;
                Err(format!("starting the jstream-media-audio thread: {err}"))
            }
        }
    }
}

/// An open [`MediaAudio`]: its thread runs until this is dropped.
struct MediaAudioStream {
    shared: Shared,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    format: AudioFormat,
}

impl AudioStream for MediaAudioStream {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

impl Drop for MediaAudioStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        {
            // Under the lock, so the thread is either waiting (and is woken) or will see
            // the flag before it waits again.
            let _state = lock(&self.shared.0);
            self.shared.1.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            // A panic there has already stopped the feed; the handler went with it.
            let _ = thread.join();
        }
        lock(&self.shared.0).audio_open = false;
        // The decode thread may have been waiting for room.
        self.shared.1.notify_all();
    }
}

/// The thread's own state.
struct Feed {
    shared: Shared,
    stop: Arc<AtomicBool>,
    handler: Box<dyn AudioInputHandler>,
    clock: Arc<dyn MediaClock>,
    /// The source's time zero on the clock, in microseconds.
    start: i64,
    format: AudioFormat,
    video: bool,
    /// Frames handed on so far: the stream's device position.
    position: u64,
    /// Where the next packet's stamp falls if it continues the last one, in
    /// microseconds on the clock.
    next: Option<i64>,
    /// Whether audio was skipped since the last packet, so the next one is not
    /// continuous with it.
    jumped: bool,
}

impl Feed {
    fn run(mut self) {
        let shared = Arc::clone(&self.shared);
        loop {
            let due = {
                let state = lock(&shared.0);
                if self.stop.load(Ordering::Acquire) || state.stop {
                    return;
                }
                let mut state = shared
                    .1
                    .wait_timeout(state, TICK)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                if self.stop.load(Ordering::Acquire) || state.stop {
                    return;
                }
                let now = self.now();
                self.take(&mut state, now)
            };
            // There may be room for the decode thread again.
            shared.1.notify_all();
            for (sound, timing) in due {
                self.hand_on(&sound, timing);
            }
        }
    }

    /// The source's clock now, in microseconds.
    fn now(&self) -> i64 {
        micros(self.clock.now()).map_or(0, |now| (now - self.start).max(0))
    }

    /// Takes from the queue what is due by `now` (on the source's clock), dropping what
    /// can't be heard. Returns the blocks to hand on, each with the timing it plays by.
    fn take(&mut self, state: &mut player::State, now: i64) -> Vec<(Sound, player::Timing)> {
        let timing = state.timing;
        if !state.audible {
            // Kept for when it is heard again, unless the timeline has passed it by
            // then; the decode thread waits meanwhile.
            self.jumped = true;
            return Vec::new();
        }
        let position = timing.position(now);
        let mut due = Vec::new();
        let mut behind = 0;
        while let Some(front) = state.audio.front() {
            let late = front.end() <= position - LATE_US;
            // At another speed the source is silent: the audio goes as the timeline
            // passes it.
            let silent = timing.speed != 1.0 && front.at <= position;
            if late || silent {
                behind = behind.max(position - front.end());
                state.audio.pop_front();
                self.jumped = true;
            } else if timing.speed == 1.0 && front.at <= position + LEAD_US {
                if let Some(sound) = state.audio.pop_front() {
                    due.push((sound, timing));
                }
            } else {
                break;
            }
        }
        if !self.video && behind > CATCH_UP_US && !state.seeking && !state.ended {
            // Nothing else catches an audio-only file up: skip to where it is heard.
            drop(due);
            player::seek_locked(state, position);
            self.jumped = true;
            return Vec::new();
        }
        due
    }

    /// Hands one block on as packets of at most [`PACKET_US`], each stamped with the
    /// clock time the timeline reaches it.
    fn hand_on(&mut self, sound: &Sound, timing: player::Timing) {
        let channels = self.format.channels();
        let rate = i64::from(self.format.sample_rate());
        let per_packet = usize::try_from((rate * PACKET_US / 1_000_000).max(1)).unwrap_or(1);
        for (index, chunk) in sound.samples.chunks(per_packet * channels).enumerate() {
            let frames = chunk.len() / channels;
            if frames == 0 {
                continue;
            }
            let offset = i64::try_from(index * per_packet).unwrap_or(0) * 1_000_000 / rate;
            let stamp = self.start + timing.clock_at(sound.at + offset);
            let continuous = self
                .next
                .is_some_and(|next| (stamp - next).abs() <= JUMP_US);
            // Nothing skipped, and the stamp a little ahead: silence in the file, such as
            // a looping file's picture outlasting its sound before the next play. The
            // position steps over it, so the mixer hears a gap where it falls. A flagged
            // discontinuity there would not re-anchor at once, the stamp being off the
            // line the mixer fitted, but some 400 ms later, as a second hole (#445).
            let silence = self
                .next
                .map(|next| stamp - next)
                .filter(|&gap| !self.jumped && gap > JUMP_US && gap <= GAP_US);
            if let Some(gap) = silence {
                self.position += u64::try_from((gap * rate + 500_000) / 1_000_000).unwrap_or(0);
            }
            let continuous = continuous || silence.is_some();
            let discontinuity = self.next.is_some() && (self.jumped || !continuous);
            self.jumped = false;
            self.handler.on_packet(&AudioPacket {
                samples: chunk,
                device_position: self.position,
                time: Some(MediaTime::new(stamp, Timebase::MICROS)),
                flags: AudioPacketFlags {
                    discontinuity,
                    silent: false,
                },
            });
            let frames = u64::try_from(frames).unwrap_or(0);
            self.position += frames;
            self.next = Some(stamp + i64::try_from(frames).unwrap_or(0) * 1_000_000 / rate);
        }
    }
}
