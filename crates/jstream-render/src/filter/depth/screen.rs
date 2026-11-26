//! `screen_space_reflections` (ADR-0170 decision 4): what a 3D scene's surfaces reflect
//! of what is on screen. The engine puts it first in the video chain of a scene whose
//! `reflections` is on, while the switch for it is on, and draws the scene's surface
//! buffer ([`DepthSurface`]) with its view depth for it; no chain lists it. Of a surface
//! that may have its environment's reflection replaced, one pixel of each block of two
//! by two follows its reflected ray through the view depth in `steps` steps to what the
//! picture shows where it hits (`screen/trace.wgsl`, at half the picture's size:
//! ADR-0189). Each of the surface's pixels then takes what the marched pixels near it on
//! the same surface found in place of that reflection, under its own BRDF
//! (`screen/resolve.wgsl`). Everywhere else, and without a view depth, its camera or a
//! surface buffer, it passes its input through.

use jstream_protocol::{KindDescriptor, Settings};

use super::{DepthSurface, ViewDepth};
use crate::error::RenderError;
use crate::filter::multipass::copy;
use crate::filter::params::{Read, Uniforms, filter};
use crate::filter::{Blend, Ctx, Pass, Shader, program_system};
use crate::image::Image;

/// The kind's ID, which the engine names when it puts the pass in a scene's chain.
pub const KIND: &str = "screen_space_reflections";

/// The march, at half the picture's size: one pixel of each block of two by two.
const TRACE: Shader = Shader {
    label: "filter-screen-space-reflections-trace",
    source: include_str!("screen/trace.wgsl"),
    blend: Blend::Replace,
};

/// What spreads the march over the picture's pixels and adds it to the picture.
const RESOLVE: Shader = Shader {
    label: "filter-screen-space-reflections",
    source: include_str!("screen/resolve.wgsl"),
    blend: Blend::Replace,
};

/// The steps a ray takes without a `steps` setting: the fewest of
/// [`jstream_protocol::SCREEN_SPACE_STEPS`].
const DEFAULT_STEPS: u32 = 16;

/// The kind as the engine describes it to itself: no parameter a person sets but how
/// many steps a ray takes, which the rendering switches choose.
pub(super) fn descriptor() -> KindDescriptor {
    filter(KIND, "Screen-space reflections", Vec::new())
}

/// How many steps a ray takes at `settings`: one of
/// [`jstream_protocol::SCREEN_SPACE_STEPS`], the nearest below when another.
fn steps(settings: &Settings) -> u32 {
    let asked = Read(settings).f32("steps", DEFAULT_STEPS as f32);
    let asked = if asked.is_finite() { asked } else { 0.0 };
    jstream_protocol::SCREEN_SPACE_STEPS
        .iter()
        .copied()
        .rev()
        .find(|&steps| steps as f32 <= asked)
        .unwrap_or(jstream_protocol::SCREEN_SPACE_STEPS[0])
}

fn uniforms(steps: u32, depth: &ViewDepth) -> Option<Vec<u8>> {
    let view = depth.view?;
    let [cx, cy] = view.principal();
    Some(
        Uniforms::new()
            .vec4([depth.focal_px, cx, cy, steps as f32])
            .finish(),
    )
}

pub(super) struct ScreenReflections {
    steps: u32,
    depth: Option<ViewDepth>,
}

impl ScreenReflections {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            steps: steps(settings),
            depth: None,
        }
    }
}

impl Pass for ScreenReflections {
    /// Timed as packs are: the guard draws the 3D extras cheaper when it runs too long
    /// (ADR-0247).
    fn timed(&self) -> Option<&str> {
        Some(KIND)
    }

    fn update(&mut self, settings: &Settings) {
        self.steps = steps(settings);
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
            let surface: &DepthSurface = d.surface.as_ref()?;
            (fits(&d.image)
                && fits(&surface.normal)
                && fits(&surface.brdf)
                && fits(&surface.reflected)
                && d.focal_px.is_finite()
                && d.focal_px > 0.0)
                .then_some((d, surface))
        });
        let params = drawn.and_then(|(d, _)| uniforms(self.steps, d));
        let (Some((depth, surface)), Some(params)) = (drawn, params) else {
            return copy(ctx, input, output);
        };
        let traced = ctx.scratch(size.0.div_ceil(2), size.1.div_ceil(2))?;
        let inputs = [
            input.clone(),
            depth.image.as_ref().clone(),
            surface.normal.clone(),
            surface.brdf.clone(),
            surface.reflected.clone(),
            traced.clone(),
        ];
        let marched = &inputs[..5];
        let sys = program_system(input, &traced, marched);
        ctx.draw_program(TRACE.into(), marched, &sys, &params, &traced)?;
        let sys = program_system(input, output, &inputs);
        ctx.draw_program(RESOLVE.into(), &inputs, &sys, &params, output)
    }
}

#[cfg(test)]
mod tests;
