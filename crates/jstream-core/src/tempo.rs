//! The engine's tempo as shared values (ADR-0122 §4, ADR-0147): `tempo.bpm`,
//! `tempo.playing`, `tempo.beat` and `tempo.bar`, from the MIDI clock the every-input
//! MIDI state follows. A `jstream-tempo` thread reads that state without a lock and
//! takes the engine's only when a value changes, which is at most once a beat while a
//! song plays.
//!
//! The render thread reads the same state each frame, through [`Tempo`], for the
//! song's position the filters that follow the tempo move with (ADR-0148).

use std::sync::Arc;

use jstream_media::{MediaTime, Rounding, Timebase};
use jstream_sources::{MidiInputs, TempoReading};
use serde_json::{Value, json};

/// How often the thread reads the tempo.
pub(crate) const EVERY: std::time::Duration = std::time::Duration::from_millis(20);

/// The beats a bar `tempo.bar` counts: MIDI clock carries no meter.
const BEATS_PER_BAR: u64 = 4;

/// The least change of tempo that is published, in beats per minute: a clock's jitter
/// moves its measured tempo by less.
const BPM_STEP: f64 = 0.1;

/// The tempo values as last published.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Published {
    /// While a clock runs at a known tempo, rounded to a tenth.
    pub(crate) bpm: Option<f64>,
    /// While a clock or transport has been heard.
    pub(crate) playing: Option<bool>,
    /// Whole beats and bars since the start of the song.
    pub(crate) beat: Option<u64>,
    pub(crate) bar: Option<u64>,
}

impl Published {
    /// What `reading` gives at `now_ms`, after `self`: the tempo moves only by at least
    /// [`BPM_STEP`], so it doesn't flicker between two tenths.
    pub(crate) fn next(&self, reading: Option<&TempoReading>, now_ms: u64) -> Self {
        let Some(reading) = reading else {
            return Self::default();
        };
        let clock = reading.clock(now_ms);
        let bpm = reading.bpm.filter(|_| clock).map(|bpm| match self.bpm {
            Some(last) if (bpm - last).abs() < BPM_STEP => last,
            _ => (bpm * 10.0).round() / 10.0,
        });
        let beat = reading.beat_at(now_ms).max(0.0).floor() as u64;
        Self {
            bpm,
            playing: Some(reading.playing && clock),
            beat: Some(beat),
            bar: Some(beat / BEATS_PER_BAR),
        }
    }

    /// The values that differ from `before`, by name: each new value, or `None` for one
    /// to remove.
    pub(crate) fn changes(&self, before: &Self) -> Vec<(&'static str, Option<Value>)> {
        let mut out = Vec::new();
        if self.bpm != before.bpm {
            out.push(("tempo.bpm", self.bpm.map(|bpm| json!(bpm))));
        }
        if self.playing != before.playing {
            out.push(("tempo.playing", self.playing.map(|p| json!(p))));
        }
        if self.beat != before.beat {
            out.push(("tempo.beat", self.beat.map(|b| json!(b))));
        }
        if self.bar != before.bar {
            out.push(("tempo.bar", self.bar.map(|b| json!(b))));
        }
        out
    }
}

/// The engine's MIDI inputs, as a plan hands them to the render thread: equal only to
/// the same inputs.
#[derive(Clone)]
pub(crate) struct Tempo(pub(crate) Arc<MidiInputs>);

impl PartialEq for Tempo {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Tempo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tempo")
    }
}

impl Tempo {
    /// The song's position at `at` on the media clock, in quarter notes since its start
    /// (ADR-0148): the every-input clock's beat carried on to `at` while it plays, never
    /// past its next clock, so a clock that goes quiet holds; where it stopped while it
    /// is stopped; 0 before any clock. Reads without a lock or an allocation.
    pub(crate) fn beat_at(&self, at: MediaTime) -> f64 {
        let ms = at
            .rescale(Timebase::MILLIS, Rounding::Down)
            .map_or(0, |t| u64::try_from(t.ticks()).unwrap_or(0));
        beat_of(self.0.any().tempo(), ms)
    }
}

