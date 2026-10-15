//! wgpu (D3D12) side: adapter choice, opening CEF's shared texture on wgpu's device, the
//! copy out of CEF's pool, and a compositor pass that blends the page over an opaque
//! background. Only the probe row is read back to the CPU, for the checks.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::err::{Context, Result, bail};
use s4_browser::analysis::Px;
use s4_browser::json::{self, Obj};
use s4_browser::page::{PROBE_Y, ROW_PIXELS};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{HANDLE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{D3D12CreateDevice, ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
};
use windows::core::Interface;

/// The compositor's opaque background. It has no green, so the 50 % green probe shows
/// whether blending happened.
pub const BACKGROUND: Px = Px::rgba(0, 0, 255, 255);

const ROW_BYTES: u64 = ROW_PIXELS as u64 * 4;
const GPU_TIMEOUT: Duration = Duration::from_secs(2);

const SHADER: &str = r#"
@group(0) @binding(0) var page: texture_2d<f32>;

// One triangle that covers the whole target.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

// 1:1 pixels, so no filtering touches the probe cells. CEF's pixels are premultiplied;
// the blend state does `src + dst * (1 - src.a)`.
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(page, vec2<i32>(pos.xy), 0);
}
"#;

/// Facts about a DXGI adapter for the report.
pub struct AdapterFacts {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub luid: LUID,
    pub driver_version: String,
}

impl AdapterFacts {
    pub fn json(&self) -> String {
        Obj::new()
            .str("name", &self.name)
            .str("vendor_id", &format!("0x{:04x}", self.vendor_id))
            .str("device_id", &format!("0x{:04x}", self.device_id))
            .str("luid", &format!("{:#x}", luid_u64(self.luid)))
            .str("driver_version", &self.driver_version)
            .build()
    }
}

pub fn adapter_facts(adapter: &IDXGIAdapter) -> Result<AdapterFacts> {
    let a1: IDXGIAdapter1 = adapter.cast().context("IDXGIAdapter1")?;
    // SAFETY: plain method call on a live COM object.
    let desc = unsafe { a1.GetDesc1() }.context("IDXGIAdapter1::GetDesc1")?;
    let name_len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    // The user-mode driver version, as Device Manager shows it (a.b.c.d).
    // SAFETY: plain method call; the GUID pointer refers to a static.
    let driver_version = match unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) } {
        Ok(v) => {
            let v = v as u64;
            format!(
                "{}.{}.{}.{}",
                (v >> 48) & 0xffff,
                (v >> 32) & 0xffff,
                (v >> 16) & 0xffff,
                v & 0xffff
            )
        }
        Err(e) => format!("unknown ({e})"),
    };
    Ok(AdapterFacts {
        name: String::from_utf16_lossy(&desc.Description[..name_len]),
        vendor_id: desc.VendorId,
        device_id: desc.DeviceId,
        luid: desc.AdapterLuid,
        driver_version,
    })
}

pub fn luid_u64(luid: LUID) -> u64 {
    ((luid.HighPart as u32 as u64) << 32) | u64::from(luid.LowPart)
}

/// Every DXGI adapter, in enumeration order (index 0 is the one Chromium's GPU process
/// uses unless told otherwise).
pub fn dxgi_adapters() -> Result<Vec<IDXGIAdapter>> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1")?;
    let mut out = Vec::new();
    for i in 0.. {
        // SAFETY: plain method call; an error (DXGI_ERROR_NOT_FOUND) ends the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        out.push(adapter.cast().context("IDXGIAdapter")?);
    }
    Ok(out)
}

