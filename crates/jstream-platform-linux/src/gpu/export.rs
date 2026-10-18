//! Encode targets: NV12 Vulkan images exported as DMA-BUFs, so the VA-API encoder
//! (`vaapi::session`) reads what render wrote without a CPU copy (ADR-0060 §6,
//! `jstream-platform` README › GPU frames).
//!
//! - A device can export when [`exportable`] says so at open: Vulkan 1.2 on both the
//!   instance and the device (for timeline semaphores, core there), wgpu's NV12 and
//!   DMA-BUF features, a DRM render node to name to VA-API
//!   (`VK_EXT_physical_device_drm`), and an NV12 image in the `LINEAR` DRM layout that
//!   render can draw into and that the driver exports as a DMA-BUF. Anything missing
//!   leaves the context without encode targets, and the engine encodes CPU frames.
//! - [`target`] allocates one such image with dedicated, exported memory, wraps it as
//!   a `wgpu` NV12 texture, and keeps the DMA-BUF's descriptor and plane layout in its
//!   [`TargetSync`], which VA-API finds through [`FrameSync::platform`].
//! - Render writes a target in one submit and calls [`FrameSync::stage`] first, which
//!   adds a signal of the context's timeline semaphore to that submit. The encoder waits
//!   for that value ([`TargetSync::wait_written`]) before it encodes the frame.
//!
//! `LINEAR` is the one layout every VA-API driver imports; a tiled layout would save
//! bandwidth on the GPU but needs the two drivers to agree on its modifier.

use std::any::Any;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ash::vk;
use jstream_platform::media::{ColorInfo, FrameSize, MediaTime, PixelFormat, VideoFrameInfo};
use jstream_platform::{FrameSync, GpuFrame, PlatformError};
use wgpu::hal::api::Vulkan;

use super::{Shared, raw_queue};

/// `DRM_FORMAT_MOD_LINEAR`.
pub(crate) const MOD_LINEAR: u64 = 0;
/// The Vulkan format of wgpu's `NV12` (wgpu-hal's mapping).
const NV12: vk::Format = vk::Format::G8_B8R8_2PLANE_420_UNORM;
/// What render does with a target: draw both planes (plane views, hence
/// `MUTABLE_FORMAT` and `EXTENDED_USAGE`), sample it, and copy it out in tests.
const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
        | vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw(),
);
const FLAGS: vk::ImageCreateFlags = vk::ImageCreateFlags::from_raw(
    vk::ImageCreateFlags::MUTABLE_FORMAT.as_raw() | vk::ImageCreateFlags::EXTENDED_USAGE.as_raw(),
);
const DMA_BUF: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

fn os(call: &'static str, message: impl ToString) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: message.to_string(),
    }
}

/// The `major:minor` of a Linux device number, as glibc's `gnu_dev_major` and
/// `gnu_dev_minor` split it.
pub(crate) fn device_number(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    (major, minor)
}

/// The DRM render node of `adapter` as `(major, minor)`, when the adapter can export
/// encode targets VA-API reads (see the module docs); `None` otherwise.
pub(crate) fn exportable(adapter: &wgpu::Adapter) -> Option<(u64, u64)> {
    let wanted =
        wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
    if !adapter.features().contains(wanted) {
        return None;
    }
    // SAFETY: the hal adapter is only used to read the raw instance and physical
    // device for the property queries below, while `adapter` keeps both alive.
    let hal = unsafe { adapter.as_hal::<Vulkan>() }?;
    let instance = hal.shared_instance();
    let version = hal.physical_device_capabilities().properties().api_version;
    if instance.instance_api_version() < vk::API_VERSION_1_2 || version < vk::API_VERSION_1_2 {
        return None;
    }
    if !hal
        .physical_device_capabilities()
        .supports_extension(ash::ext::physical_device_drm::NAME)
    {
        return None;
    }
    let raw = instance.raw_instance();
    let device = hal.raw_physical_device();
    let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
    {
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
        // SAFETY: a valid physical device of `raw` that supports the extension whose
        // struct is chained, and an initialised chain that outlives the call.
        unsafe { raw.get_physical_device_properties2(device, &mut props) };
    }
    if drm.has_render == vk::FALSE {
        return None;
    }
    let node = (
        u64::try_from(drm.render_major).ok()?,
        u64::try_from(drm.render_minor).ok()?,
    );
    exports_nv12(raw, device).then_some(node)
}

