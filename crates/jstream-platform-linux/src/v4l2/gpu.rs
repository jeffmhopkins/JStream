//! The GPU path of a V4L2 capture (`V4l2Devices::on_context`, ARCH › GPU pipeline): the
//! driver's buffers are exported as DMA-BUFs (`VIDIOC_EXPBUF`) once the stream is on,
//! each image plane imported once into the context's Vulkan device in the `LINEAR`
//! layout V4L2 buffers have ([`dmabuf::import`]), and each filled buffer is drawn on the
//! context's queue into a texture of the stream's own ([`blit::Ring`]), which is the frame
//! render samples.
//!
//! The capture thread waits for that draw before it hands the buffer back to the driver
//! (`VIDIOC_QBUF`), so the driver never overwrites a frame the GPU is still reading, and
//! render never holds a driver buffer. A draw that hasn't finished after [`COPY_WAIT`]
//! ends the stream, and the backend on that context opens nothing more on the GPU path
//! ([`GpuPath::failed`]), so the engine opens the device again on the CPU path.
//!
//! How each [`GpuLayout`] is imported and drawn ([`planes`]): the 32-bit RGB formats as
//! `Bgra8Unorm` or `Rgba8Unorm`, copied as they are and made opaque; YUYV as
//! `Rgba8Unorm` at half the width (each texel `Y0 U Y1 V`, two pixels), converted to
//! R'G'B' with the frame's matrix and range ([`blit::Pass::Yuyv`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jstream_platform::media::{ColorInfo, MediaTime};
use jstream_platform::{GpuFrame, PlatformError};

use super::device::Streaming;
use super::format::{GpuLayout, plane_bytes};
use crate::gpu::blit::{self, Pass, Ring};
use crate::gpu::{self, dmabuf};

/// How long the capture thread waits for a frame's GPU draw before it gives up the GPU
/// path.
pub(crate) const COPY_WAIT: Duration = Duration::from_secs(1);

/// The layouts the GPU path draws.
const LAYOUTS: [GpuLayout; 3] = [GpuLayout::Bgra, GpuLayout::Rgba, GpuLayout::Yuyv];

/// The backend's GPU path: the context, and whether a stream on it failed.
#[derive(Clone, Debug)]
pub(crate) struct GpuPath {
    pub(crate) shared: Arc<gpu::Shared>,
    /// Set when a stream's draw failed: the backend on this context then opens nothing
    /// more on it.
    pub(crate) failed: Arc<AtomicBool>,
}

impl GpuPath {
    /// The context's path, when its device imports any [`GpuLayout`]'s planes.
    pub(crate) fn new(shared: Arc<gpu::Shared>) -> Option<Self> {
        let path = Self {
            shared,
            failed: Arc::default(),
        };
        LAYOUTS
            .into_iter()
            .any(|layout| path.imports(layout))
            .then_some(path)
    }

    /// Whether the device imports every one of `layout`'s planes.
    pub(crate) fn imports(&self, layout: GpuLayout) -> bool {
        plane_formats(layout)
            .iter()
            .all(|format| self.shared.linear.contains(format))
    }
}

/// The formats `layout`'s image planes are imported as, in order.
fn plane_formats(layout: GpuLayout) -> &'static [wgpu::TextureFormat] {
    match layout {
        GpuLayout::Bgra => &[wgpu::TextureFormat::Bgra8Unorm],
        GpuLayout::Rgba | GpuLayout::Yuyv => &[wgpu::TextureFormat::Rgba8Unorm],
    }
}

/// The pass that draws `layout`'s planes into the frame.
fn pass(layout: GpuLayout) -> Pass {
    match layout {
        GpuLayout::Bgra | GpuLayout::Rgba => Pass::Copy,
        GpuLayout::Yuyv => Pass::Yuyv,
    }
}

