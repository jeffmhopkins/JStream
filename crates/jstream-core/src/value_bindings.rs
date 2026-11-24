//! Value bindings as the engine applies them
//! ([ADR-0178](../../../docs/adr/0178-value-bindings-drive-any-number.md)).
//!
//! A value binding reads a MIDI control, a live-data field, a channel or an audio level as
//! a `midi`, `data`, `channel` or `audio` motion modifier reads it (the readers in
//! `channels.rs`; an audio level is measured on each tick and follows its attack and
//! release, ADR-0206), follows it with the same one-pole [`Smoothing`] and maps it with
//! the same [`Midi`] parameters.
//! The engine's `jstream-values` thread ticks the [`ValueDriver`] about 30 times a
//! second while the collection holds a binding, and about 60 while a value is still
//! moving (ADR-0190). Each tick turns every moved binding's
//! value into its target's setter edit ([`jstream_graph::value_edit`]); when the set of
//! edits changes, the engine draws and hears the collection with them on top of the open
//! gestures' settings, through the shown graph a `live.set` of a source setting uses.
//! Nothing is stored, so `state.get`, undo and the saved file keep the stored values.
//!
//! # Behaviour
//!
//! **Value bindings** (`value_bindings.rs`, ADR-0178): each binding waits until its
//! control first reads something other than it read when the engine started following
//! it (a controller rests at 0 until it sends, and a data field until it changes), so a
//! collection opens with its stored values; from then on it drives its target. Each
//! target takes the value of its `replace` binding that moved last, or its stored value
//! when none has, plus every moved `add` binding's smoothed, mapped value, kept within
//! the target's range; a binding that isn't `enabled` drives nothing
//! ([ADR-0203](../../../docs/adr/0203-mappings-add-or-replace.md)). Changing only a binding's
//! target or mapping keeps its smoothing and whether it has moved. A binding whose
//! target is missing drives nothing. Removing a binding shows the stored value again.
//! A binding with an `amount` moves from its `from` by that channel's level
//! ([ADR-0183](../../../docs/adr/0183-virtual-control-channels.md) decision 7).
//!
//! Docs: docs/adr/0178-value-bindings-drive-any-number.md

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_anim::{Midi, Smoothing};
use jstream_graph::Graph;
use jstream_protocol::{Edit, PtzAxis, ValueBinding, ValueTarget};
use jstream_sources::{LiveData, MidiInputs};

use crate::reactive::HeardScopes;

/// How often the engine ticks the driver while the collection holds a binding: about
/// 30 times a second, the rate a gesture's `live.changed` events go out at.
pub(crate) const EVERY: Duration = Duration::from_micros(33_334);

/// One binding as the driver follows it.
#[derive(Debug)]
struct Driven {
    binding: ValueBinding,
    reader: crate::channels::ReaderOf,
    /// The channel that sets how much it drives (ADR-0183 decision 7), if it names one.
    amount: Option<Arc<jstream_sources::ChannelCell>>,
    midi: Midi,
    smoothing: Smoothing,
    /// What it read when the driver started following it.
    rest: f32,
    /// Whether its control has read something else since: only then does it drive.
    moved: bool,
    /// Its mapped value as of the last tick, once it has moved.
    value: Option<f64>,
    /// The driver's clock when that value last changed.
    changed: u64,
    /// Whether it names a MIDI input the engine doesn't track yet, so it is looked up
    /// again each tick until it appears.
    unresolved: bool,
}

/// The engine's value bindings between ticks: each one's smoothing, and the setter
/// edits the last tick gave.
#[derive(Debug, Default)]
pub(crate) struct ValueDriver {
    driven: BTreeMap<String, Driven>,
    last: Option<Instant>,
    edits: Vec<Edit>,
    /// Counts the changes of the bindings' values, so the last moved `replace` binding
    /// on a target is known.
    clock: u64,
    /// The PTZ positions the last tick gave (ADR-0209): a camera, an axis and the count,
    /// in binding order.
    aims: Vec<PtzAim>,
}

/// One PTZ axis a value binding drives, and the count it gives it now (ADR-0209).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PtzAim {
    /// The camera, by name.
    pub(crate) camera: String,
    /// The axis.
    pub(crate) axis: PtzAxis,
    /// The count.
    pub(crate) count: i32,
}

