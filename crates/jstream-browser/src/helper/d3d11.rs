//! The helper's frames: each accelerated paint's texture copied, inside CEF's callback,
//! into the next free one of three textures the page shares with the engine (NT
//! handles), on a D3D11 device on the engine's adapter. The copy has finished on the GPU
//! before the callback returns, as CEF takes its texture back then, and before the
//! engine is told, so the engine reads a finished texture. A texture stays the engine's
//! until it says it has copied it ([`PageTextures::release`]).

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext,
    ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter,
    IDXGIFactory4, IDXGIResource1,
};
use windows::core::{Interface, PCWSTR};

/// Textures each page shares with the engine.
pub(crate) const SLOTS: usize = 3;

/// The longest a paint waits for its copy before giving up on the frame.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

/// A D3D11 device on the engine's adapter. Used on CEF's UI thread only, where paints
/// arrive.
pub(crate) struct Gpu {
    device: ID3D11Device1,
    context: ID3D11DeviceContext,
    /// An event query, ended after each copy to wait for it.
    done: ID3D11Query,
    /// Which device this is, so pages remake their textures on a new one.
    generation: u64,
}

// SAFETY: the D3D11 device is free-threaded; the immediate context and the query are
// only used on CEF's UI thread, where paints arrive and the device is replaced. The lock
// around the `Gpu` only moves it between threads, it doesn't share the context.
unsafe impl Send for Gpu {}

impl Gpu {
    /// A device on the adapter with this LUID; `generation` tells it from earlier ones.
    pub(crate) fn on_adapter(luid: u64, generation: u64) -> Result<Self, String> {
        // SAFETY: plain factory and enumeration calls; the LUID only selects an adapter.
        let adapter: IDXGIAdapter = unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory1().map_err(|e| format!("creating a DXGI factory: {e}"))?;
            factory.EnumAdapterByLuid(LUID {
                LowPart: luid as u32,
                HighPart: (luid >> 32) as i32,
            })
        }
        .map_err(|e| format!("finding the engine's adapter: {e}"))?;
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: `adapter` is live, the out-pointers are valid, and the feature-level
        // slice outlives the call.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| format!("creating a D3D11 device on the engine's adapter: {e}"))?;
        let device = device
            .ok_or("D3D11CreateDevice returned no device")?
            .cast::<ID3D11Device1>()
            .map_err(|e| format!("the D3D11 device has no ID3D11Device1: {e}"))?;
        let context = context.ok_or("D3D11CreateDevice returned no context")?;
        let mut done: Option<ID3D11Query> = None;
        // SAFETY: a valid query description and out-pointer.
        unsafe {
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut done),
            )
        }
        .map_err(|e| format!("creating a D3D11 event query: {e}"))?;
        Ok(Self {
            device,
            context,
            done: done.ok_or("CreateQuery returned no query")?,
            generation,
        })
    }
}

