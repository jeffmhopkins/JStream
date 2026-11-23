//! Channels as the engine follows them
//! ([ADR-0183](../../../docs/adr/0183-virtual-control-channels.md)).
//!
//! The collection holds each channel's definition; the [`ChannelDriver`] holds what is
//! live: each input's smoothing and whether it has moved, the value actions set and its
//! ramp, the switch, the averaging window and the fade. The engine's `jstream-values`
//! thread ticks it about 30 times a second, under the engine's lock, while the collection
//! holds a channel, before it ticks the value bindings, so a binding reads this tick's
//! value. Each tick stores every channel's value in its cell in the engine's
//! [`ChannelLevels`], where the render thread, value bindings and other channels load it
//! without a lock.
//!
//! # Behaviour
//!
//! **Channels** (`channels.rs`, ADR-0183): a channel starts at its `default` and its
//! `on`. An input takes part once it first reads something other than it read when the
//! engine started following it, as a value binding does. Each tick the moved inputs and
//! the action value are combined (`latest` takes whichever changed last), replaced by
//! `off_value` while switched off, averaged over `average_ms`, ramped toward at no more
//! than `rise_ms` and `fall_ms` allow, and bent by the curve. A channel input reads the
//! other channel's value of the same tick: channels tick in the order their inputs
//! need. A channel that belongs to a scene starts again from its `default` and `on`
//! each time that scene goes on program, at once: that reset doesn't fade. Changing only a channel's processing keeps
//! what it follows and its value; deleting it makes its cell read as no channel.
//!
//! **Lanes** (ADR-0184): a timeline's lane on a channel is one more input of it, from
//! the time the timeline starts (or is sought) until something else changes the
//! channel: under `latest` a control moved, an action or a later timeline's lane takes
//! it over. A lane with no point at 0 starts from the channel's value at that moment
//! (before its curve), so nothing jumps; a seek keeps the value the timeline's lane
//! started from (unless a control or action took the channel since), so scrubbing
//! doesn't jump either; each pass of a loop after the first starts
//! from the lane's last point. When the timeline ends or stops, the lane holds its last
//! value. The channel's own switch, averaging, fade and curve apply after the lane.
//!
//! Docs: docs/adr/0183-virtual-control-channels.md

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use jstream_anim::{Midi, Smoothing};
use jstream_graph::Graph;
use jstream_protocol::{
    ChannelCombine, ChannelDriver as Driver, ChannelInfo, ChannelSwitch, ChannelValueInfo,
    Finite32, MotionClock, MotionCurve, MotionKind, MotionModifier, MotionProperty, SceneId,
    TimelineId, TimelineInfo, TimelineLane,
};
use jstream_sources::{ChannelCell, ChannelLevels, DataCell, LiveData, MidiInputs, MidiState};

use crate::reactive::{Control, DataMap};

/// What one input reads.
#[derive(Debug)]
enum Reader {
    Midi {
        state: Arc<MidiState>,
        control: Control,
    },
    Data {
        cell: Arc<DataCell>,
        map: DataMap,
    },
    Channel {
        cell: Arc<ChannelCell>,
    },
}

impl Reader {
    /// What it reads now, as `0..=1`: atomics only. A channel that doesn't exist reads 0.
    fn read(&self) -> f32 {
        match self {
            Reader::Midi { state, control } => control.read(state),
            Reader::Data { cell, map } => map.read(cell.read()),
            Reader::Channel { cell } => cell.read().unwrap_or(0.0),
        }
    }
}

/// `kind` as a modifier, so the reactive readers and `jstream-anim` take it.
fn as_modifier(id: &str, kind: &MotionKind) -> MotionModifier {
    MotionModifier {
        modifier_id: id.to_owned(),
        property: MotionProperty::X,
        effect: None,
        source: None,
        kind: kind.clone(),
        clock: MotionClock::default(),
        enabled: true,
        amount: None,
    }
}

/// The smoothing, curve and range `kind` maps a reading with, when it is a reader.
pub(crate) fn mapping(kind: &MotionKind) -> Option<Midi> {
    let modifier = as_modifier("m", kind);
    match jstream_graph::anim_modifiers(std::slice::from_ref(&modifier))
        .pop()?
        .kind
    {
        jstream_anim::Kind::Midi(params) => Some(params),
        _ => None,
    }
}

