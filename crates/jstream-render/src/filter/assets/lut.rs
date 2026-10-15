//! `apply_lut`: a 3D colour lookup table over the input, blended by `amount`.
//!
//! The table is applied to sRGB-encoded straight colour (unpremultiplied, then the sRGB
//! transfer), the convention `.cube` looks are made for, and the result is decoded and
//! premultiplied again. Alpha is kept. The table arrives as an image from [`lut_image`]:
//! its `size` blue slices laid out as 2D tiles, so it binds as the ordinary aux texture,
//! and the shader does the trilinear lookup as two bilinear taps in adjacent slices.

use std::sync::Arc;

use jstream_protocol::{KindDescriptor, Settings};

use super::cube::{Lut, size_of_layout, tiles};
use super::{Reference, asset_param};
use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, filter, float};
use crate::filter::{Blend, Ctx, Draw, FilterClock, Pass, Shader, VideoFilter};
use crate::image::{self, Image};

pub(super) const KIND: &str = "apply_lut";

const SHADER: Shader = Shader {
    label: "assets-apply-lut",
    source: include_str!("lut.wgsl"),
    blend: Blend::Replace,
};

/// Copies an upload texel for texel into a layer.
const COPY: Shader = Shader {
    label: "assets-lut-copy",
    source: "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { \
             return texel(vec2<i32>(in.pos.xy)); }",
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    filter(
        KIND,
        "Apply LUT",
        vec![
            asset_param("lut", "LUT file", &["cube", "png"]),
            float("amount", "Amount", 0.0, 1.0, 1.0, ""),
        ],
    )
}

pub(super) struct ApplyLut {
    lut: Reference,
    amount: f32,
}

impl ApplyLut {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut pass = Self {
            lut: Reference::default(),
            amount: 1.0,
        };
        pass.update(settings);
        pass
    }
}

/// The uniform for a table of `size` points (0: none, pass through) at `amount`.
fn params(size: usize, amount: f32) -> Vec<u8> {
    let cols = if size == 0 { 1 } else { tiles(size).0 };
    Uniforms::new()
        .u32(size as u32)
        .u32(cols as u32)
        .f32(amount.clamp(0.0, 1.0))
        .finish()
}

impl Pass for ApplyLut {
    fn update(&mut self, settings: &Settings) {
        self.lut.update(settings, "lut");
        self.amount = Read(settings).f32("amount", 1.0);
    }

    fn set_reference(&mut self, image: Option<Arc<Image>>) {
        self.lut.image = image;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        // An image that is not a table layout (or none) passes the input through.
        let table = self.lut.get(ctx, input)?.and_then(|image| {
            size_of_layout(image.width(), image.height()).map(|size| (image, size))
        });
        let (aux, size) = table.map_or((None, 0), |(image, size)| (Some(image), size));
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux,
            params: &params(size, self.amount),
            target: output,
        })
    }
}

/// The image `apply_lut` reads for `lut`, to hand to [`VideoFilter::set_reference`]:
/// the table's 2D tile layout as `Rgba16Float`, so a 33- or 64-point table keeps its
/// precision. Output values are clamped to `0..=1`.
///
/// # Errors
/// [`RenderError::TooLarge`] if the layout (512x512 at most) exceeds the adapter limit.
pub(crate) fn lut_image(compositor: &mut Compositor, lut: &Lut) -> Result<Image, RenderError> {
    let (width, height, texels) = lut.layout();
    // A data upload only lives inside a filter pass, where the compositor's image maker
    // is at hand; copying it into the pass's output layer keeps the f16 values exactly.
    let mut upload = VideoFilter {
        kind: "assets-lut-upload".into(),
        pass: Box::new(Upload {
            width,
            height,
            texels,
        }),
    };
    let unused = compositor.layer(1, 1)?;
    compositor.apply_filter(&mut upload, &unused, FilterClock::default())
}

struct Upload {
    width: u32,
    height: u32,
    texels: Vec<[f32; 4]>,
}

impl Pass for Upload {
    fn update(&mut self, _: &Settings) {}

    fn output_size(&self, _: u32, _: u32) -> (u32, u32) {
        (self.width, self.height)
    }

