//! Source images: an uploaded RGBA8 buffer, or a texture already on this device.

use std::sync::Arc;

use jstream_platform::{FrameSync, GpuFrame};

use crate::error::RenderError;
use crate::gpu::Gpu;

/// A sampled sRGB image on the compositor's device.
///
/// The bind group is created once, with the compositor's linear sampler, so a frame that
/// redraws the same images does not allocate one.
pub struct Image {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    sync: Option<Arc<dyn FrameSync>>,
}

impl Image {
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The sampled texture.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn sync(&self) -> Option<&Arc<dyn FrameSync>> {
        self.sync.as_ref()
    }
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.texture.format())
            .field("synced", &self.sync.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) fn upload(
    gpu: &Gpu,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Image, RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::Invalid {
            what: "image",
            detail: format!("size is {width}x{height}; both dimensions must be non-zero"),
        });
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(RenderError::ImageBytes {
            width,
            height,
            len: rgba.len(),
            expected: usize::MAX,
        })?;
    if rgba.len() != expected {
        return Err(RenderError::ImageBytes {
            width,
            height,
            len: rgba.len(),
            expected,
        });
    }
    check_dimension(gpu, width, height)?;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-render-image"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let padded = pad_rows(width, height, rgba);
    let stride = padded_stride(width);
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &padded,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: Some(height),
        },
        extent(width, height),
    );
    Ok(finish(texture, width, height, None, gpu, layout, sampler))
}

pub(crate) fn import(
    gpu: &Gpu,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture: wgpu::Texture,
    sync: Option<Arc<dyn FrameSync>>,
) -> Result<Image, RenderError> {
    let format = texture.format();
    if !matches!(
        format,
        wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        return Err(RenderError::UnsupportedFormat { format });
    }
    let width = texture.width();
    let height = texture.height();
    if texture.dimension() != wgpu::TextureDimension::D2
        || texture.sample_count() != 1
        || texture.size().depth_or_array_layers != 1
        || !texture
            .usage()
            .contains(wgpu::TextureUsages::TEXTURE_BINDING)
    {
        return Err(RenderError::UnsupportedTexture {
            detail: format!(
                "{width}x{height} {:?}, {} sample(s), usage {:?}",
                texture.dimension(),
                texture.sample_count(),
                texture.usage()
            ),
        });
    }
    check_dimension(gpu, width, height)?;
    Ok(finish(texture, width, height, sync, gpu, layout, sampler))
}

pub(crate) fn import_frame(
    gpu: &Gpu,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    frame: &GpuFrame,
) -> Result<Image, RenderError> {
    import(
        gpu,
        layout,
        sampler,
        frame.texture().clone(),
        Some(Arc::clone(frame.sync())),
    )
}

fn finish(
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    sync: Option<Arc<dyn FrameSync>>,
    gpu: &Gpu,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
) -> Image {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("jstream-render-image"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Image {
        texture,
        bind_group,
        width,
        height,
        sync,
    }
}

fn check_dimension(gpu: &Gpu, width: u32, height: u32) -> Result<(), RenderError> {
    let limit = gpu.max_texture_dimension_2d;
    if width > limit || height > limit {
        Err(RenderError::TooLarge {
            what: "image",
            width,
            height,
            limit,
        })
    } else {
        Ok(())
    }
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn padded_stride(width: u32) -> u32 {
    let row = width.saturating_mul(4);
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    row.div_ceil(align) * align
}

fn pad_rows(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = width as usize * 4;
    let stride = padded_stride(width) as usize;
    if stride == row {
        return rgba.to_vec();
    }
    let mut padded = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        let src = y * row;
        let dst = y * stride;
        padded[dst..dst + row].copy_from_slice(&rgba[src..src + row]);
    }
    padded
}
