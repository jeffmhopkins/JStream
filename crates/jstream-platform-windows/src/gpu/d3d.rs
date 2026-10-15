//! D3D11 on wgpu's adapter: the device that capture and Media Foundation use, textures
//! shared with D3D12 through NT handles, and the fence both APIs signal and wait on
//! (ARCH › GPU pipeline, steps 1, 2 and 5). Ported from spike S1.

use jstream_platform::PlatformError;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_FLAG, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIResource1,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::{Interface, PCWSTR};

use crate::com::OsResult;

/// The D3D11 device and immediate context, on the same adapter as wgpu's device.
pub(crate) struct D3D11 {
    pub(crate) device: ID3D11Device5,
    /// The immediate context, for capture's copies into shared textures and its fence
    /// signals.
    pub(crate) context: ID3D11DeviceContext4,
}

/// Creates a multithread-protected D3D11 device on `adapter` (Media Foundation's DXGI
/// device manager requires the protection; shared fences need `ID3D11Device5`).
pub(crate) fn create_d3d11(adapter: &IDXGIAdapter) -> Result<D3D11, PlatformError> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: `adapter` is a live COM object; the out-pointers point to valid locals.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
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
    let device = device.ok_or_else(|| none("device"))?;
    let context = context.ok_or_else(|| none("context"))?;
    let mt: ID3D11Multithread = context
        .cast()
        .or_else(|_| device.cast())
        .os("ID3D11Multithread")?;
    // SAFETY: plain COM call on a live object.
    unsafe {
        let _ = mt.SetMultithreadProtected(true);
    }
    Ok(D3D11 {
        device: device
            .cast()
            .os("ID3D11Device5 (shared fences need Windows 10 1703 or later)")?,
        context: context.cast().os("ID3D11DeviceContext4")?,
    })
}

/// An NT handle, closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A texture allocated by D3D11 with a shared NT handle and opened on D3D12.
pub(crate) struct SharedTexture {
    /// The D3D11 side, which capture copies into (and the encoders will read).
    pub(crate) d3d11: ID3D11Texture2D,
    pub(crate) d3d12: ID3D12Resource,
}

impl SharedTexture {
    pub(crate) fn new(
        d3d11: &ID3D11Device5,
        d3d12: &ID3D12Device,
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
    ) -> Result<Self, PlatformError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                as u32,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialised and `texture` is a valid out-pointer.
        unsafe { d3d11.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .os("ID3D11Device::CreateTexture2D (shared)")?;
        let texture = texture.ok_or(PlatformError::Os {
            call: "ID3D11Device::CreateTexture2D",
            code: None,
            message: "returned no texture".into(),
        })?;
        let resource: IDXGIResource1 = texture.cast().os("IDXGIResource1")?;
        // SAFETY: plain COM call on a live object; no name, default security.
        let handle = OwnedHandle(
            unsafe {
                resource.CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )
            }
            .os("IDXGIResource1::CreateSharedHandle")?,
        );
        let mut opened: Option<ID3D12Resource> = None;
        // SAFETY: `handle.0` is a live NT handle to a shared resource and `opened` a valid
        // out-pointer. D3D12 takes its own reference, so the handle can close afterwards.
        unsafe { d3d12.OpenSharedHandle(handle.0, &mut opened) }
            .os("ID3D12Device::OpenSharedHandle (texture)")?;
        Ok(Self {
            d3d11: texture,
            d3d12: opened.ok_or(PlatformError::Os {
                call: "ID3D12Device::OpenSharedHandle",
                code: None,
                message: "returned no resource".into(),
            })?,
        })
    }
}

/// One fence shared by D3D11 and D3D12. Its values only ever increase, so only one
/// queue signals it (see the `gpu` module's fence values).
pub(crate) struct SharedFence {
    /// The D3D11 side, which capture signals.
    pub(crate) d3d11: ID3D11Fence,
    pub(crate) d3d12: ID3D12Fence,
}

impl SharedFence {
    pub(crate) fn new(d3d11: &ID3D11Device5, d3d12: &ID3D12Device) -> Result<Self, PlatformError> {
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: `fence` is a valid out-pointer.
        unsafe { d3d11.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .os("ID3D11Device5::CreateFence (shared)")?;
        let fence = fence.ok_or(PlatformError::Os {
            call: "ID3D11Device5::CreateFence",
            code: None,
            message: "returned no fence".into(),
        })?;
        // SAFETY: plain COM call on a live object; no name, default security.
        let handle = OwnedHandle(
            unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
                .os("ID3D11Fence::CreateSharedHandle")?,
        );
        let mut opened: Option<ID3D12Fence> = None;
        // SAFETY: `handle.0` is a live NT handle to a shared fence and `opened` a valid
        // out-pointer.
        unsafe { d3d12.OpenSharedHandle(handle.0, &mut opened) }
            .os("ID3D12Device::OpenSharedHandle (fence)")?;
        Ok(Self {
            d3d11: fence,
            d3d12: opened.ok_or(PlatformError::Os {
                call: "ID3D12Device::OpenSharedHandle",
                code: None,
                message: "returned no fence".into(),
            })?,
        })
    }
}

/// Whether one of `adapter`'s outputs shows `monitor`.
pub(crate) fn adapter_drives_monitor(adapter: &IDXGIAdapter, monitor: HMONITOR) -> bool {
    for i in 0.. {
        // SAFETY: plain COM call; an error ends the enumeration.
        let Ok(output) = (unsafe { adapter.EnumOutputs(i) }) else {
            return false;
        };
        // SAFETY: plain COM call on a live object.
        if let Ok(desc) = unsafe { output.GetDesc() }
            && desc.Monitor == monitor
        {
            return true;
        }
    }
    false
}
