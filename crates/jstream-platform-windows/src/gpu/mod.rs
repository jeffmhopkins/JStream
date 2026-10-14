//! GPU interop on Windows (`jstream-platform` README › GPU frames, ARCH › GPU pipeline):
//! the engine's wgpu device on the media adapter, a D3D11 device beside it, NV12 encode
//! targets shared between them, and the shared fence that orders their use. Ported from
//! spike S1, which proved each piece on real hardware.
//!
//! Fence values: every [`FrameSync::stage`] (and, once the encoders and virtual camera
//! take GPU frames, every D3D11-side release) takes the next value of one counter per
//! device, so values only increase. A shared texture remembers the value that last
//! released it to the GPU (`released`) and the value the GPU's last write signals
//! (`written`).
//!
//! Not yet: the D3D11 consumers. Capture and the encoders need the context's D3D11 device,
//! and how they get it is a trait-shape decision (see the crate README's open questions).

mod d3d;

use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrameInfo};
use jstream_platform::{AdapterId, FrameSync, GpuContext, GpuFrame, GpuInterop, PlatformError};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE};
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIAdapter1};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::core::Interface;

use self::d3d::{D3D11, SharedFence, SharedTexture};
use crate::com::OsResult;

/// GPU interop through DX12 (wgpu) and D3D11 (capture and Media Foundation).
#[derive(Debug, Default)]
pub struct WindowsGpu {
    _private: (),
}

