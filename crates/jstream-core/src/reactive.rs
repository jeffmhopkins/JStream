//! Audio-reactive motion modifiers as the engine draws them
//! ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md)).
//!
//! An `audio` modifier hears what a `spectrum` source with the same `audio` and
//! `audio_source` would draw ([`Hearing`]). The engine keeps a scope for every bus and
//! source one hears (`crate::audio::retain_scopes`) and finds each scope after every
//! sync ([`HeardScopes`], handed to plans with the filter assets). Planning records each
//! drawn item's audio modifiers with their scopes in the shot ([`Bindings`]). Once per
//! tick, before any mix is drawn, the render thread's [`Follower`] measures each heard
//! scope once, advances each binding's envelope by the time since the last tick, and
//! stores its level in the shot, where every mix drawing that tick reads it: program,
//! preview, a scene source and a reflection agree. An envelope is kept by item and
//! modifier ID, so it carries across a new shot, and across a keyframe Move whose two
//! sides share the ID.
//!
//! Docs: docs/adr/0100-audio-reactive-motion-modifiers.md

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use jstream_anim::{Audio, Envelope, Measure};
use jstream_media::MediaTime;
use jstream_protocol::{BusId, ItemId, MotionKind, MotionModifier, SourceId};
use jstream_sources::{AudioMeter, AudioScope, AudioScopes};

/// What an audio modifier hears.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Hearing {
    /// The program mix: the master after its filters, as the outputs hear it.
    Program,
    /// A bus after its fader.
    Bus(BusId),
    /// A source after its own filters, fader, mute and follow.
    Source(SourceId),
}

/// What `modifier` hears, when it is an audio one. A source overrides a bus, as for the
/// `spectrum` kind.
pub(crate) fn hearing(modifier: &MotionModifier) -> Option<Hearing> {
    match &modifier.kind {
        MotionKind::Audio {
            audio,
            audio_source,
            ..
        } => Some(match (audio_source, audio) {
            (Some(source), _) => Hearing::Source(source.clone()),
            (None, Some(bus)) => Hearing::Bus(bus.clone()),
            (None, None) => Hearing::Program,
        }),
        _ => None,
    }
}

/// The scope each audio modifier of the collection hears, found after the scopes were
/// kept for the collection.
pub(crate) type HeardScopes = Arc<BTreeMap<Hearing, Arc<AudioScope>>>;

/// The scope of every [`Hearing`] in `heard`, from `scopes`.
pub(crate) fn heard_scopes(
    scopes: &AudioScopes,
    heard: impl IntoIterator<Item = Hearing>,
) -> BTreeMap<Hearing, Arc<AudioScope>> {
    heard
        .into_iter()
        .map(|hearing| {
            let scope = match &hearing {
                Hearing::Program => Arc::clone(scopes.program()),
                Hearing::Bus(bus) => scopes.bus(bus),
                Hearing::Source(source) => scopes.source(source),
            };
            (hearing, scope)
        })
        .collect()
}

/// One audio modifier of a drawn item, with the scope it hears and its level for the
/// current tick.
#[derive(Debug)]
pub(crate) struct Binding {
    audio: Audio,
    scope: Arc<AudioScope>,
    /// The envelope's level, `0..=1`, as `f32` bits: the render thread's [`Follower`]
    /// stores it once a tick, and every mix of the tick reads it.
    level: AtomicU32,
}

impl PartialEq for Binding {
    /// Two bindings are the same when they hear the same scope with the same
    /// parameters: a level is the tick's, never the plan's.
    fn eq(&self, other: &Self) -> bool {
        self.audio == other.audio && Arc::ptr_eq(&self.scope, &other.scope)
    }
}

/// A shot's audio modifiers, by item and modifier ID.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Bindings {
    /// Each item's bindings, by modifier ID, in its list's order.
    by_item: BTreeMap<ItemId, Vec<(String, Binding)>>,
}

