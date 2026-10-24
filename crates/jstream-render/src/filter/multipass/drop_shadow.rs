//! `drop_shadow`: the input's alpha shape, blurred, tinted and moved, drawn under it.
//! It draws within the input's frame: a source or scene chain's output is its input's
//! size, so the part of the shadow past the frame is cut (ADR-0065 §5). The composite
//! under the shape is shared with `glow`.

use jstream_protocol::{KindDescriptor, Settings};

use super::bounds::Insets;
use super::gauss::{self, Edge};
use super::premultiplied;
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "drop_shadow";

const SHADOW: Shader = Shader {
    label: "multipass-shadow",
    source: wgsl!("shadow.wgsl"),
    blend: Blend::Replace,
};

/// Black at 70 % alpha, `#000000b3`, as straight linear RGBA.
const DEFAULT_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 179.0 / 255.0];

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Drop shadow",
        vec![
            color("color", "Color", true, "#000000b3"),
            float("offset_x", "Offset X", -200.0, 200.0, 8.0, "px"),
            float("offset_y", "Offset Y", -200.0, 200.0, 8.0, "px"),
            float("blur", "Blur", 0.0, 100.0, 8.0, "px"),
        ],
    )
}

/// A blurred, tinted copy of the input's shape under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Under {
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Where the copy sits relative to the input, px.
    pub offset: (f32, f32),
    pub radius: f32,
    /// The blurred alpha's multiplier (1 for a shadow).
    pub strength: f32,
}

impl Under {
    /// Draws `input` over the copy into `output`, a frame of the input's size.
    pub(super) fn encode(
        &self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        // The shape alone, in a frame `pad` larger on each side so its blur fits.
        let pad = Insets::even(self.radius);
        let shape_size = pad.grow(input.width(), input.height());
        let blurred = gauss::blur(
            ctx,
            input,
            self.radius,
            shape_size,
            pad.origin(),
            Edge::Transparent,
        )?;
        // The output is the input's frame (ADR-0065 §5), so the blurred shape's frame sits
        // `pad` up and left of it, moved by the offset.
        let params = Uniforms::new()
            .vec4(premultiplied(self.color))
            .vec2([
                self.offset.0 - pad.left as f32,
                self.offset.1 - pad.top as f32,
            ])
            .vec2([shape_size.0 as f32, shape_size.1 as f32])
            .f32(blurred.scale as f32)
            .f32(self.strength)
            .finish();
        ctx.draw(&Draw {
            shader: SHADOW,
            input,
            aux: Some(&blurred.image),
            params: &params,
            target: output,
        })
    }
}

pub(super) struct DropShadow(Under);

impl DropShadow {
    pub(super) fn new(settings: &Settings) -> Self {
        Self(read(settings))
    }
}

fn read(settings: &Settings) -> Under {
    let read = Read(settings);
    Under {
        color: read.color_linear("color", DEFAULT_COLOR),
        offset: (read.f32("offset_x", 8.0), read.f32("offset_y", 8.0)),
        radius: read.f32("blur", 8.0),
        strength: 1.0,
    }
}

impl Pass for DropShadow {
    fn update(&mut self, settings: &Settings) {
        self.0 = read(settings);
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
    use super::read;
    use crate::filter::multipass::square;
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn settings_map_to_the_copy_and_the_frame_keeps_its_size() {
        let under = read(&settings(
            "drop_shadow",
            json!({"color": "#ff000080", "offset_x": -3.0, "offset_y": 5.0, "blur": 2.0}),
        ));
        assert_eq!(under.offset, (-3.0, 5.0));
        assert!((under.color[0] - 1.0).abs() < 1e-6);
        assert!((under.color[3] - 128.0 / 255.0).abs() < 1e-6);
        // ADR-0065 §5: on a source or scene chain the output is the input's size,
        // whatever the offset and blur.
        let f = filter(
            "drop_shadow",
            json!({"offset_x": -3.0, "offset_y": 5.0, "blur": 2.0}),
        );
        assert_eq!(f.output_size(10, 10), (10, 10));
        assert_eq!(
            filter("drop_shadow", json!({})).output_size(10, 10),
            (10, 10)
        );
    }

    #[test]
    fn a_hard_shadow_is_the_shape_moved_and_tinted_under_it() {
        let Some(mut opened) = open() else {
            return;
        };
        // A 4x4 red square at (2..6, 2..6) in a transparent 12x12 frame.
        let input = square(&opened, 12, 2, 4, RED);
        let mut shadow = filter(
            "drop_shadow",
            json!({"color": "#0000ff", "offset_x": 4.0, "offset_y": 4.0, "blur": 0.0}),
        );
        let out = run(&mut opened, &mut shadow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (12, 12));
        near(&out, 2, 2, RED, 0);
        near(&out, 5, 5, RED, 0);
        // The copy covers (6..10, 6..10).
        near(&out, 6, 6, BLUE, 0);
        near(&out, 9, 9, BLUE, 0);
        near(&out, 10, 10, CLEAR, 0);
        near(&out, 9, 3, CLEAR, 0);
        near(&out, 3, 9, CLEAR, 0);
    }

    /// ADR-0065 §5: an opaque frame keeps its size, and the shadow past its edge is cut,
    /// so nothing of it shows (#369).
    #[test]
    fn a_shadow_past_the_frame_is_cut() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 6, RED);
        let mut shadow = filter(
            "drop_shadow",
            json!({"color": "#0000ff", "offset_x": -3.0, "offset_y": -2.0, "blur": 4.0}),
        );
        let out = run(&mut opened, &mut shadow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (8, 6));
        for (x, y) in [(0, 0), (7, 0), (0, 5), (7, 5), (4, 3)] {
            near(&out, x, y, RED, 0);
        }
    }

    #[test]
    fn a_soft_shadow_fades_out_around_the_shape() {
        let Some(mut opened) = open() else {
            return;
        };
        // An 8x8 red square at (6..14, 6..14) in a transparent 20x20 frame.
        let input = square(&opened, 20, 6, 8, RED);
        let mut shadow = filter(
            "drop_shadow",
            json!({"color": "#000000", "offset_x": 0.0, "offset_y": 0.0, "blur": 6.0}),
        );
        let out = run(&mut opened, &mut shadow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (20, 20));
        near(&out, 6, 6, RED, 0);
        near(&out, 13, 13, RED, 0);
        // Outside the shape the shadow's alpha falls with distance, to nothing at the edge.
        let alpha = |x| out.pixel(x, 10).expect("inside")[3];
        assert!(
            alpha(5) > alpha(3) && alpha(3) > alpha(1),
            "{:?}",
            (alpha(5), alpha(3), alpha(1))
        );
        assert!(alpha(5) > 60 && alpha(0) < 12);
        assert_eq!(out.pixel(5, 10).expect("inside")[0], 0, "black shadow");
    }
}
