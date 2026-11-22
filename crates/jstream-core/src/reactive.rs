//! Audio-reactive motion modifiers as the engine draws them
//! ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md)), and MIDI-driven
//! ones ([ADR-0168](../../../docs/adr/0168-midi-controllers-drive-motion.md)).
//!
//! An `audio` modifier hears what a `spectrum` source with the same `audio` and
//! `audio_source` would draw ([`Hearing`]). The engine keeps a scope for every bus and
//! source one hears (`crate::audio::retain_scopes`) and finds each scope after every
//! sync ([`HeardScopes`], handed to plans with the filter assets). Planning records each
//! drawn item's audio modifiers with their scopes in the shot ([`Bindings`]). Once per
//! tick, before any mix is drawn, the render thread's [`Follower`] measures each heard
//! scope once, advances each binding's envelope by the time since the last tick, and
//! stores its level in the shot, where every mix drawing that tick reads it: program,
//! preview, a scene source and a reflection agree. A scene frame sheet keeps a
//! [`Follower`] of its own on the `jstream-frames` thread and does the same for its
//! pictures. An envelope is kept by item and
//! modifier ID, so it carries across a new shot, and across a keyframe Move whose two
//! sides share the ID.
//!
//! # Behaviour
//!
//! **Audio-reactive motion** (`reactive.rs`,
//! ADR-0100): an `audio`
//! modifier hears the program mix, a bus or a source (`Hearing`), as a `spectrum` source
//! with the same settings would. The live sources find what items' and keyframe
//! overrides' motion hears on each sync (`audio::heard_by_motion`, so those buses and
//! sources are fed as a scope source's are) and keep its scopes in `FilterAssets`
//! (`HeardScopes`). Planning a shot records each audio modifier it draws with its scope
//! (`Bindings`, part of the shot). Once a tick, before the mixes are drawn, the render
//! loop's `Follower` measures each scope once with its own `AudioMeter` (made when a
//! scope is first heard, never while it is steady), running the FFT only for a scope a
//! `band` binding hears (`measure_levels` otherwise), and advances each modifier's
//! envelope by the tick's media time, per item and modifier ID, so every pass and mix of
//! the tick reads the same level. The follower names a meter by its scope's address and
//! measures through the shot's binding, never holding a scope itself, so no scope's last
//! reference drops on the render thread (R-RT-003). An item drawn through two lists that
//! share a modifier ID (a keyframe and its parent) shares one envelope, and the first
//! list planned decides what it hears and measures. Each binding holds that level as an atomic, which the
//! draw paths read by item and ID without allocating. A modifier with nothing heard, a
//! preview shot (no follower) and the program mix while no output runs read silence,
//! the modifier's `from`. A scene frame sheet follows those scopes itself, across its
//! pictures, so an audio modifier there hears the mix (#1189).
//!
//! **MIDI-driven motion** (`reactive.rs`, ADR-0168): a `midi` modifier is recorded the
//! same way, with its input's `MidiState` (the named input's, or every input's) and the
//! control it reads (`Control`), when the engine has MIDI inputs. Once a tick the same
//! `Follower` reads each control from atomics, maps it onto 0 to 1, follows it with a
//! `Smoothing` per item and modifier ID (its first step lands on the value, so an item
//! that appears shows the controller where it is) and stores the level. A binding no
//! follower has stored (a preview shot's) reads its control as it is, unsmoothed, so
//! previews show the controller too. A modifier ID that turns from audio to MIDI between
//! plans gets a fresh state of its new kind.
//!
//! Docs: docs/adr/0100-audio-reactive-motion-modifiers.md, docs/adr/0168-midi-controllers-drive-motion.md

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use jstream_anim::{Audio, Envelope, Measure, Midi, Smoothing};
use jstream_media::MediaTime;
use jstream_protocol::{BusId, ItemId, MotionKind, MotionMidiControl, MotionModifier, SourceId};
use jstream_sources::{
    AudioMeter, AudioScope, AudioScopes, DataCell, DataReading, DataWatch, LiveData, MidiInputs,
    MidiState,
};

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

/// What a MIDI modifier reads of its input's state (ADR-0168): one control, on one
/// channel (0 to 15) or the latest of any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// A control change's value.
    Cc { channel: Option<u8>, controller: u8 },
    /// The pitch bend.
    Bend { channel: Option<u8> },
    /// The channel pressure.
    Pressure { channel: Option<u8> },
}

