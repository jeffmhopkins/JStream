//! The bounded gaussian blur that blur, drop shadow and glow share, and the box
//! downsample that blur and pixelate share.
//!
//! A blur of radius `r` px spans `r` on each side (sigma `r / 3`). It runs at a working
//! scale of 1, 2, 4, 8 or 16 (the smallest that keeps `r / scale` within [`MAX_TAPS`]):
//! a box downsample to that scale, then a horizontal and a vertical pass of at most
//! `2 * MAX_TAPS + 1` taps each, read back bilinear at full size. So every radius up
//! to [`MAX_RADIUS`] costs at most 64 loads per downsampled texel and 17 per blur pass,
//! and three scratch layers. The tap bound is low because a software rasterizer pays
//! for every load (#1070): above 8 px, a blur at half scale or less costs a fraction of
//! one at full scale, and its gaussian is still several texels wide, so the bilinear
//! read back stays smooth.

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Shader};
use crate::image::Image;

/// Taps on each side of a blur pass at the working scale.
pub(super) const MAX_TAPS: u32 = 8;
/// The coarsest working scale.
pub(super) const MAX_SCALE: u32 = 16;
/// The largest radius, px, the bound covers; larger radii are clamped to it.
pub(super) const MAX_RADIUS: f32 = (MAX_TAPS * MAX_SCALE) as f32;
/// Samples on each axis of a downsampled block: blocks up to this size are averaged
/// exactly, larger ones on an evenly spaced grid.
pub(super) const MAX_BLOCK_SAMPLES: u32 = 8;

const DOWN: Shader = Shader {
    label: "multipass-down",
    source: wgsl!("down.wgsl"),
    blend: Blend::Replace,
};

const GAUSS: Shader = Shader {
    label: "multipass-gauss",
    source: wgsl!("gauss.wgsl"),
    blend: Blend::Replace,
};

/// What lies outside the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    /// The nearest edge texel: a whole frame, whose edges should not darken.
    Clamp,
    /// Transparent: a shape placed in a larger frame.
    Transparent,
}

impl Edge {
    fn code(self) -> u32 {
        match self {
            Self::Clamp => 0,
            Self::Transparent => 1,
        }
    }
}

/// How a blur of some radius runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Plan {
    /// Full-size pixels per working texel: 1, 2, 4, 8 or 16.
    pub scale: u32,
    /// Taps on each side of each pass, at most [`MAX_TAPS`]; 0 is no blur.
    pub taps: u32,
    /// The gaussian's sigma in working texels.
    pub sigma: f32,
}

/// The plan for a blur of `radius` px.
pub(super) fn plan(radius: f32) -> Plan {
    let radius = if radius.is_finite() {
        radius.clamp(0.0, MAX_RADIUS)
    } else {
        0.0
    };
    let mut scale = 1;
    while radius / scale as f32 > MAX_TAPS as f32 && scale < MAX_SCALE {
        scale *= 2;
    }
    let r = radius / scale as f32;
    Plan {
        scale,
        taps: (r.ceil() as u32).min(MAX_TAPS),
        sigma: (r / 3.0).max(1e-3),
    }
}

/// The normalised weight of the centre texel (`[0]`) and of each texel `i` either side
/// of it (`[i]`, for `i` up to `plan.taps`) in one pass of `plan`: `exp(-i² / 2σ²)`
/// over the sum of all `2 * taps + 1`. The rest are zero. Padded to whole `vec4`s.
pub(super) fn weights(plan: Plan) -> [f32; WEIGHTS] {
    let taps = plan.taps.min(MAX_TAPS) as usize;
    let k = 1.0 / (2.0 * plan.sigma * plan.sigma);
    let mut weights = [0.0; WEIGHTS];
    for (i, w) in weights.iter_mut().enumerate().take(taps + 1) {
        let i = i as f32;
        *w = (-i * i * k).exp();
    }
    let total = weights[0] + 2.0 * weights[1..].iter().sum::<f32>();
    for w in &mut weights {
        *w /= total;
    }
    weights
}

