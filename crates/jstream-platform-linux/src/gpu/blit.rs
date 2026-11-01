//! The GPU copy of a captured frame (ARCH › GPU pipeline, steps 1 and 2): an imported
//! DMA-BUF is drawn into a ring of the stream's own textures, cropped and, for the
//! formats whose fourth byte is padding, made opaque. A ring texture is the frame
//! render samples, so the PipeWire or V4L2 buffer can go back to its producer as soon as
//! the copy is done.
//!
//! The frame textures are `Rgba8UnormSrgb`, the format render imports, and are drawn
//! through an `Rgba8Unorm` view from an UNORM source, so the bytes move unchanged.
//!
//! A ring can also convert as it draws ([`Pass`]): packed 4:2:2 (`YUYV`, imported as RGBA
//! at half width, each texel `Y0 U Y1 V`) to R'G'B' with the frame's matrix and range, as
//! the engine's CPU path converts NV12 (`jstream-core`'s `rgba_from_surface`): each chroma
//! sample covers its two pixels, and limited range is scaled to full; and NV12, its luma
//! and chroma planes imported as `R8` and `RG8` textures, the same way, each chroma
//! sample covering its 2x2 block.

use std::sync::Arc;

use jstream_platform::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, VideoFrameInfo,
};
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

/// The YCbCr to R'G'B' conversion shared by the converting passes. `range` is the luma
/// offset and scale, then the chroma scale and offset; `coef` is Cr to R, Cb to G, Cr to
/// G and Cb to B.
const YUV: &str = r"
struct Params {
    origin: vec2<u32>,
    opaque: u32,
    pad: u32,
    range: vec4<f32>,
    coef: vec4<f32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn rgb(y: f32, cb: f32, cr: f32) -> vec4<f32> {
    let l = (y - params.range.x) * params.range.y;
    let u = (cb - params.range.w) * params.range.z;
    let v = (cr - params.range.w) * params.range.z;
    let c = vec3<f32>(
        l + params.coef.x * v,
        l - params.coef.y * u - params.coef.z * v,
        l + params.coef.w * u,
    );
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
";

/// [`Pass::Yuyv`]'s fragment shader, after [`YUV`]: output pixel x reads texel x / 2,
/// its first luma when x is even and its second when odd, and the texel's chroma.
const YUYV: &str = r"
@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<u32>(at.xy);
    let t = textureLoad(source, vec2<u32>(p.x / 2u, p.y), 0);
    return rgb(select(t.r, t.b, (p.x & 1u) == 1u), t.g, t.a);
}
";

/// [`Pass::Nv12`]'s fragment shader, after [`YUV`]: luma from the `R8` plane at the
/// pixel, chroma from the `RG8` plane at half its coordinates.
const NV12: &str = r"
@group(0) @binding(2) var chroma: texture_2d<f32>;

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<u32>(at.xy);
    let c = textureLoad(chroma, p / 2u, 0);
    return rgb(textureLoad(source, p, 0).r, c.r, c.g);
}
";

/// How a ring draws its source into a frame texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Copied as it is, cropped, opaque when the fourth byte is padding.
    Copy,
    /// Packed 4:2:2 in an RGBA texture of half the frame's width, converted to R'G'B'.
    Yuyv,
    /// NV12's luma (`R8`, the frame's size) and chroma (`RG8`, half each way, rounded
    /// up) planes, converted to R'G'B'.
    Nv12,
}

impl Pass {
    fn shader(self) -> String {
        match self {
            Self::Copy => SHADER.to_owned(),
            Self::Yuyv => format!("{YUV}{YUYV}"),
            Self::Nv12 => format!("{YUV}{NV12}"),
        }
    }

    /// How many source textures it reads.
    pub(crate) fn planes(self) -> usize {
        match self {
            Self::Copy | Self::Yuyv => 1,
            Self::Nv12 => 2,
        }
    }
}

/// The parameters [`YUV`] converts with, for frames in `color`: the luma offset and
/// scale, the chroma scale and offset, then Cr to R, Cb to G, Cr to G and Cb to B, in the
/// shader's 0 to 1 code values. The same matrices and ranges as the engine's CPU path.
pub(crate) fn yuv_params(color: ColorInfo) -> [f32; 8] {
    let (kr, kb) = match color.space {
        ColorSpace::Bt601 => (0.299, 0.114),
        ColorSpace::Bt2020 => (0.2627, 0.0593),
        _ => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    let (y_offset, y_scale, c_scale) = match color.range {
        ColorRange::Full => (0.0, 1.0, 1.0),
        ColorRange::Limited => (16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0),
    };
    [
        y_offset,
        y_scale,
        c_scale,
        128.0 / 255.0,
        2.0 * (1.0 - kr),
        2.0 * kb * (1.0 - kb) / kg,
        2.0 * kr * (1.0 - kr) / kg,
        2.0 * (1.0 - kb),
    ]
}

/// The size of the parameters buffer: the origin, opacity and padding words, then
/// [`yuv_params`].
const PARAMS: usize = 48;

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

/// One capture stream's copy pipeline and ring. Used by its PipeWire or V4L2 thread only.
pub(crate) struct Ring {
    shared: Arc<Shared>,
    pass: Pass,
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
            .field("pass", &self.pass)
            .field("size", &self.size)
            .field("slots", &self.slots.len())
            .finish_non_exhaustive()
    }
}

