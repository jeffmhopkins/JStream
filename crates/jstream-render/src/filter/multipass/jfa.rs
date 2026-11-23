//! The nearest seed that `outline` and `glow` share: for every pixel of a frame, the
//! vector to its nearest seed, a pixel of the input's alpha shape (alpha at least 0.5,
//! or with `inside` the pixels below it), and that seed's alpha.
//!
//! **A short reach** (up to [`SCAN_REACH`] px) is two passes that find it exactly. The
//! first gives each pixel the nearest seed along its own row, scanning outward up to
//! the reach each way and stopping at the first. The second scans the rows above and
//! below, up to the reach, for the row whose nearest seed is closest, stopping once the
//! rows are farther than the best found. A pass costs about the same whatever it
//! reads until it reads a great deal, so two passes of a few loads beat the flood's
//! several.
//!
//! **A longer reach** is a jump flood: steps of `S, S/2, ..., 1` and one more step of 1
//! carry to every pixel the vector to its nearest seed and that seed's alpha, where `S`
//! is the power of two that lets the steps reach as far as asked. The first step reads
//! the input itself, marking the seeds as it goes, and each step is 9 loads; a reach of
//! up to 128 px takes at most 8 steps. Jump flood distances are near-exact (an
//! occasional pixel takes a seed a little farther than the nearest).
//!
//! Under a bound (#1070) each pass shades only the bound and clears the rest. That
//! costs neither method anything within the bound: the bound holds every seed within
//! the reach of a pixel it shades, the scans read only along the row and the column
//! between a pixel and its seed, and the flood's steps that carry a seed to a pixel
//! each move toward that pixel on both axes, so every pixel they pass lies in the
//! rectangle between the two.

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Shader};
use crate::image::Image;

/// The farthest reach, px, the two scanning passes are used for. Past it they read more
/// than the flood's extra passes cost.
const SCAN_REACH: f32 = 17.0;

const ROWS: Shader = Shader {
    label: "multipass-scan-rows",
    source: wgsl!("scan_rows.wgsl"),
    blend: Blend::Replace,
};

const COLUMNS: Shader = Shader {
    label: "multipass-scan-columns",
    source: wgsl!("scan_columns.wgsl"),
    blend: Blend::Replace,
};

const FIRST: Shader = Shader {
    label: "multipass-jfa-first",
    source: wgsl!("jfa_first.wgsl"),
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

/// Whether a reach of `reach` px is found by the two scanning passes, not the flood.
fn scans(reach: f32) -> bool {
    reach <= SCAN_REACH
}

/// Finds the nearest of `input`'s seeds to each pixel of a scratch layer of `output`'s
/// size, as far as `reach` px. Each texel's rg is the vector to its nearest seed in
/// pixels, b that seed's alpha, and a 1 where it has one within reach (the flood may
/// also find one farther).
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
    if scans(reach) {
        let reach = reach.max(1.0).ceil() as i32;
        ctx.draw(&Draw {
            shader: ROWS,
            input,
            aux: None,
            params: &Uniforms::new().u32(u32::from(inside)).i32(reach).finish(),
            target: &from,
        })?;
        ctx.draw(&Draw {
            shader: COLUMNS,
            input: &from,
            aux: None,
            params: &Uniforms::new().i32(reach).finish(),
            target: &to,
        })?;
        return Ok(to);
    }
    let mut steps = steps(reach);
    let first = steps.next().unwrap_or(1);
    let params = Uniforms::new()
        .u32(u32::from(inside))
        .i32(first as i32)
        .finish();
    ctx.draw(&Draw {
        shader: FIRST,
        input,
        aux: None,
        params: &params,
        target: &from,
    })?;
    for step in steps {
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
    use super::{SCAN_REACH, scans, steps};

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

    /// The default outline (4 px and its antialiased pixel) and glow (16 px and the half
    /// pixel to its seed's centre) are scanned; the widest of each is flooded.
    #[test]
    fn a_short_reach_is_scanned_and_a_long_one_flooded() {
        assert!(scans(5.0) && scans(16.5) && scans(SCAN_REACH));
        assert!(!scans(SCAN_REACH + 0.5) && !scans(65.0) && !scans(100.5));
    }
}