/// The song's position at `now_ms` from `reading`, as [`Tempo::beat_at`] gives it.
fn beat_of(reading: Option<TempoReading>, now_ms: u64) -> f64 {
    reading.map_or(0.0, |reading| reading.beat_at(now_ms).max(0.0))
}

/// The every-input tempo now, and the media clock's time, if it has one.
pub(crate) fn read(midi: &MidiInputs) -> Option<(Option<TempoReading>, u64)> {
    let now_ms = midi.now_ms()?;
    Some((midi.any().tempo(), now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(bpm: Option<f64>, playing: bool, beat: f64, at_ms: u64) -> TempoReading {
        TempoReading {
            at_ms,
            clock_ms: Some(at_ms),
            bpm,
            playing,
            beat,
        }
    }

    /// ADR-0148: the render thread's beat runs on while the song plays, holds where it
    /// stopped, holds a tick past a clock that went quiet, and is 0 before any clock.
    #[test]
    fn the_song_position_runs_while_it_plays_and_holds_otherwise() {
        assert_eq!(beat_of(None, 5_000), 0.0);
        let playing = reading(Some(120.0), true, 8.0, 1_000);
        assert!((beat_of(Some(playing), 1_010) - 8.02).abs() < 1e-9);
        // MIDI clock ticks 24 times a beat.
        let a_tick = 8.0 + 1.0 / 24.0;
        assert!((beat_of(Some(playing), 9_000) - a_tick).abs() < 1e-9);
        let stopped = reading(Some(120.0), false, 8.0, 1_000);
        assert_eq!(beat_of(Some(stopped), 9_000), 8.0);
        let fresh = Tempo(Arc::new(MidiInputs::new()));
        assert_eq!(
            fresh.beat_at(MediaTime::zero(Timebase::MILLIS)),
            0.0,
            "no clock yet"
        );
        assert_eq!(fresh, fresh.clone());
        assert_ne!(fresh, Tempo(Arc::new(MidiInputs::new())));
    }

    #[test]
    fn nothing_is_published_before_a_clock() {
        assert_eq!(Published::default().next(None, 1000), Published::default());
        assert!(
            Published::default()
                .changes(&Published::default())
                .is_empty()
        );
    }

    #[test]
    fn a_playing_clock_publishes_whole_beats_and_bars() {
        let now = Published::default().next(Some(&reading(Some(120.04), true, 9.5, 1000)), 1000);
        assert_eq!(
            now,
            Published {
                bpm: Some(120.0),
                playing: Some(true),
                beat: Some(9),
                bar: Some(2),
            }
        );
        let names: Vec<_> = now
            .changes(&Published::default())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            ["tempo.bpm", "tempo.playing", "tempo.beat", "tempo.bar"]
        );
        // Within the beat, and a jitter under a tenth: nothing changes.
        let later = now.next(Some(&reading(Some(120.08), true, 9.9, 1010)), 1010);
        assert!(later.changes(&now).is_empty());
        // The next beat moves only the beat.
        let next = later.next(Some(&reading(Some(119.97), true, 10.0, 1020)), 1020);
        assert_eq!(next.changes(&later), [("tempo.beat", Some(json!(10)))]);
    }

    #[test]
    fn a_quiet_clock_removes_the_tempo_and_stops_playing() {
        let now = Published::default().next(Some(&reading(Some(90.0), true, 4.0, 0)), 0);
        let quiet = now.next(Some(&reading(Some(90.0), true, 4.0, 0)), 2000);
        assert_eq!(quiet.bpm, None);
        assert_eq!(quiet.playing, Some(false));
        assert_eq!(
            quiet.changes(&now),
            [("tempo.bpm", None), ("tempo.playing", Some(json!(false)))]
        );
    }
}
