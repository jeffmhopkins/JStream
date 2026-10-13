//! Windows.Graphics.Capture of the primary display into D3D11 textures.

use crate::err::{Context, Result};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

pub fn primary_monitor() -> HMONITOR {
    // SAFETY: no pointers; (0,0) is always on the primary monitor.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

pub struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    pub size: (u32, u32),
}

/// The newest frame WGC delivered since the last call.
pub struct Frame {
    pub texture: ID3D11Texture2D,
    pub size: (u32, u32),
    /// Keeps the pool buffer alive until the copy is queued.
    _frame: Direct3D11CaptureFrame,
}

impl Capture {
    pub fn start(device: &ID3D11Device, monitor: HMONITOR) -> Result<Self> {
        let dxgi: IDXGIDevice = device.cast().context("IDXGIDevice")?;
        // SAFETY: `dxgi` is a live DXGI device.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .context("CreateDirect3D11DeviceFromDXGIDevice")?;
        let rt_device: IDirect3DDevice = inspectable.cast().context("IDirect3DDevice")?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .context("GraphicsCaptureItem interop factory")?;
        // SAFETY: `monitor` is a monitor handle from MonitorFromPoint.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }
            .context("IGraphicsCaptureItemInterop::CreateForMonitor")?;
        let size = item.Size().context("GraphicsCaptureItem::Size")?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &rt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .context("Direct3D11CaptureFramePool::CreateFreeThreaded")?;
        let session = pool
            .CreateCaptureSession(&item)
            .context("CreateCaptureSession")?;
        // Not available on every build; the yellow border is harmless for the spike.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);
        session.StartCapture().context("StartCapture")?;

        Ok(Self {
            pool,
            session,
            _item: item,
            size: (size.Width as u32, size.Height as u32),
        })
    }

    /// Drains the pool and returns the newest frame plus how many frames were drained.
    pub fn latest(&self) -> Result<(Option<Frame>, u64)> {
        let mut newest = None;
        let mut count = 0;
        // TryGetNextFrame yields an error (null) when the pool is empty.
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            count += 1;
            newest = Some(frame);
        }
        let Some(frame) = newest else {
            return Ok((None, 0));
        };
        let content = frame.ContentSize().context("ContentSize")?;
        let surface = frame.Surface().context("Direct3D11CaptureFrame::Surface")?;
        let access: IDirect3DDxgiInterfaceAccess =
            surface.cast().context("IDirect3DDxgiInterfaceAccess")?;
        // SAFETY: plain method call on a live COM object.
        let texture: ID3D11Texture2D =
            unsafe { access.GetInterface() }.context("GetInterface<ID3D11Texture2D>")?;
        Ok((
            Some(Frame {
                texture,
                size: (content.Width as u32, content.Height as u32),
                _frame: frame,
            }),
            count,
        ))
    }

    pub fn stop(&self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
