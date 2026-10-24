//! The GPU copy of a captured frame (ARCH › GPU pipeline, steps 1 and 2): an imported
//! DMA-BUF is drawn into a ring of the stream's own textures, cropped and, for the
//! formats whose fourth byte is padding, made opaque. A ring texture is the frame
//! render samples, so the PipeWire buffer can go back to the compositor as soon as the
//! copy is done.
//!
//! The frame textures are `Rgba8UnormSrgb`, the format render imports, and are drawn
//! through an `Rgba8Unorm` view from an UNORM source, so the bytes move unchanged.

use std::sync::Arc;

use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrameInfo};
use jstream_platform::{GpuFrame, PlatformError};

use super::{QueueOrder, Shared};

/// Ring textures per capture: one render is reading, one waiting in the mailbox, one
/// being drawn, and one spare for a frame render holds a tick longer.
pub(crate) const SLOTS: usize = 4;

const SHADER: &str = r"
struct Params {
    origin: vec2<u32>,
    opaque: u32,
    pad: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole target.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(source, vec2<u32>(at.xy) + params.origin, 0);
    return select(c, vec4<f32>(c.rgb, 1.0), params.opaque != 0u);
}
";

/// One ring texture and its hold. The hold's strong count says whether a frame still
/// uses the texture.
struct Slot {
    // The hold drops before the texture, as in `GpuFrame`.
    sync: Arc<QueueOrder>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// What [`Ring::copy`] draws: the source texture, and the part of it to show.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Source<'a> {
    /// The imported frame, UNORM.
    pub(crate) texture: &'a wgpu::Texture,
    /// The visible rectangle (x, y, width, height), inside the texture.
    pub(crate) visible: (u32, u32, u32, u32),
    /// Whether the fourth byte is padding, drawn as opaque.
    pub(crate) padded: bool,
}

/// One capture stream's copy pipeline and ring. Used by its PipeWire thread only.
pub(crate) struct Ring {
    shared: Arc<Shared>,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    slots: Vec<Slot>,
    size: (u32, u32),
    next: usize,
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring")
            .field("size", &self.size)
            .field("slots", &self.slots.len())
            .finish_non_exhaustive()
    }
}

