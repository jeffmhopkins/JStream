//! The GPU side every filter shares: one bind group layout, a pipeline per shader built
//! on first use, and [`Ctx::draw`], one fullscreen pass from an input (and an optional
//! auxiliary image) into a target.
//!
//! Each draw creates its two small uniform buffers and its bind group. The chain runtime
//! (V0) pools intermediate images and these buffers; until then a filter costs a few
//! small allocations per frame.

use std::collections::HashMap;
use std::sync::Arc;

use jstream_protocol::Settings;

use crate::error::RenderError;
use crate::filter::FilterClock;
use crate::gpu::Gpu;
use crate::image::{self, Alpha, Image, LAYER_FORMAT, Maker};

/// The WGSL every filter shader starts with.
const PRELUDE: &str = include_str!("prelude.wgsl");

/// Bytes of the `Frame` uniform in `prelude.wgsl`.
const FRAME_BYTES: u64 = 48;

/// Media-clock seconds are wrapped to a day before they reach a shader, so an `f32`
/// keeps better than 10 ms of precision.
const TIME_WRAP_SECONDS: f64 = 86_400.0;

/// One filter kind's drawing: what [`super::VideoFilter`] wraps.
pub(crate) trait Pass: Send {
    /// Applies new settings, complete as the graph stores them, in place.
    fn update(&mut self, settings: &Settings);

    /// The size this filter draws for an input of `width` by `height`.
    fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        (width, height)
    }

    /// Hands the filter its external image: a decoded asset (a LUT from
    /// `assets::lut_image`, a mask image) or the referenced scene's composited layer,
    /// set by the chain runtime whenever it changes; `None` clears it. Kinds that read no
    /// external image ignore it.
    fn set_reference(&mut self, image: Option<Arc<Image>>) {
        let _ = image;
    }

    /// Records the filter into `ctx`: read `input`, fill all of `output`, a layer of
    /// [`Self::output_size`]. Multi-pass filters draw into [`Ctx::scratch`] images first.
    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        input: &Image,
        output: &Image,
    ) -> Result<(), RenderError>;
}

/// A filter's fragment shader, appended to the prelude. The pipeline is built the first
/// time a draw uses it and kept for the compositor's life, keyed by `label`, so a label
/// must name exactly one source and blend.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shader {
    /// Unique among filter shaders.
    pub label: &'static str,
    /// WGSL defining `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and, when it
    /// has parameters, `@group(0) @binding(4) var<uniform> p: Params;`.
    pub source: &'static str,
    /// How the shader's output meets the target's contents.
    pub blend: Blend,
}

/// How a draw writes its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blend {
    /// Clear the target to transparent and write the shader's output.
    Replace,
    /// Keep the target and draw premultiplied src-over it (a shadow under its shape).
    Over,
}

/// One fullscreen draw.
pub(crate) struct Draw<'a> {
    pub shader: Shader,
    /// What `src` and `texel` read.
    pub input: &'a Image,
    /// What `aux` reads; the input when `None`.
    pub aux: Option<&'a Image>,
    /// The `Params` uniform, from [`super::params::Uniforms::finish`].
    pub params: &'a [u8],
    /// A layer: [`Ctx::scratch`] or the pass's output.
    pub target: &'a Image,
}

/// Owned by the compositor: the filters' layout, sampler and pipelines.
pub(crate) struct Runtime {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    pipelines: HashMap<&'static str, wgpu::RenderPipeline>,
}

impl Runtime {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-filter"),
            entries: &[
                texture(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform(2),
                texture(3),
                uniform(4),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-filter"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-filter-linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            layout,
            pipeline_layout,
            sampler,
            pipelines: HashMap::new(),
        }
    }

    fn pipeline(&mut self, device: &wgpu::Device, shader: Shader) -> &wgpu::RenderPipeline {
        self.pipelines.entry(shader.label).or_insert_with(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(shader.label),
                source: wgpu::ShaderSource::Wgsl(format!("{PRELUDE}\n{}", shader.source).into()),
            });
            let blend = match shader.blend {
                Blend::Replace => None,
                Blend::Over => Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(shader.label),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        })
    }
}

/// What a [`Pass`] draws with: the device, the frame's command encoder, and the clock.
pub(crate) struct Ctx<'a> {
    pub(crate) gpu: &'a Gpu,
    pub(crate) maker: Maker<'a>,
    pub(crate) runtime: &'a mut Runtime,
    pub(crate) encoder: &'a mut wgpu::CommandEncoder,
    pub(crate) clock: FilterClock,
}

impl Ctx<'_> {
    /// The media clock for this frame.
    pub(crate) fn clock(&self) -> FilterClock {
        self.clock
    }

    /// A new premultiplied layer to draw an intermediate pass into, or to keep between
    /// frames.
    pub(crate) fn scratch(&self, width: u32, height: u32) -> Result<Image, RenderError> {
        image::layer(&self.maker, width, height)
    }

    /// Records one fullscreen draw of `draw.shader` into `draw.target`.
    pub(crate) fn draw(&mut self, draw: &Draw<'_>) -> Result<(), RenderError> {
        let Some(target) = draw.target.target() else {
            return Err(RenderError::Invalid {
                what: "filter target",
                detail: "the target is not a layer".into(),
            });
        };
        if draw.target.texture() == draw.input.texture()
            || draw
                .aux
                .is_some_and(|aux| aux.texture() == draw.target.texture())
        {
            return Err(RenderError::Invalid {
                what: "filter target",
                detail: "a draw cannot sample the image it draws into".into(),
            });
        }
        let aux = draw.aux.unwrap_or(draw.input);
        let straight = |image: &Image| f32::from(u8::from(image.alpha() == Alpha::Straight));
        let seconds = (self.clock.seconds.rem_euclid(TIME_WRAP_SECONDS)) as f32;
        let frame = super::params::Uniforms::new()
            .vec2([draw.input.width() as f32, draw.input.height() as f32])
            .vec2([draw.target.width() as f32, draw.target.height() as f32])
            .vec2([aux.width() as f32, aux.height() as f32])
            .f32(seconds)
            .f32(straight(draw.input))
            .f32(straight(aux))
            .finish();
        debug_assert_eq!(frame.len() as u64, FRAME_BYTES);
        let device = &self.gpu.device;
        let frame_buffer = uniform_buffer(device, "jstream-render-filter-frame", &frame);
        let params_buffer = uniform_buffer(device, "jstream-render-filter-params", draw.params);
        let input_view = draw
            .input
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let aux_view = aux
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(draw.shader.label),
            layout: &self.runtime.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.runtime.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&aux_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });
        let load = match draw.shader.blend {
            Blend::Replace => wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            Blend::Over => wgpu::LoadOp::Load,
        };
        let pipeline = self.runtime.pipeline(device, draw.shader);
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(draw.shader.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

fn uniform_buffer(device: &wgpu::Device, label: &str, bytes: &[u8]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}