impl Control {
    /// What `modifier` reads, when it is a MIDI one this version knows.
    pub(crate) fn of(modifier: &MotionModifier) -> Option<Self> {
        let MotionKind::Midi {
            control,
            controller,
            channel,
            ..
        } = &modifier.kind
        else {
            return None;
        };
        // The wire's channel is 1 to 16; the state's is 0 to 15.
        let channel = channel.map(|c| c.saturating_sub(1) & 0x0F);
        Some(match control {
            MotionMidiControl::Cc => Control::Cc {
                channel,
                controller: (*controller)? & 0x7F,
            },
            MotionMidiControl::PitchBend => Control::Bend { channel },
            MotionMidiControl::Pressure => Control::Pressure { channel },
            _ => return None,
        })
    }

    /// Its value in `state` now, as `0..=1`. Reads atomics only.
    pub(crate) fn read(self, state: &MidiState) -> f32 {
        match self {
            Control::Cc {
                channel: Some(c),
                controller,
            } => Midi::normalize_cc(state.controller(c, controller)),
            Control::Cc {
                channel: None,
                controller,
            } => Midi::normalize_cc(state.controller_any(controller)),
            Control::Bend { channel: Some(c) } => Midi::normalize_bend(state.bend(c)),
            Control::Bend { channel: None } => Midi::normalize_bend(state.bend_any()),
            Control::Pressure { channel: Some(c) } => Midi::normalize_cc(state.pressure(c)),
            Control::Pressure { channel: None } => Midi::normalize_cc(state.pressure_any()),
        }
    }
}

/// How a `data` modifier reads its field's reading as `0..=1` (ADR-0171 decision 3).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DataMap {
    /// A number at `low` reads 0 and at `high` 1, clamped between.
    low: f64,
    high: f64,
    /// Each text's value, by the text's hash, as a reading holds it.
    states: Vec<(u64, f32)>,
}

impl DataMap {
    /// What `modifier` reads, and how, when it is a data one.
    pub(crate) fn of(modifier: &MotionModifier) -> Option<(DataWatch, Self)> {
        let MotionKind::Data {
            input,
            key,
            field,
            range,
            states,
            ..
        } = &modifier.kind
        else {
            return None;
        };
        let watch = DataWatch {
            input: input.clone(),
            key: key.clone(),
            field: field.clone(),
            color: false,
        };
        let map = DataMap {
            low: f64::from(range[0].get()),
            high: f64::from(range[1].get()),
            states: states
                .iter()
                .map(|(text, value)| (jstream_sources::text_hash(text), value.get()))
                .collect(),
        };
        Some((watch, map))
    }

    /// `reading` as `0..=1`: a number through the range, a text through the table (0
    /// when it isn't listed), and anything else 0. Allocates nothing.
    pub(crate) fn read(&self, reading: DataReading) -> f32 {
        let v = match reading {
            DataReading::Number(n) => {
                let span = self.high - self.low;
                if span == 0.0 {
                    return 0.0;
                }
                ((n - self.low) / span).clamp(0.0, 1.0) as f32
            }
            DataReading::Text(hash) => self
                .states
                .iter()
                .find(|(h, _)| *h == hash)
                .map_or(0.0, |(_, v)| *v),
            DataReading::Absent | DataReading::Color(_) => 0.0,
        };
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// What a binding follows.
#[derive(Debug)]
enum Input {
    /// An audio modifier's scope (ADR-0100).
    Scope {
        audio: Audio,
        scope: Arc<AudioScope>,
    },
    /// A MIDI modifier's input state and control (ADR-0168).
    Midi {
        midi: Midi,
        state: Arc<MidiState>,
        control: Control,
    },
    /// A data modifier's watched field and how it reads (ADR-0171 decision 3).
    Data {
        midi: Midi,
        cell: Arc<DataCell>,
        map: DataMap,
    },
}

impl Input {
    /// What a MIDI or data input reads now, as `0..=1`: atomics only.
    fn read(&self) -> Option<f32> {
        match self {
            Input::Scope { .. } => None,
            Input::Midi { state, control, .. } => Some(control.read(state)),
            Input::Data { cell, map, .. } => Some(map.read(cell.read())),
        }
    }

    /// The smoothing parameters of a MIDI or data input.
    fn smoothed(&self) -> Option<&Midi> {
        match self {
            Input::Scope { .. } => None,
            Input::Midi { midi, .. } | Input::Data { midi, .. } => Some(midi),
        }
    }
}

impl PartialEq for Input {
    /// Two inputs are the same when they follow the same scope or state with the same
    /// parameters.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Input::Scope { audio, scope }, Input::Scope { audio: a, scope: s }) => {
                audio == a && Arc::ptr_eq(scope, s)
            }
            (
                Input::Midi {
                    midi,
                    state,
                    control,
                },
                Input::Midi {
                    midi: m,
                    state: s,
                    control: c,
                },
            ) => midi == m && control == c && Arc::ptr_eq(state, s),
            (
                Input::Data { midi, cell, map },
                Input::Data {
                    midi: m,
                    cell: c,
                    map: p,
                },
            ) => midi == m && map == p && Arc::ptr_eq(cell, c),
            _ => false,
        }
    }
}