/// Whether the driver creates and exports the NV12 image [`target`] makes.
fn exports_nv12(instance: &ash::Instance, device: vk::PhysicalDevice) -> bool {
    let mut drm = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(MOD_LINEAR)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(DMA_BUF);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(NV12)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(USAGE)
        .flags(FLAGS)
        .push_next(&mut external)
        .push_next(&mut drm);
    let mut external_props = vk::ExternalImageFormatProperties::default();
    let ok = {
        let mut props = vk::ImageFormatProperties2::default().push_next(&mut external_props);
        // SAFETY: a valid physical device of `instance`, and initialised info and
        // properties chains that outlive the call.
        unsafe { instance.get_physical_device_image_format_properties2(device, &info, &mut props) }
            .is_ok()
    };
    ok && external_props
        .external_memory_properties
        .external_memory_features
        .contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
}

/// A context's export state: the timeline semaphore render's target writes signal.
pub(crate) struct Export {
    raw: ash::Device,
    memory_fd: ash::khr::external_memory_fd::Device,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    timeline: vk::Semaphore,
    /// The last value handed to a submit.
    last: AtomicU64,
    /// The device's DRM render node, `(major, minor)`.
    pub(crate) render_node: (u64, u64),
}

impl std::fmt::Debug for Export {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("render_node", &self.render_node)
            .finish_non_exhaustive()
    }
}

impl Export {
    /// The export state of `device`, opened on an adapter [`exportable`] accepted,
    /// with render node `node`.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the device is not Vulkan or the semaphore can't be
    /// created.
    pub(crate) fn new(device: &wgpu::Device, node: (u64, u64)) -> Result<Self, PlatformError> {
        // SAFETY: the hal device is only used to clone its raw handles, which `Shared`
        // keeps no longer than the `wgpu::Device` it also holds.
        let hal = unsafe { device.as_hal::<Vulkan>() }
            .ok_or_else(|| os("wgpu::Device::as_hal", "the device is not Vulkan"))?;
        let raw = hal.raw_device().clone();
        let instance = hal.shared_instance().raw_instance().clone();
        let physical = hal.raw_physical_device();
        let memory_fd = ash::khr::external_memory_fd::Device::new(&instance, &raw);
        let mut kind = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let info = vk::SemaphoreCreateInfo::default().push_next(&mut kind);
        // SAFETY: a live device with timeline semaphores (core in Vulkan 1.2, which
        // `exportable` required, and wgpu-hal enables the feature where it exists).
        let timeline = unsafe { raw.create_semaphore(&info, None) }
            .map_err(|e| os("vkCreateSemaphore (timeline)", e))?;
        Ok(Self {
            raw,
            memory_fd,
            instance,
            physical,
            timeline,
            last: AtomicU64::new(0),
            render_node: node,
        })
    }

    /// Waits until the timeline reaches `value`, for at most `timeout`.
    fn wait(&self, value: u64, timeout: Duration) -> Result<(), PlatformError> {
        let semaphores = [self.timeline];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        let nanos = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: the semaphore is a live timeline semaphore of this device.
        match unsafe { self.raw.wait_semaphores(&info, nanos) } {
            Ok(()) => Ok(()),
            Err(vk::Result::TIMEOUT) => Err(PlatformError::Os {
                call: "vkWaitSemaphores",
                code: None,
                message: format!("render didn't finish an encode target within {timeout:?}"),
            }),
            Err(e) => Err(os("vkWaitSemaphores", e)),
        }
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        // SAFETY: the semaphore is destroyed once, after every submit that may signal it
        // has finished (the device is idle).
        unsafe {
            let _ = self.raw.device_wait_idle();
            self.raw.destroy_semaphore(self.timeline, None);
        }
    }
}

