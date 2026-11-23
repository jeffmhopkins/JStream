//! Value bindings (ADR-0122 decision 1, ADR-0178): the `binding.set_value` and
//! `binding.clear_value` edits, what number a binding's target takes
//! ([`ValueRange`]), and the setter edit that gives it a value ([`value_edit`]).
//!
//! A value binding names a source setting, a filter setting on any chain, a fader or a
//! send, or (ADR-0208) one axis of a PTZ camera's position, which the engine turns into
//! aims rather than edits ([`value_edit`] gives none). `binding.set_value` checks the target exists and takes a number: a setting its
//! kind marks `live` of type `int`, `float` or `bool`, an audio source's fader (not a
//! `scene` source's), a bus's, or a send that exists. Unlike an action binding, a value
//! binding is not removed with the object it names: it is left driving nothing, as a
//! timeline's cue is, and drives it again if an object with that ID returns. The engine
//! gives the target its value as a live value ([`value_edit`] through
//! [`Graph::with_values`](crate::Graph::with_values)), never as an edit of the
//! collection.

use crate::Graph;
use crate::apply::{Kinds, invalid, not_found};
use crate::model::State;
use jstream_protocol::{
    Edit, EntityKind, Finite32, MAX_GAIN_DB, MAX_VALUE_BINDINGS, MIN_GAIN_DB, ParamType,
    ProtocolError, Settings, SourceKind, ValueBinding, ValueTarget,
};
use serde_json::Value;

/// How a value binding's target takes its number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRange {
    /// A number kept within `min..=max`, rounded when `whole`.
    Number {
        /// The least it takes.
        min: f64,
        /// The most it takes.
        max: f64,
        /// Whether it is a whole number (`int`).
        whole: bool,
    },
    /// A `bool`: on at one half or more.
    Switch,
}

impl ValueRange {
    /// `value` as the target takes it: kept within the range and rounded for a whole
    /// number, or on at one half or more. A value that isn't finite is the least.
    pub fn json(self, value: f64) -> Value {
        match self {
            ValueRange::Number { min, max, whole } => {
                let v = if value.is_finite() { value } else { min };
                let v = v.clamp(min, max);
                if whole {
                    // Within an `i64` range it came from, so the cast is exact.
                    Value::from(v.round() as i64)
                } else {
                    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
                }
            }
            ValueRange::Switch => Value::Bool(value >= 0.5),
        }
    }
}

/// The setter edit that gives `target` `value`, as `range` takes it: what the engine
/// draws and hears the collection with while the binding drives it.
pub fn value_edit(target: &ValueTarget, range: ValueRange, value: f64) -> Option<Edit> {
    let setting = |param: &str| {
        let mut patch = Settings::new();
        patch.insert(param.to_owned(), range.json(value));
        patch
    };
    let gain = || match range.json(value) {
        Value::Number(n) => n
            .as_f64()
            .and_then(|db| Finite32::new((db as f32).clamp(MIN_GAIN_DB, MAX_GAIN_DB))),
        _ => None,
    };
    Some(match target {
        ValueTarget::SourceSetting { source_id, param } => Edit::SourceSetSettings {
            source_id: source_id.clone(),
            patch: setting(param),
        },
        ValueTarget::FilterSetting {
            owner,
            filter_id,
            param,
        } => Edit::FilterSetSettings {
            owner: owner.clone(),
            filter_id: filter_id.clone(),
            patch: setting(param),
        },
        ValueTarget::SourceGain { source_id } => Edit::SourceSetAudio {
            source_id: source_id.clone(),
            enabled: None,
            follow: None,
            gain_db: Some(gain()?),
            muted: None,
        },
        ValueTarget::BusGain { bus_id } => Edit::BusSetFader {
            bus_id: bus_id.clone(),
            gain_db: Some(gain()?),
            muted: None,
        },
        ValueTarget::SendGain { source_id, bus_id } => Edit::SendSet {
            source_id: source_id.clone(),
            bus_id: bus_id.clone(),
            gain_db: gain()?,
        },
        _ => return None,
    })
}

