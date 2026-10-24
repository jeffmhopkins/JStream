//! Source images: an uploaded RGBA8 buffer, or a texture already on this device.
//!
//! The composite pass samples with a linear filter. Filtering straight-alpha texels blends
//! the colour of fully transparent neighbours into an edge, which darkens it (a fringe), so
//! uploaded bytes are premultiplied once, on upload, in linear light: sRGB decode, then
//! RGB times alpha, stored as `Rgba16Float`. Imported textures are sampled as they are:
//! straight sRGB (a captured frame), or premultiplied sRGB code values (a browser page),
//! which the shader converts to premultiplied linear after the filter.

use std::sync::Arc;

use jstream_media::FrameSize;
use jstream_platform::{FrameSync, GpuFrame};

use crate::color::srgb_channel_to_linear;
use crate::error::RenderError;
use crate::gpu::Gpu;

/// The format uploaded images are stored in: premultiplied linear light.
pub(crate) const UPLOAD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The format of a composited scene placed as an image: the composite pass's own,
/// premultiplied linear light, the same as an upload.
pub(crate) const LAYER_FORMAT: wgpu::TextureFormat = UPLOAD_FORMAT;

/// How the composite shader reads an image's samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alpha {
    /// Premultiplied linear (uploads and rendered images). Filtering is exact at transparent edges.
    Premultiplied,
    /// Straight sRGB (imports). The shader premultiplies after the filter, which is exact
    /// where alpha is 1, as in a captured frame, and fringes at partly transparent edges.
    Straight,
    /// sRGB code values already multiplied by alpha (imports of a browser page, CEF's
    /// layout), sampled as raw `Unorm`. Filtering premultiplied texels keeps transparent
    /// edges clean; the shader then divides by alpha, decodes sRGB and multiplies by
    /// alpha again in linear light.
    PremultipliedSrgb,
    /// Opaque sRGB (imports of a captured frame), decoded by the sampler with alpha read
    /// as 1 whatever the texture holds, as the CPU capture path ignores it (#451).
    Opaque,
}

impl Alpha {
    /// The shaders' number for this mode: how they turn a filtered sample into
    /// premultiplied linear light.
    pub(crate) fn shader_mode(self) -> f32 {
        match self {
            Alpha::Premultiplied => 0.0,
            Alpha::Straight => 1.0,
            Alpha::PremultipliedSrgb => 2.0,
            Alpha::Opaque => 3.0,
        }
    }
}

/// Which [`crate::Compositor`] made an image. The image keeps a clone, so the allocation
/// the identity compares by cannot be reused while the image is alive.
#[derive(Clone, Debug, Default)]
pub(crate) struct Owner(Arc<()>);

impl Owner {
    pub(crate) fn is(&self, other: &Owner) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Everything an image needs from the compositor that makes it.
pub(crate) struct Maker<'a> {
    pub gpu: &'a Gpu,
    pub layout: &'a wgpu::BindGroupLayout,
    pub sampler: &'a wgpu::Sampler,
    pub owner: &'a Owner,
}

/// A sampled image on the compositor's device.
///
/// The bind group is created once, with the compositor's linear sampler, so a frame that
/// redraws the same images does not allocate one. An image only draws in the
/// [`crate::Compositor`] that made it; another one refuses it with
/// [`RenderError::Invalid`].
///
/// A clone shares the same texture, so a filter can read a layer the caller keeps (a
/// scene that a `scene_mask` reads); the texture lives until its last clone is dropped.
#[derive(Clone)]
pub struct Image {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    /// The size it is placed at ([`Self::extent`]).
    extent: (u32, u32),
    alpha: Alpha,
    owner: Owner,
    sync: Option<Arc<dyn FrameSync>>,
    /// The render-target view of a layer, kept so drawing into it again allocates no
    /// view. `None` for uploads and imports, which are never drawn into.
    target: Option<wgpu::TextureView>,
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

