//! Tempo and transport from a MIDI clock.
//!
//! The MIDI specification's rules, which this follows:
//!
//! - A timing clock (`F8`) comes 24 times per quarter note while the sender's clock runs,
//!   whether or not its transport is playing. The tempo is measured from the spacing of
//!   clocks either way.
//! - Start (`FA`) sets the song position to 0, and the **next** clock is position 0.
//!   Continue (`FB`) keeps the song position, and the next clock plays it. Stop (`FC`)
//!   holds the position where it is: the next clock that would have played.
//! - Song position (`F2`) is in sixteenth notes, six clocks each. It is sent while
//!   stopped, before a continue.
//!
//! Every event comes with the media-clock time it maps to, in nanoseconds (ADR-0014).
//! For a JTunnel stream that is the sender's sample offset mapped through the receiver's
//! clock estimate, so the timing is the sender's, not the network's.

use crate::Message;

/// MIDI timing clocks per quarter note.
pub const TICKS_PER_BEAT: u32 = 24;

/// The spacing of clocks the tempo is averaged over: one beat's worth.
const WINDOW: usize = TICKS_PER_BEAT as usize;
/// A gap longer than this between clocks restarts the measurement: the clock stopped, or
/// the tempo is below 10 BPM, which no sender uses.
const MAX_TICK_NS: u64 = 250_000_000;
/// Clock spacings needed before the tempo is reported.
const MIN_INTERVALS: usize = 3;
/// How long after the last clock the clock still counts as running.
const CLOCK_TIMEOUT_NS: u64 = 500_000_000;

/// The tempo and transport at one moment, from [`Tempo::at`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoNow {
    /// Beats (quarter notes) per minute, averaged over the last beat of clocks. `None`
    /// until a few clocks have arrived with a steady spacing.
    pub bpm: Option<f64>,
    /// Whether clocks are arriving (one in the last half second).
    pub clock: bool,
    /// Whether the transport is playing: after start or continue, before stop.
    pub playing: bool,
    /// Quarter notes since the start of the song, with the fraction since the last clock
    /// carried forward from the tempo (never past the next clock's position).
    pub beat: f64,
    /// Bars since the start of the song, with its fraction, at the meter the tracker
    /// was made with. MIDI clock carries no meter.
    pub bar: f64,
    /// The beat within the bar, from 0 up to the beats per bar.
    pub beat_in_bar: f64,
}

/// Turns MIDI clock and transport messages into tempo, beat and bar.
///
/// It keeps a fixed amount of state and never allocates, so a MIDI input thread can run
/// it on every message.
#[derive(Clone, Debug)]
pub struct Tempo {
    beats_per_bar: u32,
    playing: bool,
    /// The song position, in clocks, that the next clock plays.
    next: u64,
    /// The position and time of the last clock that played, if one has since the last
    /// start, continue or song position.
    last_played: Option<(u64, u64)>,
    /// The time of the last clock received, playing or not.
    last_tick_ns: Option<u64>,
    intervals: [u64; WINDOW],
    count: usize,
    head: usize,
    sum: u64,
}

impl Tempo {
    /// A tracker for a meter of `beats_per_bar` quarter notes (at least 1), stopped at
    /// the start of the song with no tempo yet.
    #[must_use]
    pub fn new(beats_per_bar: u32) -> Self {
        Self {
            beats_per_bar: beats_per_bar.max(1),
            playing: false,
            next: 0,
            last_played: None,
            last_tick_ns: None,
            intervals: [0; WINDOW],
            count: 0,
            head: 0,
            sum: 0,
        }
    }

    /// The beats per bar the tracker counts bars with.
    #[must_use]
    pub const fn beats_per_bar(&self) -> u32 {
        self.beats_per_bar
    }

    /// Takes one message received at `at_ns` on the media clock. Messages other than
    /// clock, start, continue, stop and song position are ignored, and so is a song
    /// position while playing (the specification sends it only while stopped).
    pub fn apply(&mut self, message: &Message, at_ns: u64) {
        match *message {
            Message::TimingClock => self.tick(at_ns),
            Message::Start => {
                self.playing = true;
                self.next = 0;
                self.last_played = None;
            }
            Message::Continue => {
                self.playing = true;
                self.last_played = None;
            }
            Message::Stop => {
                self.playing = false;
                self.last_played = None;
            }
            Message::SongPosition { sixteenths } if !self.playing => {
                self.next = u64::from(sixteenths) * u64::from(TICKS_PER_BEAT / 4);
                self.last_played = None;
            }
            _ => {}
        }
    }

