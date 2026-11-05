//! CPU frames into uploaded images on the GPU (ADR-0117 §2): the bytes are copied into a
//! storage buffer as they are, and a compute pass writes premultiplied linear
//! `Rgba16Float` from them (`ingest.wgsl`). The calling thread copies bytes and records
//! one dispatch; it no longer decodes sRGB, premultiplies or converts NV12 per pixel.
//!
//! The arithmetic is the CPU conversions': the shader reads the sRGB decode and the
//! alpha from tables built here with the CPU's functions, and converts NV12 in the same
//! 16.16 fixed point. Only the rounding to half precision is the GPU's, so a texel can
//! differ from the CPU's by one step of `f16` (a GPU may round either way).

use jstream_media::{ColorInfo, ColorRange, ColorSpace, PixelFormat, VideoFrame};

use crate::color::srgb_channel_to_linear;
use crate::error::RenderError;
use crate::gpu::Gpu;

/// Words of header before the planes.
const HEADER_WORDS: usize = 16;
/// Header bytes.
const HEADER_BYTES: usize = HEADER_WORDS * 4;
/// The compute shader's workgroup side.
const GROUP: u32 = 8;

/// How the shader reads the planes (`ingest.wgsl`'s `mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Straight sRGB RGBA8, premultiplied by its alpha.
    Straight = 0,
    /// RGBA8 whose alpha is ignored.
    OpaqueRgba = 1,
    /// BGRA8 whose alpha is ignored.
    OpaqueBgra = 2,
    /// NV12, opaque.
    Nv12 = 3,
}

/// The planes of one CPU frame, as the shader reads them.
pub(crate) struct Planes<'a> {
    mode: Mode,
    width: u32,
    height: u32,
    plane0: &'a [u8],
    stride0: usize,
    plane1: &'a [u8],
    stride1: usize,
    color: ColorInfo,
}

impl<'a> Planes<'a> {
    /// Tightly packed straight-alpha sRGB RGBA8 (`width * height * 4` bytes, checked by
    /// the caller).
    pub(crate) fn straight(width: u32, height: u32, rgba: &'a [u8]) -> Self {
        Self {
            mode: Mode::Straight,
            width,
            height,
            plane0: rgba,
            stride0: width as usize * 4,
            plane1: &[],
            stride1: 0,
            color: ColorInfo::default(),
        }
    }