/// What `kind` reads, or `None` when it reads MIDI and the engine has no MIDI inputs, or
/// is something no channel reads. The flag says it names a MIDI input the engine doesn't
/// track yet, so it is looked up again each tick.
pub(crate) fn reader(
    kind: &MotionKind,
    midi: Option<&MidiInputs>,
    data: &LiveData,
) -> Option<(ReaderOf, bool)> {
    let modifier = as_modifier("m", kind);
    let mut unresolved = false;
    let reader = match kind {
        MotionKind::Midi { input, .. } => {
            let control = Control::of(&modifier)?;
            let inputs = midi?;
            let state = match input {
                Some(name) => inputs.input(name).unwrap_or_else(|| {
                    unresolved = true;
                    Arc::new(MidiState::new())
                }),
                None => inputs.any(),
            };
            Reader::Midi { state, control }
        }
        MotionKind::Data { .. } => {
            let (watch, map) = DataMap::of(&modifier)?;
            Reader::Data {
                cell: data.watch(&watch),
                map,
            }
        }
        MotionKind::Channel { channel_id, .. } => Reader::Channel {
            cell: data.channels().cell(channel_id),
        },
        _ => return None,
    };
    Some((ReaderOf(reader), unresolved))
}

/// A reader of a `midi`, `data` or `channel` kind, for value bindings as well as
/// channels.
#[derive(Debug)]
pub(crate) struct ReaderOf(Reader);

impl ReaderOf {
    /// What it reads now, as `0..=1`: atomics only.
    pub(crate) fn read(&self) -> f32 {
        self.0.read()
    }
}

/// One input as a channel follows it.
#[derive(Debug)]
struct Input {
    kind: MotionKind,
    reader: ReaderOf,
    unresolved: bool,
    midi: Midi,
    smoothing: Smoothing,
    /// What it read when the channel started following it.
    rest: f32,
    /// What it read last tick, to tell when it changed.
    last: f32,
    /// Whether it has read something other than `rest`: only then does it take part.
    moved: bool,
    /// When it last changed, in the driver's order of changes.
    changed: u64,
    /// Its mapped value this tick, `0..=1`.
    value: f32,
}

/// The value actions set, and its ramp.
#[derive(Debug, Clone, Copy)]
struct Action {
    from: f32,
    to: f32,
    start: Instant,
    fade_ms: u32,
    /// When it was set, in the driver's order of changes.
    changed: u64,
}

impl Action {
    fn at(&self, now: Instant) -> f32 {
        if self.fade_ms == 0 {
            return self.to;
        }
        let t =
            now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.fade_ms as f32;
        self.from + (self.to - self.from) * t.clamp(0.0, 1.0)
    }
}

/// A timeline's lane driving a channel (ADR-0184).
#[derive(Debug, Clone)]
struct Lane {
    timeline_id: TimelineId,
    lane: TimelineLane,
    /// Where it started from: the channel's value when the timeline started.
    from: f32,
    /// Its value now.
    value: f32,
    /// When it took the channel, in the driver's order of changes.
    changed: u64,
}

/// One channel as the driver follows it.
#[derive(Debug)]
struct Followed {
    info: ChannelInfo,
    cell: Arc<ChannelCell>,
    inputs: Vec<Input>,
    action: Option<Action>,
    lane: Option<Lane>,
    on: bool,
    /// The last values combined, with when, over `average_ms`.
    window: VecDeque<(Instant, f32)>,
    /// The faded value, before the curve; `None` before the first tick, which lands.
    faded: Option<f32>,
    /// The curve, as a mapping onto 0 to 1.
    curve: Midi,
    /// Its value this tick, after the curve.
    value: f32,
    driver: Option<Driver>,
}

impl Followed {
    fn new(info: &ChannelInfo, cell: Arc<ChannelCell>) -> Self {
        let followed = Followed {
            info: info.clone(),
            cell,
            inputs: Vec::new(),
            action: None,
            lane: None,
            on: info.on,
            window: VecDeque::new(),
            faded: None,
            curve: curve_of(info.curve),
            value: info.default.get(),
            driver: None,
        };
        followed.cell.store(followed.value);
        followed
    }

    /// Starts again from its `default` and `on`, its inputs at rest where they are.
    fn reset(&mut self) {
        self.action = None;
        self.lane = None;
        self.on = self.info.on;
        self.window.clear();
        self.faded = None;
        self.driver = None;
        for input in &mut self.inputs {
            let now = input.reader.read();
            input.rest = now;
            input.last = now;
            input.moved = false;
            input.smoothing = Smoothing::default();
        }
    }
}

/// `curve` as a mapping of 0 to 1 onto itself.
fn curve_of(curve: MotionCurve) -> Midi {
    mapping(&MotionKind::Channel {
        channel_id: "c".into(),
        smoothing_ms: 0,
        curve,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    })
    .unwrap_or(Midi {
        smoothing_ms: 0.0,
        curve: jstream_anim::Curve::Linear,
        from: 0.0,
        to: 1.0,
    })
}

/// Why a channel action failed: the collection holds no such channel.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NoChannel;

