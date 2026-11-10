//! `volumetric_fog` (ADR-0151 decision 6): fog with height that drifts and is lit by the
//! scene's lights. Each pixel marches its viewing ray from `start_px` to the depth there
//! (or `max_px` where nothing was drawn), in steps jittered per pixel and per frame,
//! taking density from a falloff above `base_y_px` and moving noise, and at each step
//! the fog's own colour plus the light of each scene light reaching it, with a
//! forward-scattering phase. The light it scatters toward the camera is added over the
//! picture times what the fog lets through. A light that casts shadows reaches a step
//! only where its shadow map sees it (ADR-0151 decision 3), so what stands in a beam
//! cuts a shaft of shadow through it. Without a view depth and its camera (a 2D
//! scene, any chain but a scene's) it passes its input through.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::ViewDepth;
use crate::error::RenderError;
use crate::filter::multipass::copy;
use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, Ctx, FilterClock, Pass, Shader};
use crate::image::Image;
use crate::model::{SceneLighting, ShadowMap, shadow_view};

pub(super) const KIND: &str = "volumetric_fog";

const SHADER: Shader = Shader {
    label: "filter-volumetric-fog",
    source: include_str!("volumetric/fog.wgsl"),
    blend: Blend::Replace,
};

/// The shadow maps the shader reads, as a `scene`-lit model does.
const MAX_SHADOWS: usize = crate::MAX_SHADOW_MAPS;

/// The farthest a distance may be set, in scene pixels: `MAX_DRAWN_PX`.
const MAX_DISTANCE: f32 = jstream_protocol::MAX_DRAWN_PX;

/// The scene lights the shader takes, as a `scene`-lit model does.
const MAX_LIGHTS: usize = crate::MAX_SCENE_LIGHTS;

/// The noise repeats every this many of its features, so its drift can be wrapped.
const NOISE_PERIOD: f64 = 256.0;

/// The most a setting in scene pixels may be, other than the distances.
const MAX_PX: f64 = 100_000.0;

pub(super) fn descriptor() -> KindDescriptor {
    let far = f64::from(MAX_DISTANCE);
    filter(
        KIND,
        "Volumetric fog",
        vec![
            ParamDescriptor {
                description: Some(
                    "Fog with height that drifts and is lit by the scene's lights: a spot \
                     light through it shows its beam, and what stands in the beam of a light \
                     that casts shadows cuts a shaft of shadow through it. It works in a scene's filters when \
                     the scene has a camera, best first in the chain; anywhere else it \
                     changes nothing. The fog's own colour, the light it scatters from \
                     everywhere; its alpha thins it."
                        .into(),
                ),
                ..color("color", "Color", true, "#c8ccd2ff")
            },
            ParamDescriptor {
                description: Some("Fog per 1000 px at the base height.".into()),
                ..float("density", "Density", 0.0, 20.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "The height where fog is densest, in canvas pixels (y down): 1080 is \
                     the bottom of a 1080p canvas. Below it the fog stays as dense."
                        .into(),
                ),
                ..float("base_y_px", "Base height", -MAX_PX, MAX_PX, 1080.0, "px")
            },
            ParamDescriptor {
                description: Some(
                    "Height above the base over which the density halves; 0 makes it even.".into(),
                ),
                ..float("falloff_px", "Falloff", 0.0, MAX_PX, 400.0, "px")
            },
            ParamDescriptor {
                description: Some(
                    "Distance along the camera's view where fog begins, in scene pixels.".into(),
                ),
                ..float("start_px", "Start", 0.0, far, 0.0, "px")
            },
            ParamDescriptor {
                description: Some(
                    "How far a ray marches where nothing was drawn, in scene pixels.".into(),
                ),
                ..float("max_px", "Farthest", 0.0, far, 8000.0, "px")
            },
            ParamDescriptor {
                description: Some("How much moving noise varies the density.".into()),
                ..float("noise", "Noise", 0.0, 1.0, 0.3, "")
            },
            ParamDescriptor {
                description: Some("The size of the noise's features.".into()),
                ..float("noise_scale_px", "Noise size", 1.0, MAX_PX, 300.0, "px")
            },
            ParamDescriptor {
                description: Some("How far the noise drifts right per second.".into()),
                ..float("wind_x_px", "Wind X", -MAX_PX, MAX_PX, 20.0, "px")
            },
            ParamDescriptor {
                description: Some("How far the noise drifts down per second.".into()),
                ..float("wind_y_px", "Wind Y", -MAX_PX, MAX_PX, 0.0, "px")
            },
            ParamDescriptor {
                description: Some("How far the noise drifts toward the viewer per second.".into()),
                ..float("wind_z_px", "Wind Z", -MAX_PX, MAX_PX, 0.0, "px")
            },
            ParamDescriptor {
                description: Some("How strongly the scene's lights light the fog.".into()),
                ..float("scattering", "Scattering", 0.0, 8.0, 1.0, "")
            },
            ParamDescriptor {
                description: Some(
                    "Steps along each ray: Low 24, Medium 48, High 96. More is smoother and \
                     costs more."
                        .into(),
                ),
                ..choice(
                    "quality",
                    "Quality",
                    &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
                    "medium",
                )
            },
        ],
    )
}

