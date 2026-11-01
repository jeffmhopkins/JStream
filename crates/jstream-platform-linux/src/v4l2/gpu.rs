//! The GPU path of a V4L2 capture (`V4l2Devices::on_context`, ARCH › GPU pipeline): the
//! driver's buffers are exported as DMA-BUFs (`VIDIOC_EXPBUF`) once the stream is on,
//! each imported once into the context's Vulkan device in the `LINEAR` layout V4L2
//! buffers have ([`dmabuf::import`]), and each filled buffer is copied on the context's
//! queue into a texture of the stream's own ([`blit::Ring`]), which is the frame render
//! samples.
//!
//! The capture thread waits for that copy before it hands the buffer back to the driver
//! (`VIDIOC_QBUF`), so the driver never overwrites a frame the GPU is still reading, and
//! render never holds a driver buffer. A copy that hasn't finished after [`COPY_WAIT`]
//! ends the stream, and the backend on that context opens nothing more on the GPU path
//! ([`GpuPath::failed`]), so the engine opens the device again on the CPU path.
//!
//! The 32-bit RGB formats are imported as `Bgra8Unorm` or `Rgba8Unorm` and copied as they
//! are, made opaque ([`GpuLayout`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jstream_platform::media::MediaTime;
use jstream_platform::{GpuFrame, PlatformError};

use super::device::Streaming;
use super::format::{GpuLayout, plane_bytes};
use crate::gpu::blit::{self, Ring};
use crate::gpu::{self, dmabuf};

/// How long the capture thread waits for a frame's GPU copy before it gives up the GPU
/// path.
pub(crate) const COPY_WAIT: Duration = Duration::from_secs(1);

/// The backend's GPU path: the context, and whether a stream on it failed.
#[derive(Clone, Debug)]
pub(crate) struct GpuPath {
    pub(crate) shared: Arc<gpu::Shared>,
    /// Set when a stream's import or copy failed: the backend on this context then opens
    /// nothing more on it.
    pub(crate) failed: Arc<AtomicBool>,
}

impl GpuPath {
    /// The context's path, when its device imports any [`GpuLayout`]'s planes.
    pub(crate) fn new(shared: Arc<gpu::Shared>) -> Option<Self> {
        let path = Self {
            shared,
            failed: Arc::default(),
        };
        [GpuLayout::Bgra, GpuLayout::Rgba]
            .into_iter()
            .any(|layout| path.imports(layout))
            .then_some(path)
    }

    /// Whether the device imports `layout`'s planes.
    pub(crate) fn imports(&self, layout: GpuLayout) -> bool {
        self.shared.linear.contains(&texture_format(layout))
    }
}

/// The format `layout`'s buffer plane is imported as.
pub(crate) fn texture_format(layout: GpuLayout) -> wgpu::TextureFormat {
    match layout {
        GpuLayout::Bgra => wgpu::TextureFormat::Bgra8Unorm,
        GpuLayout::Rgba => wgpu::TextureFormat::Rgba8Unorm,
    }
}

/// One stream's GPU delivery: its copy ring and each driver buffer, imported.
pub(crate) struct GpuDeliver {
    shared: Arc<gpu::Shared>,
    failed: Arc<AtomicBool>,
    ring: Ring,
    layout: GpuLayout,
    /// The frame's size in pixels.
    size: (u32, u32),
    /// The first plane's row stride in bytes.
    stride: usize,
    /// Each buffer's imported plane, by buffer index.
    buffers: Vec<wgpu::Texture>,
}

impl std::fmt::Debug for GpuDeliver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDeliver")
            .field("layout", &self.layout)
            .field("size", &self.size)
            .field("buffers", &self.buffers.len())
            .finish_non_exhaustive()
    }
}