impl Graph {
    /// Every value binding (ADR-0178), ordered by ID.
    pub fn value_bindings(&self) -> impl ExactSizeIterator<Item = &ValueBinding> {
        self.state.value_bindings.values()
    }

    /// How `target` takes its number now, or `None` while what it names is missing or
    /// takes none.
    pub fn value_range(&self, target: &ValueTarget) -> Option<ValueRange> {
        range(&self.state, &self.kinds, target, 0).ok()
    }
}

/// Applies `binding.set_value` and `binding.clear_value`, and returns the edits that
/// undo it. `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::BindingSetValue { binding } => set(state, kinds, binding, at),
        Edit::BindingClearValue { binding_id } => clear(state, binding_id),
        _ => return None,
    })
}

fn set(
    state: &mut State,
    kinds: &Kinds,
    binding: &ValueBinding,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if let Some((field, reason)) = binding.problem() {
        return Err(invalid(at, &format!("binding.{field}"), reason));
    }
    let id = &binding.binding_id;
    if !state.value_bindings.contains_key(id) && state.value_bindings.len() >= MAX_VALUE_BINDINGS {
        return Err(invalid(
            at,
            "binding",
            format!("a collection holds at most {MAX_VALUE_BINDINGS} value bindings"),
        ));
    }
    let range = range(state, kinds, &binding.target, at)?;
    // A PTZ axis (ADR-0208) is no setting of the collection: the engine aims the head, so
    // there is no setter to probe. Its camera belongs to the machine and isn't looked up.
    if matches!(binding.target, ValueTarget::PtzAxis { .. }) {
        let old = state.value_bindings.insert(id.clone(), binding.clone());
        return Ok(vec![match old {
            Some(binding) => Edit::BindingSetValue { binding },
            None => Edit::BindingClearValue {
                binding_id: id.clone(),
            },
        }]);
    }
    // The engine gives the target its value through the setter, so a target the setter
    // refuses (a `scene` source's fader, a send that doesn't exist) is refused here.
    let mut work = state.clone();
    let probe = value_edit(&binding.target, range, probe_value(range))
        .ok_or_else(|| invalid(at, "binding.target", "is not one this version knows"))?;
    if let Err(error) = crate::apply::apply_one(&mut work, kinds, &probe, at) {
        return Err(match error {
            ProtocolError::InvalidArgument { reason, .. } => invalid(at, "binding.target", reason),
            other => other,
        });
    }
    let old = state.value_bindings.insert(id.clone(), binding.clone());
    Ok(vec![match old {
        Some(binding) => Edit::BindingSetValue { binding },
        None => Edit::BindingClearValue {
            binding_id: id.clone(),
        },
    }])
}

/// A value within `range`, to check the setter takes the target.
fn probe_value(range: ValueRange) -> f64 {
    match range {
        ValueRange::Number { min, .. } => min,
        ValueRange::Switch => 0.0,
    }
}

/// Puts `binding` in `state` as a collection file saved it: its shape checked, what it
/// names not looked up.
pub(crate) fn load(state: &mut State, binding: &ValueBinding) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = binding.problem() {
        return Err(invalid(0, &format!("binding.{field}"), reason));
    }
    if state.value_bindings.len() >= MAX_VALUE_BINDINGS {
        return Err(invalid(
            0,
            "binding",
            format!("a collection holds at most {MAX_VALUE_BINDINGS} value bindings"),
        ));
    }
    state
        .value_bindings
        .insert(binding.binding_id.clone(), binding.clone());
    Ok(())
}

fn clear(state: &mut State, binding_id: &str) -> Result<Vec<Edit>, ProtocolError> {
    let binding = state
        .value_bindings
        .remove(binding_id)
        .ok_or_else(|| not_found(EntityKind::Binding, binding_id))?;
    Ok(vec![Edit::BindingSetValue { binding }])
}

