//! Controller learn (ADR-0178): a button beside a live setting or a fader that binds the
//! next MIDI control moved to it, as a value binding, and shows and forgets the one it
//! has.
//!
//! Learning reads `midi.last_controls` once for a baseline, then about seven times a
//! second for up to 15 s; the first input whose count of control changes grows is the
//! control moved, and `binding.set_value` binds it over the setting's whole range (a
//! fader's over -60 to +6 dB). A binding the button made is named after its target
//! ([`learn_id`]), so learning again replaces it.

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    Edit, Finite32, MidiControlInfo, MotionCurve, MotionKind, MotionMidiControl, ParamDescriptor,
    ParamType, Query, QueryResult, ValueBinding, ValueTarget,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How often learning reads the inputs' latest controls.
const POLL: Duration = Duration::from_millis(150);
/// How many times learning reads the controls before it gives up: 15 s.
const TRIES: u32 = 100;
/// The throw a learned fader gets, in dB: wide enough to fade out, short of the top.
pub const FADER_THROW: (f64, f64) = (-60.0, 6.0);

/// What owns the settings a form shows, for the learn button beside each.
#[derive(Clone, Debug, PartialEq)]
pub enum LearnOwner {
    /// A source's own settings.
    Source(jstream_control::protocol::SourceId),
    /// A filter's settings.
    Filter(
        jstream_control::protocol::FilterOwner,
        jstream_control::protocol::FilterId,
    ),
}

impl LearnOwner {
    /// The value target of its setting `param`.
    pub fn target(&self, param: &str) -> ValueTarget {
        match self {
            LearnOwner::Source(source_id) => ValueTarget::SourceSetting {
                source_id: source_id.clone(),
                param: param.to_owned(),
            },
            LearnOwner::Filter(owner, filter_id) => ValueTarget::FilterSetting {
                owner: owner.clone(),
                filter_id: filter_id.clone(),
                param: param.to_owned(),
            },
        }
    }
}

/// The range a learned binding maps a controller onto for `param`, or `None` when
/// `param` takes no controller: a `live` number with both ends, or a `bool`.
pub fn param_range(param: &ParamDescriptor) -> Option<(f64, f64)> {
    if !param.live {
        return None;
    }
    match &param.ty {
        ParamType::Int {
            min: Some(min),
            max: Some(max),
            ..
        } => Some((*min as f64, *max as f64)),
        ParamType::Float {
            min: Some(min),
            max: Some(max),
            ..
        } => Some((*min, *max)),
        ParamType::Bool => Some((0.0, 1.0)),
        _ => None,
    }
}

/// The ID the button names a binding of `target` with: `learn_` and a hash of the
/// target, so each target has one and learning again replaces it.
pub fn learn_id(target: &ValueTarget) -> String {
    // FNV-1a over the target's wire form: stable across runs and builds.
    let text = serde_json::to_string(target).unwrap_or_default();
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("learn_{hash:016x}")
}

/// The control in `now` that moved since `baseline`: the first input whose count grew,
/// or that wasn't there before.
pub fn moved(baseline: &[MidiControlInfo], now: &[MidiControlInfo]) -> Option<MidiControlInfo> {
    now.iter()
        .find(|control| {
            baseline
                .iter()
                .find(|before| before.input == control.input)
                .is_none_or(|before| control.count > before.count)
        })
        .cloned()
}

/// The binding of `control` to `target` over `range`, named [`learn_id`].
pub fn learned(control: &MidiControlInfo, target: &ValueTarget, range: (f64, f64)) -> ValueBinding {
    let end = |v: f64| Finite32::new(v as f32).unwrap_or(Finite32::ZERO);
    ValueBinding {
        binding_id: learn_id(target),
        input: MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(control.controller),
            channel: Some(control.channel),
            input: Some(control.input.clone()),
            smoothing_ms: 50,
            curve: MotionCurve::Linear,
            from: end(range.0),
            to: end(range.1),
        },
        target: target.clone(),
    }
}

/// What a binding reads, as the button shows it: `CC 7`, `Bend`, `Pressure` or `Data`.
pub fn short_name(binding: &ValueBinding) -> String {
    match &binding.input {
        MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(n),
            ..
        } => format!("CC {n}"),
        MotionKind::Midi {
            control: MotionMidiControl::PitchBend,
            ..
        } => "Bend".into(),
        MotionKind::Midi { .. } => "Pressure".into(),
        _ => "Data".into(),
    }
}

/// What a binding reads, in full, for the button's tooltip.
pub fn long_name(binding: &ValueBinding) -> String {
    match &binding.input {
        MotionKind::Midi { channel, input, .. } => {
            let on = channel.map_or("any channel".to_owned(), |c| format!("channel {c}"));
            let from = input.as_deref().unwrap_or("any input");
            format!(
                "{} on {on} from {from} drives this. Click to unbind it.",
                short_name(binding)
            )
        }
        MotionKind::Data {
            input, key, field, ..
        } => format!(
            "Field `{field}` of `{key}` from {} drives this. Click to unbind it.",
            input.as_deref().unwrap_or("any sender")
        ),
        _ => "A binding drives this. Click to unbind it.".into(),
    }
}

/// Each input's latest control change, or `None` when the engine answers none.
async fn controls(session: &crate::session::Session) -> Option<Vec<MidiControlInfo>> {
    match query(session, Query::MidiLastControls).await {
        Ok(QueryResult::MidiLastControls { controls }) => Some(controls),
        _ => None,
    }
}

