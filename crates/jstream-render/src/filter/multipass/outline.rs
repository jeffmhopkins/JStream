//! `outline`: a stroke of `width` px around the input's alpha shape, outside it or
//! inside it. Either way it draws within the input's frame, so an outside stroke past
//! the frame's edge is cut (ADR-0065 §5); on an item, its `max_outset_px` gives the
//! effects layer room for the widest outside stroke past the box.
//!
//! Method: [`jfa`] gives every pixel the vector to its nearest seed within `width + 1`
//! px: the shape's pixels (alpha at least 0.5; inside: the pixels below it). The stroke
//! covers each pixel by `clamp(width + 1 - distance, 0, 1)`. A short stroke is two
//! exact passes and a long one a jump flood; [`MAX_WIDTH`] is 64 px: at most 8 flood
//! passes, each of 9 loads. Either takes two scratch layers.

use jstream_protocol::{KindDescriptor, Settings};

use super::{copy, jfa, premultiplied};
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "outline";

/// The widest stroke, px.
pub(super) const MAX_WIDTH: f32 = 64.0;

const STROKE: Shader = Shader {
    label: "multipass-outline",
    source: wgsl!("outline.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    KindDescriptor {
        // The widest stroke and its one antialiased pixel.
        max_outset_px: Some(MAX_WIDTH as u32 + 1),
        ..filter(
            KIND,
            "Outline",
            vec![
                float("width", "Width", 0.0, f64::from(MAX_WIDTH), 4.0, "px"),
                color("color", "Color", true, "#ffffff"),
                choice(
                    "position",
                    "Position",
                    &[("outside", "Outside"), ("inside", "Inside")],
                    "outside",
                ),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Stroke {
    width: f32,
    /// Straight linear RGBA.
    color: [f32; 4],
    inside: bool,
}

pub(super) struct Outline(Stroke);

impl Outline {
    pub(super) fn new(settings: &Settings) -> Self {
        Self(read(settings))
    }
}

fn read(settings: &Settings) -> Stroke {
    let read = Read(settings);
    let width = read.f32("width", 4.0);
    Stroke {
        width: if width.is_finite() {
            width.clamp(0.0, MAX_WIDTH)
        } else {
            0.0
        },
        color: read.color_linear("color", [1.0; 4]),
        inside: read.choice("position", "outside") == "inside",
    }
}

/// The width and the pixel the antialiased edge takes, for a stroke outside the shape;
/// none for one inside it.
pub(super) fn outset(settings: &Settings) -> f32 {
    let stroke = read(settings);
    if stroke.inside {
        0.0
    } else {
        stroke.width + 1.0
    }
}

impl Pass for Outline {
    fn update(&mut self, settings: &Settings) {
        self.0 = read(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let stroke = self.0;
        if stroke.width <= 0.0 {
            return copy(ctx, input, output);
        }
        let inside = u32::from(stroke.inside);
        let nearest = jfa::flood(ctx, input, output, stroke.width + 1.0, stroke.inside)?;
        let params = Uniforms::new()
            .vec4(premultiplied(stroke.color))
            .f32(stroke.width)
            .u32(inside)
            .finish();
        ctx.draw(&Draw {
            shader: STROKE,
            input,
            aux: Some(&nearest),
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    /// ADR-0065 §5: outside or inside, the frame keeps its size.
    #[test]
    fn the_frame_keeps_its_size() {
        for position in ["outside", "inside"] {
            let outline = filter("outline", json!({"width": 2.5, "position": position}));
            assert_eq!(outline.output_size(8, 4), (8, 4), "{position}");
        }
        assert_eq!(
            filter("outline", json!({"width": 0.0})).output_size(8, 4),
            (8, 4)
        );
    }

    #[test]
    fn an_outside_stroke_rings_the_shape_at_its_width() {
        let Some(mut opened) = open() else {
            return;
        };
        // A 4x4 red square in the middle of a transparent 12x12 frame.
        let pixels: Vec<[u8; 4]> = (0..12)
            .flat_map(|y| {
                (0..12).map(move |x| {
                    if (4..8).contains(&x) && (4..8).contains(&y) {
                        RED
                    } else {
                        CLEAR
                    }
                })
            })
            .collect();
        let input = image(&opened, 12, &pixels);
        let mut outline = filter("outline", json!({"width": 2.0}));
        let out = run(&mut opened, &mut outline, &input, 0.0);
        assert_eq!((out.width(), out.height()), (12, 12));
        // The square stays at (4..8, 4..8), and the stroke rings it 2 px wide.
        near(&out, 5, 5, RED, 0);
        near(&out, 3, 5, WHITE, 0);
        near(&out, 2, 6, WHITE, 0);
        near(&out, 1, 6, CLEAR, 0);
        near(&out, 9, 4, WHITE, 0);
        near(&out, 10, 4, CLEAR, 0);
        // Corners round off: (2, 2) is 2.8 px from the square's corner pixel.
        assert!(out.pixel(2, 2).expect("inside")[3] < 64);
        near(&out, 3, 3, WHITE, 0);
        near(&out, 0, 0, CLEAR, 0);
    }

    /// A short reach is found exactly: over scattered single pixels, every pixel's
    /// stroke is what its distance to the nearest of them gives, for an outside stroke
    /// and an inside one.
    #[test]
    fn a_short_stroke_is_exact_about_scattered_pixels() {
        let Some(mut opened) = open() else {
            return;
        };
        const N: i32 = 40;
        // Pixels picked by a fixed hash, about one in 23.
        let seed = |x: i32, y: i32| {
            (x as u32)
                .wrapping_mul(73_856_093)
                .wrapping_add((y as u32).wrapping_mul(19_349_663))
                .is_multiple_of(23)
        };
        for (position, inside) in [("outside", false), ("inside", true)] {
            // Outside: a stroke around each picked pixel. Inside: the picked pixels are
            // holes in an opaque frame, and the stroke is drawn from each hole and from
            // the frame's edge.
            let shape = |x: i32, y: i32| seed(x, y) != inside;
            let pixels: Vec<[u8; 4]> = (0..N)
                .flat_map(|y| (0..N).map(move |x| if shape(x, y) { RED } else { CLEAR }))
                .collect();
            let input = image(&opened, N as u32, &pixels);
            let width = 5.5f32;
            let mut outline = filter("outline", json!({"width": width, "position": position}));
            let out = run(&mut opened, &mut outline, &input, 0.0);
            for y in 0..N {
                for x in 0..N {
                    if shape(x, y) != inside {
                        continue;
                    }
                    // The nearest pixel the stroke is measured from, by brute force.
                    let mut nearest = f32::MAX;
                    for sy in 0..N {
                        for sx in 0..N {
                            if shape(sx, sy) != inside {
                                let d = (((sx - x).pow(2) + (sy - y).pow(2)) as f32).sqrt();
                                nearest = nearest.min(d);
                            }
                        }
                    }
                    if inside {
                        let edge = (x + 1).min(y + 1).min(N - x).min(N - y);
                        nearest = nearest.min(edge as f32);
                    }
                    let cover = (width + 1.0 - nearest).clamp(0.0, 1.0);
                    // White over clear (outside) or over red (inside), in linear light.
                    let got = out.pixel(x as u32, y as u32).expect("inside");
                    let want = if inside {
                        // Red loses nothing; green and blue rise with the stroke.
                        (crate::color::linear_channel_to_srgb(cover) * 255.0).round() as u8
                    } else {
                        (cover * 255.0).round() as u8
                    };
                    let seen = if inside { got[1] } else { got[3] };
                    assert!(
                        seen.abs_diff(want) <= 1,
                        "{position} ({x}, {y}): {nearest} px away, {seen} for {want}"
                    );
                }
            }
        }
    }

    /// ADR-0065 §5: an outside stroke past the frame's edge is cut, so an opaque frame
    /// shows no stroke at all.
    #[test]
    fn an_outside_stroke_past_the_frame_is_cut() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 8, RED);
        let mut outline = filter("outline", json!({"width": 3.0}));
        let out = run(&mut opened, &mut outline, &input, 0.0);
        assert_eq!((out.width(), out.height()), (8, 8));
        for (x, y) in [(0, 0), (7, 0), (0, 7), (7, 7), (4, 4)] {
            near(&out, x, y, RED, 0);
        }
    }

    #[test]
    fn an_inside_stroke_draws_over_the_edge_of_the_frame() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 12, 12, RED);
        let mut outline = filter(
            "outline",
            json!({"width": 2.0, "position": "inside", "color": "#00ff00"}),
        );
        let out = run(&mut opened, &mut outline, &input, 0.0);
        assert_eq!((out.width(), out.height()), (12, 12));
        near(&out, 0, 6, [0, 255, 0, 255], 0);
        near(&out, 1, 6, [0, 255, 0, 255], 0);
        near(&out, 2, 6, RED, 0);
        near(&out, 6, 6, RED, 0);
        near(&out, 11, 11, [0, 255, 0, 255], 0);
    }
}
