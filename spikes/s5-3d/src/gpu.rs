//! The GPU path: one textured convex polygon per item, drawn bottom first with
//! premultiplied src-over, into an sRGB target that is read back.

use std::time::Duration;

use crate::geom::Vertex;
use crate::image::Image;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
}

/// How a render samples and antialiases.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub mips: bool,
    pub anisotropy: u16,
    pub msaa: u32,
    pub analytic: bool,
}

/// One item as the GPU draws it: a fan over `vertices`, with analytic coverage from
/// `edges` (the unexpanded polygon's lines).
pub struct Draw {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<[f32; 3]>,
}

impl Gpu {
    pub fn open(software: bool) -> Self {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.flags = wgpu::InstanceFlags::empty().with_env();
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: software,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .expect("no adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("s5-3d"),
            ..Default::default()
        }))
        .expect("device");
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("s5-item"),
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
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("s5-item"),
            source: wgpu::ShaderSource::Wgsl(include_str!("item.wgsl").into()),
        });
        Gpu {
            device,
            queue,
            adapter,
            layout,
            shader,
        }
    }

    pub fn description(&self) -> String {
        let info = self.adapter.get_info();
        format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
    }

    /// The texture with a full mip chain, or level 0 only.
    pub fn upload(&self, levels: &[Image], mips: bool) -> wgpu::Texture {
        let count = if mips { levels.len() as u32 } else { 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("s5-source"),
            size: wgpu::Extent3d {
                width: levels[0].width,
                height: levels[0].height,
                depth_or_array_layers: 1,
            },
            mip_level_count: count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, image) in levels.iter().take(count as usize).enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.to_srgb8(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        texture
    }

    pub fn render(
        &self,
        width: u32,
        height: u32,
        background: [f32; 3],
        texture: &wgpu::Texture,
        draws: &[Draw],
        options: Options,
    ) -> Vec<u8> {
        let device = &self.device;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let pipeline = self.pipeline(options.msaa);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("s5-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: if options.mips {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            anisotropy_clamp: options.anisotropy.max(1),
            ..Default::default()
        });
        let view = texture.create_view(&Default::default());
        let target_desc = |samples: u32, usage| wgpu::TextureDescriptor {
            label: Some("s5-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        };
        let output = device.create_texture(&target_desc(
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        ));
        let output_view = output.create_view(&Default::default());
        let msaa = (options.msaa > 1).then(|| {
            device
                .create_texture(&target_desc(
                    options.msaa,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                ))
                .create_view(&Default::default())
        });

        let mut bind_groups = Vec::new();
        let mut vertex_data: Vec<f32> = Vec::new();
        let mut ranges = Vec::new();
        for draw in draws {
            // Uniform: canvas (2 f32), edge_count, analytic (2 u32), then 8 vec4 edges.
            let mut uniform = vec![0u8; 16 + 8 * 16];
            uniform[0..4].copy_from_slice(&(width as f32).to_le_bytes());
            uniform[4..8].copy_from_slice(&(height as f32).to_le_bytes());
            uniform[8..12].copy_from_slice(&(draw.edges.len() as u32).to_le_bytes());
            uniform[12..16].copy_from_slice(&u32::from(options.analytic).to_le_bytes());
            for (i, e) in draw.edges.iter().enumerate() {
                for (j, v) in e.iter().enumerate() {
                    let at = 16 + i * 16 + j * 4;
                    uniform[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("s5-item-uniform"),
                size: uniform.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &uniform);
            bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("s5-item"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            }));
            let first = (vertex_data.len() / 5) as u32;
            let v = &draw.vertices;
            for i in 1..v.len().saturating_sub(1) {
                for vertex in [v[0], v[i], v[i + 1]] {
                    vertex_data.extend([
                        vertex.canvas[0],
                        vertex.canvas[1],
                        vertex.w,
                        vertex.uv[0],
                        vertex.uv[1],
                    ]);
                }
            }
            ranges.push(first..(vertex_data.len() / 5) as u32);
        }
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("s5-vertices"),
            size: (vertex_data.len().max(1) * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bytes: Vec<u8> = vertex_data.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.queue.write_buffer(&vertices, 0, &bytes);

        let row = (width * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("s5-readback"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let (view, resolve) = match &msaa {
                Some(msaa) => (msaa, Some(&output_view)),
                None => (&output_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("s5-composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(background[0]),
                            g: f64::from(background[1]),
                            b: f64::from(background[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_vertex_buffer(0, vertices.slice(..));
            for (bind_group, range) in bind_groups.iter().zip(ranges) {
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(range, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
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
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (send, recv) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        recv.recv_timeout(Duration::from_secs(60))
            .expect("map")
            .expect("map ok");
        let data = slice.get_mapped_range().expect("mapped");
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * row) as usize;
            out.extend_from_slice(&data[start..start + (width * 4) as usize]);
        }
        out
    }

    fn pipeline(&self, samples: u32) -> wgpu::RenderPipeline {
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("s5-item"),
                bind_group_layouts: &[Some(&self.layout)],
                immediate_size: 0,
            });
        let blend = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32x2];
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("s5-item"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 20,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        blend: Some(wgpu::BlendState {
                            color: blend,
                            alpha: blend,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
    }
}
