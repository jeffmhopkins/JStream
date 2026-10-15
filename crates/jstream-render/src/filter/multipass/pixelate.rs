//! `pixelate`: a mosaic of square blocks, each the mean of the pixels it covers, over
//! the whole frame or inside or outside a rectangle. Blocks are aligned to the frame's
//! top-left. Blocks up to 8 px are averaged exactly; larger ones from an 8 x 8 grid of
//! samples, so the cost per block is bounded.

use jstream_protocol::{KindDescriptor, Settings};

use super::area::{self, Area, Sample};
use super::gauss::{Edge, down};
use crate::error::RenderError;
use crate::filter::params::{Read, filter, int};
use crate::filter::{Ctx, Pass};
use crate::image::Image;

pub(super) const KIND: &str = "pixelate";

pub(super) fn descriptor() -> KindDescriptor {
    let mut params = vec![int("block_size", "Block size", 1, 256, 16, "px")];
    params.extend(area::params());
    filter(KIND, "Pixelate", params)
}

pub(super) struct Pixelate {
    block: u32,
    area: Area,
}

impl Pixelate {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut pass = Self {
            block: 16,
            area: area::read(settings),
        };
        pass.update(settings);
        pass
    }
}

impl Pass for Pixelate {
    fn update(&mut self, settings: &Settings) {
        self.block = Read(settings).i64("block_size", 16).clamp(1, 256) as u32;
        self.area = area::read(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let size = (input.width(), input.height());
        let blocks = down(ctx, input, self.block, size, (0, 0), Edge::Clamp)?;
        area::composite(
            ctx,
            input,
            &blocks,
            self.block,
            Sample::Blocks,
            self.area,
            output,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open};
    use serde_json::json;

    const R: [u8; 4] = [255, 0, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];

    /// 8x4 columns alternating red and blue, one pixel each.
    fn stripes(opened: &crate::tests::Opened) -> crate::Image {
        let pixels: Vec<[u8; 4]> = (0..4)
            .flat_map(|_| (0..8).map(|x| if x % 2 == 0 { R } else { B }))
            .collect();
        image(opened, 8, &pixels)
    }

    #[test]
    fn each_block_is_the_mean_of_its_pixels() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = stripes(&opened);
        let mut pixelate = filter("pixelate", json!({ "block_size": 2 }));
        let out = run(&mut opened, &mut pixelate, &input, 0.0);
        // Half red, half blue in linear light: 0.5 encodes to sRGB 188.
        for (x, y) in [(0, 0), (1, 1), (5, 2), (7, 3)] {
            near(&out, x, y, [188, 0, 188, 255], 2);
        }
        let mut one = filter("pixelate", json!({ "block_size": 1 }));
        let out = run(&mut opened, &mut one, &input, 0.0);
        near(&out, 2, 1, R, 0);
        near(&out, 3, 1, B, 0);
    }

    #[test]
    fn blocks_align_to_the_top_left_and_partial_blocks_average_what_they_cover() {
        let Some(mut opened) = open() else {
            return;
        };
        // Column 0..3 red, 3..8 blue; blocks of 3: [R R R] [B B B] [B B].
        let pixels: Vec<[u8; 4]> = (0..4)
            .flat_map(|_| (0..8).map(|x| if x < 3 { R } else { B }))
            .collect();
        let input = image(&opened, 8, &pixels);
        let mut pixelate = filter("pixelate", json!({ "block_size": 3 }));
        let out = run(&mut opened, &mut pixelate, &input, 0.0);
        near(&out, 1, 1, R, 1);
        near(&out, 4, 1, B, 1);
        near(&out, 7, 3, B, 1);
        // Blocks past 8 px are sampled on an 8 x 8 grid; a flat block stays flat.
        let pixels: Vec<[u8; 4]> = (0..16)
            .flat_map(|_| (0..32).map(|x| if x < 16 { R } else { B }))
            .collect();
        let input = image(&opened, 32, &pixels);
        let mut pixelate = filter("pixelate", json!({ "block_size": 16 }));
        let out = run(&mut opened, &mut pixelate, &input, 0.0);
        near(&out, 8, 8, R, 1);
        near(&out, 24, 8, B, 1);
        let mut pixelate = filter("pixelate", json!({ "block_size": 32 }));
        let out = run(&mut opened, &mut pixelate, &input, 0.0);
        near(&out, 8, 8, [188, 0, 188, 255], 2);
    }

    #[test]
    fn inside_an_area_only_the_area_is_pixelated() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = stripes(&opened);
        let mut pixelate = filter(
            "pixelate",
            json!({"block_size": 2, "area": "inside", "area_x": 0.0, "area_y": 0.0,
                "area_width": 0.5, "area_height": 1.0}),
        );
        let out = run(&mut opened, &mut pixelate, &input, 0.0);
        near(&out, 1, 1, [188, 0, 188, 255], 2);
        near(&out, 4, 1, R, 0);
        near(&out, 5, 1, B, 0);
    }
}
