//! The Windows.Graphics.Capture backend. On the CPU path each frame is copied into a
//! staging texture, read back, converted to NV12 and published to the stream's mailbox.
//! On the GPU path each frame is copied into a shared texture of the stream's ring
//! (`gpu::CaptureRing`) and published as a `GpuFrame`.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use jstream_platform::media::{
    ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, GpuContext,
    PlatformError, VideoSurface,
};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0,
    D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1,
};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{IInspectable, Interface, Ref};

use super::convert::{bgra_to_nv12, even_size};
use super::id::{monitor_id, parse_monitor, parse_window, window_id};
use super::mailbox::Mailbox;
use super::targets;
use crate::clock::timebase;
use crate::com::{MtaUsage, OsResult};
use crate::gpu::{CaptureGpu, CaptureRing, adapter_drives_monitor};

/// The frame pool's format: 8-bit BGRA, which the converter reads.
const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// Frame pool buffers: one being converted, one for WGC to render the next frame into.
const BUFFERS: i32 = 2;

/// Display and window capture through Windows.Graphics.Capture.
///
/// Made with [`new`](Self::new), it delivers `VideoSurface::Cpu` NV12 frames in BT.709
/// limited range, and each capture has its own D3D11 device, on the adapter that drives
/// the captured display. Made with [`on_gpu`](Self::on_gpu), it delivers
/// `VideoSurface::Gpu` frames on that context's device instead. Either way `pts` is on
/// the [`QpcClock`](crate::QpcClock) time line.
///
/// It is `Send + Sync`. Each capture keeps the process's multithreaded apartment alive,
/// so it can be started, read and dropped from threads that never initialised COM.
#[derive(Debug, Default)]
pub struct WgcCapture {
    /// The context's GPU state, on the GPU path.
    gpu: Option<CaptureGpu>,
}

impl WgcCapture {
    /// The backend on the CPU path. Opening nothing yet, it can't fail.
    pub fn new() -> Self {
        Self::default()
    }

    /// The backend on the GPU path, for `context`, which [`WindowsGpu`](crate::WindowsGpu)
    /// opened. Every capture uses the context's D3D11 device, so its frames are on the
    /// context's adapter, and delivers each frame as a `GpuFrame`: a BGRA texture shared
    /// with the context's wgpu device (`Bgra8UnormSrgb`, `TEXTURE_BINDING | COPY_SRC`),
    /// in sRGB (BT.709 primaries, full range), opaque. Render stages the frame
    /// ([`GpuFrame::stage`](jstream_platform::GpuFrame::stage)) on each submit that
    /// reads it.
    ///
    /// The backend keeps the context's device alive, not the context itself.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another backend opened.
    pub fn on_gpu(context: &GpuContext) -> Result<Self, PlatformError> {
        Ok(Self {
            gpu: Some(CaptureGpu::of(context)?),
        })
    }
}

impl Capture for WgcCapture {
    /// The displays, then the windows. A display's name is its GDI device name without
    /// the `\\.\` prefix, such as `DISPLAY1`; a window's is its title.
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        let displays = targets::monitors()?.into_iter().map(|m| CaptureTargetInfo {
            target: CaptureTarget::Display(monitor_id(&m.device)),
            name: m.device.trim_start_matches(r"\\.\").to_owned(),
            size: m.size,
            primary: m.primary,
        });
        let windows = targets::top_level_windows()?
            .into_iter()
            .map(|w| CaptureTargetInfo {
                target: CaptureTarget::Window(window_id(w.hwnd.0.addr())),
                name: w.title,
                size: w.size,
                primary: false,
            });
        Ok(displays.chain(windows).collect())
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.start_with(target, self.gpu.as_ref())
    }

    /// The GPU path on `context`, whichever path this backend was made for.
    fn start_on(
        &self,
        target: &CaptureTarget,
        context: &GpuContext,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.start_with(target, Some(&CaptureGpu::of(context)?))
    }
}

impl WgcCapture {
    fn start_with(
        &self,
        target: &CaptureTarget,
        gpu: Option<&CaptureGpu>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        hold_mta_for_the_process()?;
        let mta = MtaUsage::new()?;
        let resolved = resolve(target)?;
        if !GraphicsCaptureSession::IsSupported().os("GraphicsCaptureSession::IsSupported")? {
            return Err(PlatformError::Unsupported {
                what: "Windows.Graphics.Capture on this system".into(),
            });
        }
        Ok(Box::new(WgcStream::start(resolved, gpu, mta)?))
    }
}

/// Keeps the process's multithreaded apartment until the process exits, from the first
/// capture on.
///
/// The `windows` crate caches agile WinRT activation factories (here the capture
/// session's and frame pool's statics) for the whole process and never releases them.
/// When the last MTA usage goes, COM uninitialises and may unload the DLLs behind them,
/// so the next capture called through a dangling factory and crashed with an access
/// violation (seen on hardware: a second capture in one process). Holding one usage for
/// the process keeps those factories valid. It is a process-lifetime OS hold, set once
/// and never changed, not application state (R-RS-008).
fn hold_mta_for_the_process() -> Result<(), PlatformError> {
    static HOLD: OnceLock<MtaUsage> = OnceLock::new();
    if HOLD.get().is_none() {
        // A thread that loses the race drops its usage at once, which balances it.
        let _ = HOLD.set(MtaUsage::new()?);
    }
    Ok(())
}

