//! `scanlines`: the CRT and monitor-screen look for any overlay, lower third or screen
//! (#1406). Horizontal dark lines, optionally rolling, a red and blue fringe, animated
//! grain and a flicker, all on the engine's animation clock, so two machines draw the
//! same frame at the same time. Coverage is kept, so a transparent pixel stays
//! transparent.
//!
//! The lines count in the pixels the filter draws: on a source chain the source's, on an
//! item or a scene chain (ADR-0065) what that level shows. The time-driven parts (the
//! roll, the grain's seed and the flicker) are worked out on the CPU in `f64` from the
//! clock, never from the shader's day-wrapped `frame.time`.

use jstream_protocol::{KindDescriptor, ParamDescriptor, Settings};

use crate::error::RenderError;
use crate::filter::params::{Read, Uniforms, filter, float};
use crate::filter::{Blend, Ctx, Draw, Pass, Shader};
use crate::image::Image;

pub(super) const KIND: &str = "scanlines";

const SHADER: Shader = Shader {
    label: "filter-scanlines",
    source: include_str!("scanlines.wgsl"),
    blend: Blend::Replace,
};

/// The widest line period, px.
const MAX_SPACING: f64 = 64.0;
/// The fastest roll, px a second either way.
const MAX_SPEED: f64 = 2000.0;
/// The widest colour fringe, px.
const MAX_OFFSET: f64 = 16.0;
/// How many times a second the grain and the flicker change.
const STEPS_PER_SECOND: f64 = 60.0;

