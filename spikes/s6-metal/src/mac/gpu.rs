//! The wgpu (Metal) side: two devices on one adapter, importing IOSurface planes through
//! wgpu-hal, staging shared-event waits and signals, and the two render pipelines.
//!
//! The producer is its own wgpu device, so its work is on its own `MTLCommandQueue` and
//! its textures are separate `MTLTexture` objects over the same IOSurfaces. Metal tracks
//! no hazards between them, as with a frame ScreenCaptureKit or a decoder wrote: only the
//! synchronisation under test orders the engine's read after the producer's write.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLSharedEvent, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};
use wgpu::hal::api::Metal;

use crate::err::{Context, Result};

/// The probe blocks' size, in pixels, at the top-left of every frame. The import probe is
/// at x 0..64 and the export probe at x 64..128.
pub const PROBE: u32 = 64;

const VS: &str = r#"
// One triangle that covers the whole target.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}
"#;

/// The producer: `load` iterations of busy work per pixel, so the frame takes a while on
/// the GPU, then a moving pattern, with the frame's code in the import probe block. Every
/// pixel runs the loop, and the probe's value depends on its result in a way the compiler
/// can't prove dead, so the probe is among the last things written, not the first.
const PRODUCER: &str = r#"
struct Params { code: u32, load: u32, frame: u32, pad: u32 };
@group(0) @binding(0) var<uniform> p: Params;

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var acc = fract(pos.x * 0.0013 + pos.y * 0.0071 + f32(p.frame) * 0.01);
    for (var i = 0u; i < p.load; i = i + 1u) {
        acc = fract(acc * 1.618 + sin(acc * 6.283));
    }
    var c = vec4<f32>(acc, fract(pos.x / 512.0 + f32(p.frame) / 120.0), fract(pos.y / 256.0), 1.0);
    if (pos.x < 64.0 && pos.y < 64.0) {
        let code = f32(p.code) / 255.0;
        c = vec4<f32>(code, code, code, 1.0);
        if (acc < -1.0) {
            c.r = 0.0;
        }
    }
    return c;
}
"#;

/// The engine's conversion: BGRA to NV12 (BT.709 limited range) at 1:1 scale, with the
/// import probe copied exactly from the source's red channel and the export probe set to
/// the engine's own code for the frame.
const CONVERT: &str = r#"
struct Params { code: u32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> e: Params;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_y(@builtin(position) pos: vec4<f32>) -> @location(0) f32 {
    let p = vec2<i32>(pos.xy);
    let c = textureLoad(src, p, 0).rgb;
    if (p.y < 64 && p.x < 64) {
        return c.r;
    }
    if (p.y < 64 && p.x < 128) {
        return f32(e.code) / 255.0;
    }
    return (16.0 + 219.0 * luma(c)) / 255.0;
}

@fragment
fn fs_uv(@builtin(position) pos: vec4<f32>) -> @location(0) vec2<f32> {
    let p = vec2<i32>(pos.xy) * 2;
    let c = (textureLoad(src, p, 0).rgb + textureLoad(src, p + vec2<i32>(1, 0), 0).rgb
        + textureLoad(src, p + vec2<i32>(0, 1), 0).rgb + textureLoad(src, p + vec2<i32>(1, 1), 0).rgb) * 0.25;
    let y = luma(c);
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec2<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0);
}
"#;

pub type SharedEvent = Retained<ProtocolObject<dyn MTLSharedEvent>>;

/// One wgpu device and its queue, with its Metal device.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub mtl: Retained<ProtocolObject<dyn MTLDevice>>,
}

/// The adapter's description, and the engine's and the producer's devices on it.
pub fn open() -> Result<(wgpu::AdapterInfo, Gpu, Gpu)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        ..Default::default()
    }))
    .context("no Metal adapter")?;
    let engine = Gpu::new(&adapter, "s6 engine")?;
    let producer = Gpu::new(&adapter, "s6 producer")?;
    Ok((adapter.get_info(), engine, producer))
}