    fn encode(&mut self, ctx: &mut Ctx<'_>, _: &Image, output: &Image) -> Result<(), RenderError> {
        let data = image::upload_unit_f16(&ctx.maker, self.width, self.height, &self.texels)?;
        ctx.draw(&Draw {
            shader: COPY,
            input: &data,
            aux: None,
            params: &Uniforms::new().finish(),
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{Lut, lut_image, params};
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open};

    const ASSET: &str = r#"{"type":"asset","root":"app","path":"look.cube"}"#;

    fn asset() -> serde_json::Value {
        serde_json::from_str(ASSET).expect("valid JSON")
    }

    const RED: [u8; 4] = [200, 30, 60, 255];
    const TEAL: [u8; 4] = [20, 140, 160, 255];
    const GREY: [u8; 4] = [128, 128, 128, 255];
    const HALF: [u8; 4] = [240, 120, 10, 128];

    /// An 8x8 image: four 4x4 quadrants of the colours above.
    fn quadrants(opened: &crate::tests::Opened) -> crate::Image {
        let pixels: Vec<[u8; 4]> = (0..64)
            .map(|i| match ((i % 8) / 4, (i / 8) / 4) {
                (0, 0) => RED,
                (1, 0) => TEAL,
                (0, 1) => GREY,
                _ => HALF,
            })
            .collect();
        image(opened, 8, &pixels)
    }

    /// The quadrant centres, as `(x, y, colour)`.
    const SAMPLES: [(u32, u32, [u8; 4]); 4] =
        [(2, 2, RED), (5, 2, TEAL), (2, 5, GREY), (5, 5, HALF)];

    #[test]
    fn params_carry_size_tiles_and_amount() {
        let bytes = params(17, 0.5);
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..4], &17u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &5u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &0.5f32.to_le_bytes());
        assert_eq!(&params(0, 2.0)[0..4], &0u32.to_le_bytes());
    }

    #[test]
    fn identity_tables_leave_the_image_unchanged() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = quadrants(&opened);
        for size in [2, 17, 33] {
            let table = lut_image(&mut opened.compositor, &Lut::identity(size)).expect("table");
            let mut f = filter("apply_lut", json!({ "lut": asset() }));
            f.set_reference(Some(Arc::new(table)));
            let out = run(&mut opened, &mut f, &input, 0.0);
            for (x, y, c) in SAMPLES {
                near(&out, x, y, c, 2);
            }
        }
    }

    #[test]
    fn an_inverting_table_inverts_and_a_swap_table_swaps() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = quadrants(&opened);
        let invert = Lut::from_fn(2, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        let table = lut_image(&mut opened.compositor, &invert).expect("table");
        let mut f = filter("apply_lut", json!({ "lut": asset() }));
        f.set_reference(Some(Arc::new(table)));
        let out = run(&mut opened, &mut f, &input, 0.0);
        let inv = |[r, g, b, a]: [u8; 4]| [255 - r, 255 - g, 255 - b, a];
        for (x, y, c) in SAMPLES {
            near(&out, x, y, inv(c), 2);
        }

        let swap = Lut::from_fn(9, |[r, g, b]| [b, r, g]);
        let table = lut_image(&mut opened.compositor, &swap).expect("table");
        f.set_reference(Some(Arc::new(table)));
        let out = run(&mut opened, &mut f, &input, 0.0);
        let swapped = |[r, g, b, a]: [u8; 4]| [b, r, g, a];
        for (x, y, c) in SAMPLES {
            near(&out, x, y, swapped(c), 2);
        }

        // Half the amount lands halfway between input and swap, in sRGB-encoded values.
        f.set_settings(&crate::filter::testing::settings(
            "apply_lut",
            json!({ "lut": asset(), "amount": 0.5 }),
        ));
        let out = run(&mut opened, &mut f, &input, 0.0);
        near(&out, 2, 5, GREY, 2);
        near(&out, 2, 2, [130, 115, 45, 255], 2);
    }

    #[test]
    fn without_a_table_or_a_setting_the_input_passes_through() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = quadrants(&opened);
        let mut unset = filter("apply_lut", json!({ "lut": asset() }));
        let out = run(&mut opened, &mut unset, &input, 0.0);
        for (x, y, c) in SAMPLES {
            near(&out, x, y, c, 1);
        }
        // A table but a null setting: the setting was cleared before the runtime caught up.
        let invert = Lut::from_fn(2, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        let table = lut_image(&mut opened.compositor, &invert).expect("table");
        let mut cleared = filter("apply_lut", json!({}));
        cleared.set_reference(Some(Arc::new(table)));
        let out = run(&mut opened, &mut cleared, &input, 0.0);
        for (x, y, c) in SAMPLES {
            near(&out, x, y, c, 1);
        }
        // An image that is not a table layout is ignored.
        let mut wrong = filter("apply_lut", json!({ "lut": asset() }));
        let not_a_table = crate::tests::solid(&opened.compositor, 7, 3, TEAL);
        wrong.set_reference(Some(Arc::new(not_a_table)));
        let out = run(&mut opened, &mut wrong, &input, 0.0);
        near(&out, 2, 2, RED, 1);
    }

    #[test]
    fn a_table_from_another_compositor_is_refused() {
        let Some(mut opened) = open() else {
            return;
        };
        let mut other = opened.compositor.sibling();
        let table = lut_image(&mut other, &Lut::identity(2)).expect("table");
        let input = quadrants(&opened);
        let mut f = filter("apply_lut", json!({ "lut": asset() }));
        f.set_reference(Some(Arc::new(table)));
        let result = opened
            .compositor
            .apply_filter(&mut f, &input, crate::FilterClock::default());
        assert!(matches!(result, Err(crate::RenderError::Invalid { .. })));
    }
}
