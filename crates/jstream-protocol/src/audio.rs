//! Audio in the collection (ADR-0028, docs/adr/0028-audio-model-v2.md): each audio
//! source's settings, the buses, and the send matrix that routes sources into buses,
//! plus the meter readings the engine pushes while the mixer runs.
//!
//! Sources, buses and sends are collection state, changed by undoable edits
//! (`source.set_audio`, `bus.*`, `send.*`). Meters and loudness are runtime state: they
//! travel in the `meters.tick` event, never in a query of the collection.

use crate::finite::Finite32;
use crate::id::{BusId, SourceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The loudest fader setting, in dB. The mixer's gains top out here too.
pub const MAX_GAIN_DB: f32 = 24.0;

/// The quietest fader setting, in dB: about −100 dB is far below any converter's noise
/// floor. Silence is `muted`, not a gain.
pub const MIN_GAIN_DB: f32 = -100.0;

/// The most buses a collection holds, the master included: what the engine's mixer
/// holds. One more `bus.create` is `invalid_argument`.
pub const MAX_BUSES: usize = 16;

/// Why `gain_db` is not a fader setting the engine accepts, or `None` when it is.
pub fn gain_db_problem(gain_db: Finite32) -> Option<&'static str> {
    let db = gain_db.get();
    if db > MAX_GAIN_DB {
        Some("must be at most +24 dB")
    } else if db < MIN_GAIN_DB {
        Some("must be at least -100 dB; mute for silence")
    } else {
        None
    }
}

/// When a source's audio is heard (ADR-0028, D18). Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AudioFollow {
    /// Heard whether or not the source is on program: microphones and other audio
    /// inputs, desktop and application audio.
    Always,
    /// Heard only while the source is shown on the Program mix, fading with the
    /// transition that shows or hides it: browser, media, webcam audio and nested
    /// scenes.
    #[default]
    WhenShown,
}

/// A source's audio settings, as `source.list` and `state.get` report them. Only an
/// audio source has them: a source whose kind produces audio, or a `scene` source (a
/// nested scene, whose audio is its contents').
///
/// Signal order (ADR-0028): the source's stream, then its fader and mute, then the
/// activity gain `follow` sets, then each send's gain into its bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceAudio {
    /// Whether the engine opens the source's audio at all. Off, the source has no mixer
    /// channel and no meter, and its sends are kept but carry nothing, so turning it
    /// back on restores them. Not the same as `muted`, which keeps the stream open and
    /// metered. For a `scene` source, off stops what the scene shows from being heard
    /// through it. New sources start on.
    pub enabled: bool,
    /// When it is heard. A new source takes its kind's default: `always` for a kind
    /// with audio and no video (an audio input), `when_shown` for every other audio
    /// source, `scene` sources included.
    pub follow: AudioFollow,
    /// The fader, in dB, applied before the sends: from −100 to +24. New sources start
    /// at 0. Always 0 for a `scene` source, which has no fader of its own.
    pub gain_db: Finite32,
    /// Whether the source is muted: still open and metered, but feeding no bus. Always
    /// false for a `scene` source, which has no mute of its own.
    pub muted: bool,
}

impl SourceAudio {
    /// A new source's audio: on, at 0 dB, unmuted, following `follow`.
    pub const fn new(follow: AudioFollow) -> Self {
        Self {
            enabled: true,
            follow,
            gain_db: Finite32::ZERO,
            muted: false,
        }
    }
}

/// One bus, as `bus.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BusInfo {
    /// The bus's ID. The master bus's is `master`.
    pub bus_id: BusId,
    /// Its display name.
    pub name: String,
    /// The bus fader, in dB, applied after the sum: from −100 to +24.
    pub gain_db: Finite32,
    /// Whether the bus is muted.
    pub muted: bool,
}

/// One send: a source's audio routed into a bus, post-fader, as `bus.list` and
/// `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendInfo {
    /// The source sending.
    pub source_id: SourceId,
    /// The bus it sends into.
    pub bus_id: BusId,
    /// The send's gain, in dB, applied after the source's fader: from −100 to +24.
    pub gain_db: Finite32,
}

/// One source's levels in a `meters.tick`, post-fader and before its sends. Levels are
/// linear full scale per channel, left then right: 1.0 is 0 dBFS, 0.0 is silence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceMeter {
    /// The source.
    pub source_id: SourceId,
    /// The largest absolute sample since the previous tick.
    pub peak: [Finite32; 2],
    /// The RMS level over the most recent mixer block.
    pub rms: [Finite32; 2],
}

/// One bus's levels in a `meters.tick`, after its fader. Levels are linear full scale
/// per channel, left then right.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BusMeter {
    /// The bus.
    pub bus_id: BusId,
    /// The largest absolute sample since the previous tick.
    pub peak: [Finite32; 2],
    /// The RMS level over the most recent mixer block.
    pub rms: [Finite32; 2],
    /// The true peak since the previous tick (ITU-R BS.1770, 4× oversampled). Measured
    /// on the master bus only for now; absent on the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub true_peak: Option<[Finite32; 2]>,
    /// The bus's loudness. Measured on the master bus only for now; absent on the
    /// others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<Loudness>,
}

/// BS.1770 loudness, in LUFS. Each value is absent until enough audio has been measured
/// for it, and while the signal is below the absolute gate (−70 LUFS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Loudness {
    /// Momentary loudness, over the last 400 ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub momentary: Option<Finite32>,
    /// Short-term loudness, over the last 3 s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_term: Option<Finite32>,
    /// Gated integrated loudness since the mixer started or the last
    /// `audio.reset_loudness`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrated: Option<Finite32>,
}

#[cfg(test)]
mod tests {
    use super::{AudioFollow, MAX_BUSES, SourceAudio, gain_db_problem};
    use crate::Finite32;
    use serde_json::{Value, json};

    #[test]
    fn source_audio_wire_form() {
        let audio = SourceAudio::new(AudioFollow::Always);
        assert_eq!(
            serde_json::to_value(audio).ok(),
            Some(json!({"enabled": true, "follow": "always", "gain_db": 0.0, "muted": false}))
        );
    }

    #[test]
    fn gains_outside_the_fader_range_are_rejected() {
        let db = |v| Finite32::new(v).expect("finite");
        assert_eq!(gain_db_problem(db(24.0)), None);
        assert_eq!(gain_db_problem(db(-100.0)), None);
        assert!(gain_db_problem(db(24.5)).is_some());
        assert!(gain_db_problem(db(-100.5)).is_some());
    }

    /// `bus.create`'s description, which clients and agents read, states `MAX_BUSES`.
    #[test]
    fn bus_create_states_the_bus_limit() {
        fn descriptions(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    if let Some(Value::String(d)) = map.get("description") {
                        out.push(d.replace('\n', " "));
                    }
                    map.values().for_each(|v| descriptions(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| descriptions(v, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        descriptions(&crate::json_schema(), &mut all);
        let bus_create: Vec<_> = all
            .iter()
            .filter(|d| d.starts_with("Adds a bus,"))
            .collect();
        assert!(!bus_create.is_empty());
        for d in bus_create {
            assert!(d.contains(&format!("at most {MAX_BUSES} buses")), "{d}");
        }
    }
}
