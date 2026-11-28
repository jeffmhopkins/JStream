//! A model's view depth from the depth its layer's pass tested (ADR-0143 decision 4):
//! one full-screen pass after the layer's, in its encoder, instead of drawing the
//! model's geometry a second time ([`super::ModelRuntime::draw_depth`]).

use crate::gpu::Gpu;
use crate::image::DEPTH_LAYER_FORMAT;

const SOURCE: &str = include_str!("depth_resolve.wgsl");

/// The pass's pipelines and layouts, for a single-sampled depth and a multisampled one.
pub(super) struct DepthResolve {
    single: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
    multi: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
    /// Keeps the nearer of the scene depth and what `target` already holds (ADR-0259).
    max: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
}

impl DepthResolve {
    pub(super) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-model-depth-resolve"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let near_w = [("NEAR_W", f64::from(jstream_protocol::NEAR_W))];
        let made = |multisampled: bool| {
            let binding = u32::from(multisampled);
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("jstream-render-model-depth-resolve"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled,
                    },
                    count: None,
                }],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("jstream-render-model-depth-resolve"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let options = wgpu::PipelineCompilationOptions {
                constants: &near_w,
                ..Default::default()
            };
            let desc = wgpu::RenderPipelineDescriptor {
                label: Some(if multisampled {
                    "jstream-render-model-depth-resolve-multi"
                } else {
                    "jstream-render-model-depth-resolve"
                }),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: options.clone(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if multisampled {
                        "fs_multi"
                    } else {
                        "fs_single"
                    }),
                    compilation_options: options,
                    targets: &[Some(wgpu::ColorTargetState {
                        format: DEPTH_LAYER_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            };
            let pipeline = gpu.built.render(device, SOURCE, &multisampled, &desc);
            (layout, pipeline)
        };
        let max = {
            let (layout, _) = made(false);
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("jstream-render-scene-depth-max"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let options = wgpu::PipelineCompilationOptions {
                constants: &near_w,
                ..Default::default()
            };
            let desc = wgpu::RenderPipelineDescriptor {
                label: Some("jstream-render-scene-depth-max"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: options.clone(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_single"),
                    compilation_options: options,
                    targets: &[Some(wgpu::ColorTargetState {
                        format: DEPTH_LAYER_FORMAT,
                        // Nearer is the larger 1/w. Max keeps a card the scene depth
                        // doesn't hold, and a model nearer than that card.
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Max,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Max,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            };
            let pipeline = gpu.built.render(device, SOURCE, &"max", &desc);
            (layout, pipeline)
        };
        Self {
            single: made(false),
            multi: made(true),
            max,
        }
    }

    /// Records the pass that replaces every texel of `target`, a depth layer of the
    /// tested depth's size, with the view depth `depth` (sampled `samples` times) holds.
    pub(super) fn record(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        (depth, samples): (&wgpu::TextureView, u32),
        target: &wgpu::TextureView,
    ) {
        let multisampled = samples > 1;
        let (layout, pipeline) = if multisampled {
            &self.multi
        } else {
            &self.single
        };
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-model-depth-resolve"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: u32::from(multisampled),
                resource: wgpu::BindingResource::TextureView(depth),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-model-depth-resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Max-blends `depth`, a single-sampled scene depth, into `target` as `1 / w`.
    /// A texel already nearer in `target` stays. `depth` of 0 (the far clear) writes 0
    /// and loses to anything drawn.
    pub(super) fn record_max(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let (layout, pipeline) = &self.max;
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-scene-depth-max"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-scene-depth-max"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