    fn tick(&mut self, at_ns: u64) {
        if let Some(prev) = self.last_tick_ns {
            let gap = at_ns.saturating_sub(prev);
            if gap == 0 || gap > MAX_TICK_NS {
                self.count = 0;
                self.head = 0;
                self.sum = 0;
            } else {
                if self.count == WINDOW {
                    self.sum -= self.intervals[self.head];
                } else {
                    self.count += 1;
                }
                self.intervals[self.head] = gap;
                self.sum += gap;
                self.head = (self.head + 1) % WINDOW;
            }
        }
        self.last_tick_ns = Some(at_ns);
        if self.playing {
            self.last_played = Some((self.next, at_ns));
            self.next += 1;
        }
    }

    /// The average spacing of clocks, in nanoseconds, once enough have arrived.
    fn tick_ns(&self) -> Option<f64> {
        (self.count >= MIN_INTERVALS).then(|| self.sum as f64 / self.count as f64)
    }

    /// The tempo, beat and bar at `at_ns` on the media clock.
    #[must_use]
    pub fn at(&self, at_ns: u64) -> TempoNow {
        let tick_ns = self.tick_ns();
        let ticks = match (self.playing, self.last_played, tick_ns) {
            (true, Some((pos, t)), Some(spacing)) => {
                let since = at_ns.saturating_sub(t) as f64 / spacing;
                pos as f64 + since.min(1.0)
            }
            (true, Some((pos, _)), None) => pos as f64,
            _ => self.next as f64,
        };
        let beat = ticks / f64::from(TICKS_PER_BEAT);
        let per_bar = f64::from(self.beats_per_bar);
        TempoNow {
            bpm: tick_ns.map(|spacing| 60e9 / (spacing * f64::from(TICKS_PER_BEAT))),
            clock: self
                .last_tick_ns
                .is_some_and(|t| at_ns.saturating_sub(t) <= CLOCK_TIMEOUT_NS),
            playing: self.playing,
            beat,
            bar: beat / per_bar,
            beat_in_bar: beat % per_bar,
        }
    }

    /// The media-clock time the song reaches `beat` (in quarter notes), extrapolated at
    /// the current tempo from the last clock that played. `None` while stopped, before
    /// the tempo is known, for a beat already past, or for one too far ahead to name.
    #[must_use]
    pub fn time_of_beat(&self, beat: f64) -> Option<u64> {
        let (pos, t) = self.last_played.filter(|_| self.playing)?;
        let spacing = self.tick_ns()?;
        let ahead = beat * f64::from(TICKS_PER_BEAT) - pos as f64;
        let wait = (ahead * spacing).round();
        if !(0.0..=u64::MAX as f64).contains(&wait) {
            return None;
        }
        t.checked_add(wait as u64)
    }