/// Reads the controls after [`POLL`] and binds the first that moved since `baseline`,
/// or reads again, `tries` more times, while `learning` holds. The button's signals are
/// read with `try_`: once its form closes they are gone, and the loop stops.
fn watch(
    session: crate::session::Session,
    studio: Studio,
    learning: RwSignal<bool>,
    target: StoredValue<ValueTarget>,
    range: (f64, f64),
    baseline: Vec<MidiControlInfo>,
    tries: u32,
) {
    set_timeout(
        move || {
            spawn_local(async move {
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                let now = controls(&session).await.unwrap_or_default();
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                if let Some(control) = moved(&baseline, &now) {
                    let Some(binding) = target.try_with_value(|t| learned(&control, t, range))
                    else {
                        return;
                    };
                    studio.apply(&session, vec![Edit::BindingSetValue { binding }]);
                    learning.try_set(false);
                } else if tries == 0 {
                    learning.try_set(false);
                } else {
                    watch(
                        session,
                        studio,
                        learning,
                        target,
                        range,
                        baseline,
                        tries - 1,
                    );
                }
            });
        },
        POLL,
    );
}

/// The learn button for `target`, mapping a controller onto `range`.
#[component]
pub fn Learn(
    /// The setting a learned control drives.
    target: ValueTarget,
    /// What the control's lowest and highest map onto, in the setting's unit.
    range: (f64, f64),
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let target = StoredValue::new(target);
    let learning = RwSignal::new(false);
    let bound = move || {
        studio.collection.with(|c| {
            c.as_ref().and_then(|c| {
                target.with_value(|t| c.value_bindings.iter().find(|b| &b.target == t).cloned())
            })
        })
    };
    let learn = move || {
        let Some(s) = session.0.get_value() else {
            return;
        };
        learning.set(true);
        spawn_local(async move {
            match controls(&s).await {
                // The form closed while the baseline was read: nothing to learn for.
                Some(_) if learning.try_get_untracked() != Some(true) => {}
                Some(baseline) => watch(s, studio, learning, target, range, baseline, TRIES),
                None => {
                    // `try_set` hands the value back when the form has closed.
                    if learning.try_set(false).is_some() {
                        return;
                    }
                    studio
                        .problem
                        .set(Some("This engine reads no MIDI controls.".into()));
                }
            }
        });
    };
    let click = move |_| {
        if learning.get_untracked() {
            learning.set(false);
        } else if let Some(binding) = bound() {
            if let Some(s) = session.0.get_value() {
                studio.apply(
                    &s,
                    vec![Edit::BindingClearValue {
                        binding_id: binding.binding_id,
                    }],
                );
            }
        } else {
            learn();
        }
    };
    let title = move || match (learning.get(), bound()) {
        (true, _) => "Move a fader, knob or button on a MIDI controller. Click to stop.".into(),
        (false, Some(binding)) => long_name(&binding),
        (false, None) => "Learn a MIDI controller: click, then move it.".to_owned(),
    };
    let text = move || match (learning.get(), bound()) {
        (true, _) => "Move…".to_owned(),
        (false, Some(binding)) => short_name(&binding),
        (false, None) => String::new(),
    };
    view! {
        <button
            class="learn"
            class:learning=learning
            class:bound=move || bound().is_some()
            title=title
            aria-label=title
            on:click=click
        >
            <Icon name="midi" size=13 />
            {text}
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{BusId, SourceId};

    fn control(input: &str, controller: u8, count: u64) -> MidiControlInfo {
        MidiControlInfo {
            input: input.into(),
            channel: 1,
            controller,
            value: 64,
            count,
        }
    }

    #[test]
    fn the_moved_control_is_the_first_whose_count_grew() {
        let baseline = [control("Bitwig", 7, 3), control("Pads", 1, 9)];
        assert_eq!(moved(&baseline, &baseline), None);
        let now = [control("Bitwig", 7, 3), control("Pads", 21, 10)];
        assert_eq!(moved(&baseline, &now).map(|c| c.controller), Some(21));
        let new_input = [control("Bitwig", 7, 3), control("Knobs", 74, 1)];
        assert_eq!(
            moved(&baseline, &new_input).map(|c| c.input),
            Some("Knobs".into())
        );
    }

    #[test]
    fn a_learned_binding_reads_that_control_over_the_range() {
        let target = ValueTarget::BusGain {
            bus_id: BusId::master(),
        };
        let binding = learned(&control("Bitwig", 7, 1), &target, FADER_THROW);
        assert_eq!(binding.problem(), None);
        assert_eq!(binding.binding_id, learn_id(&target));
        assert_eq!(short_name(&binding), "CC 7");
        assert!(long_name(&binding).contains("channel 1 from Bitwig"));
        let other = ValueTarget::SourceGain {
            source_id: SourceId::new("mic"),
        };
        assert_ne!(learn_id(&other), learn_id(&target));
    }

    #[test]
    fn only_a_live_number_with_ends_or_a_switch_takes_a_controller() {
        let param = |ty: ParamType, live: bool| ParamDescriptor {
            warning: None,
            id: "p".into(),
            ty,
            default: serde_json::Value::Null,
            label: "P".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live,
        };
        let float = ParamType::Float {
            min: Some(0.0),
            max: Some(2.0),
            step: None,
            unit: None,
        };
        assert_eq!(param_range(&param(float.clone(), true)), Some((0.0, 2.0)));
        assert_eq!(param_range(&param(float, false)), None);
        assert_eq!(param_range(&param(ParamType::Bool, true)), Some((0.0, 1.0)));
        let open = ParamType::Int {
            min: Some(0),
            max: None,
            step: None,
            unit: None,
        };
        assert_eq!(param_range(&param(open, true)), None);
    }
}
