//! `rounded_corners`: rounds the frame's corners, antialiased, leaving the outside
//! transparent, with an optional border drawn inside the frame over the picture. A
//! radius past half the shorter side is clamped there (a pill or a circle).

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::filter::params::{Read, Uniforms, color, filter, float};
use crate::filter::{Blend, Shader};

pub(super) const KIND: &str = "rounded_corners";

pub(super) const SHADER: Shader = Shader {
    label: "filter-rounded-corners",
    source: include_str!("rounded_corners.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Rounded corners",
        vec![
            ParamDescriptor {
                description: Some(
                    "In source pixels. Past half the shorter side it stays at half.".into(),
                ),
                ..float("radius", "Corner radius", 0.0, 2000.0, 16.0, "px")
            },
            ParamDescriptor {
                description: Some("Drawn inside the frame, over the picture. 0 is none.".into()),
                ..float("border_width", "Border width", 0.0, 500.0, 0.0, "px")
            },
            color("border_color", "Border color", true, "#ffffffff"),
        ],
    )
}

/// The shader's values: premultiplied linear border colour, radius and border width.
fn values(settings: &Settings) -> ([f32; 4], f32, f32) {
    let read = Read(settings);
    let [r, g, b, a] = read.color_linear("border_color", [1.0; 4]);
    (
        [r * a, g * a, b * a, a],
        read.f32("radius", 16.0).max(0.0),
        read.f32("border_width", 0.0).max(0.0),
    )
}

pub(super) fn uniforms(settings: &Settings) -> Vec<u8> {
    let (color, radius, border) = values(settings);
    Uniforms::new().vec4(color).f32(radius).f32(border).finish()
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
            ([1.0; 4], 16.0, 0.0)
        );
        let (color, radius, border) = super::values(&settings(
            KIND,
            json!({"radius": 4.0, "border_width": 3.0, "border_color": "#ff000080"}),
        ));
        let a = 128.0 / 255.0;
        assert_eq!(color, [a, 0.0, 0.0, a]);
        assert_eq!((radius, border), (4.0, 3.0));
        assert_eq!(super::uniforms(&settings(KIND, json!({}))).len(), 32);
    }

    const W: [u8; 4] = [255, 255, 255, 255];
    const R: [u8; 4] = [255, 0, 0, 255];
    const GONE: [u8; 4] = [0, 0, 0, 0];

    fn draw(overrides: serde_json::Value) -> Option<crate::Snapshot> {
        let mut opened = open()?;
        let input = solid(&opened.compositor, 16, 16, W);
        let mut filter = filter(KIND, overrides);
        Some(run(&mut opened, &mut filter, &input, 0.0))
    }

    #[test]
    fn corners_are_cut_and_antialiased() {
        let Some(s) = draw(json!({"radius": 6.0})) else {
            return;
        };
        for (x, y) in [(0, 0), (15, 0), (0, 15), (15, 15)] {
            near(&s, x, y, GONE, 0);
        }
        // (1.5, 1.5) is 6.36 px from the corner circle's centre (6, 6): 0.14 covered.
        let edge = s.pixel(1, 1).expect("inside");
        assert!(edge[3].abs_diff(35) <= 3, "{edge:?}");
        for (x, y) in [(2, 2), (8, 0), (0, 8), (8, 8), (15, 8)] {
            near(&s, x, y, W, 1);
        }
    }

    #[test]
    fn the_radius_stops_at_half_the_side() {
        let Some(s) = draw(json!({"radius": 500.0})) else {
            return;
        };
        // A circle of radius 8: the middle of each side is in, the corners are out.
        near(&s, 8, 1, W, 1);
        near(&s, 8, 8, W, 1);
        near(&s, 1, 1, GONE, 0);
        // (2.5, 2.5) is 7.78 px from the centre: 0.72 covered, where radius 6 was solid.
        let edge = s.pixel(2, 2).expect("inside");
        assert!(edge[3].abs_diff(184) <= 3, "{edge:?}");
    }

    #[test]
    fn the_border_is_drawn_inside_over_the_picture() {
        let Some(s) = draw(json!({"radius": 0.0, "border_width": 2.0, "border_color": "#ff0000"}))
        else {
            return;
        };
        for (x, y) in [(0, 8), (1, 8), (8, 0), (8, 1), (14, 8), (15, 8), (0, 0)] {
            near(&s, x, y, R, 1);
        }
        for (x, y) in [(2, 8), (8, 2), (8, 8), (13, 13)] {
            near(&s, x, y, W, 1);
        }
        // Half-transparent red over white: linear (1, 0.5, 0.5).
        let Some(s) =
            draw(json!({"radius": 0.0, "border_width": 2.0, "border_color": "#ff000080"}))
        else {
            return;
        };
        near(&s, 0, 8, [255, 188, 188, 255], 2);
        near(&s, 8, 8, W, 1);
        // With a radius the border follows the corner, and the outside stays empty.
        let Some(s) = draw(json!({"radius": 6.0, "border_width": 2.0, "border_color": "#ff0000"}))
        else {
            return;
        };
        near(&s, 0, 0, GONE, 0);
        near(&s, 0, 8, R, 1);
        near(&s, 8, 8, W, 1);
    }
}
