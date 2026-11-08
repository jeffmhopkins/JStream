//! The multi-pass filters (roadmap V2): blur, pixelate, drop shadow, outline, glow,
//! bloom and video delay.
//!
//! Docs: crates/jstream-render/README.md#filters
//!
//! Shared pieces: [`gauss`] is the bounded separable blur that blur, drop shadow,
//! glow and bloom use; [`area`] is the whole / inside / outside rectangle that blur and pixelate
//! share; [`bounds`] is the room drop shadow and glow blur their shape in; `common.wgsl`
//! holds the WGSL helpers these shaders share.

/// A shader of this group: `common.wgsl`, then the named file.
macro_rules! wgsl {
    ($file:literal) => {
        concat!(include_str!("common.wgsl"), "\n", include_str!($file))
    };
}

mod area;
mod bloom;
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
        bloom::descriptor(),
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
        bloom::KIND => Box::new(bloom::Bloom::new(settings)),
        video_delay::KIND => Box::new(video_delay::VideoDelay::new(settings)),
        _ => return None,
    })
}

/// How far a filter of this group's `kind_id` draws past its input at `settings`, px,
/// for the kinds that declare an outset; `None` for any other kind.
pub(crate) fn outset(kind_id: &str, settings: &Settings) -> Option<f32> {
    Some(match kind_id {
        drop_shadow::KIND => drop_shadow::outset(settings),
        outline::KIND => outline::outset(settings),
        glow::KIND => glow::outset(settings),
        bloom::KIND => bloom::outset(settings),
        _ => return None,
    })
}

/// `v` clamped to `0..=max`, and `max` when it isn't finite.
fn within(v: f32, max: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, max)
    } else {
        max
    }
}

/// A straight-alpha linear colour, premultiplied, as shaders take it.
fn premultiplied([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

const PAD: Shader = Shader {
    label: "multipass-pad",
    source: wgsl!("pad.wgsl"),
    blend: Blend::Replace,
};

/// Draws `input` into `output`, `pad` pixels larger on every side, centred, with
/// transparent around it.
pub(crate) fn pad(
    ctx: &mut Ctx<'_>,
    input: &Image,
    pad: u32,
    output: &Image,
) -> Result<(), RenderError> {
    ctx.draw(&Draw {
        shader: PAD,
        input,
        aux: None,
        params: &Uniforms::new().i32(pad as i32).finish(),
        target: output,
    })
}

/// Copies `input` texel for texel into `output`, the same size.
pub(crate) fn copy(ctx: &mut Ctx<'_>, input: &Image, output: &Image) -> Result<(), RenderError> {
    ctx.draw(&Draw {
        shader: COPY,
        input,
        aux: None,
        params: &Uniforms::new().finish(),
        target: output,
    })
}

#[cfg(test)]
mod outset_tests {
    use jstream_protocol::Settings;
    use serde_json::json;

    use super::{descriptors, outset};

    fn settings(pairs: &[(&str, serde_json::Value)]) -> Settings {
        let mut out = Settings::new();
        for (id, value) in pairs {
            out.insert(*id, value.clone());
        }
        out
    }

    /// Each kind's outset follows its settings, and never passes the `max_outset_px`
    /// it declares (ADR-0136), at its defaults, at its largest settings and
    /// at values out of range.
    #[test]
    fn an_outset_follows_the_settings_within_the_declared_most() {
        let none = Settings::new();
        assert_eq!(outset("drop_shadow", &none), Some(16.0));
        assert_eq!(outset("outline", &none), Some(5.0));
        assert_eq!(outset("glow", &none), Some(16.0));
        assert_eq!(outset("bloom", &none), Some(24.0));
        let shadow = settings(&[
            ("offset_x", json!(-30)),
            ("offset_y", json!(12)),
            ("blur", json!(20)),
        ]);
        assert_eq!(outset("drop_shadow", &shadow), Some(50.0));
        let inside = settings(&[("width", json!(40)), ("position", json!("inside"))]);
        assert_eq!(outset("outline", &inside), Some(0.0));
        assert_eq!(outset("blur", &none), None);
        let huge = settings(&[
            ("offset_x", json!(1e9)),
            ("offset_y", json!(-1e9)),
            ("blur", json!(1e9)),
            ("width", json!(1e9)),
            ("radius", json!(1e9)),
        ]);
        for kind in descriptors() {
            let declared = kind.max_outset_px.map(f64::from);
            let at_most = outset(&kind.kind_id, &huge).map(f64::from);
            assert_eq!(at_most, declared, "{}", kind.kind_id);
        }
    }
}