/// How `target` takes its number in `state`: `not_found` for what it names that doesn't
/// exist, `invalid_argument` at `binding.target` for a setting that isn't `live` or
/// isn't a number or `bool`.
fn range(
    state: &State,
    kinds: &Kinds,
    target: &ValueTarget,
    at: usize,
) -> Result<ValueRange, ProtocolError> {
    let fader = ValueRange::Number {
        min: f64::from(MIN_GAIN_DB),
        max: f64::from(MAX_GAIN_DB),
        whole: false,
    };
    let (kind, param) = match target {
        ValueTarget::SourceSetting { source_id, param } => {
            let source = state
                .source(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            match &source.kind {
                SourceKind::Builtin { kind } => (kind.clone(), param),
                _ => {
                    return Err(invalid(
                        at,
                        "binding.target",
                        "a `scene` source has no settings",
                    ));
                }
            }
        }
        ValueTarget::FilterSetting {
            owner,
            filter_id,
            param,
        } => {
            let chains = crate::filter::owner_chains(state, owner)?;
            let (_, _, filter) = chains
                .find(filter_id)
                .ok_or_else(|| not_found(EntityKind::Filter, filter_id.as_str()))?;
            (filter.kind.clone(), param)
        }
        ValueTarget::SourceGain { source_id } | ValueTarget::SendGain { source_id, .. } => {
            state
                .source(source_id)
                .ok_or_else(|| not_found(EntityKind::Source, source_id.as_str()))?;
            if let ValueTarget::SendGain { bus_id, .. } = target {
                state
                    .bus(bus_id)
                    .ok_or_else(|| not_found(EntityKind::Bus, bus_id.as_str()))?;
                // `send.set` would make the send; a binding drives one that exists.
                if state.send(source_id, bus_id).is_none() {
                    return Err(invalid(
                        at,
                        "binding.target",
                        format!("source `{source_id}` sends nothing into bus `{bus_id}`"),
                    ));
                }
            }
            return Ok(fader);
        }
        ValueTarget::BusGain { bus_id } => {
            state
                .bus(bus_id)
                .ok_or_else(|| not_found(EntityKind::Bus, bus_id.as_str()))?;
            return Ok(fader);
        }
        // The head's own counts (ADR-0146 §5, ADR-0208): whole numbers over the range
        // the protocol carries.
        ValueTarget::PtzAxis { axis, .. } => {
            let (min, max) = axis.range();
            return Ok(ValueRange::Number {
                min: f64::from(min),
                max: f64::from(max),
                whole: true,
            });
        }
        _ => {
            return Err(invalid(
                at,
                "binding.target",
                "is not one this version knows",
            ));
        }
    };
    let descriptor = kinds.get(&kind).ok_or_else(|| {
        invalid(
            at,
            "binding.target",
            format!("kind `{kind}` is not available, so its settings can't be checked"),
        )
    })?;
    let p = descriptor.param(param).ok_or_else(|| {
        invalid(
            at,
            "binding.target.param",
            format!("is not a parameter of kind `{kind}`"),
        )
    })?;
    if !p.live {
        return Err(invalid(
            at,
            "binding.target.param",
            format!(
                "is not a live setting of kind `{kind}`, so it changes only by an edit; \
                 a binding drives live settings"
            ),
        ));
    }
    match &p.ty {
        ParamType::Int { min, max, .. } => Ok(ValueRange::Number {
            min: min.map_or(-1e9, |v| v as f64),
            max: max.map_or(1e9, |v| v as f64),
            whole: true,
        }),
        ParamType::Float { min, max, .. } => Ok(ValueRange::Number {
            min: min.unwrap_or(-1e9),
            max: max.unwrap_or(1e9),
            whole: false,
        }),
        ParamType::Bool => Ok(ValueRange::Switch),
        _ => Err(invalid(
            at,
            "binding.target.param",
            "is not a number or `bool`; a binding drives numbers and switches",
        )),
    }
}
