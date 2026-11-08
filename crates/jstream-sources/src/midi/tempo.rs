//! An input's tempo, as its MIDI clock gives it (ADR-0142 §3): the input thread runs
//! `jstream-midi`'s [`Tempo`] tracker in its [`MidiFeed`] and publishes what it reads
//! into the input's [`MidiState`], where the `midi_beat` kind reads it.

use super::{MidiInputs, MidiState};
use jstream_midi::{Message, TICKS_PER_BEAT, Tempo};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering, fence};

/// How long after its last clock message an input's clock still counts as running, as
/// `jstream-midi` counts it.
pub const CLOCK_TIMEOUT_MS: u64 = 500;

/// The beats per bar the feed's tracker counts with. A `midi_beat` source counts bars
/// from the beat with its own meter, since MIDI clock carries none.
const TRACKER_METER: u32 = 4;

/// The tempo and transport an input's clock gave at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoReading {
    /// When it was read, in milliseconds on the media clock: the clock message's time.
    pub at_ms: u64,
    /// Beats (quarter notes) per minute, once the clock is steady.
    pub bpm: Option<f64>,
    /// Whether the transport is playing.
    pub playing: bool,
    /// Quarter notes since the start of the song, at `at_ms`.
    pub beat: f64,
}

impl TempoReading {
    /// Whether the input's clock still runs at `now_ms`: a clock message came in the
    /// last [`CLOCK_TIMEOUT_MS`].
    #[must_use]
    pub fn clock(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.at_ms) <= CLOCK_TIMEOUT_MS
    }

    /// The beat at `now_ms`: while playing at a known tempo, carried on from `beat`, but
    /// never past the next clock's position, as `jstream-midi` carries it.
    #[must_use]
    pub fn beat_at(&self, now_ms: u64) -> f64 {
        match self.bpm {
            Some(bpm) if self.playing => {
                let since = now_ms.saturating_sub(self.at_ms) as f64 * bpm / 60_000.0;
                self.beat + since.min(1.0 / f64::from(TICKS_PER_BEAT))
            }
            _ => self.beat,
        }
    }
}

/// A [`TempoReading`] in atomics behind a sequence number: odd while a publish writes
/// it. A publish that finds it odd skips, so two inputs handing over the every-input
/// state's tempo never interleave, and a reader retries rather than see it torn.
pub(super) struct TempoCell {
    seq: AtomicU64,
    /// `at_ms` plus one; 0 before any reading.
    at: AtomicU64,
    /// `bpm`'s bits; NaN's for none.
    bpm: AtomicU64,
    beat: AtomicU64,
    playing: AtomicU8,
    /// Which feed published last: the every-input state takes one input's clock at a
    /// time, until that clock stops.
    owner: AtomicUsize,
}

impl TempoCell {
    pub(super) fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            at: AtomicU64::new(0),
            bpm: AtomicU64::new(f64::NAN.to_bits()),
            beat: AtomicU64::new(0),
            playing: AtomicU8::new(0),
            owner: AtomicUsize::new(0),
        }
    }

    /// Publishes `reading` from feed `from`, unless another feed's clock holds it, or
    /// another publish is writing it. Returns whether it was published.
    pub(super) fn publish(&self, from: usize, reading: &TempoReading) -> bool {
        let owner = self.owner.load(Ordering::Relaxed);
        if owner != 0 && owner != from {
            let last = self.at.load(Ordering::Relaxed).saturating_sub(1);
            if reading.at_ms.saturating_sub(last) <= CLOCK_TIMEOUT_MS {
                return false;
            }
        }
        let seq = self.seq.load(Ordering::Relaxed);
        if seq % 2 == 1
            || self
                .seq
                .compare_exchange(seq, seq + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        // Keeps the stores below from being seen before the sequence turns odd.
        fence(Ordering::Release);
        self.owner.store(from, Ordering::Relaxed);
        self.at
            .store(reading.at_ms.saturating_add(1), Ordering::Relaxed);
        self.bpm
            .store(reading.bpm.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        self.beat.store(reading.beat.to_bits(), Ordering::Relaxed);
        self.playing
            .store(u8::from(reading.playing), Ordering::Relaxed);
        self.seq.store(seq + 2, Ordering::Release);
        true
    }

    /// The last reading published, or `None` before any (or, rarely, while a publish
    /// keeps writing it through every retry).
    pub(super) fn read(&self) -> Option<TempoReading> {
        for _ in 0..8 {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let at = self.at.load(Ordering::Relaxed);
            let bpm = f64::from_bits(self.bpm.load(Ordering::Relaxed));
            let beat = f64::from_bits(self.beat.load(Ordering::Relaxed));
            let playing = self.playing.load(Ordering::Relaxed) != 0;
            // Keeps the loads above from being seen after the check below.
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) != before {
                continue;
            }
            return (at > 0).then(|| TempoReading {
                at_ms: at - 1,
                bpm: (!bpm.is_nan()).then_some(bpm),
                playing,
                beat,
            });
        }
        None
    }
}

/// One input's feed into [`MidiInputs`]: its own state and the every-input one, looked
/// up once, and the tracker its clock runs. An input thread holds one per input and
/// pushes every message through it, without a lock or an allocation.
pub struct MidiFeed {
    own: Option<Arc<MidiState>>,
    any: Arc<MidiState>,
    tempo: Tempo,
    id: usize,
}

impl std::fmt::Debug for MidiFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiFeed")
            .field("named", &self.own.is_some())
            .finish_non_exhaustive()
    }
}

impl MidiFeed {
    /// Records `message`, which arrived at `at_ns` on the media clock, in the input's
    /// state and the every-input one. A clock, start, stop, continue or song position
    /// also moves the input's tempo, which both publish (the every-input state's from
    /// one input at a time).
    pub fn push(&mut self, at_ns: u64, message: &Message) {
        let at_ms = at_ns / 1_000_000;
        if matches!(
            message,
            Message::TimingClock
                | Message::Start
                | Message::Continue
                | Message::Stop
                | Message::SongPosition { .. }
        ) {
            self.tempo.apply(message, at_ns);
            let now = self.tempo.at(at_ns);
            let reading = TempoReading {
                at_ms,
                bpm: now.bpm,
                playing: now.playing,
                beat: now.beat,
            };
            if let Some(own) = &self.own {
                own.tempo.publish(self.id, &reading);
            }
            self.any.tempo.publish(self.id, &reading);
        }
        // After the tempo, so a source that sees the change count sees the tempo too.
        if let Some(own) = &self.own {
            own.push(at_ms, message);
        }
        self.any.push(at_ms, message);
    }
}

impl MidiInputs {
    /// The feed for input `name`. Past [`MAX_INPUTS`](super::MAX_INPUTS), or for a name
    /// [`input`](Self::input) refuses, it feeds only the every-input state.
    pub fn feed(&self, name: &str) -> MidiFeed {
        static FEEDS: AtomicUsize = AtomicUsize::new(1);
        MidiFeed {
            own: self.input(name),
            any: self.any(),
            tempo: Tempo::new(TRACKER_METER),
            id: FEEDS.fetch_add(1, Ordering::Relaxed),
        }
    }
}
