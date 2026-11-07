//! Filter presets (#1226, #1227): named sets of settings for a built-in filter kind, which the
//! Filters window offers above the kind's form. Choosing one sets every setting at once
//! in one `filter.set_settings`: the preset's values, and every other setting (but the
//! scene a scene-input filter uses) back to its default, so a preset looks the same
//! whatever was set before. A preset is plain settings, so an agent sets the same values
//! over MCP. Plain functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#filters

use jstream_control::protocol::{KindDescriptor, ParamType, Settings};
use serde_json::{Value, json};

/// A named set of settings for one kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// What the Filters window calls it.
    pub name: &'static str,
    /// The settings it sets; every other one goes back to its default.
    pub settings: Settings,
}

fn preset(name: &'static str, settings: Value) -> Preset {
    let Value::Object(map) = settings else {
        unreachable!("invariant: a preset's settings are a JSON object")
    };
    Preset {
        name,
        settings: map.into_iter().collect(),
    }
}

/// The presets of `kind_id`, in the order they are offered; empty for a kind that has
/// none.
pub fn presets(kind_id: &str) -> Vec<Preset> {
    match kind_id {
        "vignette" => vec![
            preset(
                "Subtle",
                json!({"strength": 0.3, "radius": 0.5, "softness": 0.7}),
            ),
            preset(
                "Cinematic",
                json!({"strength": 0.6, "radius": 0.3, "softness": 0.7}),
            ),
            preset(
                "Spotlight",
                json!({
                    "strength": 0.9, "radius": 0.15, "softness": 0.45, "shape": "round"
                }),
            ),
        ],
        "border" => vec![
            preset(
                "Card",
                json!({"width": 1.0, "color": "#ffffff33", "radius": 16.0}),
            ),
            preset(
                "Neon",
                json!({
                    "width": 3.0, "radius": 24.0, "fill": "conic", "color": "#00e5ff",
                    "color2": "#ff2bd6", "speed": 120.0, "outer_width": 2.0,
                    "outer_gap": 4.0, "outer_color": "#00e5ff66"
                }),
            ),
            preset(
                "Broadcast frame",
                json!({
                    "width": 9.0, "style": "double", "color": "#f4f4f2",
                    "outer_width": 2.0, "outer_color": "#0b2545"
                }),
            ),
        ],
        _ => Vec::new(),
    }
}

/// The patch that applies `preset` to a filter of `kind`: each of its settings, and
/// `null` (the default) for every other parameter but a scene reference.
pub fn preset_patch(kind: &KindDescriptor, preset: &Preset) -> Settings {
    kind.params
        .iter()
        .filter(|p| p.ty != ParamType::Scene)
        .map(|p| {
            let value = preset.settings.get(&p.id).cloned().unwrap_or(Value::Null);
            (p.id.clone(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{preset_patch, presets};
    use jstream_control::protocol::{
        KindCategory, KindDescriptor, ParamDescriptor, ParamType, Settings,
    };
    use serde_json::{Value, json};

    fn param(id: &str, ty: ParamType) -> ParamDescriptor {
        ParamDescriptor {
            id: id.into(),
            ty,
            default: Value::Null,
            label: id.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        }
    }

    fn kind(params: Vec<ParamDescriptor>) -> KindDescriptor {
        KindDescriptor {
            kind_id: "vignette".into(),
            category: KindCategory::VideoFilter,
            name: "Vignette".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params,
        }
    }

    #[test]
    fn the_vignette_and_the_border_have_their_looks_and_other_kinds_none() {
        let names: Vec<_> = presets("vignette").iter().map(|p| p.name).collect();
        assert_eq!(names, ["Subtle", "Cinematic", "Spotlight"]);
        let names: Vec<_> = presets("border").iter().map(|p| p.name).collect();
        assert_eq!(names, ["Card", "Neon", "Broadcast frame"]);
        assert!(presets("blur").is_empty());
    }

    /// A preset sets its own values and puts every other setting back to its default,
    /// but leaves the scene a scene-input filter uses.
    #[test]
    fn a_preset_resets_what_it_doesnt_set_but_a_scene() {
        let float = ParamType::Float {
            min: None,
            max: None,
            step: None,
            unit: None,
        };
        let kind = kind(vec![
            param("strength", float.clone()),
            param("center_x", float),
            param("shape", ParamType::Enum { options: vec![] }),
            param("scene", ParamType::Scene),
        ]);
        let spotlight = presets("vignette")
            .into_iter()
            .find(|p| p.name == "Spotlight")
            .expect("a spotlight");
        let patch = preset_patch(&kind, &spotlight);
        let expected: Settings = [
            ("strength".to_owned(), json!(0.9)),
            ("center_x".to_owned(), Value::Null),
            ("shape".to_owned(), json!("round")),
        ]
        .into_iter()
        .collect();
        assert_eq!(patch, expected);
    }
}
