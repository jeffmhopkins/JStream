//! Effect settings that motion modifiers drive
//! ([ADR-0138](../../../docs/adr/0138-motion-drives-effect-settings.md)).
//!
//! A modifier with `property` `effect` names one of its item's own effects and a number
//! or colour parameter of it. Planning resolves each such modifier against the item's chain once
//! ([`Driven::of`]): the filter's place in the chain and the parameter's range, from the
//! kind's descriptor. Each frame the render thread evaluates the modifiers on the
//! item's clocks, as it evaluates the ones that move the box, adds their values to the
//! settings the chain would draw otherwise (the plan's, or a Move's blend of them,
//! ADR-0127), keeps each within its range, rounds a whole-number one, and hands the
//! chain the result before it runs ([`Driven::settings`]). The collection never
//! changes. Planning also widens the item's layer for the most and the least each
//! setting reaches ([`Driven::extremes`]), as ADR-0136 pads for a Move's two looks.
//!
//! # Behaviour
//!
//! **Driven effect settings** (`driven.rs`, ADR-0138): an `effect` modifier adds its
//! value to one number or colour setting of one of its item's effects, each frame, as the item
//! draws them (a keyframe's overridden effects in that keyframe), on the item's
//! clocks and with an `audio` modifier's envelope, so a border's `width` or a glow's
//! strength can pulse with a bus. Modifiers on the same setting add up. A number's sum
//! is kept within the range the kind's descriptor gives the parameter, and a
//! whole-number (`int`) one is rounded; a colour takes it as a fraction of full on each
//! of its red, green and blue, within 0 and full, and keeps its alpha. One that names
//! no effect of the item, a parameter its kind doesn't have, or one of another type
//! adds nothing. A frame where
//! every driven setting adds 0 draws the plan's settings untouched. During a Move the
//! arriving item's modifiers drive the blended settings. The item's effects layer is
//! padded for the most and the least the settings reach, so a pulsing outer stroke or
//! glow is never cut.
//!
//! Docs: docs/adr/0138-motion-drives-effect-settings.md

use jstream_anim::{Kind, Modifier};
use jstream_protocol::{ItemId, MotionModifier, Settings};
use serde_json::Value;

use crate::chain::Chain;

/// The effect settings an item's modifiers drive in its chain.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Driven {
    /// The item, whose clocks and audio envelopes the modifiers read.
    pub(crate) item: ItemId,
    /// Each driven setting, with the modifiers on it in list order.
    settings: Vec<Setting>,
}

/// What a driven setting is, from its kind's descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Target {
    /// A number: its smallest and largest value, and whether it is whole (`int`).
    Number { range: (f64, f64), whole: bool },
    /// A colour, `#rrggbb` or `#rrggbbaa`: the value is added to each of its red, green
    /// and blue, as a fraction of full, and its alpha stays.
    Color,
}

/// One driven setting.
#[derive(Clone, Debug, PartialEq)]
struct Setting {
    /// The filter's place in the chain.
    filter: usize,
    /// The parameter's ID.
    param: String,
    /// What it is.
    target: Target,
    modifiers: Vec<Modifier>,
}

impl Driven {
    /// What `motion`, `item`'s effective modifiers, drives in `chain`, its effects as
    /// planned. `None` when none of them drives a number or colour setting `chain` has.
    pub(crate) fn of(item: &ItemId, motion: &[MotionModifier], chain: &Chain) -> Option<Self> {
        let mut settings: Vec<Setting> = Vec::new();
        for (target, modifier) in jstream_graph::effect_anim_modifiers(motion) {
            let Some(filter) = chain.filters.iter().position(|f| f.id == target.filter_id) else {
                continue;
            };
            let Some(kind) = chain.drivable(filter, &target.param) else {
                continue;
            };
            match settings
                .iter_mut()
                .find(|s| s.filter == filter && s.param == target.param)
            {
                Some(setting) => setting.modifiers.push(modifier),
                None => settings.push(Setting {
                    filter,
                    param: target.param,
                    target: kind,
                    modifiers: vec![modifier],
                }),
            }
        }
        (!settings.is_empty()).then(|| Driven {
            item: item.clone(),
            settings,
        })
    }