    /// A CPU video frame read as opaque: packed RGBA8 or BGRA8, or NV12 with its matrix
    /// and range. A frame's alpha means nothing for a capture, so every texel's alpha is
    /// 1, as the CPU path read it (#451).
    ///
    /// # Errors
    /// [`RenderError::UnsupportedTexture`] for another layout, and
    /// [`RenderError::Invalid`] for planes too short for the frame's size.
    pub(crate) fn opaque(frame: &'a VideoFrame) -> Result<Self, RenderError> {
        let info = frame.info();
        let (width, height) = (info.size.width(), info.size.height());
        let plane = |index: usize| {
            frame.plane(index).ok_or_else(|| RenderError::Invalid {
                what: "frame",
                detail: format!("{:?} frame has no plane {index}", info.format),
            })
        };
        let (mode, rows1) = match info.format {
            PixelFormat::Rgba8 => (Mode::OpaqueRgba, 0),
            PixelFormat::Bgra8 => (Mode::OpaqueBgra, 0),
            PixelFormat::Nv12 => (Mode::Nv12, height.div_ceil(2)),
            other => {
                return Err(RenderError::UnsupportedTexture {
                    detail: format!("a {other:?} CPU frame (RGBA8, BGRA8 or NV12 only)"),
                });
            }
        };
        let p0 = plane(0)?;
        let row0 = if mode == Mode::Nv12 {
            width as usize
        } else {
            width as usize * 4
        };
        check_plane(p0.data(), p0.stride(), row0, height)?;
        let (plane1, stride1) = if mode == Mode::Nv12 {
            let p1 = plane(1)?;
            check_plane(
                p1.data(),
                p1.stride(),
                width.div_ceil(2) as usize * 2,
                rows1,
            )?;
            (p1.data(), p1.stride())
        } else {
            (&[][..], 0)
        };
        Ok(Self {
            mode,
            width,
            height,
            plane0: p0.data(),
            stride0: p0.stride(),
            plane1,
            stride1,
            color: info.color,
        })
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// The header and planes, as written into the storage buffer.
    fn len(&self) -> usize {
        HEADER_BYTES + self.plane0.len() + self.plane1.len()
    }

    fn header(&self) -> Result<[u32; HEADER_WORDS], RenderError> {
        let word = |v: usize| {
            u32::try_from(v).map_err(|_| RenderError::TooLarge {
                what: "frame",
                width: self.width,
                height: self.height,
                limit: u32::MAX,
            })
        };
        let yuv = Yuv::new(self.color);
        let mut header = [0u32; HEADER_WORDS];
        header[..13].copy_from_slice(&[
            self.width,
            self.height,
            self.mode as u32,
            word(self.stride0)?,
            word(self.plane0.len())?,
            word(self.stride1)?,
            yuv.y_offset as u32,
            yuv.y_scale as u32,
            yuv.c_scale as u32,
            yuv.cr_r as u32,
            yuv.cb_g as u32,
            yuv.cr_g as u32,
            yuv.cb_b as u32,
        ]);
        Ok(header)
    }
}

fn check_plane(data: &[u8], stride: usize, row: usize, rows: u32) -> Result<(), RenderError> {
    let need = match rows {
        0 => Some(0),
        rows => (rows as usize - 1)
            .checked_mul(stride)
            .and_then(|n| n.checked_add(row)),
    }
    .filter(|_| stride >= row);
    match need {
        Some(need) if data.len() >= need => Ok(()),
        _ => Err(RenderError::Invalid {
            what: "frame",
            detail: format!(
                "a plane of {} bytes with stride {stride} is too short for {rows} rows of {row}",
                data.len()
            ),
        }),
    }
}

/// YCbCr to R'G'B' for one matrix and range in 16.16 fixed point: the integers the CPU
/// conversion used (`jstream-core`'s capture path), so the shader's bytes are its bytes.
struct Yuv {
    y_offset: i32,
    y_scale: i32,
    c_scale: i32,
    cr_r: i32,
    cb_g: i32,
    cr_g: i32,
    cb_b: i32,
}

impl Yuv {
    fn new(color: ColorInfo) -> Self {
        let (kr, kb) = match color.space {
            ColorSpace::Bt601 => (0.299, 0.114),
            ColorSpace::Bt2020 => (0.2627, 0.0593),
            _ => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let fixed = |v: f64| (v * 65_536.0).round() as i32;
        let (y_offset, y_scale, c_scale) = match color.range {
            ColorRange::Full => (0, fixed(1.0), fixed(1.0)),
            ColorRange::Limited => (16, fixed(255.0 / 219.0), fixed(255.0 / 224.0)),
        };
        Self {
            y_offset,
            y_scale,
            c_scale,
            cr_r: fixed(2.0 * (1.0 - kr)),
            cb_g: fixed(2.0 * kb * (1.0 - kb) / kg),
            cr_g: fixed(2.0 * kr * (1.0 - kr) / kg),
            cb_b: fixed(2.0 * (1.0 - kb)),
        }
    }
}

/// The pipeline and the buffers the conversions share, on one compositor's device.
pub(crate) struct Ingest {
    pipeline: wgpu::ComputePipeline,
    image_layout: wgpu::BindGroupLayout,
    source_layout: wgpu::BindGroupLayout,
    lut: wgpu::Buffer,
    /// The storage buffer each conversion writes its bytes into, and its bind group.
    /// It only grows: every write is ordered before the submit of its dispatch, so one
    /// buffer serves every conversion.
    source: Option<(wgpu::Buffer, wgpu::BindGroup)>,
}

impl Ingest {
    pub(crate) fn new(gpu: &Gpu) -> Result<Self, RenderError> {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render ingest"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ingest.wgsl").into()),
        });
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render ingest image"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: crate::image::UPLOAD_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            }],
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render ingest source"),
            entries: &[storage(0), storage(1)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render ingest"),
            bind_group_layouts: &[Some(&image_layout), Some(&source_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("jstream-render ingest"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let table: Vec<u8> = (0..256)
            .map(|i| srgb_channel_to_linear(i as f32 / 255.0))
            .chain((0..256).map(|a| a as f32 / 255.0))
            .flat_map(f32::to_le_bytes)
            .collect();
        let lut = mapped_buffer(gpu, "jstream-render ingest lut", &table)?;
        Ok(Self {
            pipeline,
            image_layout,
            source_layout,
            lut,
            source: None,
        })
    }

    /// The bind group that lets a conversion write into `texture`, an upload. Made once
    /// per upload and kept on its image.
    pub(crate) fn image_bind_group(&self, gpu: &Gpu, texture: &wgpu::Texture) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render ingest image"),
            layout: &self.image_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        })
    }

    /// Converts `planes` into the upload behind `image_bind`, `planes`' size, in a
    /// submit of its own: ordered after every submit already on the queue (so frames
    /// that sampled the old contents keep them) and before the next.
    pub(crate) fn convert(
        &mut self,
        gpu: &Gpu,
        image_bind: &wgpu::BindGroup,
        planes: &Planes<'_>,
    ) -> Result<(), RenderError> {
        let header = planes.header()?;
        let len = planes.len().next_multiple_of(4);
        // Checked before a buffer is made or its bind group cached: the shader binds the
        // whole buffer, so it must fit one storage binding as well as one buffer.
        if len as u64 > source_limit(gpu) {
            return Err(RenderError::TooLarge {
                what: "frame",
                width: planes.width,
                height: planes.height,
                limit: gpu.max_texture_dimension_2d,
            });
        }
        let (buffer, bind) = self.source(gpu, len as u64);
        // Written straight into the queue's staging memory: one copy of the bytes.
        let size = wgpu::BufferSize::new(len as u64).ok_or_else(|| RenderError::Invalid {
            what: "frame",
            detail: "a frame has no bytes".into(),
        })?;
        let mut view = gpu
            .queue
            .write_buffer_with(&buffer, 0, size)
            .ok_or_else(|| RenderError::Uncaptured {
                detail: "the frame's bytes could not be staged".into(),
            })?;
        let mut words = [0u8; HEADER_BYTES];
        for (bytes, word) in words.chunks_exact_mut(4).zip(header) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        let mut at = 0;
        for part in [&words[..], planes.plane0, planes.plane1] {
            view.slice(at..at + part.len()).copy_from_slice(part);
            at += part.len();
        }
        view.slice(at..len).copy_from_slice(&[0; 4][..len - at]);
        drop(view);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render ingest"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("jstream-render ingest"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, image_bind, &[]);
            pass.set_bind_group(1, &bind, &[]);
            pass.dispatch_workgroups(
                planes.width.div_ceil(GROUP),
                planes.height.div_ceil(GROUP),
                1,
            );
        }
        // The submit lock keeps another thread's staged platform fences off this submit,
        // as every submit on a platform context's queue does.
        let _submitting = gpu.submit.lock();
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    /// The source buffer, at least `len` bytes, and its bind group.
    fn source(&mut self, gpu: &Gpu, len: u64) -> (wgpu::Buffer, wgpu::BindGroup) {
        if let Some((buffer, bind)) = &self.source
            && buffer.size() >= len
        {
            return (buffer.clone(), bind.clone());
        }
        let size = len.next_power_of_two().min(source_limit(gpu)).max(len);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render ingest source"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render ingest source"),
            layout: &self.source_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.lut.as_entire_binding(),
                },
            ],
        });
        self.source = Some((buffer.clone(), bind.clone()));
        (buffer, bind)
    }
}

