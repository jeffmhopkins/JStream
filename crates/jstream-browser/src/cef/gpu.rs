//! Frames on the GPU: each of the helper's shared textures opened once on the engine's
//! D3D12 device, and each paint copied from it into a texture the page owns (ARCH ›
//! Browser source). Nothing is read back to the CPU.

use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, VideoFrameInfo,
};
use jstream_platform::{FrameSync, GpuContext, GpuFrame, PlatformError, SubmitLock, wgpu};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::link::{Frames, PageFrames, Paint};

/// Textures each page writes its paints into in turn, so a frame the compositor still
/// holds isn't written while it is drawn.
const RING: usize = 3;

/// The most shared textures the helper paints a page into (its own ring).
const HELPER_SLOTS: usize = 3;

/// The longest the engine waits for a copy before giving up on the frame.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

/// The engine's device, if it gave one, with its raw D3D12 device for opening the
/// helper's textures. Changing it bumps a generation, so pages remake their textures.
#[derive(Default)]
pub(crate) struct DeviceSlot(Mutex<Slot>);

#[derive(Default)]
struct Slot {
    generation: u64,
    device: Option<Device>,
    /// The device's adapter, which the helper must paint on.
    luid: Option<u64>,
}

/// A device pages copy onto.
#[derive(Clone)]
pub(crate) struct Device {
    // Field order is drop order: the raw clone goes before wgpu's device, which may
    // unload d3d12.dll when it goes (spike S1).
    d3d12: ID3D12Device,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The context's submit lock: render stages frames on this queue from its own
    /// thread, so the copies here are submitted with it held.
    submit: SubmitLock,
    generation: u64,
}

// SAFETY: `ID3D12Device` is free-threaded (D3D12 devices may be used from any thread), and
// the wgpu handles are `Send + Sync` themselves.
unsafe impl Send for Device {}
// SAFETY: as above.
unsafe impl Sync for Device {}

impl DeviceSlot {
    /// Uses `gpu`'s device from the next paint on, or none. Returns the LUID of the
    /// adapter the helper must now paint on, if any.
    pub(crate) fn set(&self, gpu: Option<&GpuContext>) -> Option<u64> {
        let device = gpu.and_then(|gpu| {
            // SAFETY: only the raw `ID3D12Device` is cloned (an AddRef); wgpu keeps its own
            // reference, and this one is never used to destroy anything.
            let d3d12 = unsafe { gpu.device().as_hal::<Dx12>() }.map(|d| d.raw_device().clone());
            if d3d12.is_none() {
                tracing::warn!("the GPU device is not D3D12; browser pages can't deliver frames");
            }
            Some((
                d3d12?,
                gpu.device().clone(),
                gpu.queue().clone(),
                gpu.submit_lock().clone(),
            ))
        });
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slot.generation += 1;
        let generation = slot.generation;
        slot.luid = device
            .as_ref()
            .and(gpu)
            .and_then(|gpu| gpu.adapter_id().luid);
        if device.is_some() && slot.luid.is_none() {
            tracing::warn!(
                "the engine's GPU adapter has no LUID; browser pages can't deliver frames"
            );
        }
        slot.device = device.map(|(d3d12, device, queue, submit)| Device {
            d3d12,
            device,
            queue,
            submit,
            generation,
        });
        slot.luid
    }

    /// The LUID of the adapter the helper must paint on, if any.
    pub(crate) fn luid(&self) -> Option<u64> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).luid
    }

    fn get(&self) -> Option<Device> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .device
            .clone()
    }
}

/// Imports one helper process's paints: its textures' handles are its own, so each is
/// duplicated into the engine's process before D3D12 opens it.
pub(crate) struct HelperFrames {
    device: Arc<DeviceSlot>,
    process: Arc<OwnedHandle>,
}

impl HelperFrames {
    /// Imports from the helper whose process handle is `process` onto `device`'s device.
    pub(crate) fn new(device: Arc<DeviceSlot>, process: OwnedHandle) -> Self {
        Self {
            device,
            process: Arc::new(process),
        }
    }
}

