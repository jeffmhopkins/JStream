//! `floor_reflection` (ADR-0151 decision 4): mirrors a 3D scene in a horizontal floor.
//! The engine draws the scene a second time through its camera mirrored in the plane
//! `y = floor_y_px`, with that picture's own view depth ([`DepthReflection`]). Each pixel
//! whose view depth puts it on the plane takes the mirrored picture where its viewing
//! ray, reflected, meets something above the floor, at `strength`, faded with that
//! thing's height over `fade_px` and blurred over `blur_px`. Everywhere else, and
//! without a view depth, its camera and a reflection (a 2D scene, any chain but a
//! scene's), it passes its input through.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::{DepthReflection, ViewDepth};
use crate::error::RenderError;
use crate::filter::multipass::copy;
use crate::filter::params::{Read, Uniforms, filter, float};
use crate::filter::{Blend, Ctx, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "floor_reflection";

const SHADER: Shader = Shader {
    label: "filter-floor-reflection",
    source: include_str!("reflection/floor.wgsl"),
    blend: Blend::Replace,
};

/// The most a height setting may be, in scene pixels.
const MAX_PX: f64 = 100_000.0;

/// The widest blur, in canvas pixels.
const MAX_BLUR: f64 = 64.0;

/// The floor's height when its setting is missing: the bottom of a 1080p canvas.
pub(super) const DEFAULT_FLOOR: f32 = 1080.0;

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Floor reflection",
        vec![
            ParamDescriptor {
                description: Some(
                    "Mirrors the scene in a level floor, over whatever the scene draws on \
                     it: a plane model or a card lying flat. It works in a scene's filters \
                     when the scene has a camera; anywhere else it changes nothing. The \
                     floor's height, in canvas pixels (y down): 1080 is the bottom of a \
                     1080p canvas."
                        .into(),
                ),
                ..float("floor_y_px", "Floor height", -MAX_PX, MAX_PX, 1080.0, "px")
            },
            ParamDescriptor {
                description: Some("How much of the reflection shows.".into()),
                ..float("strength", "Strength", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "Height above the floor over which a reflected thing fades out; 0 never \
                     fades."
                        .into(),
                ),
                ..float("fade_px", "Fade", 0.0, MAX_PX, 0.0, "px")
            },
            ParamDescriptor {
                description: Some("Blurs the reflection, for a rough floor.".into()),
                ..float("blur_px", "Blur", 0.0, MAX_BLUR, 0.0, "px")
            },
        ],
    )
}

/// `v` clamped to `min..=max`, `fallback` when it isn't finite.
fn bounded(v: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        fallback
    }
}

/// The floor's height a chain's `floor_reflection` with `settings` mirrors in, which
/// the engine draws the reflection for.
pub(super) fn floor(settings: &Settings) -> f32 {
    let px = MAX_PX as f32;
    bounded(
        Read(settings).f32("floor_y_px", DEFAULT_FLOOR),
        -px,
        px,
        DEFAULT_FLOOR,
    )
}

/// The shader's values, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    strength: f32,
    fade: f32,
    blur: f32,
}

fn values(settings: &Settings) -> Values {
    let read = Read(settings);
    Values {
        strength: bounded(read.f32("strength", 0.5), 0.0, 1.0, 0.5),
        fade: bounded(read.f32("fade_px", 0.0), 0.0, MAX_PX as f32, 0.0),
        blur: bounded(read.f32("blur_px", 0.0), 0.0, MAX_BLUR as f32, 0.0),
    }
}

fn uniforms(v: &Values, depth: &ViewDepth, reflection: &DepthReflection) -> Option<Vec<u8>> {
    let view = depth.view?;
    let [right, down, forward] = view.axes().map(|axis| axis.map(|c| c as f32));
    let position = view.position().map(|c| c as f32);
    let [cx, cy] = view.principal();
    Some(
        Uniforms::new()
            .vec4([right[0], right[1], right[2], 0.0])
            .vec4([down[0], down[1], down[2], 0.0])
            .vec4([forward[0], forward[1], forward[2], 0.0])
            .vec4([position[0], position[1], position[2], depth.focal_px])
            .vec4([cx, cy, reflection.floor_y, v.strength])
            .vec4([v.fade, v.blur, 0.0, 0.0])
            .finish(),
    )
}

/// The `sys` uniform of the program prelude the shader is drawn with: the sizes, and
/// how each of its four inputs stores alpha.
fn system(input: &Image, inputs: &[Image; 4]) -> [u8; 96] {
    let mut bytes = [0u8; 96];
    let size = [input.width() as f32, input.height() as f32];
    for (at, value) in [size[0], size[1], size[0], size[1]].iter().enumerate() {
        bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (n, image) in inputs.iter().enumerate() {
        let at = 48 + n * 4;
        bytes[at..at + 4].copy_from_slice(&image.alpha().shader_mode().to_le_bytes());
    }
    bytes
}

pub(super) struct FloorReflection {
    values: Values,
    depth: Option<ViewDepth>,
}

impl FloorReflection {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
            depth: None,
        }
    }
}

impl Pass for FloorReflection {
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
        // Every image must line up with the picture: a stale one across a canvas resize
        // doesn't, and with no camera there is no ray.
        let size = (input.width(), input.height());
        let fits = |image: &Image| (image.width(), image.height()) == size;
        let drawn = self.depth.as_ref().and_then(|d| {
            let reflection = d.reflection.as_ref()?;
            (fits(&d.image)
                && fits(&reflection.image)
                && fits(&reflection.depth)
                && d.focal_px.is_finite()
                && d.focal_px > 0.0)
                .then_some((d, reflection))
        });
        let params = drawn.and_then(|(d, r)| uniforms(&self.values, d, r));
        let (Some((depth, reflection)), Some(params)) = (drawn, params) else {
            return copy(ctx, input, output);
        };
        if self.values.strength <= 0.0 {
            return copy(ctx, input, output);
        }
        let inputs = [
            input.clone(),
            depth.image.as_ref().clone(),
            reflection.image.clone(),
            reflection.depth.clone(),
        ];
        let system = system(input, &inputs);
        ctx.draw_program(SHADER.into(), &inputs, &system, &params, output)
    }
}

#[cfg(test)]
mod tests;