impl WindowsGpu {
    /// The backend. Opening nothing yet, it can't fail.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The platform's state for one [`GpuContext`]: the raw objects taken from wgpu's device
/// and the D3D11 device beside it.
///
/// Fields drop in order: every raw object goes before `device`, a clone of the wgpu
/// device that keeps wgpu-hal's `d3d12.dll` loaded until they are released (README › GPU
/// frames, driver lifetime).
pub(crate) struct Shared {
    fence: SharedFence,
    d3d11: D3D11,
    d3d12: ID3D12Device,
    /// The last fence value handed out.
    last_value: AtomicU64,
    /// Held, not used: keeps the driver loaded until the fields above are released.
    _device: wgpu::Device,
}

// SAFETY: the D3D12 device and fence are free-threaded. The D3D11 device is created
// multithread-protected (`d3d::create_d3d11`), which serialises its immediate context's
// calls, and its fence is free-threaded too. The counter is atomic.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Shared {
    /// The next fence value. Values only increase, from 1.
    fn next_value(&self) -> u64 {
        self.last_value.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Runs a future that is already complete (wgpu's native adapter and device requests
/// resolve on their first poll). `None` if it isn't.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

fn not_ready(what: &str) -> PlatformError {
    PlatformError::Os {
        call: "wgpu",
        code: None,
        message: format!("{what} didn't complete at once on the native backend"),
    }
}

/// The DXGI adapter behind a wgpu adapter.
fn dxgi_adapter(adapter: &wgpu::Adapter) -> Option<IDXGIAdapter> {
    // SAFETY: only the raw DXGI adapter is cloned (an AddRef); wgpu keeps its own
    // reference and the adapter is never destroyed here.
    let hal = unsafe { adapter.as_hal::<Dx12>() }?;
    hal.as_raw().cast().ok()
}

/// The adapter's LUID and PCI vendor ID.
fn adapter_id(dxgi: &IDXGIAdapter) -> Result<AdapterId, PlatformError> {
    let a1: IDXGIAdapter1 = dxgi.cast().os("IDXGIAdapter1")?;
    // SAFETY: plain COM call on a live object.
    let desc = unsafe { a1.GetDesc1() }.os("IDXGIAdapter1::GetDesc1")?;
    let luid = desc.AdapterLuid;
    Ok(AdapterId {
        luid: Some((u64::from(luid.HighPart.cast_unsigned()) << 32) | u64::from(luid.LowPart)),
        vendor: desc.VendorId,
    })
}

impl GpuInterop for WindowsGpu {
    /// Opens the DX12 device on the media adapter: the one that drives the primary
    /// display (so capture needs no copy across adapters), else the first hardware
    /// adapter. It must support NV12 textures.
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters = ready(instance.enumerate_adapters(wgpu::Backends::DX12))
            .ok_or_else(|| not_ready("enumerate_adapters"))?;
        // SAFETY: no pointers; (0, 0) is always on the primary monitor.
        let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let candidates: Vec<wgpu::Adapter> = adapters
            .into_iter()
            .filter(|a| a.get_info().device_type != wgpu::DeviceType::Cpu)
            .filter(|a| a.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12))
            .collect();
        let index = candidates
            .iter()
            .position(|a| dxgi_adapter(a).is_some_and(|d| d3d::adapter_drives_monitor(&d, primary)))
            .unwrap_or(0);
        let adapter = candidates
            .into_iter()
            .nth(index)
            .ok_or(PlatformError::Unsupported {
                what: "GPU interop: no DX12 hardware adapter with NV12 textures".into(),
            })?;
        let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jstream"),
            required_features: wgpu::Features::TEXTURE_FORMAT_NV12,
            ..Default::default()
        }))
        .ok_or_else(|| not_ready("request_device"))?
        .map_err(|e| PlatformError::Os {
            call: "wgpu::Adapter::request_device",
            code: None,
            message: e.to_string(),
        })?;
        let dxgi = dxgi_adapter(&adapter).ok_or(PlatformError::Os {
            call: "wgpu::Adapter::as_hal",
            code: None,
            message: "the adapter is not DX12".into(),
        })?;
        let id = adapter_id(&dxgi)?;
        // SAFETY: only the raw ID3D12Device is cloned (an AddRef); wgpu keeps ownership of
        // its own reference. `Shared` releases the clone before its wgpu device clone.
        let d3d12 = unsafe { device.as_hal::<Dx12>() }
            .map(|d| d.raw_device().clone())
            .ok_or(PlatformError::Os {
                call: "wgpu::Device::as_hal",
                code: None,
                message: "the device is not DX12".into(),
            })?;
        let d3d11 = d3d::create_d3d11(&dxgi)?;
        let fence = SharedFence::new(&d3d11.device, &d3d12)?;
        let shared = Arc::new(Shared {
            fence,
            d3d11,
            d3d12,
            last_value: AtomicU64::new(0),
            _device: device.clone(),
        });
        Ok(GpuContext::new(
            adapter,
            device,
            queue,
            id,
            Some(Box::new(shared)),
        ))
    }

    fn encode_target(
        &self,
        context: &GpuContext,
        size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        let shared = context
            .platform::<Arc<Shared>>()
            .ok_or(PlatformError::InvalidState {
                what: "encode_target",
                state: "the context was not opened by the Windows GPU interop",
            })?;
        let (w, h) = (size.width(), size.height());
        if w % 2 != 0 || h % 2 != 0 {
            return Err(PlatformError::Unsupported {
                what: format!("an NV12 encode target at an odd size, {w}x{h}"),
            });
        }
        let texture = SharedTexture::new(
            &shared.d3d11.device,
            &shared.d3d12,
            (w, h),
            DXGI_FORMAT_NV12,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        )?;
        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        // SAFETY: the resource is a live 2D NV12 texture with one mip and one sample, of
        // exactly this size, created on the D3D11 side and opened on this device's own
        // ID3D12Device.
        let hal_texture = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                texture.d3d12.clone(),
                wgpu::TextureFormat::NV12,
                wgpu::TextureDimension::D2,
                extent,
                1,
                1,
            )
        };
        // SAFETY: the hal texture matches the descriptor below, belongs to this device,
        // and is in COMMON state, which wgpu calls `PRESENT` on D3D12. Its contents are
        // undefined until the engine writes them, which it does before any read.
        let wgpu_texture = unsafe {
            context.device().create_texture_from_hal::<Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("jstream encode target (NV12, shared)"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::NV12,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::TextureUses::PRESENT,
            )
        };
        let info = VideoFrameInfo {
            size,
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::zero(crate::clock::timebase()),
        };
        let sync = Arc::new(SlotSync {
            _texture: texture,
            shared: Arc::clone(shared),
            released: AtomicU64::new(0),
            written: AtomicU64::new(0),
        });
        Ok(GpuFrame::new(wgpu_texture, info, sync))
    }
}

