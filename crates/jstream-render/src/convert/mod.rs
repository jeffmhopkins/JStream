//! Composite to NV12 on the GPU (ARCH › GPU pipeline step 4): two render passes read
//! the compositor's straight-alpha sRGB output and write a Y plane (`R8Unorm`) and an
//! interleaved CbCr plane (`Rg8Unorm`, half size). The planes are either two textures
//! this module allocates ([`Nv12Planes`], the path every adapter has) or the plane views
//! of a native `NV12` texture ([`Nv12Target::nv12_texture`], such as the platform's
//! encode target).

use jstream_media::{ColorInfo, ColorRange, ColorSpace, FrameSize};

use crate::error::RenderError;

/// Y plane format, and the format a native NV12 texture's plane 0 is viewed as.
const Y_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// CbCr plane format, and the format of a native NV12 texture's plane 1 view.
const UV_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;

/// Builds [`Nv12Pass`]es on one device. Holds the two pipelines, so make it once.
pub struct Nv12Converter {
    device: wgpu::Device,
    layout: wgpu::BindGroupLayout,
    y_pipeline: wgpu::RenderPipeline,
    uv_pipeline: wgpu::RenderPipeline,
    max_texture_dimension_2d: u32,
}

impl Nv12Converter {
    /// Compiles the conversion pipelines on `device`, which must be the device the
    /// sources and targets are on (the compositor's, [`crate::Compositor::device`]).
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render nv12"),
            source: wgpu::ShaderSource::Wgsl(include_str!("nv12.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render nv12"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render nv12"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            device: device.clone(),
            y_pipeline: pipeline("fs_y", Y_FORMAT),
            uv_pipeline: pipeline("fs_uv", UV_FORMAT),
            layout,
            max_texture_dimension_2d: device.limits().max_texture_dimension_2d,
        }
    }

    /// Allocates a Y and a CbCr texture of `size` to convert into. Both are
    /// `RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC | COPY_DST`, so the result can be
    /// sampled, copied to or from another texture's planes, or read back with
    /// [`Nv12Planes::read`].
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for an odd width or height (NV12 has no half chroma
    /// sample), and [`RenderError::TooLarge`] beyond the adapter's 2D limit.
    pub fn planes(&self, size: FrameSize) -> Result<Nv12Planes, RenderError> {
        let (width, height) = (size.width(), size.height());
        check_size(width, height, self.max_texture_dimension_2d)?;
        let texture = |label, format, width, height| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        Ok(Nv12Planes {
            y: texture("jstream-render nv12 y", Y_FORMAT, width, height),
            uv: texture("jstream-render nv12 uv", UV_FORMAT, width / 2, height / 2),
            width,
            height,
        })
    }

    /// Binds `source` to `target` once, so converting a frame allocates nothing. Bind
    /// again when either texture is replaced (the compositor's output is recreated when
    /// the canvas size changes).
    ///
    /// `source` is the compositor's output: `Rgba8UnormSrgb` or `Bgra8UnormSrgb`, 2D,
    /// one mip, one sample, with `TEXTURE_BINDING`, the target's size. Its straight
    /// alpha is composited over black, since NV12 has no alpha. `color` must be BT.709
    /// (the engine's output space); limited and full range are both converted.
    ///
    /// # Errors
    /// [`RenderError::UnsupportedFormat`] or [`RenderError::UnsupportedTexture`] for a
    /// source this pass cannot read, [`RenderError::Invalid`] for a size mismatch or a
    /// colour space other than BT.709.
    pub fn bind(
        &self,
        source: &wgpu::Texture,
        target: &Nv12Target,
        color: ColorInfo,
    ) -> Result<Nv12Pass, RenderError> {
        check_source(source)?;
        if (source.width(), source.height()) != (target.width, target.height) {
            return Err(RenderError::Invalid {
                what: "nv12 target",
                detail: format!(
                    "the source is {}x{} but the target is {}x{}",
                    source.width(),
                    source.height(),
                    target.width,
                    target.height
                ),
            });
        }
        let params = params(color)?;
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            &self.device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render nv12 params"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render nv12"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        Ok(Nv12Pass {
            y_pipeline: self.y_pipeline.clone(),
            uv_pipeline: self.uv_pipeline.clone(),
            bind_group,
            y: target.y.clone(),
            uv: target.uv.clone(),
        })
    }

    /// Converts into a platform frame in a submit of its own, on `queue`: `pass`, bound
    /// to `frame`'s texture, then the texture handed back to the platform
    /// (`TextureUses::PRESENT`, as the compositor does with the source frames it
    /// samples), with `frame`'s sync staged around the submit. This is the encode
    /// tick's step: the platform's encoder reads the frame once its fence is signalled.
    /// Submit it after the composite that wrote the source.
    ///
    /// # Errors
    /// [`RenderError::Sync`] when staging the frame fails. Nothing is submitted then,
    /// though the platform may already have armed the queue's next submit.
    pub fn submit(
        &self,
        queue: &wgpu::Queue,
        pass: &Nv12Pass,
        frame: &jstream_platform::GpuFrame,
    ) -> Result<(), RenderError> {
        frame.stage(queue).map_err(RenderError::Sync)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render nv12"),
            });
        pass.encode(&mut encoder);
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: frame.texture(),
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

impl std::fmt::Debug for Nv12Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nv12Converter").finish_non_exhaustive()
    }
}

