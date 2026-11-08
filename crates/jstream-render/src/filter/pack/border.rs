//! `border`: the full set of border tools for an item (#1227), as one filter. A rounded
//! rectangle with a radius per corner (or one for all) that can clip the picture; an
//! inner stroke inside its edge and an outer stroke past it, after a gap, each with its
//! own colour and width; a style (solid, double, dashed with its dash and gap, dotted);
//! a fill (the colour, or a linear or conic gradient to a second colour); and a speed
//! that moves the dashes and the gradient around the edge.
//!
//! The rectangle is the frame inset by the padding the chain gives the filter
//! ([`super::super::VideoFilter::set_pad`]): on an item whose border is the first
//! effect that draws past the box, that is the box itself, so the outer stroke shows
//! around it (ADR-0125). On a source or scene chain there is no padding: the rectangle
//! is the frame and the outer stroke past it is cut (ADR-0065 §5). A shadow or glow
//! after a clipping border follows its rounded shape, since they follow the alpha.
//!
//! With a frame image (#1294), the inner stroke is drawn from it as a 9-slice frame
//! instead of in its style and fill: the image's corners, `frame_slice` image pixels
//! square, fill the stroke's corners, `width` px square, unstretched, and its edges
//! stretch or tile along the sides between them. The image is a decoded asset the
//! chain runtime hands over ([`super::super::VideoFilter::set_reference`]), as an
//! image mask's is; until it is there the stroke draws as without one.
//!
//! Time is worked out on the CPU in `f64` and handed in as distances, so the movement
//! stays exact however long the clock runs.

use std::sync::Arc;

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings, VisibleIf};
use serde_json::Value;