/// `weights` on each side of the centre read in pairs, as one bilinear sample of the
/// two: pair `k` is texels `2k + 1` and `2k + 2`, its weight their sum and its offset
/// from the centre their weighted mean, in texels. The second of the last pair is 0
/// when `taps` is odd, so that sample lands on texel `taps` alone. Returns the pairs
/// and how many there are.
pub(super) fn pairs(weights: &[f32; WEIGHTS], taps: u32) -> ([[f32; 2]; PAIRS], usize) {
    let taps = taps.min(MAX_TAPS) as usize;
    let count = taps.div_ceil(2);
    let mut pairs = [[0.0; 2]; PAIRS];
    for (k, pair) in pairs.iter_mut().enumerate().take(count) {
        let (near, far) = (2 * k + 1, 2 * k + 2);
        let a = weights[near];
        let b = if far <= taps { weights[far] } else { 0.0 };
        let sum = a + b;
        let offset = if sum > 0.0 {
            (near as f32 * a + far as f32 * b) / sum
        } else {
            near as f32
        };
        *pair = [sum, offset];
    }
    (pairs, count)
}

/// The pairs a pass's uniform holds: half of [`MAX_TAPS`], rounded up to whole `vec4`s
/// of two.
const PAIRS: usize = (MAX_TAPS as usize).div_ceil(2).div_ceil(2) * 2;

/// The weights a pass's uniform holds: [`MAX_TAPS`] + 1, rounded up to whole `vec4`s.
const WEIGHTS: usize = (MAX_TAPS as usize + 1).div_ceil(4) * 4;

/// `n` divided by `d`, rounded up, at least 1.
pub(super) fn div_ceil(n: u32, d: u32) -> u32 {
    n.div_ceil(d.max(1)).max(1)
}

/// A box downsample of `input` placed at `origin` in a frame of `frame` pixels: one
/// texel per `block` x `block` square of the frame, the mean of that square.
pub(super) fn down(
    ctx: &mut Ctx<'_>,
    input: &Image,
    block: u32,
    frame: (u32, u32),
    origin: (i32, i32),
    edge: Edge,
) -> Result<Image, RenderError> {
    let block = block.max(1);
    let target = ctx.scratch(div_ceil(frame.0, block), div_ceil(frame.1, block))?;
    let params = Uniforms::new()
        .i32(origin.0)
        .i32(origin.1)
        .i32(block as i32)
        .i32(block.min(MAX_BLOCK_SAMPLES) as i32)
        .i32(frame.0 as i32)
        .i32(frame.1 as i32)
        .u32(edge.code())
        .finish();
    ctx.draw(&Draw {
        shader: DOWN,
        input,
        aux: None,
        params: &params,
        target: &target,
    })?;
    Ok(target)
}

/// A blurred image and the full-size pixels each of its texels covers.
pub(super) struct Blurred {
    pub image: Image,
    pub scale: u32,
}

/// Blurs `input`, placed at `origin` in a frame of `frame` pixels, by `radius` px. The
/// result covers the frame at the plan's working scale: frame pixel `(x, y)` is texel
/// `(x, y) / scale`, so read it bilinear at `pos / (scale * size)`.
pub(super) fn blur(
    ctx: &mut Ctx<'_>,
    input: &Image,
    radius: f32,
    frame: (u32, u32),
    origin: (i32, i32),
    edge: Edge,
) -> Result<Blurred, RenderError> {
    let plan = plan(radius);
    // A whole frame at full scale blurs straight from the input; anything else is
    // placed (and downsampled) first.
    let placed = if edge == Edge::Clamp && plan.scale == 1 && plan.taps > 0 {
        None
    } else {
        Some(down(ctx, input, plan.scale, frame, origin, edge)?)
    };
    let placed = match placed {
        Some(image) if plan.taps == 0 => {
            return Ok(Blurred {
                image,
                scale: plan.scale,
            });
        }
        placed => placed,
    };
    let source = placed.as_ref().unwrap_or(input);
    let both = ctx.scratch(source.width(), source.height())?;
    passes(ctx, source, plan, edge, &both)?;
    Ok(Blurred {
        image: both,
        scale: plan.scale,
    })
}

/// Blurs the whole of `input` by `radius` px straight into `output`, of the same size,
/// when the blur runs at full scale: then the vertical pass is the result, and drawing
/// it into a scratch layer to copy it out would cost a full-size pass more (#1070).
/// Returns `false`, having drawn nothing, when the blur works at a coarser scale or is
/// no blur, so the caller reads [`blur`]'s result back instead.
pub(super) fn blur_whole_into(
    ctx: &mut Ctx<'_>,
    input: &Image,
    radius: f32,
    output: &Image,
) -> Result<bool, RenderError> {
    let plan = plan(radius);
    let same = (input.width(), input.height()) == (output.width(), output.height());
    if plan.scale != 1 || plan.taps == 0 || !same {
        return Ok(false);
    }
    passes(ctx, input, plan, Edge::Clamp, output)?;
    Ok(true)
}

