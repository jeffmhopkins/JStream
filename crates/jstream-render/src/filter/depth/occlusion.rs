//! `ambient_occlusion` (ADR-0151 decision 5): darkens creases and contact points in a 3D
//! scene, read from its view depth. A first pass rebuilds each pixel's point and normal
//! from the depth and counts the samples of a hemisphere of `radius_px` around it that
//! the depth puts behind something nearer, in a pattern rotated per pixel over a 4 × 4
//! tile; a second blurs that share over the tile, keeping to pixels at about the same
//! distance, and multiplies the picture toward `color` by it times `strength`. Where
//! nothing was drawn, and without a view depth (a 2D scene, any chain but a scene's), it
//! passes its input through.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::ViewDepth;
use crate::error::RenderError;
use crate::filter::multipass::copy;
use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "ambient_occlusion";

/// Counts each pixel's occluded share into red, with its `1 / w` kept in green.
const SAMPLE: Shader = Shader {
    label: "filter-ambient-occlusion-sample",
    source: include_str!("occlusion/sample.wgsl"),
    blend: Blend::Replace,
};

/// Blurs the share over 4 × 4 by distance and lays it over the picture.
const APPLY: Shader = Shader {
    label: "filter-ambient-occlusion-apply",
    source: include_str!("occlusion/apply.wgsl"),
    blend: Blend::Replace,
};

/// The largest radius, in scene pixels.
const MAX_RADIUS: f32 = 2000.0;

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Ambient occlusion",
        vec![
            ParamDescriptor {
                description: Some(
                    "Darkens creases, corners and where things meet, from the scene's depth. \
                     It works in a scene's filters when the scene has a camera, best first in \
                     the chain; anywhere else it changes nothing. How far from a point \
                     something nearer still shades it, in scene pixels."
                        .into(),
                ),
                ..float(
                    "radius_px",
                    "Radius",
                    1.0,
                    f64::from(MAX_RADIUS),
                    48.0,
                    "px",
                )
            },
            ParamDescriptor {
                description: Some("How dark full occlusion is.".into()),
                ..float("strength", "Strength", 0.0, 2.0, 1.0, "")
            },
            ParamDescriptor {
                description: Some("The colour occlusion shades toward.".into()),
                ..color("color", "Color", false, "#000000")
            },
            ParamDescriptor {
                description: Some(
                    "Low takes 8 samples a pixel, High 16: smoother, at twice the cost.".into(),
                ),
                ..choice(
                    "quality",
                    "Quality",
                    &[("low", "Low"), ("high", "High")],
                    "low",
                )
            },
        ],
    )
}

/// The shaders' values, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    radius: f32,
    strength: f32,
    /// Straight linear colour.
    color: [f32; 4],
    samples: u32,
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
    Values {
        radius: bounded(read.f32("radius_px", 48.0), 1.0, MAX_RADIUS, 48.0),
        strength: bounded(read.f32("strength", 1.0), 0.0, 2.0, 1.0),
        color: read.color_linear("color", [0.0, 0.0, 0.0, 1.0]),
        samples: if read.choice("quality", "low") == "high" {
            16
        } else {
            8
        },
    }
}

fn sample_uniforms(v: &Values, depth: &ViewDepth) -> Vec<u8> {
    Uniforms::new()
        .f32(v.radius)
        .f32(depth.focal_px)
        .u32(v.samples)
        .finish()
}

fn apply_uniforms(v: &Values, depth: &ViewDepth) -> Vec<u8> {
    Uniforms::new()
        .vec4(v.color)
        .f32(v.strength)
        .f32(v.radius)
        .f32(depth.focal_px)
        .finish()
}

pub(super) struct AmbientOcclusion {
    values: Values,
    depth: Option<ViewDepth>,
}

impl AmbientOcclusion {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
            depth: None,
        }
    }
}

impl Pass for AmbientOcclusion {
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
        // with the picture, and nothing is shaded at no strength: pass through.
        let depth = self.depth.as_ref().filter(|d| {
            (d.image.width(), d.image.height()) == (input.width(), input.height())
                && d.focal_px.is_finite()
                && d.focal_px > 0.0
                && self.values.strength > 0.0
        });
        let Some(depth) = depth else {
            return copy(ctx, input, output);
        };
        let share = ctx.scratch(input.width(), input.height())?;
        ctx.draw(&Draw {
            shader: SAMPLE,
            input: depth.image.as_ref(),
            aux: None,
            params: &sample_uniforms(&self.values, depth),
            target: &share,
        })?;
        ctx.draw(&Draw {
            shader: APPLY,
            input,
            aux: Some(&share),
            params: &apply_uniforms(&self.values, depth),
            target: output,
        })
    }
}

#[cfg(test)]
mod tests;