impl Bindings {
    /// Records `item`'s audio modifiers in `motion` that hear a scope in `heard`. A
    /// modifier whose scope isn't found (the collection changed since the scopes were
    /// found) is left out, so it reads as silence until the next plan.
    ///
    /// One shot can draw an item through two motion lists with the same modifier ID: a
    /// keyframe Move's two sides, or a keyframe in one mix and its parent in another.
    /// They share one envelope (ADR-0100 decision 4 keys it by item and ID), and the
    /// first list recorded decides what it hears and measures and its dB map and times;
    /// each side's own curve and range still map it.
    pub(crate) fn record(&mut self, item: &ItemId, motion: &[MotionModifier], heard: &HeardScopes) {
        for modifier in motion {
            let Some(hearing) = hearing(modifier) else {
                continue;
            };
            let Some(scope) = heard.get(&hearing) else {
                continue;
            };
            let Some(jstream_anim::Modifier {
                kind: jstream_anim::Kind::Audio(audio),
                ..
            }) = jstream_graph::anim_modifiers(std::slice::from_ref(modifier)).pop()
            else {
                continue;
            };
            let listed = self.by_item.entry(item.clone()).or_default();
            if listed.iter().all(|(id, _)| *id != modifier.modifier_id) {
                listed.push((
                    modifier.modifier_id.clone(),
                    Binding {
                        audio,
                        scope: Arc::clone(scope),
                        level: AtomicU32::new(0),
                    },
                ));
            }
        }
    }

    /// Whether there are none.
    pub(crate) fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    /// The level of `item`'s audio modifier `modifier` for the current tick, if the shot
    /// has it. Allocates nothing.
    pub(crate) fn level(&self, item: &ItemId, modifier: &str) -> Option<f32> {
        self.by_item
            .get(item)?
            .iter()
            .find(|(id, _)| id == modifier)
            .map(|(_, binding)| f32::from_bits(binding.level.load(Ordering::Relaxed)))
    }

    /// Every binding, with its item and modifier ID.
    fn all(&self) -> impl Iterator<Item = (&ItemId, &str, &Binding)> {
        self.by_item.iter().flat_map(|(item, listed)| {
            listed
                .iter()
                .map(move |(id, binding)| (item, id.as_str(), binding))
        })
    }

    /// Whether it has `item`'s modifier `modifier`.
    fn has(&self, item: &ItemId, modifier: &str) -> bool {
        self.by_item
            .get(item)
            .is_some_and(|listed| listed.iter().any(|(id, _)| id == modifier))
    }
}

/// A meter of the [`Follower`]'s, for one heard scope.
#[derive(Debug)]
struct Meter {
    /// The scope's address. The follower never holds the scope itself: the shot's
    /// bindings do, and the shot is reclaimed off the render thread, so no scope's last
    /// reference is ever dropped there (R-RT-003). While a binding names it, no other
    /// scope can have its address.
    scope: usize,
    /// Whether a `band` binding hears it, so that its spectrum is worth measuring
    /// (ADR-0100 decision 4).
    spectrum: bool,
    meter: Box<AudioMeter>,
}

/// `scope`'s address, which names its meter.
fn address(scope: &Arc<AudioScope>) -> usize {
    Arc::as_ptr(scope) as usize
}

/// The render thread's half: a meter per heard scope and an envelope per item and
/// modifier, advanced once a tick ([`Follower::follow`]).
#[derive(Debug, Default)]
pub(crate) struct Follower {
    /// Each heard scope's meter.
    meters: Vec<Meter>,
    /// Each binding's envelope, by item and modifier ID.
    envelopes: BTreeMap<ItemId, Vec<(String, Envelope)>>,
    /// The tick it last followed, so a second call for the same tick changes nothing.
    last: Option<MediaTime>,
}

