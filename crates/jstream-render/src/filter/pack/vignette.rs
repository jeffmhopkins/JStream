//! `vignette`: darkens, or tints with a colour, the picture towards its frame's edges,
//! around a centre, in an ellipse that follows the frame's aspect or a circle (#529,
//! #1226). On a source chain it frames the source's whole frame; on an item or a scene
//! chain (ADR-0065) it frames what that level shows. Coverage is kept, so a transparent
//! pixel stays transparent.
//!
//! `radius` and `softness` are fractions of the half-diagonal (the centre to a corner),
//! and `strength` is the share of the colour the edge takes, in linear light. The
//! settings a saved vignette has keep their meaning (ADR-0025 › Versioning a kind): the
//! new `shape`, `blend` and centre default to what it drew before, an ellipse that
//! follows the frame, mixed towards the colour, around the middle.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, Shader};

pub(super) const KIND: &str = "vignette";

pub(super) const SHADER: Shader = Shader {
    label: "filter-vignette",
    source: include_str!("vignette.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Vignette",
        vec![
            ParamDescriptor {
                description: Some(
                    "How much of the color the edges take, in linear light: 0 is none. A \
                     dark vignette looks half as bright at the edge near 0.8."
                        .into(),
                ),
                ..float("strength", "Strength", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "Where the fade starts, as a fraction of the way from the centre to a \
                     corner."
                        .into(),
                ),
                ..float("radius", "Radius", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "How far the fade runs past the radius, as the same fraction. 0 is a \
                     hard edge."
                        .into(),
                ),
                ..float("softness", "Softness", 0.0, 1.0, 0.5, "")
            },
            color("color", "Color", true, "#000000ff"),
            ParamDescriptor {
                description: Some(
                    "Follow the frame: an ellipse of the frame's shape. Round: a circle.".into(),
                ),
                ..choice(
                    "shape",
                    "Shape",
                    &[("frame", "Follow the frame"), ("round", "Round")],
                    "frame",
                )
            },
            ParamDescriptor {
                description: Some("Multiply darkens by the color; Normal tints towards it.".into()),
                ..choice(
                    "blend",
                    "Blend",
                    &[("normal", "Normal"), ("multiply", "Multiply")],
                    "normal",
                )
            },
            ParamDescriptor {
                description: Some("The centre, from the left edge (0) to the right (1).".into()),
                ..float("center_x", "Centre X", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some("The centre, from the top edge (0) to the bottom (1).".into()),
                ..float("center_y", "Centre Y", 0.0, 1.0, 0.5, "")
            },
        ],
    )
}

/// The shader's values.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    /// Straight linear colour.
    color: [f32; 4],
    center: [f32; 2],
    /// The strength, in linear light.
    amount: f32,
    radius: f32,
    softness: f32,
    round: bool,
    multiply: bool,
}

