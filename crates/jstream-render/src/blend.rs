//! How each blend mode is drawn (ADR-0082): a fixed-function blend state where one is
//! exact for premultiplied colour at any alpha, and otherwise a shader that reads a copy
//! of what is below the item, the backdrop. Each set of pipelines draws into a target of
//! one space (ADR-0126 §3): premultiplied linear light, or premultiplied sRGB-encoded
//! colour, where the same blend states give a browser's composite.

use jstream_protocol::BlendMode;

use crate::error::RenderError;
use crate::image::DEPTH_LAYER_FORMAT;

/// The modes whose shader reads the backdrop, in the order of their pipelines and
/// fragment entry points.
const BACKDROP_MODES: [(BlendMode, &str); 5] = [
    (BlendMode::Darken, "fs_darken"),
    (BlendMode::Lighten, "fs_lighten"),
    (BlendMode::Overlay, "fs_overlay"),
    (BlendMode::Difference, "fs_difference"),
    (BlendMode::Exclusion, "fs_exclusion"),
];

/// One pipeline per way of drawing an item.
pub(crate) struct BlendPipelines {
    normal: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    screen: wgpu::RenderPipeline,
    /// Multiply's two draws, in order.
    multiply: [wgpu::RenderPipeline; 2],
    backdrop: [wgpu::RenderPipeline; BACKDROP_MODES.len()],
    /// A scene's view depth (ADR-0143 decision 4): `fs_depth`, keeping the larger value.
    depth: wgpu::RenderPipeline,
}

/// How one item is drawn.
pub(crate) enum Steps<'a> {
    /// Each pipeline in turn draws the item's triangles, with blend states only.
    Fixed(&'a [wgpu::RenderPipeline]),
    /// The pipeline reads the backdrop at bind group 1 and writes the result in place:
    /// the backdrop must hold the target's texels under the item.
    Backdrop(&'a wgpu::RenderPipeline),
}

/// What the pipelines are built from.
pub(crate) struct Layouts<'a> {
    /// The image's bind group, group 0.
    pub(crate) image: &'a wgpu::BindGroupLayout,
    /// The backdrop's, group 1.
    pub(crate) backdrop: &'a wgpu::BindGroupLayout,
    /// The composite shader.
    pub(crate) shader: &'a wgpu::ShaderModule,
    /// Whether the target holds sRGB-encoded colour: the shader's `ENCODED`.
    pub(crate) encoded: bool,
    /// The pipelines made on the device already (`pipelines.rs`).
    pub(crate) built: &'a crate::pipelines::Built,
}

/// The depth format of a scene pass with a model's depth (ADR-0107 decision 7): reversed,
/// larger nearer, as the model draw's own.
pub(crate) const SCENE_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