impl ValueDriver {
    /// The setter edits the bindings give their targets now, in binding order.
    pub(crate) fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// The PTZ positions the bindings give now, in binding order (ADR-0209).
    pub(crate) fn aims(&self) -> &[PtzAim] {
        &self.aims
    }

    /// Follows `graph`'s bindings for the time since the last tick and works out their
    /// setter edits, and the PTZ positions ([`aims`](Self::aims)). Returns whether the
    /// edits changed.
    pub(crate) fn tick(
        &mut self,
        graph: &Graph,
        midi: Option<&MidiInputs>,
        data: &LiveData,
        heard: &HeardScopes,
        now: Instant,
    ) -> bool {
        let dt_s = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.last = Some(now);
        self.keep(graph, midi, data, heard);
        // Each moved binding's value, then each target's: the last moved `replace`
        // binding's value or else the stored one, plus every `add` binding's (ADR-0203).
        let mut aims = Vec::new();
        for driven in self.driven.values_mut() {
            driven.reader.sample(dt_s, now);
            let raw = driven.reader.read();
            if !driven.moved {
                if raw == driven.rest {
                    continue;
                }
                driven.moved = true;
            }
            let level = driven.smoothing.advance(&driven.midi, raw, dt_s);
            // Pulled toward `from` by its amount; a channel that doesn't exist drives it
            // fully.
            let amount = driven
                .amount
                .as_ref()
                .and_then(|cell| cell.read())
                .map_or(1.0, f64::from);
            let from = driven.midi.from;
            let value = from + (driven.midi.value(level) - from) * amount;
            if driven.binding.enabled {
                if let ValueTarget::PtzAxis { camera, axis } = &driven.binding.target {
                    let (min, max) = axis.range();
                    let v = if value.is_finite() { value } else { 0.0 };
                    // Within the axis's `i32` range once clamped, so the cast is exact.
                    let count = v.round().clamp(f64::from(min), f64::from(max)) as i32;
                    aims.push(PtzAim {
                        camera: camera.clone(),
                        axis: *axis,
                        count,
                    });
                }
            }
            if driven.value != Some(value) {
                self.clock += 1;
                driven.changed = self.clock;
                driven.value = Some(value);
            }
        }
        let mut targets: Vec<&ValueTarget> = Vec::new();
        for driven in self.driven.values() {
            if driven.value.is_some()
                && driven.binding.enabled
                && !matches!(driven.binding.target, ValueTarget::PtzAxis { .. })
                && !targets.contains(&&driven.binding.target)
            {
                targets.push(&driven.binding.target);
            }
        }
        let mut edits = Vec::with_capacity(targets.len());
        for target in targets {
            let Some(range) = graph.value_range(target) else {
                continue;
            };
            let driving = || {
                self.driven.values().filter(move |d| {
                    d.binding.enabled && d.value.is_some() && d.binding.target == *target
                })
            };
            let replaced = driving()
                .filter(|d| d.binding.mode.is_replace())
                .max_by_key(|d| d.changed)
                .and_then(|d| d.value);
            let added: f64 = driving()
                .filter(|d| d.binding.mode.is_add())
                .filter_map(|d| d.value)
                .sum();
            let Some(base) = replaced.or_else(|| graph.stored_value(target)) else {
                continue;
            };
            if let Some(edit) = jstream_graph::value_edit(target, range, base + added) {
                edits.push(edit);
            }
        }
        self.aims = aims;
        if edits == self.edits {
            return false;
        }
        self.edits = edits;
        true
    }

    /// Follows each of `graph`'s bindings it doesn't yet, afresh for one whose input
    /// changed, and lets go of the ones `graph` no longer holds.
    fn keep(
        &mut self,
        graph: &Graph,
        midi: Option<&MidiInputs>,
        data: &LiveData,
        heard: &HeardScopes,
    ) {
        self.driven
            .retain(|id, _| graph.value_bindings().any(|b| &b.binding_id == id));
        for binding in graph.value_bindings() {
            match self.driven.get_mut(&binding.binding_id) {
                Some(driven)
                    if driven.binding == *binding
                        && !driven.unresolved
                        && driven.reader.hears_current(&binding.input, heard) => {}
                // The same control with another target, amount or smoothing time: it keeps
                // following, so a moved control goes on driving.
                Some(driven)
                    if crate::channels::same_but_smoothing(
                        &driven.binding.input,
                        &binding.input,
                    ) && !driven.unresolved
                        && driven.reader.hears_current(&binding.input, heard) =>
                {
                    if let Some(params) = crate::channels::mapping(&binding.input) {
                        driven.midi = params;
                    }
                    driven.amount = binding.amount.as_ref().map(|id| data.channels().cell(id));
                    driven.binding = binding.clone();
                }
                _ => {
                    let driven = follow(binding, midi, data, heard);
                    match driven {
                        Some(driven) => {
                            self.driven.insert(binding.binding_id.clone(), driven);
                        }
                        None => {
                            self.driven.remove(&binding.binding_id);
                        }
                    }
                }
            }
        }
    }
}

