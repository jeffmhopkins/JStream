//! `bloom`: bright areas bleed light outward, added over the picture. A bright pass keeps
//! the light above `threshold` (sRGB-encoded luma, with a soft knee), [`gauss`] blurs it
//! by `radius`, and the result, tinted and scaled by `intensity`, is added over the
//! input. Unlike `glow`, which lights the area around an alpha shape and so needs a
//! transparent edge, bloom works on any picture, an opaque camera included.
//!
//! The bright pass is drawn into the output layer, which the blur then reads and the
//! composite overwrites, so bloom takes no more scratch layers than the blur's own.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::gauss::{self, Edge};
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "bloom";

/// The largest radius a setting takes, px. It is also the most bloom draws past an
/// item's box ([`descriptor`]'s `max_outset_px`).
pub(super) const MAX_RADIUS: f32 = 100.0;

/// Half the width of the threshold's soft knee, in encoded luma.
const KNEE: f32 = 0.1;

const BRIGHT: Shader = Shader {
    label: "multipass-bloom-bright",
    source: wgsl!("bloom_bright.wgsl"),
    blend: Blend::Replace,
};

const ADD: Shader = Shader {
    label: "multipass-bloom",
    source: wgsl!("bloom.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    KindDescriptor {
        max_outset_px: Some(MAX_RADIUS as u32),
        ..filter(
            KIND,
            "Bloom",
            vec![
                ParamDescriptor {
                    description: Some(
                        "How bright a part of the picture must be to bloom, as it looks: \
                         0 blooms everything, 1 only pure white."
                            .into(),
                    ),
                    ..float("threshold", "Threshold", 0.0, 1.0, 0.7, "")
                },
                ParamDescriptor {
                    description: Some(
                        "How much light the bright parts add. Above 1 they flare.".into(),
                    ),
                    ..float("intensity", "Intensity", 0.0, 4.0, 1.0, "")
                },
                ParamDescriptor {
                    description: Some("How far the light spreads.".into()),
                    ..float("radius", "Radius", 0.0, f64::from(MAX_RADIUS), 24.0, "px")
                },
                color("tint", "Tint", false, "#ffffff"),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    threshold: f32,
    intensity: f32,
    radius: f32,
    /// Linear RGB.
    tint: [f32; 3],
}

pub(super) struct Bloom(Look);

impl Bloom {
    pub(super) fn new(settings: &Settings) -> Self {
        Self(read(settings))
    }
}

/// `v` if finite, clamped to `lo..=hi`, else `fallback`.
fn clamped(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        fallback
    }
}

fn read(settings: &Settings) -> Look {
    let read = Read(settings);
    let [r, g, b, _] = read.color_linear("tint", [1.0; 4]);
    Look {
        threshold: clamped(read.f32("threshold", 0.7), 0.0, 1.0, 0.7),
        intensity: clamped(read.f32("intensity", 1.0), 0.0, 4.0, 1.0),
        radius: clamped(read.f32("radius", 24.0), 0.0, MAX_RADIUS, 24.0),
        tint: [r, g, b],
    }
}

/// The radius: as far as the light spreads past the bright parts.
pub(super) fn outset(settings: &Settings) -> f32 {
    read(settings).radius
}

impl Pass for Bloom {
    fn update(&mut self, settings: &Settings) {
        self.0 = read(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let s = self.0;
        if s.intensity <= 0.0 {
            return super::copy(ctx, input, output);
        }
        let params = Uniforms::new().f32(s.threshold).f32(KNEE).finish();
        ctx.draw(&Draw {
            shader: BRIGHT,
            input,
            aux: None,
            params: &params,
            target: output,
        })?;
        // Light doesn't come from past the frame, so its edge is transparent.
        let size = (output.width(), output.height());
        let blurred = gauss::blur(ctx, output, s.radius, size, (0, 0), Edge::Transparent)?;
        let [r, g, b] = s.tint;
        let k = s.intensity;
        let params = Uniforms::new()
            .vec4([r * k, g * k, b * k, 0.0])
            .f32(blurred.scale as f32)
            .finish();
        ctx.draw(&Draw {
            shader: ADD,
            input,
            aux: Some(&blurred.image),
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{KIND, read};
    use crate::filter::multipass::square;
    use crate::filter::testing::{filter, image, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const GREY: [u8; 4] = [60, 60, 60, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn settings_are_clamped_and_the_outset_is_the_largest_radius() {
        let s = read(&settings(
            KIND,
            json!({"threshold": 2.0, "intensity": -1.0, "radius": 500.0, "tint": "#ff0000"}),
        ));
        assert_eq!((s.threshold, s.intensity, s.radius), (1.0, 0.0, 100.0));
        assert_eq!(s.tint, [1.0, 0.0, 0.0]);
        assert_eq!(super::descriptor().max_outset_px, Some(100));
        // The frame keeps its size (ADR-0065 §5).
        assert_eq!(filter(KIND, json!({})).output_size(20, 12), (20, 12));
    }

    /// A dark, opaque frame, the camera case: nothing is over the threshold, so bloom
    /// leaves it alone, where a glow would be hidden under it.
    #[test]
    fn a_dark_opaque_picture_is_left_alone() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 16, 16, GREY);
        let mut bloom = filter(KIND, json!({}));
        let out = run(&mut opened, &mut bloom, &input, 0.0);
        for (x, y) in [(0, 0), (8, 8), (15, 15)] {
            near(&out, x, y, GREY, 1);
        }
    }

    /// A bright spot on an opaque grey frame, the camera case: the light spreads over
    /// the dark around it, fading with distance, evenly on every side.
    #[test]
    fn a_bright_spot_on_an_opaque_picture_bleeds_light_outward() {
        let Some(mut opened) = open() else {
            return;
        };
        let pixels: Vec<[u8; 4]> = (0..24)
            .flat_map(|y| {
                (0..24).map(move |x| {
                    if (10..14).contains(&x) && (10..14).contains(&y) {
                        WHITE
                    } else {
                        GREY
                    }
                })
            })
            .collect();
        let input = image(&opened, 24, &pixels);
        let mut bloom = filter(KIND, json!({"radius": 6.0, "intensity": 2.0}));
        let out = run(&mut opened, &mut bloom, &input, 0.0);
        near(&out, 11, 11, WHITE, 1);
        let r = |x, y| out.pixel(x, y).expect("inside")[0];
        // Lit beside the spot, less so farther out, and untouched far away.
        assert!(r(8, 11) > GREY[0] + 20, "{}", r(8, 11));
        assert!(
            r(8, 11) > r(6, 11) && r(6, 11) >= r(4, 11),
            "{:?}",
            (r(8, 11), r(6, 11), r(4, 11))
        );
        near(&out, 0, 0, GREY, 2);
        assert!(r(8, 11).abs_diff(r(15, 11)) <= 3 && r(11, 8).abs_diff(r(11, 15)) <= 3);
        assert_eq!(out.pixel(8, 11).expect("inside")[3], 255, "still opaque");
    }

    /// A white logo on a transparent frame: the light spills past its shape as a
    /// coloured halo that keeps the tint, and the logo itself is kept.
    #[test]
    fn light_spills_past_a_shape_with_alpha_in_the_tint() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = square(&opened, 20, 6, 8, WHITE);
        let mut bloom = filter(
            KIND,
            json!({"radius": 6.0, "intensity": 1.0, "tint": "#00ff00", "threshold": 0.5}),
        );
        let out = run(&mut opened, &mut bloom, &input, 0.0);
        near(&out, 10, 10, WHITE, 1);
        let halo = out.pixel(4, 10).expect("inside");
        assert!(halo[3] > 30, "{halo:?}");
        assert!(
            halo[1] > 200 && halo[0] < 10 && halo[2] < 10,
            "green: {halo:?}"
        );
        assert!(out.pixel(0, 0).expect("inside")[3] < 10);
    }

    #[test]
    fn intensity_0_and_a_threshold_of_1_on_grey_pass_the_picture_through() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = square(&opened, 12, 3, 6, [255, 0, 0, 255]);
        for settings in [
            json!({"intensity": 0.0}),
            json!({"threshold": 1.0, "radius": 4.0}),
        ] {
            let mut bloom = filter(KIND, settings);
            let out = run(&mut opened, &mut bloom, &input, 0.0);
            near(&out, 5, 5, [255, 0, 0, 255], 1);
            near(&out, 1, 1, [0, 0, 0, 0], 1);
        }
    }
}
