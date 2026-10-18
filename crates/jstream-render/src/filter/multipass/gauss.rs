//! The bounded gaussian blur that blur, drop shadow and glow share, and the box
//! downsample that blur and pixelate share.
//!
//! A blur of radius `r` px spans `r` on each side (sigma `r / 3`). It runs at a working
//! scale of 1, 2, 4 or 8 (the smallest that keeps `r / scale` within [`MAX_TAPS`]):
//! a box downsample to that scale, then a horizontal and a vertical pass of at most
//! `2 * MAX_TAPS + 1` taps each, read back bilinear at full size. So every radius up
//! to [`MAX_RADIUS`] costs at most 64 loads per downsampled texel and 33 per blur pass,
//! and three scratch layers.

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Shader};
use crate::image::Image;

/// Taps on each side of a blur pass at the working scale.
pub(super) const MAX_TAPS: u32 = 16;
/// The coarsest working scale.
pub(super) const MAX_SCALE: u32 = 8;
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
    /// Full-size pixels per working texel: 1, 2, 4 or 8.
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
    let (w, h) = (source.width(), source.height());
    let across = ctx.scratch(w, h)?;
    let both = ctx.scratch(w, h)?;
    for ((dx, dy), from, to) in [((1, 0), source, &across), ((0, 1), &across, &both)] {
        let params = Uniforms::new()
            .i32(dx)
            .i32(dy)
            .i32(plan.taps as i32)
            .u32(edge.code())
            .f32(plan.sigma)
            .finish();
        ctx.draw(&Draw {
            shader: GAUSS,
            input: from,
            aux: None,
            params: &params,
            target: to,
        })?;
    }
    Ok(Blurred {
        image: both,
        scale: plan.scale,
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_RADIUS, MAX_TAPS, div_ceil, plan};

    #[test]
    fn the_plan_keeps_every_radius_within_the_tap_bound() {
        assert_eq!(plan(0.0).taps, 0);
        assert_eq!((plan(0.0).scale, plan(-3.0).taps), (1, 0));
        assert_eq!((plan(4.0).scale, plan(4.0).taps), (1, 4));
        assert_eq!((plan(16.0).scale, plan(16.0).taps), (1, 16));
        assert_eq!((plan(16.5).scale, plan(16.5).taps), (2, 9));
        assert_eq!((plan(100.0).scale, plan(100.0).taps), (8, 13));
        assert_eq!(plan(1000.0), plan(MAX_RADIUS));
        assert_eq!(plan(f32::NAN).taps, 0);
        let mut r = 0.0;
        while r <= 200.0 {
            let p = plan(r);
            assert!(p.taps <= MAX_TAPS && [1, 2, 4, 8].contains(&p.scale), "{r}");
            r += 0.25;
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
