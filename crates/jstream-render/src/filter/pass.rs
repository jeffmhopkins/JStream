//! The GPU side every filter shares: one bind group layout, a pipeline per shader built
//! on first use, and [`Ctx::draw`], one fullscreen pass from an input (and an optional
//! auxiliary image) into a target.
//!
//! A multi-pass filter's intermediate layers come from its own [`Scratch`] pool, so a
//! filter that runs every frame at one size makes them once. So do each draw's two
//! uniform buffers, which a draw rewrites in place. Its bind group, with its two views,
//! is still made per draw: it holds the textures the draw reads, and a kept one could
//! outlive the cache entry that owns them and free them on the drawing thread (R-RT-003).

use std::collections::HashMap;
use std::sync::Arc;

use jstream_platform::FrameSync;
use jstream_protocol::Settings;

use crate::error::RenderError;
use crate::filter::FilterClock;
use crate::gpu::Gpu;
use crate::image::{self, Image, LAYER_FORMAT, Maker, Owner};

/// The WGSL every filter shader starts with: the vertex stage, the bindings and the colour
/// helpers. Effect packs validate against it.
pub const PRELUDE: &str = concat!(
    include_str!("prelude.wgsl"),
    "\n",
    include_str!("colour.wgsl")
);

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
    /// `assets::lut::lut_image`, a mask image) or the referenced scene's composited layer,
    /// set by the chain runtime whenever it changes; `None` clears it. Kinds that read no
    /// external image ignore it.
    fn set_reference(&mut self, image: Option<Arc<Image>>) {
        let _ = image;
    }

    /// Fills a shader source's input slot `slot` (a scene's composite, the audio data
    /// image), or empties it with `None`. Other kinds ignore it.
    fn set_input(&mut self, slot: usize, image: Option<Arc<Image>>) {
        let _ = (slot, image);
    }

    /// Hands a shader source its audio levels for the next frame. Other kinds ignore it.
    fn set_audio(&mut self, audio: super::ProgramAudio) {
        let _ = audio;
    }

    /// Tells the filter its input is padded by `pad` transparent pixels on every side
    /// (`VideoFilter::set_pad`), for a kind that frames the input's own rectangle.
    /// Other kinds ignore it.
    fn set_pad(&mut self, pad: u32) {
        let _ = pad;
    }

    /// Starts a shader source over: its time and frame count from 0 and its feedback
    /// transparent, as when it is shown again. Other kinds ignore it.
    fn restart(&mut self) {}

    /// How far, in input pixels, what this filter draws can reach past its input's
    /// non-transparent texels, for a kind that draws transparent everywhere farther from
    /// them, keeps its input's size, and draws each pass over the whole frame at that
    /// pass's own scale (#1070). An input whose content is known
    /// ([`Image::content`]) then has every pass drawn only over that content grown by
    /// this ([`Bound`]). `None`, the default, for a kind that can draw anywhere: one that
    /// holds frames, moves its input, draws a picture of its own or reshapes the frame.
    /// Nothing checks the promise, and a wrong one cuts what the kind draws, so a kind
    /// that gives one joins `a_filter_over_a_bounded_layer_draws_only_near_its_content`
    /// (`src/tests/content.rs`), which draws it bounded and whole and compares the two.
    /// A pass that draws into a frame placed differently from its output's (a shadow's
    /// grown shape) moves the bound into it first ([`Bound::placed`]).
    fn reach(&self) -> Option<f32> {
        None
    }

    /// Records the filter into `ctx`: read `input`, fill all of `output`, a layer of
    /// [`Self::output_size`]. Multi-pass filters draw into [`Ctx::scratch`] images first,
    /// and keep nothing from them past the call; an image kept between frames comes from
    /// [`Ctx::layer`].
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

