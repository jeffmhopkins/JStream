//! Source settings that motion modifiers drive
//! ([ADR-0171](../../../docs/adr/0171-live-data-from-a-daw-drives-scenes.md) decision 4).
//!
//! A modifier with `property` `source` names a live setting of its item's own source.
//! Planning keeps each item's such modifiers ([`of`]); each frame the render thread
//! evaluates them on the item's clocks, as it evaluates the ones that move the box, and
//! changes how that item draws its source: a light's colour, intensity, range, cone and
//! softness as it lights the scene ([`light`]), an environment light's sky
//! ([`environment`]), and a model's materials as it draws them
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
//! blue, within 0 and full. A model's `color`, `metallic`, `roughness` and
//! `emissive_strength` (a multiplier of the glow, 1 at rest, within 0 and 64) change
//! every material, or with `material` only the file's materials of that name, which also
//! take `emissive` (the glow's colour, lightened at its own brightness). A setting the
//! source's kind doesn't drive, an `emissive` one with no material, and a material the
//! model doesn't have add nothing. An environment light's `intensity` follows too,
//! and so does its sky ([`environment`]): `stars`, `nebula` and `galaxy` within 0 to 1,
//! `star_brightness` within 0 to 8 and `warp` within 0 to 1, which its backdrop's ramp
//! then eases toward; its `sky_color`, `horizon_color`, `ground_color`, `nebula_color` and
//! `nebula_color_2` take theirs as `color` does. Its `sky_seed` and `warp_ramp_s` don't.
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
use jstream_sources::{ShownEnvironment, ShownLight};

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

/// How an environment light lights and draws its sky this frame (ADR-0151 decision 9,
/// ADR-0167): the settings of its source's sky as its item's modifiers change them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SkyLook {
    /// How bright the light is, 0 to 64.
    pub(crate) intensity: f32,
    /// The sky straight up, at the horizon and below it, straight sRGB, 0 to 1.
    pub(crate) sky: [f32; 3],
    pub(crate) horizon: [f32; 3],
    pub(crate) ground: [f32; 3],
    /// How many stars the backdrop scatters, 0 to 1, and how bright they are, 0 to 8.
    pub(crate) stars: f32,
    pub(crate) star_brightness: f32,
    /// How much nebula the backdrop draws, 0 to 1, in its two colours.
    pub(crate) nebula: f32,
    pub(crate) nebula_colors: [[f32; 3]; 2],
    /// How bright the galaxy band is, 0 to 1.
    pub(crate) galaxy: f32,
    /// How fast the backdrop streams past, 0 to 1, before its ramp.
    pub(crate) warp: f32,
}

/// The highest star brightness a sky takes, as its source's setting does.
const MAX_STAR_BRIGHTNESS: f32 = 8.0;