/// The shader's values, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    /// Straight linear colour.
    color: [f32; 4],
    density: f32,
    base_y: f32,
    falloff: f32,
    start: f32,
    farthest: f32,
    noise: f32,
    noise_scale: f32,
    wind: [f32; 3],
    scattering: f32,
    steps: u32,
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
    let far = MAX_DISTANCE;
    let px = MAX_PX as f32;
    Values {
        color: read.color_linear("color", [0.578, 0.604, 0.644, 1.0]),
        density: bounded(read.f32("density", 0.5), 0.0, 20.0, 0.5),
        base_y: bounded(read.f32("base_y_px", 1080.0), -px, px, 1080.0),
        falloff: bounded(read.f32("falloff_px", 400.0), 0.0, px, 400.0),
        start: bounded(read.f32("start_px", 0.0), 0.0, far, 0.0),
        farthest: bounded(read.f32("max_px", 8000.0), 0.0, far, 8000.0),
        noise: bounded(read.f32("noise", 0.3), 0.0, 1.0, 0.3),
        noise_scale: bounded(read.f32("noise_scale_px", 300.0), 1.0, px, 300.0),
        wind: [
            bounded(read.f32("wind_x_px", 20.0), -px, px, 20.0),
            bounded(read.f32("wind_y_px", 0.0), -px, px, 0.0),
            bounded(read.f32("wind_z_px", 0.0), -px, px, 0.0),
        ],
        scattering: bounded(read.f32("scattering", 1.0), 0.0, 8.0, 1.0),
        steps: match read.choice("quality", "medium") {
            "low" => 24,
            "high" => 96,
            _ => 48,
        },
    }
}

/// How far the noise has drifted at `clock`, in its own features, wrapped to its period
/// in `f64` so a long run keeps its precision.
fn drift(v: &Values, clock: FilterClock) -> [f32; 3] {
    let seconds = if clock.seconds.is_finite() {
        clock.seconds
    } else {
        0.0
    };
    v.wind.map(|w| {
        ((f64::from(w) * seconds / f64::from(v.noise_scale)).rem_euclid(NOISE_PERIOD)) as f32
    })
}

/// A per-frame seed for the steps' jitter, from the clock's frame-sized slices.
fn seed(clock: FilterClock) -> f32 {
    let frame = (clock.seconds * 60.0).floor();
    if frame.is_finite() {
        (frame.rem_euclid(64.0) as f32) * 0.618_034
    } else {
        0.0
    }
}

fn uniforms(v: &Values, depth: &ViewDepth, clock: FilterClock) -> Option<Vec<u8>> {
    let view = depth.view?;
    let [right, down, forward] = view.axes().map(|axis| axis.map(|c| c as f32));
    let position = view.position().map(|c| c as f32);
    let [cx, cy] = view.principal();
    let lighting = SceneLighting::new(&depth.lights, &view);
    let wind = drift(v, clock);
    let lights = lighting.lights.len().min(MAX_LIGHTS);
    let mut out = Uniforms::new()
        .vec4(v.color)
        .vec4([right[0], right[1], right[2], 0.0])
        .vec4([down[0], down[1], down[2], 0.0])
        .vec4([forward[0], forward[1], forward[2], 0.0])
        .vec4([position[0], position[1], position[2], depth.focal_px])
        .vec4([cx, cy, v.start, v.farthest])
        .vec4([v.density / 1000.0, v.base_y, v.falloff, v.scattering])
        .vec4([v.noise, 1.0 / v.noise_scale, seed(clock), 0.0])
        .vec4([wind[0], wind[1], wind[2], 0.0])
        .u32(v.steps)
        .u32(lights as u32);
    let block = |out: Uniforms, values: [f32; 16]| {
        (0..4).fold(out, |out, at| {
            out.vec4([
                values[at * 4],
                values[at * 4 + 1],
                values[at * 4 + 2],
                values[at * 4 + 3],
            ])
        })
    };
    for slot in 0..MAX_LIGHTS {
        out = block(out, lighting.lights.get(slot).copied().unwrap_or([0.0; 16]));
    }
    // An unused map has a negative slot, as `model.wgsl` reads it.
    let mut unused = [0.0; 16];
    unused[3] = -1.0;
    for slot in 0..MAX_SHADOWS {
        let packed = depth.shadows.get(slot).and_then(|shadow| {
            shadow_view(
                &ShadowMap {
                    light: shadow.light,
                    image: &shadow.image,
                    camera: shadow.camera,
                    softness: 0.0,
                },
                &lighting,
                &view,
            )
        });
        out = block(out, packed.unwrap_or(unused));
    }
    Some(out.finish())
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

pub(super) struct VolumetricFog {
    values: Values,
    depth: Option<ViewDepth>,
}

impl VolumetricFog {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
            depth: None,
        }
    }
}

impl Pass for VolumetricFog {
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
        // with the picture, and with no camera there is no ray: pass through.
        let depth = self.depth.as_ref().filter(|d| {
            (d.image.width(), d.image.height()) == (input.width(), input.height())
                && d.focal_px.is_finite()
                && d.focal_px > 0.0
        });
        let params = depth.and_then(|d| uniforms(&self.values, d, ctx.clock()));
        let (Some(depth), Some(params)) = (depth, params) else {
            return copy(ctx, input, output);
        };
        // The input, the depth and two maps; a missing map is the depth again, never
        // read, since its slot is unused.
        let map = |slot: usize| {
            depth
                .shadows
                .get(slot)
                .map_or_else(|| depth.image.as_ref().clone(), |s| s.image.clone())
        };
        let inputs = [input.clone(), depth.image.as_ref().clone(), map(0), map(1)];
        let system = system(input, &inputs);
        ctx.draw_program(SHADER.into(), &inputs, &system, &params, output)
    }
}

#[cfg(test)]
mod tests;
