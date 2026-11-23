//! The names JStream gives a JTunnel sender's modulation params
//! ([ADR-0181](../../../../docs/adr/0181-jstream-names-a-jtunnel-senders-params.md)).
//!
//! A JTunnel sender streams its 32 modulation params as its `params` key's `values`, so
//! a `data` motion modifier or value binding reads Mod 5 as key `params`, field
//! `values.4`. [`param_names`] walks the collection for those readers and names each
//! slot after what it drives: the item's source and property, the scene's camera
//! property, or the binding's target. The receiver sends each accepted sender whose
//! state it uses the names for its slots, and JTunnel shows them in the DAW.

use std::collections::BTreeMap;

use jstream_graph::Graph;
use jstream_jtunnel::wire::{MAX_MOD_NAME, MOD_SLOTS};
use jstream_protocol::{FilterOwner, MotionKind, ValueTarget};

/// The key a JTunnel sender streams its modulation params under.
pub(crate) const PARAMS_KEY: &str = "params";

/// Each sender's slot names: by the sender a reader names, or `None` for a reader that
/// takes any sender's key. Each list has [`MOD_SLOTS`] names, empty for none.
pub(crate) type ParamNames = BTreeMap<Option<String>, Vec<String>>;

/// The slot a `data` reader reads, and the sender it names, if it reads a modulation
/// param: key `params`, field `values.<n>` with `n` below [`MOD_SLOTS`].
fn slot(kind: &MotionKind) -> Option<(Option<&str>, usize)> {
    let MotionKind::Data {
        input, key, field, ..
    } = kind
    else {
        return None;
    };
    if key != PARAMS_KEY {
        return None;
    }
    let n = field.strip_prefix("values.")?;
    // Digits only: `values.+4` and `values. 4` are not slot 4 to the data reader either.
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: usize = n.parse().ok()?;
    (n < MOD_SLOTS).then_some((input.as_deref(), n))
}