/// What a capture item is created for.
enum Source {
    Monitor(HMONITOR),
    Window(HWND),
}

/// A target resolved to its OS handle.
struct Resolved {
    source: Source,
    /// The display it is on, to pick the adapter.
    monitor: HMONITOR,
    /// Names the target in a `DeviceLost` error.
    what: String,
    /// The kind and ID for a `NotFound` error, when the target goes before the capture
    /// starts.
    kind: &'static str,
    id: String,
}

fn resolve(target: &CaptureTarget) -> Result<Resolved, PlatformError> {
    match target {
        CaptureTarget::Display(id) => {
            let not_found = || PlatformError::NotFound {
                kind: "display",
                id: id.to_string(),
            };
            let name = parse_monitor(id).ok_or_else(not_found)?;
            let monitor = targets::monitors()?
                .into_iter()
                .find(|m| m.device == name)
                .ok_or_else(not_found)?;
            Ok(Resolved {
                source: Source::Monitor(monitor.handle),
                monitor: monitor.handle,
                what: format!("display {id}"),
                kind: "display",
                id: id.to_string(),
            })
        }
        CaptureTarget::Window(id) => {
            // A handle, never dereferenced: it needs no provenance.
            let hwnd = parse_window(id)
                .map(|h| HWND(std::ptr::without_provenance_mut::<c_void>(h)))
                // SAFETY: IsWindow accepts any value and says whether it names a window.
                .filter(|&h| unsafe { IsWindow(Some(h)) }.as_bool())
                .ok_or_else(|| PlatformError::NotFound {
                    kind: "window",
                    id: id.to_string(),
                })?;
            // SAFETY: plain call; a window destroyed meanwhile gives the nearest monitor
            // or none, and either only picks the adapter.
            let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            Ok(Resolved {
                source: Source::Window(hwnd),
                monitor,
                what: format!("window {id}"),
                kind: "window",
                id: id.to_string(),
            })
        }
        other => Err(PlatformError::Unsupported {
            what: format!("capture target {other:?}"),
        }),
    }
}

/// Creates the capture item. A target that has gone since `resolve` checked it (a window
/// closed, a display unplugged, in between) is `NotFound`, as `Capture::start` promises,
/// rather than the OS error.
fn create_item(resolved: &Resolved) -> Result<GraphicsCaptureItem, PlatformError> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .os("GraphicsCaptureItem activation factory")?;
    create_item_for(&interop, &resolved.source).map_err(|e| not_found_if_gone(e, resolved))
}

/// `error`, or `NotFound` for `resolved` if its target no longer exists.
fn not_found_if_gone(error: PlatformError, resolved: &Resolved) -> PlatformError {
    if target_gone(&resolved.source) {
        PlatformError::NotFound {
            kind: resolved.kind,
            id: resolved.id.clone(),
        }
    } else {
        error
    }
}

/// Whether the target `source` names no longer exists. A window handle that Windows has
/// already reused for a new window counts as still there, so that rare case keeps the OS
/// error.
fn target_gone(source: &Source) -> bool {
    match *source {
        Source::Monitor(m) => targets::monitors()
            .map(|monitors| monitors.iter().all(|monitor| monitor.handle != m))
            .unwrap_or(false),
        // SAFETY: IsWindow accepts any value and says whether it names a window.
        Source::Window(h) => !unsafe { IsWindow(Some(h)) }.as_bool(),
    }
}

fn create_item_for(
    interop: &IGraphicsCaptureItemInterop,
    source: &Source,
) -> Result<GraphicsCaptureItem, PlatformError> {
    match *source {
        Source::Monitor(m) => {
            // SAFETY: plain COM call; a monitor that has gone since it was listed fails it.
            let item = unsafe { interop.CreateForMonitor(m) };
            item.os("IGraphicsCaptureItemInterop::CreateForMonitor")
        }
        Source::Window(h) => {
            // SAFETY: plain COM call; a window destroyed since it was checked fails it.
            let item = unsafe { interop.CreateForWindow(h) };
            item.os("IGraphicsCaptureItemInterop::CreateForWindow")
        }
    }
}