    /// The size the compositor places it at, in source pixels: what an item's crop and a
    /// legacy transform's scale count in, and whose aspect ratio a fit mode keeps. The
    /// texture is sampled across it. It is the image's own size unless
    /// [`Self::set_extent`] gave another.
    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }

    /// Places the image as if it were `extent`, for an image drawn with more pixels than
    /// the size it stands for: a source drawn sharp for an item that shows it enlarged
    /// (ADR-0071, docs/adr/0071-sources-drawn-at-their-shown-size.md).
    pub fn set_extent(&mut self, extent: FrameSize) {
        self.extent = (extent.width(), extent.height());
    }

    /// The sampled texture. For an uploaded or rendered image it is `Rgba16Float` holding
    /// premultiplied linear light; for an imported one it is the texture that was given.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn alpha(&self) -> Alpha {
        self.alpha
    }

    pub(crate) fn owner(&self) -> &Owner {
        &self.owner
    }

    pub(crate) fn sync(&self) -> Option<&Arc<dyn FrameSync>> {
        self.sync.as_ref()
    }

    /// Replaces the hold staged on each frame that draws the image. The cache keeps an
    /// import across frames and attaches each frame's hold only while that frame uses it.
    pub(crate) fn set_sync(&mut self, sync: Option<Arc<dyn FrameSync>>) {
        self.sync = sync;
    }

    /// Whether this is an import ([`import`]) read with `alpha`.
    pub(crate) fn is_import_as(&self, alpha: Alpha) -> bool {
        alpha != Alpha::Premultiplied && self.alpha == alpha
    }

    /// The view a layer is drawn into; `None` unless the image is a layer.
    pub(crate) fn target(&self) -> Option<&wgpu::TextureView> {
        self.target.as_ref()
    }

    /// Whether this is an upload, which [`rewrite`] can refill in place.
    pub(crate) fn is_upload(&self) -> bool {
        self.alpha == Alpha::Premultiplied
            && self.target.is_none()
            && self.texture.format() == UPLOAD_FORMAT
            && self.texture.usage().contains(wgpu::TextureUsages::COPY_DST)
    }
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("extent", &self.extent)
            .field("format", &self.texture.format())
            .field("alpha", &self.alpha)
            .field("synced", &self.sync.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) fn upload(
    maker: &Maker<'_>,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Image, RenderError> {
    check_bytes(width, height, rgba)?;
    check_dimension(maker.gpu, width, height)?;
    let texture = maker.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-render-image"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: UPLOAD_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    write_texels(maker.gpu, &texture, width, height, rgba)?;
    Ok(finish(
        maker,
        texture,
        width,
        height,
        Alpha::Premultiplied,
        None,
    ))
}

/// Uploads `texels` (RGBA, row-major, `width * height` of them) as `Rgba16Float` exactly
/// as given, each channel clamped to `0..=1`, and samples them as premultiplied. For data
/// that is not colour, such as a colour lookup table (`filter::assets`).
pub(crate) fn upload_unit_f16(
    maker: &Maker<'_>,
    width: u32,
    height: u32,
    texels: &[[f32; 4]],
) -> Result<Image, RenderError> {
    if width == 0 || height == 0 || texels.len() as u64 != u64::from(width) * u64::from(height) {
        return Err(RenderError::Invalid {
            what: "image",
            detail: format!("{} texels for a {width}x{height} image", texels.len()),
        });
    }
    check_dimension(maker.gpu, width, height)?;
    let texture = maker.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-render-data"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: UPLOAD_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = texels
        .iter()
        .flatten()
        .flat_map(|v| f16_bits(v.clamp(0.0, 1.0)).to_le_bytes())
        .collect();
    maker.gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 8),
            rows_per_image: Some(height),
        },
        extent(width, height),
    );
    Ok(finish(
        maker,
        texture,
        width,
        height,
        Alpha::Premultiplied,
        None,
    ))
}

/// Refills an uploaded `image` with new bytes of the same size, in place. The write is
/// ordered after every submit already on the queue, so frames that sampled the old
/// contents still see them.
pub(crate) fn rewrite(maker: &Maker<'_>, image: &Image, rgba: &[u8]) -> Result<(), RenderError> {
    if !image.owner.is(maker.owner) || !image.is_upload() {
        return Err(RenderError::Invalid {
            what: "image",
            detail: "only an upload made by this compositor can be rewritten".into(),
        });
    }
    check_bytes(image.width, image.height, rgba)?;
    write_texels(maker.gpu, &image.texture, image.width, image.height, rgba)
}

fn check_bytes(width: u32, height: u32, rgba: &[u8]) -> Result<(), RenderError> {
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
    Ok(())
}