/// An environment light's sky as `drives` change it: its `intensity`, its `stars`,
/// `star_brightness`, `nebula`, `galaxy` and `warp` take the sum of their modifiers within
/// their ranges, and its `sky_color`, `horizon_color`, `ground_color`, `nebula_color` and
/// `nebula_color_2` take theirs as a fraction of full on each channel. A painting
/// modifier sets a colour while it has one, before the others add, and adds nothing.
pub(crate) fn environment(
    intensity: f32,
    sky: &ShownEnvironment,
    drives: &[(MotionSetting, Modifier)],
    value: impl Fn(&Modifier) -> f64,
    tint: impl Fn(&Modifier) -> Tint,
) -> SkyLook {
    let mut colors = [
        sky.sky,
        sky.horizon,
        sky.ground,
        sky.nebula_colors[0],
        sky.nebula_colors[1],
    ];
    let mut lighten = [0.0f32; 5];
    let mut add = [0.0f32; 6];
    for (setting, modifier) in drives {
        if setting.material.is_some() {
            continue;
        }
        let slot = match setting.param.as_str() {
            "sky_color" => Err(0),
            "horizon_color" => Err(1),
            "ground_color" => Err(2),
            "nebula_color" => Err(3),
            "nebula_color_2" => Err(4),
            "intensity" => Ok(0),
            "stars" => Ok(1),
            "star_brightness" => Ok(2),
            "nebula" => Ok(3),
            "galaxy" => Ok(4),
            "warp" => Ok(5),
            _ => continue,
        };
        if let Some(color) = tint(modifier) {
            if let (Err(i), Some(color)) = (slot, color) {
                colors[i] = color.map(|c| c.clamp(0.0, 1.0));
            }
            continue;
        }
        let v = value(modifier) as f32;
        if !v.is_finite() {
            continue;
        }
        match slot {
            Err(i) => lighten[i] += v,
            Ok(i) => add[i] += v,
        }
    }
    let [sky_color, horizon, ground, nebula_a, nebula_b] = std::array::from_fn(|i| {
        if lighten[i] == 0.0 {
            colors[i]
        } else {
            colors[i].map(|c| (c + lighten[i]).clamp(0.0, 1.0))
        }
    });
    SkyLook {
        intensity: (intensity + add[0]).clamp(0.0, MAX_INTENSITY),
        sky: sky_color,
        horizon,
        ground,
        stars: (sky.stars + add[1]).clamp(0.0, 1.0),
        star_brightness: (sky.star_brightness + add[2]).clamp(0.0, MAX_STAR_BRIGHTNESS),
        nebula: (sky.nebula + add[3]).clamp(0.0, 1.0),
        nebula_colors: [nebula_a, nebula_b],
        galaxy: (sky.galaxy + add[4]).clamp(0.0, 1.0),
        warp: (sky.warp + add[5]).clamp(0.0, 1.0),
    }
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
            ("emissive_strength", _) => drive.emissive_strength = v,
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
            environment(
                1.0,
                &sky(),
                &[(setting("intensity", None), modifier("-5"))],
                by_id,
                |_| None
            )
            .intensity,
            0.0
        );
    }

    fn sky() -> ShownEnvironment {
        ShownEnvironment {
            sky: [0.2, 0.3, 0.6],
            horizon: [0.5; 3],
            ground: [0.1; 3],
            image: None,
            backdrop: true,
            stars: 0.5,
            star_brightness: 1.0,
            nebula: 0.0,
            nebula_colors: [[0.4, 0.1, 0.5], [0.1, 0.3, 0.6]],
            galaxy: 0.2,
            sky_seed: 7,
            warp: 0.0,
            warp_ramp_s: 1.0,
            warp_aim: jstream_sources::WarpAim::Sky,
            warp_look_deg: 10.0,
            sky_pictures: None,
            sky_picture_deg: 30.0,
            sky_picture_light: 1.0,
        }
    }

    /// An environment light's sky settings follow its item's modifiers, within their
    /// ranges, as its intensity does.
    #[test]
    fn an_environment_lights_sky_takes_its_settings_added_within_their_ranges() {
        let drives = [
            (setting("warp", None), modifier("0.5")),
            (setting("warp", None), modifier("0.25")),
            (setting("stars", None), modifier("3")),
            (setting("star_brightness", None), modifier("100")),
            (setting("nebula", None), modifier("-1")),
            (setting("galaxy", None), modifier("0.3")),
            (setting("intensity", None), modifier("2")),
            (setting("sky_color", None), modifier("0.5")),
            (setting("ground_color", None), modifier("-1")),
            // Not a sky's, or not a number: nothing.
            (setting("sky_seed", None), modifier("3")),
            (setting("warp_ramp_s", None), modifier("3")),
            (setting("cone_deg", None), modifier("3")),
            (setting("warp", Some("glass")), modifier("1")),
        ];
        let look = environment(1.0, &sky(), &drives, by_id, |_| None);
        assert_eq!(look.warp, 0.75);
        assert_eq!(look.stars, 1.0);
        assert_eq!(look.star_brightness, 8.0);
        assert_eq!(look.nebula, 0.0);
        assert!((look.galaxy - 0.5).abs() < 1e-6);
        assert_eq!(look.intensity, 3.0);
        assert_eq!(look.sky, [0.7, 0.8, 1.0]);
        assert_eq!(look.ground, [0.0; 3]);
        assert_eq!(look.horizon, sky().horizon);
        assert_eq!(look.nebula_colors, sky().nebula_colors);
        let still = environment(1.0, &sky(), &[], by_id, |_| None);
        assert_eq!(still.warp, 0.0);
        assert_eq!(still.stars, 0.5);
        assert_eq!(still.sky, sky().sky);
    }

    #[test]
    fn a_model_takes_a_drive_a_modifier_and_a_glows_colour_only_by_material() {
        let drives = [
            (setting("color", None), modifier("0.5")),
            (setting("emissive_strength", Some("led")), modifier("2")),
            (setting("emissive", Some("led")), modifier("0.1")),
            (setting("roughness", Some("trim")), modifier("-0.2")),
            // Every material's glow strength at once, as a switch for all its lights.
            (setting("emissive_strength", None), modifier("-3")),
            // A glow's colour needs a material, a zero adds nothing, and an unknown
            // setting none.
            (setting("emissive", None), modifier("0.4")),
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
                MaterialDrive {
                    emissive_strength: -3.0,
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
        let painted = environment(
            1.0,
            &sky(),
            &[
                (setting("intensity", None), red.clone()),
                (setting("horizon_color", None), red.clone()),
                (setting("horizon_color", None), modifier("0.25")),
                (setting("nebula_color_2", None), none.clone()),
            ],
            by_id,
            tint,
        );
        assert_eq!(painted.intensity, 1.0, "it adds nothing to a number");
        assert_eq!(
            painted.horizon,
            [1.0, 0.25, 0.25],
            "set red, then lightened"
        );
        assert_eq!(painted.nebula_colors, sky().nebula_colors, "no colour yet");
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