    /// The media-clock time of the first bar line after `at_ns`, as
    /// [`time_of_beat`](Self::time_of_beat) extrapolates it: where a "start on the next
    /// bar" lands.
    #[must_use]
    pub fn next_bar_at(&self, at_ns: u64) -> Option<u64> {
        let now = self.at(at_ns);
        let per_bar = f64::from(self.beats_per_bar);
        let next = (now.bar.floor() + 1.0) * per_bar;
        self.time_of_beat(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SEC: u64 = 1_000_000_000;

    /// Clock times for `bpm`, from `from` for `n` clocks.
    fn ticks(bpm: f64, from: u64, n: u64) -> impl Iterator<Item = u64> {
        let spacing = 60.0 * SEC as f64 / (bpm * f64::from(TICKS_PER_BEAT));
        (0..n).map(move |i| from + (i as f64 * spacing).round() as u64)
    }

    #[test]
    fn a_beat_too_far_ahead_has_no_time() {
        let mut t = Tempo::new(4);
        t.apply(&Message::Start, 0);
        for k in 0..48_u64 {
            t.apply(&Message::TimingClock, k * 20_833_333);
        }
        assert!(t.time_of_beat(4.0).is_some());
        assert_eq!(t.time_of_beat(1e300), None);
        assert_eq!(t.time_of_beat(f64::INFINITY), None);
    }

    #[test]
    fn measures_the_tempo_from_clock_spacing_even_while_stopped() {
        let mut t = Tempo::new(4);
        assert_eq!(t.at(0).bpm, None);
        for at in ticks(128.0, 0, 48) {
            t.apply(&Message::TimingClock, at);
        }
        let now = t.at(SEC);
        assert!((now.bpm.unwrap_or(0.0) - 128.0).abs() < 0.01, "{now:?}");
        assert!(now.clock);
        assert!(!now.playing);
        assert_eq!(now.beat, 0.0);
        // Half a second later without a clock, the clock has stopped; the tempo stays.
        assert!(!t.at(2 * SEC).clock);
        assert!(t.at(2 * SEC).bpm.is_some());
    }

    #[test]
    fn start_plays_position_zero_on_the_next_clock_and_counts_beats_and_bars() {
        let mut t = Tempo::new(4);
        t.apply(&Message::Start, 0);
        // 120 BPM: 2 beats a second, a clock every 1/48 s. 10 s is 20 beats, 5 bars.
        let mut last = 0;
        for at in ticks(120.0, 10, 480) {
            t.apply(&Message::TimingClock, at);
            last = at;
        }
        let now = t.at(last);
        assert!((now.beat - 479.0 / 24.0).abs() < 1e-9, "{now:?}");
        // Between clocks the beat moves on at the tempo.
        let half_tick = SEC / 96;
        let later = t.at(last + half_tick);
        assert!((later.beat - 479.5 / 24.0).abs() < 1e-3, "{later:?}");
        // But never past the next clock's position, when a clock is late.
        let stalled = t.at(last + SEC / 4);
        assert!((stalled.beat - 20.0).abs() < 1e-9, "{stalled:?}");
        assert!((stalled.bar - 5.0).abs() < 1e-9);
        assert!(stalled.beat_in_bar.abs() < 1e-9);
    }

    #[test]
    fn stop_holds_the_position_and_song_position_then_continue_moves_it() {
        let mut t = Tempo::new(3);
        t.apply(&Message::Start, 0);
        let mut last = 0;
        for at in ticks(90.0, 1, 30) {
            t.apply(&Message::TimingClock, at);
            last = at;
        }
        t.apply(&Message::Stop, last + 1);
        // 30 clocks played positions 0..=29; the held position is the next, 30.
        assert_eq!(t.at(last + SEC).beat, 30.0 / 24.0);
        // Clocks while stopped don't move it.
        for at in ticks(90.0, last + 10, 10) {
            t.apply(&Message::TimingClock, at);
        }
        assert_eq!(t.at(last + SEC).beat, 30.0 / 24.0);
        // Song position 16 (sixteenths) is beat 4, bar 4/3 in 3/4.
        t.apply(&Message::SongPosition { sixteenths: 16 }, last + SEC);
        let now = t.at(last + SEC);
        assert_eq!(now.beat, 4.0);
        assert!((now.bar - 4.0 / 3.0).abs() < 1e-12);
        assert!((now.beat_in_bar - 1.0).abs() < 1e-12);
        // Continue plays position 4 beats on the next clock.
        t.apply(&Message::Continue, last + 2 * SEC);
        t.apply(&Message::TimingClock, last + 2 * SEC + 5);
        assert_eq!(t.at(last + 2 * SEC + 5).beat, 4.0);
        // A song position while playing is ignored.
        t.apply(&Message::SongPosition { sixteenths: 0 }, last + 2 * SEC + 6);
        assert_eq!(t.at(last + 2 * SEC + 6).beat, 4.0);
    }

    #[test]
    fn a_long_gap_restarts_the_measurement_at_the_new_tempo() {
        let mut t = Tempo::new(4);
        for at in ticks(60.0, 0, 48) {
            t.apply(&Message::TimingClock, at);
        }
        // A second's silence, then 150 BPM: the old spacing doesn't drag the average.
        for at in ticks(150.0, 3 * SEC, 8) {
            t.apply(&Message::TimingClock, at);
        }
        let bpm = t.at(4 * SEC).bpm.unwrap_or(0.0);
        assert!((bpm - 150.0).abs() < 0.1, "{bpm}");
    }

    #[test]
    fn predicts_the_next_bar_line() {
        let mut t = Tempo::new(4);
        t.apply(&Message::Start, 0);
        // 120 BPM from t = 0: a bar is 2 s, so bar 1 starts at 2 s.
        let mut last = 0;
        for at in ticks(120.0, 0, 30) {
            t.apply(&Message::TimingClock, at);
            last = at;
        }
        let at = t.next_bar_at(last).unwrap_or(0);
        assert!(at.abs_diff(2 * SEC) < 1_000, "{at}");
        assert_eq!(t.time_of_beat(0.0), None, "a beat already past");
        t.apply(&Message::Stop, last);
        assert_eq!(t.next_bar_at(last), None);
    }
}