/// The most bytes the source buffer may hold: one buffer, bound whole.
fn source_limit(gpu: &Gpu) -> u64 {
    gpu.max_buffer_size.min(gpu.max_storage_binding)
}

/// A read-only storage buffer holding `bytes`. Not `create_buffer_init`, which panics
/// when the buffer can't be mapped, as on a lost device.
fn mapped_buffer(gpu: &Gpu, label: &str, bytes: &[u8]) -> Result<wgpu::Buffer, RenderError> {
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: true,
    });
    buffer
        .get_mapped_range_mut(..)
        .map_err(|err| RenderError::Uncaptured {
            detail: format!("mapping {label}: {err}"),
        })?
        .slice(..bytes.len())
        .copy_from_slice(bytes);
    buffer.unmap();
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use jstream_media::{
        ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, Timebase,
        VideoFrame, VideoFrameInfo,
    };

    use super::Yuv;
    use crate::image::premultiply_to_f16;
    use crate::tests::open;
    use crate::{Compositor, Image};

    /// The `Rgba16Float` texels of `image`, as little-endian halves, row by row.
    fn read_halves(compositor: &Compositor, image: &Image) -> Vec<u16> {
        let (width, height) = (image.width(), image.height());
        let row = width * 8;
        let stride =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let device = compositor.device();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ingest test readback"),
            size: u64::from(stride * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            image.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            image.texture().size(),
        );
        compositor.queue().submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("the readback maps");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the device polls");
        let view = buffer.slice(..).get_mapped_range().expect("mapped");
        let mut halves = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            let at = y * stride as usize;
            halves.extend(
                view[at..at + row as usize]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]])),
            );
        }
        halves
    }

    fn halves(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    /// Every half within one step of the CPU's: a GPU may round a float to half either
    /// way (ADR-0117 §2). Halves of non-negative values order as their bits do.
    fn assert_close(got: &[u16], want: &[u16], what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: length");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                g.abs_diff(*w) <= 1,
                "{what}: half {i} (texel {}, channel {}) is {g:#06x}, the CPU's {w:#06x}",
                i / 4,
                i % 4
            );
        }
    }

    /// Every channel byte at every alpha, premultiplied on the GPU, matches the CPU
    /// conversion it replaces, and a rewrite in place does too.
    #[test]
    fn straight_rgba_premultiplies_as_the_cpu_did() {
        let Some(opened) = open() else { return };
        let compositor = &opened.compositor;
        // Texel (c, a): each channel byte c at alpha a, with the channels told apart.
        let rgba: Vec<u8> = (0..=255u8)
            .flat_map(|a| (0..=255u8).flat_map(move |c| [c, 255 - c, c ^ 0x5a, a]))
            .collect();
        let image = compositor.upload_rgba8(256, 256, &rgba).expect("uploads");
        let want = halves(&premultiply_to_f16(&rgba));
        assert_close(&read_halves(compositor, &image), &want, "upload");
        let flipped: Vec<u8> = rgba.chunks_exact(4).rev().flatten().copied().collect();
        compositor
            .rewrite_rgba8(&image, &flipped)
            .expect("rewrites");
        let want = halves(&premultiply_to_f16(&flipped));
        assert_close(&read_halves(compositor, &image), &want, "rewrite");
    }

    fn frame(
        format: PixelFormat,
        width: u32,
        height: u32,
        planes: Vec<Plane>,
        color: ColorInfo,
    ) -> VideoFrame {
        let info = VideoFrameInfo {
            size: FrameSize::new(width, height).expect("a size"),
            format,
            color,
            pts: MediaTime::new(0, Timebase::new(1, 1000).expect("a timebase")),
        };
        VideoFrame::new(info, planes).expect("a frame")
    }

    /// Packed RGBA8 and BGRA8 with padded rows are read as opaque, whatever their alpha.
    #[test]
    fn packed_frames_upload_opaque() {
        let Some(opened) = open() else { return };
        let compositor = &opened.compositor;
        let (width, height, stride) = (5u32, 3u32, 24usize);
        let data: Vec<u8> = (0..stride * height as usize)
            .map(|i| (i * 37 % 256) as u8)
            .collect();
        let packed = |swap: bool| -> Vec<u8> {
            (0..height as usize)
                .flat_map(|y| {
                    let data = &data;
                    (0..width as usize).flat_map(move |x| {
                        let p = &data[y * stride + x * 4..][..4];
                        if swap {
                            [p[2], p[1], p[0], 255]
                        } else {
                            [p[0], p[1], p[2], 255]
                        }
                    })
                })
                .collect()
        };
        for (format, swap) in [(PixelFormat::Rgba8, false), (PixelFormat::Bgra8, true)] {
            let video = frame(
                format,
                width,
                height,
                vec![Plane::new(data.clone(), stride)],
                ColorInfo::default(),
            );
            let image = compositor.upload_frame(&video).expect("uploads");
            let want = halves(&premultiply_to_f16(&packed(swap)));
            assert_close(
                &read_halves(compositor, &image),
                &want,
                &format!("{format:?}"),
            );
        }
    }

    /// The CPU's NV12 conversion (`jstream-core`'s capture path), pixel by pixel.
    fn nv12_reference(yuv: &Yuv, luma: u8, cb: u8, cr: u8) -> [u8; 4] {
        let l = i64::from((i32::from(luma) - yuv.y_offset) * yuv.y_scale);
        let cb = (i64::from(i32::from(cb) - 128) * i64::from(yuv.c_scale)) >> 16;
        let cr = (i64::from(i32::from(cr) - 128) * i64::from(yuv.c_scale)) >> 16;
        let channel = |v: i64| ((v + (1 << 15)) >> 16).clamp(0, 255) as u8;
        [
            channel(l + cr * i64::from(yuv.cr_r)),
            channel(l - cb * i64::from(yuv.cb_g) - cr * i64::from(yuv.cr_g)),
            channel(l + cb * i64::from(yuv.cb_b)),
            255,
        ]
    }

    /// NV12 with padded rows and odd sizes converts to the bytes the CPU conversion gave,
    /// for each matrix and range, and a rewrite in place does too.
    #[test]
    fn nv12_converts_as_the_cpu_did() {
        let Some(opened) = open() else { return };
        let compositor = &opened.compositor;
        let colors = [
            ColorInfo::default(),
            ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Limited,
            },
            ColorInfo {
                space: ColorSpace::Bt601,
                range: ColorRange::Full,
            },
            ColorInfo {
                space: ColorSpace::Bt2020,
                range: ColorRange::Limited,
            },
        ];
        let mut image = None;
        for (round, color) in colors.into_iter().enumerate() {
            let (width, height) = (7u32, 5u32);
            let (w, h) = (width as usize, height as usize);
            let (luma_stride, chroma_stride) = (w + 3, w.div_ceil(2) * 2 + 2);
            let luma: Vec<u8> = (0..luma_stride * h)
                .map(|i| ((i * 53 + round * 11) % 256) as u8)
                .collect();
            let chroma: Vec<u8> = (0..chroma_stride * h.div_ceil(2))
                .map(|i| ((i * 97 + round * 29) % 256) as u8)
                .collect();
            let yuv = Yuv::new(color);
            let rgba: Vec<u8> = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .flat_map(|(x, y)| {
                    let c = (y / 2) * chroma_stride + (x / 2) * 2;
                    nv12_reference(&yuv, luma[y * luma_stride + x], chroma[c], chroma[c + 1])
                })
                .collect();
            let video = frame(
                PixelFormat::Nv12,
                width,
                height,
                vec![
                    Plane::new(luma, luma_stride),
                    Plane::new(chroma, chroma_stride),
                ],
                color,
            );
            let image = match &image {
                None => image.insert(compositor.upload_frame(&video).expect("uploads")),
                Some(image) => {
                    compositor.rewrite_frame(image, &video).expect("rewrites");
                    image
                }
            };
            let want = halves(&premultiply_to_f16(&rgba));
            assert_close(
                &read_halves(compositor, image),
                &want,
                &format!("{color:?}"),
            );
        }
    }

    /// A frame too large for one storage binding is refused before any buffer is made
    /// for it, and the next frame that fits still converts (#1236 B1).
    #[test]
    fn a_frame_past_the_binding_limit_is_refused_and_the_next_converts() {
        let Some(mut opened) = open() else { return };
        let compositor = &mut opened.compositor;
        compositor.cap_storage_binding(4096);
        let large = compositor.upload_rgba8(64, 64, &[255; 64 * 64 * 4]);
        assert!(
            matches!(large, Err(crate::RenderError::TooLarge { .. })),
            "{large:?}"
        );
        let small = compositor
            .upload_rgba8(8, 8, &[255; 8 * 8 * 4])
            .expect("a frame that fits converts");
        let halves = read_halves(compositor, &small);
        // Opaque white: 1.0 in every channel.
        assert!(halves.iter().all(|half| *half == 0x3c00), "{halves:?}");
    }

    /// A frame of another layout, or one whose planes are short, is refused.
    #[test]
    fn unreadable_frames_are_refused() {
        let Some(opened) = open() else { return };
        let compositor = &opened.compositor;
        let short = VideoFrame::new(
            VideoFrameInfo {
                size: FrameSize::new(4, 4).expect("a size"),
                format: PixelFormat::Rgba8,
                color: ColorInfo::default(),
                pts: MediaTime::new(0, Timebase::new(1, 1000).expect("a timebase")),
            },
            vec![Plane::new(vec![0; 64], 16)],
        );
        if let Ok(short) = short {
            let image = compositor.upload_rgba8(4, 4, &[0; 64]).expect("uploads");
            assert!(compositor.rewrite_frame(&image, &short).is_ok());
            let other = frame(
                PixelFormat::Rgba8,
                2,
                2,
                vec![Plane::new(vec![0; 16], 8)],
                ColorInfo::default(),
            );
            assert!(
                compositor.rewrite_frame(&image, &other).is_err(),
                "another size"
            );
        }
    }
}