/// A property or setting ID as words: `position_x` reads `Position X`.
fn words(id: &str) -> String {
    id.split(['_', '-', '.'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A serde enum's name, as words.
fn named<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => words(&name),
        _ => String::new(),
    }
}

/// `label` as a slot name JTunnel takes: control characters dropped, spaces trimmed,
/// and cut at a character boundary to [`MAX_MOD_NAME`] bytes.
pub(crate) fn fit(label: &str) -> String {
    let clean: String = label.chars().filter(|c| !c.is_control()).collect();
    let mut out = String::new();
    for c in clean.trim().chars() {
        if out.len() + c.len_utf8() > MAX_MOD_NAME {
            break;
        }
        out.push(c);
    }
    out.trim_end().to_owned()
}

/// What a value binding's target is called.
fn target_label(graph: &Graph, target: &ValueTarget) -> String {
    let state = graph.state();
    let source = |id| state.source(id).map_or("", |s| s.name());
    let bus = |id| state.bus(id).map_or("", |b| b.name());
    match target {
        ValueTarget::SourceSetting { source_id, param } => {
            format!("{} {}", source(source_id), words(param))
        }
        ValueTarget::FilterSetting { owner, param, .. } => {
            let owner = match owner {
                FilterOwner::Source { source_id } => source(source_id).to_owned(),
                FilterOwner::Scene { scene_id } => {
                    state.scene(scene_id).map_or("", |s| s.name()).to_owned()
                }
                FilterOwner::Item { item_id } => state
                    .item(item_id)
                    .map_or("", |(_, item)| source(item.source_id()))
                    .to_owned(),
                FilterOwner::Output { .. } => "Output".to_owned(),
                FilterOwner::Master => "Master".to_owned(),
                _ => String::new(),
            };
            format!("{owner} {}", words(param))
        }
        ValueTarget::SourceGain { source_id } => format!("{} Volume", source(source_id)),
        ValueTarget::BusGain { bus_id } => format!("{} Volume", bus(bus_id)),
        ValueTarget::SendGain { source_id, bus_id } => {
            format!("{} to {}", source(source_id), bus(bus_id))
        }
        _ => String::new(),
    }
}

/// Every modulation param the collection reads, named after what it drives. A slot read
/// by more than one reader takes the first one's name and a count of the rest, as
/// `Logo X +2`. Disabled modifiers name nothing.
pub(crate) fn param_names(graph: &Graph) -> ParamNames {
    let mut found: BTreeMap<Option<String>, Vec<Vec<String>>> = BTreeMap::new();
    let mut add = |kind: &MotionKind, label: String| {
        if let Some((input, n)) = slot(kind) {
            let slots = found
                .entry(input.map(str::to_owned))
                .or_insert_with(|| vec![Vec::new(); MOD_SLOTS]);
            // A keyframe scene's copy of an item names it again with the same words.
            if !slots[n].contains(&label) {
                slots[n].push(label);
            }
        }
    };
    let state = graph.state();
    for scene in state.scenes() {
        for modifier in scene.camera_motion().iter().filter(|m| m.enabled) {
            add(
                &modifier.kind,
                format!("{} {}", scene.name(), named(&modifier.property)),
            );
        }
        for item in scene.items() {
            let source = state.source(item.source_id()).map_or("", |s| s.name());
            for modifier in item.motion().iter().filter(|m| m.enabled) {
                let what = match (&modifier.source, &modifier.effect) {
                    (Some(setting), _) => words(&setting.param),
                    (None, Some(effect)) => words(&effect.param),
                    (None, None) => named(&modifier.property),
                };
                add(&modifier.kind, format!("{source} {what}"));
            }
        }
    }
    for binding in graph.value_bindings() {
        add(&binding.input, target_label(graph, &binding.target));
    }
    found
        .into_iter()
        .map(|(sender, slots)| {
            let names = slots
                .into_iter()
                .map(|labels| match labels.as_slice() {
                    [] => String::new(),
                    [one] => fit(one),
                    [first, rest @ ..] => {
                        let more = format!(" +{}", rest.len());
                        let mut name = fit(first);
                        while name.len() + more.len() > MAX_MOD_NAME {
                            name.pop();
                        }
                        format!("{}{more}", name.trim_end())
                    }
                })
                .collect();
            (sender, names)
        })
        .collect()
}

/// The names for the sender called `name`: its own readers' names, and for a slot none
/// of them read, the name from readers that take any sender's key.
pub(crate) fn for_sender(names: &ParamNames, name: &str) -> Vec<String> {
    let own = names.get(&Some(name.to_owned()));
    let any = names.get(&None);
    (0..MOD_SLOTS)
        .map(|n| {
            own.map(|slots| &slots[n])
                .filter(|slot| !slot.is_empty())
                .or_else(|| any.map(|slots| &slots[n]))
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Finite32, MotionCurve};

    fn data(input: Option<&str>, key: &str, field: &str) -> MotionKind {
        let f = |v: f32| Finite32::new(v).expect("finite");
        MotionKind::Data {
            input: input.map(str::to_owned),
            key: key.into(),
            field: field.into(),
            range: [f(0.0), f(1.0)],
            states: BTreeMap::new(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: f(0.0),
            to: f(1.0),
            color_from: None,
            palette: BTreeMap::new(),
        }
    }

    #[test]
    fn only_a_params_values_field_is_a_slot() {
        assert_eq!(slot(&data(None, "params", "values.0")), Some((None, 0)));
        assert_eq!(
            slot(&data(Some("Bitwig"), "params", "values.31")),
            Some((Some("Bitwig"), 31))
        );
        for (key, field) in [
            ("params", "values.32"),
            ("params", "values"),
            ("params", "values."),
            ("params", "values.+4"),
            ("params", "names.4"),
            ("params", "values.4.x"),
            ("transport", "values.4"),
        ] {
            assert_eq!(slot(&data(None, key, field)), None, "{key} {field}");
        }
    }

    #[test]
    fn a_label_fits_what_jtunnel_takes() {
        assert_eq!(words("position_x"), "Position X");
        assert_eq!(fit("  Logo\tScale "), "LogoScale");
        let long = "é".repeat(30);
        let fitted = fit(&long);
        assert_eq!(fitted.len(), MAX_MOD_NAME);
        assert!(jstream_jtunnel::wire::is_mod_name(&fitted));
        assert!(jstream_jtunnel::wire::is_mod_name(&fit(&format!(
            "{} x",
            "a".repeat(39)
        ))));
    }

    #[test]
    fn a_senders_own_readers_win_a_slot_over_any_senders() {
        let mut names = ParamNames::new();
        let mut any = vec![String::new(); MOD_SLOTS];
        any[0] = "Cam Pan".into();
        any[1] = "Master Volume".into();
        let mut own = vec![String::new(); MOD_SLOTS];
        own[1] = "Logo X".into();
        names.insert(None, any);
        names.insert(Some("Bitwig".into()), own);
        let bitwig = for_sender(&names, "Bitwig");
        assert_eq!(&bitwig[..3], ["Cam Pan", "Logo X", ""]);
        let other = for_sender(&names, "Laptop");
        assert_eq!(&other[..3], ["Cam Pan", "Master Volume", ""]);
        assert!(
            for_sender(&ParamNames::new(), "Bitwig")
                .iter()
                .all(String::is_empty)
        );
    }
}