/// A shader by reference: a built-in [`Shader`], or one whose WGSL arrives at run time
/// (an effect pack, [`super::ExternalFilter`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShaderRef<'a> {
    /// The pipeline cache key. A later draw with the same label and different source
    /// rebuilds the pipeline, so a reloaded pack replaces its old one.
    pub label: &'a str,
    /// As [`Shader::source`].
    pub source: &'a str,
    /// As [`Shader::blend`].
    pub blend: Blend,
}

impl From<Shader> for ShaderRef<'static> {
    fn from(shader: Shader) -> Self {
        Self {
            label: shader.label,
            source: shader.source,
            blend: shader.blend,
        }
    }
}

/// How a draw writes its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blend {
    /// Clear the target to transparent and write the shader's output.
    Replace,
    /// Keep the target and draw premultiplied src-over it (a shadow under its shape).
    #[expect(
        dead_code,
        reason = "the harness offers it for a multi-pass kind that draws under its shape; none does yet"
    )]
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
    pipelines: HashMap<String, Vec<Cached>>,
    /// Shader-source passes' bindings and pipelines (ADR-0093).
    programs: Programs,
}

/// What shader-source passes draw with: a bind group layout per number of inputs, made
/// on first use, a repeating sampler, and their pipelines.
struct Programs {
    /// Index `n`: the layout of a pass with `n` inputs.
    layouts: Vec<Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout)>>,
    repeat: wgpu::Sampler,
    pipelines: HashMap<String, Vec<Cached>>,
}

/// A built pipeline and the source it was built from.
struct Cached {
    source: String,
    /// The pipeline, or why wgpu refused it, so a refused shader isn't rebuilt each frame.
    built: Result<wgpu::RenderPipeline, String>,
}

/// The most sources kept per label: a reloaded pack's old and new filters may both be
/// live in different chains for a while, and each must not evict the other every frame.
const SOURCES_PER_LABEL: usize = 4;

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
        let repeat = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-program-repeat"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
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
            programs: Programs {
                layouts: Vec::new(),
                repeat,
                pipelines: HashMap::new(),
            },
        }
    }

    /// The bind group and pipeline layouts of a shader-source pass with `inputs`
    /// textures, made on first use.
    fn program_layouts(
        &mut self,
        device: &wgpu::Device,
        inputs: usize,
    ) -> &(wgpu::BindGroupLayout, wgpu::PipelineLayout) {
        if self.programs.layouts.len() <= inputs {
            self.programs.layouts.resize_with(inputs + 1, || None);
        }
        self.programs.layouts[inputs].get_or_insert_with(|| {
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
            let sampler = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            };
            let mut entries = vec![
                uniform(0),
                sampler(1),
                sampler(2),
                uniform(super::program::PROGRAM_PARAMS_BINDING),
            ];
            for n in 0..inputs {
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: super::program::FIRST_INPUT_BINDING + n as u32,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                });
            }
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("jstream-render-program"),
                entries: &entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("jstream-render-program"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            (layout, pipeline_layout)
        })
    }

    /// The pipeline for `shader`, built on first use of its label and source. A shader
    /// wgpu refuses is an error, not a panic: a pack's WGSL arrives at run time. The
    /// refusal is kept too, so the shader is not recompiled every frame.
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        shader: ShaderRef<'_>,
    ) -> Result<&wgpu::RenderPipeline, RenderError> {
        cached(
            &mut self.pipelines,
            device,
            shader,
            &self.pipeline_layout,
            PRELUDE,
        )
    }

    /// [`Self::pipeline`] for a shader-source pass reading `inputs` textures.
    fn program_pipeline(
        &mut self,
        device: &wgpu::Device,
        shader: ShaderRef<'_>,
        inputs: usize,
    ) -> Result<&wgpu::RenderPipeline, RenderError> {
        self.program_layouts(device, inputs);
        let Some(Some((_, layout))) = self.programs.layouts.get(inputs) else {
            return Err(RenderError::Invalid {
                what: "shader source",
                detail: format!("{}: no layout for {inputs} inputs", shader.label),
            });
        };
        cached(
            &mut self.programs.pipelines,
            device,
            shader,
            layout,
            super::program::PROGRAM_PRELUDE,
        )
    }
}

