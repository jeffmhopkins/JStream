//! The helper's frames: each accelerated paint's texture copied, inside CEF's callback,
//! into the next free one of three textures the page shares with the engine (NT
//! handles), on a D3D11 device on the engine's adapter. The copy is submitted before the
//! callback returns, as CEF hands its texture back to its pool then, and waited for off
//! CEF's UI thread ([`Copy::wait`]), before the engine is told, so the engine reads a
//! finished texture. A texture stays the engine's until it says it has copied it
//! ([`PageTextures::release`]).
//!
//! # Behaviour
//!
//! A D3D11 device on the engine's adapter. Each
//! accelerated paint's texture is opened, checked to be single-mip BGRA8, and copied into
//! the next of three textures the page shares with the engine through NT handles. The
//! copy is submitted and flushed inside CEF's callback, followed by a fence signal, and
//! the callback returns without waiting: every page's paints arrive on CEF's one UI
//! thread, which also paces their frames, so waiting there for each copy held back every
//! page. A thread of the helper's waits for the fence, up to 2 s, before the engine is
//! told of the frame. A slot the engine hasn't released isn't painted into: with all
//! three held, the paint is skipped. Up to eight of CEF's pooled textures stay open per
//! page, each matched by a duplicate of CEF's handle, so a texture handed over again
//! isn't opened again. Popup-widget paints are ignored.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HMODULE,
    LUID, WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_FENCE_FLAG_NONE, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11Device1, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter,
    IDXGIFactory4, IDXGIResource1,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, WaitForSingleObject};
use windows::core::{Interface, PCWSTR};

/// Textures each page shares with the engine.
pub(crate) const SLOTS: usize = 3;

/// The longest a paint's copy is waited for before the frame is given up.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

/// A D3D11 device on the engine's adapter. Used on CEF's UI thread only, where paints
/// arrive.
pub(crate) struct Gpu {
    device: ID3D11Device1,
    context: ID3D11DeviceContext4,
    /// Signalled after each copy with the next of `fenced`, for [`Copy::wait`].
    fence: ID3D11Fence,
    /// The fence's last value.
    fenced: u64,
    /// Which device this is, so pages remake their textures on a new one.
    generation: u64,
}

// SAFETY: the D3D11 device and its fences are free-threaded; the immediate context is
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
        let context = context
            .ok_or("D3D11CreateDevice returned no context")?
            .cast::<ID3D11DeviceContext4>()
            .map_err(|e| format!("the D3D11 context has no ID3D11DeviceContext4: {e}"))?;
        let device5 = device
            .cast::<ID3D11Device5>()
            .map_err(|e| format!("the D3D11 device has no ID3D11Device5: {e}"))?;
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: a plain fence creation, starting at 0, not shared, with a valid
        // out-pointer.
        unsafe { device5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence) }
            .map_err(|e| format!("creating a D3D11 fence: {e}"))?;
        let fence = fence.ok_or("CreateFence returned no fence")?;
        Ok(Self {
            device,
            context,
            fence,
            fenced: 0,
            generation,
        })
    }
}

/// One texture a page shares with the engine, and its NT handle, closed with it.
struct Shared {
    texture: ID3D11Texture2D,
    handle: HANDLE,
}

// SAFETY: a D3D11 resource's reference count is thread-safe, and the texture is only
// used in a context on CEF's UI thread; other threads (the pipe's reader through the
// ring, the copy thread through a `Copy`) only hold it or drop it. The handle is a plain
// value, closed once, by whichever thread drops the last holder.
unsafe impl Send for Shared {}
// SAFETY: as above: no thread but the UI thread does more than hold it.
unsafe impl Sync for Shared {}

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
    slots: [Option<Arc<Shared>>; SLOTS],
    /// Slots the engine hasn't released. Kept across a new device or size: the engine
    /// may still be reading the slot's old texture.
    busy: [bool; SLOTS],
    next: usize,
    /// CEF's textures opened on this device, most recent last, so a texture CEF's pool
    /// hands over again isn't opened again (opening one costs milliseconds of CEF's UI
    /// thread). Each keeps a duplicate of CEF's handle, which keeps the texture's
    /// identity: a handle value CEF reuses for another texture doesn't match.
    sources: Vec<Source>,
}

/// One of CEF's textures, opened.
struct Source {
    /// CEF's handle value when it was opened.
    value: usize,
    /// Our duplicate of that handle, compared with each later one.
    handle: OwnedHandle,
    texture: ID3D11Texture2D,
}

/// The most of CEF's textures a page keeps open: more than its pool holds.
const SOURCES: usize = 8;

// SAFETY: the D3D11 textures are only used on CEF's UI thread (paints), and the handles
// are plain values; the lock moves the ring between that thread and the pipe's reader,
// which only touches `busy`. A [`Copy`] holds a slot's texture only to keep it alive,
// dropping it on the copy thread, as COM reference counts allow.
unsafe impl Send for Ring {}