/// The engine's channels between ticks.
#[derive(Debug, Default)]
pub(crate) struct ChannelDriver {
    followed: BTreeMap<String, Followed>,
    /// The order channels tick in: each after the channels its inputs read.
    order: Vec<String>,
    last: Option<Instant>,
    /// Counts changes of inputs and actions, so `latest` takes the newest.
    changes: u64,
}

impl ChannelDriver {
    /// Whether it follows any channel.
    pub(crate) fn is_empty(&self) -> bool {
        self.followed.is_empty()
    }

    /// Follows `graph`'s channels for the time since the last tick, storing each one's
    /// value in its cell in `data`'s channel levels.
    pub(crate) fn tick(
        &mut self,
        graph: &Graph,
        midi: Option<&MidiInputs>,
        data: &LiveData,
        now: Instant,
    ) {
        let dt_ms = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32() * 1000.0
        });
        self.last = Some(now);
        self.keep(graph, midi, data);
        for id in &self.order {
            let Some(followed) = self.followed.get_mut(id) else {
                continue;
            };
            step(followed, &mut self.changes, midi, dt_ms, now);
        }
    }

    /// Follows each of `graph`'s channels it doesn't yet, takes a changed definition,
    /// and lets go of the ones `graph` no longer holds.
    pub(crate) fn keep(&mut self, graph: &Graph, midi: Option<&MidiInputs>, data: &LiveData) {
        let levels: &ChannelLevels = data.channels();
        let mut changed = false;
        self.followed.retain(|id, followed| {
            let kept = graph.channel(id).is_some();
            if !kept {
                followed.cell.clear();
                changed = true;
            }
            kept
        });
        for info in graph.channels() {
            let followed = match self.followed.get_mut(&info.channel_id) {
                Some(followed) => followed,
                None => {
                    changed = true;
                    self.followed
                        .entry(info.channel_id.clone())
                        .or_insert_with(|| Followed::new(info, levels.cell(&info.channel_id)))
                }
            };
            if followed.info != *info {
                changed |= followed.info.inputs != info.inputs;
                if followed.info.on != info.on {
                    followed.on = info.on;
                }
                followed.curve = curve_of(info.curve);
                followed.info = info.clone();
            }
            keep_inputs(followed, midi, data);
        }
        if changed || self.order.len() != self.followed.len() {
            self.order = order(&self.followed);
        }
    }

    /// Sets channel `channel_id`'s action value to `value`, ramping over `fade_ms`.
    pub(crate) fn set_value(
        &mut self,
        channel_id: &str,
        value: f32,
        fade_ms: Option<u32>,
        now: Instant,
    ) -> Result<(), NoChannel> {
        let followed = self.followed.get_mut(channel_id).ok_or(NoChannel)?;
        self.changes += 1;
        let from = followed
            .action
            .map_or(followed.faded.unwrap_or(followed.value), |a| a.at(now));
        followed.action = Some(Action {
            from,
            to: value.clamp(0.0, 1.0),
            start: now,
            fade_ms: fade_ms.unwrap_or(0),
            changed: self.changes,
        });
        Ok(())
    }

    /// Adds `by` to channel `channel_id`'s action value, from its value now if no action
    /// set it yet.
    pub(crate) fn nudge(
        &mut self,
        channel_id: &str,
        by: f32,
        fade_ms: Option<u32>,
        now: Instant,
    ) -> Result<(), NoChannel> {
        let followed = self.followed.get(channel_id).ok_or(NoChannel)?;
        let base = followed
            .action
            .map_or(followed.faded.unwrap_or(followed.value), |a| a.to);
        self.set_value(channel_id, (base + by).clamp(0.0, 1.0), fade_ms, now)
    }

    /// Timeline `timeline_id` started (or, with `seek`, was sought) at `at_ms`: each of
    /// its lanes that is on takes its channel, replacing another timeline's lane there
    /// (ADR-0184: the last started wins). A lane starts from the channel's value now,
    /// except that a seek keeps the value this timeline's lane already started from.
    pub(crate) fn lanes_start(
        &mut self,
        timeline_id: &TimelineId,
        info: &TimelineInfo,
        at_ms: u32,
        seek: bool,
    ) {
        for lane in info.lanes.iter().filter(|lane| lane.on) {
            let Some(followed) = self.followed.get_mut(&lane.target.channel) else {
                continue;
            };
            self.changes += 1;
            // A seek keeps where this timeline's lane started from, so scrubbing a
            // playing sequence never jumps (ADR-0184 decision 3). A play, or a seek
            // that takes the channel from something else, starts from the value now.
            // A control or action that moved since the lane last did took the channel,
            // so the lane is no longer the one holding it.
            let newer = followed
                .inputs
                .iter()
                .filter(|input| input.moved)
                .map(|input| input.changed)
                .chain(followed.action.map(|action| action.changed))
                .max();
            let from = match &followed.lane {
                Some(held)
                    if seek
                        && held.timeline_id == *timeline_id
                        && newer.is_none_or(|changed| changed < held.changed) =>
                {
                    held.from
                }
                _ => followed.faded.unwrap_or(followed.value),
            };
            followed.lane = Some(Lane {
                timeline_id: timeline_id.clone(),
                lane: lane.clone(),
                from,
                value: lane.value_at(f64::from(at_ms), from),
                changed: self.changes,
            });
        }
    }

    /// Moves timeline `timeline_id`'s lanes to `at_ms` into pass `pass`. A pass after
    /// the first starts from where the lane ends.
    pub(crate) fn lanes_at(&mut self, timeline_id: &TimelineId, at_ms: f64, pass: u64) {
        for followed in self.followed.values_mut() {
            if let Some(lane) = &mut followed.lane
                && lane.timeline_id == *timeline_id
            {
                let from = match lane.lane.points.last() {
                    Some(last) if pass > 0 => last.value.get(),
                    _ => lane.from,
                };
                lane.value = lane.lane.value_at(at_ms, from);
            }
        }
    }

    /// Whether a lane drives any channel: the values thread then ticks faster.
    pub(crate) fn lanes_playing(&self, playing: impl Fn(&TimelineId) -> bool) -> bool {
        self.followed.values().any(|f| {
            f.lane
                .as_ref()
                .is_some_and(|lane| playing(&lane.timeline_id))
        })
    }

    /// The value and switch of channel `channel_id` now, if the driver follows it.
    pub(crate) fn level(&self, channel_id: &str) -> Option<(f32, bool)> {
        self.followed
            .get(channel_id)
            .map(|followed| (followed.value, followed.on))
    }

    /// Switches channel `channel_id` as `to` says.
    pub(crate) fn switch(&mut self, channel_id: &str, to: ChannelSwitch) -> Result<(), NoChannel> {
        let followed = self.followed.get_mut(channel_id).ok_or(NoChannel)?;
        followed.on = match to {
            ChannelSwitch::On => true,
            ChannelSwitch::Off => false,
            ChannelSwitch::Toggle => !followed.on,
        };
        Ok(())
    }

    /// Starts every channel that belongs to `scene` again from its `default` and `on`:
    /// that scene went on program.
    pub(crate) fn scene_live(&mut self, scene: &SceneId) {
        for followed in self.followed.values_mut() {
            if followed.info.scene_id.as_ref() == Some(scene) {
                followed.reset();
            }
        }
    }

    /// Every channel's value now, ordered by ID.
    pub(crate) fn values(&self) -> Vec<ChannelValueInfo> {
        self.followed
            .values()
            .map(|followed| ChannelValueInfo {
                channel_id: followed.info.channel_id.clone(),
                value: Finite32::new(followed.value).unwrap_or(Finite32::ZERO),
                on: followed.on,
                driver: followed.driver.clone(),
            })
            .collect()
    }
}