/// The horizontal pass of `plan` over `source`, then the vertical one into `to`.
fn passes(
    ctx: &mut Ctx<'_>,
    source: &Image,
    plan: Plan,
    edge: Edge,
    to: &Image,
) -> Result<(), RenderError> {
    let across = ctx.scratch(source.width(), source.height())?;
    let weights = weights(plan);
    for ((dx, dy), from, target) in [((1, 0), source, &across), ((0, 1), &across, to)] {
        let mut params = Uniforms::new()
            .i32(dx)
            .i32(dy)
            .i32(plan.taps as i32)
            .u32(edge.code());
        for four in weights.chunks_exact(4) {
            params = params.vec4([four[0], four[1], four[2], four[3]]);
        }
        let (pairs, count) = pairs(&weights, plan.taps);
        for two in pairs.chunks_exact(2) {
            params = params.vec4([two[0][0], two[0][1], two[1][0], two[1][1]]);
        }
        let params = params.i32(count as i32).finish();
        ctx.draw(&Draw {
            shader: GAUSS,
            input: from,
            aux: None,
            params: &params,
            target,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_RADIUS, MAX_TAPS, div_ceil, pairs, plan, weights};

    #[test]
    fn the_plan_keeps_every_radius_within_the_tap_bound() {
        assert_eq!(plan(0.0).taps, 0);
        assert_eq!((plan(0.0).scale, plan(-3.0).taps), (1, 0));
        assert_eq!((plan(4.0).scale, plan(4.0).taps), (1, 4));
        assert_eq!((plan(8.0).scale, plan(8.0).taps), (1, 8));
        assert_eq!((plan(8.5).scale, plan(8.5).taps), (2, 5));
        assert_eq!((plan(12.0).scale, plan(12.0).taps), (2, 6));
        assert_eq!((plan(100.0).scale, plan(100.0).taps), (16, 7));
        assert_eq!(plan(1000.0), plan(MAX_RADIUS));
        assert_eq!(plan(f32::NAN).taps, 0);
        let mut r = 0.0;
        while r <= 200.0 {
            let p = plan(r);
            assert!(
                p.taps <= MAX_TAPS && [1, 2, 4, 8, 16].contains(&p.scale),
                "{r}"
            );
            r += 0.25;
        }
    }

    #[test]
    fn the_weights_sum_to_one_and_fall_off_from_the_centre() {
        for radius in [0.5, 3.0, 8.0, 12.0, 100.0] {
            let p = plan(radius);
            let w = weights(p);
            let taps = p.taps as usize;
            let total = w[0] + 2.0 * w[1..=taps].iter().sum::<f32>();
            assert!((total - 1.0).abs() < 1e-6, "{radius}: {w:?}");
            assert!(
                w[..=taps].windows(2).all(|pair| pair[0] >= pair[1]),
                "{w:?}"
            );
            assert!(w[taps + 1..].iter().all(|&v| v == 0.0), "{w:?}");
        }
    }

    #[test]
    fn each_pair_reads_its_two_weights_as_one_bilinear_sample() {
        for radius in [1.0, 3.0, 4.0, 7.5, 8.0, 12.0, 100.0] {
            let p = plan(radius);
            let w = weights(p);
            let (pairs, count) = pairs(&w, p.taps);
            assert_eq!(count, (p.taps as usize).div_ceil(2), "{radius}");
            for (k, &[sum, offset]) in pairs.iter().enumerate().take(count) {
                let near = 2 * k + 1;
                let far = if near < p.taps as usize {
                    w[near + 1]
                } else {
                    0.0
                };
                // A bilinear sample at `offset` weights texel `near` by `1 - t` and texel
                // `near + 1` by `t`.
                let t = offset - near as f32;
                assert!((0.0..=1.0).contains(&t), "{radius}: {offset}");
                assert!((sum * (1.0 - t) - w[near]).abs() < 1e-6, "{radius}: {k}");
                assert!((sum * t - far).abs() < 1e-6, "{radius}: {k}");
            }
            assert!(pairs[count..].iter().all(|&pair| pair == [0.0, 0.0]));
        }
    }

    #[test]
    fn working_sizes_round_up() {
        assert_eq!(div_ceil(64, 8), 8);
        assert_eq!(div_ceil(65, 8), 9);
        assert_eq!(div_ceil(3, 8), 1);
        assert_eq!(div_ceil(5, 0), 5);
    }
}