impl Gpu {
    fn new(adapter: &wgpu::Adapter, label: &str) -> Result<Self> {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            ..Default::default()
        }))
        .with_context(|| format!("request_device for {label}"))?;
        // SAFETY: only clones the raw MTLDevice (a retain); wgpu keeps its own reference
        // and the device is never released through this one early.
        let mtl = unsafe { device.as_hal::<Metal>() }
            .map(|d| d.raw_device().clone())
            .context("the wgpu device is not Metal")?;
        Ok(Self { device, queue, mtl })
    }

    /// A new shared event at 0, on this device.
    pub fn shared_event(&self) -> Result<SharedEvent> {
        self.mtl
            .newSharedEvent()
            .context("newSharedEvent returned nothing")
    }

    /// Wraps plane `plane` of `surface`, `size` in pixels, as a wgpu texture of `format`.
    pub fn import(
        &self,
        surface: &IOSurfaceRef,
        plane: usize,
        format: wgpu::TextureFormat,
        (width, height): (u32, u32),
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> Result<wgpu::Texture> {
        let mtl_format = match format {
            wgpu::TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
            wgpu::TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
            wgpu::TextureFormat::Rg8Unorm => MTLPixelFormat::RG8Unorm,
            other => crate::err::bail!("no Metal format for {other:?}"),
        };
        let mut mtl_usage = MTLTextureUsage::empty();
        if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
            mtl_usage |= MTLTextureUsage::ShaderRead;
        }
        if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
            mtl_usage |= MTLTextureUsage::RenderTarget;
        }
        // SAFETY: a class constructor with a valid format and a non-zero size.
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format,
                width as usize,
                height as usize,
                false,
            )
        };
        desc.setUsage(mtl_usage);
        // `surface`'s plane `plane` is `width` x `height` of a layout matching `mtl_format`
        // (the pixel buffer was made that way), so Metal wraps it without a copy.
        let raw = self
            .mtl
            .newTextureWithDescriptor_iosurface_plane(&desc, surface, plane)
            .with_context(|| format!("wrapping plane {plane} of an IOSurface as {label}"))?;
        // SAFETY: `raw` is a live 2D texture with one mip and one layer, of exactly
        // `format` and this size, made on this wgpu device's MTLDevice.
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            )
        };
        // SAFETY: the texture was made from this device's MTLDevice and matches the
        // descriptor below. Metal has no layouts, and every frame overwrites the whole
        // texture before it is read, so its contents may be treated as uninitialised.
        Ok(unsafe {
            self.device.create_texture_from_hal::<Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                },
                wgpu::TextureUses::UNINITIALIZED,
            )
        })
    }

    /// Stages a GPU wait for `event` to reach `value` before the next submit's work.
    pub fn stage_wait(&self, event: &SharedEvent, value: u64) -> Result<()> {
        // SAFETY: only stages an event wait that wgpu-hal encodes around its next submit;
        // the raw queue is not touched.
        let queue = unsafe { self.queue.as_hal::<Metal>() }.context("the queue is not Metal")?;
        queue.add_wait_event(event.clone(), value);
        Ok(())
    }

    /// Stages a signal of `event` to `value` once the next submit's work completes.
    pub fn stage_signal(&self, event: &SharedEvent, value: u64) -> Result<()> {
        // SAFETY: as in `stage_wait`.
        let queue = unsafe { self.queue.as_hal::<Metal>() }.context("the queue is not Metal")?;
        queue.add_signal_event(event.clone(), value);
        Ok(())
    }

    /// Turns on wgpu-hal's strict event sync for this queue, for good.
    pub fn enable_strict_event_sync(&self) -> Result<()> {
        // SAFETY: as in `stage_wait`; this only changes how wgpu-hal encodes the waits.
        let queue = unsafe { self.queue.as_hal::<Metal>() }.context("the queue is not Metal")?;
        queue
            .enable_strict_event_sync()
            .map_err(|e| crate::err::Error::msg(format!("enable_strict_event_sync: {e}")))
    }

    /// The raw Metal device's address, to tell whether two devices share one.
    pub fn mtl_address(&self) -> usize {
        Retained::as_ptr(&self.mtl).cast::<u8>() as usize
    }
}

/// A 16-byte uniform buffer of four `u32`s.
fn uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Four `u32`s as bytes, for a uniform buffer.
pub fn words(w: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, v) in w.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
    }
    out
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    vs: &wgpu::ShaderModule,
    fs: &wgpu::ShaderModule,
    entry: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: vs,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: fs,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn shader(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("s6 pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

/// The producer's pipeline, writing frames into its imported source textures.
pub struct Producer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
}

impl Producer {
    pub fn new(device: &wgpu::Device) -> Self {
        let vs = shader(device, "s6 vs", VS);
        let fs = shader(device, "s6 producer", PRODUCER);
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("s6 producer"),
            entries: &[uniform_entry(0)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("s6 producer"),
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let params = uniform(device, "s6 producer params");
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("s6 producer"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });
        Self {
            pipeline: pipeline(
                device,
                &layout,
                &vs,
                &fs,
                "fs",
                wgpu::TextureFormat::Bgra8Unorm,
            ),
            bind_group,
            params,
        }
    }

    /// Writes frame `frame`, with probe `code`, into `target`.
    pub fn record(
        &self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        code: u8,
        load: u32,
        frame: u32,
    ) -> wgpu::CommandBuffer {
        gpu.queue
            .write_buffer(&self.params, 0, &words([u32::from(code), load, frame, 0]));
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("s6 producer"),
            });
        {
            let mut pass = pass(&mut encoder, target);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
}

/// The engine's BGRA-to-NV12 conversion, one bind group per ring slot.
pub struct Converter {
    y_pipeline: wgpu::RenderPipeline,
    uv_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl Converter {
    pub fn new(device: &wgpu::Device) -> Self {
        let vs = shader(device, "s6 vs", VS);
        let fs = shader(device, "s6 convert", CONVERT);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("s6 convert"),
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
                uniform_entry(1),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("s6 convert"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        Self {
            y_pipeline: pipeline(
                device,
                &pipeline_layout,
                &vs,
                &fs,
                "fs_y",
                wgpu::TextureFormat::R8Unorm,
            ),
            uv_pipeline: pipeline(
                device,
                &pipeline_layout,
                &vs,
                &fs,
                "fs_uv",
                wgpu::TextureFormat::Rg8Unorm,
            ),
            params: uniform(device, "s6 convert params"),
            layout,
        }
    }

    /// The bind group that reads `source`.
    pub fn bind(&self, device: &wgpu::Device, source: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("s6 convert"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        })
    }

    /// Converts the source behind `bind_group` into the `y` and `uv` planes, with the
    /// export probe set to `code`.
    pub fn record(
        &self,
        gpu: &Gpu,
        bind_group: &wgpu::BindGroup,
        y: &wgpu::TextureView,
        uv: &wgpu::TextureView,
        code: u8,
    ) -> wgpu::CommandBuffer {
        gpu.queue
            .write_buffer(&self.params, 0, &words([u32::from(code), 0, 0, 0]));
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("s6 convert"),
            });
        for (view, pipeline) in [(y, &self.y_pipeline), (uv, &self.uv_pipeline)] {
            let mut pass = pass(&mut encoder, view);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
}