impl Follower {
    /// Measures each scope `bindings` hears at `now`, advances each binding's envelope
    /// by the time since the last tick, and stores its level for every mix of the tick.
    ///
    /// A meter is made the first time a shot names its scope, and an envelope the first
    /// time a shot names its binding (so on a new plan, never while a steady shot draws);
    /// both are dropped once no binding names them. Allocates nothing otherwise.
    pub(crate) fn follow(&mut self, bindings: &Bindings, now: MediaTime) {
        if bindings.is_empty() {
            if !self.envelopes.is_empty() || !self.meters.is_empty() {
                self.envelopes.clear();
                self.meters.clear();
            }
            self.last = Some(now);
            return;
        }
        let dt_s = match self.last {
            Some(last) if now <= last => return,
            Some(last) => (now.as_secs_f64() - last.as_secs_f64()) as f32,
            // The first tick has nothing to follow from: it starts at silence.
            None => 0.0,
        };
        self.last = Some(now);
        let ms = (now.as_secs_f64() * 1000.0).max(0.0) as u64;
        self.keep(bindings);
        // Each scope once, through the first binding that names it.
        for meter in &mut self.meters {
            let Some(binding) = bindings
                .all()
                .map(|(_, _, binding)| binding)
                .find(|binding| address(&binding.scope) == meter.scope)
            else {
                continue;
            };
            if meter.spectrum {
                meter.meter.measure(&binding.scope, ms);
            } else {
                meter.meter.measure_levels(&binding.scope, ms);
            }
        }
        for (item, id, binding) in bindings.all() {
            let Some(Meter { meter, .. }) = self
                .meters
                .iter()
                .find(|meter| meter.scope == address(&binding.scope))
            else {
                continue;
            };
            let raw = meter.raw();
            let measured = match binding.audio.measure {
                Measure::Level => raw.level,
                Measure::Peak => raw.peak,
                Measure::Onset => raw.onset,
                Measure::Band { low_hz, high_hz } => meter.band(low_hz, high_hz),
            };
            let Some((_, envelope)) = self
                .envelopes
                .get_mut(item)
                .and_then(|listed| listed.iter_mut().find(|(at, _)| at == id))
            else {
                continue;
            };
            let level = envelope.advance(&binding.audio, measured, dt_s);
            binding.level.store(level.to_bits(), Ordering::Relaxed);
        }
    }