/// `binding` as the driver follows it, or `None` when it reads MIDI and the engine has
/// no MIDI inputs, or reads something this version can't.
fn follow(
    binding: &ValueBinding,
    midi: Option<&MidiInputs>,
    data: &LiveData,
    heard: &HeardScopes,
) -> Option<Driven> {
    let params = crate::channels::mapping(&binding.input)?;
    let (reader, unresolved) = crate::channels::reader(&binding.input, midi, data, heard)?;
    let rest = reader.read();
    Some(Driven {
        binding: binding.clone(),
        reader,
        amount: binding.amount.as_ref().map(|id| data.channels().cell(id)),
        midi: params,
        smoothing: Smoothing::default(),
        rest,
        moved: false,
        value: None,
        changed: 0,
        unresolved,
    })
}

#[cfg(test)]
mod tests {
    use super::ValueDriver;
    use jstream_graph::Graph;
    use jstream_protocol::{
        BusId, Edit, Finite32, MappingMode, MotionCurve, MotionKind, MotionMidiControl,
        ValueBinding, ValueTarget,
    };
    use jstream_sources::{LiveData, MidiInputs};
    use std::time::{Duration, Instant};

    fn bound(smoothing_ms: u32, controller: u8, bus: &str) -> Edit {
        Edit::BindingSetValue {
            binding: ValueBinding {
                enabled: true,
                mode: jstream_protocol::MappingMode::Replace,
                binding_id: "f".into(),
                input: MotionKind::Midi {
                    control: MotionMidiControl::Cc,
                    controller: Some(controller),
                    channel: None,
                    input: None,
                    smoothing_ms,
                    curve: MotionCurve::Linear,
                    from: Finite32::new(-100.0).expect("finite"),
                    to: Finite32::ZERO,
                },
                target: ValueTarget::BusGain {
                    bus_id: BusId::new(bus),
                },
                amount: None,
            },
        }
    }

    fn gain(edits: &[Edit]) -> Option<f32> {
        match edits {
            [Edit::BusSetFader { gain_db, .. }] => gain_db.map(Finite32::get),
            _ => None,
        }
    }

    fn cc(controller: u8, value: u8) -> jstream_midi::Message {
        jstream_midi::Message::parse(&[0xB0, controller, value]).expect("a CC")
    }

