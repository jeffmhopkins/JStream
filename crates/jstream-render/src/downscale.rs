//! A frame the compositor drew, scaled down on the GPU to a smaller size.
//!
//! A consumer that wants the program at a size smaller than the one it is drawn at (a
//! preview stream of a smaller stage) takes the program's own frame scaled down instead
//! of a second composite of the scene at its size. One pass writes each target texel as
//! the exact box average of the source texels it covers (`downscale.wgsl`), in linear
//! light and premultiplied, so thin lines and small text keep their weight.
//!
//! The target is the compositor's output format, straight-alpha `Rgba8UnormSrgb` with
//! the same usages, so whatever reads an [`Output`] (a readback ring, the NV12
//! conversion) reads a scaled one the same way.

use jstream_media::FrameSize;

use crate::Compositor;
use crate::error::RenderError;
use crate::scene::Output;

/// The format of a scaled frame: the compositor's output's.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The pass's uniform: one `vec4<f32>`, the target's size.
const PARAMS_BYTES: u64 = 16;

/// Builds and runs the scaling pass on one device. Holds the pipeline, so make it once.
pub struct Downscaler {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl Downscaler {
    /// Compiles the pass on the compositor's device.
    pub fn new(compositor: &Compositor) -> Self {
        let device = compositor.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-downscale"),
            source: wgpu::ShaderSource::Wgsl(include_str!("downscale.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-downscale"),
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
            label: Some("jstream-render-downscale"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-downscale"),
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
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { layout, pipeline }
    }

    /// A target of `size` to scale frames into, kept and reused frame after frame.
    ///
    /// # Errors
    /// [`RenderError::TooLarge`] beyond the adapter's 2D limit, [`RenderError::Invalid`]
    /// for an empty size, or what the device reported while making it.
    pub fn target(
        &self,
        compositor: &Compositor,
        size: FrameSize,
    ) -> Result<Downscaled, RenderError> {
        let (width, height) = (size.width(), size.height());
        let limit = compositor.device().limits().max_texture_dimension_2d;
        if width == 0 || height == 0 {
            return Err(RenderError::Invalid {
                what: "downscale target",
                detail: format!("{width}x{height} is empty"),
            });
        }
        if width > limit || height > limit {
            return Err(RenderError::TooLarge {
                what: "downscale target",
                width,
                height,
                limit,
            });
        }
        compositor.scoped(|| {
            let device = compositor.device();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-render-downscaled"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                // As the compositor's output: drawn into, read back, and sampled by the
                // NV12 conversion.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("jstream-render-downscale"),
                size: PARAMS_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let dst = [width as f32, height as f32, 0.0, 0.0];
            let bytes: Vec<u8> = dst.iter().flat_map(|v| v.to_le_bytes()).collect();
            compositor.queue().write_buffer(&params, 0, &bytes);
            Ok(Downscaled {
                texture,
                view,
                params,
                bound: None,
                width,
                height,
            })
        })
    }

    /// Scales `source`, a frame the compositor just drew, into `target`, in a submit of
    /// its own after the composite's, and returns the scaled frame. The source binding
    /// is kept while the source texture stays the same, so a steady frame allocates
    /// nothing. Submit it before anything draws into `source` again: the compositor
    /// reuses its output texture for the next frame of that size.
    ///
    /// `source` must be at least `target`'s size along each axis; a smaller one is
    /// scaled up by the same box filter, which repeats its texels.
    ///
    /// # Errors
    /// What the device reported while binding the source.
    pub fn submit(
        &self,
        compositor: &Compositor,
        source: &Output,
        target: &mut Downscaled,
    ) -> Result<Output, RenderError> {
        let stale = target
            .bound
            .as_ref()
            .is_none_or(|(texture, _)| texture != source.texture());
        if stale {
            let group = compositor.scoped(|| {
                let view = source
                    .texture()
                    .create_view(&wgpu::TextureViewDescriptor::default());
                Ok(compositor
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("jstream-render-downscale"),
                        layout: &self.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: target.params.as_entire_binding(),
                            },
                        ],
                    }))
            })?;
            target.bound = Some((source.texture().clone(), group));
        }
        let Some((_, group)) = &target.bound else {
            unreachable!("bound just above");
        };
        let mut encoder =
            compositor
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("jstream-render-downscale"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-downscale"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let _submitting = compositor.submit_lock().lock();
            compositor.submit_work(encoder.finish());
        }
        Ok(Output::new(
            target.texture.clone(),
            target.width,
            target.height,
            None,
        ))
    }
}