/// An encode target's DMA-BUF: one object holding both NV12 planes.
#[derive(Debug)]
pub(crate) struct Nv12DmaBuf {
    fd: OwnedFd,
    /// The object's size in bytes.
    pub(crate) size: u64,
    pub(crate) modifier: u64,
    /// Byte offsets of the Y and the CbCr plane.
    pub(crate) offsets: [u32; 2],
    /// Bytes between rows of the Y and the CbCr plane.
    pub(crate) pitches: [u32; 2],
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Nv12DmaBuf {
    /// The descriptor, which the target owns; VA-API takes it without closing it.
    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// An encode target's [`FrameSync`]: staging signals the context's timeline after the
/// submit that writes the target, and VA-API waits for that value.
#[derive(Debug)]
pub(crate) struct TargetSync {
    shared: Arc<Shared>,
    dma_buf: Nv12DmaBuf,
    /// The timeline value the last staged write signals; 0 before the first.
    written: AtomicU64,
}

impl TargetSync {
    /// The target's DMA-BUF.
    pub(crate) fn dma_buf(&self) -> &Nv12DmaBuf {
        &self.dma_buf
    }

    /// Whether this target belongs to the context whose state is `shared`.
    pub(crate) fn is_on(&self, shared: &Arc<Shared>) -> bool {
        Arc::ptr_eq(&self.shared, shared)
    }

    /// Waits, for at most `timeout`, until the last staged write of the target is done.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when it isn't by then, or the driver fails.
    pub(crate) fn wait_written(&self, timeout: Duration) -> Result<(), PlatformError> {
        let value = self.written.load(Ordering::Acquire);
        let Some(export) = self.shared.export.as_ref() else {
            return Ok(());
        };
        if value == 0 {
            return Ok(());
        }
        export.wait(value, timeout)
    }
}

impl FrameSync for TargetSync {
    fn stage(&self, queue: &wgpu::Queue) -> Result<(), PlatformError> {
        if raw_queue(queue) != Some(self.shared.raw_queue) {
            return Err(PlatformError::InvalidState {
                what: "FrameSync::stage",
                state: "the queue belongs to another device than the encode target's",
            });
        }
        let export = self
            .shared
            .export
            .as_ref()
            .ok_or(PlatformError::InvalidState {
                what: "FrameSync::stage",
                state: "the context has no encode targets",
            })?;
        // SAFETY: the hal queue is only used to add a signal to its next submit; the
        // semaphore is a live timeline semaphore of the queue's device.
        let hal = unsafe { queue.as_hal::<Vulkan>() }
            .ok_or_else(|| os("wgpu::Queue::as_hal", "the queue is not Vulkan"))?;
        let value = export.last.fetch_add(1, Ordering::AcqRel) + 1;
        hal.add_signal_semaphore(export.timeline, Some(value));
        self.written.store(value, Ordering::Release);
        Ok(())
    }

    fn platform(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// The Vulkan objects behind one target, freed when the texture is dropped.
struct Owned {
    raw: ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: wgpu calls the texture's drop callback once nothing on the GPU uses
        // the image any more; each handle is freed once, here, image first.
        unsafe {
            self.raw.destroy_image(self.image, None);
            if self.memory != vk::DeviceMemory::null() {
                self.raw.free_memory(self.memory, None);
            }
        }
    }
}

/// The memory type to allocate a target in: device-local first.
fn memory_type(props: &vk::PhysicalDeviceMemoryProperties, bits: u32) -> Option<u32> {
    let types = props.memory_types_as_slice();
    let allowed = |i: usize| bits & (1 << i) != 0;
    let local = (0..types.len()).find(|&i| {
        allowed(i)
            && types[i]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
    });
    local
        .or_else(|| (0..types.len()).find(|&i| allowed(i)))
        .and_then(|i| u32::try_from(i).ok())
}

/// Allocates one encode target of `size` on the context whose state is `shared`.
///
/// # Errors
/// [`PlatformError::Unsupported`] when the context can't export or the size is odd,
/// and [`PlatformError::Os`] when the driver refuses.
pub(crate) fn target(shared: &Arc<Shared>, size: FrameSize) -> Result<GpuFrame, PlatformError> {
    let export = shared
        .export
        .as_ref()
        .ok_or_else(|| PlatformError::Unsupported {
            what: "GPU encode targets: this Vulkan device can't export NV12 DMA-BUFs for VA-API"
                .into(),
        })?;
    let (width, height) = (size.width(), size.height());
    if width % 2 != 0 || height % 2 != 0 {
        return Err(PlatformError::Unsupported {
            what: format!("an NV12 encode target at an odd size, {width}x{height}"),
        });
    }
    let raw = &export.raw;
    let modifiers = [MOD_LINEAR];
    let mut list =
        vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(&modifiers);
    let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(DMA_BUF);
    let info = vk::ImageCreateInfo::default()
        .flags(FLAGS)
        .image_type(vk::ImageType::TYPE_2D)
        .format(NV12)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(USAGE)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external)
        .push_next(&mut list);
    // SAFETY: a live device with the extensions `exportable` checked, and an
    // initialised create info whose chain outlives the call.
    let image = unsafe { raw.create_image(&info, None) }
        .map_err(|e| os("vkCreateImage (NV12 export)", e))?;
    let mut owned = Owned {
        raw: raw.clone(),
        image,
        memory: vk::DeviceMemory::null(),
    };
    // SAFETY: the image is live and has no memory yet.
    let needs = unsafe { raw.get_image_memory_requirements(image) };
    // SAFETY: the physical device belongs to the instance.
    let props = unsafe {
        export
            .instance
            .get_physical_device_memory_properties(export.physical)
    };
    let type_index = memory_type(&props, needs.memory_type_bits).ok_or_else(|| {
        os(
            "vkAllocateMemory (NV12 export)",
            "no memory type fits the image",
        )
    })?;
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let mut exported = vk::ExportMemoryAllocateInfo::default().handle_types(DMA_BUF);
    let alloc = vk::MemoryAllocateInfo::default()
        .allocation_size(needs.size)
        .memory_type_index(type_index)
        .push_next(&mut exported)
        .push_next(&mut dedicated);
    // SAFETY: an initialised allocate info, dedicated to the live image above.
    owned.memory = unsafe { raw.allocate_memory(&alloc, None) }
        .map_err(|e| os("vkAllocateMemory (NV12 export)", e))?;
    // SAFETY: the memory was allocated for this image, which has none bound yet.
    unsafe { raw.bind_image_memory(image, owned.memory, 0) }
        .map_err(|e| os("vkBindImageMemory (NV12 export)", e))?;
    let layout = |aspect| {
        let subresource = vk::ImageSubresource {
            aspect_mask: aspect,
            mip_level: 0,
            array_layer: 0,
        };
        // SAFETY: a live image in a DRM-modifier layout, whose memory planes are named
        // by the `MEMORY_PLANE_*` aspects; NV12 in one object has two.
        unsafe { raw.get_image_subresource_layout(image, subresource) }
    };
    let (y, uv) = (
        layout(vk::ImageAspectFlags::MEMORY_PLANE_0_EXT),
        layout(vk::ImageAspectFlags::MEMORY_PLANE_1_EXT),
    );
    let u32_of = |v: u64| {
        u32::try_from(v).map_err(|_| os("vkGetImageSubresourceLayout", "a plane past 4 GiB"))
    };
    let offsets = [u32_of(y.offset)?, u32_of(uv.offset)?];
    let pitches = [u32_of(y.row_pitch)?, u32_of(uv.row_pitch)?];
    let get = vk::MemoryGetFdInfoKHR::default()
        .memory(owned.memory)
        .handle_type(DMA_BUF);
    // SAFETY: the memory was allocated exportable as a DMA-BUF.
    let fd =
        unsafe { export.memory_fd.get_memory_fd(&get) }.map_err(|e| os("vkGetMemoryFdKHR", e))?;
    // SAFETY: vkGetMemoryFdKHR returns a new descriptor the caller owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let dma_buf = Nv12DmaBuf {
        fd,
        size: needs.size,
        modifier: MOD_LINEAR,
        offsets,
        pitches,
        width,
        height,
    };

    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let label = "jstream encode target (NV12, DMA-BUF)";
    let hal_texture = {
        // SAFETY: the hal device is only used to wrap the image below, while
        // `shared.device` keeps it alive.
        let hal = unsafe { shared.device.as_hal::<Vulkan>() }
            .ok_or_else(|| os("wgpu::Device::as_hal", "the device is not Vulkan"))?;
        let desc = wgpu::hal::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::NV12,
            usage: wgpu::TextureUses::COLOR_TARGET
                | wgpu::TextureUses::RESOURCE
                | wgpu::TextureUses::COPY_SRC,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let callback: wgpu::hal::DropCallback = Box::new(move || drop(owned));
        // SAFETY: the image was created on this device as `desc` describes (one 2D NV12
        // mip and sample of this size, with MUTABLE_FORMAT for the plane views and
        // render-target, sampled and copy usage); its memory is bound and owned by
        // `owned`, which the drop callback frees after the image.
        unsafe {
            hal.texture_from_raw(
                image,
                &desc,
                Some(callback),
                wgpu::hal::vulkan::TextureMemory::External,
            )
        }
    };
    // SAFETY: the hal texture was made on this device from the descriptor below; it is
    // new, so its contents may be discarded (UNDEFINED, `UNINITIALIZED`), and render
    // writes every texel of both planes before anything reads it.
    let texture = unsafe {
        shared.device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some(label),
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
            wgpu::TextureUses::UNINITIALIZED,
        )
    };
    let info = VideoFrameInfo {
        size,
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::zero(crate::clock::timebase()),
    };
    let sync = Arc::new(TargetSync {
        shared: shared.clone(),
        dma_buf,
        written: AtomicU64::new(0),
    });
    Ok(GpuFrame::new(texture, info, sync))
}