    /// Makes a meter for each scope and an envelope for each binding `bindings` names
    /// that it has none for, drops the ones it no longer names, and marks each meter
    /// whose scope a `band` binding hears.
    fn keep(&mut self, bindings: &Bindings) {
        let named = |scope: usize| {
            bindings
                .all()
                .any(|(_, _, binding)| address(&binding.scope) == scope)
        };
        if self.meters.iter().any(|meter| !named(meter.scope)) {
            self.meters.retain(|meter| named(meter.scope));
        }
        for (_, _, binding) in bindings.all() {
            let scope = address(&binding.scope);
            if !self.meters.iter().any(|meter| meter.scope == scope) {
                self.meters.push(Meter {
                    scope,
                    spectrum: false,
                    meter: Box::new(AudioMeter::new()),
                });
            }
        }
        for meter in &mut self.meters {
            meter.spectrum = bindings.all().any(|(_, _, binding)| {
                matches!(binding.audio.measure, Measure::Band { .. })
                    && address(&binding.scope) == meter.scope
            });
        }
        let stale = self
            .envelopes
            .iter()
            .any(|(item, listed)| listed.iter().any(|(id, _)| !bindings.has(item, id)));
        if stale {
            for (item, listed) in &mut self.envelopes {
                listed.retain(|(id, _)| bindings.has(item, id));
            }
            self.envelopes.retain(|_, listed| !listed.is_empty());
        }
        for (item, id, _) in bindings.all() {
            let listed = match self.envelopes.get_mut(item) {
                Some(listed) => listed,
                None => self.envelopes.entry(item.clone()).or_default(),
            };
            if listed.iter().all(|(at, _)| at != id) {
                listed.push((id.to_owned(), Envelope::default()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Finite32, MotionClock, MotionCurve, MotionMeasure, MotionProperty};

    fn at_ms(ms: i64) -> MediaTime {
        MediaTime::new(ms * 1000, jstream_media::Timebase::MICROS)
    }

    fn light(id: &str, audio: Option<&str>, measure: MotionMeasure) -> MotionModifier {
        let band = measure == MotionMeasure::Band;
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::Opacity,
            kind: MotionKind::Audio {
                audio: audio.map(BusId::new),
                audio_source: None,
                measure,
                low_hz: band.then(|| Finite32::new(800.0).expect("finite")),
                high_hz: band.then(|| Finite32::new(1200.0).expect("finite")),
                floor_db: Finite32::new(-60.0).expect("finite"),
                ceiling_db: Finite32::ZERO,
                attack_ms: 0,
                release_ms: 100,
                curve: MotionCurve::Linear,
                from: Finite32::new(-1.0).expect("finite"),
                to: Finite32::ZERO,
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    /// A full-scale 1 kHz sine, stereo, one scope window long.
    fn tone(scope: &AudioScope) {
        let samples: Vec<f32> = (0..2048)
            .flat_map(|n| {
                let s = (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0).sin();
                [s, s]
            })
            .collect();
        scope.push(48_000, &samples);
    }

    #[test]
    fn hearing_is_the_source_else_the_bus_else_the_program() {
        assert_eq!(
            hearing(&light("a", None, MotionMeasure::Level)),
            Some(Hearing::Program)
        );
        assert_eq!(
            hearing(&light("a", Some("music"), MotionMeasure::Level)),
            Some(Hearing::Bus(BusId::new("music")))
        );
        let mut heard = light("a", Some("music"), MotionMeasure::Level);
        if let MotionKind::Audio { audio_source, .. } = &mut heard.kind {
            *audio_source = Some(SourceId::new("src_mic"));
        }
        assert_eq!(
            hearing(&heard),
            Some(Hearing::Source(SourceId::new("src_mic")))
        );
        let still = MotionModifier {
            kind: MotionKind::Drift {
                rate_per_s: Finite32::ONE,
                wrap: None,
            },
            ..light("b", None, MotionMeasure::Level)
        };
        assert_eq!(hearing(&still), None);
    }

    #[test]
    fn a_binding_follows_its_scope_once_a_tick() {
        let scopes = AudioScopes::new();
        let heard: HeardScopes = Arc::new(heard_scopes(
            &scopes,
            [Hearing::Program, Hearing::Bus(BusId::new("music"))],
        ));
        let item = ItemId::new("itm_light");
        let mut bindings = Bindings::default();
        bindings.record(
            &item,
            &[
                light("level", None, MotionMeasure::Level),
                light("band", None, MotionMeasure::Band),
                light("quiet", Some("music"), MotionMeasure::Peak),
                light("unheard", Some("elsewhere"), MotionMeasure::Peak),
            ],
            &heard,
        );
        assert!(bindings.level(&item, "unheard").is_none(), "no scope found");
        let mut follower = Follower::default();
        follower.follow(&bindings, at_ms(0));
        assert_eq!(
            bindings.level(&item, "level"),
            Some(0.0),
            "the first tick is silent"
        );

        tone(scopes.program());
        follower.follow(&bindings, at_ms(16));
        // An RMS of 0.707 is -3 dB: 0.95 of the way from -60 to 0, with a 0 ms attack.
        let level = bindings.level(&item, "level").expect("level");
        assert!((level - 0.95).abs() < 0.01, "{level}");
        let band = bindings.level(&item, "band").expect("band");
        assert!(band > 0.99, "a full-scale sine in its band: {band}");
        assert_eq!(
            bindings.level(&item, "quiet"),
            Some(0.0),
            "the bus is silent"
        );
        assert!(bindings.level(&ItemId::new("other"), "level").is_none());

        // The same tick again changes nothing; a later one releases once the ring is
        // still.
        follower.follow(&bindings, at_ms(16));
        assert_eq!(bindings.level(&item, "level"), Some(level));
        follower.follow(&bindings, at_ms(16 + 400));
        let released = bindings.level(&item, "level").expect("level");
        assert!(released < 0.05, "{released}");
    }

    /// ADR-0100 decision 4: the FFT runs only for a scope a `band` binding hears.
    #[test]
    fn a_scope_only_levels_hear_is_measured_without_its_spectrum() {
        let scopes = AudioScopes::new();
        let heard: HeardScopes = Arc::new(heard_scopes(&scopes, [Hearing::Program]));
        let item = ItemId::new("itm_light");
        let plan = |measure| {
            let mut bindings = Bindings::default();
            bindings.record(&item, &[light("pulse", None, measure)], &heard);
            bindings
        };
        let levels = plan(MotionMeasure::Level);
        let mut follower = Follower::default();
        follower.follow(&levels, at_ms(0));
        tone(scopes.program());
        follower.follow(&levels, at_ms(16));
        assert!(levels.level(&item, "pulse").expect("level") > 0.9);
        let [meter] = follower.meters.as_slice() else {
            panic!("one meter: {:?}", follower.meters);
        };
        assert!(!meter.spectrum);
        assert_eq!(meter.meter.band(800.0, 1200.0), 0.0, "no FFT ran");

        // A band binding on the same scope turns its spectrum on.
        let band = plan(MotionMeasure::Band);
        tone(scopes.program());
        follower.follow(&band, at_ms(32));
        assert!(follower.meters[0].spectrum);
        assert!(follower.meters[0].meter.band(800.0, 1200.0) > 0.99);
    }

    /// One item drawn through two lists that share a modifier ID (a keyframe and its
    /// parent) follows the first list's input, in one envelope.
    #[test]
    fn the_first_list_recorded_decides_what_a_shared_id_hears() {
        let scopes = AudioScopes::new();
        let heard: HeardScopes = Arc::new(heard_scopes(
            &scopes,
            [Hearing::Program, Hearing::Bus(BusId::new("music"))],
        ));
        let item = ItemId::new("itm_light");
        let mut bindings = Bindings::default();
        bindings.record(&item, &[light("pulse", None, MotionMeasure::Level)], &heard);
        bindings.record(
            &item,
            &[light("pulse", Some("music"), MotionMeasure::Peak)],
            &heard,
        );
        let mut follower = Follower::default();
        follower.follow(&bindings, at_ms(0));
        tone(scopes.program());
        follower.follow(&bindings, at_ms(16));
        assert!(
            bindings.level(&item, "pulse").expect("level") > 0.9,
            "the program's"
        );
        assert_eq!(follower.meters.len(), 1);
    }

    #[test]
    fn an_envelope_carries_into_a_new_shot_with_the_same_ids() {
        let scopes = AudioScopes::new();
        let heard: HeardScopes = Arc::new(heard_scopes(&scopes, [Hearing::Program]));
        let item = ItemId::new("itm_light");
        let plan = || {
            let mut bindings = Bindings::default();
            bindings.record(&item, &[light("level", None, MotionMeasure::Level)], &heard);
            bindings
        };
        let first = plan();
        let mut follower = Follower::default();
        follower.follow(&first, at_ms(0));
        tone(scopes.program());
        follower.follow(&first, at_ms(16));
        let lit = first.level(&item, "level").expect("level");
        // A new plan of the same item: its new binding starts at 0 in the shot, but the
        // follower's envelope goes on from where it was, falling slowly from the tone.
        let second = plan();
        assert_eq!(first, second, "a level is never part of the plan");
        follower.follow(&second, at_ms(32));
        let next = second.level(&item, "level").expect("level");
        assert!(next > 0.8 && next <= lit + 1e-6, "{next} after {lit}");
        // A shot without it lets its envelope and meter go.
        follower.follow(&Bindings::default(), at_ms(48));
        assert!(follower.envelopes.is_empty() && follower.meters.is_empty());
    }
}