    #[test]
    fn a_binding_waits_for_its_control_then_follows_it_smoothed() {
        let mut graph = Graph::new();
        graph
            .apply(&[bound(100, 7, "master")], None)
            .expect("binds");
        let (midi, data) = (MidiInputs::new(), LiveData::new());
        let mut driver = ValueDriver::default();
        let t0 = Instant::now();
        assert!(!driver.tick(&graph, Some(&midi), &data, &Default::default(), t0));
        assert!(driver.edits().is_empty(), "at rest: nothing drives");
        midi.push("Pad", 0, &cc(7, 127));
        assert!(driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(33)
        ));
        assert_eq!(gain(driver.edits()), Some(0.0), "the first step lands");
        midi.push("Pad", 1, &cc(7, 0));
        driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(133),
        );
        let after = gain(driver.edits()).expect("a fader");
        // One time constant: about two thirds of the way down.
        assert!((after + 63.2).abs() < 0.5, "{after}");
        // Another bus, same control: it keeps following, onto the new target.
        graph
            .apply(
                &[
                    Edit::BusCreate {
                        bus_id: BusId::new("fx"),
                        name: "FX".into(),
                    },
                    bound(100, 7, "fx"),
                ],
                None,
            )
            .expect("rebinds");
        driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(166),
        );
        assert!(matches!(
            driver.edits(),
            [Edit::BusSetFader { bus_id, .. }] if bus_id.as_str() == "fx"
        ));
        // A new smoothing time keeps the moved control driving, from where it is.
        let before = gain(driver.edits()).expect("a fader");
        graph.apply(&[bound(20, 7, "fx")], None).expect("rebinds");
        driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(183),
        );
        let kept = gain(driver.edits()).expect("still driving");
        assert!(kept <= before && kept > -100.0, "{before} then {kept}");
        // Another control starts afresh: at rest until it moves.
        graph.apply(&[bound(100, 8, "fx")], None).expect("rebinds");
        assert!(driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(200)
        ));
        assert!(driver.edits().is_empty());
        graph
            .apply(
                &[Edit::BindingClearValue {
                    binding_id: "f".into(),
                }],
                None,
            )
            .expect("clears");
        assert!(!driver.tick(
            &graph,
            Some(&midi),
            &data,
            &Default::default(),
            t0 + Duration::from_millis(233)
        ));
    }

    /// A binding `id` of controller `controller` onto the master fader, from `from` to
    /// `to` dB with no smoothing.
    fn on_master(id: &str, mode: MappingMode, controller: u8, from: f32, to: f32) -> Edit {
        Edit::BindingSetValue {
            binding: ValueBinding {
                enabled: true,
                mode,
                binding_id: id.into(),
                input: MotionKind::Midi {
                    control: MotionMidiControl::Cc,
                    controller: Some(controller),
                    channel: None,
                    input: None,
                    smoothing_ms: 0,
                    curve: MotionCurve::Linear,
                    from: Finite32::new(from).expect("finite"),
                    to: Finite32::new(to).expect("finite"),
                },
                target: ValueTarget::BusGain {
                    bus_id: BusId::new("master"),
                },
                amount: None,
            },
        }
    }

    #[test]
    fn add_bindings_sum_onto_the_stored_value_or_the_last_moved_replace() {
        use jstream_protocol::MappingMode::{Add, Replace};
        let mut graph = Graph::new();
        graph
            .apply(
                &[
                    Edit::BusSetFader {
                        bus_id: BusId::new("master"),
                        gain_db: Some(Finite32::new(-10.0).expect("finite")),
                        muted: None,
                    },
                    on_master("a", Add, 1, 0.0, 6.0),
                    on_master("b", Add, 2, 0.0, -3.0),
                    on_master("c", Replace, 3, -40.0, 0.0),
                    on_master("d", Replace, 4, -20.0, -30.0),
                ],
                None,
            )
            .expect("binds");
        let (midi, data) = (MidiInputs::new(), LiveData::new());
        let mut driver = ValueDriver::default();
        let t0 = Instant::now();
        let mut tick = |graph: &Graph, n: u64| {
            driver.tick(
                graph,
                Some(&midi),
                &data,
                &Default::default(),
                t0 + Duration::from_millis(33 * n),
            );
            gain(driver.edits())
        };
        assert_eq!(tick(&graph, 0), None, "nothing has moved");
        midi.push("Pad", 0, &cc(1, 127));
        assert_eq!(tick(&graph, 1), Some(-4.0), "the stored -10 plus 6");
        midi.push("Pad", 1, &cc(2, 127));
        assert_eq!(tick(&graph, 2), Some(-7.0), "and less 3");
        midi.push("Pad", 2, &cc(3, 127));
        assert_eq!(tick(&graph, 3), Some(3.0), "replaced by 0, plus 6 less 3");
        midi.push("Pad", 3, &cc(4, 0));
        midi.push("Pad", 4, &cc(4, 127));
        assert_eq!(tick(&graph, 4), Some(-27.0), "the last moved replace wins");
        midi.push("Pad", 5, &cc(3, 0));
        assert_eq!(tick(&graph, 5), Some(-37.0), "until the other moves again");
        // Bypassed, an add binding adds nothing; the others drive on.
        let Edit::BindingSetValue { mut binding } = on_master("a", Add, 1, 0.0, 6.0) else {
            unreachable!()
        };
        binding.enabled = false;
        graph
            .apply(&[Edit::BindingSetValue { binding }], None)
            .expect("bypasses");
        assert_eq!(tick(&graph, 6), Some(-43.0));
    }
}
