//! A reflection probe's capture (ADR-0170 decision 3): the six square faces a probe draws
//! of its scene, resampled once into the equirectangular panorama a surface inside its
//! box reads ([`crate::ProbeLookup`]).
//!
//! The faces are layers this compositor made, each the scene drawn from the probe's
//! centre through a 90-degree camera on a square canvas. The panorama is a layer four
//! faces wide and two high, in the scene's axes. The pipeline is built the first time a
//! probe resamples, so a compositor that never draws one pays nothing for it.

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::image::{Image, LAYER_FORMAT, Owner};
use wgpu::util::DeviceExt;

/// One face a probe drew: the layer, and the camera axes it was drawn through.
#[derive(Clone, Copy, Debug)]
pub struct ProbeFace<'a> {
    /// The face: a square layer, the scene seen from the probe's centre through a
    /// 90-degree camera.
    pub image: &'a Image,
    /// The camera's right, down and forward axes, in the scene's space.
    pub axes: [[f32; 3]; 3],
}

/// The resampling pipeline and its sampler.
pub(crate) struct ProbeRuntime {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl ProbeRuntime {
    pub(crate) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-probe"),
            source: wgpu::ShaderSource::Wgsl(include_str!("probe.wgsl").into()),
        });
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        for binding in 2..8 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-probe"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-probe"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-probe"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LAYER_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-probe"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    /// Resamples `faces` into `target`, replacing every texel.
    pub(crate) fn resample(
        &self,
        gpu: &Gpu,
        owner: &Owner,
        faces: &[ProbeFace<'_>; 6],
        target: &Image,
    ) -> Result<(), RenderError> {
        let Some(view) = target.target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the panorama is not a layer this compositor made".into(),
            });
        };
        let side = faces[0].image.width();
        if faces.iter().any(|face| {
            !face.image.owner().is(owner)
                || face.image.width() != side
                || face.image.height() != side
        }) {
            return Err(RenderError::Invalid {
                what: "probe face",
                detail: "a face is not a square layer of this compositor, the size of the others"
                    .into(),
            });
        }
        if target.width() != side * 4 || target.height() != side * 2 {
            return Err(RenderError::Invalid {
                what: "probe panorama",
                detail: format!(
                    "the panorama is {}x{}, not four faces of {side} by two",
                    target.width(),
                    target.height()
                ),
            });
        }
        let device = &gpu.device;
        let values = axes_values(faces);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("jstream-render-probe"),
            contents: &values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<u8>>(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let views: Vec<wgpu::TextureView> = faces
            .iter()
            .map(|face| {
                face.image
                    .texture()
                    .create_view(&wgpu::TextureViewDescriptor::default())
            })
            .collect();
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
        ];
        for (i, face) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: 2 + i as u32,
                resource: wgpu::BindingResource::TextureView(face),
            });
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-probe"),
            layout: &self.layout,
            entries: &entries,
        });
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render-probe"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The triangle writes every texel.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.submit_work(encoder.finish());
        target.mark_changed();
        drop(submitting);
        Ok(())
    }
}

/// The faces' axes in the shader's terms (`probe.wgsl`'s `Faces`): six `mat3x3`, each
/// column padded to four floats.
fn axes_values(faces: &[ProbeFace<'_>; 6]) -> Vec<f32> {
    let mut values = Vec::with_capacity(6 * 12);
    for face in faces {
        for axis in face.axes {
            let axis = axis.map(|c| if c.is_finite() { c } else { 0.0 });
            values.extend(axis);
            values.push(0.0);
        }
    }
    values
}

#[cfg(test)]
mod tests;
