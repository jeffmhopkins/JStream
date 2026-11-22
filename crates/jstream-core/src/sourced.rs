//! Source settings that motion modifiers drive
//! ([ADR-0171](../../../docs/adr/0171-live-data-from-a-daw-drives-scenes.md) decision 4).
//!
//! A modifier with `property` `source` names a live setting of its item's own source.
//! Planning keeps each item's such modifiers ([`of`]); each frame the render thread
//! evaluates them on the item's clocks, as it evaluates the ones that move the box, and
//! changes how that item draws its source: a light's colour, intensity, range, cone and
//! softness as it lights the scene ([`light`]), and a model's materials as it draws them
//! ([`materials`]). Nothing is stored, and another item placing the same source draws it
//! as it is.
//!
//! # Behaviour
//!
//! **Driven source settings** (`sourced.rs`, ADR-0171 decision 4): a `source` modifier
//! adds its value to one live setting of its item's source as that item draws it, each
//! frame, on the item's clocks and with an `audio` modifier's envelope. Modifiers on the
//! same setting add up. A light's `intensity` is kept within 0 to 64, `range_px` within 0
//! to 1 000 000, `cone_deg` within 1 to 170, and `softness` and `shadow_softness` within
//! 0 to 1; its `color` takes the sum as a fraction of full on each of its red, green and
//! blue, within 0 and full. A model's `color`, `metallic` and `roughness` change every
//! material, or with `material` only the file's materials of that name, which also take
//! `emissive` (the glow's colour, lightened at its own brightness) and
//! `emissive_strength` (a multiplier of the glow, 1 at rest, within 0 and 64). A setting
//! the source's kind doesn't drive, an `emissive` one with no material, and a material
//! the model doesn't have add nothing. An environment light's `intensity` follows too.
//! A `data` modifier that paints (ADR-0171 decision 5) sets a light's `color`, or a
//! model's `color` or a material's `emissive`, to its field's colour while it has one,
//! before the others on it add theirs; it adds nothing to any setting.
//!
//! Docs: docs/adr/0171-live-data-from-a-daw-drives-scenes.md

use std::sync::Arc;

use jstream_anim::Modifier;
use jstream_graph::Item as GraphItem;
use jstream_protocol::{MAX_MOTION_MODIFIERS, MotionSetting};
use jstream_render::MaterialDrive;
use jstream_sources::ShownLight;

/// An item's modifiers on its source's settings, each with the setting it drives.
pub(crate) type SourceDrives = Arc<[(MotionSetting, Modifier)]>;

/// For a modifier: `Some` when it paints a colour (ADR-0171 decision 5), with the colour
/// it sets now, straight sRGB 0 to 1, if any; `None` when it adds its value instead.
pub(crate) type Tint = Option<Option<[f32; 3]>>;

/// `item`'s modifiers on its source's settings, or `None` when it has none.
pub(crate) fn of(item: &GraphItem) -> Option<SourceDrives> {
    if item.motion().is_empty() {
        return None;
    }
    let drives = jstream_graph::source_anim_modifiers(item.motion());
    (!drives.is_empty()).then(|| drives.into())
}

/// How an item lights its scene with its `light` source this frame: the settings it
/// draws the light with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LightLook {
    /// Straight sRGB, 0 to 1.
    pub(crate) color: [f32; 3],
    pub(crate) intensity: f32,
    pub(crate) range_px: f32,
    pub(crate) cone_deg: f32,
    pub(crate) softness: f32,
    pub(crate) shadow_softness: f32,
}

/// The largest intensity a light reaches, as its kind keeps it.
const MAX_INTENSITY: f32 = 64.0;
/// The largest range a light reaches, as its kind keeps it.
const MAX_RANGE_PX: f32 = 1_000_000.0;