/// One source bound to one NV12 target. [`Self::encode`] records the conversion.
#[derive(Debug)]
pub struct Nv12Pass {
    y_pipeline: wgpu::RenderPipeline,
    uv_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    y: wgpu::TextureView,
    uv: wgpu::TextureView,
}

impl Nv12Pass {
    /// Records the Y pass and the CbCr pass into `encoder`. Every texel of both planes
    /// is written. Order it after the composite that
    /// writes the source, in the same or an earlier submit, and stage the target's
    /// `FrameSync` before the submit when it is a shared encode target.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        for (pipeline, view) in [(&self.y_pipeline, &self.y), (&self.uv_pipeline, &self.uv)] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render nv12"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Where a conversion writes: a Y plane view (`R8Unorm`) and a CbCr plane view
/// (`Rg8Unorm`, half the size on each side).
#[derive(Clone, Debug)]
pub struct Nv12Target {
    y: wgpu::TextureView,
    uv: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Nv12Target {
    /// The plane views of a native `NV12` texture, such as the platform's encode target
    /// (`GpuInterop::encode_target`). It needs `RENDER_ATTACHMENT`, which needs the
    /// device's `TEXTURE_FORMAT_NV12` feature.
    ///
    /// # Errors
    /// [`RenderError::UnsupportedFormat`] for another format, and
    /// [`RenderError::UnsupportedTexture`] for a texture that is not a single 2D render
    /// target.
    pub fn nv12_texture(texture: &wgpu::Texture) -> Result<Self, RenderError> {
        if texture.format() != wgpu::TextureFormat::NV12 {
            return Err(RenderError::UnsupportedFormat {
                format: texture.format(),
            });
        }
        check_shape(texture, "nv12 target")?;
        if !texture
            .usage()
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            return Err(RenderError::UnsupportedTexture {
                detail: "the nv12 target lacks RENDER_ATTACHMENT usage".into(),
            });
        }
        let plane = |aspect, format| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("jstream-render nv12 plane"),
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        Ok(Self {
            y: plane(wgpu::TextureAspect::Plane0, Y_FORMAT),
            uv: plane(wgpu::TextureAspect::Plane1, UV_FORMAT),
            width: texture.width(),
            height: texture.height(),
        })
    }

    /// Luma width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Luma height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// A Y texture and a CbCr texture from [`Nv12Converter::planes`]: the conversion target
/// on an adapter without NV12 render targets, and the CPU fallback's source.
#[derive(Debug)]
pub struct Nv12Planes {
    y: wgpu::Texture,
    uv: wgpu::Texture,
    width: u32,
    height: u32,
}

impl Nv12Planes {
    /// The target that writes these two textures.
    pub fn target(&self) -> Nv12Target {
        Nv12Target {
            y: self.y.create_view(&wgpu::TextureViewDescriptor::default()),
            uv: self.uv.create_view(&wgpu::TextureViewDescriptor::default()),
            width: self.width,
            height: self.height,
        }
    }

    /// The Y plane, `R8Unorm`, full size.
    pub fn y(&self) -> &wgpu::Texture {
        &self.y
    }

    /// The CbCr plane, `Rg8Unorm`, half size on each side.
    pub fn uv(&self) -> &wgpu::Texture {
        &self.uv
    }