/// A capture's own D3D11 device, and the same device as WinRT sees it.
struct Device {
    d3d: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

/// The hardware adapter whose outputs include `monitor`, if any.
fn adapter_for(monitor: HMONITOR) -> Option<IDXGIAdapter> {
    // SAFETY: plain call.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        // SAFETY: plain COM call; an error (DXGI_ERROR_NOT_FOUND) ends the enumeration.
        .map_while(|i| unsafe { factory.EnumAdapters1(i) }.ok())
        .filter_map(|a| a.cast::<IDXGIAdapter>().ok())
        .find(|a| adapter_drives_monitor(a, monitor))
}

/// Creates a multithread-protected D3D11 device on the adapter that drives `monitor`,
/// else on the default hardware adapter. Capture on the display's own adapter needs no
/// copy across adapters.
fn create_device(monitor: HMONITOR) -> Result<Device, PlatformError> {
    let adapter = adapter_for(monitor);
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: the adapter, if any, is a live COM object; the out-pointers point to valid
    // locals.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_10_0,
            ]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .os("D3D11CreateDevice")?;
    let none = |what: &str| PlatformError::Os {
        call: "D3D11CreateDevice",
        code: None,
        message: format!("returned no {what}"),
    };
    let d3d = device.ok_or_else(|| none("device"))?;
    let context = context.ok_or_else(|| none("context"))?;
    // WGC uses the device on its own threads while the frame handler uses the immediate
    // context, so the device serialises its calls.
    let mt: ID3D11Multithread = context.cast().os("ID3D11Multithread")?;
    // SAFETY: plain COM call on a live object.
    let _ = unsafe { mt.SetMultithreadProtected(true) };
    wrap_device(d3d, context)
}

/// The GPU context's D3D11 device (already multithread-protected), for the GPU path.
fn context_device(gpu: &CaptureGpu) -> Result<Device, PlatformError> {
    let d3d = gpu.d3d11()?;
    // SAFETY: plain COM call on a live object.
    let context = unsafe { d3d.GetImmediateContext() }.os("ID3D11Device::GetImmediateContext")?;
    wrap_device(d3d, context)
}

/// Bundles a multithread-protected D3D11 device with its WinRT wrapper.
fn wrap_device(d3d: ID3D11Device, context: ID3D11DeviceContext) -> Result<Device, PlatformError> {
    let dxgi: IDXGIDevice = d3d.cast().os("IDXGIDevice")?;
    // SAFETY: `dxgi` is a live DXGI device; the WinRT wrapper takes its own reference.
    let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .os("CreateDirect3D11DeviceFromDXGIDevice")?
        .cast()
        .os("IDirect3DDevice")?;
    Ok(Device {
        d3d,
        context,
        winrt,
    })
}

/// What the frame handler and the stream share.
struct Shared {
    /// The newest converted frame.
    mailbox: Mailbox<VideoSurface>,
    /// Set once, by the first failure or by the item's `Closed` event. After it is set
    /// the handler does nothing and the stream returns it.
    failure: OnceLock<PlatformError>,
    captured: AtomicU64,
    /// Frames replaced in the mailbox, plus, on the GPU path, frames dropped because
    /// every slot of the ring was held.
    skipped: AtomicU64,
}

impl Shared {
    fn fail(&self, e: PlatformError) {
        // Only the first failure is kept.
        let _ = self.failure.set(e);
    }
}

/// A staging texture the GPU copies a frame into, for the CPU to read.
struct Staging {
    texture: ID3D11Texture2D,
    size: (u32, u32),
}

/// The frame handler's state: the device, and what the path needs.
struct Converter {
    device: Device,
    /// The size the pool's buffers were last created with.
    pool_size: SizeInt32,
    path: Path,
}

/// Where frames go.
enum Path {
    /// Read back and converted to NV12.
    Cpu {
        staging: Option<Staging>,
        /// A frame the reader never took, reused for the next one of the same size, so a
        /// reader that falls behind doesn't reallocate the frame's planes. (The mailbox
        /// still boxes each frame it is given.)
        spare: Option<VideoFrame>,
    },
    /// Copied into shared textures.
    Gpu(CaptureRing),
}

// SAFETY: the D3D11 device is multithread-protected and the WinRT device is agile; the
// ring is `Send`. The converter lives in a `Mutex`, so one thread at a time uses the
// immediate context and the textures.
unsafe impl Send for Converter {}

impl Converter {
    /// Converts the frame for the stream's path. `Ok(None)` when there is nothing to
    /// publish: the content is empty (a minimised window), smaller than 2x2 on the CPU
    /// path, or, on the GPU path, every slot of the ring is held.
    fn convert(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        content: SizeInt32,
    ) -> Result<Option<VideoSurface>, PlatformError> {
        let Path::Gpu(ring) = &mut self.path else {
            return Ok(self.convert_cpu(frame, content)?.map(VideoSurface::Cpu));
        };
        let (texture, time) = frame_texture(frame)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a valid out-pointer.
        unsafe { texture.GetDesc(&mut desc) };
        // Clipped to the buffer, as on the CPU path; any size of at least 1x1.
        let clip = |content: i32, buffer: u32| u32::try_from(content).unwrap_or(0).min(buffer);
        let Ok(size) = FrameSize::new(
            clip(content.Width, desc.Width),
            clip(content.Height, desc.Height),
        ) else {
            return Ok(None);
        };
        // SystemRelativeTime is QPC in 100 ns units, the clock's own time line and
        // unit.
        let pts = MediaTime::new(time, timebase());
        Ok(ring.copy(&texture, size, pts)?.map(VideoSurface::Gpu))
    }