/// `light` as `drives` change it, each modifier's value given by `value`, and whether
/// it paints by `tint`.
pub(crate) fn light(
    light: &ShownLight,
    drives: &[(MotionSetting, Modifier)],
    value: impl Fn(&Modifier) -> f64,
    tint: impl Fn(&Modifier) -> Tint,
) -> LightLook {
    let mut look = LightLook {
        color: light.color,
        intensity: light.intensity,
        range_px: light.range_px,
        cone_deg: light.cone_deg,
        softness: light.softness,
        shadow_softness: light.shadow_softness,
    };
    let mut lighten = 0.0f32;
    let mut add = [0.0f32; 5];
    for (setting, modifier) in drives {
        if setting.material.is_some() {
            continue;
        }
        let slot = match setting.param.as_str() {
            "color" => None,
            "intensity" => Some(0),
            "range_px" => Some(1),
            "cone_deg" => Some(2),
            "softness" => Some(3),
            "shadow_softness" => Some(4),
            _ => continue,
        };
        if let Some(color) = tint(modifier) {
            if let (None, Some(color)) = (slot, color) {
                look.color = color.map(|c| c.clamp(0.0, 1.0));
            }
            continue;
        }
        let v = value(modifier) as f32;
        if !v.is_finite() {
            continue;
        }
        match slot {
            None => lighten += v,
            Some(i) => add[i] += v,
        }
    }
    if lighten != 0.0 {
        look.color = look.color.map(|c| (c + lighten).clamp(0.0, 1.0));
    }
    look.intensity = (look.intensity + add[0]).clamp(0.0, MAX_INTENSITY);
    look.range_px = (look.range_px + add[1]).clamp(0.0, MAX_RANGE_PX);
    look.cone_deg = (look.cone_deg + add[2]).clamp(1.0, 170.0);
    look.softness = (look.softness + add[3]).clamp(0.0, 1.0);
    look.shadow_softness = (look.shadow_softness + add[4]).clamp(0.0, 1.0);
    look
}

/// An environment light's intensity as `drives` change it; a painting modifier adds
/// nothing.
pub(crate) fn environment_intensity(
    intensity: f32,
    drives: &[(MotionSetting, Modifier)],
    value: impl Fn(&Modifier) -> f64,
    tint: impl Fn(&Modifier) -> Tint,
) -> f32 {
    let add: f32 = drives
        .iter()
        .filter(|(s, m)| s.material.is_none() && s.param == "intensity" && tint(m).is_none())
        .map(|(_, m)| value(m) as f32)
        .filter(|v| v.is_finite())
        .sum();
    (intensity + add).clamp(0.0, MAX_INTENSITY)
}