/// One audio or MIDI modifier of a drawn item, with what it follows and its level for
/// the current tick.
#[derive(Debug)]
pub(crate) struct Binding {
    input: Input,
    /// The envelope's level, `0..=1`, as `f32` bits: the render thread's [`Follower`],
    /// or a scene frame sheet's own, stores it once a tick, and every mix of the tick
    /// reads it.
    level: AtomicU32,
    /// Whether a follower has stored `level`. A MIDI binding no follower follows (a
    /// preview shot's) reads its control as it is now, unsmoothed, so a preview shows
    /// the controller too.
    followed: AtomicBool,
}

impl PartialEq for Binding {
    /// Two bindings are the same when they follow the same input with the same
    /// parameters: a level is the tick's, never the plan's.
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
    }
}

impl Binding {
    fn new(input: Input) -> Self {
        Binding {
            input,
            level: AtomicU32::new(0),
            followed: AtomicBool::new(false),
        }
    }

    /// The scope it hears, when it is an audio binding.
    fn scope(&self) -> Option<&Arc<AudioScope>> {
        match &self.input {
            Input::Scope { scope, .. } => Some(scope),
            Input::Midi { .. } | Input::Data { .. } => None,
        }
    }

    /// Its level this tick: what a follower stored, or a MIDI control as it is now
    /// when none has.
    fn level(&self) -> f32 {
        if !self.followed.load(Ordering::Relaxed)
            && let Some(now) = self.input.read()
        {
            return now;
        }
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn store(&self, level: f32) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
        self.followed.store(true, Ordering::Relaxed);
    }
}

/// A shot's audio modifiers, by item and modifier ID.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Bindings {
    /// Each item's bindings, by modifier ID, in its list's order.
    by_item: BTreeMap<ItemId, Vec<(String, Binding)>>,
}