/// Brings `followed`'s inputs in line with its definition: an input whose kind changed
/// starts afresh, one the same keeps its smoothing and whether it moved.
fn keep_inputs(followed: &mut Followed, midi: Option<&MidiInputs>, data: &LiveData) {
    let kinds = &followed.info.inputs;
    let same = followed.inputs.len() == kinds.len()
        && followed
            .inputs
            .iter()
            .zip(kinds)
            .all(|(input, kind)| input.kind == *kind && !input.unresolved);
    if same {
        return;
    }
    let mut old: Vec<Option<Input>> = std::mem::take(&mut followed.inputs)
        .into_iter()
        .map(Some)
        .collect();
    for kind in kinds {
        let kept = old
            .iter_mut()
            .find(|i| i.as_ref().is_some_and(|i| i.kind == *kind && !i.unresolved))
            .and_then(Option::take);
        let input = kept.or_else(|| {
            let (reader, unresolved) = reader(kind, midi, data)?;
            let midi = mapping(kind)?;
            let rest = reader.read();
            Some(Input {
                kind: kind.clone(),
                reader,
                unresolved,
                midi,
                smoothing: Smoothing::default(),
                rest,
                last: rest,
                moved: false,
                changed: 0,
                value: 0.0,
            })
        });
        // An input this version or this engine can't read stays in its place, reading
        // nothing, so indexes keep matching the definition's.
        followed.inputs.push(input.unwrap_or_else(|| Input {
            kind: kind.clone(),
            reader: ReaderOf(Reader::Channel {
                cell: Arc::new(ChannelCell::default()),
            }),
            unresolved: false,
            midi: Midi {
                smoothing_ms: 0.0,
                curve: jstream_anim::Curve::Linear,
                from: 0.0,
                to: 0.0,
            },
            smoothing: Smoothing::default(),
            rest: 0.0,
            last: 0.0,
            moved: false,
            changed: 0,
            value: 0.0,
        }));
    }
}

