//! Mips for a source placed in 3D (ADR-0050 decision 5).
//!
//! A source with a 3D placement gets a chain of its own: `Rgba16Float`, premultiplied
//! linear like an upload, with every level down to 1×1. The composite pass samples it
//! with trilinear filtering and 16× anisotropy, clamped to what the adapter offers. The
//! chain lives on the [`Image`] and goes with it, so a source never placed in 3D has none.
//!
//! It is rebuilt on the GPU at most once per frame: an upload when it was rewritten
//! since the last build, and a layer or an import every frame it is drawn in 3D, since
//! a filter, a nested scene or the platform can change those without this crate seeing it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::image::{Alpha, Image};

/// Never built.
const NEVER: u64 = u64::MAX;

/// What builds every chain: one pipeline per way a level-0 sample is read, and the
/// samplers.
pub(crate) struct MipPipelines {
    premultiplied: wgpu::RenderPipeline,
    straight: wgpu::RenderPipeline,
    srgb: wgpu::RenderPipeline,
    /// Linear, one level: what a level reads the one above with.
    linear: wgpu::Sampler,
    /// Trilinear with 16× anisotropy: what the composite pass reads a chain with.
    pub(crate) anisotropic: wgpu::Sampler,
}

impl MipPipelines {
    pub(crate) fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-mip"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mip.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-mip"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("jstream-render-mip"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
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
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: MIP_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let clamp = wgpu::AddressMode::ClampToEdge;
        Self {
            premultiplied: pipeline("fs_premultiplied"),
            straight: pipeline("fs_straight"),
            srgb: pipeline("fs_srgb"),
            linear: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("jstream-render-mip-linear"),
                address_mode_u: clamp,
                address_mode_v: clamp,
                address_mode_w: clamp,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            }),
            // wgpu clamps the anisotropy to 1 on an adapter without anisotropic
            // filtering, and every filter must be linear for it.
            anisotropic: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("jstream-render-anisotropic"),
                address_mode_u: clamp,
                address_mode_v: clamp,
                address_mode_w: clamp,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                anisotropy_clamp: 16,
                ..Default::default()
            }),
        }
    }
}

/// The format of a chain: premultiplied linear, as uploads and layers.
const MIP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// One source's chain.
pub(crate) struct Mips {
    /// What the composite pass binds: the whole chain and the anisotropic sampler.
    pub(crate) bind_group: wgpu::BindGroup,
    /// Each level's render-target view, level 0 first.
    targets: Vec<wgpu::TextureView>,
    /// Level `i`'s input for level `i + 1`: its view and the linear sampler.
    inputs: Vec<wgpu::BindGroup>,
    /// The upload generation of the last build, or [`NEVER`].
    built_generation: AtomicU64,
    /// The compositor call of the last build, or [`NEVER`].
    built_call: AtomicU64,
}

/// How many levels a `width` × `height` chain has, down to 1×1.
pub(crate) fn level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

impl Mips {
    pub(crate) fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        pipelines: &MipPipelines,
        width: u32,
        height: u32,
    ) -> Self {
        let levels = level_count(width, height);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-mips"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MIP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let level = |i: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("jstream-render-mip-level"),
                base_mip_level: i,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let bind = |view: &wgpu::TextureView, sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-mips"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let targets: Vec<wgpu::TextureView> = (0..levels).map(level).collect();
        let inputs = targets[..targets.len() - 1]
            .iter()
            .map(|view| bind(view, &pipelines.linear))
            .collect();
        let whole = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            bind_group: bind(&whole, &pipelines.anisotropic),
            targets,
            inputs,
            built_generation: AtomicU64::new(NEVER),
            built_call: AtomicU64::new(NEVER),
        }
    }

    /// Whether `image`'s chain needs building in compositor call `call`, and if so marks
    /// it built.
    pub(crate) fn claim(&self, image: &Image, call: u64) -> bool {
        if self.built_call.swap(call, Ordering::Relaxed) == call {
            return false;
        }
        if image.is_static() {
            let generation = image.generation();
            return self.built_generation.swap(generation, Ordering::Relaxed) != generation;
        }
        true
    }

    /// Records the passes that rebuild the chain from `image`.
    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipelines: &MipPipelines,
        image: &Image,
    ) {
        let first = match image.alpha() {
            Alpha::Premultiplied => &pipelines.premultiplied,
            Alpha::Straight => &pipelines.straight,
            Alpha::PremultipliedSrgb => &pipelines.srgb,
        };
        for (i, target) in self.targets.iter().enumerate() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-mip"),
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
            match i.checked_sub(1) {
                None => {
                    pass.set_pipeline(first);
                    pass.set_bind_group(0, image.bind_group(), &[]);
                }
                Some(above) => {
                    pass.set_pipeline(&pipelines.premultiplied);
                    pass.set_bind_group(0, &self.inputs[above], &[]);
                }
            }
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::level_count;

    #[test]
    fn a_chain_goes_down_to_one_texel() {
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(2, 1), 2);
        assert_eq!(level_count(64, 64), 7);
        assert_eq!(level_count(65, 3), 7);
        assert_eq!(level_count(1920, 1080), 11);
    }
}