fn write_texels(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), RenderError> {
    let row_bytes = width.checked_mul(8).ok_or(RenderError::TooLarge {
        what: "image",
        width,
        height,
        limit: gpu.max_texture_dimension_2d,
    })?;
    let texels = premultiply_to_f16(rgba);
    // `write_texture`, unlike a buffer-to-texture copy, does not need 256-byte rows.
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_bytes),
            rows_per_image: Some(height),
        },
        extent(width, height),
    );
    Ok(())
}

/// An import of `texture` read with `alpha`, `Straight`, `Opaque` or `PremultipliedSrgb`.
/// A straight or opaque import must be `Rgba8UnormSrgb` or `Bgra8UnormSrgb`, so sampling
/// decodes it; a premultiplied one `Rgba8Unorm` or `Bgra8Unorm`, so sampling returns the
/// code values the shader unpremultiplies before decoding.
pub(crate) fn import(
    maker: &Maker<'_>,
    texture: wgpu::Texture,
    sync: Option<Arc<dyn FrameSync>>,
    alpha: Alpha,
) -> Result<Image, RenderError> {
    use wgpu::TextureFormat as F;
    let format = texture.format();
    let accepted = match alpha {
        Alpha::Straight | Alpha::Opaque => {
            matches!(format, F::Rgba8UnormSrgb | F::Bgra8UnormSrgb)
        }
        Alpha::PremultipliedSrgb => matches!(format, F::Rgba8Unorm | F::Bgra8Unorm),
        Alpha::Premultiplied => false,
    };
    if !accepted {
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
    check_dimension(maker.gpu, width, height)?;
    Ok(finish(maker, texture, width, height, alpha, sync))
}

pub(crate) fn import_frame(maker: &Maker<'_>, frame: &GpuFrame) -> Result<Image, RenderError> {
    import(
        maker,
        frame.texture().clone(),
        Some(Arc::clone(frame.sync())),
        Alpha::Straight,
    )
}

/// A new layer: a texture [`crate::Compositor::render_into`] composites into, placed as
/// an image. Its contents are undefined until the first composite.
pub(crate) fn layer(maker: &Maker<'_>, width: u32, height: u32) -> Result<Image, RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::Invalid {
            what: "layer",
            detail: format!("size is {width}x{height}; both dimensions must be non-zero"),
        });
    }
    check_dimension(maker.gpu, width, height)?;
    let texture = maker.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-render-layer"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: LAYER_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut image = finish(maker, texture, width, height, Alpha::Premultiplied, None);
    image.target = Some(target);
    Ok(image)
}