/// How `drives` change a model's materials as its item draws it, at most one drive a
/// modifier, on the stack: the drives and how many of them there are. A painting
/// modifier (`tint`) sets a `color` or an `emissive` while it has a colour, and adds
/// nothing.
pub(crate) fn materials(
    drives: &[(MotionSetting, Modifier)],
    value: impl Fn(&Modifier) -> f64,
    tint: impl Fn(&Modifier) -> Tint,
) -> ([MaterialDrive<'_>; MAX_MOTION_MODIFIERS], usize) {
    let mut out = [MaterialDrive::default(); MAX_MOTION_MODIFIERS];
    let mut n = 0;
    for (setting, modifier) in drives {
        if n == out.len() {
            break;
        }
        let material = setting.material.as_deref();
        if let Some(color) = tint(modifier) {
            let Some(color) = color else {
                continue;
            };
            let mut drive = MaterialDrive {
                material,
                ..MaterialDrive::default()
            };
            match (setting.param.as_str(), material) {
                ("color", _) => drive.set_color = Some(color),
                ("emissive", Some(_)) => drive.set_emissive = Some(color),
                _ => continue,
            }
            out[n] = drive;
            n += 1;
            continue;
        }
        let v = value(modifier) as f32;
        if !v.is_finite() || v == 0.0 {
            continue;
        }
        let mut drive = MaterialDrive {
            material,
            ..MaterialDrive::default()
        };
        match (setting.param.as_str(), material) {
            ("color", _) => drive.color = v,
            ("metallic", _) => drive.metallic = v,
            ("roughness", _) => drive.roughness = v,
            ("emissive", Some(_)) => drive.emissive = v,
            ("emissive_strength", Some(_)) => drive.emissive_strength = v,
            _ => continue,
        }
        out[n] = drive;
        n += 1;
    }
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_anim::{Clock, Kind, Property};
    use jstream_sources::LightType;

    fn modifier(id: &str) -> Modifier {
        Modifier {
            id: id.into(),
            property: Property::X,
            kind: Kind::Drift {
                rate_per_s: 0.0,
                wrap: None,
            },
            clock: Clock::Engine,
            enabled: true,
        }
    }

    fn setting(param: &str, material: Option<&str>) -> MotionSetting {
        MotionSetting {
            param: param.into(),
            material: material.map(Into::into),
        }
    }

    /// Each modifier's value is its ID parsed as a number.
    fn by_id(m: &Modifier) -> f64 {
        m.id.parse().unwrap_or(f64::NAN)
    }

    fn lamp() -> ShownLight {
        ShownLight {
            kind: LightType::Point,
            color: [0.5, 0.5, 0.5],
            intensity: 1.0,
            range_px: 100.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        }
    }

    #[test]
    fn a_light_takes_its_settings_added_within_their_ranges() {
        let drives = [
            (setting("intensity", None), modifier("2")),
            (setting("intensity", None), modifier("1")),
            (setting("color", None), modifier("0.25")),
            (setting("cone_deg", None), modifier("500")),
            (setting("softness", None), modifier("-1")),
            (setting("range_px", None), modifier("50")),
            // Not a light's: nothing.
            (setting("metallic", None), modifier("1")),
            (setting("intensity", Some("glass")), modifier("9")),
        ];
        let look = light(&lamp(), &drives, by_id, |_| None);
        assert_eq!(look.intensity, 4.0);
        assert_eq!(look.color, [0.75; 3]);
        assert_eq!(look.cone_deg, 170.0);
        assert_eq!(look.softness, 0.0);
        assert_eq!(look.range_px, 150.0);
        assert_eq!(look.shadow_softness, 0.3);
        // Never past 64, never below 0, and a light no modifier drives is as it is.
        let bright = [(setting("intensity", None), modifier("1000"))];
        assert_eq!(light(&lamp(), &bright, by_id, |_| None).intensity, 64.0);
        let dark = [(setting("color", None), modifier("-2"))];
        assert_eq!(light(&lamp(), &dark, by_id, |_| None).color, [0.0; 3]);
        let still = light(&lamp(), &[], by_id, |_| None);
        assert_eq!(still.color, lamp().color);
        assert_eq!(still.intensity, lamp().intensity);
        assert_eq!(
            environment_intensity(
                1.0,
                &[(setting("intensity", None), modifier("-5"))],
                by_id,
                |_| None
            ),
            0.0
        );
    }

    #[test]
    fn a_model_takes_a_drive_a_modifier_and_glow_only_by_material() {
        let drives = [
            (setting("color", None), modifier("0.5")),
            (setting("emissive_strength", Some("led")), modifier("2")),
            (setting("emissive", Some("led")), modifier("0.1")),
            (setting("roughness", Some("trim")), modifier("-0.2")),
            // A glow needs a material, a zero adds nothing, and an unknown setting none.
            (setting("emissive_strength", None), modifier("3")),
            (setting("metallic", None), modifier("0")),
            (setting("lighting", None), modifier("1")),
        ];
        let (out, n) = materials(&drives, by_id, |_| None);
        let got = &out[..n];
        assert_eq!(
            got,
            &[
                MaterialDrive {
                    color: 0.5,
                    ..MaterialDrive::default()
                },
                MaterialDrive {
                    material: Some("led"),
                    emissive_strength: 2.0,
                    ..MaterialDrive::default()
                },
                MaterialDrive {
                    material: Some("led"),
                    emissive: 0.1,
                    ..MaterialDrive::default()
                },
                MaterialDrive {
                    material: Some("trim"),
                    roughness: -0.2,
                    ..MaterialDrive::default()
                },
            ]
        );
        assert_eq!(materials(&[], by_id, |_| None).1, 0);
    }

    /// ADR-0171 decision 5: a painting modifier sets a light's colour, a model's colour
    /// or a material's glow while it has a colour, before the others add, and never adds
    /// itself.
    #[test]
    fn a_painting_modifier_sets_a_colour_and_adds_nothing() {
        let tint = |m: &Modifier| match m.id.as_str() {
            "red" => Some(Some([1.0, 0.0, 0.0])),
            "none" => Some(None),
            _ => None,
        };
        let mut red = modifier("9");
        red.id = "red".into();
        let mut none = modifier("9");
        none.id = "none".into();
        let drives = [
            (setting("color", None), red.clone()),
            (setting("color", None), modifier("-0.5")),
            (setting("intensity", None), red.clone()),
        ];
        let look = light(&lamp(), &drives, by_id, tint);
        assert_eq!(look.color, [0.5, 0.0, 0.0], "set red, then darkened");
        assert_eq!(look.intensity, 1.0, "it adds nothing to a number");
        let unset = [(setting("color", None), none.clone())];
        assert_eq!(light(&lamp(), &unset, by_id, tint).color, lamp().color);
        assert_eq!(
            environment_intensity(
                1.0,
                &[(setting("intensity", None), red.clone())],
                by_id,
                tint
            ),
            1.0
        );
        let drives = [
            (setting("color", None), red.clone()),
            (setting("emissive", Some("led")), red.clone()),
            (setting("emissive", None), red.clone()),
            (setting("metallic", None), red),
            (setting("color", None), none),
        ];
        let (out, n) = materials(&drives, by_id, tint);
        assert_eq!(
            &out[..n],
            &[
                MaterialDrive {
                    set_color: Some([1.0, 0.0, 0.0]),
                    ..MaterialDrive::default()
                },
                MaterialDrive {
                    material: Some("led"),
                    set_emissive: Some([1.0, 0.0, 0.0]),
                    ..MaterialDrive::default()
                },
            ]
        );
    }
}
