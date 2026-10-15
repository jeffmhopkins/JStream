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
        // A short or ragged buffer would fail wgpu's binding-size check at draw time,
        // outside any error scope, and panic.
        if self.params.len() < 16 || !self.params.len().is_multiple_of(16) {
            return Err(RenderError::Invalid {
                what: "filter uniforms",
                detail: format!(
                    "{}: {} bytes; the Params uniform is a multiple of 16, at least 16",
                    self.filter.label(),
                    self.params.len()
                ),
            });
        }
        let shader = ShaderRef {
            label: self.filter.label(),
            source: self.filter.wgsl(),
            blend: Blend::Replace,
        };
        ctx.draw_ref(shader, input, None, &self.params, output)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jstream_protocol::Settings;

    use super::ExternalFilter;
    use crate::filter::testing::image;
    use crate::filter::{FilterClock, VideoFilter};
    use crate::tests::open;

    struct Fake {
        wgsl: &'static str,
        uniforms: usize,
    }

    impl ExternalFilter for Fake {
        fn label(&self) -> &str {
            "test/fake"
        }

        fn wgsl(&self) -> &str {
            self.wgsl
        }

        fn uniforms(&self, _: &Settings) -> Vec<u8> {
            vec![0; self.uniforms]
        }
    }

    fn apply(fake: Fake) -> Result<(), crate::RenderError> {
        let Some(mut opened) = open() else {
            return Ok(());
        };
        let input = image(&opened, 1, &[[255, 0, 0, 255]]);
        let mut filter = VideoFilter::external("test/fake", Arc::new(fake), &Settings::default());
        let clock = FilterClock::default();
        // Twice: the second draw meets the cached result.
        opened.compositor.apply_filter(&mut filter, &input, clock)?;
        opened
            .compositor
            .apply_filter(&mut filter, &input, clock)
            .map(drop)
    }

    #[test]
    fn a_refused_external_shader_is_an_error_not_a_panic() {
        let result = apply(Fake {
            wgsl: "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return nope; }",
            uniforms: 16,
        });
        if open().is_some() {
            assert!(result.is_err(), "the shader was accepted");
        }
    }

    #[test]
    fn short_external_uniforms_are_an_error() {
        let result = apply(Fake {
            wgsl: "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv); }",
            uniforms: 8,
        });
        if open().is_some() {
            assert!(result.is_err());
        }
    }

    #[test]
    fn a_valid_external_shader_draws() {
        let result = apply(Fake {
            wgsl: "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv); }",
            uniforms: 16,
        });
        assert!(result.is_ok(), "{result:?}");
    }
}