/// Which adapter a shared handle belongs to: tries to open it on a D3D12 device on each
/// adapter. Only for the report, after the import on wgpu's device failed.
pub fn owner_of(handle: *mut c_void) -> String {
    let Ok(adapters) = dxgi_adapters() else {
        return "unknown (no DXGI factory)".into();
    };
    for adapter in adapters {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: `adapter` is live and `device` is a valid out-pointer.
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_err() {
            continue;
        }
        let Some(device) = device else { continue };
        let mut opened: Option<ID3D12Resource> = None;
        // SAFETY: the handle is the one CEF passed to the callback we are still inside;
        // `opened` is a valid out-pointer and is dropped right away.
        if unsafe { device.OpenSharedHandle(HANDLE(handle), &mut opened) }.is_ok() {
            return adapter_facts(&adapter).map_or_else(|e| e.to_string(), |f| f.name);
        }
    }
    "none of the DXGI adapters could open it".into()
}

// Field order is drop order: the raw COM clones go before the wgpu objects, because
// wgpu-hal unloads d3d12.dll when its last instance reference drops (spike S1's exit
// crash).
pub struct Gpu {
    d3d12: ID3D12Device,
    /// wgpu errors raised outside an error scope. wgpu's default handler panics, and a
    /// panic inside a CEF callback aborts with no report, so they become failed paints.
    errors: Arc<Mutex<Option<String>>>,
    pub dxgi_adapter: IDXGIAdapter,
    queue: wgpu::Queue,
    device: wgpu::Device,
    pub adapter: wgpu::Adapter,
    pub selection: String,
}

impl Gpu {
    /// Picks the wgpu DX12 adapter that is DXGI adapter 0, the one Chromium renders on by
    /// default, because a shared texture only opens on the adapter that made it. Falls
    /// back to wgpu's high-performance adapter.
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let first_luid = dxgi_adapters()?
            .first()
            .map(|a| adapter_facts(a).map(|f| luid_u64(f.luid)))
            .transpose()?;
        let mut chosen = None;
        for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12)) {
            let luid = dxgi_adapter(&adapter)
                .and_then(|a| adapter_facts(&a).ok())
                .map(|f| luid_u64(f.luid));
            if luid.is_some() && luid == first_luid {
                chosen = Some((adapter, "DXGI adapter 0 (Chromium's default)"));
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
                "high-performance adapter (DXGI adapter 0 has no wgpu DX12 adapter)",
            ),
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("s4"),
            ..Default::default()
        }))
        .context("request_device")?;
        let dxgi_adapter = dxgi_adapter(&adapter).context("wgpu adapter is not DX12")?;
        // SAFETY: we only clone the raw ID3D12Device (an AddRef); wgpu keeps ownership of
        // its own reference and we never destroy the device.
        let d3d12 = unsafe { device.as_hal::<Dx12>() }
            .map(|d| d.raw_device().clone())
            .context("wgpu device is not DX12")?;
        let errors = Arc::new(Mutex::new(None::<String>));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            let mut slot = sink.lock().unwrap_or_else(|p| p.into_inner());
            slot.get_or_insert_with(|| e.to_string());
        }));
        Ok(Self {
            d3d12,
            errors,
            dxgi_adapter,
            queue,
            device,
            adapter,
            selection: selection.to_owned(),
        })
    }

    /// Fails with the first wgpu error raised since the last check.
    fn check(&self) -> Result<()> {
        match self.errors.lock().unwrap_or_else(|p| p.into_inner()).take() {
            Some(e) => bail!("wgpu error: {e}"),
            None => Ok(()),
        }
    }

    fn wait(&self, index: wgpu::SubmissionIndex) -> Result<()> {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(GPU_TIMEOUT),
            })
            .context("waiting for the GPU")?;
        Ok(())
    }
}

fn dxgi_adapter(adapter: &wgpu::Adapter) -> Option<IDXGIAdapter> {
    // SAFETY: we only clone the raw DXGI adapter (an AddRef).
    let hal = unsafe { adapter.as_hal::<Dx12>() }?;
    hal.as_raw().cast().ok()
}

/// The source's own copy of the page, which outlives CEF's callback.
struct PageTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    format: wgpu::TextureFormat,
    size: (u32, u32),
}