/// `v` clamped to 0..1, `fallback` when it isn't finite.
fn unit(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

fn values(settings: &Settings) -> Values {
    let read = Read(settings);
    Values {
        color: read.color_linear("color", [0.0, 0.0, 0.0, 1.0]),
        center: [
            unit(read.f32("center_x", 0.5), 0.5),
            unit(read.f32("center_y", 0.5), 0.5),
        ],
        amount: unit(read.f32("strength", 0.5), 0.5),
        radius: unit(read.f32("radius", 0.5), 0.5),
        softness: unit(read.f32("softness", 0.5), 0.5),
        round: read.choice("shape", "frame") == "round",
        multiply: read.choice("blend", "normal") == "multiply",
    }
}

pub(super) fn uniforms(settings: &Settings) -> Vec<u8> {
    let v = values(settings);
    Uniforms::new()
        .vec4(v.color)
        .vec2(v.center)
        .f32(v.amount)
        .f32(v.radius)
        .f32(v.softness)
        .u32(u32::from(v.round))
        .u32(u32::from(v.multiply))
        .finish()
}

#[cfg(test)]
mod tests {
    use super::{KIND, Values};
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    #[test]
    fn settings_map_to_uniforms() {
        let v = super::values(&settings(KIND, json!({})));
        assert_eq!(
            v,
            Values {
                color: [0.0, 0.0, 0.0, 1.0],
                center: [0.5, 0.5],
                amount: 0.5,
                radius: 0.5,
                softness: 0.5,
                round: false,
                multiply: false,
            }
        );
        let v = super::values(&settings(
            KIND,
            json!({
                "strength": 2.0, "radius": -1.0, "softness": 0.25, "color": "#ff0000",
                "shape": "round", "blend": "normal", "center_x": 0.25, "center_y": 3.0
            }),
        ));
        assert_eq!(v.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!((v.amount, v.radius, v.softness), (1.0, 0.0, 0.25));
        assert_eq!(v.center, [0.25, 1.0]);
        assert!(v.round && !v.multiply);
        assert_eq!(super::uniforms(&settings(KIND, json!({}))).len(), 48);
    }

    /// A vignette saved before `shape`, `blend` and the centre existed draws as it did:
    /// their defaults are the old ellipse, mix and middle, and `strength` is still the
    /// linear share of the colour (ADR-0025 › Versioning a kind; #1281 review B1).
    #[test]
    fn a_saved_vignette_keeps_its_look() {
        let saved = json!({
            "strength": 0.5, "radius": 0.5, "softness": 0.5, "color": "#ffffff"
        });
        let v = super::values(&settings(KIND, saved));
        assert_eq!((v.amount, v.radius, v.softness), (0.5, 0.5, 0.5));
        assert_eq!(v.center, [0.5, 0.5]);
        assert!(!v.round && !v.multiply, "a white tint still tints");
    }

    const W: [u8; 4] = [255, 255, 255, 255];

    fn draw(
        size: (u32, u32),
        input: [u8; 4],
        overrides: serde_json::Value,
    ) -> Option<crate::Snapshot> {
        let mut opened = open()?;
        let input = solid(&opened.compositor, size.0, size.1, input);
        let mut filter = filter(KIND, overrides);
        Some(run(&mut opened, &mut filter, &input, 0.0))
    }

    #[test]
    fn the_corners_darken_and_the_centre_is_untouched() {
        let Some(s) = draw((16, 16), W, json!({})) else {
            return;
        };
        near(&s, 7, 7, W, 1);
        near(&s, 8, 8, W, 1);
        // Pixel (0, 0)'s centre is 0.9375 of the way to the corner: smoothstep(0.5, 1.0)
        // is 0.957, so it keeps 1 - 0.5 * 0.957 = 0.52 of its linear light, sRGB 191:
        // what it drew before #1226.
        for (x, y) in [(0, 0), (15, 0), (0, 15), (15, 15)] {
            near(&s, x, y, [191, 191, 191, 255], 2);
        }
        // The middle of an edge (0.66 out) is darkened less than a corner.
        let edge = s.pixel(0, 8).expect("inside")[0];
        assert!(edge > 191 && edge < 255, "{edge}");
    }

    #[test]
    fn strength_0_leaves_the_picture_alone() {
        let Some(s) = draw((16, 16), W, json!({"strength": 0.0})) else {
            return;
        };
        for (x, y) in [(0, 0), (15, 15), (8, 8), (0, 8)] {
            near(&s, x, y, W, 1);
        }
    }

    #[test]
    fn a_hard_full_vignette_takes_the_colour_and_keeps_coverage() {
        for blend in ["multiply", "normal"] {
            let hard = json!({
                "strength": 1.0, "radius": 0.0, "softness": 0.0, "color": "#ff0000",
                "blend": blend
            });
            let Some(s) = draw((16, 16), W, hard.clone()) else {
                return;
            };
            for (x, y) in [(0, 0), (15, 15), (7, 7), (0, 8)] {
                near(&s, x, y, [255, 0, 0, 255], 1);
            }
            // A transparent pixel stays transparent, whatever the colour.
            let Some(s) = draw((16, 16), [0, 0, 0, 0], hard) else {
                return;
            };
            near(&s, 0, 0, [0, 0, 0, 0], 0);
            near(&s, 8, 8, [0, 0, 0, 0], 0);
        }
    }

    /// On a wide frame, a hard edge at 0.8 of the half-diagonal: following the frame,
    /// the middle of the left edge is inside the ellipse (its UV distance is 0.71); a
    /// circle of that radius doesn't reach it, so it takes the colour.
    #[test]
    fn a_round_vignette_is_a_circle_and_one_that_follows_the_frame_an_ellipse() {
        let blue = |shape: &str| {
            json!({
                "strength": 1.0, "radius": 0.8, "softness": 0.0, "color": "#0000ff",
                "shape": shape
            })
        };
        let Some(frame) = draw((32, 8), W, blue("frame")) else {
            return;
        };
        near(&frame, 0, 4, W, 1);
        near(&frame, 16, 0, W, 1);
        let Some(round) = draw((32, 8), W, blue("round")) else {
            return;
        };
        near(&round, 0, 4, [0, 0, 255, 255], 1);
        near(&round, 16, 0, W, 1);
    }

    /// Multiply darkens by the colour; Normal mixes towards it, so a white tint on a
    /// grey picture brightens its edge, where multiplying by white leaves it.
    #[test]
    fn multiply_darkens_and_normal_tints() {
        const GREY: [u8; 4] = [128, 128, 128, 255];
        let white = |blend: &str| {
            json!({
                "strength": 1.0, "radius": 0.0, "softness": 0.0, "color": "#ffffff",
                "blend": blend
            })
        };
        let Some(s) = draw((16, 16), GREY, white("multiply")) else {
            return;
        };
        near(&s, 0, 0, GREY, 1);
        let Some(s) = draw((16, 16), GREY, white("normal")) else {
            return;
        };
        near(&s, 0, 0, W, 1);
    }

    /// The centre moved to the top-left corner: that corner is untouched, and the
    /// opposite one takes the colour.
    #[test]
    fn the_centre_moves_the_vignette() {
        let Some(s) = draw(
            (16, 16),
            W,
            json!({
                "strength": 1.0, "radius": 0.5, "softness": 0.0, "color": "#0000ff",
                "center_x": 0.0, "center_y": 0.0
            }),
        ) else {
            return;
        };
        near(&s, 0, 0, W, 1);
        near(&s, 15, 15, [0, 0, 255, 255], 1);
    }
}
