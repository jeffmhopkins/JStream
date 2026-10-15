//! Frames on the GPU: each accelerated paint's shared texture opened on the engine's
//! D3D12 device and copied, inside CEF's callback, into a texture the page owns (ARCH ›
//! Browser source, the path spike S4 measured). Nothing is read back to the CPU.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, VideoFrameInfo,
};
use jstream_platform::{FrameSync, GpuContext, GpuFrame, PlatformError, wgpu};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::PageShared;

/// Textures each page writes its paints into in turn, so a frame the compositor still
/// holds isn't written while it is drawn.
const RING: usize = 3;

/// The longest a paint waits for its copy before giving up on the frame.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

/// The engine's device, if it gave one, with its raw D3D12 device for opening CEF's
/// handles. Changing it bumps a generation, so pages remake their textures.
#[derive(Default)]
pub(crate) struct DeviceSlot(Mutex<(u64, Option<Device>)>);

/// A device pages copy onto.
#[derive(Clone)]
pub(crate) struct Device {
    // Field order is drop order: the raw clone goes before wgpu's device, which may
    // unload d3d12.dll when it goes (spike S1).
    d3d12: ID3D12Device,
    device: wgpu::Device,
    queue: wgpu::Queue,
    generation: u64,
}

// SAFETY: `ID3D12Device` is free-threaded (D3D12 devices may be used from any thread), and
// the wgpu handles are `Send + Sync` themselves.
unsafe impl Send for Device {}
// SAFETY: as above.
unsafe impl Sync for Device {}

impl DeviceSlot {
    /// Uses `gpu`'s device from the next paint on, or none.
    pub(crate) fn set(&self, gpu: Option<&GpuContext>) {
        let device = gpu.and_then(|gpu| {
            // SAFETY: only the raw `ID3D12Device` is cloned (an AddRef); wgpu keeps its own
            // reference, and this one is never used to destroy anything.
            let d3d12 = unsafe { gpu.device().as_hal::<Dx12>() }.map(|d| d.raw_device().clone());
            if d3d12.is_none() {
                tracing::warn!(
                    "the engine's GPU device is not D3D12; browser pages can't deliver frames"
                );
            }
            Some((d3d12?, gpu.device().clone(), gpu.queue().clone()))
        });
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slot.0 += 1;
        let generation = slot.0;
        slot.1 = device.map(|(d3d12, device, queue)| Device {
            d3d12,
            device,
            queue,
            generation,
        });
    }

    fn get(&self) -> Option<Device> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }
}

/// One page's textures and its latest frame.
#[derive(Default)]
pub(crate) struct PageFrames(Mutex<Frames>);

#[derive(Default)]
struct Frames {
    /// The device generation the textures were made on.
    generation: u64,
    size: (u32, u32),
    ring: Vec<wgpu::Texture>,
    next: usize,
    latest: Option<GpuFrame>,
    /// Whether a failed paint was logged since the last good one.
    warned: bool,
}

impl PageFrames {
    /// The latest frame, if any.
    pub(crate) fn latest(&self) -> Option<GpuFrame> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest
            .clone()
    }
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

/// Handles one accelerated paint of `page`, painted at `pts`: copies CEF's texture
/// into the page's next ring texture and makes it the latest frame. Runs inside CEF's
/// callback on its UI thread; the copy has finished on the GPU before it returns, as
/// CEF takes the texture back then. A paint that can't be copied is skipped, keeping the
/// last frame, and logged once until a paint succeeds again.
pub(crate) fn paint(page: &PageShared, handle: *mut c_void, pts: MediaTime) {
    let Some(device) = page.backend.device.get() else {
        return;
    };
    let mut frames = page.frames.0.lock().unwrap_or_else(PoisonError::into_inner);
    match copy(&device, &mut frames, handle, pts) {
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

fn copy(
    device: &Device,
    frames: &mut Frames,
    handle: *mut c_void,
    pts: MediaTime,
) -> Result<GpuFrame, String> {
    if handle.is_null() {
        return Err("CEF passed no shared texture".into());
    }
    let mut opened: Option<ID3D12Resource> = None;
    // SAFETY: CEF guarantees the handle is a live NT handle to a shared texture for the
    // duration of the callback this runs inside; `opened` is a valid out-pointer. D3D12
    // takes its own reference to the resource; the handle stays CEF's to close.
    unsafe { device.d3d12.OpenSharedHandle(HANDLE(handle), &mut opened) }.map_err(|e| {
        format!(
            "opening CEF's texture on the engine's device failed ({e}); CEF may render on \
             another GPU than the engine"
        )
    })?;
    let resource = opened.ok_or("OpenSharedHandle returned no resource")?;
    // SAFETY: a plain method call on a live COM object.
    let desc = unsafe { resource.GetDesc() };
    if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(format!(
            "CEF's texture has DXGI format {}, not BGRA8",
            desc.Format.0
        ));
    }
    if desc.MipLevels != 1 || desc.DepthOrArraySize != 1 || desc.SampleDesc.Count != 1 {
        return Err(format!(
            "CEF's texture has {} mips, {} layers and {} samples, not 1 each",
            desc.MipLevels, desc.DepthOrArraySize, desc.SampleDesc.Count
        ));
    }
    let size = (
        u32::try_from(desc.Width).map_err(|_| "CEF's texture is too wide")?,
        desc.Height,
    );
    let frame_size = FrameSize::new(size.0, size.1).map_err(|e| e.to_string())?;
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
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
    // `PRESENT` on D3D12; it is only read, and transitioned back to COMMON before the
    // submit ends.
    let cef_texture = unsafe {
        device.device.create_texture_from_hal::<Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("jstream-browser-cef-texture"),
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
    if frames.generation != device.generation || frames.size != size {
        frames.generation = device.generation;
        frames.size = size;
        frames.ring.clear();
        frames.next = 0;
        frames.latest = None;
    }
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
    encoder.copy_texture_to_texture(cef_texture.as_image_copy(), target.as_image_copy(), extent);
    encoder.transition_resources(
        std::iter::empty(),
        std::iter::once(wgpu::TextureTransition {
            texture: &cef_texture,
            selector: None,
            state: wgpu::TextureUses::PRESENT,
        }),
    );
    let submitted = device.queue.submit([encoder.finish()]);
    let waited = device.device.poll(wgpu::PollType::Wait {
        submission_index: Some(submitted),
        timeout: Some(COPY_TIMEOUT),
    });
    drop(cef_texture);
    waited.map_err(|e| format!("waiting for the copy: {e}"))?;
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
