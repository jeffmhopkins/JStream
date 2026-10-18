//! `outline`: a stroke of `width` px around the input's alpha shape, outside it (the
//! frame grows by the width on every side, [`super::bounds`]) or inside it (the frame
//! keeps its size).
//!
//! Method: a jump flood. A seed pass marks the shape's pixels (alpha at least 0.5;
//! inside: the pixels below it), then steps of `S, S/2, ..., 1` and one more step of 1
//! carry to every pixel the vector to its nearest seed, where `S` is the power of two
//! that lets the steps reach `width + 1` px. The stroke covers each pixel by
//! `clamp(width + 1 - distance, 0, 1)`. Jump flood distances are near-exact (an
//! occasional pixel takes a seed a little farther than the nearest). [`MAX_WIDTH`] is
//! 64 px: at most 8 step passes, each of 9 loads, and two scratch layers.

use jstream_protocol::{KindDescriptor, Settings};

use super::bounds::Insets;
use super::{copy, premultiplied};
use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, ContentBox, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "outline";

/// The widest stroke, px.
pub(super) const MAX_WIDTH: f32 = 64.0;

const SEED: Shader = Shader {
    label: "multipass-jfa-seed",
    source: wgsl!("jfa_seed.wgsl"),
    blend: Blend::Replace,
};

const STEP: Shader = Shader {
    label: "multipass-jfa-step",
    source: wgsl!("jfa_step.wgsl"),
    blend: Blend::Replace,
};

const STROKE: Shader = Shader {
    label: "multipass-outline",
    source: wgsl!("outline.wgsl"),
    blend: Blend::Replace,
};

pub(super) fn descriptor() -> KindDescriptor {
    filter(
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

impl Stroke {
    fn insets(&self) -> Insets {
        if self.inside {
            Insets::default()
        } else {
            Insets::even(self.width)
        }
    }
}

/// The jump flood's step sizes for a stroke of `width` px, largest first.
fn steps(width: f32) -> impl Iterator<Item = u32> {
    let reach = width.max(0.0).ceil() as u32 + 1;
    let first = (reach.next_power_of_two() / 2).max(1);
    std::iter::successors(Some(first), |s| (*s > 1).then_some(s / 2)).chain([1])
}

impl Pass for Outline {
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
        let stroke = self.0;
        if stroke.width <= 0.0 {
            return copy(ctx, input, output);
        }
        let (ix, iy) = stroke.insets().origin();
        let inside = u32::from(stroke.inside);
        let (w, h) = (output.width(), output.height());
        let mut from = ctx.scratch(w, h)?;
        let mut to = ctx.scratch(w, h)?;
        let params = Uniforms::new().i32(ix).i32(iy).u32(inside).finish();
        ctx.draw(&Draw {
            shader: SEED,
            input,
            aux: None,
            params: &params,
            target: &from,
        })?;
        for step in steps(stroke.width) {
            let params = Uniforms::new().i32(step as i32).finish();
            ctx.draw(&Draw {
                shader: STEP,
                input: &from,
                aux: None,
                params: &params,
                target: &to,
            })?;
            std::mem::swap(&mut from, &mut to);
        }
        let params = Uniforms::new()
            .vec4(premultiplied(stroke.color))
            .i32(ix)
            .i32(iy)
            .f32(stroke.width)
            .u32(inside)
            .finish();
        ctx.draw(&Draw {
            shader: STROKE,
            input,
            aux: Some(&from),
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WIDTH, steps};
    use crate::filter::testing::{filter, image, run};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn the_steps_reach_past_the_width_and_stay_bounded() {
        let all = |w| steps(w).collect::<Vec<_>>();
        assert_eq!(all(0.0), [1, 1]);
        assert_eq!(all(1.0), [1, 1]);
        assert_eq!(all(4.0), [4, 2, 1, 1]);
        assert_eq!(all(MAX_WIDTH), [64, 32, 16, 8, 4, 2, 1, 1]);
        let mut w = 0.0;
        while w <= MAX_WIDTH {
            let s = all(w);
            let reach: u32 = s.iter().sum();
            assert!(reach > w as u32 && s.len() <= 9, "{w}: {s:?}");
            w += 0.5;
        }
    }

    #[test]
    fn sizes_grow_outside_and_not_inside() {
        use crate::filter::ContentBox;

        assert_eq!(
            filter("outline", json!({"width": 2.5})).output_size(8, 4),
            (14, 10)
        );
        let inside = filter("outline", json!({"width": 2.5, "position": "inside"}));
        assert_eq!(inside.output_size(8, 4), (8, 4));
        // The frame sits 3 px in on every side outside, and fills the output inside.
        assert_eq!(
            filter("outline", json!({"width": 2.5})).content_box(8, 4),
            ContentBox {
                x: 3,
                y: 3,
                width: 8,
                height: 4
            }
        );
        assert_eq!(inside.content_box(8, 4), ContentBox::whole((8, 4)));
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
        assert_eq!((out.width(), out.height()), (16, 16));
        // The square sits at (6..10, 6..10) in the grown frame.
        near(&out, 7, 7, RED, 0);
        near(&out, 5, 7, WHITE, 0);
        near(&out, 4, 8, WHITE, 0);
        near(&out, 3, 8, CLEAR, 0);
        near(&out, 11, 6, WHITE, 0);
        near(&out, 12, 6, CLEAR, 0);
        // Corners round off: (4, 4) is 2.8 px from the square's corner pixel.
        assert!(out.pixel(4, 4).expect("inside")[3] < 64);
        near(&out, 5, 5, WHITE, 0);
        near(&out, 0, 0, CLEAR, 0);
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