/// The pipeline for `shader` in `pipelines`, built with `layout` after `prelude` on first
/// use of its label and source. A shader wgpu refuses is an error, not a panic: a pack's
/// WGSL arrives at run time. The refusal is kept too, so the shader is not recompiled
/// every frame.
fn cached<'a>(
    pipelines: &'a mut HashMap<String, Vec<Cached>>,
    device: &wgpu::Device,
    shader: ShaderRef<'_>,
    layout: &wgpu::PipelineLayout,
    prelude: &str,
) -> Result<&'a wgpu::RenderPipeline, RenderError> {
    // A known label is looked up without allocating its key (`entry` would allocate
    // one on every draw, and `get_mut` then `entry` does not borrow-check).
    if !pipelines.contains_key(shader.label) {
        pipelines.insert(shader.label.to_owned(), Vec::new());
    }
    let Some(entries) = pipelines.get_mut(shader.label) else {
        return Err(RenderError::Invalid {
            what: "filter shader",
            detail: format!("{}: pipeline cache entry missing", shader.label),
        });
    };
    let index = match entries.iter().position(|c| c.source == shader.source) {
        Some(index) => index,
        None => {
            // Validation errors (wgpu's own checks) and internal ones (a backend
            // compiler refusing naga's output) both go to wgpu's panicking default
            // handler unless a scope catches them.
            let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
            let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let pipeline = build(layout, device, shader, prelude);
            let error = ready(validation.pop()).or_else(|| ready(internal.pop()));
            if entries.len() >= SOURCES_PER_LABEL {
                entries.remove(0);
            }
            entries.push(Cached {
                source: shader.source.to_owned(),
                built: match error {
                    None => Ok(pipeline),
                    Some(error) => Err(error.to_string()),
                },
            });
            entries.len() - 1
        }
    };
    match &entries[index].built {
        Ok(pipeline) => Ok(pipeline),
        Err(error) => Err(RenderError::Invalid {
            what: "filter shader",
            detail: format!("{}: {error}", shader.label),
        }),
    }
}

fn build(
    pipeline_layout: &wgpu::PipelineLayout,
    device: &wgpu::Device,
    shader: ShaderRef<'_>,
    prelude: &str,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader.label),
        source: wgpu::ShaderSource::Wgsl(format!("{prelude}\n{}", shader.source).into()),
    });
    let blend = match shader.blend {
        Blend::Replace => None,
        Blend::Over => Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(shader.label),
        layout: Some(pipeline_layout),
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
}

/// The value of a future that native wgpu resolves at once (an error scope's pop).
/// `None` if it is still pending, which native wgpu never leaves it.
fn ready<T>(future: impl Future<Output = Option<T>>) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => None,
    }
}

/// One filter's intermediate layers, kept between its calls (V0, #369): the `n`th
/// [`Ctx::scratch`] of a call gets the `n`th layer of the call before when it has the
/// size asked for, so a filter at a steady size makes its layers once. A layer of another
/// size, or of another compositor, is replaced. Commands on the queue run in order, so a
/// layer redrawn this call doesn't disturb what an earlier submit drew from it, and two
/// scratch layers of one call are never the same.
///
/// It lives in the [`super::VideoFilter`], so a filter the chain runtime retires takes
/// its layers to the cache's collector with it (R-RT-003). Layers a call didn't ask for
/// are dropped at its end, and one replaced for a new size at once, on the drawing thread:
/// a size change or a filter needing fewer layers, not every frame.
///
/// It keeps each draw's uniform buffers the same way ([`DrawSlot`]): the `n`th draw of a
/// call reuses the `n`th draw's of the call before.
#[derive(Default)]
pub(crate) struct Scratch {
    layers: Vec<Image>,
    /// How many layers this call has taken.
    taken: usize,
    draws: Vec<DrawSlot>,
    /// How many draws this call has recorded.
    drawn: usize,
    /// Uniform buffers made, for tests.
    #[cfg(test)]
    made_buffers: usize,
}

