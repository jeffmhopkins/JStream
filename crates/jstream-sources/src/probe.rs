//! The `reflection_probe` source (ADR-0170 decision 3): an invisible box the item that
//! places it puts in a 3D scene, centred where the item is. The engine draws the scene
//! from the box's centre into a panorama, and a `scene`-lit model or lit card inside the
//! box reflects that capture in place of the environment, in a scene whose
//! `reflections` option is on. It draws nothing itself.

use crate::color::rgba_frame;
use crate::describe::{kind, param};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    EnumOption, KindDescriptor, PROBE_RESOLUTIONS, ParamDescriptor, ParamType, Settings,
};
use serde_json::json;
use std::sync::Arc;

/// The largest a probe's box is along an axis, in scene pixels.
const MAX_SIZE_PX: f64 = 100_000.0;

/// The most frames a probe waits between captures.
pub const MAX_REFRESH_FRAMES: u32 = 600;

/// A `reflection_probe` source's settings ([`Source::probe`]): the box it captures for,
/// before its centre, which the item that places it gives.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownProbe {
    /// The box's full size along the scene's x, y and z, in scene pixels.
    pub size_px: [f32; 3],
    /// How far inside its faces a surface takes all of the probe: from 0 at a face to 1
    /// this far in, in scene pixels. 0 takes it all at once.
    pub fade_px: f32,
    /// How many pixels a side each face is captured at, one of [`PROBE_RESOLUTIONS`],
    /// before the app's cap.
    pub resolution: u32,
    /// Captures again every this many frames; 0 captures once, when it is first drawn
    /// or its settings change.
    pub refresh_frames: u32,
}

/// The `reflection_probe` kind.
pub(crate) struct ProbeKind(KindDescriptor);

impl ProbeKind {
    pub(crate) fn new() -> Self {
        let number = |min: f64, max: f64, step: f64| ParamType::Float {
            min: Some(min),
            max: Some(max),
            step: Some(step),
            unit: Some("px".into()),
        };
        let p = |id: &str, label: &str, ty: ParamType, default, text: &str| ParamDescriptor {
            live: true,
            description: Some(text.into()),
            ..param(id, label, ty, default)
        };
        let size = |id: &str, label: &str, axis: &str| {
            p(
                id,
                label,
                number(1.0, MAX_SIZE_PX, 10.0),
                json!(2000.0),
                &format!(
                    "The box's size along the scene's {axis} axis. A surface inside the \
                     box reflects what the probe sees."
                ),
            )
        };
        Self(KindDescriptor {
            params: vec![
                size("width_px", "Width", "x"),
                size("height_px", "Height", "y"),
                size("depth_px", "Depth", "z"),
                p(
                    "fade_px",
                    "Edge fade",
                    number(0.0, MAX_SIZE_PX, 10.0),
                    json!(100.0),
                    "How far inside the box's faces its reflection fades in, so a \
                     surface moving out of the box loses it gradually; 0 cuts at the \
                     faces.",
                ),
                p(
                    "resolution",
                    "Resolution",
                    ParamType::Enum {
                        options: PROBE_RESOLUTIONS
                            .iter()
                            .map(|size| EnumOption {
                                value: size.to_string(),
                                label: format!("{size} px"),
                            })
                            .collect(),
                    },
                    json!("256"),
                    "How many pixels a side each of the six views the probe captures is. \
                     Larger is sharper and costs more; the app's 3D rendering settings \
                     cap it.",
                ),
                p(
                    "refresh_frames",
                    "Refresh every",
                    ParamType::Int {
                        min: Some(0),
                        max: Some(MAX_REFRESH_FRAMES.into()),
                        step: None,
                        unit: Some("frames".into()),
                    },
                    json!(0),
                    "How often the probe captures its scene again, in frames; 0 captures \
                     once, when it is first drawn or a setting changes. Each capture \
                     draws the scene six times, so a probe that sees live video wants a \
                     rate, and a slow machine a long one.",
                ),
            ],
            ..kind("reflection_probe", "Reflection probe", true, false)
        })
    }
}

impl SourceKind for ProbeKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        Ok(Box::new(ProbeSource {
            probe: shown(settings)?,
            frame: Arc::new(rgba_frame(
                FrameSize::new(1, 1)?,
                vec![0; 4],
                MediaTime::zero(Timebase::MILLIS),
            )?),
        }))
    }
}

/// A running `reflection_probe` source: its settings, and a transparent pixel for its
/// frame.
struct ProbeSource {
    probe: ShownProbe,
    frame: Arc<VideoFrame>,
}

impl Source for ProbeSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        self.probe = shown(settings)?;
        Ok(())
    }

    fn probe(&self) -> Option<ShownProbe> {
        Some(self.probe.clone())
    }
}

fn shown(settings: &Settings) -> Result<ShownProbe, SourceError> {
    let bad = |param: &str, reason: String| SourceError::Setting {
        param: param.into(),
        reason,
    };
    let number = |id: &str, min: f64, max: f64| {
        setting(settings, id)?
            .as_f64()
            .filter(|v| (min..=max).contains(v))
            .map(|v| v as f32)
            .ok_or_else(|| bad(id, format!("must be a number from {min} to {max}")))
    };
    let resolution = setting(settings, "resolution")?
        .as_str()
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|size| PROBE_RESOLUTIONS.contains(size))
        .ok_or_else(|| bad("resolution", "must be `128`, `256` or `512`".into()))?;
    let refresh_frames = setting(settings, "refresh_frames")?
        .as_u64()
        .filter(|frames| *frames <= u64::from(MAX_REFRESH_FRAMES))
        .ok_or_else(|| {
            bad(
                "refresh_frames",
                format!("must be a whole number from 0 to {MAX_REFRESH_FRAMES}"),
            )
        })?;
    Ok(ShownProbe {
        size_px: [
            number("width_px", 1.0, MAX_SIZE_PX)?,
            number("height_px", 1.0, MAX_SIZE_PX)?,
            number("depth_px", 1.0, MAX_SIZE_PX)?,
        ],
        fade_px: number("fade_px", 0.0, MAX_SIZE_PX)?,
        resolution,
        refresh_frames: refresh_frames as u32,
    })
}

#[cfg(test)]
mod tests;