impl Ring {
    /// The pipeline, on the context's device. Its textures are allocated on the first
    /// copy, at the frame's size.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        let device = &shared.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream capture copy"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream capture copy"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream capture copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream capture copy"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream capture copy"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            shared,
            pipeline,
            layout,
            params,
            slots: Vec::new(),
            size: (0, 0),
            next: 0,
        }
    }

    /// Reallocates the ring for frames of `size`. Frames still held keep their own
    /// textures.
    fn resize(&mut self, (width, height): (u32, u32)) {
        if self.size == (width, height) && !self.slots.is_empty() {
            return;
        }
        let device = &self.shared.device;
        self.slots = (0..SLOTS)
            .map(|_| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("jstream capture frame"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("jstream capture frame (UNORM)"),
                    format: Some(wgpu::TextureFormat::Rgba8Unorm),
                    ..Default::default()
                });
                Slot {
                    sync: Arc::new(QueueOrder::new(Arc::clone(&self.shared))),
                    texture,
                    view,
                }
            })
            .collect();
        self.size = (width, height);
        self.next = 0;
    }

    /// The next slot no frame holds, if any.
    fn free(&mut self) -> Option<usize> {
        let len = self.slots.len();
        let at = (0..len)
            .map(|step| (self.next + step) % len)
            .find(|&at| Arc::strong_count(&self.slots[at].sync) == 1)?;
        self.next = (at + 1) % len;
        Some(at)
    }

    /// Draws `source` into a free slot and submits it, returning the slot as a frame at
    /// `pts`, and the submission to wait for before the source may be written again.
    /// `Ok(None)` when frames hold every slot (the frame is dropped).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for an empty visible rectangle.
    pub(crate) fn copy(
        &mut self,
        source: Source<'_>,
        pts: MediaTime,
    ) -> Result<Option<(GpuFrame, wgpu::SubmissionIndex)>, PlatformError> {
        let (x, y, width, height) = source.visible;
        let size = FrameSize::new(width, height).map_err(|e| PlatformError::Unsupported {
            what: format!("a captured frame of {width}x{height}: {e}"),
        })?;
        self.resize((width, height));
        let Some(at) = self.free() else {
            return Ok(None);
        };
        let device = &self.shared.device;
        let queue = &self.shared.queue;
        let mut params = [0u8; 16];
        params[0..4].copy_from_slice(&x.to_ne_bytes());
        params[4..8].copy_from_slice(&y.to_ne_bytes());
        params[8..12].copy_from_slice(&u32::from(source.padded).to_ne_bytes());
        queue.write_buffer(&self.params, 0, &params);
        let view = source
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream capture copy"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let slot = &self.slots[at];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream capture copy"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream capture copy"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &slot.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        let index = queue.submit([encoder.finish()]);
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Rgba8,
            color: ColorInfo::default(),
            pts,
        };
        let sync: Arc<dyn jstream_platform::FrameSync> = slot.sync.clone();
        Ok(Some((
            GpuFrame::new(slot.texture.clone(), info, sync),
            index,
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use jstream_platform::media::MediaTime;
    use jstream_platform::{GpuInterop, PlatformError};

    use super::{Ring, SLOTS, Source};
    use crate::gpu::{LinuxGpu, Shared};
    use crate::timebase;

    /// A 4x2 source whose pixel (x, y) is [x, y, 7, 99], read back from the frame
    /// texture as straight bytes.
    fn source(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // BGRA in memory: the shader reads (r, g, b, a) = (x, y, 7, 99).
        let bytes: Vec<u8> = (0..2u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [7, y, x, 99]))
            .collect();
        queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16),
                rows_per_image: None,
            },
            texture.size(),
        );
        texture
    }

    fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("maps"));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .expect("the readback completes");
        let data = buffer.slice(..).get_mapped_range().expect("mapped");
        (0..h as usize)
            .flat_map(|row| data[row * 256..row * 256 + w as usize * 4].to_vec())
            .collect()
    }

    /// On a software adapter where there is no GPU: the copy crops, keeps the bytes,
    /// makes padding opaque, and hands out each slot only while no frame holds it.
    #[test]
    fn the_copy_crops_keeps_bytes_and_recycles_free_slots() {
        let context = match LinuxGpu::with_software().open_device() {
            Ok(context) => context,
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("{e}"),
        };
        let shared = Shared::of(&context, "test").expect("ours");
        let (device, queue) = (context.device(), context.queue());
        let texture = source(device, queue);
        let mut ring = Ring::new(Arc::clone(&shared));
        let pts = MediaTime::new(42, timebase());
        let (frame, _) = ring
            .copy(
                Source {
                    texture: &texture,
                    visible: (1, 1, 2, 1),
                    padded: true,
                },
                pts,
            )
            .expect("copies")
            .expect("a free slot");
        assert_eq!(frame.info().pts, pts);
        assert_eq!(
            (frame.info().size.width(), frame.info().size.height()),
            (2, 1)
        );
        assert_eq!(
            frame.texture().format(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            read(device, queue, frame.texture()),
            [1, 1, 7, 255, 2, 1, 7, 255]
        );
        frame.stage(queue).expect("the context's own queue");
        // Straight alpha is kept when the fourth byte is real.
        let whole = Source {
            texture: &texture,
            visible: (0, 0, 4, 2),
            padded: false,
        };
        let (frame, _) = ring.copy(whole, pts).expect("copies").expect("free");
        assert_eq!(
            &read(device, queue, frame.texture())[..8],
            [0, 0, 7, 99, 1, 0, 7, 99]
        );
        // Held frames keep their slots; once every slot is held, frames are dropped.
        let mut held = vec![frame];
        for _ in 1..SLOTS {
            held.push(ring.copy(whole, pts).expect("copies").expect("free").0);
        }
        assert!(ring.copy(whole, pts).expect("copies").is_none());
        held.pop();
        assert!(ring.copy(whole, pts).expect("copies").is_some());
        // A frame staged on another device's queue is refused.
        if let Ok(other) = LinuxGpu::with_software().open_device() {
            assert!(matches!(
                held[0].stage(other.queue()),
                Err(PlatformError::InvalidState { .. })
            ));
        }
    }
}