impl BlendPipelines {
    /// The pipelines for a pass without a depth target, or, with `depth`, for one with a
    /// scene depth target that each draw tests with that function and never writes.
    pub(crate) fn new(
        device: &wgpu::Device,
        layouts: &Layouts<'_>,
        depth: Option<wgpu::CompareFunction>,
    ) -> Self {
        let over = component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);
        let fixed = |label: &str, color: wgpu::BlendComponent, alpha: wgpu::BlendComponent| {
            pipeline(
                device,
                layouts,
                label,
                "fs",
                Some(wgpu::BlendState { color, alpha }),
                depth,
            )
        };
        // Multiply, premultiplied: s·d + d·(1 − αs) + s·(1 − αd). The first draw writes
        // the first two terms and keeps αd; the second adds the third, with the αd the
        // first draw left, and makes the alpha αs + αd·(1 − αs).
        let keep = component(wgpu::BlendFactor::Zero, wgpu::BlendFactor::One);
        let under = component(wgpu::BlendFactor::OneMinusDstAlpha, wgpu::BlendFactor::One);
        let multiply = [
            fixed(
                "jstream-render-multiply-1",
                component(wgpu::BlendFactor::Dst, wgpu::BlendFactor::OneMinusSrcAlpha),
                keep,
            ),
            fixed("jstream-render-multiply-2", under, under),
        ];
        let backdrop = BACKDROP_MODES.map(|(mode, entry)| {
            pipeline(
                device,
                layouts,
                &format!("jstream-render-{}", mode.as_str()),
                entry,
                None,
                depth,
            )
        });
        let largest = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let depth = pipeline_with(
            device,
            layouts,
            "jstream-render-view-depth",
            "fs_depth",
            Some(wgpu::BlendState {
                color: largest,
                alpha: largest,
            }),
            depth.map(|compare| (compare, false)),
            (wgpu::ColorWrites::ALL, DEPTH_LAYER_FORMAT),
        );
        Self {
            depth,
            normal: fixed("jstream-render-composite", over, over),
            // s + d, alpha as source-over so it stays in [0, 1].
            additive: fixed(
                "jstream-render-additive",
                component(wgpu::BlendFactor::One, wgpu::BlendFactor::One),
                over,
            ),
            // s + d − s·d, premultiplied: exact at any alpha.
            screen: fixed(
                "jstream-render-screen",
                component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc),
                over,
            ),
            multiply,
            backdrop,
        }
    }

    /// The blend pipelines of a model layer that shares the scene depth (ADR-0256):
    /// each fragment's depth is the model's view-depth texel (`fs_model`), tested
    /// nearer-wins. `write` stores that depth; a blended or additive model passes
    /// `false` so it is tested and does not occlude what is drawn after it.
    pub(crate) fn sharing_depth(device: &wgpu::Device, layouts: &Layouts<'_>, write: bool) -> Self {
        let over = component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);
        let fixed = |label: &str, color: wgpu::BlendComponent, alpha: wgpu::BlendComponent| {
            pipeline_sharing(
                device,
                layouts,
                label,
                "fs_model",
                Some(wgpu::BlendState { color, alpha }),
                write,
            )
        };
        let keep = component(wgpu::BlendFactor::Zero, wgpu::BlendFactor::One);
        let under = component(wgpu::BlendFactor::OneMinusDstAlpha, wgpu::BlendFactor::One);
        let multiply = [
            fixed(
                "jstream-render-model-multiply-1",
                component(wgpu::BlendFactor::Dst, wgpu::BlendFactor::OneMinusSrcAlpha),
                keep,
            ),
            fixed("jstream-render-model-multiply-2", under, under),
        ];
        let backdrop = BACKDROP_MODES.map(|(mode, entry)| {
            pipeline_sharing(
                device,
                layouts,
                &format!("jstream-render-model-{}", mode.as_str()),
                &format!("fs_model_{}", entry.trim_start_matches("fs_")),
                None,
                write,
            )
        });
        // Unused: a model layer is not a card's view-depth draw. Built so the set has
        // the same shape as [`Self::new`].
        let depth_pipe = pipeline_with(
            device,
            layouts,
            "jstream-render-model-view-depth",
            "fs_depth",
            Some(wgpu::BlendState::REPLACE),
            Some((wgpu::CompareFunction::Always, false)),
            (wgpu::ColorWrites::ALL, DEPTH_LAYER_FORMAT),
        );
        Self {
            depth: depth_pipe,
            normal: fixed("jstream-render-model-composite", over, over),
            additive: fixed(
                "jstream-render-model-additive",
                component(wgpu::BlendFactor::One, wgpu::BlendFactor::One),
                over,
            ),
            screen: fixed(
                "jstream-render-model-screen",
                component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc),
                over,
            ),
            multiply,
            backdrop,
        }
    }

    /// The pipeline that draws a scene's view depth (ADR-0143 decision 4).
    pub(crate) fn depth(&self) -> &wgpu::RenderPipeline {
        &self.depth
    }

    /// How `mode` is drawn.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a mode this compositor does not draw: one added to
    /// the protocol after it.
    pub(crate) fn steps(&self, mode: BlendMode) -> Result<Steps<'_>, RenderError> {
        Ok(match mode {
            BlendMode::Normal => Steps::Fixed(std::slice::from_ref(&self.normal)),
            BlendMode::Additive => Steps::Fixed(std::slice::from_ref(&self.additive)),
            BlendMode::Screen => Steps::Fixed(std::slice::from_ref(&self.screen)),
            BlendMode::Multiply => Steps::Fixed(&self.multiply),
            other => {
                let index = BACKDROP_MODES
                    .iter()
                    .position(|(mode, _)| *mode == other)
                    .ok_or_else(|| RenderError::Invalid {
                        what: "blend",
                        detail: format!("`{}` is not a mode the compositor draws", other.as_str()),
                    })?;
                Steps::Backdrop(&self.backdrop[index])
            }
        })
    }
}

/// The pipeline that writes a model's view depth, an item's image, into a scene pass's
/// depth target (ADR-0107 decision 7): `fs_write_depth`, keeping the nearer value, with
/// no colour written.
pub(crate) fn depth_writer(device: &wgpu::Device, layouts: &Layouts<'_>) -> wgpu::RenderPipeline {
    pipeline_with(
        device,
        layouts,
        "jstream-render-scene-depth",
        "fs_write_depth",
        Some(wgpu::BlendState::REPLACE),
        Some((wgpu::CompareFunction::GreaterEqual, true)),
        (wgpu::ColorWrites::empty(), crate::image::LAYER_FORMAT),
    )
}

/// The pipeline that adds a model's view depth, an item's image, to a scene's view depth
/// (ADR-0143 decision 4): `fs_copy_depth`, keeping the larger value, as the cards'
/// [`BlendPipelines::depth`] does.
pub(crate) fn depth_copier(device: &wgpu::Device, layouts: &Layouts<'_>) -> wgpu::RenderPipeline {
    let largest = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Max,
    };
    pipeline_with(
        device,
        layouts,
        "jstream-render-view-depth-copy",
        "fs_copy_depth",
        Some(wgpu::BlendState {
            color: largest,
            alpha: largest,
        }),
        None,
        (wgpu::ColorWrites::ALL, DEPTH_LAYER_FORMAT),
    )
}

