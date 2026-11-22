//! `hologram`: any picture (a camera, a screen capture, a page) as a projected hologram,
//! for a card floating in a 3D scene or a flat overlay. Its luminance takes one colour,
//! its dark parts turn see-through, and rolling scanlines, a sweep band, an edge glow,
//! grain, a colour fringe, a flicker and an occasional sideways glitch go over it.
//! `reveal` materialises it from one side behind a bright leading edge, so a Move
//! between a keyframe at `reveal` 0 and one at 1 (ADR-0127) plays its arrival and its
//! departure.
//!
//! The time-driven parts (the roll, the sweep, the flicker, the glitch and the grain's
//! seed) are worked out on the CPU in `f64` from the clock, as `scanlines` does, so two
//! machines draw the same frame at the same time. Coverage is kept: a transparent pixel
//! stays transparent. One draw of the input's size, a handful of texture reads a pixel.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, choice, color, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "hologram";

const SHADER: Shader = Shader {
    label: "filter-hologram",
    source: include_str!("hologram.wgsl"),
    blend: Blend::Replace,
};

/// The widest line period, px.
const MAX_SPACING: f64 = 64.0;
/// The fastest roll, px a second either way.
const MAX_SPEED: f64 = 2000.0;
/// The widest edge glow falloff, px.
const MAX_EDGE_WIDTH: f64 = 200.0;
/// The widest colour fringe, px.
const MAX_FRINGE: f64 = 16.0;
/// The shortest and longest sweep period, seconds.
const SWEEP_PERIOD: (f64, f64) = (0.5, 60.0);
/// How many times a second the grain and the flicker change.
const STEPS_PER_SECOND: f64 = 30.0;
/// How many times a second the glitch band may move.
const GLITCH_STEPS_PER_SECOND: f64 = 12.0;
/// The default colour, a pale cyan.
const DEFAULT_COLOR: &str = "#5ad2ff";

pub(super) fn descriptor() -> KindDescriptor {
    let described = |text: &str, param: ParamDescriptor| ParamDescriptor {
        description: Some(text.into()),
        ..param
    };
    filter(
        KIND,
        "Hologram",
        vec![
            described(
                "The hologram's colour. The picture's brightness is kept.",
                color("color", "Color", false, DEFAULT_COLOR),
            ),
            described(
                "How far the picture turns to the colour: 0 keeps its own colours.",
                float("tint", "Tint", 0.0, 1.0, 0.85, ""),
            ),
            described(
                "How far the picture's dark parts turn see-through: 0 keeps them opaque.",
                float("see_through", "See-through", 0.0, 1.0, 0.5, ""),
            ),
            described(
                "How bright the picture is drawn, a gain on its light: 1 keeps it.",
                float("brightness", "Brightness", 0.0, 4.0, 1.3, ""),
            ),
            described(
                "How dark the scanlines are: 0 draws none.",
                float("lines", "Scanlines", 0.0, 1.0, 0.35, ""),
            ),
            described(
                "One scanline every this many pixels.",
                float("spacing", "Line spacing", 1.0, MAX_SPACING, 3.0, "px"),
            ),
            described(
                "How fast the lines roll; positive moves them down. 0 holds them still.",
                float("speed", "Roll speed", -MAX_SPEED, MAX_SPEED, 30.0, "px/s"),
            ),
            described(
                "How bright the band that sweeps down the picture is: 0 is none.",
                float("sweep", "Sweep", 0.0, 1.0, 0.35, ""),
            ),
            described(
                "How long the band takes to sweep from top to bottom.",
                float(
                    "sweep_period",
                    "Sweep period",
                    SWEEP_PERIOD.0,
                    SWEEP_PERIOD.1,
                    4.0,
                    "s",
                ),
            ),
            described(
                "How bright the glow along the edges is: 0 is none.",
                float("edge", "Edge glow", 0.0, 1.0, 0.5, ""),
            ),
            described(
                "How far in from the edges the glow reaches.",
                float("edge_width", "Edge width", 1.0, MAX_EDGE_WIDTH, 8.0, "px"),
            ),
            described(
                "How far the picture dims at random from moment to moment. 0 holds it \
                 steady.",
                float("flicker", "Flicker", 0.0, 1.0, 0.2, ""),
            ),
            described(
                "How often a band of the picture jumps sideways for a moment. 0 is never.",
                float("glitch", "Glitch", 0.0, 1.0, 0.15, ""),
            ),
            described(
                "Red is read this far to the left and blue to the right. 0 is none.",
                float("fringe", "Color fringe", 0.0, MAX_FRINGE, 1.5, "px"),
            ),
            described(
                "Noise that changes every moment. 0 is none.",
                float("grain", "Grain", 0.0, 1.0, 0.1, ""),
            ),
            described(
                "How much of the hologram is materialised: 0 shows nothing, 1 all of it. \
                 Between them a bright edge crosses it. Glide it with a Move between \
                 keyframes to project it in and out.",
                float("reveal", "Reveal", 0.0, 1.0, 1.0, ""),
            ),
            described(
                "Where the reveal starts.",
                choice(
                    "reveal_from",
                    "Reveal from",
                    &[
                        ("bottom", "Bottom"),
                        ("top", "Top"),
                        ("middle", "The middle out"),
                    ],
                    "bottom",
                ),
            ),
        ],
    )
}

