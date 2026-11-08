//! `depth_fog` (ADR-0143 decision 6): fog that thickens with distance from a scene's
//! camera, read from the scene's view depth. Linear between `start_px` and `end_px`, or
//! exponential past `start_px` by `density` per 1000 px, never above `max`, and filling
//! where nothing was drawn when `background` is set. Without a view depth (a 2D scene,
//! any chain but a scene's) it passes its input through.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::ViewDepth;
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, boolean, choice, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "depth_fog";

const SHADER: Shader = Shader {
    label: "filter-depth-fog",
    source: include_str!("fog.wgsl"),
    blend: Blend::Replace,
};

/// The farthest a fog distance may be set, in scene pixels: `MAX_DRAWN_PX`.
const MAX_DISTANCE: f64 = jstream_protocol::MAX_DRAWN_PX as f64;

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Depth fog",
        vec![
            ParamDescriptor {
                description: Some(
                    "Fog that thickens with distance from the scene's camera. It works in a \
                     scene's filters when the scene has a camera, best first in the chain; \
                     anywhere else it changes nothing. Linear fades in from Start to End; \
                     Exponential thickens past Start by Density."
                        .into(),
                ),
                ..choice(
                    "mode",
                    "Mode",
                    &[("exponential", "Exponential"), ("linear", "Linear")],
                    "exponential",
                )
            },
            color("color", "Color", true, "#c8ccd2ff"),
            ParamDescriptor {
                description: Some(
                    "Distance along the camera's view where fog begins, in scene pixels.".into(),
                ),
                ..float("start_px", "Start", 0.0, MAX_DISTANCE, 0.0, "px")
            },
            ParamDescriptor {
                description: Some(
                    "Linear: the distance where fog is at its most. At or before Start, fog \
                     goes straight to its most at Start."
                        .into(),
                ),
                ..float("end_px", "End", 0.0, MAX_DISTANCE, 4000.0, "px")
            },
            ParamDescriptor {
                description: Some(
                    "Exponential: how quickly fog thickens, per 1000 px past Start.".into(),
                ),
                ..float("density", "Density", 0.0, 20.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some("The most fog anywhere: 1 hides what is far entirely.".into()),
                ..float("max", "Most", 0.0, 1.0, 1.0, "")
            },
            ParamDescriptor {
                description: Some(
                    "Fill where nothing was drawn, as if infinitely far, with the most fog.".into(),
                ),
                ..boolean("background", "Fog the background", true)
            },
        ],
    )
}

/// The shader's values, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    /// Straight linear colour.
    color: [f32; 4],
    start: f32,
    end: f32,
    density: f32,
    most: f32,
    exponential: bool,
    background: bool,
}

/// `v` clamped to `min..=max`, `fallback` when it isn't finite.
fn bounded(v: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        fallback
    }
}

fn values(settings: &Settings) -> Values {
    let read = Read(settings);
    let far = MAX_DISTANCE as f32;
    Values {
        color: read.color_linear("color", [0.578, 0.604, 0.644, 1.0]),
        start: bounded(read.f32("start_px", 0.0), 0.0, far, 0.0),
        end: bounded(read.f32("end_px", 4000.0), 0.0, far, 4000.0),
        density: bounded(read.f32("density", 0.5), 0.0, 20.0, 0.5),
        most: bounded(read.f32("max", 1.0), 0.0, 1.0, 1.0),
        exponential: read.choice("mode", "exponential") != "linear",
        background: read.bool("background", true),
    }
}

fn uniforms(v: &Values, depth: Option<&ViewDepth>) -> Vec<u8> {
    Uniforms::new()
        .vec4(v.color)
        .f32(v.start)
        .f32(v.end)
        .f32(v.density)
        .f32(v.most)
        .f32(depth.map_or(0.0, |d| d.focal_px))
        .u32(u32::from(v.exponential))
        .u32(u32::from(v.background))
        .u32(u32::from(depth.is_some()))
        .finish()
}

pub(super) struct DepthFog {
    values: Values,
    depth: Option<ViewDepth>,
}

impl DepthFog {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
            depth: None,
        }
    }
}

impl Pass for DepthFog {
    fn update(&mut self, settings: &Settings) {
        self.values = values(settings);
    }

    fn set_depth(&mut self, depth: Option<ViewDepth>) {
        self.depth = depth;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        // A depth of another size (a stale one across a canvas resize) doesn't line up
        // with the picture: pass through for that frame.
        let depth = self.depth.as_ref().filter(|d| {
            (d.image.width(), d.image.height()) == (input.width(), input.height())
                && d.focal_px.is_finite()
                && d.focal_px > 0.0
        });
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux: depth.map(|d| d.image.as_ref()),
            params: &uniforms(&self.values, depth),
            target: output,
        })
    }
}

#[cfg(test)]
mod tests;
