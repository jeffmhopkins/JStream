//! The `shape` source: a rectangle, rounded rectangle, pill or ellipse of an exact size,
//! filled with a solid colour or a linear or radial gradient, with an optional border.
//!
//! It is what a designer draws plates, bars, chips, badges, dividers and dots with. A
//! `color` source is a full frame of one colour, which a box of another aspect
//! letterboxes; a shape is drawn at the size it is placed at, with anti-aliased edges,
//! so its corners and border stay crisp. Gradients mix in linear light and are
//! dithered to 8 bits, so a soft sweep doesn't band. It is drawn once, on the CPU, on
//! create and on each update; every setting is live, its size too.

use crate::backdrop::{Encoder, byte, hash, linear};
use crate::color::{frame_size, parse_color, rgba_frame};
use crate::describe::{kind, param, size};
use crate::source::setting;
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings, VisibleIf,
};
use serde_json::json;
use std::sync::Arc;

/// The `shape` kind.
pub(crate) struct ShapeKind(KindDescriptor);

/// The outlines, by option value, with their label.
const OUTLINES: [(&str, &str); 2] = [("rectangle", "Rectangle"), ("ellipse", "Ellipse")];

/// The fills, by option value, with their label.
const FILLS: [(&str, &str); 3] = [
    ("solid", "Solid"),
    ("linear", "Linear gradient"),
    ("radial", "Radial gradient"),
];

/// The largest corner radius and border width, in pixels.
const MAX_STROKE: f64 = 4096.0;

impl ShapeKind {
    pub(crate) fn new() -> Self {
        let options = |list: &[(&str, &str)]| ParamType::Enum {
            options: list
                .iter()
                .map(|(value, label)| EnumOption {
                    value: (*value).into(),
                    label: (*label).into(),
                })
                .collect(),
        };
        let pixels = || ParamType::Float {
            min: Some(0.0),
            max: Some(MAX_STROKE),
            step: Some(1.0),
            unit: Some("px".into()),
        };
        let colour = || ParamType::Color { alpha: true };
        let p = |id: &str, label: &str, ty: ParamType, default, text: &str| ParamDescriptor {
            live: true,
            description: Some(text.into()),
            ..param(id, label, ty, default)
        };
        let gradient = |d: ParamDescriptor, fill: &str| ParamDescriptor {
            visible_if: Some(VisibleIf {
                param: "fill".into(),
                equals: json!(fill),
            }),
            ..d
        };
        Self(KindDescriptor {
            params: vec![
                p(
                    "shape",
                    "Shape",
                    options(&OUTLINES),
                    json!("rectangle"),
                    "A rectangle (rounded by Corner radius, a pill when it is half the \
                     height) or an ellipse filling the size (a circle when square).",
                ),
                p(
                    "width",
                    "Width",
                    size(),
                    json!(400),
                    "Draw it at the size of the box it is placed in, so it isn't scaled.",
                ),
                p("height", "Height", size(), json!(120), "See Width."),
                p(
                    "radius",
                    "Corner radius",
                    pixels(),
                    json!(0.0),
                    "A rectangle's corner radius; at most half the shorter side, which \
                     makes the short ends round.",
                ),
                p(
                    "fill",
                    "Fill",
                    options(&FILLS),
                    json!("solid"),
                    "Solid Color, or a gradient from Color to End color.",
                ),
                p(
                    "color",
                    "Color",
                    colour(),
                    json!("#ffffffff"),
                    "The fill, or a gradient's start.",
                ),
                p(
                    "color2",
                    "End color",
                    colour(),
                    json!("#000000ff"),
                    "A gradient's end: at the far side of a linear one, the edge of a \
                     radial one.",
                ),
                gradient(
                    p(
                        "angle",
                        "Angle",
                        ParamType::Float {
                            min: Some(0.0),
                            max: Some(360.0),
                            step: Some(1.0),
                            unit: Some("deg".into()),
                        },
                        json!(90.0),
                        "The direction a linear gradient runs from Color to End color: \
                         0 is left to right, 90 top to bottom.",
                    ),
                    "linear",
                ),
                p(
                    "border_width",
                    "Border width",
                    pixels(),
                    json!(0.0),
                    "A border inside the edge; 0 is none.",
                ),
                p(
                    "border_color",
                    "Border color",
                    colour(),
                    json!("#ffffffff"),
                    "The border's color.",
                ),
            ],
            ..kind("shape", "Shape", true, false)
        })
    }
}