/// What one paint cost, and the probe rows it produced.
pub struct Frame {
    /// `OpenSharedHandle` and wrapping the resource for wgpu.
    pub open_ms: f64,
    /// Copy out of CEF's texture into ours, submitted and waited for: what the callback has
    /// to finish before it returns the texture to CEF's pool.
    pub copy_ms: f64,
    /// Compositing our copy over the background and reading the probe rows back.
    pub composite_ms: f64,
    pub page_row: Vec<u8>,
    pub composite_row: Vec<u8>,
    /// What the shared texture's own D3D12 description says.
    pub texture: TextureDesc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureDesc {
    pub width: u64,
    pub height: u32,
    pub dxgi_format: i32,
    pub flags: i32,
}

impl TextureDesc {
    pub fn json(&self) -> String {
        Obj::new()
            .int("width", self.width)
            .int("height", self.height)
            .int("dxgi_format", self.dxgi_format)
            .str("flags", &format!("{:#x}", self.flags))
            .build()
    }
}

pub struct Compositor {
    page: Option<PageTexture>,
    out: wgpu::Texture,
    out_view: wgpu::TextureView,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    readback: wgpu::Buffer,
    pub gpu: Gpu,
}

impl Compositor {
    pub fn new(gpu: Gpu, (width, height): (u32, u32)) -> Self {
        let device = &gpu.device;
        let out = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let out_view = out.create_view(&wgpu::TextureViewDescriptor::default());
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("page"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let premultiplied_over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    blend: Some(wgpu::BlendState {
                        color: premultiplied_over,
                        alpha: premultiplied_over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let readback = readback_buffer(device);
        Self {
            page: None,
            out,
            out_view,
            layout,
            pipeline,
            readback,
            gpu,
        }
    }

    /// Handles one `OnAcceleratedPaint`. Runs inside the callback, so everything that reads
    /// CEF's texture has finished on the GPU before this returns.
    pub fn paint(&mut self, handle: *mut c_void) -> Result<Frame> {
        if handle.is_null() {
            bail!("CEF passed a null shared texture handle");
        }
        let start = Instant::now();
        let mut opened: Option<ID3D12Resource> = None;
        // SAFETY: CEF guarantees the handle is a live NT handle to a shared texture for the
        // duration of the callback we are inside; `opened` is a valid out-pointer. D3D12
        // takes its own reference to the resource; the handle stays CEF's to close.
        unsafe { self.gpu.d3d12.OpenSharedHandle(HANDLE(handle), &mut opened) }
            .context("ID3D12Device::OpenSharedHandle on CEF's texture")?;
        let resource = opened.context("OpenSharedHandle returned no resource")?;
        // SAFETY: plain method call on a live COM object.
        let desc = unsafe { resource.GetDesc() };
        let texture = TextureDesc {
            width: desc.Width,
            height: desc.Height,
            dxgi_format: desc.Format.0,
            flags: desc.Flags.0,
        };
        let format = match desc.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
            // CEF documents BGRA on Windows, and the probes decode BGRA rows.
            other => bail!("CEF texture has DXGI format {}, expected BGRA8", other.0),
        };
        let size = (
            u32::try_from(desc.Width).context("texture width")?,
            desc.Height,
        );
        if desc.MipLevels != 1 || desc.DepthOrArraySize != 1 || desc.SampleDesc.Count != 1 {
            bail!(
                "CEF texture has {} mips, {} layers, {} samples; expected 1 each",
                desc.MipLevels,
                desc.DepthOrArraySize,
                desc.SampleDesc.Count
            );
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        // SAFETY: `resource` is a live single-mip, single-sample 2D texture of exactly this
        // format and size (from its own description above).
        let hal_texture = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                format,
                wgpu::TextureDimension::D2,
                extent,
                1,
                1,
            )
        };
        // SAFETY: the resource was opened on this wgpu device's own ID3D12Device and matches
        // the descriptor. A shared texture starts in COMMON, which wgpu calls `PRESENT` on
        // D3D12; it is only read, and transitioned back to COMMON before the submit ends.
        let cef_texture = unsafe {
            self.gpu.device.create_texture_from_hal::<Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("cef shared texture"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::TextureUses::PRESENT,
            )
        };
        let opened_at = Instant::now();

        self.ensure_page_texture(format, size);
        let page = self.page.as_ref().context("no page texture")?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("copy"),
            });
        encoder.copy_texture_to_texture(
            cef_texture.as_image_copy(),
            page.texture.as_image_copy(),
            extent,
        );
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &cef_texture,
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        let index = self.gpu.queue.submit([encoder.finish()]);
        let waited = self.gpu.wait(index);
        drop(cef_texture);
        waited?;
        self.gpu.check()?;
        let copied_at = Instant::now();

