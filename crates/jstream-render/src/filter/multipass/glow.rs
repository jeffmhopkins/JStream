//! `glow`: soft light around the shape, an even width all round. A jump flood ([`jfa`])
//! finds each pixel's distance to the shape's edge, and the glow there is that of a
//! straight edge blurred by `radius` (sigma `radius / 3`) at that distance, times
//! `strength` and the nearest shape pixel's alpha, clamped to 1, drawn under the input.
//! So a corner lights as far and as brightly as an edge, and rounds off, where a blur
//! of the shape gives a corner about half the light of an edge (#1563). It lights the
//! transparent area around an alpha shape (a logo, text, a keyed camera), so on an
//! opaque picture it shows only past the picture's edge: on an item, in the room its
//! `max_outset_px` gives the effects layer; on a source or scene chain, whose output is
//! the input's frame (ADR-0065 §5), not at all. The shape is its pixels at least half
//! opaque. `bloom` is the light that bright parts of any picture give off.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::{copy, jfa, premultiplied, within};
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "glow";

/// White, straight linear RGBA.
const DEFAULT_COLOR: [f32; 4] = [1.0; 4];

/// The largest radius a setting takes, px: also the most the glow draws past an item's
/// box.
const MAX_RADIUS: f64 = 100.0;

const GLOW: Shader = Shader {
    label: "multipass-glow",
    source: wgsl!("glow.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    KindDescriptor {
        max_outset_px: Some(MAX_RADIUS as u32),
        ..filter(
            KIND,
            "Glow",
            vec![
                color("color", "Color", true, "#ffffff"),
                ParamDescriptor {
                    description: Some(
                        "How far the light reaches from the shape's edge. A glow lights the \
                     transparent area around a shape, so an opaque picture shows it only \
                     past its edge, around an item; for the light of a picture's bright \
                     parts, use Bloom."
                            .into(),
                    ),
                    ..float("radius", "Radius", 0.0, MAX_RADIUS, 16.0, "px")
                },
                ParamDescriptor {
                    description: Some(
                        "Multiplies the glow's opacity before it is clamped, so values above \
                     1 widen its bright core."
                            .into(),
                    ),
                    ..float("strength", "Strength", 0.0, 4.0, 1.0, "")
                },
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Light {
    /// Straight linear RGBA.
    color: [f32; 4],
    /// Px, within `0..=MAX_RADIUS`.
    radius: f32,
    strength: f32,
}

pub(super) struct Glow(Light);

impl Glow {
    pub(super) fn new(settings: &Settings) -> Self {
        Self(read(settings))
    }
}

fn read(settings: &Settings) -> Light {
    let read = Read(settings);
    let strength = read.f32("strength", 1.0);
    Light {
        color: read.color_linear("color", DEFAULT_COLOR),
        radius: within(read.f32("radius", 16.0), MAX_RADIUS as f32),
        strength: if strength.is_finite() {
            strength.max(0.0)
        } else {
            0.0
        },
    }
}

/// The radius: as far as the glow reaches past the shape's edge.
pub(super) fn outset(settings: &Settings) -> f32 {
    read(settings).radius
}

/// How far the flood must reach: the radius, and the half pixel from the shape's edge
/// to its nearest pixel's centre.
fn reach(light: &Light) -> f32 {
    light.radius + 0.5
}

impl Pass for Glow {
    fn update(&mut self, settings: &Settings) {
        self.0 = read(settings);
    }

    /// The radius and the half pixel to the seed's centre, rounded up a pixel.
    fn reach(&self) -> Option<f32> {
        Some(reach(&self.0).ceil() + 1.0)
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let light = self.0;
        if light.radius <= 0.0 || light.strength <= 0.0 {
            return copy(ctx, input, output);
        }
        let nearest = jfa::flood(ctx, input, output, reach(&light), false)?;
        let params = Uniforms::new()
            .vec4(premultiplied(light.color))
            .f32(light.radius)
            .f32(light.strength)
            .finish();
        ctx.draw(&Draw {
            shader: GLOW,
            input,
            aux: Some(&nearest),
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::multipass::square;
    use crate::filter::testing::{filter, run};
    use crate::tests::{near, open};
    use serde_json::json;

    #[test]
    fn the_glow_surrounds_the_shape_evenly_and_strength_brightens_it() {
        let Some(mut opened) = open() else {
            return;
        };
        // An 8x8 blue square at (6..14, 6..14) in a transparent 20x20 frame.
        let input = square(&opened, 20, 6, 8, [0, 0, 255, 255]);
        let mut glow = filter("glow", json!({"color": "#ffff00", "radius": 6.0}));
        // ADR-0065 §5: the frame keeps its size.
        assert_eq!(glow.output_size(20, 20), (20, 20));
        let out = run(&mut opened, &mut glow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (20, 20));
        near(&out, 10, 10, [0, 0, 255, 255], 0);
        near(&out, 6, 6, [0, 0, 255, 255], 0);
        near(&out, 13, 13, [0, 0, 255, 255], 0);
        let a = |x, y| out.pixel(x, y).expect("inside")[3];
        // Even on all four sides.
        assert!(a(4, 10).abs_diff(a(15, 10)) <= 2 && a(10, 4).abs_diff(a(10, 15)) <= 2);
        assert!(a(4, 10) > 40 && a(0, 0) < 8);
        let c = out.pixel(4, 10).expect("inside");
        assert!(c[0] > 240 && c[1] > 240 && c[2] < 10, "yellow: {c:?}");
        let mut strong = filter(
            "glow",
            json!({"color": "#ffff00", "radius": 6.0, "strength": 3.0}),
        );
        let out = run(&mut opened, &mut strong, &input, 0.0);
        assert!(out.pixel(4, 10).expect("inside")[3] > a(4, 10));
        near(&out, 10, 10, [0, 0, 255, 255], 0);
    }

    /// The light `e` px past a straight edge, as the shader gives it.
    fn profile(e: f64, radius: f64, strength: f64) -> f64 {
        if e >= radius {
            return 0.0;
        }
        // erfc, Abramowitz and Stegun 7.1.26, as `glow.wgsl`.
        let x = e * 3.0 / (radius * std::f64::consts::SQRT_2);
        let t = 1.0 / (1.0 + 0.327_591_1 * x);
        let poly = t
            * (0.254_829_592
                + t * (-0.284_496_736
                    + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
        (strength * 0.5 * poly * (-x * x).exp()).min(1.0)
    }

    /// #1563, golden: every pixel around a square takes the light of its distance from
    /// the square's edge, so a corner lights as far and as brightly as an edge, and
    /// rounds off. A pixel 3 px across and 4 px up from the corner pixel is 5 px from
    /// it, as one 5 px off the middle of an edge, and takes the same light; a blur of
    /// the square would give it about half. The distance is to the nearest shape
    /// pixel's centre, less the half pixel to its edge.
    #[test]
    fn a_corner_glows_as_far_and_as_brightly_as_an_edge() {
        let Some(mut opened) = open() else {
            return;
        };
        // A 16x16 square at (12..28, 12..28) in a transparent 40x40 frame.
        let input = square(&opened, 40, 12, 16, [255, 255, 255, 255]);
        for (radius, strength) in [(8.0, 1.0), (11.0, 1.5), (6.0, 3.0)] {
            let mut glow = filter(
                "glow",
                json!({"color": "#00ff00", "radius": radius, "strength": strength}),
            );
            let out = run(&mut opened, &mut glow, &input, 0.0);
            let a = |x: u32, y: u32| i32::from(out.pixel(x, y).expect("inside")[3]);
            let case = format!("radius {radius}, strength {strength}");
            for y in 0..40u32 {
                for x in 0..40u32 {
                    let near = |v: u32| f64::from(v.clamp(12, 27));
                    let (dx, dy) = (f64::from(x) - near(x), f64::from(y) - near(y));
                    let d = dx.hypot(dy);
                    if d == 0.0 {
                        continue;
                    }
                    let want = (profile(d - 0.5, radius, strength) * 255.0).round() as i32;
                    assert!(
                        (a(x, y) - want).abs() <= 2,
                        "{case}: ({x}, {y}), {d:.2} px from the square, is {} not {want}",
                        a(x, y)
                    );
                }
            }
            // The corner against the edge, 5 px out: every corner, both ways round.
            let edge = a(7, 20);
            assert!(edge > 0, "{case}: the edge glows 5 px out");
            for (x, y) in [(9, 8), (8, 9), (30, 8), (31, 9), (9, 31), (8, 30), (30, 31)] {
                assert!(
                    (a(x, y) - edge).abs() <= 1,
                    "{case}: ({x}, {y}) is {} where the edge is {edge}",
                    a(x, y)
                );
            }
        }
    }

    /// #1563, the numbers: at Strength 1.5 a pixel just off the edge and one just off
    /// the corner, at the same distance, take the same light, near three quarters.
    #[test]
    fn at_strength_one_and_a_half_the_corner_matches_the_edge() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = square(&opened, 40, 12, 16, [255, 255, 255, 255]);
        let mut glow = filter(
            "glow",
            json!({"color": "#ffffff", "radius": 12.0, "strength": 1.5}),
        );
        let out = run(&mut opened, &mut glow, &input, 0.0);
        let a = |x, y| i32::from(out.pixel(x, y).expect("inside")[3]);
        // 1 px out: half a pixel past the edge.
        let (edge, corner) = (a(11, 20), a(11, 12));
        assert!((edge - corner).abs() <= 1, "edge {edge}, corner {corner}");
        assert!((165..=200).contains(&edge), "about 0.7 of full: {edge}");
        // Diagonal neighbour of the corner pixel: 1.41 px to its centre, 0.91 past the
        // edge, between the edge's 1 and 2 px out.
        let diagonal = a(11, 11);
        assert!(
            diagonal <= edge + 1 && diagonal + 1 >= a(10, 20),
            "{diagonal}"
        );
    }

    /// A shape that is half transparent glows by its alpha.
    #[test]
    fn a_translucent_shape_glows_by_its_alpha() {
        let Some(mut opened) = open() else {
            return;
        };
        let solid = square(&opened, 24, 8, 8, [255, 255, 255, 255]);
        let faint = square(&opened, 24, 8, 8, [255, 255, 255, 191]);
        let settings = json!({"color": "#ffffff", "radius": 6.0});
        let out = run(
            &mut opened,
            &mut filter("glow", settings.clone()),
            &solid,
            0.0,
        );
        let full = i32::from(out.pixel(6, 12).expect("inside")[3]);
        let out = run(&mut opened, &mut filter("glow", settings), &faint, 0.0);
        let part = i32::from(out.pixel(6, 12).expect("inside")[3]);
        let want = full * 191 / 255;
        assert!((part - want).abs() <= 3, "{part} against {want}");
    }

    /// Padded, as an item's effects pad their layer (ADR-0065 §5): an opaque frame is
    /// placed in the middle of a larger transparent one, and the glow shows around it,
    /// where unpadded it is hidden under the frame (#1226).
    #[test]
    fn padded_the_glow_of_an_opaque_frame_shows_around_it() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::solid(&opened.compositor, 8, 8, [255, 255, 255, 255]);
        let mut glow = filter(
            "glow",
            json!({"color": "#0000ff", "radius": 4.0, "strength": 4.0}),
        );
        let out = run(&mut opened, &mut glow, &input, 0.0);
        near(&out, 0, 0, [255, 255, 255, 255], 0);
        glow.set_pad(6);
        assert_eq!(glow.output_size(8, 8), (20, 20));
        let out = run(&mut opened, &mut glow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (20, 20));
        near(&out, 10, 10, [255, 255, 255, 255], 0);
        near(&out, 6, 6, [255, 255, 255, 255], 0);
        let ring = out.pixel(4, 10).expect("inside");
        assert!(
            ring[2] > 200 && ring[0] < 10 && ring[3] > 100,
            "blue: {ring:?}"
        );
        assert!(out.pixel(0, 0).expect("inside")[3] < 40);
    }
}