impl std::fmt::Debug for Downscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downscaler").finish_non_exhaustive()
    }
}

/// Where [`Downscaler::submit`] scales frames to: a texture of one size, kept with the
/// binding of the source it last read.
pub struct Downscaled {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    params: wgpu::Buffer,
    /// The source texture last bound, and its bind group.
    bound: Option<(wgpu::Texture, wgpu::BindGroup)>,
    width: u32,
    height: u32,
}

impl Downscaled {
    /// The size frames are scaled to.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl std::fmt::Debug for Downscaled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downscaled")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use jstream_media::FrameSize;

    use crate::readback::ReadbackRing;
    use crate::tests::{item, open, solid};
    use crate::{Color, Compositor, Crop, LegacyTransform, Output, Placement, Scene};

    use super::Downscaler;

    /// A `width` by `height` frame, black with a white column `column` texels wide at
    /// its left edge.
    fn white_left(compositor: &mut Compositor, width: u32, height: u32, column: u32) -> Output {
        let white = solid(compositor, column, height, [255, 255, 255, 255]);
        let items = [item(
            &white,
            Placement::Legacy(LegacyTransform::IDENTITY),
            Crop::NONE,
            1.0,
        )];
        let scene = Scene {
            canvas: FrameSize::new(width, height).expect("a test size"),
            background: Color::BLACK,
            items: &items,
        };
        compositor.render(&scene, false).expect("render")
    }

    /// `output`'s pixels, read back.
    fn read(compositor: &Compositor, output: &Output) -> Vec<u8> {
        let mut ring = ReadbackRing::default();
        assert!(ring.record(compositor, output, 0u32).expect("record"));
        let mut out = Vec::new();
        let mut got = None;
        for _ in 0..10_000 {
            ring.collect(compositor, &mut out, |_, rgba| got = Some(rgba.to_vec()));
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        got.expect("the readback landed")
    }

    /// Each scaled texel is the box average of the texels it covers in linear light: a
    /// half-white region is mid-grey in linear, about 188 in sRGB, not 128; and a white
    /// column a quarter as wide as a texel's box keeps a quarter of its light rather than
    /// vanishing or filling the texel, as one tap would.
    #[test]
    fn a_scaled_texel_averages_what_it_covers() {
        let Some(mut opened) = open() else { return };
        let compositor = &mut opened.compositor;
        let downscaler = Downscaler::new(compositor);
        let half = white_left(compositor, 8, 4, 4);
        let mut two = downscaler
            .target(compositor, FrameSize::new(2, 1).expect("a size"))
            .expect("target");
        let scaled = downscaler
            .submit(compositor, &half, &mut two)
            .expect("scale");
        assert_eq!((scaled.width(), scaled.height()), (2, 1));
        assert_eq!(
            read(compositor, &scaled),
            [255, 255, 255, 255, 0, 0, 0, 255]
        );

        let mut one = downscaler
            .target(compositor, FrameSize::new(1, 1).expect("a size"))
            .expect("target");
        let scaled = downscaler
            .submit(compositor, &half, &mut one)
            .expect("scale");
        let grey = read(compositor, &scaled);
        assert!(grey[0].abs_diff(188) <= 1 && grey[3] == 255, "{grey:?}");

        // A new source texture, of another size, is bound afresh.
        let line = white_left(compositor, 4, 4, 1);
        let scaled = downscaler
            .submit(compositor, &line, &mut one)
            .expect("scale");
        let quarter = read(compositor, &scaled);
        assert!(
            quarter[0].abs_diff(137) <= 1 && quarter[3] == 255,
            "{quarter:?}"
        );
    }

    /// A target past the adapter's 2D limit is refused.
    #[test]
    fn a_target_past_the_limit_is_refused() {
        let Some(opened) = open() else { return };
        let downscaler = Downscaler::new(&opened.compositor);
        let huge = opened.compositor.device().limits().max_texture_dimension_2d + 1;
        let size = FrameSize::new(huge, 1).expect("a size");
        assert!(downscaler.target(&opened.compositor, size).is_err());
    }
}