/// What one draw of a filter keeps between calls: its two uniform buffers, rewritten
/// with `queue.write_buffer` before the submit that reads them. They hold no texture, so
/// keeping them keeps nothing of the cache's alive. Buffers belong to one compositor's
/// device, so a draw for another compositor makes them again. Commands on the queue run
/// in order, so rewriting a buffer an earlier submit read doesn't change what that submit
/// drew, and two draws of one call never share a slot.
struct DrawSlot {
    owner: Owner,
    frame: wgpu::Buffer,
    params: wgpu::Buffer,
}

impl Scratch {
    /// Starts a call: its first [`Self::take`] gets the first layer.
    pub(crate) fn begin(&mut self) {
        self.taken = 0;
        self.drawn = 0;
    }

    /// Ends a call, dropping the layers it didn't take and the draws it didn't record.
    pub(crate) fn end(&mut self) {
        self.layers.truncate(self.taken);
        self.draws.truncate(self.drawn);
    }

    /// The call's next draw slot's uniform buffers, holding `frame` and `params` once the
    /// queue writes them.
    fn slot(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        frame: &[u8],
        params: &[u8],
    ) -> (&wgpu::Buffer, &wgpu::Buffer) {
        let index = self.drawn;
        self.drawn += 1;
        let fits = self.draws.get(index).is_some_and(|slot| {
            slot.owner.is(owner)
                && slot.frame.size() == frame.len() as u64
                && slot.params.size() == params.len() as u64
        });
        if !fits {
            let slot = DrawSlot {
                owner: owner.clone(),
                frame: uniform_buffer(&gpu.device, "jstream-render-filter-frame", frame.len()),
                params: uniform_buffer(&gpu.device, "jstream-render-filter-params", params.len()),
            };
            #[cfg(test)]
            {
                self.made_buffers += 2;
            }
            match self.draws.get_mut(index) {
                Some(old) => *old = slot,
                None => self.draws.push(slot),
            }
        }
        let slot = &self.draws[index];
        gpu.queue.write_buffer(&slot.frame, 0, frame);
        gpu.queue.write_buffer(&slot.params, 0, params);
        (&slot.frame, &slot.params)
    }

    /// The uniform buffers the filter's draws have made, for tests.
    #[cfg(test)]
    pub(crate) fn made_buffers(&self) -> usize {
        self.made_buffers
    }

    /// The call's next layer, of `width` by `height`.
    fn take(&mut self, maker: &Maker<'_>, width: u32, height: u32) -> Result<Image, RenderError> {
        let index = self.taken;
        let fits = self.layers.get(index).is_some_and(|layer| {
            (layer.width(), layer.height()) == (width, height) && layer.owner().is(maker.owner)
        });
        if !fits {
            let layer = image::layer(maker, width, height)?;
            match self.layers.get_mut(index) {
                Some(slot) => *slot = layer,
                None => self.layers.push(layer),
            }
        }
        self.taken += 1;
        Ok(self.layers[index].clone())
    }

    /// The layers kept, for tests.
    #[cfg(test)]
    pub(crate) fn kept(&self) -> &[Image] {
        &self.layers
    }
}

