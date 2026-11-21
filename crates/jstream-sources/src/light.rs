//! The `light` source (ADR-0143 decision 1): a light the item that places it puts in a
//! 3D scene, for the models there that are lit by their scene. It draws nothing itself.
//! An `environment` light (ADR-0151 decision 9) is the sky around the whole scene, which
//! a 3D scene can also draw behind its items as a backdrop (ADR-0167).

use crate::color::{parse_color, rgba_frame};
use crate::describe::{kind, param};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    EnumOption, KindDescriptor, Media, ParamDescriptor, ParamType, Reference, Settings, SourceId,
    VisibleIf,
};
use serde_json::json;
use std::sync::Arc;

/// The most irradiance a light gives a surface facing it.
const MAX_INTENSITY: f64 = 64.0;

/// An environment's default sky straight up, at the horizon, and below it.
const SKY: &str = "#9cc2e8";
const HORIZON: &str = "#ffffff";
const GROUND: &str = "#3a3a3a";

/// The most an environment backdrop's stars are brightened (ADR-0167).
const MAX_STAR_BRIGHTNESS: f32 = 8.0;

/// A backdrop's default nebula colours, a violet and a blue.
const NEBULA: &str = "#8c2e9e";
const NEBULA_2: &str = "#2471a8";

/// The longest a backdrop's warp takes to change from still to full, in seconds.
const MAX_WARP_RAMP_S: f64 = 10.0;

