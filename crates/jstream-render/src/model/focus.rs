//! Depth of field on a model's layer (ADR-0165): the layer blurred by the depth of the
//! surface each pixel shows, in four fullscreen passes over the model's colour and view
//! depth. [`crate::Compositor::draw_model`] runs it when the model's camera has depth of
//! field; otherwise the model draws with no pass.

use crate::error::RenderError;
use crate::filter::params::Uniforms;
use crate::filter::{Blend, Ctx, Draw, Pass, Shader, ViewDepth};
use crate::image::Image;

/// Pixels on a side of a reach tile: `TILE` in `focus/common.wgsl`.
const TILE: u32 = 8;

const TILES: Shader = Shader {
    label: "model-focus-tiles",
    source: concat!(
        include_str!("focus/common.wgsl"),
        include_str!("focus/tiles.wgsl")
    ),
    blend: Blend::Replace,
};

const REACH: Shader = Shader {
    label: "model-focus-reach",
    source: concat!(
        include_str!("focus/common.wgsl"),
        include_str!("focus/reach.wgsl")
    ),
    blend: Blend::Replace,
};

const PACK: Shader = Shader {
    label: "model-focus-pack",
    source: concat!(
        include_str!("focus/common.wgsl"),
        include_str!("focus/pack.wgsl")
    ),
    blend: Blend::Replace,
};

const GATHER: Shader = Shader {
    label: "model-focus-gather",
    source: concat!(
        include_str!("focus/common.wgsl"),
        include_str!("focus/gather.wgsl")
    ),
    blend: Blend::Replace,
};

/// The kind name the pass runs under; it is no filter kind a chain can hold.
pub(crate) const KIND: &str = "model_depth_of_field";

/// The passes: the model's layer in, blurred out, by the depth and camera
/// [`Pass::set_depth`] last handed it.
#[derive(Default)]
pub(crate) struct ModelFocus {
    depth: Option<ViewDepth>,
}

impl Pass for ModelFocus {
    fn update(&mut self, _settings: &jstream_protocol::Settings) {}

    fn set_depth(&mut self, depth: Option<ViewDepth>) {
        self.depth = depth;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let (width, height) = (input.width(), input.height());
        let focused = self.depth.as_ref().and_then(|depth| {
            let focus = depth.view.as_ref()?.focus()?;
            let fits = (depth.image.width(), depth.image.height()) == (width, height);
            (fits && depth.focal_px.is_finite() && depth.focal_px > 0.0).then_some((depth, focus))
        });
        let Some((depth, focus)) = focused else {
            return Err(RenderError::Invalid {
                what: "model depth of field",
                detail: "no view depth of the layer's size, or no focus".into(),
            });
        };
        let params = Uniforms::new()
            .f32(focus.distance / depth.focal_px)
            .f32(focus.blur)
            .finish();
        let tiled = (width.div_ceil(TILE), height.div_ceil(TILE));
        let tiles = ctx.scratch(tiled.0, tiled.1)?;
        ctx.draw(&Draw {
            shader: TILES,
            input: depth.image.as_ref(),
            aux: None,
            params: &params,
            target: &tiles,
        })?;
        let reach = ctx.scratch(tiled.0, tiled.1)?;
        ctx.draw(&Draw {
            shader: REACH,
            input: &tiles,
            aux: None,
            params: &params,
            target: &reach,
        })?;
        let packed = ctx.scratch(width, height)?;
        ctx.draw(&Draw {
            shader: PACK,
            input: depth.image.as_ref(),
            aux: Some(&reach),
            params: &params,
            target: &packed,
        })?;
        ctx.draw(&Draw {
            shader: GATHER,
            input,
            aux: Some(&packed),
            params: &params,
            target: output,
        })
    }
}