/// Whether `mode` is drawn by reading the backdrop.
pub(crate) fn reads_backdrop(mode: BlendMode) -> bool {
    BACKDROP_MODES.iter().any(|(m, _)| *m == mode)
}

fn component(src: wgpu::BlendFactor, dst: wgpu::BlendFactor) -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        src_factor: src,
        dst_factor: dst,
        operation: wgpu::BlendOperation::Add,
    }
}

/// A model layer's pipeline (ADR-0256): group 0 is the colour, group 1 is the colour
/// again for a fixed-function mode (so group 2 can be the depth) or the backdrop for a
/// mode that reads it, and group 2 is the model's view depth. The fragment writes
/// `frag_depth` from that depth and tests nearer-wins; `write` stores it.
fn pipeline_sharing(
    device: &wgpu::Device,
    layouts: &Layouts<'_>,
    label: &str,
    entry: &str,
    blend: Option<wgpu::BlendState>,
    write: bool,
) -> wgpu::RenderPipeline {
    let groups = if blend.is_some() {
        vec![
            Some(layouts.image),
            Some(layouts.image),
            Some(layouts.image),
        ]
    } else {
        vec![
            Some(layouts.image),
            Some(layouts.backdrop),
            Some(layouts.image),
        ]
    };
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &groups,
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32,
        3 => Float32,
        4 => Float32,
        5 => Float32x3,
        6 => Float32x3,
        7 => Float32x3,
        8 => Float32x3,
        9 => Float32x3,
        10 => Float32x4,
        11 => Float32x3,
    ];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: crate::compositor::VERTEX_STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let desc = wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: layouts.shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: SCENE_DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: layouts.shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("ENCODED", if layouts.encoded { 1.0 } else { 0.0 })],
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format: crate::image::LAYER_FORMAT,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    layouts
        .built
        .render(device, crate::compositor::COMPOSITE_SOURCE, &(), &desc)
}

fn pipeline(
    device: &wgpu::Device,
    layouts: &Layouts<'_>,
    label: &str,
    entry: &str,
    blend: Option<wgpu::BlendState>,
    depth: Option<wgpu::CompareFunction>,
) -> wgpu::RenderPipeline {
    pipeline_with(
        device,
        layouts,
        label,
        entry,
        blend,
        depth.map(|compare| (compare, false)),
        (wgpu::ColorWrites::ALL, crate::image::LAYER_FORMAT),
    )
}

fn pipeline_with(
    device: &wgpu::Device,
    layouts: &Layouts<'_>,
    label: &str,
    entry: &str,
    blend: Option<wgpu::BlendState>,
    depth: Option<(wgpu::CompareFunction, bool)>,
    (write_mask, format): (wgpu::ColorWrites, wgpu::TextureFormat),
) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32,
        3 => Float32,
        4 => Float32,
        5 => Float32x3,
        6 => Float32x3,
        7 => Float32x3,
        8 => Float32x3,
        9 => Float32x3,
        10 => Float32x4,
        11 => Float32x3,
    ];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: crate::compositor::VERTEX_STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    // A fixed-function mode never reads the backdrop, so its layout leaves group 1 out.
    let groups = if blend.is_some() {
        vec![Some(layouts.image)]
    } else {
        vec![Some(layouts.image), Some(layouts.backdrop)]
    };
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &groups,
        immediate_size: 0,
    });
    let desc = wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: layouts.shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: depth.map(|(compare, write)| wgpu::DepthStencilState {
            format: SCENE_DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: layouts.shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("ENCODED", if layouts.encoded { 1.0 } else { 0.0 })],
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    // The layout follows from `blend`, which the descriptor holds.
    layouts
        .built
        .render(device, crate::compositor::COMPOSITE_SOURCE, &(), &desc)
}

#[cfg(test)]
mod tests {
    use super::{BACKDROP_MODES, reads_backdrop};
    use jstream_protocol::BlendMode;

    /// Every mode the protocol lists is drawn one way or the other.
    #[test]
    fn every_mode_has_a_way_to_draw() {
        let fixed = [
            BlendMode::Normal,
            BlendMode::Additive,
            BlendMode::Screen,
            BlendMode::Multiply,
        ];
        for mode in BlendMode::ALL {
            assert!(
                fixed.contains(&mode) != reads_backdrop(mode),
                "{} is drawn exactly one way",
                mode.as_str()
            );
        }
        assert_eq!(fixed.len() + BACKDROP_MODES.len(), BlendMode::ALL.len());
    }
}