/// The fence bookkeeping for one shared texture. The GPU side ([`FrameSync::stage`])
/// waits for `released` and signals a new `written`. The D3D11 side (the encoders and
/// virtual camera, when they take GPU frames) will wait for `written` and signal a new
/// `released`.
pub(crate) struct SlotSync {
    // The texture's raw objects drop before `shared`, which holds the wgpu device clone.
    // Held so the D3D11 side can read the texture, and so the share stays open.
    _texture: SharedTexture,
    shared: Arc<Shared>,
    /// The fence value after which the GPU may write the texture again.
    released: AtomicU64,
    /// The fence value the GPU's last staged use signals.
    written: AtomicU64,
}

// SAFETY: the texture's D3D11 and D3D12 objects are free-threaded resources (no context
// state), and the rest is `Shared` (Send + Sync) and atomics.
unsafe impl Send for SlotSync {}
// SAFETY: as above.
unsafe impl Sync for SlotSync {}

impl fmt::Debug for SlotSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotSync")
            .field("released", &self.released.load(Ordering::Relaxed))
            .field("written", &self.written.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FrameSync for SlotSync {
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        let wait = self.released.load(Ordering::Acquire);
        let signal = self.shared.next_value();
        // SAFETY: the hal queue is used only to stage fence operations that wgpu applies
        // around its next submit; the raw queue is not touched.
        let hal_queue = unsafe { queue.as_hal::<Dx12>() }.ok_or(PlatformError::InvalidState {
            what: "FrameSync::stage",
            state: "the queue is not a DX12 queue",
        })?;
        hal_queue.add_wait_fence(self.shared.fence.d3d12.clone(), wait);
        hal_queue.add_signal_fence(self.shared.fence.d3d12.clone(), signal);
        self.written.store(signal, Ordering::Release);
        Ok(())
    }
}

/// Platform tests on a real GPU (R-TEST-004): behind `hw-tests` and `#[ignore]`d, because
/// CI machines have no GPU. Run them on a Windows machine with a DX12 GPU:
/// `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
#[cfg(all(test, feature = "hw-tests"))]
mod device_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use jstream_platform::GpuInterop;
    use jstream_platform::media::FrameSize;

    use super::{Shared, WindowsGpu};

    #[test]
    #[ignore = "needs a DX12 GPU with NV12 textures"]
    fn renders_into_a_shared_encode_target_and_signals_the_fence() {
        let gpu = WindowsGpu::new();
        let ctx = gpu.open_device().expect("open the media adapter's device");
        assert!(ctx.adapter_id().luid.is_some());
        let shared = ctx.platform::<Arc<Shared>>().expect("our context").clone();
        let frame = gpu
            .encode_target(&ctx, FrameSize::new(64, 32).expect("size"))
            .expect("encode target");
        assert_eq!(frame.info().size.width(), 64);
        let device = ctx.device();
        let plane = |aspect, format| {
            frame.texture().create_view(&wgpu::TextureViewDescriptor {
                label: Some("nv12 plane"),
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let y = plane(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm);
        let uv = plane(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm);
        // Two uses of the same target in a row: the second waits for nothing the D3D11
        // side has to release (it hasn't read yet), and both must reach the fence.
        for round in 0..2 {
            let mut enc = device.create_command_encoder(&Default::default());
            for view in [&y, &uv] {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("clear nv12 plane"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.5,
                                g: 0.5,
                                b: 0.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            // Back to COMMON for D3D11, before the staged signal (GpuFrame::stage).
            enc.transition_resources(
                std::iter::empty(),
                std::iter::once(wgpu::TextureTransition {
                    texture: frame.texture(),
                    selector: None,
                    state: wgpu::TextureUses::PRESENT,
                }),
            );
            frame.stage(ctx.queue()).expect("stage");
            ctx.queue().submit([enc.finish()]);
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(5)),
                })
                .expect("the submit completes");
            let sync = frame.sync();
            let written = format!("{sync:?}");
            // SAFETY: plain COM call on the live shared fence.
            let completed = unsafe { shared.fence.d3d12.GetCompletedValue() };
            assert!(
                completed >= shared.last_value.load(Ordering::Relaxed),
                "round {round}: fence at {completed}, {written}"
            );
        }
    }
}