    /// Copies the frame's content into a staging texture and converts it to NV12.
    /// `Ok(None)` when the content is smaller than 2x2 (a minimised window).
    fn convert_cpu(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        content: SizeInt32,
    ) -> Result<Option<VideoFrame>, PlatformError> {
        let (texture, time) = frame_texture(frame)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a valid out-pointer.
        unsafe { texture.GetDesc(&mut desc) };
        // The content can be smaller than the buffer (the window shrank before the pool
        // was recreated) or larger (it grew; the excess is cut off until then). An odd
        // size loses its last column or row.
        let clip = |content: i32, buffer: u32| u32::try_from(content).unwrap_or(0).min(buffer);
        let Some((w, h)) = even_size(
            clip(content.Width, desc.Width),
            clip(content.Height, desc.Height),
        ) else {
            return Ok(None);
        };
        let staging = self.staging(w, h)?;
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: w,
            bottom: h,
            back: 1,
        };
        // SAFETY: both textures are live, on this device and in the same format, and the
        // box lies within both (w and h are clipped to the source and are the staging
        // texture's size).
        unsafe {
            self.device.context.CopySubresourceRegion(
                &staging,
                0,
                0,
                0,
                0,
                &texture,
                0,
                Some(&region),
            );
        }
        let mut out = self.output(w, h)?;
        // SystemRelativeTime is QPC in 100 ns units, the clock's own time line and unit.
        out.set_pts(MediaTime::new(time, timebase()));
        let mapped = Mapped::new(&self.device.context, &staging, (w, h))?;
        bgra_to_nv12(mapped.bytes(), mapped.pitch(), &mut out)?;
        Ok(Some(out))
    }

    /// The staging texture, recreated when the size changes.
    fn staging(&mut self, w: u32, h: u32) -> Result<ID3D11Texture2D, PlatformError> {
        let Path::Cpu { staging, .. } = &mut self.path else {
            return Err(gpu_path_mixup());
        };
        if let Some(s) = staging
            && s.size == (w, h)
        {
            return Ok(s.texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialised and `texture` is a valid out-pointer.
        unsafe {
            self.device
                .d3d
                .CreateTexture2D(&desc, None, Some(&mut texture))
        }
        .os("ID3D11Device::CreateTexture2D (staging)")?;
        let texture = texture.ok_or(PlatformError::Os {
            call: "ID3D11Device::CreateTexture2D",
            code: None,
            message: "returned no texture".into(),
        })?;
        *staging = Some(Staging {
            texture: texture.clone(),
            size: (w, h),
        });
        Ok(texture)
    }

    /// The frame to convert into: the spare one if it has this size, else a new one.
    fn output(&mut self, w: u32, h: u32) -> Result<VideoFrame, PlatformError> {
        let Path::Cpu { spare, .. } = &mut self.path else {
            return Err(gpu_path_mixup());
        };
        let size = FrameSize::new(w, h)?;
        match spare.take() {
            Some(f) if f.info().size == size => Ok(f),
            _ => Ok(VideoFrame::zeroed(VideoFrameInfo {
                size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: MediaTime::zero(timebase()),
            })?),
        }
    }
}

/// A CPU-path helper called on the GPU path: a bug, reported rather than panicking on a
/// WinRT thread.
fn gpu_path_mixup() -> PlatformError {
    PlatformError::InvalidState {
        what: "WGC capture",
        state: "a CPU-path step ran on the GPU path",
    }
}

/// The frame's BGRA texture and its `SystemRelativeTime` (100 ns QPC ticks). The pool's
/// format is BGRA, so another format is `Unsupported`.
fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<(ID3D11Texture2D, i64), PlatformError> {
    let time = frame
        .SystemRelativeTime()
        .os("Direct3D11CaptureFrame::SystemRelativeTime")?;
    let access: IDirect3DDxgiInterfaceAccess = frame
        .Surface()
        .os("Direct3D11CaptureFrame::Surface")?
        .cast()
        .os("IDirect3DDxgiInterfaceAccess")?;
    // SAFETY: plain COM call on a live object.
    let texture: ID3D11Texture2D =
        unsafe { access.GetInterface() }.os("IDirect3DDxgiInterfaceAccess::GetInterface")?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is a valid out-pointer.
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(PlatformError::Unsupported {
            what: format!("a WGC frame in DXGI format {}", desc.Format.0),
        });
    }
    Ok((texture, time.Duration))
}

/// A staging texture mapped for reading, unmapped on drop.
struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    data: D3D11_MAPPED_SUBRESOURCE,
    /// The texture's size in pixels.
    size: (u32, u32),
}

impl<'a> Mapped<'a> {
    /// Maps `texture`, a BGRA staging texture of `size` (both at least 1) with CPU read
    /// access. Waits for the GPU's copy into it.
    fn new(
        context: &'a ID3D11DeviceContext,
        texture: &'a ID3D11Texture2D,
        size: (u32, u32),
    ) -> Result<Self, PlatformError> {
        let mut data = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the texture is a live staging texture with CPU read access that is not
        // mapped yet; `data` is a valid out-pointer. Drop unmaps it.
        unsafe { context.Map(texture, 0, D3D11_MAP_READ, 0, Some(&mut data)) }
            .os("ID3D11DeviceContext::Map")?;
        let mapped = Self {
            context,
            texture,
            data,
            size,
        };
        if mapped.data.pData.is_null() || mapped.pitch() < size.0 as usize * 4 {
            return Err(PlatformError::Os {
                call: "ID3D11DeviceContext::Map",
                code: None,
                message: format!("mapped no memory, or rows of {} bytes", mapped.pitch()),
            });
        }
        Ok(mapped)
    }

