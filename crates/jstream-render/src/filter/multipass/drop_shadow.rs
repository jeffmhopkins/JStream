//! `drop_shadow`: the input's alpha shape, blurred, tinted and moved, drawn under it.
//! The frame grows to hold the shadow ([`super::bounds`]). The composite under the
//! shape is shared with `glow`.

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
    pub(super) fn insets(&self) -> Insets {
        Insets::shadow(self.radius, self.offset.0, self.offset.1)
    }

    /// Draws `input` over the copy into `output`, the input grown by [`Self::insets`].
    pub(super) fn encode(
        &self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let insets = self.insets();
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
        let (ix, iy) = insets.origin();
        let params = Uniforms::new()
            .vec4(premultiplied(self.color))
            .i32(ix)
            .i32(iy)
            .vec2([
                (ix - pad.left as i32) as f32 + self.offset.0,
                (iy - pad.top as i32) as f32 + self.offset.1,
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

    fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.0.insets().grow(width, height)
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
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 4] = [255, 0, 0, 255];

    #[test]
    fn settings_map_to_the_copy_and_its_room() {
        let under = read(&settings(
            "drop_shadow",
            json!({"color": "#ff000080", "offset_x": -3.0, "offset_y": 5.0, "blur": 2.0}),
        ));
        assert_eq!(under.offset, (-3.0, 5.0));
        assert!((under.color[0] - 1.0).abs() < 1e-6);
        assert!((under.color[3] - 128.0 / 255.0).abs() < 1e-6);
        let f = filter(
            "drop_shadow",
            json!({"offset_x": -3.0, "offset_y": 5.0, "blur": 2.0}),
        );
        // left 2 + 3, top 0, right 0, bottom 2 + 5.
        assert_eq!(f.output_size(10, 10), (15, 17));
        // The defaults: blur 8 and offset (8, 8) grow the right and bottom by 16.
        assert_eq!(
            filter("drop_shadow", json!({})).output_size(10, 10),
            (26, 26)
        );
    }

    #[test]
    fn a_hard_shadow_is_the_shape_moved_and_tinted_under_it() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 8, RED);
        let mut shadow = filter(
            "drop_shadow",
            json!({"color": "#0000ff", "offset_x": 4.0, "offset_y": 4.0, "blur": 0.0}),
        );
        let out = run(&mut opened, &mut shadow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (12, 12));
        near(&out, 2, 2, RED, 0);
        near(&out, 7, 7, RED, 0);
        near(&out, 10, 10, [0, 0, 255, 255], 0);
        near(&out, 10, 1, CLEAR, 0);
        near(&out, 1, 10, CLEAR, 0);
    }

    #[test]
    fn a_soft_shadow_fades_out_inside_the_grown_frame() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 8, RED);
        let mut shadow = filter(
            "drop_shadow",
            json!({"color": "#000000", "offset_x": 0.0, "offset_y": 0.0, "blur": 6.0}),
        );
        let out = run(&mut opened, &mut shadow, &input, 0.0);
        assert_eq!((out.width(), out.height()), (20, 20));
        // The input is drawn 1:1 at the inset.
        near(&out, 6, 6, RED, 0);
        near(&out, 13, 13, RED, 0);
        // Outside it the shadow's alpha falls with distance, to nothing at the edge.
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
