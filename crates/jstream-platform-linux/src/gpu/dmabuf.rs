//! DMA-BUF import into the context's Vulkan device (ADR-0060 §3): which DRM format
//! modifiers the device can import, and wrapping one PipeWire buffer's DMA-BUF as a
//! `wgpu::Texture`.
//!
//! Only single-plane layouts are offered and imported (wgpu-hal's
//! `texture_from_dmabuf_fd` takes one plane). That leaves out the modifiers with an
//! auxiliary compression plane (Intel CCS, AMD DCC with metadata), which the producer
//! then doesn't pick; linear and plain tiled layouts remain.
//!
//! The images are imported as UNORM, not sRGB: the copy into the frame texture
//! ([`super::blit`]) moves the bytes as they are.

use std::os::fd::BorrowedFd;

use ash::vk;
use jstream_platform::PlatformError;
use wgpu::hal::api::Vulkan;

use crate::spa::SpaVideoFormat;

/// The format PipeWire's bytes are read as: BGRx and BGRA are B, G, R, A in memory
/// (DRM's `XRGB8888` and `ARGB8888`), RGBx and RGBA are R, G, B, A (`XBGR8888`,
/// `ABGR8888`).
pub(crate) fn texture_format(format: SpaVideoFormat) -> wgpu::TextureFormat {
    match format {
        SpaVideoFormat::Bgrx | SpaVideoFormat::Bgra => wgpu::TextureFormat::Bgra8Unorm,
        SpaVideoFormat::Rgbx | SpaVideoFormat::Rgba => wgpu::TextureFormat::Rgba8Unorm,
    }
}

/// [`texture_format`] as Vulkan names it (wgpu-hal's mapping).
fn vk_format(format: SpaVideoFormat) -> vk::Format {
    match texture_format(format) {
        wgpu::TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        _ => vk::Format::R8G8B8A8_UNORM,
    }
}

/// For each format PipeWire may send, the DRM modifiers the adapter can import as a
/// single-plane DMA-BUF image to sample (in the driver's order). Empty lists, rather
/// than an error, when the adapter is not Vulkan 1.1 or reports none.
pub(crate) fn importable_modifiers(adapter: &wgpu::Adapter) -> Vec<(SpaVideoFormat, Vec<u64>)> {
    // SAFETY: the hal adapter is only used to read the raw instance and physical
    // device for the property queries below, while `adapter` keeps both alive.
    let Some(hal) = (unsafe { adapter.as_hal::<Vulkan>() }) else {
        return Vec::new();
    };
    let instance = hal.shared_instance();
    let version = hal.physical_device_capabilities().properties().api_version;
    // `vkGetPhysicalDevice*Properties2` are core in Vulkan 1.1, which ash loads them as.
    if instance.instance_api_version() < vk::API_VERSION_1_1 || version < vk::API_VERSION_1_1 {
        return Vec::new();
    }
    let raw = instance.raw_instance();
    let device = hal.raw_physical_device();
    SpaVideoFormat::OFFERED
        .iter()
        .map(|&format| (format, modifiers(raw, device, vk_format(format))))
        .collect()
}

/// The importable single-plane modifiers of `format` on `device`.
fn modifiers(instance: &ash::Instance, device: vk::PhysicalDevice, format: vk::Format) -> Vec<u64> {
    let mut count = vk::DrmFormatModifierPropertiesListEXT::default();
    {
        let mut props = vk::FormatProperties2::default().push_next(&mut count);
        // SAFETY: a valid physical device of `instance`, and a properties chain of
        // initialised structs; the first call only fills the count.
        unsafe { instance.get_physical_device_format_properties2(device, format, &mut props) };
    }
    let n = count.drm_format_modifier_count as usize;
    if n == 0 {
        return Vec::new();
    }
    let mut entries = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
    let filled = {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut entries);
        {
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            // SAFETY: as above; `list` points at `n` entries for the driver to fill,
            // and its count says so.
            unsafe { instance.get_physical_device_format_properties2(device, format, &mut props) };
        }
        list.drm_format_modifier_count as usize
    };
    entries.truncate(filled.min(n));
    entries
        .iter()
        .filter(|e| {
            e.drm_format_modifier_plane_count == 1
                && e.drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
        })
        .map(|e| e.drm_format_modifier)
        .filter(|&modifier| importable(instance, device, format, modifier))
        .collect()
}