impl Bindings {
    /// Records `item`'s audio modifiers in `motion` that hear a scope in `heard`, and
    /// its MIDI modifiers (ADR-0168), each on its input's state in `midi`. A modifier
    /// whose scope isn't found (the collection changed since the scopes were found) is
    /// left out, so it reads as silence until the next plan, and so is a MIDI one when
    /// the engine has no MIDI inputs, which rests at its `from`. Its data modifiers
    /// (ADR-0171 decision 3) each watch their field in `data`, the live data's lock
    /// taken briefly as a leaf; one is left out when there is no live data.
    ///
    /// One shot can draw an item through two motion lists with the same modifier ID: a
    /// keyframe Move's two sides, or a keyframe in one mix and its parent in another.
    /// They share one envelope (ADR-0100 decision 4 keys it by item and ID), and the
    /// first list recorded decides what it hears and measures and its dB map and times;
    /// each side's own curve and range still map it. A MIDI modifier's control and
    /// smoothing are decided the same way.
    pub(crate) fn record(
        &mut self,
        item: &ItemId,
        motion: &[MotionModifier],
        heard: &HeardScopes,
        midi: Option<&MidiInputs>,
        data: Option<&LiveData>,
    ) {
        for modifier in motion {
            let Some(kind) = jstream_graph::anim_modifiers(std::slice::from_ref(modifier))
                .pop()
                // One that drives an effect setting (ADR-0138) follows as one that moves
                // the box does.
                .or_else(|| {
                    jstream_graph::effect_anim_modifiers(std::slice::from_ref(modifier))
                        .pop()
                        .map(|(_, modifier)| modifier)
                })
                // So does one that drives a source setting (ADR-0171 decision 4).
                .or_else(|| {
                    jstream_graph::source_anim_modifiers(std::slice::from_ref(modifier))
                        .pop()
                        .map(|(_, modifier)| modifier)
                })
                .map(|modifier| modifier.kind)
            else {
                continue;
            };
            let input = match kind {
                jstream_anim::Kind::Audio(audio) => {
                    let Some(scope) = hearing(modifier).and_then(|h| heard.get(&h)) else {
                        continue;
                    };
                    Input::Scope {
                        audio,
                        scope: Arc::clone(scope),
                    }
                }
                // A data modifier follows as a MIDI one, on its watched field.
                jstream_anim::Kind::Midi(params)
                    if matches!(modifier.kind, MotionKind::Data { .. }) =>
                {
                    let (Some((watch, map)), Some(data)) = (DataMap::of(modifier), data) else {
                        continue;
                    };
                    Input::Data {
                        midi: params,
                        cell: data.watch(&watch),
                        map,
                    }
                }
                jstream_anim::Kind::Midi(params) => {
                    let (Some(control), Some(inputs)) = (Control::of(modifier), midi) else {
                        continue;
                    };
                    let state = match &modifier.kind {
                        MotionKind::Midi {
                            input: Some(name), ..
                        } => match inputs.input(name) {
                            Some(state) => state,
                            // Past the inputs the engine tracks by name: nothing comes.
                            None => Arc::new(MidiState::new()),
                        },
                        _ => inputs.any(),
                    };
                    Input::Midi {
                        midi: params,
                        state,
                        control,
                    }
                }
                _ => continue,
            };
            let listed = self.by_item.entry(item.clone()).or_default();
            if listed.iter().all(|(id, _)| *id != modifier.modifier_id) {
                listed.push((modifier.modifier_id.clone(), Binding::new(input)));
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
            .map(|(_, binding)| binding.level())
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
    /// scope can have its address. A meter goes only on a tick that follows, so across
    /// ticks that don't (no mix exists, or none is due) a scope reclaimed and another
    /// made at its address would inherit its meter: that costs one tick's reading (the
    /// meter's ring position and onset average), never memory safety, since the
    /// address is only compared.
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

/// What a [`Follower`] keeps for one binding between ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Follow {
    /// An audio binding's envelope (ADR-0100).
    Envelope(Envelope),
    /// A MIDI binding's smoothing (ADR-0168).
    Smoothing(Smoothing),
}

impl Follow {
    /// A fresh state for `binding`.
    fn new(binding: &Binding) -> Self {
        match binding.input {
            Input::Scope { .. } => Follow::Envelope(Envelope::default()),
            Input::Midi { .. } | Input::Data { .. } => Follow::Smoothing(Smoothing::default()),
        }
    }

    /// Whether it is the state `binding` keeps.
    fn fits(&self, binding: &Binding) -> bool {
        matches!(
            (self, &binding.input),
            (Follow::Envelope(_), Input::Scope { .. })
                | (
                    Follow::Smoothing(_),
                    Input::Midi { .. } | Input::Data { .. }
                )
        )
    }
}

/// The render thread's half, and a scene frame sheet's own: a meter per heard scope
/// and an envelope (or a MIDI smoothing) per item and modifier, advanced once a tick
/// ([`Follower::follow`]).
#[derive(Debug, Default)]
pub(crate) struct Follower {
    /// Each heard scope's meter.
    meters: Vec<Meter>,
    /// Each binding's envelope or smoothing, by item and modifier ID.
    envelopes: BTreeMap<ItemId, Vec<(String, Follow)>>,
    /// The tick it last followed, so a second call for the same tick changes nothing.
    last: Option<MediaTime>,
}

impl Follower {
    /// Measures each scope `bindings` hears at `now`, reads each MIDI control, advances
    /// each binding's envelope or smoothing by the time since the last tick, and stores
    /// its level for every mix of the tick.
    ///
    /// A meter is made the first time a shot names its scope, and an envelope the first
    /// time a shot names its binding (so on a new plan, never while a steady shot draws);
    /// both are dropped once no binding names them. Allocates nothing otherwise. A MIDI
    /// smoothing's first step lands on the control's value (ADR-0168).
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
            let Some(scope) = bindings
                .all()
                .filter_map(|(_, _, binding)| binding.scope())
                .find(|scope| address(scope) == meter.scope)
            else {
                continue;
            };
            if meter.spectrum {
                meter.meter.measure(scope, ms);
            } else {
                meter.meter.measure_levels(scope, ms);
            }
        }
        for (item, id, binding) in bindings.all() {
            let Some((_, follow)) = self
                .envelopes
                .get_mut(item)
                .and_then(|listed| listed.iter_mut().find(|(at, _)| at == id))
            else {
                continue;
            };
            let level = match (&binding.input, follow) {
                (Input::Scope { audio, scope }, Follow::Envelope(envelope)) => {
                    let Some(Meter { meter, .. }) = self
                        .meters
                        .iter()
                        .find(|meter| meter.scope == address(scope))
                    else {
                        continue;
                    };
                    let raw = meter.raw();
                    let measured = match audio.measure {
                        Measure::Level => raw.level,
                        Measure::Peak => raw.peak,
                        Measure::Onset => raw.onset,
                        Measure::Band { low_hz, high_hz } => meter.band(low_hz, high_hz),
                    };
                    envelope.advance(audio, measured, dt_s)
                }
                (
                    input @ (Input::Midi { .. } | Input::Data { .. }),
                    Follow::Smoothing(smoothing),
                ) => {
                    let (Some(midi), Some(now)) = (input.smoothed(), input.read()) else {
                        continue;
                    };
                    smoothing.advance(midi, now, dt_s)
                }
                // `keep` gave every binding the state of its kind.
                _ => continue,
            };
            binding.store(level);
        }
    }

    /// Makes a meter for each scope and an envelope or smoothing for each binding
    /// `bindings` names that it has none for (or one of the other kind), drops the ones
    /// it no longer names, and marks each meter whose scope a `band` binding hears.
    fn keep(&mut self, bindings: &Bindings) {
        let named = |scope: usize| {
            bindings
                .all()
                .filter_map(|(_, _, binding)| binding.scope())
                .any(|heard| address(heard) == scope)
        };
        if self.meters.iter().any(|meter| !named(meter.scope)) {
            self.meters.retain(|meter| named(meter.scope));
        }
        for scope in bindings.all().filter_map(|(_, _, binding)| binding.scope()) {
            let scope = address(scope);
            if !self.meters.iter().any(|meter| meter.scope == scope) {
                self.meters.push(Meter {
                    scope,
                    spectrum: false,
                    meter: Box::new(AudioMeter::new()),
                });
            }
        }
        for meter in &mut self.meters {
            meter.spectrum = bindings.all().any(|(_, _, binding)| match &binding.input {
                Input::Scope { audio, scope } => {
                    matches!(audio.measure, Measure::Band { .. }) && address(scope) == meter.scope
                }
                Input::Midi { .. } | Input::Data { .. } => false,
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
        for (item, id, binding) in bindings.all() {
            let listed = match self.envelopes.get_mut(item) {
                Some(listed) => listed,
                None => self.envelopes.entry(item.clone()).or_default(),
            };
            match listed.iter_mut().find(|(at, _)| at == id) {
                Some((_, follow)) if !follow.fits(binding) => *follow = Follow::new(binding),
                Some(_) => {}
                None => listed.push((id.to_owned(), Follow::new(binding))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Finite32, MotionClock, MotionCurve, MotionMeasure, MotionProperty};
    use jstream_sources::MidiInputs;

    fn at_ms(ms: i64) -> MediaTime {
        MediaTime::new(ms * 1000, jstream_media::Timebase::MICROS)
    }

    fn light(id: &str, audio: Option<&str>, measure: MotionMeasure) -> MotionModifier {
        let band = measure == MotionMeasure::Band;
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::Opacity,
            effect: None,
            source: None,
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
            None,
            None,
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
            bindings.record(&item, &[light("pulse", None, measure)], &heard, None, None);
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
        bindings.record(
            &item,
            &[light("pulse", None, MotionMeasure::Level)],
            &heard,
            None,
            None,
        );
        bindings.record(
            &item,
            &[light("pulse", Some("music"), MotionMeasure::Peak)],
            &heard,
            None,
            None,
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
            bindings.record(
                &item,
                &[light("level", None, MotionMeasure::Level)],
                &heard,
                None,
                None,
            );
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

    fn fader(
        id: &str,
        smoothing_ms: u32,
        channel: Option<u8>,
        input: Option<&str>,
    ) -> MotionModifier {
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::Tilt,
            effect: None,
            source: None,
            kind: MotionKind::Midi {
                control: MotionMidiControl::Cc,
                controller: Some(7),
                channel,
                input: input.map(str::to_owned),
                smoothing_ms,
                curve: MotionCurve::Linear,
                from: Finite32::new(-25.0).expect("finite"),
                to: Finite32::new(25.0).expect("finite"),
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    fn cc(channel: u8, controller: u8, value: u8) -> jstream_midi::Message {
        jstream_midi::Message::parse(&[0xB0 | channel, controller, value]).expect("a CC")
    }

    /// ADR-0168: a MIDI binding reads its control on its input and channel, lands on it
    /// on the first tick, smooths after, and a 0 ms one jumps.
    #[test]
    fn a_midi_binding_follows_its_control_smoothed() {
        let midi = MidiInputs::new();
        let item = ItemId::new("itm_lever");
        let heard: HeardScopes = Arc::default();
        let mut bindings = Bindings::default();
        bindings.record(
            &item,
            &[
                fader("smooth", 100, None, None),
                fader("instant", 0, Some(2), None),
                fader("other", 0, None, Some("Pads")),
            ],
            &heard,
            Some(&midi),
            None,
        );
        assert_eq!(
            bindings.level(&item, "smooth"),
            Some(0.0),
            "nothing sent yet"
        );
        midi.push("Faders", 0, &cc(0, 7, 127));
        // A shot no follower follows (a preview) reads the control as it is.
        assert_eq!(bindings.level(&item, "smooth"), Some(1.0));
        let mut follower = Follower::default();
        follower.follow(&bindings, at_ms(0));
        assert_eq!(
            bindings.level(&item, "smooth"),
            Some(1.0),
            "the first tick lands"
        );
        assert_eq!(
            bindings.level(&item, "instant"),
            Some(0.0),
            "channel 2 is silent"
        );
        assert_eq!(bindings.level(&item, "other"), Some(0.0), "another input");

        midi.push("Faders", 1, &cc(0, 7, 0));
        midi.push("Faders", 1, &cc(1, 7, 127));
        follower.follow(&bindings, at_ms(100));
        let smooth = bindings.level(&item, "smooth").expect("level");
        // One time constant: about 63% of the way from 1 to the latest on any channel,
        // which is channel 2's 127, so it stays at 1.
        assert_eq!(smooth, 1.0, "the latest on any channel is 127");
        assert_eq!(bindings.level(&item, "instant"), Some(1.0), "0 ms jumps");
        midi.push("Faders", 2, &cc(1, 7, 0));
        follower.follow(&bindings, at_ms(200));
        let fell = bindings.level(&item, "smooth").expect("level");
        assert!((fell - (-1f32).exp()).abs() < 1e-3, "{fell}");
        assert_eq!(bindings.level(&item, "instant"), Some(0.0));
        midi.push("Pads", 3, &cc(4, 7, 127));
        follower.follow(&bindings, at_ms(216));
        assert_eq!(bindings.level(&item, "other"), Some(1.0), "its own input");

        // A plan without MIDI inputs records none: it rests at `from`.
        let mut none = Bindings::default();
        none.record(&item, &[fader("smooth", 0, None, None)], &heard, None, None);
        assert!(none.is_empty());
    }

    /// A modifier ID that turns from audio to MIDI between plans gets a fresh state of
    /// its new kind.
    #[test]
    fn a_binding_that_changes_kind_starts_afresh() {
        let midi = MidiInputs::new();
        let scopes = AudioScopes::new();
        let heard: HeardScopes = Arc::new(heard_scopes(&scopes, [Hearing::Program]));
        let item = ItemId::new("itm_light");
        let mut audio = Bindings::default();
        audio.record(
            &item,
            &[light("m", None, MotionMeasure::Level)],
            &heard,
            Some(&midi),
            None,
        );
        let mut follower = Follower::default();
        follower.follow(&audio, at_ms(0));
        let mut fader_plan = Bindings::default();
        fader_plan.record(
            &item,
            &[fader("m", 1000, None, None)],
            &heard,
            Some(&midi),
            None,
        );
        midi.push("Faders", 0, &cc(0, 7, 127));
        follower.follow(&fader_plan, at_ms(16));
        assert_eq!(
            fader_plan.level(&item, "m"),
            Some(1.0),
            "a fresh smoothing lands on its first step"
        );
        assert!(follower.meters.is_empty(), "the scope's meter went");
    }

    fn daw(
        id: &str,
        input: Option<&str>,
        (key, field): (&str, &str),
        range: [f32; 2],
        states: &[(&str, f32)],
        smoothing_ms: u32,
    ) -> MotionModifier {
        let f = |v: f32| Finite32::new(v).expect("finite");
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::Opacity,
            effect: None,
            source: None,
            kind: MotionKind::Data {
                input: input.map(str::to_owned),
                key: key.into(),
                field: field.into(),
                range: [f(range[0]), f(range[1])],
                states: states
                    .iter()
                    .map(|(text, v)| ((*text).to_owned(), f(*v)))
                    .collect(),
                smoothing_ms,
                curve: MotionCurve::Linear,
                from: f(-1.0),
                to: Finite32::ZERO,
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    /// ADR-0171 decision 3: a data binding reads its field as 0 to 1 (a boolean, a
    /// number through its range, a text through its table), reads 0 while it is absent,
    /// and is smoothed as a MIDI one is.
    #[test]
    fn a_data_binding_reads_its_field_through_its_range_or_states() {
        let data = LiveData::new();
        let item = ItemId::new("itm_lamp");
        let heard: HeardScopes = Arc::default();
        let mut bindings = Bindings::default();
        bindings.record(
            &item,
            &[
                daw("play", None, ("transport", "playing"), [0.0, 1.0], &[], 0),
                daw("meter", Some("DAW"), ("meters", "0"), [-60.0, 0.0], &[], 0),
                daw(
                    "clip",
                    Some("DAW"),
                    ("track/1", "slots.0.state"),
                    [0.0, 1.0],
                    &[("recording", 1.0), ("playing", 0.5)],
                    0,
                ),
                daw(
                    "glide",
                    None,
                    ("transport", "playing"),
                    [0.0, 1.0],
                    &[],
                    100,
                ),
                daw(
                    "other",
                    Some("Elsewhere"),
                    ("transport", "playing"),
                    [0.0, 1.0],
                    &[],
                    0,
                ),
            ],
            &heard,
            None,
            Some(&data),
        );
        for id in ["play", "meter", "clip", "glide", "other"] {
            assert_eq!(bindings.level(&item, id), Some(0.0), "{id} rests at 0");
        }
        let put = |key: &str, json: &str, ms: u64| {
            data.put("DAW", key, json.as_bytes(), ms).expect("taken");
        };
        put("transport", r#"{"playing": true}"#, 0);
        put("meters", "[-15, -60]", 0);
        put("track/1", r#"{"slots": [{"state": "playing"}]}"#, 0);
        // A preview reads it as it is.
        assert_eq!(bindings.level(&item, "play"), Some(1.0));
        let mut follower = Follower::default();
        follower.follow(&bindings, at_ms(0));
        assert_eq!(bindings.level(&item, "play"), Some(1.0));
        assert_eq!(
            bindings.level(&item, "meter"),
            Some(0.75),
            "-15 dB of -60..0"
        );
        assert_eq!(
            bindings.level(&item, "clip"),
            Some(0.5),
            "its table's playing"
        );
        assert_eq!(
            bindings.level(&item, "glide"),
            Some(1.0),
            "the first tick lands"
        );
        assert_eq!(bindings.level(&item, "other"), Some(0.0), "another sender");

        put("transport", r#"{"playing": false}"#, 50);
        put("track/1", r#"{"slots": [{"state": "stopped"}]}"#, 50);
        put("meters", "[20]", 50);
        follower.follow(&bindings, at_ms(100));
        assert_eq!(bindings.level(&item, "play"), Some(0.0), "0 ms jumps");
        assert_eq!(
            bindings.level(&item, "clip"),
            Some(0.0),
            "a text not listed"
        );
        assert_eq!(
            bindings.level(&item, "meter"),
            Some(1.0),
            "clamped at the top"
        );
        let glide = bindings.level(&item, "glide").expect("level");
        assert!((glide - (-1f32).exp()).abs() < 1e-3, "{glide}");

        // A plan without live data records none: it rests at `from`.
        let mut none = Bindings::default();
        none.record(
            &item,
            &[daw("play", None, ("t", ""), [0.0, 1.0], &[], 0)],
            &heard,
            None,
            None,
        );
        assert!(none.is_empty());
    }
}