impl Ring {
    /// The copying pipeline, on the context's device. Its textures are allocated on the
    /// first copy, at the frame's size.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self::with_pass(shared, Pass::Copy)
    }

    /// The pipeline drawing as `pass` says, on the context's device.
    pub(crate) fn with_pass(shared: Arc<Shared>, pass: Pass) -> Self {
        let device = &shared.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream capture copy"),
            source: wgpu::ShaderSource::Wgsl(pass.shader().into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        // The first source, the parameters, then any further sources from binding 2.
        let mut entries = vec![
            texture(0),
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
        ];
        entries.extend((2..).take(pass.planes() - 1).map(texture));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream capture copy"),
            entries: &entries,
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
            size: PARAMS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            shared,
            pass,
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
        self.draw(
            &[source.texture],
            source.visible,
            source.padded,
            ColorInfo::default(),
            pts,
        )
    }

    /// Converts a YCbCr frame of `size` pixels in `color`, its source textures `planes`
    /// as the ring's [`Pass`] reads them, into a free slot, as [`copy`](Self::copy)
    /// does.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for an empty size, or another number of planes
    /// than the pass reads.
    pub(crate) fn convert(
        &mut self,
        planes: &[&wgpu::Texture],
        (width, height): (u32, u32),
        color: ColorInfo,
        pts: MediaTime,
    ) -> Result<Option<(GpuFrame, wgpu::SubmissionIndex)>, PlatformError> {
        self.draw(planes, (0, 0, width, height), true, color, pts)
    }

    fn draw(
        &mut self,
        planes: &[&wgpu::Texture],
        visible: (u32, u32, u32, u32),
        padded: bool,
        color: ColorInfo,
        pts: MediaTime,
    ) -> Result<Option<(GpuFrame, wgpu::SubmissionIndex)>, PlatformError> {
        if planes.len() != self.pass.planes() {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "{} source textures for a {:?} pass",
                    planes.len(),
                    self.pass
                ),
            });
        }
        let (x, y, width, height) = visible;
        let size = FrameSize::new(width, height).map_err(|e| PlatformError::Unsupported {
            what: format!("a captured frame of {width}x{height}: {e}"),
        })?;
        self.resize((width, height));
        let Some(at) = self.free() else {
            return Ok(None);
        };
        let device = &self.shared.device;
        let queue = &self.shared.queue;
        let mut params = [0u8; PARAMS];
        params[0..4].copy_from_slice(&x.to_ne_bytes());
        params[4..8].copy_from_slice(&y.to_ne_bytes());
        params[8..12].copy_from_slice(&u32::from(padded).to_ne_bytes());
        for (at, value) in (16..).step_by(4).zip(yuv_params(color)) {
            params[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&self.params, 0, &params);
        let views: Vec<wgpu::TextureView> = planes
            .iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: self.params.as_entire_binding(),
            },
        ];
        entries.extend(
            views[1..]
                .iter()
                .zip(2..)
                .map(|(view, binding)| wgpu::BindGroupEntry {
                    binding,
                    resource: wgpu::BindingResource::TextureView(view),
                }),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream capture copy"),
            layout: &self.layout,
            entries: &entries,
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
        // Under the context's submit lock, so this copy can't take the wait or signal
        // render staged for its own next submit (#667).
        let index = {
            let _submitting = self.shared.submit_lock().lock();
            queue.submit([encoder.finish()])
        };
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

    /// The copy submits under the context's submit lock (#667): while render holds it,
    /// between staging a frame and its submit, the PipeWire thread's copy waits, and it
    /// goes on once the lock is free.
    #[test]
    fn the_copy_waits_for_the_contexts_submit_lock() {
        let context = match LinuxGpu::with_software().open_device() {
            Ok(context) => context,
            Err(PlatformError::Unsupported { .. }) => return,
            Err(e) => panic!("{e}"),
        };
        let shared = Shared::of(&context, "test").expect("ours");
        let texture = source(context.device(), context.queue());
        let mut ring = Ring::new(Arc::clone(&shared));
        let held = context.submit_lock().lock();
        let (done, copied) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let whole = Source {
                texture: &texture,
                visible: (0, 0, 4, 2),
                padded: false,
            };
            let copy = ring.copy(whole, MediaTime::new(0, timebase()));
            done.send(copy.map(|c| c.is_some()))
                .expect("the test waits");
        });
        assert!(
            copied.recv_timeout(Duration::from_millis(200)).is_err(),
            "the copy submitted while the context's submit lock was held"
        );
        drop(held);
        let copied = copied
            .recv_timeout(Duration::from_secs(30))
            .expect("the copy goes on once the lock is free");
        assert!(copied.expect("copies"), "a free slot");
        thread.join().expect("no panic");
    }
}
