//! `blur`: a gaussian blur of the whole frame, or inside or outside a rectangle.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use super::area::{self, Area, Sample};
use super::gauss::{self, Edge};
use crate::error::RenderError;
use crate::filter::params::{Read, filter, float};
use crate::filter::{Ctx, Pass};
use crate::image::Image;

pub(super) const KIND: &str = "blur";

/// The largest radius a setting takes, px.
const MAX_RADIUS: f64 = 100.0;

pub(super) fn descriptor() -> KindDescriptor {
    let mut params = vec![ParamDescriptor {
        description: Some(
            "How far each pixel spreads, in source pixels. Large radii blur a \
             downsampled copy, so the cost stays bounded."
                .into(),
        ),
        ..float("radius", "Radius", 0.0, MAX_RADIUS, 8.0, "px")
    }];
    params.extend(area::params());
    filter(KIND, "Blur", params)
}

pub(super) struct Blur {
    radius: f32,
    area: Area,
}

impl Blur {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut pass = Self {
            radius: 0.0,
            area: area::read(settings),
        };
        pass.update(settings);
        pass
    }
}

impl Pass for Blur {
    fn update(&mut self, settings: &Settings) {
        self.radius = Read(settings).f32("radius", 8.0);
        self.area = area::read(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let size = (input.width(), input.height());
        let blurred = gauss::blur(ctx, input, self.radius, size, (0, 0), Edge::Clamp)?;
        area::composite(
            ctx,
            input,
            &blurred.image,
            blurred.scale,
            Sample::Smooth,
            self.area,
            output,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const W: [u8; 4] = [255, 255, 255, 255];
    const K: [u8; 4] = [0, 0, 0, 255];

    /// A 32x8 image, black on the left half and white on the right.
    fn halves() -> Vec<[u8; 4]> {
        (0..8)
            .flat_map(|_| (0..32).map(|x| if x < 16 { K } else { W }))
            .collect()
    }

    #[test]
    fn a_flat_frame_stays_flat_and_radius_zero_is_identity() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 16, 8, [200, 40, 90, 255]);
        for radius in [0.0, 3.0, 40.0, 100.0] {
            let mut blur = filter("blur", json!({ "radius": radius }));
            let out = run(&mut opened, &mut blur, &input, 0.0);
            for (x, y) in [(0, 0), (8, 4), (15, 7)] {
                near(&out, x, y, [200, 40, 90, 255], 2);
            }
        }
        let input = image(&opened, 32, &halves());
        let mut blur = filter("blur", json!({ "radius": 0.0 }));
        let out = run(&mut opened, &mut blur, &input, 0.0);
        near(&out, 15, 4, K, 0);
        near(&out, 16, 4, W, 0);
    }

    #[test]
    fn an_edge_softens_symmetrically_and_far_pixels_keep_their_value() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = image(&opened, 32, &halves());
        let mut blur = filter("blur", json!({ "radius": 6.0 }));
        let out = run(&mut opened, &mut blur, &input, 0.0);
        // Far from the edge: unchanged.
        near(&out, 2, 4, K, 1);
        near(&out, 29, 4, W, 1);
        // Across the edge the value climbs monotonically through mid-grey.
        let at = |x| out.pixel(x, 4).expect("inside")[0];
        let row: Vec<u8> = (10..22).map(at).collect();
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
        assert!(at(14) > 20 && at(14) < 200 && at(17) > 60, "{row:?}");
        // Symmetric in linear light: the two pixels beside the edge sum to white.
        let linear = |v: u8| crate::color::srgb_channel_to_linear(f32::from(v) / 255.0);
        assert!(
            (linear(at(15)) + linear(at(16)) - 1.0).abs() < 0.03,
            "{row:?}"
        );
    }

    #[test]
    fn a_large_radius_runs_downsampled_and_stays_smooth() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = image(&opened, 32, &halves());
        let mut blur = filter("blur", json!({ "radius": 60.0 }));
        let out = run(&mut opened, &mut blur, &input, 0.0);
        let row: Vec<u8> = (0..32)
            .map(|x| out.pixel(x, 4).expect("inside")[0])
            .collect();
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
        assert!(
            row[0] > 40 && row[31] < 250,
            "a wide blur reaches the ends: {row:?}"
        );
    }

    #[test]
    fn the_area_limits_the_blur() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = image(&opened, 32, &halves());
        // Rectangle over the right quarter of the frame: x 0.75..1, full height.
        let rect = json!({"radius": 6.0, "area_x": 0.75, "area_y": 0.0,
            "area_width": 0.25, "area_height": 1.0});
        let mut inside = rect.clone();
        inside["area"] = json!("inside");
        let mut blur = filter("blur", inside);
        let out = run(&mut opened, &mut blur, &input, 0.0);
        // The edge at x 16 is outside the rectangle: still hard.
        near(&out, 15, 4, K, 0);
        near(&out, 16, 4, W, 0);
        let mut outside = rect;
        outside["area"] = json!("outside");
        let mut blur = filter("blur", outside);
        let out = run(&mut opened, &mut blur, &input, 0.0);
        let at = |x| out.pixel(x, 4).expect("inside")[0];
        assert!(at(15) > 20 && at(16) < 235, "the edge is blurred");
        // Inside the rectangle (x >= 24) the input shows through untouched.
        near(&out, 25, 4, W, 0);
    }
}