/// The highest sky seed.
const MAX_SEED: u32 = 9999;

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
    /// Whether a 3D scene draws the sky behind its items, infinitely far away (ADR-0167).
    pub backdrop: bool,
    /// How many stars the backdrop scatters over the sky, 0 (none) to 1.
    pub stars: f32,
    /// How bright the backdrop's stars are, 0 to 8.
    pub star_brightness: f32,
    /// How much nebula the backdrop draws, 0 (none) to 1.
    pub nebula: f32,
    /// The nebulae's two colours, straight sRGB, 0 to 1.
    pub nebula_colors: [[f32; 3]; 2],
    /// How bright the backdrop's galaxy band is, 0 (none) to 1.
    pub galaxy: f32,
    /// Which sky the stars, nebulae and band lay out, 0 to 9999.
    pub sky_seed: u32,
    /// How fast the backdrop streams past toward the sky's centre ahead, 0 (still) to 1.
    pub warp: f32,
    /// How many seconds the backdrop takes to go from still to full warp, or back, when
    /// `warp` changes, 0 to 10; 0 changes at once.
    pub warp_ramp_s: f32,
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
                    "The light's colour. An environment ignores it: its sky colours or \
                     its image are its colour.",
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
                     equirectangular panorama: an HDRI still, or any live source. A \
                     display, window or camera capture lights the sky only while an item \
                     also shows it.",
                )),
                sky(p(
                    "backdrop",
                    "Draw as backdrop",
                    ParamType::Bool,
                    json!(false),
                    "Draw the sky behind everything in a 3D scene, infinitely far away: it \
                     turns as the camera looks around but never moves as the camera moves. \
                     Its intensity sets its brightness and the item's opacity fades it over \
                     the scene's background. A scene without a camera draws no backdrop.",
                )),
                sky(p(
                    "stars",
                    "Stars",
                    number(0.0, 1.0, 0.05, None),
                    json!(0.0),
                    "How many stars the backdrop scatters over the sky, fixed in it, \
                     each a point a pixel or two wide at any resolution; 0 draws none. \
                     Stars show in the backdrop only, not in what the sky lights.",
                )),
                sky(p(
                    "star_brightness",
                    "Star brightness",
                    number(0.0, f64::from(MAX_STAR_BRIGHTNESS), 0.05, None),
                    json!(1.0),
                    "How bright the backdrop's stars are, before the intensity.",
                )),
                sky(p(
                    "nebula",
                    "Nebula",
                    number(0.0, 1.0, 0.05, None),
                    json!(0.0),
                    "How much glowing gas cloud the backdrop draws over the sky, in the \
                     two nebula colours; 0 draws none.",
                )),
                sky(p(
                    "nebula_color",
                    "Nebula colour",
                    ParamType::Color { alpha: false },
                    json!(NEBULA),
                    "The nebulae's first colour.",
                )),
                sky(p(
                    "nebula_color_2",
                    "Nebula colour 2",
                    ParamType::Color { alpha: false },
                    json!(NEBULA_2),
                    "The nebulae's second colour, which they shade into.",
                )),
                sky(p(
                    "galaxy",
                    "Galaxy band",
                    number(0.0, 1.0, 0.05, None),
                    json!(0.0),
                    "How bright a band of galaxy light across the backdrop is, with dark \
                     dust lanes and more stars along it; 0 draws none.",
                )),
                sky(p(
                    "sky_seed",
                    "Sky seed",
                    ParamType::Int {
                        min: Some(0),
                        max: Some(MAX_SEED.into()),
                        step: None,
                        unit: None,
                    },
                    json!(0),
                    "Which sky: each number lays the stars, nebulae and band out anew, and picks the \
                     nebulae's kind: their size, how much sky they fill, how turbulent, \
                     wispy and dusty they are, and whether they gather in a few regions.",
                )),
                sky(p(
                    "warp",
                    "Warp",
                    number(0.0, 1.0, 0.01, None),
                    json!(0.0),
                    "How fast the backdrop streams past, as if travelling toward the \
                     sky's centre ahead (the item's turn sets where that is): the stars \
                     move away from it and stretch into streaks, at any angle the camera \
                     looks, crowding ahead in bluer, brighter light and thinning behind \
                     in redder, dimmer light over a blue glow ahead, while the nebulae \
                     and band fade out; 0 is still. A change ramps over the warp ramp, so setting \
                     it jumps in and setting 0 drops out.",
                )),
                sky(p(
                    "warp_ramp_s",
                    "Warp ramp",
                    number(0.0, MAX_WARP_RAMP_S, 0.1, Some("s")),
                    json!(1.5),
                    "How long the backdrop takes to speed up from still to full warp, \
                     or slow from full warp to still, when Warp changes; 0 changes at \
                     once.",
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
        // A source reference, as the parameter's type checks it, or a bare ID.
        Some(value @ serde_json::Value::Object(_)) => match Reference::from_value(value) {
            Some(Reference::Source { source_id }) => Some(source_id),
            _ => {
                return Err(SourceError::Setting {
                    param: "image".into(),
                    reason: "must be a source's ID, or null".into(),
                });
            }
        },
        Some(serde_json::Value::String(id)) if !id.is_empty() => Some(SourceId::new(id.clone())),
        Some(serde_json::Value::String(_)) => None,
        Some(_) => {
            return Err(SourceError::Setting {
                param: "image".into(),
                reason: "must be a source's ID, or null".into(),
            });
        }
    };
    // Settings saved before the backdrop had none: no backdrop, no stars.
    let backdrop = match settings.get("backdrop") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| SourceError::Setting {
            param: "backdrop".into(),
            reason: "must be true or false".into(),
        })?,
    };
    let number = |id: &str, max: f32, default: f32| -> Result<f32, SourceError> {
        match settings.get(id) {
            None => Ok(default),
            Some(value) => value
                .as_f64()
                .filter(|v| (0.0..=f64::from(max)).contains(v))
                .map(|v| v as f32)
                .ok_or_else(|| SourceError::Setting {
                    param: id.into(),
                    reason: format!("must be a number from 0 to {max}"),
                }),
        }
    };
    Ok(ShownEnvironment {
        sky: colour("sky_color", SKY)?,
        horizon: colour("horizon_color", HORIZON)?,
        ground: colour("ground_color", GROUND)?,
        image,
        backdrop,
        stars: number("stars", 1.0, 0.0)?,
        star_brightness: number("star_brightness", MAX_STAR_BRIGHTNESS, 1.0)?,
        nebula: number("nebula", 1.0, 0.0)?,
        nebula_colors: [
            colour("nebula_color", NEBULA)?,
            colour("nebula_color_2", NEBULA_2)?,
        ],
        galaxy: number("galaxy", 1.0, 0.0)?,
        sky_seed: match settings.get("sky_seed") {
            None => 0,
            Some(value) => value
                .as_u64()
                .filter(|seed| *seed <= u64::from(MAX_SEED))
                .map(|seed| seed as u32)
                .ok_or_else(|| SourceError::Setting {
                    param: "sky_seed".into(),
                    reason: format!("must be a whole number from 0 to {MAX_SEED}"),
                })?,
        },
        warp: number("warp", 1.0, 0.0)?,
        warp_ramp_s: number("warp_ramp_s", MAX_WARP_RAMP_S as f32, 1.5)?,
    })
}

#[cfg(test)]
mod tests;