fn finish(
    maker: &Maker<'_>,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    alpha: Alpha,
    sync: Option<Arc<dyn FrameSync>>,
) -> Image {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = maker
        .gpu
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-image"),
            layout: maker.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(maker.sampler),
                },
            ],
        });
    Image {
        texture,
        bind_group,
        width,
        height,
        extent: (width, height),
        alpha,
        owner: maker.owner.clone(),
        sync,
        target: None,
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

/// Straight sRGB RGBA8 to premultiplied linear `Rgba16Float` texels, little-endian.
///
/// RGB is decoded with the sRGB transfer, then multiplied by alpha. Alpha is linear in the
/// source and stays so. A fully transparent texel becomes all zero whatever its RGB was.
fn premultiply_to_f16(rgba: &[u8]) -> Vec<u8> {
    let linear: [f32; 256] = std::array::from_fn(|i| srgb_channel_to_linear(i as f32 / 255.0));
    let mut out = Vec::with_capacity(rgba.len() * 2);
    for px in rgba.chunks_exact(4) {
        let a = f32::from(px[3]) / 255.0;
        for value in [
            linear[usize::from(px[0])] * a,
            linear[usize::from(px[1])] * a,
            linear[usize::from(px[2])] * a,
            a,
        ] {
            out.extend_from_slice(&f16_bits(value).to_le_bytes());
        }
    }
    out
}

/// IEEE 754 binary16 bits of `value`, rounded to nearest, ties to even.
///
/// Only for the values [`premultiply_to_f16`] produces: finite and in `0..=1`, so there is
/// no sign, overflow, infinity or NaN to handle.
fn f16_bits(value: f32) -> u16 {
    debug_assert!((0.0..=1.0).contains(&value), "{value} is outside 0..=1");
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    let (full, shift) = if exponent >= 1 {
        // Normal in binary16: rebias the exponent and drop 13 mantissa bits.
        ((exponent as u32) << 23 | mantissa, 13)
    } else if exponent >= -10 {
        // Subnormal in binary16: the implicit leading 1 becomes a mantissa bit.
        (mantissa | 0x80_0000, (14 - exponent) as u32)
    } else {
        // Below half the smallest subnormal (2^-25), including zero: rounds to zero.
        return 0;
    };
    let kept = full >> shift;
    let rest = full & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    // A carry out of the mantissa moves into the exponent, which is the correct result.
    let rounded = if rest > half || (rest == half && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    };
    rounded as u16
}

#[cfg(test)]
mod tests {
    use super::{Owner, f16_bits, premultiply_to_f16};

    /// binary16 bits to f32, written independently of [`f16_bits`].
    fn decode(bits: u16) -> f32 {
        let exponent = i32::from((bits >> 10) & 0x1f);
        let mantissa = f32::from(bits & 0x3ff);
        if exponent == 0 {
            mantissa * 2f32.powi(-24)
        } else {
            (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
        }
    }

    fn texel(bytes: &[u8], channel: usize) -> f32 {
        decode(u16::from_le_bytes([
            bytes[channel * 2],
            bytes[channel * 2 + 1],
        ]))
    }

    #[test]
    fn f16_known_values() {
        assert_eq!(f16_bits(0.0), 0x0000);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(0.25), 0x3400);
        // Smallest normal and smallest subnormal.
        assert_eq!(f16_bits(2f32.powi(-14)), 0x0400);
        assert_eq!(f16_bits(2f32.powi(-24)), 0x0001);
        // Half of the smallest subnormal ties to even (zero); just above rounds up.
        assert_eq!(f16_bits(2f32.powi(-25)), 0x0000);
        assert_eq!(f16_bits(2f32.powi(-25) * 1.5), 0x0001);
        // 1 - 2^-12 is halfway between 0x3bff (odd) and 1.0; ties to even goes up.
        assert_eq!(f16_bits(1.0 - 2f32.powi(-12)), 0x3c00);
        // 0.5 + 2^-12 is halfway between 0x3800 (even) and 0x3801; it stays down.
        assert_eq!(f16_bits(0.5 + 2f32.powi(-12)), 0x3800);
    }

    #[test]
    fn f16_round_trips_within_half_an_ulp() {
        // Every value an upload can produce: linear(r/255) * a/255, and alpha itself.
        for r in 0..=255u16 {
            let linear = super::srgb_channel_to_linear(f32::from(r) / 255.0);
            for a in 0..=255u16 {
                let value = linear * f32::from(a) / 255.0;
                let got = decode(f16_bits(value));
                // Half an ulp: 2^-11 relative for normals, 2^-25 absolute below them.
                let tolerance = (value * 2f32.powi(-11)).max(2f32.powi(-25));
                assert!((got - value).abs() <= tolerance, "{value} encoded as {got}");
            }
        }
    }

    #[test]
    fn transparent_texels_premultiply_to_zero_and_opaque_ones_keep_their_colour() {
        let bytes = premultiply_to_f16(&[255, 0, 0, 255, 255, 255, 255, 0]);
        assert_eq!(bytes.len(), 16);
        assert_eq!(
            [0, 1, 2, 3].map(|c| texel(&bytes[..8], c)),
            [1.0, 0.0, 0.0, 1.0]
        );
        assert!(bytes[8..].iter().all(|b| *b == 0), "{:?}", &bytes[8..]);
    }

    #[test]
    fn premultiplication_happens_after_the_srgb_decode() {
        // sRGB 188 is about 0.5029 linear; at alpha 128 (0.502) that is about 0.2524.
        let bytes = premultiply_to_f16(&[188, 188, 188, 128]);
        let a = 128.0 / 255.0;
        let expected = super::srgb_channel_to_linear(188.0 / 255.0) * a;
        assert!((texel(&bytes, 0) - expected).abs() < 1e-3);
        assert!((texel(&bytes, 3) - a).abs() < 1e-3);
    }

    #[test]
    fn owners_compare_by_identity() {
        let one = Owner::default();
        let other = Owner::default();
        assert!(one.is(&one.clone()));
        assert!(!one.is(&other));
    }
}