impl GpuDeliver {
    /// Exports and imports every buffer of `streaming`, frames of `size` in `layout`, rows
    /// `stride` bytes apart.
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
        stride: usize,
    ) -> Result<Self, PlatformError> {
        if !path.imports(layout) {
            return Err(PlatformError::Unsupported {
                what: format!("importing {layout:?} V4L2 buffers on this device"),
            });
        }
        let row_stride = u32::try_from(stride).map_err(|_| PlatformError::Unsupported {
            what: format!("a V4L2 stride of {stride} bytes"),
        })?;
        let mut buffers = Vec::new();
        for (index, planes) in streaming.buffer_planes().into_iter().enumerate() {
            if planes != 1 {
                return Err(PlatformError::Unsupported {
                    what: format!("a {layout:?} V4L2 buffer of {planes} planes"),
                });
            }
            let index = u32::try_from(index).map_err(|_| PlatformError::Unsupported {
                what: "more V4L2 buffers than a u32 counts".into(),
            })?;
            let fd = streaming.export(index, 0)?;
            buffers.push(dmabuf::import(
                &path.shared.device,
                texture_format(layout),
                size,
                dmabuf::MOD_LINEAR,
                dmabuf::Plane {
                    fd: std::os::fd::AsFd::as_fd(&fd),
                    offset: 0,
                    stride: row_stride,
                },
            )?);
            // The import keeps a duplicate; this one closes here.
        }
        Ok(Self::with_buffers(path, layout, size, stride, buffers))
    }

    /// The delivery of buffers already imported as `buffers`, by index.
    pub(crate) fn with_buffers(
        path: &GpuPath,
        layout: GpuLayout,
        size: (u32, u32),
        stride: usize,
        buffers: Vec<wgpu::Texture>,
    ) -> Self {
        Self {
            shared: Arc::clone(&path.shared),
            failed: Arc::clone(&path.failed),
            ring: Ring::new(Arc::clone(&path.shared)),
            layout,
            size,
            stride,
            buffers,
        }
    }

    /// Copies filled buffer `index` (whose planes' bytes are `planes`, each starting
    /// `offsets[i]` into its memory) into the ring and waits for the copy, so the buffer
    /// can go back to the driver. `Ok(None)` drops the frame: a short buffer, or a ring
    /// whose every texture a frame still holds.
    ///
    /// # Errors
    /// [`PlatformError::DeviceLost`] when the buffer can't be read on the GPU (an index
    /// the driver didn't give, a plane that doesn't start at its memory's start) or its
    /// copy fails or doesn't finish in [`COPY_WAIT`]. The GPU path is then given up on
    /// this context, and the stream ends.
    pub(crate) fn frame(
        &mut self,
        index: u32,
        planes: &[&[u8]],
        offsets: &[u32],
        pts: MediaTime,
    ) -> Result<Option<GpuFrame>, PlatformError> {
        let (width, height) = self.size;
        let row_bytes = width as usize * 4;
        let needed = plane_bytes(self.stride, row_bytes, height);
        match (planes.first(), needed) {
            (Some(plane), Some(needed)) if plane.len() >= needed => {}
            _ => return Ok(None),
        }
        let result = (|| {
            if offsets.iter().any(|&o| o != 0) {
                return Err(format!("a buffer whose data starts at {offsets:?}"));
            }
            let texture = self
                .buffers
                .get(index as usize)
                .ok_or_else(|| format!("buffer {index}, which wasn't exported"))?;
            let source = blit::Source {
                texture,
                visible: (0, 0, width, height),
                padded: true,
            };
            let Some((frame, submission)) = self
                .ring
                .copy(source, pts)
                .map_err(|e| format!("copying a frame: {e}"))?
            else {
                return Ok(None);
            };
            self.shared
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(COPY_WAIT),
                })
                .map_err(|e| format!("the GPU copy of a frame didn't finish: {e}"))?;
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

    use jstream_platform::media::{MediaTime, PixelFormat};
    use jstream_platform::{GpuInterop, PlatformError};

    use super::{GpuDeliver, GpuPath, texture_format};
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

    /// A 2x1 texture of `layout`'s format whose two texels are `texels`, as a driver
    /// buffer imported on the GPU path would be.
    fn buffer(
        context: &jstream_platform::GpuContext,
        layout: GpuLayout,
        texels: [[u8; 4]; 2],
    ) -> wgpu::Texture {
        let (device, queue) = (context.device(), context.queue());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 2,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format(layout),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            texels.as_flattened(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: None,
            },
            texture.size(),
        );
        texture
    }

    /// The frame texture's texels, read back.
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
            texture.size(),
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

    /// On a software adapter: each filled buffer's texels reach the frame as RGBA, made
    /// opaque, from the buffer the driver names; a short buffer is dropped; and a buffer
    /// that can't be read ends the GPU path.
    #[test]
    fn filled_buffers_become_opaque_rgba_frames() {
        let Some(context) = context() else { return };
        let path = path(&context);
        let pts = MediaTime::new(7, timebase());
        for (layout, first) in [
            // B, G, R, A in memory reads as R = 3.
            (GpuLayout::Bgra, [[1, 2, 3, 9], [4, 5, 6, 9]]),
            (GpuLayout::Rgba, [[3, 2, 1, 9], [6, 5, 4, 9]]),
        ] {
            let second = [[10, 20, 30, 0], [40, 50, 60, 0]];
            let buffers = vec![
                buffer(&context, layout, first),
                buffer(&context, layout, second),
            ];
            let mut deliver = GpuDeliver::with_buffers(&path, layout, (2, 1), 8, buffers);
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
                GpuLayout::Rgba => [10, 20, 30, 255, 40, 50, 60, 255],
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
        let buffers = vec![buffer(&context, GpuLayout::Rgba, [[0; 4]; 2])];
        let mut deliver = GpuDeliver::with_buffers(&path, GpuLayout::Rgba, (2, 1), 8, buffers);
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
            for layout in [GpuLayout::Bgra, GpuLayout::Rgba] {
                assert_eq!(
                    path.imports(layout),
                    shared.linear.contains(&texture_format(layout))
                );
            }
        }
    }
}