pub(super) fn descriptor() -> KindDescriptor {
    let described = |text: &str, param: ParamDescriptor| ParamDescriptor {
        description: Some(text.into()),
        ..param
    };
    filter(
        KIND,
        "Scanlines",
        vec![
            described(
                "How dark the lines are, in linear light: 0 draws none.",
                float("strength", "Strength", 0.0, 1.0, 0.35, ""),
            ),
            described(
                "One line every this many pixels.",
                float("spacing", "Spacing", 1.0, MAX_SPACING, 3.0, "px"),
            ),
            described(
                "How much of each spacing the dark line takes.",
                float("thickness", "Thickness", 0.0, 1.0, 0.5, ""),
            ),
            described(
                "How fast the lines roll; positive moves them down. 0 holds them still.",
                float("speed", "Roll speed", -MAX_SPEED, MAX_SPEED, 0.0, "px/s"),
            ),
            described(
                "Red is read this far to the left and blue to the right, a CRT's colour \
                 fringe. 0 is none.",
                float("rgb_offset", "Color fringe", 0.0, MAX_OFFSET, 0.0, "px"),
            ),
            described(
                "Noise that changes every frame, as film grain or a weak signal. 0 is none.",
                float("grain", "Grain", 0.0, 1.0, 0.0, ""),
            ),
            described(
                "How far the picture dims at random from frame to frame. 0 holds it steady.",
                float("flicker", "Flicker", 0.0, 1.0, 0.0, ""),
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

/// The settings, read once.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    strength: f64,
    spacing: f64,
    thickness: f64,
    speed: f64,
    rgb_offset: f64,
    grain: f64,
    flicker: f64,
}

fn values(settings: &Settings) -> Values {
    let read = Read(settings);
    Values {
        strength: within(read.f32("strength", 0.35), 0.0, 1.0, 0.35),
        spacing: within(read.f32("spacing", 3.0), 1.0, MAX_SPACING, 3.0),
        thickness: within(read.f32("thickness", 0.5), 0.0, 1.0, 0.5),
        speed: within(read.f32("speed", 0.0), -MAX_SPEED, MAX_SPEED, 0.0),
        rgb_offset: within(read.f32("rgb_offset", 0.0), 0.0, MAX_OFFSET, 0.0),
        grain: within(read.f32("grain", 0.0), 0.0, 1.0, 0.0),
        flicker: within(read.f32("flicker", 0.0), 0.0, 1.0, 0.0),
    }
}

/// The shader's `Params` at `seconds` on the animation clock.
fn uniforms(v: &Values, seconds: f64) -> Vec<u8> {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    let roll = (seconds * v.speed).rem_euclid(v.spacing);
    // Whole steps fit a u64 for any clock this engine runs.
    let step = (seconds * STEPS_PER_SECOND).floor() as u64;
    let seed = (step % 4096) as f64;
    let dim = v.flicker * random(step) * 0.5;
    Uniforms::new()
        .f32(v.spacing as f32)
        .f32(v.thickness as f32)
        .f32(v.strength as f32)
        .f32(roll as f32)
        .f32(v.rgb_offset as f32)
        .f32(v.grain as f32)
        .f32(seed as f32)
        .f32(dim as f32)
        .finish()
}

pub(super) struct Scanlines {
    values: Values,
}

impl Scanlines {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            values: values(settings),
        }
    }
}

impl Pass for Scanlines {
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
    use super::{KIND, Values, random, uniforms, values};
    use crate::filter::testing::{filter, run, settings};
    use crate::tests::{near, open, solid};
    use serde_json::json;

    const W: [u8; 4] = [255, 255, 255, 255];

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
            v,
            Values {
                strength: f64::from(0.35f32),
                spacing: 3.0,
                thickness: 0.5,
                speed: 0.0,
                rgb_offset: 0.0,
                grain: 0.0,
                flicker: 0.0,
            }
        );
        let v = values(&settings(
            KIND,
            json!({"strength": 3.0, "spacing": 0.0, "speed": -1e9, "flicker": -1.0}),
        ));
        assert_eq!(
            (v.strength, v.spacing, v.speed, v.flicker),
            (1.0, 1.0, -2000.0, 0.0)
        );
        assert_eq!(uniforms(&v, 0.0).len(), 32);
    }

    /// The roll wraps exactly to the spacing however late, and the flicker repeats for
    /// the same step on every machine.
    #[test]
    fn the_time_driven_parts_are_exact_and_repeatable() {
        let v = Values {
            speed: 2.0,
            spacing: 4.0,
            flicker: 1.0,
            ..values(&settings(KIND, json!({})))
        };
        let roll = |s: f64| f32::from_le_bytes(uniforms(&v, s)[12..16].try_into().unwrap());
        assert_eq!(roll(1.0), 2.0);
        assert_eq!(roll(1_000_000.5), 1.0);
        assert_eq!(random(7), random(7));
        assert_ne!(random(7), random(8));
        assert!((0.0..1.0).contains(&random(u64::MAX)));
        let dim = |s: f64| f32::from_le_bytes(uniforms(&v, s)[28..32].try_into().unwrap());
        assert_eq!(dim(10.0), dim(10.0 + 0.5 / 60.0), "one step holds");
        assert!((0.0..=0.5).contains(&dim(3.0)));
    }

    /// Spacing 4 and thickness 0.5: rows 0 and 1 of each 4 are the line, rows 2 and 3
    /// are untouched. At full strength the line is black.
    #[test]
    fn the_lines_darken_every_spacing() {
        let Some(s) = draw((4, 8), W, json!({"strength": 1.0, "spacing": 4.0}), 0.0) else {
            return;
        };
        for y in [0, 1, 4, 5] {
            near(&s, 1, y, [0, 0, 0, 255], 1);
        }
        for y in [2, 3, 6, 7] {
            near(&s, 1, y, W, 1);
        }
    }

    /// Rolling 1 px a second, a second later the lines are a row lower.
    #[test]
    fn the_lines_roll() {
        let look = json!({"strength": 1.0, "spacing": 4.0, "speed": 1.0});
        let Some(s) = draw((4, 8), W, look, 1.0) else {
            return;
        };
        near(&s, 1, 0, W, 1);
        near(&s, 1, 1, [0, 0, 0, 255], 1);
        near(&s, 1, 2, [0, 0, 0, 255], 1);
        near(&s, 1, 3, W, 1);
    }

    #[test]
    fn strength_0_leaves_the_picture_alone_and_coverage_is_kept() {
        let Some(s) = draw((4, 8), [120, 60, 30, 255], json!({"strength": 0.0}), 0.0) else {
            return;
        };
        for y in 0..8 {
            near(&s, 1, y, [120, 60, 30, 255], 1);
        }
        let full = json!({"strength": 1.0, "grain": 1.0, "rgb_offset": 2.0, "flicker": 1.0});
        let Some(s) = draw((4, 8), [0, 0, 0, 0], full, 0.3) else {
            return;
        };
        for y in 0..8 {
            near(&s, 1, y, [0, 0, 0, 0], 0);
        }
    }

    /// Grain differs from pixel to pixel and from frame to frame, and is the same for
    /// the same time.
    #[test]
    fn grain_moves_with_time_and_repeats_for_the_same_time() {
        const GREY: [u8; 4] = [128, 128, 128, 255];
        let look = json!({"strength": 0.0, "grain": 1.0});
        let (Some(a), Some(again), Some(later)) = (
            draw((16, 4), GREY, look.clone(), 1.0),
            draw((16, 4), GREY, look.clone(), 1.0),
            draw((16, 4), GREY, look, 1.5),
        ) else {
            return;
        };
        let row = |s: &crate::Snapshot| {
            (0..16)
                .map(|x| s.pixel(x, 1).expect("inside"))
                .collect::<Vec<_>>()
        };
        assert_eq!(row(&a), row(&again));
        assert_ne!(row(&a), row(&later));
        let distinct: std::collections::BTreeSet<_> = row(&a).into_iter().collect();
        assert!(distinct.len() > 4, "noise, not a flat change");
    }

    /// The fringe reads red from the left and blue from the right: at a red-to-blue
    /// edge, the pixel just right of it takes red too.
    #[test]
    fn the_fringe_shifts_red_and_blue_apart() {
        let Some(mut opened) = open() else {
            return;
        };
        let input = crate::tests::bands(
            &opened.compositor,
            4,
            4,
            &[[255, 0, 0, 255], [0, 0, 255, 255]],
        );
        let mut f = filter(KIND, json!({"strength": 0.0, "rgb_offset": 2.0}));
        let s = run(&mut opened, &mut f, &input, 0.0);
        let px = s.pixel(5, 1).expect("inside");
        assert!(px[0] > 200 && px[2] > 200, "red and blue both: {px:?}");
    }
}