/// What a [`Pass`] draws with: the device, the frame's command encoder, and the clock.
pub(crate) struct Ctx<'a> {
    pub(crate) gpu: &'a Gpu,
    pub(crate) maker: Maker<'a>,
    pub(crate) runtime: &'a mut Runtime,
    pub(crate) encoder: &'a mut wgpu::CommandEncoder,
    pub(crate) clock: FilterClock,
    /// The frame holds of the images the pass reads besides its input, with their
    /// textures: `apply_filter` stages them with the input's before the submit and hands
    /// them back at its end.
    pub(crate) holds: &'a mut Vec<(Arc<dyn FrameSync>, wgpu::Texture)>,
    /// The drawing filter's intermediate layers.
    pub(crate) scratch: &'a mut Scratch,
    /// Where in the output frame the filter can draw anything, when its input's content
    /// and its [`Pass::reach`] say so: every draw is cut to it, at its target's scale.
    pub(crate) bound: Option<Bound>,
}

/// The output pixels a bounded filter can draw into: `[x0, y0, x1, y1]` of a frame of
/// `frame` pixels (#1070).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bound {
    pub rect: [u32; 4],
    pub frame: (u32, u32),
}

impl Bound {
    /// The same pixels in a frame of `frame` pixels whose `origin` is the output's
    /// top-left: a pass's scratch frame grown around the output (a shadow's shape).
    pub(crate) fn placed(self, origin: (u32, u32), frame: (u32, u32)) -> Self {
        let [x0, y0, x1, y1] = self.rect;
        let (x, y) = origin;
        Self {
            rect: [
                (x0 + x).min(frame.0),
                (y0 + y).min(frame.1),
                (x1 + x).min(frame.0),
                (y1 + y).min(frame.1),
            ],
            frame,
        }
    }

    /// Texels of a target of `width` by `height` that can hold what the frame's `rect`
    /// does: scaled to the target, which spans the frame at its own scale, and grown by
    /// two texels for a target rounded up from it and for a filtered read. `None` when
    /// none can.
    fn in_target(self, width: u32, height: u32) -> Option<[u32; 4]> {
        const MARGIN: f32 = 2.0;
        let scale = |value: u32, frame: u32, side: u32| value as f32 * side as f32 / frame as f32;
        let [x0, y0, x1, y1] = self.rect;
        let (fw, fh) = (self.frame.0.max(1), self.frame.1.max(1));
        let low = |value: f32| (value - MARGIN).floor().max(0.0) as u32;
        let high = |value: f32, side: u32| ((value + MARGIN).ceil() as u32).min(side);
        let rect = [
            low(scale(x0, fw, width)),
            low(scale(y0, fh, height)),
            high(scale(x1, fw, width), width),
            high(scale(y1, fh, height), height),
        ];
        (x1 > x0 && y1 > y0 && rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
    }
}

impl Ctx<'_> {
    /// Records that this submit reads `image`, so a frame hold it carries is staged
    /// before the submit and handed back at its end, once however often it is read.
    pub(crate) fn hold(&mut self, image: &Image) {
        if let Some(sync) = image.sync()
            && !self.holds.iter().any(|(held, _)| Arc::ptr_eq(held, sync))
        {
            self.holds.push((Arc::clone(sync), image.texture().clone()));
        }
    }

    /// The media clock for this frame.
    pub(crate) fn clock(&self) -> FilterClock {
        self.clock
    }

    /// A premultiplied layer to draw an intermediate pass into, from the filter's
    /// [`Scratch`]: the same texture on the next call when the size holds, so it must not
    /// be kept past this one.
    pub(crate) fn scratch(&mut self, width: u32, height: u32) -> Result<Image, RenderError> {
        self.scratch.take(&self.maker, width, height)
    }

    /// A new premultiplied layer of the filter's own, to keep between frames (a video
    /// delay's held frames).
    pub(crate) fn layer(&self, width: u32, height: u32) -> Result<Image, RenderError> {
        image::layer(&self.maker, width, height)
    }