    /// Bytes from the start of one row to the start of the next.
    fn pitch(&self) -> usize {
        self.data.RowPitch as usize
    }

    /// Every row; the last without its padding.
    fn bytes(&self) -> &[u8] {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let len = self.pitch() * (h - 1) + w * 4;
        // SAFETY: a mapped 2D subresource has `h` rows `RowPitch` bytes apart, each at
        // least `w * 4` bytes long (`new` checked the pitch), so `len` bytes from `pData`
        // are mapped and readable. They stay mapped until `self` drops, and the slice
        // borrows `self`.
        unsafe { std::slice::from_raw_parts(self.data.pData.cast::<u8>(), len) }
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: the texture was mapped by `new` and is unmapped exactly once; no slice
        // from `bytes` outlives `self`.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

/// Handles one `FrameArrived` event: takes the frame, converts it, recreates the pool
/// when the content size changed, and publishes the result.
fn on_frame(
    pool: &Direct3D11CaptureFramePool,
    shared: &Shared,
    converter: &Mutex<Option<Converter>>,
) -> Result<(), PlatformError> {
    if shared.failure.get().is_some() {
        return Ok(());
    }
    let mut guard = converter.lock().map_err(|_| PlatformError::InvalidState {
        what: "WGC capture",
        state: "the frame handler panicked",
    })?;
    // `None` once the stream has dropped: an event already on its way does nothing.
    let Some(conv) = guard.as_mut() else {
        return Ok(());
    };
    // An event can find the pool empty (an earlier event took its frame): not an error.
    let Ok(frame) = pool.TryGetNextFrame() else {
        return Ok(());
    };
    shared.captured.fetch_add(1, Ordering::Relaxed);
    let content = frame
        .ContentSize()
        .os("Direct3D11CaptureFrame::ContentSize")?;
    let converted = conv.convert(&frame, content);
    // Give the buffer back before the pool is recreated.
    let _ = frame.Close();
    drop(frame);
    if content != conv.pool_size && content.Width > 0 && content.Height > 0 {
        pool.Recreate(&conv.device.winrt, PIXEL_FORMAT, BUFFERS, content)
            .os("Direct3D11CaptureFramePool::Recreate")?;
        conv.pool_size = content;
    }
    match converted? {
        Some(out) => {
            if let Some(old) = shared.mailbox.put(out) {
                shared.skipped.fetch_add(1, Ordering::Relaxed);
                // A GPU frame just drops, which frees its slot.
                if let (VideoSurface::Cpu(old), Path::Cpu { spare, .. }) = (old, &mut conv.path) {
                    *spare = Some(old);
                }
            }
        }
        // Every slot of the ring is held: the frame is dropped.
        None if matches!(conv.path, Path::Gpu(_)) && content.Width > 0 && content.Height > 0 => {
            shared.skipped.fetch_add(1, Ordering::Relaxed);
        }
        None => {}
    }
    Ok(())
}

/// One running capture.
struct WgcStream {
    shared: Arc<Shared>,
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    item: GraphicsCaptureItem,
    frame_token: Option<i64>,
    closed_token: Option<i64>,
    /// The frame handler's state, shared with the handler, which WinRT may release
    /// later, on a thread of its own. Emptied when the stream drops.
    converter: Arc<Mutex<Option<Converter>>>,
    /// Declared last: the apartment outlives the WinRT objects above.
    _mta: MtaUsage,
}

impl WgcStream {
    fn start(
        resolved: Resolved,
        gpu: Option<&CaptureGpu>,
        mta: MtaUsage,
    ) -> Result<Self, PlatformError> {
        let item = create_item(&resolved)?;
        let (device, path) = match gpu {
            Some(gpu) => (context_device(gpu)?, Path::Gpu(gpu.ring()?)),
            None => (
                create_device(resolved.monitor)?,
                Path::Cpu {
                    staging: None,
                    spare: None,
                },
            ),
        };
        let size = item.Size().os("GraphicsCaptureItem::Size")?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device.winrt,
            PIXEL_FORMAT,
            BUFFERS,
            size,
        )
        .os("Direct3D11CaptureFramePool::CreateFreeThreaded")?;
        let session = pool
            .CreateCaptureSession(&item)
            .os("Direct3D11CaptureFramePool::CreateCaptureSession")?;
        // Best effort: older builds lack these properties (the border needs Windows 11,
        // the cursor setting Windows 10 2004), and they then keep their defaults.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            failure: OnceLock::new(),
            captured: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
        });
        let converter = Arc::new(Mutex::new(Some(Converter {
            device,
            pool_size: size,
            path,
        })));
        // From here on, dropping the stream unregisters and closes everything.
        let mut stream = Self {
            shared: Arc::clone(&shared),
            session,
            pool,
            item,
            frame_token: None,
            closed_token: None,
            converter: Arc::clone(&converter),
            _mta: mta,
        };
        let on_shared = Arc::clone(&shared);
        let arrived = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |pool: Ref<'_, Direct3D11CaptureFramePool>, _| {
                // Called by WinRT on a thread-pool thread (R-UNSAFE-005): nothing may
                // unwind out of it. A failure or a panic ends the capture.
                let result = catch_unwind(AssertUnwindSafe(|| match pool.as_ref() {
                    Some(pool) => on_frame(pool, &on_shared, &converter),
                    None => Ok(()),
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => on_shared.fail(e),
                    Err(_) => on_shared.fail(PlatformError::InvalidState {
                        what: "WGC capture",
                        state: "the frame handler panicked",
                    }),
                }
                Ok(())
            },
        );
        stream.frame_token = Some(
            stream
                .pool
                .FrameArrived(&arrived)
                .os("Direct3D11CaptureFramePool::FrameArrived")?,
        );
        let what = resolved.what;
        let closed = TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            // Called by WinRT (R-UNSAFE-005): nothing may unwind out of it.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared.fail(PlatformError::DeviceLost {
                    what: what.clone(),
                    reason: "the capture item was closed: the window closed or the display \
                             went away"
                        .into(),
                });
            }));
            Ok(())
        });
        stream.closed_token = Some(
            stream
                .item
                .Closed(&closed)
                .os("GraphicsCaptureItem::Closed")?,
        );
        stream
            .session
            .StartCapture()
            .os("GraphicsCaptureSession::StartCapture")?;
        Ok(stream)
    }
}

