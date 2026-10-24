//! The multi-pass filters (roadmap V2): blur, pixelate, drop shadow, outline, glow and
//! video delay.
//!
//! Docs: crates/jstream-render/README.md#filters
//!
//! Shared pieces: [`gauss`] is the bounded separable blur that blur, drop shadow and
//! glow use; [`area`] is the whole / inside / outside rectangle that blur and pixelate
//! share; [`bounds`] is the room drop shadow and glow blur their shape in; `common.wgsl`
//! holds the WGSL helpers these shaders share.

/// A shader of this group: `common.wgsl`, then the named file.
macro_rules! wgsl {
    ($file:literal) => {
        concat!(include_str!("common.wgsl"), "\n", include_str!($file))
    };
}

mod area;
mod blur;
mod bounds;
mod drop_shadow;
mod gauss;
mod glow;
mod outline;
mod pixelate;
mod video_delay;

/// A `size` x `size` transparent frame with an opaque `colour` square at `at..at + side`
/// on both axes: a shape for the tests of the filters that draw around one.
#[cfg(test)]
fn square(
    opened: &crate::tests::Opened,
    size: u32,
    at: u32,
    side: u32,
    colour: [u8; 4],
) -> crate::image::Image {
    let pixels: Vec<[u8; 4]> = (0..size)
        .flat_map(|y| {
            (0..size).map(move |x| {
                let inside = |v: u32| (at..at + side).contains(&v);
                if inside(x) && inside(y) {
                    colour
                } else {
                    [0, 0, 0, 0]
                }
            })
        })
        .collect();
    crate::filter::testing::image(opened, size, &pixels)
}

use jstream_protocol::{KindDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

const COPY: Shader = Shader {
    label: "multipass-copy",
    source: wgsl!("copy.wgsl"),
    blend: Blend::Replace,
};

/// This group's kinds.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    vec![
        blur::descriptor(),
        pixelate::descriptor(),
        drop_shadow::descriptor(),
        outline::descriptor(),
        glow::descriptor(),
        video_delay::descriptor(),
    ]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    Some(match kind_id {
        blur::KIND => Box::new(blur::Blur::new(settings)),
        pixelate::KIND => Box::new(pixelate::Pixelate::new(settings)),
        drop_shadow::KIND => Box::new(drop_shadow::DropShadow::new(settings)),
        outline::KIND => Box::new(outline::Outline::new(settings)),
        glow::KIND => Box::new(glow::Glow::new(settings)),
        video_delay::KIND => Box::new(video_delay::VideoDelay::new(settings)),
        _ => return None,
    })
}

/// A straight-alpha linear colour, premultiplied, as shaders take it.
fn premultiplied([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

/// Copies `input` texel for texel into `output`, the same size.
fn copy(ctx: &mut Ctx<'_>, input: &Image, output: &Image) -> Result<(), RenderError> {
    ctx.draw(&Draw {
        shader: COPY,
        input,
        aux: None,
        params: &Uniforms::new().finish(),
        target: output,
    })
}
