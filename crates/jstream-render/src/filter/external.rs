//! Video filters whose WGSL arrives at run time: effect packs (ADR-0035), loaded and
//! validated by `jstream-effects`. The pack side implements [`ExternalFilter`]; this
//! side draws it with the same prelude, bindings and pipeline cache as the built-ins.

use std::sync::Arc;

use jstream_protocol::Settings;

use crate::error::RenderError;
use crate::filter::pass::{Blend, Ctx, Pass, ShaderRef};
use crate::image::Image;

/// A video filter kind defined outside this crate: one fullscreen pass of WGSL appended
/// to [`crate::FILTER_PRELUDE`], reading its parameters from a `Params` uniform at
/// group 0, binding 4.
pub trait ExternalFilter: Send + Sync {
    /// The pipeline cache key: unique per kind. When the WGSL behind a label changes (a
    /// reloaded pack), the next draw rebuilds the pipeline.
    fn label(&self) -> &str;

    /// WGSL defining `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and, when it
    /// has parameters, `p`.
    fn wgsl(&self) -> &str;

    /// The `Params` uniform's bytes for `settings`, complete as the graph stores them: a
    /// multiple of 16, at least 16.
    fn uniforms(&self, settings: &Settings) -> Vec<u8>;
}

/// The pass of an [`ExternalFilter`].
pub(crate) struct External {
    filter: Arc<dyn ExternalFilter>,
    params: Vec<u8>,
}

impl External {
    pub(crate) fn new(filter: Arc<dyn ExternalFilter>, settings: &Settings) -> Self {
        let params = filter.uniforms(settings);
        Self { filter, params }
    }
}

impl Pass for External {
    fn update(&mut self, settings: &Settings) {
        self.params = self.filter.uniforms(settings);
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let shader = ShaderRef {
            label: self.filter.label(),
            source: self.filter.wgsl(),
            blend: Blend::Replace,
        };
        ctx.draw_ref(shader, input, None, &self.params, output)
    }
}