    /// The settings the chain draws with this frame: `base` (one per filter: the plan's,
    /// or a Move's blend) with each driven setting moved by the sum of `value` over its
    /// modifiers: a number kept within its range and rounded when whole, a colour's
    /// channels each within 0 and full. `None` when every setting adds 0 or `base`
    /// doesn't hold them, so the chain keeps `base`.
    pub(crate) fn settings(
        &self,
        base: &[Settings],
        value: impl Fn(&Modifier) -> f64,
    ) -> Option<Vec<Settings>> {
        let mut out: Option<Vec<Settings>> = None;
        for setting in &self.settings {
            let add: f64 = setting.modifiers.iter().map(&value).sum();
            if add == 0.0 || !add.is_finite() {
                continue;
            }
            let Some(v) = base
                .get(setting.filter)
                .and_then(|s| s.0.get(&setting.param))
                .and_then(|at| moved(setting.target, at, add))
            else {
                continue;
            };
            let settings = out.get_or_insert_with(|| base.to_vec());
            settings[setting.filter].0.insert(setting.param.clone(), v);
        }
        out
    }

    /// The settings at the least and at the most each driven number reaches from
    /// `base`, for the room the item's layer needs: each modifier's value lies within
    /// [`reach`], and the sum is kept within the setting's range. A colour draws no
    /// further, so it stays.
    pub(crate) fn extremes(&self, base: &[Settings]) -> [Vec<Settings>; 2] {
        let mut least = base.to_vec();
        let mut most = base.to_vec();
        for setting in &self.settings {
            let Target::Number { .. } = setting.target else {
                continue;
            };
            let Some(at) = base
                .get(setting.filter)
                .and_then(|s| s.0.get(&setting.param))
            else {
                continue;
            };
            let (low, high) = setting
                .modifiers
                .iter()
                .map(reach)
                .fold((0.0, 0.0), |(l, h), (ml, mh)| (l + ml, h + mh));
            for (settings, add) in [(&mut least, low), (&mut most, high)] {
                if let Some(v) = moved(setting.target, at, add) {
                    settings[setting.filter].0.insert(setting.param.clone(), v);
                }
            }
        }
        [least, most]
    }
}

/// `at`, a setting of type `target`, with `add` added: a number within its range (and
/// within the finite numbers, for an unbounded end) and whole when it is, or a colour
/// with `add` of full added to each of its red, green and blue. `None` when `at` isn't
/// one.
fn moved(target: Target, at: &Value, add: f64) -> Option<Value> {
    match target {
        Target::Number { range, whole } => {
            let v = (at.as_f64()? + add)
                .clamp(range.0, range.1)
                .clamp(-f64::from(f32::MAX), f64::from(f32::MAX));
            Some(if whole {
                Value::from(v.round() as i64)
            } else {
                Value::from(v)
            })
        }
        Target::Color => {
            let hex = at.as_str()?.strip_prefix('#')?;
            if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
                return None;
            }
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            let channel = |i: usize| -> Option<u8> {
                let v = f64::from(byte(i)?) + add * 255.0;
                Some(v.round().clamp(0.0, 255.0) as u8)
            };
            let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
            Some(Value::from(match hex.len() {
                8 => format!("#{r:02x}{g:02x}{b:02x}{:02x}", byte(6)?),
                _ => format!("#{r:02x}{g:02x}{b:02x}"),
            }))
        }
    }
}