/// The order to tick channels in: each after the channels it reads. `channel.set`
/// refuses a cycle, so every channel is placed; one read that the collection doesn't hold
/// is skipped.
fn order(followed: &BTreeMap<String, Followed>) -> Vec<String> {
    fn visit(
        id: &str,
        followed: &BTreeMap<String, Followed>,
        done: &mut BTreeSet<String>,
        out: &mut Vec<String>,
        depth: usize,
    ) {
        if done.contains(id) || depth > followed.len() {
            return;
        }
        let Some(channel) = followed.get(id) else {
            return;
        };
        for read in channel.info.reads() {
            visit(read, followed, done, out, depth + 1);
        }
        if done.insert(id.to_owned()) {
            out.push(id.to_owned());
        }
    }
    let mut done = BTreeSet::new();
    let mut out = Vec::with_capacity(followed.len());
    for id in followed.keys() {
        visit(id, followed, &mut done, &mut out, 0);
    }
    out
}

/// One tick of one channel: reads, combines, switches, averages, fades and bends it, and
/// stores the result.
fn step(
    followed: &mut Followed,
    changes: &mut u64,
    midi: Option<&MidiInputs>,
    dt_ms: f32,
    now: Instant,
) {
    let dt_s = dt_ms / 1000.0;
    for input in &mut followed.inputs {
        if input.unresolved
            && let (
                Some(inputs),
                MotionKind::Midi {
                    input: Some(name), ..
                },
            ) = (midi, &input.kind)
            && let Some(state) = inputs.input(name)
            && let Reader::Midi { state: held, .. } = &mut input.reader.0
        {
            *held = state;
            input.unresolved = false;
        }
        let raw = input.reader.read();
        if raw != input.last {
            input.last = raw;
            *changes += 1;
            input.changed = *changes;
        }
        if !input.moved {
            if raw == input.rest {
                continue;
            }
            input.moved = true;
        }
        let level = input.smoothing.advance(&input.midi, raw, dt_s);
        input.value = (input.midi.value(level) as f32).clamp(0.0, 1.0);
    }
    let (combined, driver) = combine(followed, now);
    followed.driver = driver;
    let target = if followed.on {
        combined
    } else {
        followed.info.off_value.get()
    };
    // Average over the window.
    let average_ms = followed.info.average_ms;
    let averaged = if average_ms == 0 {
        followed.window.clear();
        target
    } else {
        followed.window.push_back((now, target));
        while followed.window.front().is_some_and(|(at, _)| {
            now.saturating_duration_since(*at).as_secs_f32() * 1000.0 > average_ms as f32
        }) {
            followed.window.pop_front();
        }
        let sum: f32 = followed.window.iter().map(|(_, v)| v).sum();
        sum / followed.window.len().max(1) as f32
    };
    // Fade toward it at no more than its rise and fall allow.
    let faded = match followed.faded {
        None => averaged,
        Some(was) => {
            let delta = averaged - was;
            let most = |ms: u32| {
                if ms == 0 {
                    f32::INFINITY
                } else {
                    dt_ms / ms as f32
                }
            };
            if delta > 0.0 {
                was + delta.min(most(followed.info.rise_ms))
            } else {
                was - (-delta).min(most(followed.info.fall_ms))
            }
        }
    };
    followed.faded = Some(faded);
    followed.value = (followed.curve.value(faded) as f32).clamp(0.0, 1.0);
    followed.cell.store(followed.value);
}

/// The channel's inputs that moved and its action value, made one by its `combine`, with
/// what drives it; its `default` while nothing has.
fn combine(followed: &Followed, now: Instant) -> (f32, Option<Driver>) {
    let mut sources: Vec<(f32, u64, Driver)> = followed
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.moved)
        .map(|(n, input)| {
            (
                input.value,
                input.changed,
                Driver::Input {
                    index: u8::try_from(n).unwrap_or(u8::MAX),
                },
            )
        })
        .collect();
    if let Some(action) = followed.action {
        sources.push((action.at(now), action.changed, Driver::Action));
    }
    if let Some(lane) = &followed.lane {
        sources.push((
            lane.value,
            lane.changed,
            Driver::Sequence {
                timeline_id: lane.timeline_id.clone(),
            },
        ));
    }
    let newest = || sources.iter().max_by_key(|(_, changed, _)| *changed);
    let Some(latest) = newest() else {
        return (followed.info.default.get(), None);
    };
    let by_value = |pick: fn(f32, f32) -> bool| {
        sources
            .iter()
            .fold(None::<&(f32, u64, Driver)>, |best, s| match best {
                Some(b) if !pick(s.0, b.0) => Some(b),
                _ => Some(s),
            })
    };
    let values = sources.iter().map(|(v, _, _)| *v);
    let n = sources.len() as f32;
    let (value, driver) = match followed.info.combine {
        ChannelCombine::Max => {
            let best = by_value(|a, b| a > b).unwrap_or(latest);
            (best.0, best.2.clone())
        }
        ChannelCombine::Min => {
            let best = by_value(|a, b| a < b).unwrap_or(latest);
            (best.0, best.2.clone())
        }
        ChannelCombine::Sum => (values.sum::<f32>(), latest.2.clone()),
        ChannelCombine::Product => (values.product::<f32>(), latest.2.clone()),
        ChannelCombine::Average => (values.sum::<f32>() / n, latest.2.clone()),
        _ => (latest.0, latest.2.clone()),
    };
    (value.clamp(0.0, 1.0), Some(driver))
}