/// `v` clamped to `min..=max`, `fallback` when it isn't finite.
fn within(v: f32, min: f64, max: f64, fallback: f64) -> f64 {
    let v = f64::from(v);
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        fallback
    }
}

/// A random number in `0..1` for step `step`: the same on every machine.
fn random(step: u64) -> f64 {
    // SplitMix64.
    let mut z = step.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Where the reveal starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum From {
    Bottom,
    Top,
    Middle,
}

/// The settings, read once.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    color: [f32; 4],
    tint: f64,
    see_through: f64,
    brightness: f64,
    lines: f64,
    spacing: f64,
    speed: f64,
    sweep: f64,
    sweep_period: f64,
    edge: f64,
    edge_width: f64,
    flicker: f64,
    glitch: f64,
    fringe: f64,
    grain: f64,
    reveal: f64,
    from: From,
}

fn values(settings: &Settings) -> Values {
    let read = Read(settings);
    let [r, g, b] =
        [0x5a_u8, 0xd2, 0xff].map(|c| crate::color::srgb_channel_to_linear(f32::from(c) / 255.0));
    let mut color = read.color_linear("color", [r, g, b, 1.0]);
    color[3] = 1.0;
    let f = |id: &str, min: f64, max: f64, default: f64| {
        within(read.f32(id, default as f32), min, max, default)
    };
    Values {
        color,
        tint: f("tint", 0.0, 1.0, 0.85),
        see_through: f("see_through", 0.0, 1.0, 0.5),
        brightness: f("brightness", 0.0, 4.0, 1.3),
        lines: f("lines", 0.0, 1.0, 0.35),
        spacing: f("spacing", 1.0, MAX_SPACING, 3.0),
        speed: f("speed", -MAX_SPEED, MAX_SPEED, 30.0),
        sweep: f("sweep", 0.0, 1.0, 0.35),
        sweep_period: f("sweep_period", SWEEP_PERIOD.0, SWEEP_PERIOD.1, 4.0),
        edge: f("edge", 0.0, 1.0, 0.5),
        edge_width: f("edge_width", 1.0, MAX_EDGE_WIDTH, 8.0),
        flicker: f("flicker", 0.0, 1.0, 0.2),
        glitch: f("glitch", 0.0, 1.0, 0.15),
        fringe: f("fringe", 0.0, MAX_FRINGE, 1.5),
        grain: f("grain", 0.0, 1.0, 0.1),
        reveal: f("reveal", 0.0, 1.0, 1.0),
        from: match read.choice("reveal_from", "bottom") {
            "top" => From::Top,
            "middle" => From::Middle,
            _ => From::Bottom,
        },
    }
}

/// The time-driven values at `seconds` on the animation clock.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Moment {
    roll: f64,
    sweep_y: f64,
    dim: f64,
    glitch_y: f64,
    glitch_h: f64,
    glitch_dx: f64,
    seed: f64,
}

fn moment(v: &Values, seconds: f64) -> Moment {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    let roll = (seconds * v.speed).rem_euclid(v.spacing);
    // The band enters above the top and leaves below the bottom.
    let sweep_y = (seconds / v.sweep_period).rem_euclid(1.0) * 1.4 - 0.2;
    // Whole steps fit a u64 for any clock this engine runs.
    let step = (seconds * STEPS_PER_SECOND).floor() as u64;
    // A low shimmer every step and, now and then, a deeper dip.
    let shimmer = 0.25 * random(step);
    let dip = if random(step ^ 0xD1B5_4A32_D192_ED03) < 0.06 {
        0.6
    } else {
        0.0
    };
    let dim = (v.flicker * (shimmer + dip)).min(0.9);
    let g = (seconds * GLITCH_STEPS_PER_SECOND).floor() as u64 ^ 0x2545_F491_4F6C_DD1D;
    let (glitch_y, glitch_h, glitch_dx) = if v.glitch > 0.0 && random(g) < v.glitch * 0.3 {
        let side = if random(g.wrapping_add(3)) < 0.5 {
            -1.0
        } else {
            1.0
        };
        (
            random(g.wrapping_add(1)),
            0.01 + 0.04 * random(g.wrapping_add(2)),
            side * (0.005 + 0.03 * v.glitch * random(g.wrapping_add(4))),
        )
    } else {
        (0.0, 0.0, 0.0)
    };
    Moment {
        roll,
        sweep_y,
        dim,
        glitch_y,
        glitch_h,
        glitch_dx,
        seed: (step % 4096) as f64,
    }
}