impl Frames for HelperFrames {
    fn page(&self) -> Box<dyn PageFrames> {
        Box::new(HelperPageFrames {
            device: Arc::clone(&self.device),
            process: Arc::clone(&self.process),
            frames: Mutex::new(PageState::default()),
        })
    }
}

/// One page's textures and its latest frame.
struct HelperPageFrames {
    device: Arc<DeviceSlot>,
    process: Arc<OwnedHandle>,
    frames: Mutex<PageState>,
}

#[derive(Default)]
struct PageState {
    /// The device generation the textures were made and opened on.
    generation: u64,
    size: (u32, u32),
    /// The helper's textures, opened on the engine's device, by slot, with the
    /// helper's handle each was opened from.
    opened: [Option<(u64, wgpu::Texture)>; HELPER_SLOTS],
    ring: Vec<wgpu::Texture>,
    next: usize,
    latest: Option<GpuFrame>,
    /// Whether a failed paint was logged since the last good one.
    warned: bool,
}

/// The page's frames need no synchronisation: each is a texture of the engine's own
/// device, written by a copy that finished before the frame was handed out, and read
/// on the same queue.
#[derive(Debug)]
struct Copied;

impl FrameSync for Copied {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        Ok(())
    }
}

impl PageFrames for HelperPageFrames {
    /// Copies the helper's texture into the page's next ring texture and makes it the
    /// latest frame, waiting for the copy on the GPU, as the helper reuses the texture
    /// once this returns. A paint that can't be copied is skipped, keeping the last
    /// frame, and logged once until a paint succeeds again.
    fn paint(&self, paint: &Paint, pts: MediaTime) {
        let Some(device) = self.device.get() else {
            return;
        };
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        match copy(&device, &self.process, &mut frames, paint, pts) {
            Ok(frame) => {
                frames.latest = Some(frame);
                frames.warned = false;
            }
            Err(reason) => {
                if !frames.warned {
                    tracing::warn!(%reason, "a browser page's paint could not be copied");
                    frames.warned = true;
                }
            }
        }
    }

    fn latest(&self) -> Option<GpuFrame> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest
            .clone()
    }
}

fn copy(
    device: &Device,
    process: &OwnedHandle,
    frames: &mut PageState,
    paint: &Paint,
    pts: MediaTime,
) -> Result<GpuFrame, String> {
    let slot = usize::from(paint.slot);
    if slot >= HELPER_SLOTS {
        return Err(format!("the helper named texture slot {slot}"));
    }
    let size = (paint.width, paint.height);
    let frame_size = FrameSize::new(size.0, size.1).map_err(|e| e.to_string())?;
    if frames.generation != device.generation || frames.size != size {
        frames.generation = device.generation;
        frames.size = size;
        frames.opened = Default::default();
        frames.ring.clear();
        frames.next = 0;
        frames.latest = None;
    }
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let source = match &frames.opened[slot] {
        Some((handle, texture)) if *handle == paint.handle => texture.clone(),
        _ => {
            let texture = open(device, process, paint.handle, extent)?;
            frames.opened[slot] = Some((paint.handle, texture.clone()));
            texture
        }
    };
    if frames.ring.len() < RING {
        frames
            .ring
            .push(device.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-browser-page"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                // The compositor may sample it as sRGB.
                view_formats: &[wgpu::TextureFormat::Bgra8UnormSrgb],
            }));
    }
    let index = frames.next % frames.ring.len();
    frames.next = frames.next.wrapping_add(1);
    let target = frames.ring[index].clone();
    let mut encoder = device
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-browser-copy"),
        });
    encoder.copy_texture_to_texture(source.as_image_copy(), target.as_image_copy(), extent);
    // Back to COMMON (wgpu's `PRESENT` on D3D12), the state another device's work on a
    // shared texture expects.
    encoder.transition_resources(
        std::iter::empty(),
        std::iter::once(wgpu::TextureTransition {
            texture: &source,
            selector: None,
            state: wgpu::TextureUses::PRESENT,
        }),
    );
    // Under the context's submit lock, so this submit can't take a wait or signal that
    // render staged for its own (FrameSync::stage); released before the wait below.
    let submitted = {
        let _submitting = device.submit.lock();
        device.queue.submit([encoder.finish()])
    };
    device
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: Some(COPY_TIMEOUT),
        })
        .map_err(|e| format!("waiting for the copy: {e}"))?;
    let info = VideoFrameInfo {
        size: frame_size,
        format: PixelFormat::Bgra8,
        // Chromium composites in sRGB; every code value is used.
        color: ColorInfo {
            space: ColorSpace::Bt709,
            range: ColorRange::Full,
        },
        pts,
    };
    Ok(GpuFrame::new(target, info, Arc::new(Copied)))
}