        // A recorded wgpu error explains a failed composite, and must not be left behind to
        // fail the next paint too.
        let composited = self.composite();
        self.gpu.check()?;
        let (page_row, composite_row) = composited?;
        Ok(Frame {
            open_ms: ms(opened_at - start),
            copy_ms: ms(copied_at - opened_at),
            composite_ms: ms(copied_at.elapsed()),
            page_row,
            composite_row,
            texture,
        })
    }

    /// Our copy of the page, (re)made when CEF's size or format changes.
    fn ensure_page_texture(&mut self, format: wgpu::TextureFormat, size: (u32, u32)) {
        let stale = self
            .page
            .as_ref()
            .is_none_or(|p| p.format != format || p.size != size);
        if stale {
            let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("page"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("page"),
                    layout: &self.layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    }],
                });
            self.page = Some(PageTexture {
                texture,
                bind_group,
                format,
                size,
            });
        }
    }

    /// Blends our copy over the background and reads back the probe row of both.
    fn composite(&mut self) -> Result<(Vec<u8>, Vec<u8>)> {
        let page = self.page.as_ref().context("no page texture yet")?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite"),
            });
        {
            let bg = |c: u8| f64::from(c) / 255.0;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.out_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg(BACKGROUND.r),
                            g: bg(BACKGROUND.g),
                            b: bg(BACKGROUND.b),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &page.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        let row_fits = |(w, h): (u32, u32)| w >= ROW_PIXELS && h > PROBE_Y;
        let out_size = (self.out.width(), self.out.height());
        for (texture, size, offset) in [
            (&page.texture, page.size, 0),
            (&self.out, out_size, ROW_BYTES),
        ] {
            if !row_fits(size) {
                continue;
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: PROBE_Y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(ROW_BYTES as u32),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: ROW_PIXELS,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        let index = self.gpu.queue.submit([encoder.finish()]);
        let slice = self.readback.slice(..);
        let mapped = Arc::new(AtomicBool::new(false));
        let flag = mapped.clone();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            flag.store(r.is_ok(), Ordering::Release)
        });
        let waited = self.gpu.wait(index);
        if waited.is_err() || !mapped.load(Ordering::Acquire) {
            // The map may still be pending, which would make the next copy into this
            // buffer invalid; start over with a fresh one.
            self.readback = readback_buffer(&self.gpu.device);
            waited?;
            bail!("probe readback did not map");
        }
        let rows = {
            let data = slice
                .get_mapped_range()
                .map_err(|e| crate::err::Error::msg(format!("probe readback: {e:?}")))?;
            let (page_row, composite_row) = data.split_at(ROW_BYTES as usize);
            (page_row.to_vec(), composite_row.to_vec())
        };
        self.readback.unmap();
        Ok(rows)
    }
}

fn readback_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe rows"),
        size: ROW_BYTES * 2,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The DXGI adapters, for the report.
pub fn adapters_json() -> String {
    let Ok(adapters) = dxgi_adapters() else {
        return "null".into();
    };
    json::array(
        adapters
            .iter()
            .filter_map(|a| adapter_facts(a).ok())
            .map(|f| f.json()),
    )
}
