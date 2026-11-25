//! Blurred reflections of a panorama (ADR-0170 decision 3), made once per build of its
//! mip chain: level `m` holds the 9-tap ring blur a rough surface would otherwise run
//! on every fragment, at the spread whose taps read level `m` (`panorama_blur.wgsl`).
//! A model binds these levels in place of the chain, and its shader reads one
//! trilinear sample where it read nine (`model.wgsl`'s `sky` and `probe_light`).
//!
//! The blurred levels live with the chain they are made from ([`Mips::blurred`]), so
//! they go with its image, and are made again only when the chain is rebuilt.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::mips::Mips;

/// The blurred levels' format: the chain's, premultiplied linear light.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Never made.
const NEVER: u64 = u64::MAX;

/// What makes every panorama's blurred levels.
pub(crate) struct BlurPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Trilinear, wrapping around the panorama and clamped at its poles.
    sampler: wgpu::Sampler,
}

impl BlurPipeline {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-panorama-blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("panorama_blur.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-panorama-blur"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-panorama-blur"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-panorama-blur"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-panorama-blur"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }
}

/// A chain's blurred levels: as many as the chain has, each the size of the chain's.
pub(crate) struct Blurred {
    /// The whole texture and its view, which a model binds in place of the chain's.
    pub(crate) whole: (wgpu::Texture, wgpu::TextureView),
    /// Each level's render-target view, level 0 first, and its pass's input: the chain
    /// and the level's number.
    targets: Vec<(wgpu::TextureView, wgpu::BindGroup)>,
    /// How many times the chain had been built when these levels were last made, or
    /// [`NEVER`].
    made: AtomicU64,
    /// How many times the levels were recorded, for tests.
    #[cfg(test)]
    makes: AtomicU64,
}

impl Blurred {
    /// Blurred levels for `chain`, not made yet.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &BlurPipeline,
        chain: &Mips,
    ) -> Self {
        let source = &chain.whole.0;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-panorama-blur"),
            size: source.size(),
            mip_level_count: source.mip_level_count(),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            // `COPY_SRC` so a test can read back what the passes wrote.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let targets = (0..source.mip_level_count())
            .map(|level| {
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("jstream-render-panorama-blur-level"),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                });
                let number = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jstream-render-panorama-blur-level"),
                    size: 16,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                // Ordered before the submit that first makes the levels.
                queue.write_buffer(&number, 0, &[level, 0, 0, 0].map(u32::to_le_bytes).concat());
                let input = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("jstream-render-panorama-blur"),
                    layout: &pipeline.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&chain.whole.1),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: number.as_entire_binding(),
                        },
                    ],
                });
                (view, input)
            })
            .collect();
        let whole = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            whole: (texture, whole),
            targets,
            made: AtomicU64::new(NEVER),
            #[cfg(test)]
            makes: AtomicU64::new(0),
        }
    }

    /// Whether the levels need making again from `chain`, built since they were last
    /// made or never made, and if so marks them made.
    pub(crate) fn claim(&self, chain: &Mips) -> bool {
        let built = chain.generation_built();
        self.made.swap(built, Ordering::Relaxed) != built
    }

    /// Records the passes that make the levels from their chain, after the chain's own
    /// build.
    pub(crate) fn record(&self, encoder: &mut wgpu::CommandEncoder, pipeline: &BlurPipeline) {
        #[cfg(test)]
        self.makes.fetch_add(1, Ordering::Relaxed);
        for (target, input) in &self.targets {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-panorama-blur"),
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
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, input, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::tests::open;

    /// An `f16`'s value.
    fn half(bits: u16) -> f32 {
        let sign = if bits >> 15 == 1 { -1.0 } else { 1.0 };
        let exponent = i32::from((bits >> 10) & 0x1f);
        let fraction = f32::from(bits & 0x3ff);
        sign * match exponent {
            0 => fraction * 2f32.powi(-24),
            31 => f32::INFINITY,
            _ => (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
        }
    }

    /// Level `level` of an `Rgba16Float` texture, a row of texels at a time.
    fn read_level(
        compositor: &crate::Compositor,
        texture: &wgpu::Texture,
        level: u32,
    ) -> Vec<Vec<[f32; 4]>> {
        let (width, height) = (
            (texture.width() >> level).max(1),
            (texture.height() >> level).max(1),
        );
        let row = (width * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let device = compositor.device();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-test-level"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        compositor.queue().submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the copy finishes");
        let bytes = buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapped")
            .to_vec();
        (0..height as usize)
            .map(|y| {
                (0..width as usize)
                    .map(|x| {
                        let at = y * row as usize + x * 8;
                        std::array::from_fn(|c| {
                            half(u16::from_le_bytes([
                                bytes[at + c * 2],
                                bytes[at + c * 2 + 1],
                            ]))
                        })
                    })
                    .collect()
            })
            .collect()
    }

    /// The light a level holds over the sphere, each texel weighted by the solid angle
    /// it covers (the cosine of its latitude).
    fn total(level: &[Vec<[f32; 4]>]) -> f32 {
        let rows = level.len() as f32;
        let mut sum = 0.0;
        for (y, row) in level.iter().enumerate() {
            let up = (0.5 - (y as f32 + 0.5) / rows) * std::f32::consts::PI;
            sum += row.iter().map(|texel| texel[0]).sum::<f32>() * up.cos() / row.len() as f32;
        }
        sum / rows
    }

    /// ADR-0170 decision 3: a panorama's blurred levels keep level 0 as the chain has it,
    /// so a mirror stays sharp; a picture of one colour stays that colour at every level,
    /// poles and seam included, so the blur neither darkens nor leaks; and a picture of
    /// single bright texels spreads each into a dimmer patch at a coarser level while the
    /// light over the sphere stays about what the chain's level holds.
    #[test]
    fn a_panoramas_blurred_levels_spread_its_light_and_keep_it() {
        let Some(opened) = open() else {
            return;
        };
        let compositor = &opened.compositor;
        let (width, height) = (128u32, 64u32);
        let grey = compositor
            .upload_rgba8(
                width,
                height,
                &[188u8, 188, 188, 255].repeat((width * height) as usize),
            )
            .expect("the panorama uploads");
        let (_, chain) = compositor
            .panorama_chain(Some(&grey))
            .expect("a panorama of this compositor");
        let blurred = &chain.blurred.get().expect("made with the chain").whole.0;
        let flat = read_level(compositor, &chain.whole.0, 0)[0][0][0];
        for level in 0..blurred.mip_level_count() {
            for (y, row) in read_level(compositor, blurred, level).iter().enumerate() {
                for (x, texel) in row.iter().enumerate() {
                    assert!(
                        (texel[0] - flat).abs() < 0.01 && (texel[3] - 1.0).abs() < 0.01,
                        "level {level} at ({x}, {y}) is {texel:?}, not {flat}"
                    );
                }
            }
        }

        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for y in (4..height - 4).step_by(8) {
            for x in (3..width).step_by(8) {
                let at = ((y * width + x) * 4) as usize;
                rgba[at..at + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        for at in (3..rgba.len()).step_by(4) {
            rgba[at] = 255;
        }
        let stars = compositor
            .upload_rgba8(width, height, &rgba)
            .expect("the panorama uploads");
        let (_, chain) = compositor
            .panorama_chain(Some(&stars))
            .expect("a panorama of this compositor");
        let blurred = &chain.blurred.get().expect("made with the chain").whole.0;
        assert_eq!(
            read_level(compositor, blurred, 0),
            read_level(compositor, &chain.whole.0, 0),
            "level 0 is the chain's, unblurred"
        );
        // Past level 2 each texel of the chain already averages a star of its own.
        for level in 1..3 {
            let (sharp, soft) = (
                read_level(compositor, &chain.whole.0, level),
                read_level(compositor, blurred, level),
            );
            let peak = |l: &[Vec<[f32; 4]>]| {
                l.iter()
                    .flatten()
                    .map(|texel| texel[0])
                    .fold(0.0f32, f32::max)
            };
            assert!(
                peak(&soft) < peak(&sharp),
                "level {level} spreads its stars: {} vs {}",
                peak(&soft),
                peak(&sharp)
            );
            let (before, after) = (total(&sharp), total(&soft));
            assert!(
                (after - before).abs() < 0.15 * before,
                "level {level} keeps its light: {after} vs {before}"
            );
        }
    }

    /// The blurred levels are made with their chain, and again only when the chain is
    /// rebuilt from a picture that changed: a model drawn every frame under a still sky
    /// doesn't blur it every frame.
    #[test]
    fn a_panoramas_blurred_levels_are_made_again_only_when_it_changes() {
        let Some(opened) = open() else {
            return;
        };
        let compositor = &opened.compositor;
        let sky = compositor
            .upload_rgba8(8, 4, &[40u8, 90, 200, 255].repeat(32))
            .expect("the panorama uploads");
        let makes = |compositor: &crate::Compositor| {
            let (_, chain) = compositor
                .panorama_chain(Some(&sky))
                .expect("a panorama of this compositor");
            chain
                .blurred
                .get()
                .expect("made with the chain")
                .makes
                .load(Ordering::Relaxed)
        };
        assert_eq!(makes(compositor), 1, "made with the chain");
        assert_eq!(makes(compositor), 1, "the same picture isn't blurred again");
        sky.mark_changed();
        assert_eq!(makes(compositor), 2, "a changed picture is");
        assert_eq!(makes(compositor), 2, "once");
    }
}