/// Opens the helper's texture `handle` (an NT handle in its process) on the engine's
/// device as a wgpu texture of `extent`, checking it is what the helper said.
fn open(
    device: &Device,
    process: &OwnedHandle,
    handle: u64,
    extent: wgpu::Extent3d,
) -> Result<wgpu::Texture, String> {
    let theirs = HANDLE(handle as usize as *mut std::ffi::c_void);
    let mut ours = HANDLE::default();
    // SAFETY: `process` is the helper's live process handle, which this process created
    // with full access; `theirs` is only a value to the call, which fails cleanly if it
    // names nothing in that process. `ours` is a valid out-pointer.
    unsafe {
        DuplicateHandle(
            HANDLE(process.as_raw_handle()),
            theirs,
            GetCurrentProcess(),
            &mut ours,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .map_err(|e| format!("duplicating the helper's texture handle: {e}"))?;
    let mut opened: Option<ID3D12Resource> = None;
    // SAFETY: `ours` is a live handle this process owns; `opened` is a valid
    // out-pointer. D3D12 takes its own reference to the resource.
    let result = unsafe { device.d3d12.OpenSharedHandle(ours, &mut opened) };
    // SAFETY: `ours` was duplicated above, is closed once, and isn't used after.
    let _ = unsafe { CloseHandle(ours) };
    result.map_err(|e| {
        format!(
            "opening the helper's texture on the engine's device failed ({e}); the helper may \
             paint on another GPU than the engine"
        )
    })?;
    let resource = opened.ok_or("OpenSharedHandle returned no resource")?;
    // SAFETY: a plain method call on a live COM object.
    let desc = unsafe { resource.GetDesc() };
    if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(format!(
            "the helper's texture has DXGI format {}, not BGRA8",
            desc.Format.0
        ));
    }
    if desc.MipLevels != 1 || desc.DepthOrArraySize != 1 || desc.SampleDesc.Count != 1 {
        return Err(format!(
            "the helper's texture has {} mips, {} layers and {} samples, not 1 each",
            desc.MipLevels, desc.DepthOrArraySize, desc.SampleDesc.Count
        ));
    }
    if desc.Width != u64::from(extent.width) || desc.Height != extent.height {
        return Err(format!(
            "the helper's texture is {}x{}, not the {}x{} it said",
            desc.Width, desc.Height, extent.width, extent.height
        ));
    }
    let format = wgpu::TextureFormat::Bgra8Unorm;
    // SAFETY: `resource` is a live single-mip, single-sample 2D texture of exactly this
    // format and size, from its own description above.
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
    // SAFETY: the resource was opened on this wgpu device's own `ID3D12Device` and
    // matches the descriptor. A shared texture starts in COMMON, which wgpu calls
    // `PRESENT` on D3D12; it is only read, and every copy transitions it back to COMMON
    // before its submit ends.
    Ok(unsafe {
        device.device.create_texture_from_hal::<Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("jstream-browser-helper-texture"),
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
    })
}