/// Whether a sampled 2D image of `format` with `modifier` can be imported from a
/// DMA-BUF.
fn importable(
    instance: &ash::Instance,
    device: vk::PhysicalDevice,
    format: vk::Format,
    modifier: u64,
) -> bool {
    let mut drm = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
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
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
}

/// One PipeWire buffer's single DMA-BUF plane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plane<'fd> {
    /// The buffer's file descriptor, which PipeWire owns; the import duplicates it.
    pub(crate) fd: BorrowedFd<'fd>,
    /// Bytes from the DMA-BUF's start to the first row.
    pub(crate) offset: u32,
    /// Bytes between rows.
    pub(crate) stride: u32,
}

fn os(call: &'static str, message: impl ToString) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: message.to_string(),
    }
}

/// Imports `plane`, a DMA-BUF of `size` pixels in `format` laid out by `modifier`, as a
/// texture on `device` that shaders can read (`TEXTURE_BINDING`).
///
/// The texture keeps its own duplicate of the descriptor, so it stays valid however
/// long it is held; it shows whatever the producer last wrote into the buffer.
///
/// # Errors
/// [`PlatformError::Os`] when the descriptor can't be duplicated, `device` isn't Vulkan
/// with DMA-BUF import, or the driver refuses the layout.
pub(crate) fn import(
    device: &wgpu::Device,
    format: SpaVideoFormat,
    (width, height): (u32, u32),
    modifier: u64,
    plane: Plane<'_>,
) -> Result<wgpu::Texture, PlatformError> {
    let fd = plane.fd.try_clone_to_owned().map_err(|e| os("dup", e))?;
    let texture_format = texture_format(format);
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let label = "jstream capture (DMA-BUF)";
    let hal_texture = {
        // SAFETY: the hal device is only used for the import below, while `device`
        // keeps it alive.
        let hal = unsafe { device.as_hal::<Vulkan>() }
            .ok_or_else(|| os("wgpu::Device::as_hal", "the device is not Vulkan"))?;
        let desc = wgpu::hal::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format,
            usage: wgpu::TextureUses::RESOURCE,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        // SAFETY: the device was opened with VULKAN_EXTERNAL_MEMORY_DMA_BUF (only such a
        // context offers modifiers, `LinuxGpu::open_device`); `fd` is our own duplicate
        // of a DMA-BUF PipeWire negotiated for this format, size and modifier, with this
        // offset and stride, and the import takes it (or closes it on failure).
        unsafe {
            hal.texture_from_dmabuf_fd(
                fd,
                &desc,
                modifier,
                u64::from(plane.stride),
                u64::from(plane.offset),
            )
        }
        .map_err(|e| os("vkAllocateMemory (DMA-BUF import)", e))?
    };
    // SAFETY: the hal texture was created on this device from the descriptor below
    // (one 2D mip and sample, this format and size, sampled only). Its contents are
    // the producer's: it is declared in the GENERAL layout (wgpu's storage-read state),
    // the layout an image shared with another process is in, so the first sample
    // transitions it from there without discarding them.
    let texture = unsafe {
        device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: texture_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::STORAGE_READ_ONLY,
        )
    };
    Ok(texture)
}

#[cfg(test)]
mod tests {
    use super::{texture_format, vk_format};
    use crate::spa::SpaVideoFormat;

    #[test]
    fn formats_read_the_bytes_in_memory_order() {
        use ash::vk::Format;
        for (spa, wgpu, vk) in [
            (
                SpaVideoFormat::Bgrx,
                wgpu::TextureFormat::Bgra8Unorm,
                Format::B8G8R8A8_UNORM,
            ),
            (
                SpaVideoFormat::Bgra,
                wgpu::TextureFormat::Bgra8Unorm,
                Format::B8G8R8A8_UNORM,
            ),
            (
                SpaVideoFormat::Rgbx,
                wgpu::TextureFormat::Rgba8Unorm,
                Format::R8G8B8A8_UNORM,
            ),
            (
                SpaVideoFormat::Rgba,
                wgpu::TextureFormat::Rgba8Unorm,
                Format::R8G8B8A8_UNORM,
            ),
        ] {
            assert_eq!(texture_format(spa), wgpu);
            assert_eq!(vk_format(spa), vk);
        }
    }
}