impl SourceKind for ShapeKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let frame = draw(settings, &ctx.limits)?;
        Ok(Box::new(ShapeSource {
            limits: ctx.limits,
            frame,
        }))
    }
}

/// A running `shape` source. Its one frame is drawn on create and on each update.
struct ShapeSource {
    limits: Limits,
    frame: Arc<VideoFrame>,
}

impl Source for ShapeSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        // Every setting is live, the size too: the frame is drawn again at the new one.
        self.frame = draw(settings, &self.limits)?;
        Ok(())
    }
}

fn draw(settings: &Settings, limits: &Limits) -> Result<Arc<VideoFrame>, SourceError> {
    let size = frame_size(settings, limits)?;
    let look = Look::read(settings)?;
    Ok(Arc::new(paint(size, &look)?))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Outline {
    Rectangle,
    Ellipse,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fill {
    Solid,
    Linear,
    Radial,
}

/// A colour as premultiplied linear light, so gradients and edges mix without dark
/// fringes where alpha changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Paint([f32; 4]);

impl Paint {
    fn from_rgba(rgba: [u8; 4]) -> Paint {
        let a = f32::from(rgba[3]) / 255.0;
        let [r, g, b] = linear(rgba);
        Paint([r * a, g * a, b * a, a])
    }

    fn mix(self, other: Paint, t: f32) -> Paint {
        let mut out = [0f32; 4];
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.0[i] + (other.0[i] - self.0[i]) * t;
        }
        Paint(out)
    }

    fn scaled(self, k: f32) -> Paint {
        Paint(self.0.map(|v| v * k))
    }
}

/// Every setting but the size, read and checked.
#[derive(Clone, Copy, Debug)]
struct Look {
    outline: Outline,
    radius: f32,
    fill: Fill,
    color: Paint,
    color2: Paint,
    angle_deg: f32,
    border_width: f32,
    border_color: Paint,
}

impl Look {
    fn read(settings: &Settings) -> Result<Look, SourceError> {
        let outline = match setting(settings, "shape")?.as_str() {
            Some("rectangle") => Outline::Rectangle,
            Some("ellipse") => Outline::Ellipse,
            _ => return Err(bad("shape", "must be `rectangle` or `ellipse`")),
        };
        let fill = match setting(settings, "fill")?.as_str() {
            Some("solid") => Fill::Solid,
            Some("linear") => Fill::Linear,
            Some("radial") => Fill::Radial,
            _ => return Err(bad("fill", "must be `solid`, `linear` or `radial`")),
        };
        Ok(Look {
            outline,
            radius: number(settings, "radius", 0.0, MAX_STROKE)?,
            fill,
            color: Paint::from_rgba(colour(settings, "color")?),
            color2: Paint::from_rgba(colour(settings, "color2")?),
            angle_deg: number(settings, "angle", 0.0, 360.0)?,
            border_width: number(settings, "border_width", 0.0, MAX_STROKE)?,
            border_color: Paint::from_rgba(colour(settings, "border_color")?),
        })
    }
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

fn colour(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a color as `#rrggbb` or `#rrggbbaa`"))
}

fn number(settings: &Settings, id: &str, min: f64, max: f64) -> Result<f32, SourceError> {
    setting(settings, id)?
        .as_f64()
        .filter(|v| (min..=max).contains(v))
        // Within ±4096, which an f32 holds exactly enough.
        .map(|v| v as f32)
        .ok_or_else(|| bad(id, &format!("must be a number from {min} to {max}")))
}

/// The signed distance from `(x, y)` to the outline, in pixels: negative inside. `(x, y)`
/// is relative to the frame's centre, `half` its half size.
fn distance(look: &Look, half: (f32, f32), x: f32, y: f32) -> f32 {
    match look.outline {
        Outline::Rectangle => {
            let r = look.radius.min(half.0).min(half.1);
            let qx = x.abs() - (half.0 - r);
            let qy = y.abs() - (half.1 - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            outside + qx.max(qy).min(0.0) - r
        }
        Outline::Ellipse => {
            // The implicit function over its gradient's length: exact on the outline,
            // which is where anti-aliasing needs it.
            let (a, b) = (half.0.max(0.5), half.1.max(0.5));
            let f = (x * x) / (a * a) + (y * y) / (b * b) - 1.0;
            let (gx, gy) = (2.0 * x / (a * a), 2.0 * y / (b * b));
            let grad = (gx * gx + gy * gy).sqrt();
            if grad > 1e-6 { f / grad } else { -a.min(b) }
        }
    }
}

/// Draws the shape at `size`: RGBA in sRGB with straight alpha, transparent outside it.
fn paint(size: FrameSize, look: &Look) -> Result<VideoFrame, SourceError> {
    let (w, h) = (size.width() as usize, size.height() as usize);
    let half = (w as f32 / 2.0, h as f32 / 2.0);
    // A linear gradient runs along `dir`, from the frame's nearest corner to its
    // farthest, so its ends land on the shape's extent in that direction.
    let theta = look.angle_deg.to_radians();
    let dir = (theta.cos(), theta.sin());
    let reach = (half.0 * dir.0.abs() + half.1 * dir.1.abs()).max(0.5);
    // A radial one reaches the end colour at the farthest point of an ellipse
    // inscribed in the frame, which is the shape's edge.
    let srgb = Encoder::new();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5 - half.0, y as f32 + 0.5 - half.1);
            let d = distance(look, half, px, py);
            // Anti-aliased coverage of the outline, and of the inside of the border.
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let fill = match look.fill {
                Fill::Solid => look.color,
                Fill::Linear => {
                    let t = ((px * dir.0 + py * dir.1) / reach * 0.5 + 0.5).clamp(0.0, 1.0);
                    look.color.mix(look.color2, t)
                }
                Fill::Radial => {
                    let (nx, ny) = (px / half.0.max(0.5), py / half.1.max(0.5));
                    let t = (nx * nx + ny * ny).sqrt().clamp(0.0, 1.0);
                    look.color.mix(look.color2, t)
                }
            };
            let paint = if look.border_width > 0.0 {
                let inner = (0.5 - (d + look.border_width)).clamp(0.0, 1.0);
                look.border_color.mix(fill, inner)
            } else {
                fill
            }
            .scaled(coverage);
            let alpha = paint.0[3];
            if alpha <= 0.0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // Dithered to 8 bits, so a gradient doesn't band; the same on every machine.
            let dither = hash(x as u32, y as u32, 0x5ba9) - 0.5;
            let channel = |i: usize| byte(srgb.encode(paint.0[i] / alpha) * 255.0 + dither);
            rgba.extend_from_slice(&[channel(0), channel(1), channel(2), byte(alpha * 255.0)]);
        }
    }
    Ok(rgba_frame(size, rgba, MediaTime::zero(Timebase::MILLIS))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(outline: Outline, radius: f32) -> Look {
        let white = Paint::from_rgba([255, 255, 255, 255]);
        Look {
            outline,
            radius,
            fill: Fill::Solid,
            color: white,
            color2: white,
            angle_deg: 0.0,
            border_width: 0.0,
            border_color: white,
        }
    }

    #[test]
    fn a_rectangle_is_negative_inside_and_zero_on_its_edge() {
        let l = look(Outline::Rectangle, 0.0);
        assert!(distance(&l, (50.0, 20.0), 0.0, 0.0) < 0.0);
        assert!((distance(&l, (50.0, 20.0), 50.0, 0.0)).abs() < 1e-4);
        assert!((distance(&l, (50.0, 20.0), 0.0, 25.0) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn the_radius_is_clamped_to_a_pill() {
        // A radius past half the height rounds the ends fully, no further.
        let l = look(Outline::Rectangle, 500.0);
        // The corner of the box is outside a pill by r·(√2 − 1).
        let d = distance(&l, (50.0, 20.0), 50.0, 20.0);
        assert!((d - 20.0 * (2f32.sqrt() - 1.0)).abs() < 1e-3, "{d}");
    }

    #[test]
    fn an_ellipse_passes_through_its_axes() {
        let l = look(Outline::Ellipse, 0.0);
        assert!(distance(&l, (40.0, 10.0), 40.0, 0.0).abs() < 1e-3);
        assert!(distance(&l, (40.0, 10.0), 0.0, 10.0).abs() < 1e-3);
        assert!(distance(&l, (40.0, 10.0), 0.0, 0.0) < 0.0);
    }

    #[test]
    fn mixing_premultiplied_keeps_a_fade_to_clear_its_colour() {
        // Red fading to transparent black stays red, only fainter, halfway.
        let red = Paint::from_rgba([255, 0, 0, 255]);
        let clear = Paint::from_rgba([0, 0, 0, 0]);
        let mid = red.mix(clear, 0.5);
        assert!((mid.0[3] - 0.5).abs() < 1e-6);
        assert!((mid.0[0] / mid.0[3] - 1.0).abs() < 1e-6);
        assert_eq!(mid.0[1], 0.0);
    }
}