#[cfg(test)]
mod tests {
    use ash::vk;

    use super::{device_number, memory_type};

    /// glibc's `gnu_dev_makedev`.
    fn makedev(major: u64, minor: u64) -> u64 {
        (minor & 0xff) | ((major & 0xfff) << 8) | ((minor & !0xff) << 12) | ((major & !0xfff) << 32)
    }

    #[test]
    fn device_numbers_split_as_glibc_does() {
        // The first render node, 226:128.
        assert_eq!(makedev(226, 128), 0xE280);
        assert_eq!(device_number(0xE280), (226, 128));
        // Majors and minors past 12 and 8 bits use the high bits.
        for (major, minor) in [
            (0, 0),
            (226, 129),
            (0x1234, 0x56789),
            (0xFFFF_FFFF, 0xFFFF_FFFF),
        ] {
            assert_eq!(device_number(makedev(major, minor)), (major, minor));
        }
    }

    #[test]
    fn targets_prefer_device_local_memory() {
        let mut props = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 3,
            ..Default::default()
        };
        props.memory_types[0].property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE;
        props.memory_types[2].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        assert_eq!(memory_type(&props, 0b111), Some(2));
        assert_eq!(
            memory_type(&props, 0b011),
            Some(0),
            "else the first allowed"
        );
        assert_eq!(memory_type(&props, 0), None);
    }
}