    /// Copies both planes back as one tightly packed NV12 buffer: `width * height` Y
    /// bytes, then `width * height / 2` interleaved Cb, Cr bytes. This waits on the GPU,
    /// so it is the CPU fallback and test path, not the encode tick's.
    ///
    /// # Errors
    /// [`RenderError::Readback`] when a plane exceeds the device's buffer limit or the
    /// map fails or does not finish.
    pub fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Vec<u8>, RenderError> {
        let (w, h) = (self.width, self.height);
        let y_row = padded(w);
        let uv_row = padded(w);
        let y_bytes = u64::from(y_row) * u64::from(h);
        let uv_bytes = u64::from(uv_row) * u64::from(h / 2);
        let max = device.limits().max_buffer_size;
        if y_bytes.max(uv_bytes) > max {
            return Err(RenderError::Readback {
                detail: format!("a {w}x{h} nv12 plane exceeds the {max}-byte buffer limit"),
            });
        }
        let buffer = |size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("jstream-render nv12 readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };
        let (y_buf, uv_buf) = (buffer(y_bytes), buffer(uv_bytes));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render nv12 readback"),
        });
        for (texture, buf, row, rows) in [
            (&self.y, &y_buf, y_row, h),
            (&self.uv, &uv_buf, uv_row, h / 2),
        ] {
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(rows),
                    },
                },
                texture.size(),
            );
        }
        queue.submit([encoder.finish()]);
        let mut out = Vec::with_capacity(w as usize * h as usize * 3 / 2);
        map_rows(
            device,
            &y_buf,
            y_row as usize,
            w as usize,
            h as usize,
            &mut out,
        )?;
        map_rows(
            device,
            &uv_buf,
            uv_row as usize,
            w as usize,
            h as usize / 2,
            &mut out,
        )?;
        Ok(out)
    }
}

/// Four `f32`s: Y scale and offset, then chroma scale and offset.
const PARAMS_BYTES: u64 = 16;

/// The shader's range constants, in 8-bit code values (ITU-T H.273's quantisation).
fn params(color: ColorInfo) -> Result<[f32; 4], RenderError> {
    if color.space != ColorSpace::Bt709 {
        return Err(RenderError::Invalid {
            what: "nv12 color",
            detail: format!(
                "{:?} is not converted; the composite is BT.709",
                color.space
            ),
        });
    }
    Ok(match color.range {
        ColorRange::Limited => [219.0, 16.0, 224.0, 128.0],
        ColorRange::Full => [255.0, 0.0, 255.0, 128.0],
    })
}

fn check_size(width: u32, height: u32, limit: u32) -> Result<(), RenderError> {
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(RenderError::Invalid {
            what: "nv12 target",
            detail: format!("{width}x{height} is odd; NV12 needs even dimensions"),
        });
    }
    if width > limit || height > limit {
        return Err(RenderError::TooLarge {
            what: "nv12 target",
            width,
            height,
            limit,
        });
    }
    Ok(())
}

fn check_source(source: &wgpu::Texture) -> Result<(), RenderError> {
    match source.format() {
        wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Bgra8UnormSrgb => {}
        format => return Err(RenderError::UnsupportedFormat { format }),
    }
    check_shape(source, "nv12 source")?;
    if !source
        .usage()
        .contains(wgpu::TextureUsages::TEXTURE_BINDING)
    {
        return Err(RenderError::UnsupportedTexture {
            detail: "the nv12 source lacks TEXTURE_BINDING usage".into(),
        });
    }
    if !source.width().is_multiple_of(2) || !source.height().is_multiple_of(2) {
        return Err(RenderError::Invalid {
            what: "nv12 source",
            detail: format!(
                "{}x{} is odd; NV12 needs even dimensions",
                source.width(),
                source.height()
            ),
        });
    }
    Ok(())
}

fn check_shape(texture: &wgpu::Texture, what: &str) -> Result<(), RenderError> {
    if texture.dimension() != wgpu::TextureDimension::D2
        || texture.depth_or_array_layers() != 1
        || texture.mip_level_count() != 1
        || texture.sample_count() != 1
    {
        return Err(RenderError::UnsupportedTexture {
            detail: format!("the {what} must be 2D with one layer, one mip and one sample"),
        });
    }
    Ok(())
}

/// Bytes per row padded to wgpu's copy alignment. A Y row and a CbCr row are both
/// `width` bytes.
fn padded(width: u32) -> u32 {
    width.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

fn map_rows(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    stride: usize,
    row: usize,
    rows: usize,
    out: &mut Vec<u8>,
) -> Result<(), RenderError> {
    let (send, recv) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Readback {
            detail: e.to_string(),
        })?;
    recv.recv_timeout(std::time::Duration::from_secs(30))
        .map_err(|_| RenderError::Readback {
            detail: "the nv12 readback map did not finish within 30s".into(),
        })?
        .map_err(|e| RenderError::Readback {
            detail: e.to_string(),
        })?;
    {
        let view = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| RenderError::Readback {
                detail: e.to_string(),
            })?;
        for line in view.chunks(stride).take(rows) {
            out.extend_from_slice(&line[..row]);
        }
    }
    buffer.unmap();
    Ok(())
}

#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;