#[cfg(test)]
mod tests {
    use super::ChannelDriver;
    use jstream_graph::Graph;
    use jstream_protocol::{
        ChannelCombine, ChannelInfo, ChannelSwitch, Edit, Finite32, MotionCurve, MotionKind,
        MotionMidiControl, SceneId,
    };
    use jstream_sources::{LiveData, MidiInputs};
    use std::time::{Duration, Instant};

    fn f(v: f32) -> Finite32 {
        Finite32::new(v).expect("finite")
    }

    fn cc(controller: u8) -> MotionKind {
        MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(controller),
            channel: None,
            input: None,
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }
    }

    fn message(bytes: &[u8]) -> jstream_midi::Message {
        jstream_midi::Message::parse(bytes).expect("a message")
    }

    fn set(graph: &mut Graph, channel: ChannelInfo) {
        graph
            .apply(&[Edit::ChannelSet { channel }], None)
            .expect("sets");
    }

    struct Rig {
        graph: Graph,
        midi: MidiInputs,
        data: LiveData,
        driver: ChannelDriver,
        t0: Instant,
    }

    impl Rig {
        fn new() -> Self {
            Rig {
                graph: Graph::new(),
                midi: MidiInputs::new(),
                data: LiveData::new(),
                driver: ChannelDriver::default(),
                t0: Instant::now(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn tick(&mut self, ms: u64) {
            let now = self.at(ms);
            self.driver
                .tick(&self.graph, Some(&self.midi), &self.data, now);
        }

        fn value(&self, id: &str) -> Option<f32> {
            self.data.channels().cell(id).read()
        }
    }

    #[test]
    fn a_channel_holds_its_default_until_an_input_moves_then_follows_it() {
        let mut rig = Rig::new();
        set(
            &mut rig.graph,
            ChannelInfo {
                default: f(0.5),
                inputs: vec![cc(7)],
                ..ChannelInfo::new("lights")
            },
        );
        rig.tick(0);
        assert_eq!(rig.value("lights"), Some(0.5), "at rest: its default");
        assert_eq!(rig.driver.values()[0].driver, None);
        rig.midi.push("Pad", 0, &message(&[0xB0, 7, 127]));
        rig.tick(33);
        assert_eq!(rig.value("lights"), Some(1.0));
        assert!(matches!(
            rig.driver.values()[0].driver,
            Some(jstream_protocol::ChannelDriver::Input { index: 0 })
        ));
        // An action set later wins under `latest`, until the control moves again.
        rig.driver
            .set_value("lights", 0.25, None, rig.at(40))
            .expect("a channel");
        rig.tick(66);
        assert_eq!(rig.value("lights"), Some(0.25));
        rig.midi.push("Pad", 70, &message(&[0xB0, 7, 0]));
        rig.tick(100);
        assert_eq!(rig.value("lights"), Some(0.0));
        // Deleted: its cell reads as no channel.
        rig.graph
            .apply(
                &[Edit::ChannelDelete {
                    channel_id: "lights".into(),
                }],
                None,
            )
            .expect("deletes");
        let cell = rig.data.channels().cell("lights");
        rig.tick(133);
        assert_eq!(cell.read(), None);
        assert!(rig.driver.is_empty());
    }

    #[test]
    fn a_switch_fades_to_its_off_value_and_back_at_its_rise_and_fall() {
        let mut rig = Rig::new();
        set(
            &mut rig.graph,
            ChannelInfo {
                default: f(1.0),
                rise_ms: 1000,
                fall_ms: 500,
                ..ChannelInfo::new("group")
            },
        );
        rig.tick(0);
        assert_eq!(rig.value("group"), Some(1.0), "the first tick lands");
        rig.driver
            .switch("group", ChannelSwitch::Toggle)
            .expect("a channel");
        rig.tick(250);
        let half = rig.value("group").expect("a value");
        assert!((half - 0.5).abs() < 1e-4, "falls 1 in 500 ms: {half}");
        rig.tick(600);
        assert_eq!(rig.value("group"), Some(0.0));
        rig.driver
            .switch("group", ChannelSwitch::On)
            .expect("a channel");
        rig.tick(850);
        let quarter = rig.value("group").expect("a value");
        assert!(
            (quarter - 0.25).abs() < 1e-4,
            "rises 1 in 1000 ms: {quarter}"
        );
        assert_eq!(
            rig.driver.switch("missing", ChannelSwitch::On),
            Err(super::NoChannel)
        );
    }

    #[test]
    fn an_action_ramps_over_its_fade_and_a_nudge_adds_to_it() {
        let mut rig = Rig::new();
        set(&mut rig.graph, ChannelInfo::new("dim"));
        rig.tick(0);
        rig.driver
            .set_value("dim", 1.0, Some(1000), rig.at(0))
            .expect("a channel");
        rig.tick(500);
        let mid = rig.value("dim").expect("a value");
        assert!((mid - 0.5).abs() < 1e-3, "{mid}");
        rig.tick(1000);
        assert_eq!(rig.value("dim"), Some(1.0));
        rig.driver
            .nudge("dim", -0.25, None, rig.at(1000))
            .expect("a channel");
        rig.tick(1033);
        assert_eq!(rig.value("dim"), Some(0.75));
    }

    #[test]
    fn combines_take_the_largest_product_or_mean_of_what_moved() {
        let mut rig = Rig::new();
        let both = |combine| ChannelInfo {
            inputs: vec![cc(1), cc(2)],
            combine,
            ..ChannelInfo::new("mix")
        };
        set(&mut rig.graph, both(ChannelCombine::Max));
        rig.tick(0);
        rig.midi.push("Pad", 0, &message(&[0xB0, 1, 127]));
        rig.midi.push("Pad", 1, &message(&[0xB0, 2, 0]));
        rig.tick(33);
        // CC 2 sent 0, its resting reading, so only CC 1 has moved.
        assert_eq!(rig.value("mix"), Some(1.0));
        rig.midi.push("Pad", 2, &message(&[0xB0, 2, 64]));
        rig.tick(66);
        assert_eq!(rig.value("mix"), Some(1.0), "the larger");
        set(&mut rig.graph, both(ChannelCombine::Product));
        rig.tick(100);
        let product = rig.value("mix").expect("a value");
        assert!((product - 64.0 / 127.0).abs() < 1e-4, "{product}");
        set(&mut rig.graph, both(ChannelCombine::Average));
        rig.tick(133);
        let mean = rig.value("mix").expect("a value");
        assert!((mean - (1.0 + 64.0 / 127.0) / 2.0).abs() < 1e-4, "{mean}");
    }

    #[test]
    fn a_channel_input_reads_another_channels_value_of_the_same_tick() {
        let mut rig = Rig::new();
        let follows = MotionKind::Channel {
            channel_id: "a-master".into(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: f(0.5),
        };
        // `b` sorts after `a-master` by ID, so name the follower so it sorts first.
        set(
            &mut rig.graph,
            ChannelInfo {
                inputs: vec![follows],
                ..ChannelInfo::new("0-follower")
            },
        );
        set(&mut rig.graph, ChannelInfo::new("a-master"));
        rig.tick(0);
        rig.driver
            .set_value("a-master", 1.0, None, rig.at(10))
            .expect("a channel");
        rig.tick(33);
        assert_eq!(rig.value("0-follower"), Some(0.5), "the same tick");
        // A cycle is refused.
        let back = MotionKind::Channel {
            channel_id: "0-follower".into(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        };
        assert!(
            rig.graph
                .apply(
                    &[Edit::ChannelSet {
                        channel: ChannelInfo {
                            inputs: vec![back],
                            ..ChannelInfo::new("a-master")
                        },
                    }],
                    None,
                )
                .is_err()
        );
    }

    #[test]
    fn a_held_note_gates_and_a_scene_channel_starts_again_when_its_scene_goes_live() {
        let mut rig = Rig::new();
        let note = MotionKind::Midi {
            control: MotionMidiControl::Note,
            controller: Some(36),
            channel: None,
            input: None,
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        };
        set(
            &mut rig.graph,
            ChannelInfo {
                scene_id: Some(SceneId::new("flight")),
                default: f(0.2),
                inputs: vec![note],
                ..ChannelInfo::new("pad")
            },
        );
        rig.tick(0);
        assert_eq!(rig.value("pad"), Some(0.2));
        rig.midi.push("Pad", 0, &message(&[0x99, 36, 100]));
        rig.tick(33);
        assert_eq!(rig.value("pad"), Some(1.0), "held");
        rig.midi.push("Pad", 40, &message(&[0x89, 36, 0]));
        rig.tick(66);
        assert_eq!(rig.value("pad"), Some(0.0), "released");
        rig.driver.scene_live(&SceneId::new("other"));
        rig.tick(100);
        assert_eq!(rig.value("pad"), Some(0.0), "another scene leaves it");
        rig.driver.scene_live(&SceneId::new("flight"));
        rig.tick(133);
        assert_eq!(rig.value("pad"), Some(0.2), "its own starts it again");
    }

    fn lane_timeline(id: &str, value: f32) -> jstream_protocol::TimelineInfo {
        jstream_protocol::TimelineInfo {
            timeline_id: jstream_protocol::TimelineId::new(id),
            name: String::new(),
            duration_ms: 1000,
            looping: jstream_protocol::TimelineLoop::Loop,
            cues: Vec::new(),
            labels: Vec::new(),
            lanes: vec![jstream_protocol::TimelineLane {
                target: jstream_protocol::LaneTarget {
                    channel: "c".into(),
                },
                points: vec![jstream_protocol::LanePoint {
                    at_ms: 1000,
                    value: f(value),
                    curve: jstream_protocol::LaneCurve::Linear,
                }],
                on: true,
            }],
            starts: Vec::new(),
            stops: Vec::new(),
        }
    }

    /// ADR-0184: a lane starts from the channel's value, the last timeline started wins
    /// it, a later pass starts from the lane's end, and an action takes it back.
    #[test]
    fn a_lane_starts_where_the_channel_is_and_the_last_started_wins() {
        let mut rig = Rig::new();
        set(&mut rig.graph, ChannelInfo::new("c"));
        rig.tick(0);
        rig.driver.set_value("c", 0.4, None, rig.at(0)).ok();
        rig.tick(10);
        let up = lane_timeline("up", 1.0);
        let down = lane_timeline("down", 0.0);
        rig.driver.lanes_start(&up.timeline_id, &up, 0, false);
        rig.driver.lanes_at(&up.timeline_id, 500.0, 0);
        rig.tick(20);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 0.7).abs() < 1e-5,
            "halfway from 0.4 to 1"
        );
        rig.driver.lanes_at(&up.timeline_id, 500.0, 1);
        rig.tick(30);
        assert_eq!(
            rig.value("c"),
            Some(1.0),
            "pass 1 starts from the lane's end"
        );
        rig.driver.lanes_start(&down.timeline_id, &down, 0, false);
        rig.driver.lanes_at(&up.timeline_id, 0.0, 0);
        rig.driver.lanes_at(&down.timeline_id, 500.0, 0);
        rig.tick(40);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 0.5).abs() < 1e-5,
            "down won, from 1"
        );
        rig.driver.set_value("c", 0.9, None, rig.at(40)).ok();
        rig.tick(50);
        assert_eq!(rig.value("c"), Some(0.9), "an action takes it back");
    }

    /// ADR-0184 decision 3: a seek keeps where the lane started from, so seeking to
    /// where it already is changes nothing, and seeking to 0 goes back to the start.
    #[test]
    fn a_seek_keeps_where_the_lane_started() {
        let mut rig = Rig::new();
        set(&mut rig.graph, ChannelInfo::new("c"));
        rig.tick(0);
        rig.driver.set_value("c", 1.0, None, rig.at(0)).ok();
        rig.tick(10);
        let down = lane_timeline("down", 0.0);
        rig.driver.lanes_start(&down.timeline_id, &down, 0, false);
        rig.driver.lanes_at(&down.timeline_id, 500.0, 0);
        rig.tick(20);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 0.5).abs() < 1e-5,
            "halfway down"
        );
        rig.driver.lanes_start(&down.timeline_id, &down, 500, true);
        rig.tick(30);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 0.5).abs() < 1e-5,
            "seeking to where it is doesn't jump"
        );
        rig.driver.lanes_start(&down.timeline_id, &down, 0, true);
        rig.tick(40);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 1.0).abs() < 1e-5,
            "seeking to 0 goes back to the start value"
        );
        rig.driver.lanes_start(&down.timeline_id, &down, 0, false);
        rig.tick(50);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 1.0).abs() < 1e-5,
            "a play starts from the value now"
        );
        rig.driver.lanes_at(&down.timeline_id, 500.0, 0);
        rig.tick(60);
        rig.driver.set_value("c", 0.8, None, rig.at(70)).ok();
        rig.tick(70);
        rig.driver.lanes_start(&down.timeline_id, &down, 0, true);
        rig.tick(80);
        assert!(
            (rig.value("c").unwrap_or(0.0) - 0.8).abs() < 1e-5,
            "a seek after an action took the channel starts from the value now"
        );
    }
}
