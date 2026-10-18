//! `glow`: soft light around the shape. It is a drop shadow with no offset, a colour
//! and a strength that scales the blurred alpha (clamped to 1), drawn under the input;
//! the frame grows by the radius on every side.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::drop_shadow::Under;
use crate::error::RenderError;
use crate::filter::params::{Read, color, filter, float};
use crate::filter::{ContentBox, Ctx, Pass};
use crate::image::Image;

pub(super) const KIND: &str = "glow";

/// White, straight linear RGBA.
const DEFAULT_COLOR: [f32; 4] = [1.0; 4];

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Glow",
        vec![
            color("color", "Color", true, "#ffffff"),
            float("radius", "Radius", 0.0, 100.0, 16.0, "px"),
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

pub(super) struct Glow(Under);

impl Glow {
    pub(super) fn new(settings: &Settings) -> Self {
        Self(read(settings))
    }
}

fn read(settings: &Settings) -> Under {
    let read = Read(settings);
    Under {
        color: read.color_linear("color", DEFAULT_COLOR),
        offset: (0.0, 0.0),
        radius: read.f32("radius", 16.0),
        strength: read.f32("strength", 1.0),
    }
}

impl Pass for Glow {
    fn update(&mut self, settings: &Settings) {
        self.0 = read(settings);
    }

    fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.0.insets().grow(width, height)
    }

    fn content_box(&self, width: u32, height: u32) -> ContentBox {
        self.0.insets().content(width, height)
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        self.0.encode(ctx, input, output)
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::testing::{filter, run};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    #[test]
    fn the_glow_surrounds_the_shape_evenly_and_strength_brightens_it() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 8, [0, 0, 255, 255]);
        let mut glow = filter("glow", json!({"color": "#ffff00", "radius": 6.0}));
        assert_eq!(glow.output_size(8, 8), (20, 20));
        let content = glow.content_box(8, 8);
        assert_eq!(
            (content.x, content.y, content.width, content.height),
            (6, 6, 8, 8)
        );
        let out = run(&mut opened, &mut glow, &input, 0.0);
        near(&out, 10, 10, [0, 0, 255, 255], 0);
        // The box's corners are the input's.
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
}