impl Ring {
    /// CEF's texture `cef`, opened on `gpu`'s device: the one opened before if CEF's
    /// pool hands it over again.
    fn source(&mut self, gpu: &Gpu, cef: *mut c_void) -> Result<ID3D11Texture2D, String> {
        let value = cef as usize;
        let known = self.sources.iter().position(|source| {
            // SAFETY: both are live handles: CEF's for the callback this runs inside, and
            // our duplicate, owned by the entry.
            source.value == value
                && unsafe {
                    CompareObjectHandles(HANDLE(cef), HANDLE(source.handle.as_raw_handle()))
                }
                .as_bool()
        });
        if let Some(at) = known {
            let source = self.sources.remove(at);
            let texture = source.texture.clone();
            self.sources.push(source);
            return Ok(texture);
        }
        // SAFETY: CEF guarantees the handle is a live NT handle to a shared texture for
        // the duration of the callback this runs inside; D3D11 takes its own reference.
        let texture: ID3D11Texture2D = unsafe { gpu.device.OpenSharedResource1(HANDLE(cef)) }
            .map_err(|e| {
                format!(
                    "opening CEF's texture failed ({e}); CEF may render on another GPU than \
                     the engine"
                )
            })?;
        let mut duplicate = HANDLE::default();
        // SAFETY: CEF's handle is live for the callback; the duplicate is ours, in this
        // process, and owned by the entry from here on.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(cef),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        };
        // Without a duplicate the texture is used once and not kept.
        if duplicated.is_ok() {
            // SAFETY: `DuplicateHandle` just made this handle, which nothing else owns.
            let handle = unsafe { OwnedHandle::from_raw_handle(duplicate.0) };
            if self.sources.len() >= SOURCES {
                self.sources.remove(0);
            }
            self.sources.push(Source {
                value,
                handle,
                texture: texture.clone(),
            });
        }
        Ok(texture)
    }
}

/// A copy submitted into a page's shared texture, for [`Copy::wait`] on another thread.
pub(crate) struct Copy {
    /// The texture copied into, and its handle, kept open until the engine has been
    /// told of it, even if a new size or device remakes the page's textures meanwhile.
    _shared: Arc<Shared>,
    fence: ID3D11Fence,
    value: u64,
    /// The paint the copy makes.
    pub(crate) painted: Painted,
}

// SAFETY: a D3D11 fence is free-threaded and `wait` only asks it to set an event; the
// shared texture is only held, to be released on whichever thread drops it (COM
// reference counts are thread-safe), and its handle closed there.
unsafe impl Send for Copy {}

impl Copy {
    /// Waits until the GPU has finished the copy, up to [`COPY_TIMEOUT`], on `event`
    /// (an auto-reset event only this thread waits on).
    pub(crate) fn wait(&self, event: &CopyEvent) -> Result<(), String> {
        // SAFETY: the fence is live and the event handle is owned by `event`.
        unsafe { self.fence.SetEventOnCompletion(self.value, event.0) }
            .map_err(|e| format!("waiting for the copy: {e}"))?;
        // SAFETY: a live event handle.
        let waited = unsafe {
            WaitForSingleObject(
                event.0,
                u32::try_from(COPY_TIMEOUT.as_millis()).unwrap_or(u32::MAX),
            )
        };
        if waited == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err("the copy didn't finish in time".into())
        }
    }
}

/// The event a thread waits for copies on, closed with it.
pub(crate) struct CopyEvent(HANDLE);

impl CopyEvent {
    pub(crate) fn new() -> Result<Self, String> {
        // SAFETY: an unnamed auto-reset event, initially unsignalled, default security.
        unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map(Self)
            .map_err(|e| format!("creating an event: {e}"))
    }
}

impl Drop for CopyEvent {
    fn drop(&mut self) {
        // SAFETY: the handle was created here, is closed once, here, and isn't used after.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

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

    /// Submits the copy of CEF's texture `cef` into the next free slot, which is held
    /// from now on: wait for the [`Copy`] before telling the engine, and
    /// [`release`](Self::release) the slot if it fails. `Ok(None)` when the engine still
    /// holds every slot: the paint is skipped. `opening` is set to the time spent
    /// finding or opening CEF's texture, for the paint counts.
    pub(crate) fn paint(
        &self,
        gpu: &mut Gpu,
        cef: *mut c_void,
        opening: &mut Duration,
    ) -> Result<Option<Copy>, String> {
        if cef.is_null() {
            return Err("CEF passed no shared texture".into());
        }
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if ring.generation != gpu.generation {
            ring.sources.clear();
        }
        let started = std::time::Instant::now();
        let source = ring.source(gpu, cef);
        *opening = started.elapsed();
        let source = source?;
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
            ring.slots[slot] = Some(Arc::new(create(gpu, size)?));
        }
        let shared = Arc::clone(ring.slots[slot].as_ref().ok_or("the slot has no texture")?);
        let (target, handle) = (shared.texture.clone(), shared.handle.0 as usize as u64);
        gpu.fenced += 1;
        // SAFETY: both textures are live, on this device, of the same size and format;
        // the context is used on this thread only, and the fence is this device's.
        unsafe {
            gpu.context.CopyResource(&target, &source);
            gpu.context
                .Signal(&gpu.fence, gpu.fenced)
                .map_err(|e| format!("signalling the copy's fence: {e}"))?;
            gpu.context.Flush();
        }
        ring.busy[slot] = true;
        ring.next = slot + 1;
        Ok(Some(Copy {
            _shared: shared,
            fence: gpu.fence.clone(),
            value: gpu.fenced,
            painted: Painted {
                slot: slot as u8,
                handle,
                width: size.0,
                height: size.1,
            },
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