    /// Records clearing `target`, a layer, to transparent.
    pub(crate) fn clear(&mut self, target: &Image) -> Result<(), RenderError> {
        let Some(view) = target.target() else {
            return Err(RenderError::Invalid {
                what: "clear target",
                detail: "the target is not a layer".into(),
            });
        };
        drop(self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        }));
        Ok(())
    }

    /// Records one fullscreen draw of `draw.shader` into `draw.target`.
    pub(crate) fn draw(&mut self, draw: &Draw<'_>) -> Result<(), RenderError> {
        self.draw_ref(
            draw.shader.into(),
            draw.input,
            draw.aux,
            draw.params,
            draw.target,
        )
    }

    /// [`Self::draw`] with a shader whose WGSL is not `'static`.
    pub(crate) fn draw_ref(
        &mut self,
        shader: ShaderRef<'_>,
        input: &Image,
        aux: Option<&Image>,
        params: &[u8],
        target: &Image,
    ) -> Result<(), RenderError> {
        let draw = Io {
            input,
            aux,
            params,
            target,
        };
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
        let alpha = |image: &Image| image.alpha().shader_mode();
        let seconds = (self.clock.seconds.rem_euclid(TIME_WRAP_SECONDS)) as f32;
        let frame = super::params::Uniforms::new()
            .vec2([draw.input.width() as f32, draw.input.height() as f32])
            .vec2([draw.target.width() as f32, draw.target.height() as f32])
            .vec2([aux.width() as f32, aux.height() as f32])
            .f32(seconds)
            .f32(alpha(draw.input))
            .f32(alpha(aux))
            .finish();
        debug_assert_eq!(frame.len() as u64, FRAME_BYTES);
        let (frame_buffer, params_buffer) =
            self.scratch
                .slot(self.gpu, self.maker.owner, &frame, draw.params);
        let bind_group = bind_group(
            self.gpu,
            self.runtime,
            shader.label,
            draw.input,
            aux,
            frame_buffer,
            params_buffer,
        );
        let load = match shader.blend {
            Blend::Replace => wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            Blend::Over => wgpu::LoadOp::Load,
        };
        // A bounded filter draws only where it can draw anything; the clear covers the rest.
        let cut = self
            .bound
            .map(|bound| bound.in_target(draw.target.width(), draw.target.height()));
        let pipeline = self.runtime.pipeline(&self.gpu.device, shader)?;
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(shader.label),
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
        match cut {
            // Nothing to draw: the pass only clears.
            Some(None) => return Ok(()),
            Some(Some([x0, y0, x1, y1])) => pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0),
            None => {}
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

impl Ctx<'_> {
    /// Records one fullscreen draw of a shader-source pass (ADR-0093) into `target`:
    /// `system` is its `sys` uniform, `params` its `Params`, and `inputs` its textures in
    /// binding order.
    pub(crate) fn draw_program(
        &mut self,
        shader: ShaderRef<'_>,
        inputs: &[Image],
        system: &[u8],
        params: &[u8],
        target: &Image,
    ) -> Result<(), RenderError> {
        let Some(view) = target.target() else {
            return Err(RenderError::Invalid {
                what: "shader source target",
                detail: "the target is not a layer".into(),
            });
        };
        if inputs.iter().any(|i| i.texture() == target.texture()) {
            return Err(RenderError::Invalid {
                what: "shader source target",
                detail: format!(
                    "{}: a pass cannot sample the image it draws into",
                    shader.label
                ),
            });
        }
        for input in inputs {
            self.hold(input);
        }
        let (system_buffer, params_buffer) =
            self.scratch
                .slot(self.gpu, self.maker.owner, system, params);
        let (system_buffer, params_buffer) = (system_buffer.clone(), params_buffer.clone());
        self.runtime.program_layouts(&self.gpu.device, inputs.len());
        let max = super::program::MAX_INPUTS;
        if inputs.len() > max {
            return Err(RenderError::Invalid {
                what: "shader source",
                detail: format!("{}: {} inputs, over {max}", shader.label, inputs.len()),
            });
        }
        // Fixed arrays, so a steady frame allocates no list for the bind group.
        let views: [Option<wgpu::TextureView>; super::program::MAX_INPUTS] =
            std::array::from_fn(|n| {
                inputs.get(n).map(|i| {
                    i.texture()
                        .create_view(&wgpu::TextureViewDescriptor::default())
                })
            });
        let Some(Some((layout, _))) = self.runtime.programs.layouts.get(inputs.len()) else {
            return Err(RenderError::Invalid {
                what: "shader source",
                detail: format!("{}: no layout for {} inputs", shader.label, inputs.len()),
            });
        };
        let first = super::program::FIRST_INPUT_BINDING as usize;
        let entries: [wgpu::BindGroupEntry<'_>;
            super::program::FIRST_INPUT_BINDING as usize + super::program::MAX_INPUTS] =
            std::array::from_fn(|n| {
                let (binding, resource) = match n {
                    0 => (0, system_buffer.as_entire_binding()),
                    1 => (1, wgpu::BindingResource::Sampler(&self.runtime.sampler)),
                    2 => (
                        2,
                        wgpu::BindingResource::Sampler(&self.runtime.programs.repeat),
                    ),
                    3 => (
                        super::program::PROGRAM_PARAMS_BINDING,
                        params_buffer.as_entire_binding(),
                    ),
                    _ => match &views[n - first] {
                        Some(view) => (n as u32, wgpu::BindingResource::TextureView(view)),
                        // Past the inputs: never handed to wgpu.
                        None => (
                            n as u32,
                            wgpu::BindingResource::Sampler(&self.runtime.sampler),
                        ),
                    },
                };
                wgpu::BindGroupEntry { binding, resource }
            });
        let entries = &entries[..first + inputs.len()];
        let bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(shader.label),
                layout,
                entries,
            });
        let pipeline = self
            .runtime
            .program_pipeline(&self.gpu.device, shader, inputs.len())?;
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(shader.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// [`Draw`] without its shader.
struct Io<'a> {
    input: &'a Image,
    aux: Option<&'a Image>,
    params: &'a [u8],
    target: &'a Image,
}