use crate::error::RenderError;
use crate::filter::assets::{IMAGE_TYPES, Reference, asset_param};
use crate::filter::params::{Read, Uniforms, boolean, choice, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "border";

const SHADER: Shader = Shader {
    label: "filter-border",
    source: include_str!("border.wgsl"),
    blend: Blend::Replace,
};

/// The widest stroke, inner or outer, px.
const MAX_WIDTH: f64 = 64.0;
/// The widest gap before the outer stroke, px.
const MAX_GAP: f64 = 64.0;
/// The largest corner radius, px; past half the shorter side it stays at half.
const MAX_RADIUS: f64 = 2000.0;
/// The longest dash and the widest gap between dashes or dots, px.
const MAX_DASH: f64 = 400.0;
/// The fastest the pattern travels, px/s.
const MAX_SPEED: f64 = 2000.0;
/// The largest frame image corner, image px.
const MAX_SLICE: f64 = 1024.0;

fn shown_if(param: &str, equals: &str, p: ParamDescriptor) -> ParamDescriptor {
    ParamDescriptor {
        visible_if: Some(VisibleIf {
            param: param.into(),
            equals: Value::from(equals),
        }),
        ..p
    }
}

fn described(text: &str, p: ParamDescriptor) -> ParamDescriptor {
    ParamDescriptor {
        description: Some(text.into()),
        ..p
    }
}

pub(super) fn descriptor() -> KindDescriptor {
    let corner = |id: &str, label: &str| {
        shown_if(
            "corners",
            "each",
            float(id, label, 0.0, MAX_RADIUS, 16.0, "px"),
        )
    };
    let not_solid = |p: ParamDescriptor| ParamDescriptor {
        visible_if: None,
        ..p
    };
    KindDescriptor {
        // The widest outer stroke after the widest gap, and its antialiased pixel.
        max_outset_px: Some((MAX_WIDTH + MAX_GAP) as u32 + 1),
        ..filter(
            KIND,
            "Border",
            vec![
                described(
                    "Inside the edge, over the picture. 0 is none.",
                    float("width", "Width", 0.0, MAX_WIDTH, 4.0, "px"),
                ),
                color("color", "Color", true, "#ffffffff"),
                choice(
                    "style",
                    "Style",
                    &[
                        ("solid", "Solid"),
                        ("double", "Double"),
                        ("dashed", "Dashed"),
                        ("dotted", "Dotted"),
                    ],
                    "solid",
                ),
                shown_if(
                    "style",
                    "dashed",
                    float("dash", "Dash length", 1.0, MAX_DASH, 16.0, "px"),
                ),
                not_solid(described(
                    "Between dashes or dots.",
                    float("gap", "Gap", 0.0, MAX_DASH, 8.0, "px"),
                )),
                described(
                    "Round all four corners alike, or each its own.",
                    choice(
                        "corners",
                        "Corners",
                        &[("same", "All the same"), ("each", "Each its own")],
                        "same",
                    ),
                ),
                shown_if(
                    "corners",
                    "same",
                    described(
                        "Past half the shorter side it stays at half (a pill).",
                        float("radius", "Corner radius", 0.0, MAX_RADIUS, 0.0, "px"),
                    ),
                ),
                corner("radius_top_left", "Top left"),
                corner("radius_top_right", "Top right"),
                corner("radius_bottom_right", "Bottom right"),
                corner("radius_bottom_left", "Bottom left"),
                described(
                    "Cut the picture to the rounded shape, so it and the border round \
                     together, and a shadow or glow after it follows the shape.",
                    boolean("clip", "Clip to the shape", true),
                ),
                described(
                    "The stroke's color, or a gradient from Color to Color 2.",
                    choice(
                        "fill",
                        "Fill",
                        &[
                            ("color", "Color"),
                            ("linear", "Linear gradient"),
                            ("conic", "Conic gradient"),
                        ],
                        "color",
                    ),
                ),
                color("color2", "Color 2", true, "#000000ff"),
                shown_if(
                    "fill",
                    "linear",
                    float("angle", "Gradient angle", 0.0, 360.0, 90.0, "°"),
                ),
                described(
                    "Moves the dashes and the gradient around the edge. Positive is \
                     clockwise. 0 holds still.",
                    float("speed", "Speed", -MAX_SPEED, MAX_SPEED, 0.0, "px/s"),
                ),
                described(
                    "A second stroke past the edge, after the outer gap. 0 is none.",
                    float("outer_width", "Outer width", 0.0, MAX_WIDTH, 0.0, "px"),
                ),
                color("outer_color", "Outer color", true, "#000000ff"),
                float("outer_gap", "Outer gap", 0.0, MAX_GAP, 0.0, "px"),
                described(
                    "Draw the inner stroke from an image as a 9-slice frame: its corners \
                     stay as they are and its edges run along the sides. Width sets how \
                     wide it draws.",
                    asset_param("frame", "Frame image", &IMAGE_TYPES),
                ),
                described(
                    "How far in from the image's edge its corners reach, in the image's \
                     own pixels.",
                    float("frame_slice", "Frame corner", 1.0, MAX_SLICE, 16.0, "px"),
                ),
                choice(
                    "frame_edges",
                    "Frame edges",
                    &[("stretch", "Stretch"), ("tile", "Tile")],
                    "stretch",
                ),
            ],
        )
    }
}

/// The settings, read and clamped.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    /// Straight linear colours.
    color: [f32; 4],
    color2: [f32; 4],
    outer_color: [f32; 4],
    /// Top-left, top-right, bottom-right, bottom-left, px.
    radii: [f32; 4],
    width: f32,
    dash: f32,
    gap: f32,
    outer_width: f32,
    outer_gap: f32,
    /// Degrees; 0 runs left to right, 90 top to bottom.
    angle: f32,
    speed: f64,
    style: u32,
    fill: u32,
    clip: bool,
    /// The frame image's corner, image px.
    slice: f32,
    /// The frame's edges tile rather than stretch.
    tile: bool,
}

fn within(v: f32, lo: f64, hi: f64, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo as f32, hi as f32)
    } else {
        fallback
    }
}

/// How far the border draws past its input at `settings`, px (ADR-0136, #1376): its outer
/// stroke after its gap, and the stroke's antialiased pixel; nothing without one.
pub(super) fn outset(settings: &Settings) -> f32 {
    let look = read(settings);
    if look.outer_width > 0.0 {
        look.outer_gap + look.outer_width + 1.0
    } else {
        0.0
    }
}