impl CaptureStream for WgcStream {
    /// Never blocks: a swap of the mailbox's pointer, and a read of the failure cell.
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if let Some(e) = self.shared.failure.get() {
            return Err(e.clone());
        }
        Ok(self.shared.mailbox.take())
    }

    fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.shared.captured.load(Ordering::Relaxed),
            frames_skipped: self.shared.skipped.load(Ordering::Relaxed),
            frames_undecodable: 0,
        }
    }
}

impl Drop for WgcStream {
    fn drop(&mut self) {
        // Errors at teardown change nothing.
        if let Some(t) = self.frame_token.take() {
            let _ = self.pool.RemoveFrameArrived(t);
        }
        if let Some(t) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(t);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
        // Release the handler's D3D11, WinRT and GPU objects here, on this thread, while
        // the apartment is still held, rather than whenever WinRT releases the handler.
        // Waits for a handler that is running to finish its frame. A panicked handler
        // poisoned the lock; its state is released all the same.
        let converter = match self.converter.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        drop(converter);
        // A frame no one took holds a slot's texture and the GPU context's device.
        drop(self.shared.mailbox.take());
    }
}

/// Tests that run on every Windows CI machine: they need no display or capture device.
#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    };
    use windows::core::w;

    use super::{Resolved, Source, not_found_if_gone, target_gone};

    /// A message-only window: a real window handle that needs no display or desktop.
    pub(super) fn message_window() -> HWND {
        // SAFETY: a predefined class, a message-only parent, no menu, instance or
        // creation data.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("jstream test"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .expect("a message-only window")
    }

    pub(super) fn resolved(hwnd: HWND) -> Resolved {
        Resolved {
            source: Source::Window(hwnd),
            monitor: HMONITOR::default(),
            what: "window test".into(),
            kind: "window",
            id: "hwnd:0xtest".into(),
        }
    }

    fn os_error() -> PlatformError {
        PlatformError::Os {
            call: "IGraphicsCaptureItemInterop::CreateForWindow",
            code: None,
            message: "test".into(),
        }
    }

    /// The start race (#68 N4): a window destroyed after `resolve` checked it makes
    /// `CreateForWindow` fail, and `start` must report `NotFound`, not the OS error.
    #[test]
    fn a_window_destroyed_before_the_capture_starts_is_not_found() {
        let hwnd = message_window();
        let target = resolved(hwnd);
        assert!(!target_gone(&target.source), "the window is alive");
        assert!(matches!(
            not_found_if_gone(os_error(), &target),
            PlatformError::Os { .. }
        ));

        // SAFETY: this thread created the window and destroys it once.
        unsafe { DestroyWindow(hwnd) }.expect("destroyed");
        assert!(target_gone(&target.source));
        match not_found_if_gone(os_error(), &target) {
            PlatformError::NotFound { kind, id } => {
                assert_eq!((kind, id.as_str()), ("window", "hwnd:0xtest"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}

/// Platform tests on a real display (R-TEST-004): behind `hw-tests` and `#[ignore]`d,
/// because CI machines have no display to capture. Run them on a Windows machine with a
/// display: `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
#[cfg(all(test, feature = "hw-tests"))]
mod device_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::media::{ColorInfo, ColorRange, ColorSpace, PixelFormat};
    use jstream_platform::{
        Capture, CaptureTarget, GpuFrame, GpuInterop, MediaClock, PlatformError, VideoSurface,
    };

    use super::WgcCapture;
    use crate::{QpcClock, WindowsGpu};

    /// `pts` is on the clock's time line: within the last second, and at most 100 ms
    /// ahead of `now`. `SystemRelativeTime` can be ahead of the clock: on a 29 Hz display
    /// the GPU path, which delivers a frame within milliseconds, saw frames stamped about
    /// 15 ms after `now`, less than one refresh, so the time is the composition's target
    /// rather than when the frame was ready.
    fn assert_recent(
        pts: jstream_platform::media::MediaTime,
        now: jstream_platform::media::MediaTime,
    ) {
        let (pts, now) = (pts.ticks(), now.ticks());
        assert!(
            pts <= now + 1_000_000,
            "pts {pts} more than 100 ms after now {now}"
        );
        assert!(
            now - pts < 10_000_000,
            "pts {pts} more than a second before now {now}"
        );
    }

    /// The whole start path for the race in `tests`: the real `CreateForWindow` on a
    /// destroyed window gives `NotFound`.
    #[test]
    #[ignore = "needs Windows.Graphics.Capture"]
    fn create_item_for_a_destroyed_window_is_not_found() {
        let hwnd = super::tests::message_window();
        // SAFETY: this thread created the window and destroys it once.
        unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd) }.expect("destroyed");
        match super::create_item(&super::tests::resolved(hwnd)) {
            Err(PlatformError::NotFound { kind: "window", .. }) => {}
            other => panic!("expected NotFound, got {:?}", other.map(|_| "an item")),
        }
    }

    #[test]
    #[ignore = "needs a display and Windows.Graphics.Capture"]
    fn lists_displays_and_windows() {
        let targets = WgcCapture::new().targets().expect("targets");
        let displays: Vec<_> = targets
            .iter()
            .filter(|t| matches!(t.target, CaptureTarget::Display(_)))
            .collect();
        assert!(!displays.is_empty(), "no displays in {targets:?}");
        assert_eq!(displays.iter().filter(|t| t.primary).count(), 1);
        assert!(displays.iter().all(|t| t.size.is_some()));
        assert!(
            targets
                .iter()
                .filter(|t| matches!(t.target, CaptureTarget::Window(_)))
                .all(|t| !t.primary && !t.name.trim().is_empty())
        );
    }

    #[test]
    #[ignore = "needs a display and Windows.Graphics.Capture"]
    fn captures_the_primary_display_for_a_second() {
        let capture = WgcCapture::new();
        let primary = capture
            .targets()
            .expect("targets")
            .into_iter()
            .find(|t| t.primary)
            .expect("a primary display");
        let clock = QpcClock::new().expect("clock");
        let mut stream = capture.start(&primary.target).expect("start");
        let start = Instant::now();
        let mut pts = Vec::new();
        while start.elapsed() < Duration::from_secs(1) {
            let Some(surface) = stream.latest().expect("latest") else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let now = clock.now();
            let VideoSurface::Cpu(frame) = surface else {
                panic!("a GPU frame from the CPU path");
            };
            let info = frame.info();
            assert_eq!(info.format, PixelFormat::Nv12);
            assert_eq!(info.color, ColorInfo::default());
            assert_eq!(info.size.width() % 2, 0);
            assert_eq!(info.size.height() % 2, 0);
            if let Some(size) = primary.size {
                assert!(
                    size.width().abs_diff(info.size.width()) <= 1,
                    "{size:?} vs {:?}",
                    info.size
                );
            }
            // pts is on the clock's time line: in the last second, and not in the future.
            assert_eq!(info.pts.timebase(), clock.timebase());
            assert_recent(info.pts, now);
            pts.push(info.pts.ticks());
        }
        // A display delivers a first frame even if nothing changes on it.
        assert!(!pts.is_empty(), "no frames in a second");
        assert!(pts.windows(2).all(|w| w[0] < w[1]), "pts increase: {pts:?}");
        let stats = stream.stats();
        assert!(stats.frames_captured >= pts.len() as u64);
        drop(stream);

        // A target that doesn't exist.
        let missing = CaptureTarget::Display(jstream_platform::DeviceId::new(r"monitor:\\.\NOPE"));
        assert!(matches!(
            capture.start(&missing).err(),
            Some(PlatformError::NotFound { .. })
        ));
    }

    /// A second capture in one process, after the first has been dropped with
    /// everything it held (R-TEST-001). Before the process kept its apartment, the second
    /// start called a cached WinRT factory whose DLL COM had unloaded, and crashed. Run it
    /// on its own to exercise that: other tests in the process may already have started
    /// a capture.
    #[test]
    #[ignore = "needs a display and Windows.Graphics.Capture"]
    fn captures_again_after_a_capture_is_dropped() {
        for round in 0..3 {
            let capture = WgcCapture::new();
            let mut stream = capture.start(&primary(&capture).target).expect("start");
            let start = Instant::now();
            while stream.latest().expect("latest").is_none() {
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "round {round}: no frame"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(stream);
            drop(capture);
        }
    }

    /// The primary display's capture target.
    fn primary(capture: &WgcCapture) -> jstream_platform::CaptureTargetInfo {
        capture
            .targets()
            .expect("targets")
            .into_iter()
            .find(|t| t.primary)
            .expect("a primary display")
    }

    /// Reads back the top-left 64x64 pixels of `frame` (BGRA, 256 bytes a row) through
    /// `ctx`, staging the frame on that submit as render will.
    fn read_corner(ctx: &jstream_platform::GpuContext, frame: &GpuFrame) -> Vec<u8> {
        const SIDE: u32 = 64;
        let device = ctx.device();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: u64::from(SIDE * SIDE * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: frame.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIDE * 4),
                    rows_per_image: Some(SIDE),
                },
            },
            wgpu::Extent3d {
                width: SIDE,
                height: SIDE,
                depth_or_array_layers: 1,
            },
        );
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
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .expect("the readback completes");
        let bytes = buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapped")
            .to_vec();
        buffer.unmap();
        bytes
    }

    /// The GPU path: frames are shared BGRA textures on the context's device, with the
    /// copy finished before render reads them, and the ring's slots come back after each
    /// staged use. Needs screen activity to deliver more frames than the ring has slots:
    /// move the mouse over the primary display while it runs.
    #[test]
    #[ignore = "needs a display, Windows.Graphics.Capture and a DX12 GPU; move the mouse"]
    fn captures_the_primary_display_into_shared_textures() {
        let gpu = WindowsGpu::new();
        let ctx = gpu.open_device().expect("open the media adapter's device");
        let capture = WgcCapture::on_gpu(&ctx).expect("our context");
        let primary = primary(&capture);
        let clock = QpcClock::new().expect("clock");
        let mut stream = capture.start(&primary.target).expect("start");
        let start = Instant::now();
        let mut frames = 0u32;
        let mut last_pts = None;
        while frames < 20 {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{frames} frames in 10 s: move the mouse over the primary display; {:?}",
                stream.stats()
            );
            let Some(surface) = stream.latest().expect("latest") else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let now = clock.now();
            let VideoSurface::Gpu(frame) = surface else {
                panic!("a CPU frame from the GPU path");
            };
            let info = *frame.info();
            assert_eq!(info.format, PixelFormat::Bgra8);
            assert_eq!(info.color.range, ColorRange::Full);
            assert_eq!(info.color.space, ColorSpace::Bt709);
            let texture = frame.texture();
            assert_eq!(texture.format(), wgpu::TextureFormat::Bgra8UnormSrgb);
            assert_eq!(
                (texture.width(), texture.height()),
                (info.size.width(), info.size.height())
            );
            if let Some(size) = primary.size {
                assert_eq!(size, info.size);
            }
            assert_eq!(info.pts.timebase(), clock.timebase());
            assert_recent(info.pts, now);
            assert!(
                last_pts.is_none_or(|p| p < info.pts.ticks()),
                "pts increase"
            );
            last_pts = Some(info.pts.ticks());
            // Every frame is staged and read, so every slot is reused after a staged use.
            let corner = read_corner(&ctx, &frame);
            assert!(
                corner.chunks_exact(4).all(|px| px[3] == 255),
                "frame {frames}: the desktop is opaque, so a copy that hadn't finished \
                 (or garbage) shows as alpha below 255"
            );
            frames += 1;
        }
        // A queue of another device is refused.
        let other = gpu.open_device().expect("a second context");
        loop {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "no frame for the queue check"
            );
            if let Some(VideoSurface::Gpu(frame)) = stream.latest().expect("latest") {
                assert!(matches!(
                    frame.stage(other.queue()),
                    Err(PlatformError::InvalidState { .. })
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // The capture keeps the device's driver loaded, so the context can go first
        // (spike S1 crashed when raw objects outlived it).
        drop(other);
        drop(ctx);
        for _ in 0..10 {
            drop(stream.latest().expect("latest"));
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(stream);
        drop(capture);
    }

    /// The GPU path never blocks on a full ring: with every frame held, new frames are
    /// dropped and counted as skipped, and once they are released, frames flow again.
    /// Move the mouse over the primary display while it runs.
    #[test]
    #[ignore = "needs a display, Windows.Graphics.Capture and a DX12 GPU; move the mouse"]
    fn drops_frames_while_every_slot_is_held() {
        let gpu = WindowsGpu::new();
        let ctx = gpu.open_device().expect("open the media adapter's device");
        let capture = WgcCapture::on_gpu(&ctx).expect("our context");
        let mut stream = capture.start(&primary(&capture).target).expect("start");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut held = Vec::new();
        let skipped_before = loop {
            assert!(
                Instant::now() < deadline,
                "the ring never filled with {} frames held: move the mouse; {:?}",
                held.len(),
                stream.stats()
            );
            if let Some(surface) = stream.latest().expect("latest") {
                held.push(surface);
            }
            // The ring has four slots: once they are all held, frames are dropped.
            if held.len() >= 4 {
                let skipped = stream.stats().frames_skipped;
                std::thread::sleep(Duration::from_millis(500));
                if stream.stats().frames_skipped > skipped {
                    break skipped;
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(
            held.len() <= 5,
            "{} frames held from a ring of four",
            held.len()
        );
        assert!(skipped_before < stream.stats().frames_skipped);
        // Unstaged frames free their slots as soon as they drop.
        held.clear();
        let _ = stream.latest().expect("latest");
        loop {
            assert!(
                Instant::now() < deadline,
                "no frame after the release: move the mouse"
            );
            if stream.latest().expect("latest").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
