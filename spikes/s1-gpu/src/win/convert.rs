//! wgpu (D3D12) side: adapter choice, importing shared textures through wgpu-hal, and the
//! BGRA -> NV12 (BT.709 limited range) conversion in WGSL.

use crate::err::{Context, Result, bail};
use wgpu::hal::api::Dx12;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::Interface;

use super::d3d;

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle that covers the whole target; uv (0,0) is the top-left corner.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

// BT.709 on gamma-encoded R'G'B' (WGC delivers sRGB-encoded BGRA8), limited range:
// Y in [16, 235], Cb/Cr in [16, 240], written as 8-bit code value / 255.
fn rgb(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src, samp, uv, 0.0).rgb;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_y(in: VsOut) -> @location(0) f32 {
    return (16.0 + 219.0 * luma(rgb(in.uv))) / 255.0;
}

// Plane 1 is half size, so each fragment's uv is the centre of a 2x2 luma block and the
// bilinear sample averages those four pixels at 1:1 scale.
@fragment
fn fs_uv(in: VsOut) -> @location(0) vec2<f32> {
    let c = rgb(in.uv);
    let y = luma(c);
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec2<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0);
}
"#;

// Field order is drop order, and it matters: wgpu-hal loads d3d12.dll itself (libloading)
// and unloads it when its last instance reference drops. Our raw COM clones must be
// released while the DLL is still loaded, so they come before the wgpu objects. The first
// Windows run crashed in D3D12Core.dll at exit when `d3d12` was released last.
pub struct Gpu {
    pub d3d12: ID3D12Device,
    pub dxgi_adapter: IDXGIAdapter,
    pub queue: wgpu::Queue,
    pub device: wgpu::Device,
    pub adapter: wgpu::Adapter,
    pub selection: String,
}

impl Gpu {
    /// Picks the DX12 adapter that drives `monitor` (so WGC needs no cross-adapter copy),
    /// else wgpu's high-performance adapter.
    pub fn new(monitor: HMONITOR) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
        let mut chosen = None;
        for adapter in adapters {
            if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
                continue;
            }
            if let Some(dxgi) = dxgi_adapter(&adapter)
                && d3d::adapter_drives_monitor(&dxgi, monitor)
            {
                chosen = Some((adapter, "adapter driving the primary display"));
                break;
            }
        }
        let (adapter, selection) = match chosen {
            Some(c) => c,
            None => (
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                    ..Default::default()
                }))
                .context("no DX12 adapter")?,
                "high-performance adapter (none of the DX12 adapters reports the primary \
                 display as its output; WGC will copy across adapters)",
            ),
        };

        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
        {
            bail!("adapter does not support wgpu::Features::TEXTURE_FORMAT_NV12");
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("s1"),
            required_features: wgpu::Features::TEXTURE_FORMAT_NV12,
            ..Default::default()
        }))
        .context("request_device")?;

        let dxgi_adapter = dxgi_adapter(&adapter).context("wgpu adapter is not DX12")?;
        // SAFETY: we only clone the raw ID3D12Device (an AddRef); wgpu keeps ownership of
        // its own reference and we never destroy the device.
        let d3d12 = unsafe { device.as_hal::<Dx12>() }
            .map(|d| d.raw_device().clone())
            .context("wgpu device is not DX12")?;

        Ok(Self {
            adapter,
            device,
            queue,
            dxgi_adapter,
            d3d12,
            selection: selection.to_owned(),
        })
    }

    /// Wraps a D3D12 resource opened from a shared handle as a wgpu texture.
    ///
    /// The resource is described to wgpu as being in `COMMON` (= `PRESENT`) state, and every
    /// frame ends by transitioning it back there before the fence signal, which is the state
    /// D3D11 expects for a shared resource.
    pub fn import_texture(
        &self,
        resource: ID3D12Resource,
        format: wgpu::TextureFormat,
        (width, height): (u32, u32),
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: `resource` is a live 2D texture with one mip and one sample, of exactly
        // `format` and `size` (we created it that way on the D3D11 side).
        let hal_texture = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                format,
                wgpu::TextureDimension::D2,
                size,
                1,
                1,
            )
        };
        // SAFETY: the resource was opened on this wgpu device's own ID3D12Device, matches the
        // descriptor below, has defined contents for our purposes (it is fully overwritten
        // before it is read), and is in COMMON state, which wgpu calls `PRESENT` on D3D12.
        unsafe {
            self.device.create_texture_from_hal::<Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                },
                wgpu::TextureUses::PRESENT,
            )
        }
    }

    /// Stages `Wait(fence, wait)` before and `Signal(fence, signal)` after the next submit.
    pub fn stage_fences(&self, fence: &ID3D12Fence, wait: u64, signal: u64) -> Result<()> {
        // SAFETY: we only use the hal queue to stage fence operations that wgpu itself
        // applies around its next submit; we do not touch the raw queue.
        let hal_queue = unsafe { self.queue.as_hal::<Dx12>() }.context("queue is not DX12")?;
        hal_queue.add_wait_fence(fence.clone(), wait);
        hal_queue.add_signal_fence(fence.clone(), signal);
        Ok(())
    }
}

fn dxgi_adapter(adapter: &wgpu::Adapter) -> Option<IDXGIAdapter> {
    // SAFETY: we only clone the raw DXGI adapter (an AddRef).
    let hal = unsafe { adapter.as_hal::<Dx12>() }?;
    hal.as_raw().cast().ok()
}

/// An NV12 texture shared with D3D11, plus wgpu views of its two planes.
pub struct Nv12Target {
    pub texture: wgpu::Texture,
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
}

impl Nv12Target {
    pub fn new(texture: wgpu::Texture) -> Self {
        let plane = |aspect, format| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("nv12 plane"),
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let y_view = plane(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm);
        let uv_view = plane(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm);
        Self {
            texture,
            y_view,
            uv_view,
        }
    }
}

pub struct Converter {
    y_pipeline: wgpu::RenderPipeline,
    uv_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

impl Converter {
    pub fn new(device: &wgpu::Device, source_view: &wgpu::TextureView) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bgra_to_nv12"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert"),
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
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("convert"),
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bilinear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
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
        };
        Self {
            y_pipeline: pipeline("fs_y", wgpu::TextureFormat::R8Unorm),
            uv_pipeline: pipeline("fs_uv", wgpu::TextureFormat::Rg8Unorm),
            bind_group,
        }
    }

    /// Records the conversion of the shared capture texture into `target`, ending with both
    /// shared textures transitioned back to COMMON for D3D11.
    pub fn record(
        &self,
        device: &wgpu::Device,
        source: &wgpu::Texture,
        target: &Nv12Target,
    ) -> wgpu::CommandBuffer {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("convert"),
        });
        for (view, pipeline) in [
            (&target.y_view, &self.y_pipeline),
            (&target.uv_view, &self.uv_pipeline),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12 plane"),
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
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.transition_resources(
            std::iter::empty(),
            [source, &target.texture]
                .into_iter()
                .map(|texture| wgpu::TextureTransition {
                    texture,
                    selector: None,
                    state: wgpu::TextureUses::PRESENT,
                }),
        );
        encoder.finish()
    }
}
