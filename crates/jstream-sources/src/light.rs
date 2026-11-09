//! The `light` source (ADR-0143 decision 1): a light the item that places it puts in a
//! 3D scene, for the models there that are lit by their scene. It draws nothing itself.
//! An `environment` light (ADR-0151 decision 9) is the sky around the whole scene.

use crate::color::{parse_color, rgba_frame};
use crate::describe::{kind, param};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    EnumOption, KindDescriptor, Media, ParamDescriptor, ParamType, Settings, SourceId, VisibleIf,
};
use serde_json::json;
use std::sync::Arc;

/// The most irradiance a light gives a surface facing it.
const MAX_INTENSITY: f64 = 64.0;

/// An environment's default sky straight up, at the horizon, and below it.
const SKY: &str = "#9cc2e8";
const HORIZON: &str = "#ffffff";
const GROUND: &str = "#3a3a3a";

/// The farthest a light's range reaches, in scene pixels.
const MAX_RANGE_PX: f64 = 1_000_000.0;

/// What kind of light a `light` source is (ADR-0143 decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightType {
    /// From its position in every direction, fading out to its range.
    Point,
    /// From its position along its aim, within its cone.
    Spot,
    /// Along its aim from infinitely far, with no falloff.
    Directional,
    /// From everywhere: scales a model's environment light.
    Ambient,
    /// The sky around the whole scene (ADR-0151 decision 9): a model's environment light.
    Environment,
}

const TYPES: [(LightType, &str, &str); 5] = [
    (LightType::Point, "point", "Point"),
    (LightType::Spot, "spot", "Spot"),
    (LightType::Directional, "directional", "Directional"),
    (LightType::Environment, "environment", "Environment"),
    (LightType::Ambient, "ambient", "Ambient"),
];

/// A `light` source's settings ([`Source::light`]): what the item that places it
/// lights a scene with, before its position, aim and opacity, which the item gives.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownLight {
    /// What kind it is.
    pub kind: LightType,
    /// Its colour, straight sRGB, 0 to 1. Its alpha is ignored.
    pub color: [f32; 3],
    /// Irradiance on a surface facing it, before falloff, 0 to 64.
    pub intensity: f32,
    /// Where a point or spot light has faded to nothing, in scene pixels; 0 never fades.
    pub range_px: f32,
    /// A spot light's full cone angle, 1 to 170 degrees.
    pub cone_deg: f32,
    /// The share of a spot cone, from its edge inward, over which it fades, 0 to 1.
    pub softness: f32,
    /// Whether it casts shadows (ADR-0151 decision 3): only a spot or directional
    /// light does.
    pub shadows: bool,
    /// How soft its shadows' edges are, 0 (hard) to 1.
    pub shadow_softness: f32,
    /// An environment light's sky (ADR-0151 decision 9); `None` for any other kind.
    pub environment: Option<ShownEnvironment>,
}

/// The sky of an environment light (ADR-0151 decision 9): a gradient, or a source's
/// picture as an equirectangular panorama.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownEnvironment {
    /// The sky straight up, straight sRGB, 0 to 1.
    pub sky: [f32; 3],
    /// The sky at the horizon.
    pub horizon: [f32; 3],
    /// Below the horizon.
    pub ground: [f32; 3],
    /// The source whose picture is the sky instead, when one is set.
    pub image: Option<SourceId>,
}

/// The `light` kind.
pub(crate) struct LightKind(KindDescriptor);

impl LightKind {
    pub(crate) fn new() -> Self {
        let number = |min: f64, max: f64, step: f64, unit: Option<&str>| ParamType::Float {
            min: Some(min),
            max: Some(max),
            step: Some(step),
            unit: unit.map(Into::into),
        };
        let p = |id: &str, label: &str, ty: ParamType, default, text: &str| ParamDescriptor {
            live: true,
            description: Some(text.into()),
            ..param(id, label, ty, default)
        };
        let sky = |d: ParamDescriptor| ParamDescriptor {
            visible_if: Some(VisibleIf {
                param: "type".into(),
                equals: json!("environment"),
            }),
            ..d
        };
        let spot = |d: ParamDescriptor| ParamDescriptor {
            visible_if: Some(VisibleIf {
                param: "type".into(),
                equals: json!("spot"),
            }),
            ..d
        };
        Self(KindDescriptor {
            params: vec![
                p(
                    "type",
                    "Type",
                    ParamType::Enum {
                        options: TYPES
                            .iter()
                            .map(|(_, value, label)| EnumOption {
                                value: (*value).into(),
                                label: (*label).into(),
                            })
                            .collect(),
                    },
                    json!("point"),
                    "Point shines every way from the item's centre; spot shines out of the \
                     back of the item within its cone; directional shines out of its back \
                     from infinitely far; ambient lights everything evenly; environment is \
                     the sky all around the scene, which scene-lit models take their \
                     ambient light and reflections from (only the first in a scene \
                     counts), its item's turn spinning it.",
                ),
                p(
                    "color",
                    "Color",
                    ParamType::Color { alpha: false },
                    json!("#ffffff"),
                    "The light's colour.",
                ),
                p(
                    "intensity",
                    "Intensity",
                    number(0.0, MAX_INTENSITY, 0.05, None),
                    json!(1.0),
                    "How bright it is on a surface facing it, before it fades; the item's \
                     opacity scales it, so fading or hiding the item dims it.",
                ),
                p(
                    "range_px",
                    "Range",
                    number(0.0, MAX_RANGE_PX, 10.0, Some("px")),
                    json!(0.0),
                    "How far a point or spot light reaches before it has faded to \
                     nothing; 0 never fades.",
                ),
                spot(p(
                    "cone_deg",
                    "Cone",
                    number(1.0, 170.0, 1.0, Some("deg")),
                    json!(45.0),
                    "A spot light's full cone angle.",
                )),
                spot(p(
                    "softness",
                    "Softness",
                    number(0.0, 1.0, 0.05, None),
                    json!(0.2),
                    "The share of a spot cone, from its edge inward, over which it \
                         fades out.",
                )),
                p(
                    "shadows",
                    "Shadows",
                    ParamType::Bool,
                    json!(false),
                    "A spot or directional light casts shadows from the scene's models and \
                     cards onto its scene-lit models. The first two in a scene do.",
                ),
                p(
                    "shadow_softness",
                    "Shadow softness",
                    number(0.0, 1.0, 0.05, None),
                    json!(0.3),
                    "How soft its shadows' edges are, 0 hard.",
                ),
                sky(p(
                    "sky_color",
                    "Sky",
                    ParamType::Color { alpha: false },
                    json!(SKY),
                    "An environment's sky straight up.",
                )),
                sky(p(
                    "horizon_color",
                    "Horizon",
                    ParamType::Color { alpha: false },
                    json!(HORIZON),
                    "An environment's sky at the horizon.",
                )),
                sky(p(
                    "ground_color",
                    "Ground",
                    ParamType::Color { alpha: false },
                    json!(GROUND),
                    "An environment's ground, below the horizon.",
                )),
                sky(p(
                    "image",
                    "Image",
                    ParamType::Source {
                        media: Some(Media::Video),
                    },
                    json!(null),
                    "A source whose picture is the sky instead of the colours, as an \
                     equirectangular panorama: an HDRI still, or any live source.",
                )),
            ],
            ..kind("light", "Light", true, false)
        })
    }
}