/// How bright the reveal's leading edge is at `reveal`: none at either end, so a
/// finished reveal draws no line, full through the middle.
fn lead(reveal: f64) -> f64 {
    let rise = (reveal * 20.0).clamp(0.0, 1.0);
    let fall = ((1.0 - reveal) * 10.0).clamp(0.0, 1.0);
    1.5 * rise * fall
}

/// The shader's `Params` at `seconds` on the animation clock.
fn uniforms(v: &Values, seconds: f64) -> Vec<u8> {
    let m = moment(v, seconds);
    Uniforms::new()
        .vec4(v.color)
        .f32(v.tint as f32)
        .f32(v.see_through as f32)
        .f32(v.brightness as f32)
        .f32(v.lines as f32)
        .f32(v.spacing as f32)
        .f32(m.roll as f32)
        .f32(v.sweep as f32)
        .f32(m.sweep_y as f32)
        .f32(v.edge as f32)
        .f32(v.edge_width as f32)
        .f32(m.dim as f32)
        .f32(m.glitch_y as f32)
        .f32(m.glitch_h as f32)
        .f32(m.glitch_dx as f32)
        .f32(v.fringe as f32)
        .f32(v.grain as f32)
        .f32(m.seed as f32)
        .f32(v.reveal as f32)
        .u32(match v.from {
            From::Bottom => 0,
            From::Top => 1,
            From::Middle => 2,
        })
        .f32(lead(v.reveal) as f32)
        .finish()
}

pub(super) struct Hologram {
    values: Values,
}

impl Hologram {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
        }
    }
}