/// One texture a page shares with the engine, and its NT handle, closed with it.
struct Shared {
    texture: ID3D11Texture2D,
    handle: HANDLE,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: the handle was created for this texture, is closed once, here, and
        // isn't used after.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// A page's shared textures.
#[derive(Default)]
pub(crate) struct PageTextures(Mutex<Ring>);

#[derive(Default)]
struct Ring {
    /// The device generation and size the textures were made for.
    generation: u64,
    size: (u32, u32),
    slots: [Option<Shared>; SLOTS],
    /// Slots the engine hasn't released. Kept across a new device or size: the engine
    /// may still be reading the slot's old texture.
    busy: [bool; SLOTS],
    next: usize,
}

// SAFETY: the D3D11 textures are only used on CEF's UI thread (paints), and the handles
// are plain values; the lock moves the ring between that thread and the pipe's reader,
// which only touches `busy`.
unsafe impl Send for Ring {}

/// One paint copied into a shared texture.
pub(crate) struct Painted {
    pub(crate) slot: u8,
    pub(crate) handle: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl PageTextures {
    /// The engine is done with `slot`.
    pub(crate) fn release(&self, slot: u8) {
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(busy) = ring.busy.get_mut(usize::from(slot)) {
            *busy = false;
        }
    }

    /// Copies CEF's texture `cef` into the next free slot. `Ok(None)` when the engine
    /// still holds every slot: the paint is skipped.
    pub(crate) fn paint(&self, gpu: &Gpu, cef: *mut c_void) -> Result<Option<Painted>, String> {
        if cef.is_null() {
            return Err("CEF passed no shared texture".into());
        }
        // SAFETY: CEF guarantees the handle is a live NT handle to a shared texture for
        // the duration of the callback this runs inside; D3D11 takes its own reference.
        let source: ID3D11Texture2D = unsafe { gpu.device.OpenSharedResource1(HANDLE(cef)) }
            .map_err(|e| {
                format!(
                    "opening CEF's texture failed ({e}); CEF may render on another GPU than \
                     the engine"
                )
            })?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a valid out-pointer.
        unsafe { source.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(format!(
                "CEF's texture has DXGI format {}, not BGRA8",
                desc.Format.0
            ));
        }
        if desc.MipLevels != 1 || desc.ArraySize != 1 || desc.SampleDesc.Count != 1 {
            return Err(format!(
                "CEF's texture has {} mips, {} layers and {} samples, not 1 each",
                desc.MipLevels, desc.ArraySize, desc.SampleDesc.Count
            ));
        }
        let size = (desc.Width, desc.Height);
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if ring.generation != gpu.generation || ring.size != size {
            ring.generation = gpu.generation;
            ring.size = size;
            ring.slots = Default::default();
        }
        let Some(slot) = (0..SLOTS)
            .map(|i| (ring.next + i) % SLOTS)
            .find(|&slot| !ring.busy[slot])
        else {
            return Ok(None);
        };
        if ring.slots[slot].is_none() {
            ring.slots[slot] = Some(create(gpu, size)?);
        }
        let (target, handle) = {
            let shared = ring.slots[slot].as_ref().ok_or("the slot has no texture")?;
            (shared.texture.clone(), shared.handle.0 as usize as u64)
        };
        // SAFETY: both textures are live, on this device, of the same size and format;
        // the context is used on this thread only.
        unsafe {
            gpu.context.CopyResource(&target, &source);
            gpu.context.End(&gpu.done);
            gpu.context.Flush();
        }
        let deadline = Instant::now() + COPY_TIMEOUT;
        loop {
            let mut finished = windows::core::BOOL(0);
            // SAFETY: an event query's data is a BOOL, which `finished` is, with its size.
            let polled = unsafe {
                gpu.context.GetData(
                    &gpu.done,
                    Some((&raw mut finished).cast()),
                    size_of::<windows::core::BOOL>() as u32,
                    0,
                )
            };
            match polled {
                Ok(()) if finished.as_bool() => break,
                Ok(()) if Instant::now() < deadline => std::thread::yield_now(),
                Ok(()) => return Err("the copy didn't finish in time".into()),
                Err(e) => return Err(format!("waiting for the copy: {e}")),
            }
        }
        ring.busy[slot] = true;
        ring.next = slot + 1;
        Ok(Some(Painted {
            slot: slot as u8,
            handle,
            width: size.0,
            height: size.1,
        }))
    }
}

/// A texture of `size` the engine can open through its NT handle.
fn create(gpu: &Gpu, size: (u32, u32)) -> Result<Shared, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: a valid description and out-pointer, no initial data.
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|e| format!("creating a shared texture: {e}"))?;
    let texture = texture.ok_or("CreateTexture2D returned no texture")?;
    let resource: IDXGIResource1 = texture
        .cast()
        .map_err(|e| format!("the shared texture has no IDXGIResource1: {e}"))?;
    // SAFETY: an unnamed handle with default security, for a live shared texture.
    let handle = unsafe {
        resource.CreateSharedHandle(
            None,
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            PCWSTR::null(),
        )
    }
    .map_err(|e| format!("sharing the texture: {e}"))?;
    Ok(Shared { texture, handle })
}
