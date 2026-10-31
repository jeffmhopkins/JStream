//! How each blend mode is drawn (ADR-0082): a fixed-function blend state where one is
//! exact for premultiplied colour at any alpha, and otherwise a shader that reads a copy
//! of what is below the item, the backdrop.

use jstream_protocol::BlendMode;

use crate::error::RenderError;

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
}

impl BlendPipelines {
    pub(crate) fn new(device: &wgpu::Device, layouts: &Layouts<'_>) -> Self {
        let over = component(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);
        let fixed = |label: &str, color: wgpu::BlendComponent, alpha: wgpu::BlendComponent| {
            pipeline(
                device,
                layouts,
                label,
                "fs",
                Some(wgpu::BlendState { color, alpha }),
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
            )
        });
        Self {
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

fn pipeline(
    device: &wgpu::Device,
    layouts: &Layouts<'_>,
    label: &str,
    entry: &str,
    blend: Option<wgpu::BlendState>,
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
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: layouts.shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
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