/// One image plane of a buffer, as it is imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaneImport {
    pub(crate) format: wgpu::TextureFormat,
    /// Its size in texels.
    pub(crate) size: (u32, u32),
    /// The buffer plane it is in.
    pub(crate) buffer_plane: u32,
    /// Where its first row starts in that buffer plane.
    pub(crate) offset: u32,
    /// Bytes between its rows.
    pub(crate) stride: u32,
}

impl PlaneImport {
    /// The fewest bytes its buffer plane holds for it: its offset, then its rows.
    fn end(&self) -> Option<usize> {
        let texel = self.format.block_copy_size(None)? as usize;
        let row_bytes = (self.size.0 as usize).checked_mul(texel)?;
        plane_bytes(self.stride as usize, row_bytes, self.size.1)?.checked_add(self.offset as usize)
    }
}

/// How `layout`'s frames of `size` pixels, each buffer plane's rows `strides[i]` bytes
/// apart, are imported: one entry per image plane. `None` for a stride or size the import
/// can't describe, or another number of buffer planes than the layout has.
pub(crate) fn planes(
    layout: GpuLayout,
    (width, height): (u32, u32),
    strides: &[usize],
) -> Option<Vec<PlaneImport>> {
    let stride = |i: usize| strides.get(i).and_then(|&s| u32::try_from(s).ok());
    let one = |format, size| {
        (strides.len() == 1).then_some(())?;
        Some(vec![PlaneImport {
            format,
            size,
            buffer_plane: 0,
            offset: 0,
            stride: stride(0)?,
        }])
    };
    let format = plane_formats(layout)[0];
    match layout {
        GpuLayout::Bgra | GpuLayout::Rgba => one(format, (width, height)),
        GpuLayout::Yuyv => one(format, (width.div_ceil(2), height)),
    }
}

/// One stream's GPU delivery: its ring and each driver buffer's planes, imported.
pub(crate) struct GpuDeliver {
    shared: Arc<gpu::Shared>,
    failed: Arc<AtomicBool>,
    ring: Ring,
    layout: GpuLayout,
    /// The frame's size in pixels.
    size: (u32, u32),
    /// The frame's colour, for the YCbCr layouts.
    color: ColorInfo,
    planes: Vec<PlaneImport>,
    /// Each buffer's imported planes, by buffer index.
    buffers: Vec<Vec<wgpu::Texture>>,
}

impl std::fmt::Debug for GpuDeliver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDeliver")
            .field("layout", &self.layout)
            .field("size", &self.size)
            .field("planes", &self.planes)
            .field("buffers", &self.buffers.len())
            .finish_non_exhaustive()
    }
}

