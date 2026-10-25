//! `vignette`: darkens, or tints with a colour, the picture towards its frame's edges,
//! in an ellipse that follows the frame's aspect (#529). On a source chain it frames the
//! source's whole frame; on an item or a scene chain (ADR-0065) it frames what that
//! level shows. Coverage is kept, so a transparent pixel stays transparent.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::filter::params::{Read, Uniforms, color, filter, float};
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
                description: Some("How much of the colour the edges take: 0 is none.".into()),
                ..float("strength", "Strength", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "Where the fade starts, from the centre (0) to the corners (1).".into(),
                ),
                ..float("radius", "Radius", 0.0, 1.0, 0.5, "")
            },
            ParamDescriptor {
                description: Some(
                    "How far the fade runs past the radius. 0 is a hard edge.".into(),
                ),
                ..float("softness", "Softness", 0.0, 1.0, 0.5, "")
            },
            color("color", "Color", true, "#000000ff"),
        ],
    )
}

/// The shader's values: straight linear colour, strength, radius and softness, each
/// clamped to its range.
fn values(settings: &Settings) -> ([f32; 4], f32, f32, f32) {
    let read = Read(settings);
    (
        read.color_linear("color", [0.0, 0.0, 0.0, 1.0]),
        read.f32("strength", 0.5).clamp(0.0, 1.0),
        read.f32("radius", 0.5).clamp(0.0, 1.0),
        read.f32("softness", 0.5).clamp(0.0, 1.0),
    )
}

pub(super) fn uniforms(settings: &Settings) -> Vec<u8> {
    let (color, strength, radius, softness) = values(settings);
    Uniforms::new()
        .vec4(color)
        .f32(strength)
        .f32(radius)
        .f32(softness)
        .finish()
}

#[cfg(test)]
mod tests {
    use super::KIND;
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    #[test]
    fn settings_map_to_uniforms() {
        assert_eq!(
            super::values(&settings(KIND, json!({}))),
            ([0.0, 0.0, 0.0, 1.0], 0.5, 0.5, 0.5)
        );
        let (color, strength, radius, softness) = super::values(&settings(
            KIND,
            json!({"strength": 2.0, "radius": -1.0, "softness": 0.25, "color": "#ff0000"}),
        ));
        assert_eq!(color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!((strength, radius, softness), (1.0, 0.0, 0.25));
        assert_eq!(super::uniforms(&settings(KIND, json!({}))).len(), 32);
    }

    const W: [u8; 4] = [255, 255, 255, 255];

    fn draw(input: [u8; 4], overrides: serde_json::Value) -> Option<crate::Snapshot> {
        let mut opened = open()?;
        let input = solid(&opened.compositor, 16, 16, input);
        let mut filter = filter(KIND, overrides);
        Some(run(&mut opened, &mut filter, &input, 0.0))
    }

    #[test]
    fn the_corners_darken_and_the_centre_is_untouched() {
        let Some(s) = draw(W, json!({})) else {
            return;
        };
        near(&s, 7, 7, W, 1);
        near(&s, 8, 8, W, 1);
        // Pixel (0, 0)'s centre is 0.9375 of the way to the corner: smoothstep(0.5, 1.0)
        // is 0.957, so it keeps 1 - 0.5 * 0.957 = 0.52 of its linear light, sRGB 191.
        for (x, y) in [(0, 0), (15, 0), (0, 15), (15, 15)] {
            near(&s, x, y, [191, 191, 191, 255], 2);
        }
        // The middle of an edge (0.66 out) is darkened less than a corner.
        let edge = s.pixel(0, 8).expect("inside")[0];
        assert!(edge > 191 && edge < 255, "{edge}");
    }

    #[test]
    fn strength_0_leaves_the_picture_alone() {
        let Some(s) = draw(W, json!({"strength": 0.0})) else {
            return;
        };
        for (x, y) in [(0, 0), (15, 15), (8, 8), (0, 8)] {
            near(&s, x, y, W, 1);
        }
    }

    #[test]
    fn a_hard_full_vignette_takes_the_colour_and_keeps_coverage() {
        let hard = json!({"strength": 1.0, "radius": 0.0, "softness": 0.0, "color": "#ff0000"});
        let Some(s) = draw(W, hard.clone()) else {
            return;
        };
        for (x, y) in [(0, 0), (15, 15), (7, 7), (0, 8)] {
            near(&s, x, y, [255, 0, 0, 255], 1);
        }
        // A transparent pixel stays transparent, whatever the colour.
        let Some(s) = draw([0, 0, 0, 0], hard) else {
            return;
        };
        near(&s, 0, 0, [0, 0, 0, 0], 0);
        near(&s, 8, 8, [0, 0, 0, 0], 0);
    }
}
