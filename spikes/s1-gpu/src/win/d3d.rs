//! D3D11 device, shared NT-handle textures and the shared fence (D3D11 <-> D3D12).

use crate::err::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID};
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
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIAdapter1,
    IDXGIDevice, IDXGIResource1,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::{Interface, PCWSTR};

/// The D3D11 device used by WGC and Media Foundation, on wgpu's adapter.
pub struct D3D11 {
    pub device: ID3D11Device5,
    pub context: ID3D11DeviceContext4,
}

pub fn create_d3d11(adapter: &IDXGIAdapter) -> Result<D3D11> {
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
    .context("D3D11CreateDevice on wgpu's adapter")?;
    let device = device.context("D3D11CreateDevice returned no device")?;
    let context = context.context("D3D11CreateDevice returned no context")?;

    // Media Foundation's DXGI device manager requires a multithread-protected device.
    let mt: ID3D11Multithread = context
        .cast()
        .or_else(|_| device.cast())
        .context("ID3D11Multithread")?;
    // SAFETY: plain method call on a live COM object.
    unsafe {
        let _ = mt.SetMultithreadProtected(true);
    }

    Ok(D3D11 {
        device: device
            .cast()
            .context("ID3D11Device5 (needs Windows 10 1703+ for shared fences)")?,
        context: context.cast().context("ID3D11DeviceContext4")?,
    })
}

/// Closes a Win32 handle on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A texture allocated by D3D11 with a shared NT handle and opened on wgpu's D3D12 device.
pub struct SharedTexture {
    pub d3d11: ID3D11Texture2D,
    pub d3d12: ID3D12Resource,
}

impl SharedTexture {
    pub fn new(
        d3d11: &ID3D11Device5,
        d3d12: &ID3D12Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
    ) -> Result<Self> {
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
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialised and `tex` is a valid out-pointer.
        unsafe { d3d11.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .with_context(|| format!("CreateTexture2D {width}x{height} {format:?} shared"))?;
        let tex = tex.context("CreateTexture2D returned no texture")?;

        let res: IDXGIResource1 = tex.cast().context("IDXGIResource1")?;
        // SAFETY: plain method call on a live COM object; null name, default security.
        let handle = unsafe {
            res.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                PCWSTR::null(),
            )
        }
        .context("IDXGIResource1::CreateSharedHandle")?;
        let handle = OwnedHandle(handle);

        let mut opened: Option<ID3D12Resource> = None;
        // SAFETY: `handle.0` is a live NT handle to a shared resource; `opened` is a valid
        // out-pointer. D3D12 takes its own reference, so the handle can be closed after.
        unsafe { d3d12.OpenSharedHandle(handle.0, &mut opened) }
            .with_context(|| format!("ID3D12Device::OpenSharedHandle ({format:?} texture)"))?;
        Ok(Self {
            d3d11: tex,
            d3d12: opened.context("OpenSharedHandle returned no resource")?,
        })
    }
}

/// One fence shared by both APIs. Values only ever increase.
pub struct SharedFence {
    pub d3d11: ID3D11Fence,
    pub d3d12: ID3D12Fence,
}

impl SharedFence {
    pub fn new(d3d11: &ID3D11Device5, d3d12: &ID3D12Device) -> Result<Self> {
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: `fence` is a valid out-pointer.
        unsafe { d3d11.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .context("ID3D11Device5::CreateFence(SHARED)")?;
        let fence = fence.context("CreateFence returned no fence")?;
        // SAFETY: plain method call on a live COM object; null name, default security.
        let handle = unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
            .context("ID3D11Fence::CreateSharedHandle")?;
        let handle = OwnedHandle(handle);
        let mut opened: Option<ID3D12Fence> = None;
        // SAFETY: `handle.0` is a live NT handle to a shared fence; `opened` is a valid
        // out-pointer.
        unsafe { d3d12.OpenSharedHandle(handle.0, &mut opened) }
            .context("ID3D12Device::OpenSharedHandle (fence)")?;
        Ok(Self {
            d3d11: fence,
            d3d12: opened.context("OpenSharedHandle returned no fence")?,
        })
    }
}

/// Facts about a DXGI adapter for the report.
pub struct AdapterFacts {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub luid: LUID,
    pub driver_version: String,
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

/// Whether one of `adapter`'s outputs is `monitor`.
pub fn adapter_drives_monitor(adapter: &IDXGIAdapter, monitor: HMONITOR) -> bool {
    for i in 0.. {
        // SAFETY: plain method call on a live COM object; errors end the enumeration.
        let Ok(output) = (unsafe { adapter.EnumOutputs(i) }) else {
            return false;
        };
        // SAFETY: plain method call on a live COM object.
        if let Ok(desc) = unsafe { output.GetDesc() }
            && desc.Monitor == monitor
        {
            return true;
        }
    }
    false
}

pub fn luid_u64(luid: LUID) -> u64 {
    ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
}