/// A uniform buffer of `size` bytes, written with `queue.write_buffer`.
fn uniform_buffer(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A draw's bind group: `input` and `aux` with their own views, the runtime's sampler,
/// and the draw's `frame` and `params` uniforms.
fn bind_group(
    gpu: &Gpu,
    runtime: &Runtime,
    label: &str,
    input: &Image,
    aux: &Image,
    frame: &wgpu::Buffer,
    params: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let input_view = input
        .texture()
        .create_view(&wgpu::TextureViewDescriptor::default());
    let aux_view = aux
        .texture()
        .create_view(&wgpu::TextureViewDescriptor::default());
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &runtime.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&input_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&runtime.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: frame.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&aux_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: params.as_entire_binding(),
            },
        ],
    })
}

#[cfg(test)]
mod bound_tests {
    use super::Bound;

    /// A bound moved into a frame grown around its own keeps the same pixels, shifted
    /// by the origin and clipped to the new frame; mapped into a target of that frame,
    /// it covers them with the margin.
    #[test]
    fn a_placed_bound_keeps_its_pixels_in_the_grown_frame() {
        let bound = Bound {
            rect: [10, 20, 30, 40],
            frame: (100, 50),
        };
        let placed = bound.placed((8, 8), (116, 66));
        assert_eq!(placed.rect, [18, 28, 38, 48]);
        assert_eq!(placed.frame, (116, 66));
        // Clipped to the frame it moves into.
        assert_eq!(bound.placed((90, 30), (110, 60)).rect, [100, 50, 110, 60]);
        // At a quarter scale, with two texels of margin on each side.
        assert_eq!(placed.in_target(29, 17), Some([2, 5, 12, 15]));
        // Nothing to draw.
        let empty = Bound {
            rect: [0; 4],
            frame: (100, 50),
        };
        assert_eq!(empty.in_target(100, 50), None);
    }
}
