//! Mips for a source placed in 3D (ADR-0050 decision 5).
//!
//! A source with a 3D placement gets a chain of its own: `Rgba16Float`, premultiplied
//! linear like an upload, with every level down to 1×1. The composite pass samples it
//! with trilinear filtering and 16× anisotropy, clamped to what the adapter offers. The
//! chain lives on the [`Image`] and goes with it, so a source never placed in 3D has none.
//!
//! A chain is built from the item's crop, not the whole source ([`Region`]): level 0 is
//! the cropped texels, so no coarser level averages a texel past the crop into the
//! item's edge, and clamping to the chain's edge keeps the anisotropic footprint inside
//! it too (#641). An image keeps one chain per crop it is drawn with.
//!
//! A chain is rebuilt on the GPU when its image changed since its last build
//! ([`Image::changes`]): an upload rewritten, a layer drawn into, or an import given a
//! new frame. So a source drawn in 3D by several calls (two mixes, a nested scene) is
//! rebuilt once per change, not once per call (ADR-0050 decision 5).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::image::{Alpha, Image};

/// Never built.
const NEVER: u64 = u64::MAX;

/// The source texels a chain is built from: an item's crop, in source pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The whole of a `source_width` × `source_height` source.
    pub(crate) fn whole(source_width: u32, source_height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: source_width.max(1),
            height: source_height.max(1),
        }
    }

    /// The region's `[u, v, width, height]` in the source's texture coordinates.
    fn uv(self, source_width: u32, source_height: u32) -> [f32; 4] {
        let (w, h) = (source_width as f32, source_height as f32);
        [
            self.x as f32 / w,
            self.y as f32 / h,
            self.width as f32 / w,
            self.height as f32 / h,
        ]
    }
}

/// What builds every chain: for level 0, one pipeline per way a source sample is read,
/// each reading the chain's region; for the levels below, one that halves the level
/// above; and the samplers.
pub(crate) struct MipPipelines {
    premultiplied: wgpu::RenderPipeline,
    straight: wgpu::RenderPipeline,
    srgb: wgpu::RenderPipeline,
    next: wgpu::RenderPipeline,
    /// The layout of a chain's region, level 0's second group.
    region_layout: wgpu::BindGroupLayout,
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
        let region_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-mip-region"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(REGION_BYTES),
                },
                count: None,
            }],
        });
        let first_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-mip-first"),
            bind_group_layouts: &[Some(layout), Some(&region_layout)],
            immediate_size: 0,
        });
        let next_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-mip"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        let pipeline = |pipeline_layout: &wgpu::PipelineLayout, vs: &str, entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("jstream-render-mip"),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
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
            premultiplied: pipeline(&first_layout, "vs_region", "fs_premultiplied"),
            straight: pipeline(&first_layout, "vs_region", "fs_straight"),
            srgb: pipeline(&first_layout, "vs_region", "fs_srgb"),
            next: pipeline(&next_layout, "vs", "fs_premultiplied"),
            region_layout,
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

/// A region's uniform: `vec4<f32>`, its `[u, v, width, height]` in the source.
const REGION_BYTES: u64 = 16;

/// One source's chain, for one region of it.
pub(crate) struct Mips {
    /// What the composite pass binds: the whole chain and the anisotropic sampler.
    pub(crate) bind_group: wgpu::BindGroup,
    /// The source texels level 0 holds.
    region: Region,
    /// Level 0's second group: the region, in the source's texture coordinates.
    region_group: wgpu::BindGroup,
    /// Each level's render-target view, level 0 first.
    targets: Vec<wgpu::TextureView>,
    /// Level `i`'s input for level `i + 1`: its view and the linear sampler.
    inputs: Vec<wgpu::BindGroup>,
    /// The image's [`Image::changes`] at the last build, or [`NEVER`].
    built: AtomicU64,
    /// How many times the chain was recorded, for tests.
    #[cfg(test)]
    builds: AtomicU64,
}

/// How many levels a `width` × `height` chain has, down to 1×1.
pub(crate) fn level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

impl Mips {
    /// A chain of `region` of `image`, not built yet.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        pipelines: &MipPipelines,
        image: &Image,
        region: Region,
    ) -> Self {
        let (width, height) = (region.width, region.height);
        let levels = level_count(width, height);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-mip-region"),
            size: REGION_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bytes: Vec<u8> = region
            .uv(image.width(), image.height())
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        // Ordered before the submit that first builds the chain.
        queue.write_buffer(&uniform, 0, &bytes);
        let region_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-mip-region"),
            layout: &pipelines.region_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
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
            region,
            region_group,
            targets,
            inputs,
            built: AtomicU64::new(NEVER),
            #[cfg(test)]
            builds: AtomicU64::new(0),
        }
    }

    /// The source texels the chain holds.
    pub(crate) fn region(&self) -> Region {
        self.region
    }

    /// How many times the chain was recorded.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> u64 {
        self.builds.load(Ordering::Relaxed)
    }

    /// Whether the chain needs building from `image`, which changed since the last
    /// build or was never built, and if so marks it built.
    pub(crate) fn claim(&self, image: &Image) -> bool {
        let changes = image.changes();
        self.built.swap(changes, Ordering::Relaxed) != changes
    }

    /// Records the passes that rebuild the chain from `image`.
    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipelines: &MipPipelines,
        image: &Image,
    ) {
        #[cfg(test)]
        self.builds.fetch_add(1, Ordering::Relaxed);
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
                    pass.set_bind_group(1, &self.region_group, &[]);
                }
                Some(above) => {
                    pass.set_pipeline(&pipelines.next);
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