fn read(settings: &Settings) -> Look {
    let read = Read(settings);
    let radius = |id: &str, fallback: f32| within(read.f32(id, fallback), 0.0, MAX_RADIUS, 0.0);
    let radii = if read.choice("corners", "same") == "each" {
        [
            radius("radius_top_left", 16.0),
            radius("radius_top_right", 16.0),
            radius("radius_bottom_right", 16.0),
            radius("radius_bottom_left", 16.0),
        ]
    } else {
        [radius("radius", 0.0); 4]
    };
    let speed = f64::from(read.f32("speed", 0.0));
    Look {
        color: read.color_linear("color", [1.0; 4]),
        color2: read.color_linear("color2", [0.0, 0.0, 0.0, 1.0]),
        outer_color: read.color_linear("outer_color", [0.0, 0.0, 0.0, 1.0]),
        radii,
        width: within(read.f32("width", 4.0), 0.0, MAX_WIDTH, 0.0),
        dash: within(read.f32("dash", 16.0), 1.0, MAX_DASH, 16.0),
        gap: within(read.f32("gap", 8.0), 0.0, MAX_DASH, 8.0),
        outer_width: within(read.f32("outer_width", 0.0), 0.0, MAX_WIDTH, 0.0),
        outer_gap: within(read.f32("outer_gap", 0.0), 0.0, MAX_GAP, 0.0),
        angle: within(read.f32("angle", 90.0), 0.0, 360.0, 90.0),
        speed: if speed.is_finite() {
            speed.clamp(-MAX_SPEED, MAX_SPEED)
        } else {
            0.0
        },
        style: match read.choice("style", "solid") {
            "double" => 1,
            "dashed" => 2,
            "dotted" => 3,
            _ => 0,
        },
        fill: match read.choice("fill", "color") {
            "linear" => 1,
            "conic" => 2,
            _ => 0,
        },
        clip: read.bool("clip", true),
        slice: within(read.f32("frame_slice", 16.0), 1.0, MAX_SLICE, 16.0),
        tile: read.choice("frame_edges", "stretch") == "tile",
    }
}