impl Pass for Hologram {
    fn update(&mut self, settings: &Settings) {
        self.values = values(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let params = uniforms(&self.values, ctx.clock().seconds);
        ctx.draw(&Draw {
            shader: SHADER,
            input,
            aux: None,
            params: &params,
            target: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{From, KIND, Values, lead, moment, uniforms, values};
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{open, solid};
    use serde_json::json;

    const W: [u8; 4] = [255, 255, 255, 255];

    /// Every effect but the tint off, so a pixel's colour is the tint's alone.
    fn plain() -> serde_json::Value {
        json!({
            "see_through": 0.0, "brightness": 1.0, "lines": 0.0, "sweep": 0.0, "edge": 0.0, "flicker": 0.0,
            "glitch": 0.0, "fringe": 0.0, "grain": 0.0
        })
    }

    fn with(mut base: serde_json::Value, more: serde_json::Value) -> serde_json::Value {
        for (k, v) in more.as_object().expect("an object") {
            base[k] = v.clone();
        }
        base
    }

    fn draw(
        size: (u32, u32),
        input: [u8; 4],
        overrides: serde_json::Value,
        seconds: f64,
    ) -> Option<crate::Snapshot> {
        let mut opened = open()?;
        let input = solid(&opened.compositor, size.0, size.1, input);
        let mut filter = filter(KIND, overrides);
        Some(run(&mut opened, &mut filter, &input, seconds))
    }

    #[test]
    fn settings_clamp_and_map_to_uniforms() {
        let v = values(&settings(KIND, json!({})));
        assert_eq!(
            (v.tint, v.reveal, v.from),
            (f64::from(0.85f32), 1.0, From::Bottom)
        );
        assert!(
            v.color[2] > v.color[1] && v.color[1] > v.color[0],
            "{:?}",
            v.color
        );
        let v = values(&settings(
            KIND,
            json!({"reveal": 3.0, "spacing": 0.0, "speed": -1e9, "flicker": -1.0,
                   "reveal_from": "middle", "sweep_period": 0.0}),
        ));
        assert_eq!(
            (
                v.reveal,
                v.spacing,
                v.speed,
                v.flicker,
                v.from,
                v.sweep_period
            ),
            (1.0, 1.0, -2000.0, 0.0, From::Middle, 0.5)
        );
        assert_eq!(uniforms(&v, 0.0).len(), 96);
        assert_eq!(v.brightness, f64::from(1.3f32));
    }

    /// The roll wraps to the spacing however late, the sweep stays on its path, and the
    /// flicker and glitch repeat for the same moment on every machine.
    #[test]
    fn the_time_driven_parts_are_exact_and_repeatable() {
        let v = Values {
            speed: 2.0,
            spacing: 4.0,
            flicker: 1.0,
            glitch: 1.0,
            ..values(&settings(KIND, json!({})))
        };
        assert_eq!(moment(&v, 1.0).roll, 2.0);
        assert_eq!(moment(&v, 1_000_000.5).roll, 1.0);
        assert_eq!(moment(&v, 12.3), moment(&v, 12.3));
        let mut glitched = 0;
        for i in 0..2000 {
            let m = moment(&v, f64::from(i) * 0.05);
            assert!((-0.2..=1.2).contains(&m.sweep_y));
            assert!((0.0..=0.9).contains(&m.dim));
            assert!(m.glitch_dx.abs() <= 0.035);
            glitched += usize::from(m.glitch_dx != 0.0);
        }
        // At glitch 1 a band jumps on about 30% of the steps.
        assert!((300..900).contains(&glitched), "{glitched}");
        let calm = Values { glitch: 0.0, ..v };
        assert!((0..2000).all(|i| moment(&calm, f64::from(i) * 0.05).glitch_dx == 0.0));
    }

    #[test]
    fn the_leading_edge_shows_only_mid_reveal() {
        assert_eq!(lead(0.0), 0.0);
        assert_eq!(lead(1.0), 0.0);
        assert!(lead(0.5) > 1.0);
    }

    /// White takes the colour: blue-dominant for the default cyan, and a tint of 0
    /// leaves it white.
    #[test]
    fn white_takes_the_colour() {
        let Some(s) = draw((8, 8), W, plain(), 0.0) else {
            return;
        };
        let px = s.pixel(4, 4).expect("inside");
        assert!(px[2] >= px[1] && px[1] > px[0] && px[3] == 255, "{px:?}");
        let Some(s) = draw((8, 8), W, with(plain(), json!({"tint": 0.0})), 0.0) else {
            return;
        };
        crate::tests::near(&s, 4, 4, W, 1);
    }

    /// Black turns see-through, and a transparent input stays transparent whatever is on.
    #[test]
    fn dark_parts_turn_see_through_and_coverage_is_kept() {
        let look = with(plain(), json!({"see_through": 1.0}));
        let Some(s) = draw((8, 8), [0, 0, 0, 255], look, 0.0) else {
            return;
        };
        assert_eq!(s.pixel(4, 4).expect("inside")[3], 0);
        let all = json!({"see_through": 0.0, "edge": 1.0, "sweep": 1.0, "grain": 1.0,
                         "fringe": 2.0, "glitch": 1.0, "reveal": 0.5});
        let Some(s) = draw((8, 8), [0, 0, 0, 0], all, 0.3) else {
            return;
        };
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(s.pixel(x, y).expect("inside"), [0, 0, 0, 0], "{x},{y}");
            }
        }
    }

    /// Revealed from the bottom, the top rows are empty and the bottom rows drawn; at 0
    /// nothing is drawn.
    #[test]
    fn the_reveal_materialises_from_the_bottom() {
        let look = with(plain(), json!({"reveal": 0.4}));
        let Some(s) = draw((8, 64), W, look, 0.0) else {
            return;
        };
        assert_eq!(s.pixel(4, 2).expect("inside")[3], 0, "above the edge");
        assert_eq!(
            s.pixel(4, 62).expect("inside")[3],
            255,
            "well below the edge"
        );
        let Some(s) = draw((8, 64), W, with(plain(), json!({"reveal": 0.0})), 0.0) else {
            return;
        };
        assert!((0..64).all(|y| s.pixel(4, y).expect("inside")[3] == 0));
        let look = with(plain(), json!({"reveal": 0.4, "reveal_from": "top"}));
        let Some(s) = draw((8, 64), W, look, 0.0) else {
            return;
        };
        assert_eq!(s.pixel(4, 2).expect("inside")[3], 255, "from the top");
        assert_eq!(s.pixel(4, 62).expect("inside")[3], 0);
    }

    /// The edge glow brightens the border more than the centre.
    #[test]
    fn the_edges_glow() {
        let grey = [90, 90, 90, 255];
        let look = with(
            plain(),
            json!({"edge": 1.0, "edge_width": 3.0, "tint": 0.0}),
        );
        let Some(s) = draw((32, 32), grey, look, 0.0) else {
            return;
        };
        let edge = s.pixel(0, 16).expect("inside");
        let centre = s.pixel(16, 16).expect("inside");
        assert!(edge[2] > centre[2] + 20, "{edge:?} vs {centre:?}");
    }
}