impl GpuDeliver {
    /// Exports and imports every buffer of `streaming`, frames of `size` in `layout` and
    /// `color`, each buffer plane's rows `strides[i]` bytes apart.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the device doesn't import the layout or a
    /// buffer has another plane count, and the export's or import's error. The caller
    /// then opens the device on the CPU path.
    pub(crate) fn import(
        path: &GpuPath,
        streaming: &Streaming,
        layout: GpuLayout,
        size: (u32, u32),
        color: ColorInfo,
        strides: &[usize],
    ) -> Result<Self, PlatformError> {
        let unsupported = |what: String| PlatformError::Unsupported { what };
        if !path.imports(layout) {
            return Err(unsupported(format!(
                "importing {layout:?} V4L2 buffers on this device"
            )));
        }
        let planes = planes(layout, size, strides).ok_or_else(|| {
            unsupported(format!("{layout:?} V4L2 buffers with strides {strides:?}"))
        })?;
        let mut buffers = Vec::new();
        for (index, count) in streaming.buffer_planes().into_iter().enumerate() {
            if count != strides.len() {
                return Err(unsupported(format!(
                    "a {layout:?} V4L2 buffer of {count} planes"
                )));
            }
            let index = u32::try_from(index)
                .map_err(|_| unsupported("more V4L2 buffers than a u32 counts".into()))?;
            let fds = (0..count)
                .map(|plane| streaming.export(index, plane as u32))
                .collect::<Result<Vec<_>, _>>()?;
            let imported = planes
                .iter()
                .map(|plane| {
                    dmabuf::import(
                        &path.shared.device,
                        plane.format,
                        plane.size,
                        dmabuf::MOD_LINEAR,
                        dmabuf::Plane {
                            fd: std::os::fd::AsFd::as_fd(&fds[plane.buffer_plane as usize]),
                            offset: plane.offset,
                            stride: plane.stride,
                        },
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            buffers.push(imported);
            // The imports keep duplicates; these descriptors close here.
        }
        Ok(Self::with_buffers(
            path, layout, size, color, planes, buffers,
        ))
    }

    /// The delivery of buffers already imported as `buffers` (by buffer index, then
    /// image plane), laid out as `planes` says.
    pub(crate) fn with_buffers(
        path: &GpuPath,
        layout: GpuLayout,
        size: (u32, u32),
        color: ColorInfo,
        planes: Vec<PlaneImport>,
        buffers: Vec<Vec<wgpu::Texture>>,
    ) -> Self {
        Self {
            shared: Arc::clone(&path.shared),
            failed: Arc::clone(&path.failed),
            ring: Ring::with_pass(Arc::clone(&path.shared), pass(layout)),
            layout,
            size,
            color,
            planes,
            buffers,
        }
    }

    /// Draws filled buffer `index` (whose buffer planes' bytes are `bytes`, each starting
    /// `offsets[i]` into its memory) into the ring and waits for the draw, so the buffer
    /// can go back to the driver. `Ok(None)` drops the frame: a buffer plane shorter than
    /// its image planes need, or a ring whose every texture a frame still holds.
    ///
    /// # Errors
    /// [`PlatformError::DeviceLost`] when the buffer can't be read on the GPU (an index
    /// the driver didn't give, a plane that doesn't start at its memory's start) or its
    /// draw fails or doesn't finish in [`COPY_WAIT`]. The GPU path is then given up on
    /// this context, and the stream ends.
    pub(crate) fn frame(
        &mut self,
        index: u32,
        bytes: &[&[u8]],
        offsets: &[u32],
        pts: MediaTime,
    ) -> Result<Option<GpuFrame>, PlatformError> {
        let short = self.planes.iter().any(|plane| {
            let have = bytes.get(plane.buffer_plane as usize).map(|b| b.len());
            plane
                .end()
                .is_none_or(|end| have.is_none_or(|have| have < end))
        });
        if short {
            return Ok(None);
        }
        let result = (|| {
            if offsets.iter().any(|&o| o != 0) {
                return Err(format!("a buffer whose data starts at {offsets:?}"));
            }
            let textures = self
                .buffers
                .get(index as usize)
                .ok_or_else(|| format!("buffer {index}, which wasn't exported"))?;
            let (width, height) = self.size;
            let drawn = match pass(self.layout) {
                Pass::Copy => self.ring.copy(
                    blit::Source {
                        texture: &textures[0],
                        visible: (0, 0, width, height),
                        padded: true,
                    },
                    pts,
                ),
                _ => {
                    let planes: Vec<&wgpu::Texture> = textures.iter().collect();
                    self.ring.convert(&planes, self.size, self.color, pts)
                }
            };
            let Some((frame, submission)) = drawn.map_err(|e| format!("drawing a frame: {e}"))?
            else {
                return Ok(None);
            };
            self.shared
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(COPY_WAIT),
                })
                .map_err(|e| format!("the GPU draw of a frame didn't finish: {e}"))?;
            Ok(Some(frame))
        })();
        result.map_err(|reason| {
            self.failed.store(true, Ordering::Release);
            PlatformError::DeviceLost {
                what: "the video capture device's GPU path".into(),
                reason,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use jstream_platform::media::{ColorInfo, ColorRange, ColorSpace, MediaTime, PixelFormat};
    use jstream_platform::{GpuInterop, PlatformError};

    use super::{GpuDeliver, GpuPath, PlaneImport, plane_formats, planes};
    use crate::gpu::{LinuxGpu, Shared};
    use crate::timebase;
    use crate::v4l2::format::GpuLayout;

    /// A software context, or `None` on a machine without one.
    fn context() -> Option<jstream_platform::GpuContext> {
        match LinuxGpu::with_software().open_device() {
            Ok(context) => Some(context),
            Err(PlatformError::Unsupported { .. }) => None,
            Err(e) => panic!("{e}"),
        }
    }

    /// A texture of `format` and `size` holding `bytes`, tightly packed, as a driver
    /// buffer's plane imported on the GPU path would be.
    fn plane(
        context: &jstream_platform::GpuContext,
        format: wgpu::TextureFormat,
        (width, height): (u32, u32),
        bytes: &[u8],
    ) -> wgpu::Texture {
        let (device, queue) = (context.device(), context.queue());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texel = format.block_copy_size(None).expect("a plain format");
        queue.write_texture(
            texture.as_image_copy(),
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * texel),
                rows_per_image: None,
            },
            texture.size(),
        );
        texture
    }

    /// The frame texture's first row, read back.
    fn read(context: &jstream_platform::GpuContext, texture: &wgpu::Texture) -> Vec<u8> {
        let (device, queue) = (context.device(), context.queue());
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: texture.width(),
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("maps"));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .expect("the readback completes");
        let data = buffer.slice(..).get_mapped_range().expect("mapped");
        data[..texture.width() as usize * 4].to_vec()
    }

    fn path(context: &jstream_platform::GpuContext) -> GpuPath {
        GpuPath {
            shared: Shared::of(context, "test").expect("ours"),
            failed: Arc::default(),
        }
    }

    /// A delivery of `layout` frames of `size`, rows `strides` apart, whose buffers'
    /// image planes hold `buffers`' bytes.
    fn deliver(
        context: &jstream_platform::GpuContext,
        path: &GpuPath,
        layout: GpuLayout,
        size: (u32, u32),
        color: ColorInfo,
        strides: &[usize],
        buffers: &[&[&[u8]]],
    ) -> GpuDeliver {
        let planes = planes(layout, size, strides).expect("a layout");
        let textures = buffers
            .iter()
            .map(|bytes| {
                planes
                    .iter()
                    .zip(bytes.iter())
                    .map(|(p, b)| plane(context, p.format, p.size, b))
                    .collect()
            })
            .collect();
        GpuDeliver::with_buffers(path, layout, size, color, planes, textures)
    }

    /// An image plane at the start of buffer plane 0.
    fn first(format: wgpu::TextureFormat, size: (u32, u32), stride: u32) -> PlaneImport {
        PlaneImport {
            format,
            size,
            buffer_plane: 0,
            offset: 0,
            stride,
        }
    }

    /// The import of each layout: RGB at the frame's size, YUYV at half its width
    /// (rounded up), at the driver's stride; another plane count or an unrepresentable
    /// stride is refused.
    #[test]
    fn each_layout_imports_its_planes() {
        use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm};
        assert_eq!(
            planes(GpuLayout::Bgra, (640, 480), &[2560]),
            Some(vec![first(Bgra8Unorm, (640, 480), 2560)])
        );
        assert_eq!(
            planes(GpuLayout::Rgba, (640, 480), &[2560]),
            Some(vec![first(Rgba8Unorm, (640, 480), 2560)])
        );
        assert_eq!(
            planes(GpuLayout::Yuyv, (641, 480), &[1284]),
            Some(vec![first(Rgba8Unorm, (321, 480), 1284)])
        );
        assert_eq!(planes(GpuLayout::Yuyv, (640, 480), &[1280, 1280]), None);
        assert_eq!(planes(GpuLayout::Rgba, (640, 480), &[usize::MAX]), None);
        assert_eq!(plane_formats(GpuLayout::Yuyv), &[Rgba8Unorm]);
        assert_eq!(first(Rgba8Unorm, (2, 3), 16).end(), Some(40));
    }

    /// On a software adapter: each filled buffer's texels reach the frame as RGBA, made
    /// opaque, from the buffer the driver names; a short buffer is dropped; and a buffer
    /// that can't be read ends the GPU path.
    #[test]
    fn filled_buffers_become_opaque_rgba_frames() {
        let Some(context) = context() else { return };
        let path = path(&context);
        let pts = MediaTime::new(7, timebase());
        let rgb = ColorInfo::default();
        for (layout, first) in [
            // B, G, R, A in memory reads as R = 3.
            (GpuLayout::Bgra, [1, 2, 3, 9, 4, 5, 6, 9]),
            (GpuLayout::Rgba, [3, 2, 1, 9, 6, 5, 4, 9]),
        ] {
            let second = [10, 20, 30, 0, 40, 50, 60, 0];
            let mut deliver = deliver(
                &context,
                &path,
                layout,
                (2, 1),
                rgb,
                &[8],
                &[&[&first], &[&second]],
            );
            let bytes = [0u8; 8];
            let frame = deliver
                .frame(0, &[&bytes], &[0], pts)
                .expect("copies")
                .expect("a frame");
            assert_eq!(frame.info().format, PixelFormat::Rgba8);
            assert_eq!(frame.info().pts, pts);
            assert_eq!(
                read(&context, frame.texture()),
                [3, 2, 1, 255, 6, 5, 4, 255],
                "{layout:?}"
            );
            let frame = deliver
                .frame(1, &[&bytes], &[0], pts)
                .expect("copies")
                .expect("a frame");
            let expected = match layout {
                GpuLayout::Bgra => [30, 20, 10, 255, 60, 50, 40, 255],
                _ => [10, 20, 30, 255, 40, 50, 60, 255],
            };
            assert_eq!(read(&context, frame.texture()), expected, "{layout:?}");
            assert!(
                deliver
                    .frame(0, &[&bytes[..7]], &[0], pts)
                    .expect("dropped")
                    .is_none(),
                "short"
            );
            assert!(!path.failed.load(Ordering::Acquire));
        }
        let mut deliver = deliver(
            &context,
            &path,
            GpuLayout::Rgba,
            (2, 1),
            rgb,
            &[8],
            &[&[&[0; 8]]],
        );
        let bytes = [0u8; 8];
        for (index, offset) in [(1, 0), (0, 64)] {
            assert!(matches!(
                deliver.frame(index, &[&bytes], &[offset], pts),
                Err(PlatformError::DeviceLost { .. })
            ));
        }
        assert!(
            path.failed.load(Ordering::Acquire),
            "the GPU path is given up"
        );
    }

    /// R'G'B' code values for one YCbCr sample, as the engine's CPU path converts NV12
    /// (`jstream-core`'s `rgba_from_surface`), in floating point.
    fn reference(color: ColorInfo, y: u8, cb: u8, cr: u8) -> [u8; 4] {
        let (kr, kb) = match color.space {
            ColorSpace::Bt601 => (0.299, 0.114),
            ColorSpace::Bt2020 => (0.2627, 0.0593),
            _ => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (l, c) = match color.range {
            ColorRange::Full => (f64::from(y), 1.0),
            ColorRange::Limited => ((f64::from(y) - 16.0) * 255.0 / 219.0, 255.0 / 224.0),
        };
        let (u, v) = ((f64::from(cb) - 128.0) * c, (f64::from(cr) - 128.0) * c);
        let ch = |x: f64| x.round().clamp(0.0, 255.0) as u8;
        [
            ch(l + 2.0 * (1.0 - kr) * v),
            ch(l - 2.0 * kb * (1.0 - kb) / kg * u - 2.0 * kr * (1.0 - kr) / kg * v),
            ch(l + 2.0 * (1.0 - kb) * u),
            255,
        ]
    }

    fn close(got: &[u8], expected: &[u8]) -> bool {
        got.len() == expected.len() && got.iter().zip(expected).all(|(a, b)| a.abs_diff(*b) <= 1)
    }

    /// On a software adapter: YUYV is converted to opaque R'G'B' as the CPU path converts
    /// it, each pixel its own luma and its pair's chroma, in the frame's matrix and range;
    /// an odd width's last pixel takes its texel's first luma.
    #[test]
    fn yuyv_is_converted_as_the_cpu_path_converts_it() {
        let Some(context) = context() else { return };
        let path = path(&context);
        let pts = MediaTime::new(0, timebase());
        // Two texels: white then black in one, a saturated colour pair in the other.
        let yuyv = [235, 128, 16, 128, 81, 90, 145, 240];
        for color in [
            ColorInfo {
                space: ColorSpace::Bt601,
                range: ColorRange::Limited,
            },
            ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            ColorInfo {
                space: ColorSpace::Bt2020,
                range: ColorRange::Limited,
            },
        ] {
            let mut deliver = deliver(
                &context,
                &path,
                GpuLayout::Yuyv,
                (4, 1),
                color,
                &[8],
                &[&[&yuyv]],
            );
            let frame = deliver
                .frame(0, &[&yuyv], &[0], pts)
                .expect("converts")
                .expect("a frame");
            assert_eq!(frame.info().format, PixelFormat::Rgba8);
            let expected: Vec<u8> = [
                reference(color, 235, 128, 128),
                reference(color, 16, 128, 128),
                reference(color, 81, 90, 240),
                reference(color, 145, 90, 240),
            ]
            .concat();
            let got = read(&context, frame.texture());
            assert!(close(&got, &expected), "{color:?}: {got:?} vs {expected:?}");
        }
        let limited = ColorInfo {
            space: ColorSpace::Bt601,
            range: ColorRange::Limited,
        };
        assert_eq!(reference(limited, 235, 128, 128), [255, 255, 255, 255]);
        assert_eq!(reference(limited, 16, 128, 128), [0, 0, 0, 255]);
        // Three pixels in two texels: the last takes the second texel's first luma.
        let mut odd = deliver(
            &context,
            &path,
            GpuLayout::Yuyv,
            (3, 1),
            limited,
            &[8],
            &[&[&yuyv]],
        );
        let frame = odd
            .frame(0, &[&yuyv], &[0], pts)
            .expect("converts")
            .expect("a frame");
        let got = read(&context, frame.texture());
        assert!(
            close(&got[8..12], &reference(limited, 81, 90, 240)),
            "{got:?}"
        );
        // A buffer short of the half-width row is dropped.
        assert!(
            odd.frame(0, &[&yuyv[..7]], &[0], pts)
                .expect("dropped")
                .is_none()
        );
    }

    /// A path is offered only by a device that imports some layout's planes.
    #[test]
    fn a_path_needs_linear_import() {
        let Some(context) = context() else { return };
        let shared = Shared::of(&context, "test").expect("ours");
        let imports = shared.linear.contains(&wgpu::TextureFormat::Bgra8Unorm)
            || shared.linear.contains(&wgpu::TextureFormat::Rgba8Unorm);
        let path = GpuPath::new(Arc::clone(&shared));
        assert_eq!(path.is_some(), imports, "{:?}", shared.linear);
        if let Some(path) = path {
            for layout in super::LAYOUTS {
                assert_eq!(
                    path.imports(layout),
                    plane_formats(layout)
                        .iter()
                        .all(|f| shared.linear.contains(f))
                );
            }
        }
    }
}