fn premultiplied([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

/// The length of the edge of a `half`-size rounded rectangle with `radii`, each held to
/// half its shorter side.
fn perimeter(half: [f32; 2], radii: [f32; 4]) -> f64 {
    let shortest = half[0].min(half[1]).max(0.0);
    let r: f32 = radii.iter().map(|r| r.min(shortest)).sum();
    let straight = 4.0 * (half[0] + half[1]) - 2.0 * r;
    f64::from(straight + std::f32::consts::FRAC_PI_2 * r).max(1.0)
}

pub(super) struct Border {
    look: Look,
    pad: u32,
    /// The frame image, setting `frame`.
    frame: Reference,
}

impl Border {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut frame = Reference::default();
        frame.update(settings, "frame");
        Self {
            look: read(settings),
            pad: 0,
            frame,
        }
    }

    fn uniforms(&self, seconds: f64, width: u32, height: u32, framed: bool) -> Vec<u8> {
        let l = self.look;
        let pad = self.pad as f32;
        let half = [width as f32 * 0.5 - pad, height as f32 * 0.5 - pad];
        let travelled = seconds * l.speed;
        // Dashes repeat every period, so only the distance within one matters; the
        // gradient goes once round the edge per perimeter travelled. Dots repeat every
        // stroke's own width and gap, so each stroke gets its own distance (#1296
        // review B1).
        let wrap = |w: f32| {
            let period = match l.style {
                3 => f64::from(w + l.gap),
                _ => f64::from(l.dash + l.gap),
            }
            .max(1.0);
            travelled.rem_euclid(period) as f32
        };
        let along = wrap(l.width);
        let outer_along = wrap(l.outer_width);
        let turn = (travelled / perimeter(half, l.radii)).rem_euclid(1.0) as f32;
        let (sin, cos) = l.angle.to_radians().sin_cos();
        Uniforms::new()
            .vec4(premultiplied(l.color))
            .vec4(premultiplied(l.color2))
            .vec4(premultiplied(l.outer_color))
            .vec4(l.radii)
            .vec2([cos, sin])
            .f32(l.width)
            .f32(l.dash)
            .f32(l.gap)
            .f32(l.outer_width)
            .f32(l.outer_gap)
            .f32(along)
            .f32(turn)
            .f32(pad)
            .u32(l.style)
            .u32(l.fill)
            .u32(u32::from(l.clip))
            .f32(outer_along)
            .f32(l.slice)
            .u32(u32::from(framed))
            .u32(u32::from(l.tile))
            .finish()
    }
}

impl Pass for Border {
    fn update(&mut self, settings: &Settings) {
        self.look = read(settings);
        self.frame.update(settings, "frame");
    }

    fn set_reference(&mut self, image: Option<Arc<Image>>) {
        self.frame.image = image;
    }

    fn set_pad(&mut self, pad: u32) {
        self.pad = pad;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let aux = self.frame.get(ctx)?;
        let params = self.uniforms(
            ctx.clock().seconds,
            output.width(),
            output.height(),
            aux.is_some(),
        );
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux,
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Border, KIND, outset, perimeter, read};
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const W: [u8; 4] = [255, 255, 255, 255];
    const R: [u8; 4] = [255, 0, 0, 255];
    const B: [u8; 4] = [0, 0, 255, 255];
    const GONE: [u8; 4] = [0, 0, 0, 0];

    #[test]
    fn the_outset_is_the_outer_stroke_after_its_gap() {
        let at = |v: serde_json::Value| outset(&settings(KIND, v));
        assert_eq!(at(json!({})), 0.0, "no outer stroke by default");
        assert_eq!(at(json!({"outer_width": 3.0, "outer_gap": 4.0})), 8.0);
        assert_eq!(
            at(json!({"outer_gap": 40.0})),
            0.0,
            "a gap alone draws nothing"
        );
        let most = at(json!({"outer_width": 1e9, "outer_gap": 1e9}));
        assert_eq!(Some(most as u32), super::descriptor().max_outset_px);
    }

    #[test]
    fn settings_are_read_and_clamped() {
        let l = read(&settings(KIND, json!({})));
        assert_eq!((l.width, l.radii, l.style, l.fill), (4.0, [0.0; 4], 0, 0));
        assert!(l.clip);
        let l = read(&settings(
            KIND,
            json!({
                "corners": "each", "radius_top_left": 4.0, "radius_top_right": -2.0,
                "radius": 99.0, "width": 500.0, "style": "dotted", "fill": "conic",
                "speed": 1e9
            }),
        ));
        assert_eq!(
            l.radii,
            [4.0, 0.0, 16.0, 16.0],
            "each corner, the linked one ignored"
        );
        assert_eq!((l.width, l.style, l.fill, l.speed), (64.0, 3, 2, 2000.0));
        assert_eq!(super::descriptor().max_outset_px, Some(129));
        let b = Border::new(&settings(KIND, json!({})));
        assert_eq!(b.uniforms(0.0, 10, 10, false).len(), 144);
    }

    /// Dots repeat every stroke's own width and gap, so each stroke's distance wraps
    /// by its own period: wrapping the inner stroke's by the outer's would jump its dots
    /// half a period at each wrap (#1296 review B1).
    #[test]
    fn each_strokes_dots_wrap_by_their_own_period() {
        let b = Border::new(&settings(
            KIND,
            json!({
                "style": "dotted", "width": 4.0, "outer_width": 8.0, "gap": 4.0, "speed": 9.0
            }),
        ));
        let bytes = b.uniforms(1.0, 100, 100, false);
        let at = |offset: usize| {
            f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
        };
        // Travelled 9 px: the inner dots' period is 4 + 4, the outer's 8 + 4.
        assert_eq!(at(92), 1.0, "`along`, the inner stroke's");
        assert_eq!(at(116), 9.0, "`outer_along`");
        // Dashes share one period, the dash and the gap, for both strokes.
        let b = Border::new(&settings(
            KIND,
            json!({
                "style": "dashed", "width": 4.0, "outer_width": 8.0, "dash": 10.0,
                "gap": 5.0, "speed": 20.0
            }),
        ));
        let bytes = b.uniforms(1.0, 100, 100, false);
        let at = |offset: usize| {
            f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
        };
        assert_eq!((at(92), at(116)), (5.0, 5.0));
    }

    #[test]
    fn the_perimeter_is_the_sides_and_the_arcs() {
        // A 20x10 rectangle: 60. With every corner a radius of 2, 8 of the sides become
        // four quarter circles, 4π.
        assert!((perimeter([10.0, 5.0], [0.0; 4]) - 60.0).abs() < 1e-4);
        let rounded = perimeter([10.0, 5.0], [2.0; 4]);
        assert!((rounded - (60.0 - 16.0 + 4.0 * std::f64::consts::PI)).abs() < 1e-3);
    }

    fn draw(size: u32, overrides: serde_json::Value, seconds: f64) -> Option<crate::Snapshot> {
        let mut opened = open()?;
        let input = solid(&opened.compositor, size, size, W);
        let mut f = filter(KIND, overrides);
        Some(run(&mut opened, &mut f, &input, seconds))
    }

    #[test]
    fn a_solid_border_runs_inside_the_edge() {
        let Some(s) = draw(16, json!({"width": 3.0, "color": "#ff0000"}), 0.0) else {
            return;
        };
        for (x, y) in [(0, 0), (2, 8), (15, 15), (8, 13)] {
            near(&s, x, y, R, 1);
        }
        near(&s, 3, 8, W, 1);
        near(&s, 8, 8, W, 1);
    }

    #[test]
    fn corners_round_and_clip_the_picture_or_leave_it() {
        let rounded = json!({"width": 0.0, "radius": 8.0});
        let Some(s) = draw(16, rounded, 0.0) else {
            return;
        };
        // A radius of half the side is a circle.
        near(&s, 0, 0, GONE, 0);
        near(&s, 8, 2, W, 1);
        near(&s, 8, 8, W, 1);
        let Some(s) = draw(16, json!({"width": 0.0, "radius": 8.0, "clip": false}), 0.0) else {
            return;
        };
        near(&s, 0, 0, W, 0);
        // Each its own: only the top-left corner rounds.
        let each = json!({
            "width": 0.0, "corners": "each", "radius_top_left": 8.0,
            "radius_top_right": 0.0, "radius_bottom_right": 0.0, "radius_bottom_left": 0.0
        });
        let Some(s) = draw(16, each, 0.0) else {
            return;
        };
        near(&s, 0, 0, GONE, 0);
        for (x, y) in [(15, 0), (15, 15), (0, 15)] {
            near(&s, x, y, W, 0);
        }
    }

    /// Dashes along the top edge alternate, and a speed moves them along.
    #[test]
    fn dashes_alternate_and_travel() {
        let dashed = |speed: f64| {
            json!({
                "width": 2.0, "color": "#ff0000", "style": "dashed", "dash": 4.0,
                "gap": 4.0, "speed": speed
            })
        };
        let Some(s) = draw(32, dashed(0.0), 0.0) else {
            return;
        };
        // Along the top, dash 0..4, gap 4..8, dash 8..12.
        near(&s, 2, 0, R, 1);
        near(&s, 6, 0, W, 1);
        near(&s, 10, 0, R, 1);
        // 4 px/s for one second moves the pattern 4 px clockwise.
        let Some(s) = draw(32, dashed(4.0), 1.0) else {
            return;
        };
        near(&s, 2, 0, W, 1);
        near(&s, 6, 0, R, 1);
    }

    /// An outer stroke after a gap: on a padded input (an item's effects, ADR-0125) it
    /// rings the box, and the box keeps its picture and inner stroke.
    #[test]
    fn padded_the_outer_stroke_rings_the_box() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = solid(&opened.compositor, 8, 8, W);
        let mut f = filter(
            KIND,
            json!({
                "width": 0.0, "outer_width": 2.0, "outer_gap": 2.0, "outer_color": "#0000ff"
            }),
        );
        f.set_pad(6);
        let s = run(&mut opened, &mut f, &input, 0.0);
        assert_eq!((s.width(), s.height()), (20, 20));
        // The box is 6..14; the gap 4..6 and 14..16; the stroke 2..4 and 16..18.
        near(&s, 10, 10, W, 1);
        near(&s, 5, 10, GONE, 1);
        near(&s, 3, 10, B, 1);
        near(&s, 16, 10, B, 1);
        near(&s, 0, 10, GONE, 1);
    }

    #[test]
    fn a_conic_gradient_runs_from_one_colour_to_the_other_round_the_edge() {
        let conic = json!({
            "width": 4.0, "fill": "conic", "color": "#ff0000", "color2": "#0000ff"
        });
        let Some(s) = draw(32, conic, 0.0) else {
            return;
        };
        // Straight up from the centre is all Color; straight down, half way round, all
        // Color 2.
        near(&s, 16, 1, R, 32);
        near(&s, 16, 30, B, 32);
    }

    #[test]
    fn a_linear_gradient_runs_across_at_its_angle() {
        let linear = json!({
            "width": 4.0, "fill": "linear", "angle": 0.0, "color": "#ff0000",
            "color2": "#0000ff"
        });
        let Some(s) = draw(32, linear, 0.0) else {
            return;
        };
        let (left, right) = (s.pixel(1, 16).expect("in"), s.pixel(30, 16).expect("in"));
        assert!(left[0] > 240 && left[2] < 90, "Color on the left: {left:?}");
        assert!(
            right[2] > 240 && right[0] < 90,
            "Color 2 on the right: {right:?}"
        );
    }

    /// Double: two lines of a third of the width each, with the picture between.
    #[test]
    fn a_double_border_is_two_lines() {
        let Some(s) = draw(
            24,
            json!({"width": 9.0, "color": "#ff0000", "style": "double"}),
            0.0,
        ) else {
            return;
        };
        near(&s, 1, 12, R, 1);
        near(&s, 4, 12, W, 1);
        near(&s, 7, 12, R, 1);
        near(&s, 10, 12, W, 1);
    }

    /// A 6x6 frame image, every row alike: two red corner columns each side and, between
    /// them, a blue column and a yellow one, so samples at texel centres read exactly.
    fn frame_image(opened: &crate::tests::Opened) -> crate::Image {
        const Y: [u8; 4] = [255, 255, 0, 255];
        let row = [R, R, B, Y, R, R];
        crate::filter::testing::image(opened, 6, &[row; 6].concat())
    }

    /// A 9-slice frame (#1294): the image's 2 px corners fill the stroke's 2 px corners
    /// unstretched, and its middle stretches along the side, or tiles at the corners'
    /// scale; no slice samples its neighbour's texels (#1377); inside the band the
    /// picture shows.
    #[test]
    fn a_frame_image_draws_the_stroke_as_nine_slices() {
        let Some(mut opened) = open() else {
            return;
        };
        const Y: [u8; 4] = [255, 255, 0, 255];
        let input = solid(&opened.compositor, 22, 22, W);
        let asset = json!({"type": "asset", "root": "app", "path": "frame.png"});
        for (edges, expected) in [
            // Stretched, the 2 middle image px span the 18 px between the corners; the
            // px next to a corner (2 and 19) take none of the corner's red (#1377).
            (
                "stretch",
                [
                    (0, R),
                    (1, R),
                    (2, B),
                    (6, B),
                    (15, Y),
                    (19, Y),
                    (20, R),
                    (21, R),
                ],
            ),
            // Tiled 1:1, they repeat every 2 px.
            (
                "tile",
                [
                    (0, R),
                    (1, R),
                    (2, B),
                    (3, Y),
                    (4, B),
                    (5, Y),
                    (19, Y),
                    (20, R),
                ],
            ),
        ] {
            let mut f = filter(
                KIND,
                json!({
                    "width": 2.0, "frame": asset, "frame_slice": 2.0,
                    "frame_edges": edges, "color": "#00ff00"
                }),
            );
            f.set_reference(Some(Arc::new(frame_image(&opened))));
            let s = run(&mut opened, &mut f, &input, 0.0);
            for (x, colour) in expected {
                near(&s, x, 0, colour, 2);
            }
            // The left side's band is the image's left corner columns; the middle is
            // the picture.
            near(&s, 0, 11, R, 2);
            near(&s, 11, 11, W, 1);
            near(&s, 2, 11, W, 1);
        }
        // Named but not handed over yet, the stroke draws in its colour.
        let mut f = filter(
            KIND,
            json!({"width": 2.0, "frame": asset, "color": "#00ff00"}),
        );
        let s = run(&mut opened, &mut f, &input, 0.0);
        near(&s, 11, 0, [0, 255, 0, 255], 1);
    }
}