/// The least and the most `modifier`'s value can be: a wave or a shake within its
/// amplitude, an audio modifier between its `from` and `to`, a drift within its wrap or
/// without end in its direction, and 0 when it is disabled.
fn reach(modifier: &Modifier) -> (f64, f64) {
    if !modifier.enabled {
        return (0.0, 0.0);
    }
    match modifier.kind {
        Kind::Wiggle { amplitude, .. }
        | Kind::Oscillate { amplitude, .. }
        | Kind::Impulse { amplitude, .. } => (-amplitude, amplitude),
        Kind::Audio(audio) => (audio.from.min(audio.to), audio.from.max(audio.to)),
        Kind::Drift {
            wrap: Some(wrap), ..
        } => (0.0, wrap),
        Kind::Drift { rate_per_s, .. } if rate_per_s > 0.0 => (0.0, f64::INFINITY),
        Kind::Drift { rate_per_s, .. } if rate_per_s < 0.0 => (f64::NEG_INFINITY, 0.0),
        Kind::Drift { .. } => (0.0, 0.0),
        _ => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_anim::{Audio, Clock, Curve, Measure};
    use serde_json::json;

    fn modifier(kind: Kind) -> Modifier {
        Modifier {
            id: "m".into(),
            property: jstream_anim::Property::X,
            kind,
            clock: Clock::Engine,
            enabled: true,
        }
    }

    fn driven(param: &str, range: (f64, f64), whole: bool, kinds: Vec<Kind>) -> Driven {
        driven_as(param, Target::Number { range, whole }, kinds)
    }

    fn driven_as(param: &str, target: Target, kinds: Vec<Kind>) -> Driven {
        Driven {
            item: ItemId::new("i"),
            settings: vec![Setting {
                filter: 0,
                param: param.into(),
                target,
                modifiers: kinds.into_iter().map(modifier).collect(),
            }],
        }
    }

    fn settings(value: serde_json::Value) -> Vec<Settings> {
        vec![serde_json::from_value(value).expect("settings")]
    }

    #[test]
    fn modifiers_on_one_setting_add_up_within_its_range_and_whole_ones_round() {
        let drift = |rate_per_s| Kind::Drift {
            rate_per_s,
            wrap: None,
        };
        let width = driven("width", (0.0, 64.0), false, vec![drift(1.0), drift(2.0)]);
        let base = settings(json!({"width": 4.0, "color": "#ffffff"}));
        let at = |each: f64| width.settings(&base, |_| each).expect("it moves");
        assert_eq!(at(1.5)[0].0["width"], json!(7.0));
        assert_eq!(at(-10.0)[0].0["width"], json!(0.0));
        assert_eq!(at(100.0)[0].0["width"], json!(64.0));
        assert_eq!(at(1.5)[0].0["color"], json!("#ffffff"), "the rest stays");
        // Nothing added: the chain keeps its settings.
        assert_eq!(width.settings(&base, |_| 0.0), None);
        let steps = driven("steps", (1.0, 8.0), true, vec![drift(1.0)]);
        let base = settings(json!({"steps": 3}));
        let moved = steps.settings(&base, |_| 1.6).expect("it moves");
        assert_eq!(moved[0].0["steps"], json!(5));
        // A setting that isn't a number, or a filter the base doesn't have, adds nothing.
        assert_eq!(
            steps.settings(&settings(json!({"steps": "x"})), |_| 1.0),
            None
        );
        assert_eq!(steps.settings(&[], |_| 1.0), None);
    }

    #[test]
    fn a_colour_moves_its_channels_and_keeps_its_alpha() {
        let drift = Kind::Drift {
            rate_per_s: 1.0,
            wrap: None,
        };
        let tint = driven_as("color", Target::Color, vec![drift]);
        let at = |color: &str, add: f64| {
            tint.settings(&settings(json!({ "color": color })), |_| add)
                .map(|s| s[0].0["color"].clone())
        };
        assert_eq!(at("#204060", 0.2), Some(json!("#537393")));
        assert_eq!(at("#204060ff", -1.0), Some(json!("#000000ff")));
        assert_eq!(at("#f0f0f080", 0.5), Some(json!("#ffffff80")));
        assert_eq!(at("red", 0.5), None, "not a colour it reads");
        // A colour draws no further, so the extremes keep it.
        let base = settings(json!({"color": "#204060"}));
        assert_eq!(tint.extremes(&base), [base.clone(), base]);
    }

    #[test]
    fn the_extremes_are_what_each_modifier_can_reach() {
        let audio = Kind::Audio(Audio {
            measure: Measure::Level,
            floor_db: -60.0,
            ceiling_db: 0.0,
            attack_ms: 10.0,
            release_ms: 250.0,
            curve: Curve::Linear,
            from: 6.0,
            to: -2.0,
        });
        let wave = Kind::Oscillate {
            shape: jstream_anim::Shape::Sine,
            amplitude: 3.0,
            period_ms: 1000.0,
            phase: 0.0,
        };
        let width = driven("width", (0.0, 64.0), false, vec![audio, wave]);
        let [least, most] = width.extremes(&settings(json!({"width": 10.0})));
        assert_eq!(least[0].0["width"], json!(5.0));
        assert_eq!(most[0].0["width"], json!(19.0));
        let growing = driven(
            "width",
            (0.0, 64.0),
            false,
            vec![Kind::Drift {
                rate_per_s: 1.0,
                wrap: None,
            }],
        );
        let [least, most] = growing.extremes(&settings(json!({"width": 10.0})));
        assert_eq!(least[0].0["width"], json!(10.0));
        assert_eq!(
            most[0].0["width"],
            json!(64.0),
            "without end, to its largest"
        );
    }
}
