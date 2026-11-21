//! The jump flood that `outline` and `glow` share: for every pixel of a frame, the
//! vector to its nearest seed, a pixel of the input's alpha shape.
//!
//! A seed pass marks the seeds (alpha at least 0.5, or with `inside` the pixels below
//! it) and keeps each one's alpha. Then steps of `S, S/2, ..., 1` and one more step of 1
//! carry to every pixel the vector to its nearest seed and that seed's alpha, where `S`
//! is the power of two that lets the steps reach as far as asked. Each step is 9 loads,
//! and a reach of up to 128 px takes at most 8 steps. Jump flood distances are
//! near-exact (an occasional pixel takes a seed a little farther than the nearest).
//!
//! Under a bound (#1070) each pass shades only the bound and clears the rest, so a
//! pixel outside it holds no seed. That costs the flood nothing within the bound: the
//! steps that carry a seed to a pixel each move toward that pixel on both axes, so every
//! pixel they pass lies in the rectangle between the two, which the bound holds.

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Shader};
use crate::image::Image;

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

/// The jump flood's step sizes to reach `reach` px, largest first.
fn steps(reach: f32) -> impl Iterator<Item = u32> {
    let reach = reach.max(1.0).ceil() as u32;
    let first = (reach.next_power_of_two() / 2).max(1);
    std::iter::successors(Some(first), |s| (*s > 1).then_some(s / 2)).chain([1])
}

/// Floods `input`'s seeds over a scratch layer of `output`'s size, reaching `reach` px.
/// Each texel's rg is the vector to its nearest seed in pixels, b that seed's alpha,
/// and a 1 where it has one.
pub(super) fn flood(
    ctx: &mut Ctx<'_>,
    input: &Image,
    output: &Image,
    reach: f32,
    inside: bool,
) -> Result<Image, RenderError> {
    let (w, h) = (output.width(), output.height());
    let mut from = ctx.scratch(w, h)?;
    let mut to = ctx.scratch(w, h)?;
    let params = Uniforms::new().u32(u32::from(inside)).finish();
    ctx.draw(&Draw {
        shader: SEED,
        input,
        aux: None,
        params: &params,
        target: &from,
    })?;
    for step in steps(reach) {
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
    Ok(from)
}

#[cfg(test)]
mod tests {
    use super::steps;

    #[test]
    fn the_steps_reach_as_far_as_asked_and_stay_bounded() {
        let all = |r| steps(r).collect::<Vec<_>>();
        assert_eq!(all(0.0), [1, 1]);
        assert_eq!(all(1.0), [1, 1]);
        assert_eq!(all(2.0), [1, 1]);
        assert_eq!(all(5.0), [4, 2, 1, 1]);
        // The widest outline (64 px and its antialiased pixel) and the widest glow
        // (100 px and the half pixel to its seed's centre).
        assert_eq!(all(65.0), [64, 32, 16, 8, 4, 2, 1, 1]);
        assert_eq!(all(101.0), [64, 32, 16, 8, 4, 2, 1, 1]);
        let mut r = 0.0;
        while r <= 128.0 {
            let s = all(r);
            let reach: u32 = s.iter().sum();
            assert!(reach as f32 >= r && s.len() <= 9, "{r}: {s:?}");
            r += 0.5;
        }
    }
}