impl SourceKind for LightKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        Ok(Box::new(LightSource {
            light: shown(settings)?,
            frame: Arc::new(rgba_frame(
                FrameSize::new(1, 1)?,
                vec![0; 4],
                MediaTime::zero(Timebase::MILLIS),
            )?),
        }))
    }
}

/// A running `light` source: its settings, and a transparent pixel for its frame.
struct LightSource {
    light: ShownLight,
    frame: Arc<VideoFrame>,
}

impl Source for LightSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        self.light = shown(settings)?;
        Ok(())
    }

    fn light(&self) -> Option<ShownLight> {
        Some(self.light.clone())
    }
}

/// The light `settings` describe.
fn shown(settings: &Settings) -> Result<ShownLight, SourceError> {
    let bad = |param: &str, reason: String| SourceError::Setting {
        param: param.into(),
        reason,
    };
    let name = setting(settings, "type")?.as_str().unwrap_or_default();
    let kind = TYPES
        .iter()
        .find(|(_, value, _)| *value == name)
        .map(|(kind, _, _)| *kind)
        .ok_or_else(|| {
            bad(
                "type",
                "must be `point`, `spot`, `directional`, `ambient` or `environment`".into(),
            )
        })?;
    let rgba = setting(settings, "color")?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad("color", "must be a color as `#rrggbb`".into()))?;
    let number = |id: &str, min: f64, max: f64| {
        setting(settings, id)?
            .as_f64()
            .filter(|v| (min..=max).contains(v))
            .map(|v| v as f32)
            .ok_or_else(|| bad(id, format!("must be a number from {min} to {max}")))
    };
    Ok(ShownLight {
        kind,
        color: [rgba[0], rgba[1], rgba[2]].map(|c| f32::from(c) / 255.0),
        intensity: number("intensity", 0.0, MAX_INTENSITY)?,
        range_px: number("range_px", 0.0, MAX_RANGE_PX)?,
        cone_deg: number("cone_deg", 1.0, 170.0)?,
        softness: number("softness", 0.0, 1.0)?,
        // Settings saved before shadows had none: no shadows.
        shadows: match settings.get("shadows") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| bad("shadows", "must be true or false".into()))?,
        },
        shadow_softness: if settings.get("shadow_softness").is_some() {
            number("shadow_softness", 0.0, 1.0)?
        } else {
            0.3
        },
        environment: if kind == LightType::Environment {
            Some(environment(settings)?)
        } else {
            None
        },
    })
}

/// An environment light's sky from `settings`: settings saved before it had one read as
/// its defaults.
fn environment(settings: &Settings) -> Result<ShownEnvironment, SourceError> {
    let colour = |id: &str, default: &str| -> Result<[f32; 3], SourceError> {
        let text = match settings.get(id) {
            None => default,
            Some(value) => value.as_str().unwrap_or_default(),
        };
        let rgba = parse_color(text).ok_or_else(|| SourceError::Setting {
            param: id.into(),
            reason: "must be a color as `#rrggbb`".into(),
        })?;
        Ok([rgba[0], rgba[1], rgba[2]].map(|c| f32::from(c) / 255.0))
    };
    let image = match settings.get("image") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(id)) if !id.is_empty() => Some(SourceId::new(id.clone())),
        Some(serde_json::Value::String(_)) => None,
        Some(_) => {
            return Err(SourceError::Setting {
                param: "image".into(),
                reason: "must be a source's ID, or null".into(),
            });
        }
    };
    Ok(ShownEnvironment {
        sky: colour("sky_color", SKY)?,
        horizon: colour("horizon_color", HORIZON)?,
        ground: colour("ground_color", GROUND)?,
        image,
    })
}

#[cfg(test)]
mod tests;
