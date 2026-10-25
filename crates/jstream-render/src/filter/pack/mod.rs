//! The single-pass pack (roadmap V1): colour correction, the chroma, colour and luma keys, sharpen, rounded corners with a border, scroll, and vignette.
//!
//! Every kind here is one fullscreen draw of the input's size. The colour kinds
//! (colour correction and the keys) work on straight sRGB-encoded colour, as OBS does,
//! so OBS's numbers carry over; sharpen works on premultiplied encoded colour. Their
//! WGSL decodes back to premultiplied linear light on the way out.

use jstream_protocol::{KindDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::{Ctx, Draw, Pass, Shader};
use crate::image::Image;

mod color_correction;
mod keys;
mod rounded_corners;
mod scroll;
mod sharpen;
mod vignette;

/// This group's kinds.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    vec![
        color_correction::descriptor(),
        keys::chroma_descriptor(),
        keys::color_descriptor(),
        keys::luma_descriptor(),
        sharpen::descriptor(),
        rounded_corners::descriptor(),
        scroll::descriptor(),
        vignette::descriptor(),
    ]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    let single = |shader: Shader, uniforms: fn(&Settings) -> Vec<u8>| -> Box<dyn Pass> {
        Box::new(Single {
            shader,
            uniforms,
            params: uniforms(settings),
        })
    };
    Some(match kind_id {
        color_correction::KIND => single(color_correction::SHADER, color_correction::uniforms),
        keys::CHROMA_KIND => single(keys::CHROMA_SHADER, keys::chroma_uniforms),
        keys::COLOR_KIND => single(keys::COLOR_SHADER, keys::color_uniforms),
        keys::LUMA_KIND => single(keys::LUMA_SHADER, keys::luma_uniforms),
        sharpen::KIND => single(sharpen::SHADER, sharpen::uniforms),
        rounded_corners::KIND => single(rounded_corners::SHADER, rounded_corners::uniforms),
        scroll::KIND => Box::new(scroll::Scroll::new(settings)),
        vignette::KIND => single(vignette::SHADER, vignette::uniforms),
        _ => return None,
    })
}

/// A kind that is one draw whose `Params` its settings alone fix.
struct Single {
    shader: Shader,
    uniforms: fn(&Settings) -> Vec<u8>,
    params: Vec<u8>,
}

impl Pass for Single {
    fn update(&mut self, settings: &Settings) {
        self.params = (self.uniforms)(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        ctx.draw(&Draw {
            shader: self.shader,
            input,
            aux: None,
            params: &self.params,
            target: output,
        })
    }
}

/// One linear-light channel sRGB-encoded, the inverse of
/// [`crate::color::srgb_channel_to_linear`]. The colour settings read as linear; the
/// colour kinds compare and multiply in sRGB-encoded values.
fn linear_channel_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// An RGB colour setting as sRGB-encoded values in `0..=1`; `default` is a `#rrggbb`
/// byte triple.
fn encoded_rgb(read: &crate::filter::params::Read<'_>, id: &str, default: [u8; 3]) -> [f32; 3] {
    let fallback = default.map(|c| crate::color::srgb_channel_to_linear(f32::from(c) / 255.0));
    let [r, g, b, _] = read.color_linear(id, [fallback[0], fallback[1], fallback[2], 1.0]);
    [r, g, b].map(linear_channel_to_srgb)
}

/// Runs `kind_id` with `overrides` over 4-pixel bands of `colors`, 4 pixels tall, and
/// returns each band's centre pixel.
#[cfg(test)]
fn run_bands(
    opened: &mut crate::tests::Opened,
    kind_id: &str,
    overrides: serde_json::Value,
    colors: &[[u8; 4]],
) -> Vec<[u8; 4]> {
    use crate::filter::testing::{filter, run};
    let input = crate::tests::bands(&opened.compositor, 4, 4, colors);
    let mut filter = filter(kind_id, overrides);
    let snapshot = run(opened, &mut filter, &input, 0.0);
    (0..colors.len() as u32)
        .map(|i| snapshot.pixel(i * 4 + 2, 2).expect("invariant: inside"))
        .collect()
}

/// Asserts each of `got` is within `tol` of `want`, per channel.
#[cfg(test)]
fn assert_near(got: &[[u8; 4]], want: &[[u8; 4]], tol: u8) {
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            (0..4).all(|c| g[c].abs_diff(w[c]) <= tol),
            "band {i} is {g:?}, expected {w:?} within {tol} (all: {got:?})"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::linear_channel_to_srgb;
    use crate::color::srgb_channel_to_linear;

    #[test]
    fn the_srgb_encode_inverts_the_decode() {
        for byte in 0..=255u8 {
            let c = f32::from(byte) / 255.0;
            let back = linear_channel_to_srgb(srgb_channel_to_linear(c));
            assert!((back - c).abs() < 1e-4, "{byte}: {back}");
        }
    }

    #[test]
    fn every_pack_kind_builds_and_others_do_not() {
        for kind in super::descriptors() {
            let settings = crate::filter::testing::settings(&kind.kind_id, serde_json::json!({}));
            assert!(
                super::build(&kind.kind_id, &settings).is_some(),
                "{}",
                kind.kind_id
            );
        }
        let empty = jstream_protocol::Settings::default();
        assert!(super::build("aspect_ratio", &empty).is_none());
    }
}
