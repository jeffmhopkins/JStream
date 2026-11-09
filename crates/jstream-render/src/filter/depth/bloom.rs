//! `depth_bloom` (ADR-0151 decision 8): bloom whose glow stays with what glows. A bright
//! pass keeps the light above `threshold` (linear luma of the straight colour, with a
//! soft knee), faded out toward `far_px`, at half the picture's size; a separable blur
//! there spreads it over `radius_px`,
//! each tap's weight falling off where the pixel it glows onto is more than `depth_px`
//! nearer than the tap, by their distances in the scene's view depth, so a glow doesn't
//! bleed across something in front of it; and the glow times `strength` is added over
//! the picture. Without a view depth (a 2D scene, any chain but a scene's) it blooms as
//! plain bloom does, with no fade by distance.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::ViewDepth;
use crate::error::RenderError;
use crate::filter::multipass::copy;
use crate::filter::params::{Read, Uniforms, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "depth_bloom";

const BRIGHT: Shader = Shader {
    label: "filter-depth-bloom-bright",
    source: include_str!("bloom/bright.wgsl"),
    blend: Blend::Replace,
};

const BLUR: Shader = Shader {
    label: "filter-depth-bloom-blur",
    source: include_str!("bloom/blur.wgsl"),
    blend: Blend::Replace,
};

const ADD: Shader = Shader {
    label: "filter-depth-bloom-add",
    source: include_str!("bloom/add.wgsl"),
    blend: Blend::Replace,
};

/// The widest glow, in canvas pixels.
const MAX_RADIUS: f32 = 100.0;

/// Half the width of the threshold's soft knee, in linear luma.
const KNEE: f32 = 0.1;

/// The most taps on each side of a pixel in one direction of the blur.
const MAX_TAPS: f32 = 16.0;

/// The farthest a distance may be set, in scene pixels.
const MAX_DISTANCE: f32 = jstream_protocol::MAX_DRAWN_PX;

pub(super) fn descriptor() -> KindDescriptor {
    let far = f64::from(MAX_DISTANCE);
    KindDescriptor {
        max_outset_px: Some(MAX_RADIUS as u32),
        ..filter(
            KIND,
            "Depth bloom",
            vec![
                ParamDescriptor {
                    description: Some(
                        "Bloom whose glow stays with what glows: it doesn't bleed over \
                         something in front of it. It works in a scene's filters when the \
                         scene has a camera; anywhere else it blooms as Bloom does. How \
                         bright, in linear light, a part must be to glow."
                            .into(),
                    ),
                    ..float("threshold", "Threshold", 0.0, 4.0, 0.8, "")
                },
                ParamDescriptor {
                    description: Some("How bright the glow is.".into()),
                    ..float("strength", "Strength", 0.0, 4.0, 1.0, "")
                },
                ParamDescriptor {
                    description: Some("How far the glow spreads.".into()),
                    ..float(
                        "radius_px",
                        "Radius",
                        0.0,
                        f64::from(MAX_RADIUS),
                        24.0,
                        "px",
                    )
                },
                ParamDescriptor {
                    description: Some(
                        "How far in front of a glowing thing a nearer one may be before the \
                         glow stops at it, in scene pixels."
                            .into(),
                    ),
                    ..float("depth_px", "Depth", 1.0, far, 200.0, "px")
                },
                ParamDescriptor {
                    description: Some(
                        "Distance past which glow fades to nothing, in scene pixels; 0 never \
                         fades."
                            .into(),
                    ),
                    ..float("far_px", "Farthest", 0.0, far, 0.0, "px")
                },
            ],
        )
    }
}

/// The shader's values, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    threshold: f32,
    strength: f32,
    radius: f32,
    depth: f32,
    far: f32,
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
        threshold: bounded(read.f32("threshold", 0.8), 0.0, 4.0, 0.8),
        strength: bounded(read.f32("strength", 1.0), 0.0, 4.0, 1.0),
        radius: bounded(read.f32("radius_px", 24.0), 0.0, MAX_RADIUS, 24.0),
        depth: bounded(read.f32("depth_px", 200.0), 1.0, MAX_DISTANCE, 200.0),
        far: bounded(read.f32("far_px", 0.0), 0.0, MAX_DISTANCE, 0.0),
    }
}

/// The glow's reach: as far as it spreads past the bright parts.
pub(super) fn outset(settings: &Settings) -> f32 {
    values(settings).radius
}

/// The blur's Gaussian: its sigma, the distance between taps and the taps on each side.
fn taps(radius: f32) -> (f32, f32, u32) {
    let sigma = (radius * 0.5).max(0.5);
    let reach = radius * 1.5;
    let step = (reach / MAX_TAPS).max(1.0);
    (sigma, step, (reach / step).ceil() as u32)
}

pub(super) struct DepthBloom {
    values: Values,
    depth: Option<ViewDepth>,
}

impl DepthBloom {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
            depth: None,
        }
    }
}

impl Pass for DepthBloom {
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
        let v = self.values;
        if v.strength <= 0.0 {
            return copy(ctx, input, output);
        }
        // A depth of another size doesn't line up with the picture: bloom without it.
        let depth = self.depth.as_ref().filter(|d| {
            (d.image.width(), d.image.height()) == (input.width(), input.height())
                && d.focal_px.is_finite()
                && d.focal_px > 0.0
        });
        // A focal length of 0 tells the shaders there is no depth.
        let (aux, focal) = depth.map_or((input, 0.0), |d| (d.image.as_ref(), d.focal_px));
        let params = Uniforms::new()
            .f32(v.threshold)
            .f32(KNEE)
            .f32(focal)
            .f32(v.far)
            .finish();
        // The glow is blurred at half size: a quarter of the taps' cost.
        let size = (input.width().div_ceil(2), input.height().div_ceil(2));
        let bright = ctx.scratch(size.0, size.1)?;
        ctx.draw(&Draw {
            shader: BRIGHT,
            input,
            aux: Some(aux),
            params: &params,
            target: &bright,
        })?;
        let (sigma, step, count) = taps(v.radius * 0.5);
        let across = ctx.scratch(size.0, size.1)?;
        let down = bright;
        for (dir, from, to) in [([1.0, 0.0], &down, &across), ([0.0, 1.0], &across, &down)] {
            let params = Uniforms::new()
                .vec2(dir)
                .f32(sigma)
                .f32(step)
                .u32(count)
                .f32(focal)
                .f32(v.depth)
                .finish();
            ctx.draw(&Draw {
                shader: BLUR,
                input: from,
                aux: Some(aux),
                params: &params,
                target: to,
            })?;
        }
        let params = Uniforms::new().f32(v.strength).finish();
        ctx.draw(&Draw {
            shader: ADD,
            input,
            aux: Some(&down),
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests;
