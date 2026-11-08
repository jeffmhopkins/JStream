//! The two passes: composite in FP16 linear light, then resolve to straight sRGB.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_platform::FrameSync;

use crate::blend::{self, BlendPipelines, Layouts, Steps};
use crate::color::{self, premultiplied_linear};
use crate::error::RenderError;
use crate::filter::{self, FilterClock, VideoFilter};
use crate::gpu::{Gpu, OpenOptions};
use crate::image::{self, Alpha, Image, Maker, Owner};
use crate::ingest::Ingest;
use crate::mips::{MAX_SKIP, MipPipelines, Mips, Region};
use crate::model::{GpuModel, ModelDraw, ModelRuntime};
use crate::place::{self, MAX_VERTICES, Shape};
use crate::scene::{Output, Scene, Snapshot};
use jstream_protocol::BlendMode;

/// Bytes from one vertex to the next: 29 `f32`s (position, UV, opacity, alpha mode, `w`,
/// the item's five edge lines of three each, its UV bounds, and its camera's depth of
/// field with the shape's largest blur), tightly packed. wgpu only needs the stride and attribute offsets to be
/// multiples of 4.
pub(crate) const VERTEX_STRIDE: u64 = 116;
/// How many texels a 2D item's canvas pixel spans, along its more minified axis, past
/// which it samples a mip chain (#1217, ADR-0050 decision 5's note on 2D items). One
/// bilinear tap reads a 2×2 texel footprint, so drawn smaller it skips texels and fine
/// detail aliases. Against a box-filtered reference on lavapipe a chain is better from
/// about 1.1 (mean error 3.2 against 4.7 with one level), at 1.2 (3.1 against 5.5) and
/// much better at 2 (6 against 10). Up to this an item drawn nearly at its size stays
/// on the one level, which costs no chain.
const MINIFIED: f32 = 1.15;

/// The most vertices one item draws: a five-vertex polygon as a fan of three triangles.
const MAX_VERTICES_PER_ITEM: u32 = 3 * (MAX_VERTICES as u32 - 2);

struct Draw {
    index: usize,
    first_vertex: u32,
    vertex_count: u32,
    /// A 3D item's mip chain, of its crop of its image (ADR-0050 decision 5), which it
    /// samples instead of the image.
    mips: Option<Arc<Mips>>,
    blend: BlendMode,
    /// For a mode that reads the backdrop, the target's texels it covers, which are
    /// copied into the backdrop before it draws.
    rect: Option<Rect>,
}

/// Texels of the target: `x`, `y`, `width`, `height`, none of them past its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    /// The smallest rectangle holding both.
    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

/// The copy of what is below an item that the modes reading it sample (ADR-0082). It
/// only grows, so targets of different sizes in one frame share it, each copying to the
/// same texels it reads from.
struct Backdrop {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
}

struct Targets {
    width: u32,
    height: u32,
    /// The FP16 texture, which a backdrop copies from.
    working: wgpu::Texture,
    working_view: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    resolve_bind: wgpu::BindGroup,
}

/// Draws one scene on a wgpu device.
///
/// Build it with [`Self::open`] when the platform has no GPU interop, or with
/// [`Self::on_context`] on the device `jstream-platform` opened. Images uploaded or
/// imported here are the only ones [`Self::render`] samples; an image from another
/// compositor is [`RenderError::Invalid`].
pub struct Compositor {
    gpu: Gpu,
    owner: Owner,
    sampler: wgpu::Sampler,
    image_layout: wgpu::BindGroupLayout,
    blends: BlendPipelines,
    backdrop: Option<Backdrop>,
    resolve_layout: wgpu::BindGroupLayout,
    resolve: wgpu::RenderPipeline,
    targets: Option<Targets>,
    vertices: Vec<u8>,
    vertex_buffer: wgpu::Buffer,
    draws: Vec<Draw>,
    /// The texels the prepared draws can reach, or `None` when they reach none (#1070).
    drawn: Option<Rect>,
    /// The order a 3D scene's items draw in (ADR-0094 decision 4), kept to reuse its
    /// allocation: each item's depth along the camera's axis and its index.
    order: Vec<(f64, usize)>,
    /// The holds staged for the submit being built, and their textures, which the
    /// submit hands back to the platform (`PRESENT`) after its last use.
    staged: Vec<(Arc<dyn FrameSync>, wgpu::Texture)>,
    filters: filter::Runtime,
    mip_pipelines: MipPipelines,
    models: ModelRuntime,
    /// How long the last [`Self::render`] waited for its readback (ADR-0103).
    readback_wait: std::time::Duration,
    /// The CPU frame conversion (ADR-0117 §2), built on the first upload. Behind a lock
    /// because uploads take `&self`; only the thread drawing with the compositor takes it.
    ingest: Mutex<Option<Ingest>>,
    /// Whether [`Self::render`] fails on an error the context's shared watch kept
    /// ([`Self::take_error`]). Off for a compositor that catches its own
    /// ([`Self::leave_shared_errors`], #1268).
    shared_errors: bool,
}

impl Compositor {
    /// Opens a device and compiles the compositor pipelines.
    ///
    /// # Errors
    /// [`RenderError::NoAdapter`] when nothing can be created, and [`RenderError::Device`]
    /// when an adapter refuses the device.
    pub fn open(options: OpenOptions) -> Result<Self, RenderError> {
        Ok(Self::new(Gpu::open(options)?))
    }

    /// Pipelines on the platform's device. The context's own drop order is unchanged:
    /// this only clones the device, queue and adapter.
    pub fn on_context(ctx: &jstream_platform::GpuContext) -> Self {
        Self::new(Gpu::from_context(ctx))
    }

    /// Uploads tightly packed straight-alpha sRGB RGBA8.
    ///
    /// The bytes are premultiplied in linear light on the GPU and stored as
    /// `Rgba16Float`, so the linear filter cannot darken transparent edges. This thread
    /// only copies them into a buffer and records the conversion (ADR-0117 §2).
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a zero dimension, [`RenderError::ImageBytes`] when
    /// `rgba` is not `width * height * 4` bytes, and [`RenderError::TooLarge`] when a
    /// side exceeds the adapter limit.
    pub fn upload_rgba8(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Image, RenderError> {
        image::upload(&self.maker(), width, height, rgba)
    }

    /// [`Self::upload_rgba8`] into a texture of `to` texels, smaller than the frame each
    /// way or its size, placed at the frame's size: each texel is the mean of the
    /// frame's texels under it, in premultiplied linear light (#1139).
    pub(crate) fn upload_rgba8_into(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
        to: (u32, u32),
    ) -> Result<Image, RenderError> {
        image::upload_into(&self.maker(), width, height, rgba, to)
    }

    /// Samples `texture`, which must already be on this device: 2D, one sample,
    /// `Rgba8UnormSrgb` or `Bgra8UnormSrgb`, with `TEXTURE_BINDING`. `sync` is staged
    /// on each frame that draws the image.
    ///
    /// The texture is read as straight alpha and premultiplied after filtering. That is
    /// exact for an opaque texture, such as a captured frame. Where alpha is between 0
    /// and 1 a scaled, rotated or off-pixel edge picks up a dark fringe; upload such
    /// sources with [`Self::upload_rgba8`] instead.
    ///
    /// # Errors
    /// [`RenderError::UnsupportedFormat`], [`RenderError::UnsupportedTexture`] or
    /// [`RenderError::TooLarge`].
    pub fn import(
        &self,
        texture: wgpu::Texture,
        sync: Option<Arc<dyn FrameSync>>,
    ) -> Result<Image, RenderError> {
        self.import_as(texture, sync, Alpha::Straight)
    }

    /// Samples `texture`, as [`Self::import`] does, but holding sRGB code values already
    /// multiplied by alpha, as a browser page paints (CEF's layout). It must be
    /// `Rgba8Unorm` or `Bgra8Unorm`, so sampling does not decode it.
    ///
    /// The texels are filtered premultiplied, so transparent edges keep no fringe, then
    /// divided by alpha, decoded from sRGB and multiplied by alpha again in linear light.
    /// Filtering the code values rather than linear light differs from an upload only
    /// where the filter mixes texels of different colour.
    ///
    /// # Errors
    /// As for [`Self::import`].
    pub fn import_premultiplied(
        &self,
        texture: wgpu::Texture,
        sync: Option<Arc<dyn FrameSync>>,
    ) -> Result<Image, RenderError> {
        self.import_as(texture, sync, Alpha::PremultipliedSrgb)
    }

    /// Samples `texture`, as [`Self::import`] does, reading every texel's alpha as 1:
    /// for a captured frame, whose alpha means nothing (a display or window capture can
    /// deliver alpha below 1 under some drivers or HDR modes), so it draws as the CPU
    /// capture path does, which ignores alpha (#451).
    ///
    /// # Errors
    /// As for [`Self::import`].
    pub fn import_opaque(
        &self,
        texture: wgpu::Texture,
        sync: Option<Arc<dyn FrameSync>>,
    ) -> Result<Image, RenderError> {
        self.import_as(texture, sync, Alpha::Opaque)
    }

    /// [`Self::import`], [`Self::import_premultiplied`] or [`Self::import_opaque`], by
    /// `alpha`.
    pub(crate) fn import_as(
        &self,
        texture: wgpu::Texture,
        sync: Option<Arc<dyn FrameSync>>,
        alpha: Alpha,
    ) -> Result<Image, RenderError> {
        image::import(&self.maker(), texture, sync, alpha)
    }

    /// [`Self::import`] of a [`jstream_platform::GpuFrame`], keeping its [`FrameSync`] hold.
    ///
    /// # Errors
    /// As for [`Self::import`].
    pub fn import_frame(&self, frame: &jstream_platform::GpuFrame) -> Result<Image, RenderError> {
        image::import_frame(&self.maker(), frame)
    }

    /// Composites `scene`. When `readback` is set, the output carries a CPU copy.
    ///
    /// Readback waits on the GPU. A tick that only feeds an encoder leaves it false.
    /// Does not wait on the control plane and does no file or network I/O (R-RT-002).
    /// Bind groups live on the [`Image`]s; the vertex buffer and the scratch lists grow
    /// only when the scene outgrows them.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a bad colour, opacity or placement, or an image
    /// another compositor made,
    /// [`RenderError::TooLarge`] for the canvas, [`RenderError::Sync`] when staging a
    /// source frame fails, [`RenderError::TooManyItems`] or [`RenderError::Readback`].
    /// A failed `stage` may already have armed the queue's next submit: the platform
    /// attaches the fences to whichever submit happens next.
    pub fn render(&mut self, scene: &Scene<'_>, readback: bool) -> Result<Output, RenderError> {
        self.readback_wait = std::time::Duration::ZERO;
        self.load(scene)?;
        let width = scene.canvas.width();
        let height = scene.canvas.height();
        self.ensure_targets(width, height)?;

        let readback_buffer = if readback {
            Some(self.readback_buffer(width, height)?)
        } else {
            None
        };
        // Last, so no error between the staging and the submit it arms (#391), and with
        // the queue's submit lock held until that submit, so no other thread's submit
        // takes what was staged.
        let lock = self.gpu.submit.clone();
        let submitting = lock.lock();
        self.stage_sources(scene)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render"),
            });
        let targets = self
            .targets
            .as_ref()
            .expect("invariant: ensure_targets just set them");
        self.record_mips(&mut encoder, scene);
        self.record_composite(
            &mut encoder,
            (&targets.working_view, &targets.working),
            scene,
        );
        self.record_resolve(&mut encoder, targets);
        if let Some(buffer) = &readback_buffer {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &targets.output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_row_bytes(width)),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.record_release(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        drop(submitting);
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        // wgpu validates the commands as they are recorded and submitted, so an error in
        // this frame has been raised by now; one from an earlier call fails this frame.
        if self.shared_errors
            && let Some(detail) = self.take_error()
        {
            return Err(RenderError::Uncaptured { detail });
        }

        let output = targets.output.clone();
        let waiting = std::time::Instant::now();
        let pixels = match readback_buffer {
            Some(buffer) => Some(map_snapshot(&self.gpu.device, buffer, width, height)?),
            None => None,
        };
        self.readback_wait = waiting.elapsed();
        Ok(Output::new(output, width, height, pixels))
    }

    /// How long the last [`Self::render`] spent waiting for its readback: the GPU
    /// finishing the frame and the copy to the CPU. Zero for a render with no readback.
    /// A profile counts it apart from the composite it follows (ADR-0103).
    pub fn readback_wait(&self) -> std::time::Duration {
        self.readback_wait
    }

    /// How many texels the last composite's draws could reach (#1070).
    #[cfg(test)]
    pub(crate) fn drawn_texels(&self) -> u32 {
        self.drawn.map_or(0, |rect| rect.width * rect.height)
    }

    /// Composites `scene` into a new [`Image`] that a later scene on this compositor can
    /// place, as a nested scene is placed in its parent.
    ///
    /// The image is the composite pass alone: premultiplied linear light in
    /// `Rgba16Float`, the same format as an upload, with no resolve to sRGB and no
    /// readback. Placing it filters exactly at partly transparent edges, and nothing is
    /// quantised to 8 bits between the levels. Each call allocates its own texture, so
    /// an image stays valid across later renders; it is freed when dropped. The
    /// submit is ordered before the next one on this queue, so a later `render` that
    /// places the image samples the finished composite. A caller that draws the same
    /// scene every frame keeps a [`Self::layer`] and calls [`Self::render_into`]
    /// instead, which allocates nothing.
    ///
    /// # Errors
    /// As for [`Self::render`], apart from [`RenderError::Readback`].
    pub fn render_image(&mut self, scene: &Scene<'_>) -> Result<Image, RenderError> {
        let image = self.layer(scene.canvas.width(), scene.canvas.height())?;
        self.render_into(scene, &image)?;
        Ok(image)
    }

    /// A new layer of `width` by `height`: an [`Image`] that [`Self::render_into`]
    /// composites into and a later scene places. Its contents are undefined until then.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a zero dimension, [`RenderError::TooLarge`] past the
    /// adapter limit.
    pub fn layer(&self, width: u32, height: u32) -> Result<Image, RenderError> {
        image::layer(&self.maker(), width, height)
    }

    /// Composites `scene` into `target`, a [`Self::layer`] of the canvas size, replacing
    /// what it held. Like [`Self::render_image`], but the texture is reused, so a frame
    /// that redraws a nested scene allocates no texture. Earlier submits that placed
    /// `target` are ordered before this one on the queue, so they keep the old contents.
    ///
    /// # Errors
    /// As for [`Self::render_image`]; also [`RenderError::Invalid`] when `target` is not
    /// a layer this compositor made, is not the canvas size, or is placed by `scene`
    /// itself (a layer cannot be sampled while it is drawn into).
    pub fn render_into(&mut self, scene: &Scene<'_>, target: &Image) -> Result<(), RenderError> {
        let view = match target.target() {
            Some(view) if target.owner().is(&self.owner) => view,
            _ => {
                return Err(RenderError::Invalid {
                    what: "layer",
                    detail: "the target is not a layer this compositor made".into(),
                });
            }
        };
        if target.width() != scene.canvas.width() || target.height() != scene.canvas.height() {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: format!(
                    "the target is {}x{} but the canvas is {}x{}",
                    target.width(),
                    target.height(),
                    scene.canvas.width(),
                    scene.canvas.height()
                ),
            });
        }
        if let Some(index) = scene
            .items
            .iter()
            .position(|item| item.image.texture() == target.texture())
        {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: format!("item {index} places the layer being drawn into"),
            });
        }
        self.load(scene)?;
        // The submit lock is held from staging through the submit, as in `render`.
        let lock = self.gpu.submit.clone();
        let submitting = lock.lock();
        self.stage_sources(scene)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-layer"),
            });
        self.record_mips(&mut encoder, scene);
        self.record_composite(&mut encoder, (view, target.texture()), scene);
        self.record_release(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        target.mark_changed();
        // Outside what the items reached the layer holds its clear colour, which is
        // nothing when that is transparent: a scene placing it draws only over the rest.
        let content = (scene.background.a == 0.0).then(|| {
            self.drawn.map_or(
                Region {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                |rect| Region {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                },
            )
        });
        target.set_content(content);
        drop(submitting);
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        Ok(())
    }

    /// Uploads a 3D model (ADR-0107): its meshes, its textures with the mips the model
    /// already holds, and its materials. Do it once per [`jstream_media::model::Model::version`]
    /// and keep the result; drawing it allocates nothing.
    ///
    /// # Errors
    /// [`RenderError::TooLarge`] for a texture past the adapter's limit, and
    /// [`RenderError::Invalid`] for a primitive too large for one draw or for one of the
    /// adapter's buffers.
    pub fn upload_model(
        &self,
        model: &jstream_media::model::Model,
    ) -> Result<GpuModel, RenderError> {
        self.models.upload(&self.gpu, &self.owner, model)
    }

    /// Draws a model placed by an item into `target`, a [`Self::layer`] of the canvas
    /// size, replacing what it held (ADR-0107 decision 6): transparent where the model
    /// isn't, the model with its own depth test where it is. A scene then places the
    /// layer with [`crate::Placement::Layer`], after any filters run over it.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when the model or the layer came from another compositor
    /// or the target is not a layer, or when the placement or camera can't be drawn.
    pub fn draw_model(&mut self, draw: &ModelDraw<'_>, target: &Image) -> Result<(), RenderError> {
        self.models.draw(&self.gpu, &self.owner, draw, target)
    }

    /// Refills `image`, an upload this compositor made, with new tightly packed
    /// straight-alpha sRGB RGBA8 of the same size, without allocating a texture. Frames
    /// already submitted keep sampling the old contents.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when `image` is not an upload this compositor made, and
    /// [`RenderError::ImageBytes`] when `rgba` is not its `width * height * 4` bytes.
    pub fn rewrite_rgba8(&self, image: &Image, rgba: &[u8]) -> Result<(), RenderError> {
        image::rewrite(&self.maker(), image, rgba)
    }

    /// Uploads a CPU video frame read as opaque: packed RGBA8 or BGRA8, or NV12 with its
    /// matrix and range, converted to premultiplied linear on the GPU (ADR-0117 §2). A
    /// capture's alpha means nothing, so every texel is opaque (#451).
    ///
    /// # Errors
    /// [`RenderError::UnsupportedTexture`] for another layout, [`RenderError::Invalid`]
    /// for planes too short for the frame or an odd-sized NV12 frame's missing chroma,
    /// and [`RenderError::TooLarge`] when a side exceeds the adapter limit.
    pub fn upload_frame(&self, frame: &jstream_media::VideoFrame) -> Result<Image, RenderError> {
        image::upload_frame(&self.maker(), frame)
    }

    /// Refills `image`, an upload this compositor made at `frame`'s size, with `frame`
    /// as [`Self::upload_frame`] reads it, without allocating a texture.
    ///
    /// # Errors
    /// As for [`Self::upload_frame`], and [`RenderError::Invalid`] when `image` is not
    /// an upload this compositor made at the frame's size.
    pub fn rewrite_frame(
        &self,
        image: &Image,
        frame: &jstream_media::VideoFrame,
    ) -> Result<(), RenderError> {
        image::rewrite_frame(&self.maker(), image, frame)
    }

    /// Draws `filter` over `input` into a new layer of the filter's
    /// [`VideoFilter::output_size`], at the media-clock time `clock`. The filter keeps
    /// its state, so a stateful kind (a video delay) must see every frame in order.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when `input` came from another compositor,
    /// [`RenderError::TooLarge`] for an output past the adapter limit, and
    /// [`RenderError::Sync`] when staging an imported input or reference image fails.
    pub fn apply_filter(
        &mut self,
        filter: &mut VideoFilter,
        input: &Image,
        clock: FilterClock,
    ) -> Result<Image, RenderError> {
        self.check_filter_input(input)?;
        let (width, height) = filter.output_size(input.width(), input.height());
        let output = self.layer(width, height)?;
        self.apply_filter_into(filter, input, &output, clock)?;
        Ok(output)
    }

    /// [`Self::apply_filter`] into `output`, a [`Self::layer`] of the filter's
    /// [`VideoFilter::output_size`] for `input`, replacing what it held. The layer is
    /// reused, so a chain that runs every frame allocates no output texture, and a
    /// multi-pass kind keeps its scratch layers between calls at a steady size. Earlier
    /// submits that read
    /// `output` are ordered before this one, so they keep its old contents.
    ///
    /// # Errors
    /// As for [`Self::apply_filter`]; also [`RenderError::Invalid`] when `output` is not
    /// a layer this compositor made, is not the filter's output size, or is `input`.
    pub fn apply_filter_into(
        &mut self,
        filter: &mut VideoFilter,
        input: &Image,
        output: &Image,
        clock: FilterClock,
    ) -> Result<(), RenderError> {
        self.check_filter_input(input)?;
        let invalid = |detail: String| RenderError::Invalid {
            what: "filter output",
            detail,
        };
        if !output.owner().is(&self.owner) || output.target().is_none() {
            return Err(invalid(
                "the output is not a layer this compositor made".into(),
            ));
        }
        if output.texture() == input.texture() {
            return Err(invalid("a filter cannot draw into its own input".into()));
        }
        let size = filter.output_size(input.width(), input.height());
        if (output.width(), output.height()) != size {
            return Err(invalid(format!(
                "the output is {}x{}, but the filter draws {}x{}",
                output.width(),
                output.height(),
                size.0,
                size.1
            )));
        }
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-filter"),
            });
        // A filter can draw anywhere in its output, unless what it reads is bounded and
        // its kind says how far past that it draws (#1070).
        output.set_content(None);
        let padding = filter.pad();
        let (pass, scratch) = filter.parts();
        let bound = bound_of(input, padding, size, pass.reach());
        scratch.begin();
        let mut ctx = filter::Ctx {
            gpu: &self.gpu,
            maker: Maker {
                gpu: &self.gpu,
                layout: &self.image_layout,
                sampler: &self.sampler,
                owner: &self.owner,
                ingest: &self.ingest,
            },
            runtime: &mut self.filters,
            encoder: &mut encoder,
            clock,
            holds: &mut self.staged,
            scratch,
            bound,
        };
        ctx.holds.clear();
        let encoded = if padding == 0 {
            pass.encode(&mut ctx, input, output)
        } else {
            let grow = |side: u32| side.saturating_add(padding.saturating_mul(2));
            ctx.scratch(grow(input.width()), grow(input.height()))
                .and_then(|padded| {
                    filter::pad(&mut ctx, input, padding, &padded)?;
                    pass.encode(&mut ctx, &padded, output)
                })
        };
        ctx.hold(input);
        ctx.scratch.end();
        if let Err(err) = encoded {
            self.staged.clear();
            return Err(err);
        }
        // Staged only once recording succeeded, right before the submit it arms, and
        // handed back to the platform at the end of that submit, as `render` does: the
        // input's hold and those of the images the filter reads besides it, each once.
        // The submit lock is held from staging through the submit, as in `render`.
        let submitting = self.gpu.submit.lock();
        for (sync, _) in &self.staged {
            if let Err(err) = sync.stage(&self.gpu.queue) {
                self.staged.clear();
                return Err(RenderError::Sync(err));
            }
        }
        self.record_release(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        output.set_content(bound.map(|bound| {
            let [x0, y0, x1, y1] = bound.rect;
            Region {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            }
        }));
        drop(submitting);
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        Ok(())
    }

    fn check_filter_input(&self, input: &Image) -> Result<(), RenderError> {
        if input.owner().is(&self.owner) {
            Ok(())
        } else {
            Err(RenderError::Invalid {
                what: "image",
                detail: "the filter input was made by another compositor".into(),
            })
        }
    }

    /// The device images and targets are created on.
    pub fn device(&self) -> &wgpu::Device {
        &self.gpu.device
    }

    /// The queue [`Self::render`] submits to.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.gpu.queue
    }

    /// The lock held from staging a platform frame through the submit it arms on
    /// [`Self::queue`]: the context's on a platform context, else the compositor's own.
    pub(crate) fn submit_lock(&self) -> &jstream_platform::SubmitLock {
        &self.gpu.submit
    }

    /// The adapter the device was opened on.
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.gpu.adapter
    }

    /// The platform context this compositor was opened on ([`Self::on_context`]), or
    /// `None` when it opened a device of its own. A texture made on that context can be
    /// imported here; one from another context or device can't.
    pub fn context(&self) -> Option<&jstream_platform::ContextKey> {
        self.gpu.context.as_ref()
    }

    /// Why the device was lost, from wgpu's device-lost callback: `destroyed`, or `lost by
    /// the driver` (a reset after a hang, Windows' TDR, a removed GPU) with the driver's
    /// message. `None` while the device is usable. wgpu reports a loss when the device
    /// is polled, so this polls it once, without waiting.
    ///
    /// Once it is lost, calls on this compositor fail or draw nothing. Nothing comes
    /// back: open a new compositor ([`Self::open`]) and upload the images again.
    pub fn lost(&self) -> Option<String> {
        if self.gpu.faults.lost.get().is_none() {
            let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        }
        self.gpu.faults.lost.get().cloned()
    }

    /// The first uncaptured wgpu error on this compositor's device since the last call:
    /// one no call returned, such as a validation error in a submitted command. wgpu's
    /// default would panic on it. The compositor installs its own handler instead, so a
    /// lost device's invalid resources fail the frame rather than the thread; an error
    /// before a loss is also logged, as `uncaptured wgpu error`, once until it is taken:
    /// `render` takes it each frame, so one that recurs is logged once per frame.
    pub fn take_error(&self) -> Option<String> {
        self.gpu
            .faults
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Leaves the errors the context's shared watch keeps to the context's other
    /// compositors: [`Self::render`] no longer fails on one (#1268). wgpu keeps one
    /// uncaptured-error handler per device, so on a shared [`GpuContext`] an error from
    /// any of them would otherwise fail whichever renders next. A compositor that calls
    /// this catches its own errors instead, with [`Self::catch_errors`] around its work.
    /// The engine's snapshot and preview compositors do, so their errors never fail a
    /// program frame, and the program stage's never fail a snapshot or a preview frame.
    ///
    /// [`GpuContext`]: jstream_platform::GpuContext
    pub fn leave_shared_errors(&mut self) {
        self.shared_errors = false;
    }

    /// Starts catching the wgpu errors this thread raises on the compositor's device,
    /// validation and internal ones, until [`CaughtErrors::finish`]: they go to it rather
    /// than to the shared watch (#1268). wgpu's error scopes are per thread, so another
    /// thread's errors on the same device are not caught, and these never reach the
    /// other compositors on the context. Scopes nest: the filter passes' own, inside,
    /// still take their shader errors first.
    #[must_use = "the errors are caught until `finish`"]
    pub fn catch_errors(&self) -> CaughtErrors {
        // Popped in reverse, so validation is pushed last.
        let internal = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        CaughtErrors {
            validation,
            internal,
        }
    }

    /// A one-line description of the adapter, for logs and test output.
    pub fn adapter_description(&self) -> String {
        self.gpu.description()
    }

    /// A second compositor on this one's device and queue, as two `on_context` calls on
    /// one `GpuContext` would make. Tests use it to check that images do not cross over.
    #[cfg(test)]
    pub(crate) fn sibling(&self) -> Self {
        Self::new(Gpu {
            device: self.gpu.device.clone(),
            queue: self.gpu.queue.clone(),
            adapter: self.gpu.adapter.clone(),
            max_texture_dimension_2d: self.gpu.max_texture_dimension_2d,
            max_buffer_size: self.gpu.max_buffer_size,
            max_storage_binding: self.gpu.max_storage_binding,
            context: self.gpu.context.clone(),
            faults: Arc::clone(&self.gpu.faults),
            submit: self.gpu.submit.clone(),
        })
    }

    pub(crate) fn maker(&self) -> Maker<'_> {
        Maker {
            gpu: &self.gpu,
            layout: &self.image_layout,
            sampler: &self.sampler,
            owner: &self.owner,
            ingest: &self.ingest,
        }
    }

    fn new(gpu: Gpu) -> Self {
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let image_layout = image_layout(&gpu.device);
        let composite_shader = shader(
            &gpu.device,
            "jstream-render-composite",
            include_str!("composite.wgsl"),
        );
        let resolve_layout = resolve_layout(&gpu.device);
        // The backdrop is bound as the resolve's input is: one unfiltered FP16 texture.
        let blends = BlendPipelines::new(
            &gpu.device,
            &Layouts {
                image: &image_layout,
                backdrop: &resolve_layout,
                shader: &composite_shader,
            },
        );
        let resolve_shader = shader(
            &gpu.device,
            "jstream-render-resolve",
            include_str!("resolve.wgsl"),
        );
        let resolve = resolve_pipeline(&gpu.device, &resolve_layout, &resolve_shader);
        let filters = filter::Runtime::new(&gpu.device);
        let mip_pipelines = MipPipelines::new(&gpu.device, &image_layout);
        let models = ModelRuntime::new(&gpu);
        let vertex_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-vertices"),
            size: VERTEX_STRIDE * u64::from(MAX_VERTICES_PER_ITEM),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            gpu,
            owner: Owner::default(),
            sampler,
            image_layout,
            blends,
            backdrop: None,
            resolve_layout,
            resolve,
            targets: None,
            vertices: Vec::new(),
            vertex_buffer,
            draws: Vec::new(),
            drawn: None,
            order: Vec::new(),
            staged: Vec::new(),
            filters,
            mip_pipelines,
            models,
            readback_wait: std::time::Duration::ZERO,
            ingest: Mutex::new(None),
            shared_errors: true,
        }
    }

    fn prepare(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        self.vertices.clear();
        self.draws.clear();
        self.drawn = None;
        let canvas_w = scene.canvas.width() as f32;
        let canvas_h = scene.canvas.height() as f32;
        // Stacking order, or far to near within each run of items seen through one camera
        // that sorts, in one `depth_run` (ADR-0094 decision 4); equal depths keep
        // stacking order.
        self.order.clear();
        self.order.extend(
            scene
                .items
                .iter()
                .enumerate()
                .map(|(index, _)| (0.0, index)),
        );
        let mut start = 0;
        while start < scene.items.len() {
            let (camera, run) = (scene.items[start].camera, scene.items[start].depth_run);
            let end = scene.items[start..]
                .iter()
                .position(|item| item.camera != camera || item.depth_run != run)
                .map_or(scene.items.len(), |run| start + run);
            if let Some(camera) = camera.filter(|camera| camera.depth_sort) {
                let view = view_of(camera, scene)?;
                for slot in &mut self.order[start..end] {
                    slot.0 = place::depth(&view, scene.items[slot.1].placement);
                }
                // Unstable, so it allocates nothing; the index breaks ties, which keeps
                // stacking order among items at one depth.
                self.order[start..end]
                    .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            }
            start = end;
        }
        let mut view: Option<(jstream_protocol::SceneCamera, jstream_protocol::CameraView)> = None;
        for at in 0..self.order.len() {
            let index = self.order[at].1;
            let item = &scene.items[index];
            // Checked for hidden items too, so a scene is valid or not regardless of
            // visibility. Another compositor's bind group would reach a wgpu panic.
            if !item.image.owner().is(&self.owner) {
                return Err(RenderError::Invalid {
                    what: "image",
                    detail: format!("item {index} was made by another compositor"),
                });
            }
            if !item.opacity.is_finite()
                || !(0.0..=jstream_protocol::MAX_OPACITY).contains(&item.opacity)
            {
                return Err(RenderError::Invalid {
                    what: "opacity",
                    detail: format!("{} is not a finite value in 0..=4", item.opacity),
                });
            }
            self.blends.steps(item.blend)?;
            let (width, height) = item.image.extent();
            if let Some(camera) = item.camera
                && view.as_ref().is_none_or(|(seen, _)| *seen != camera)
            {
                view = Some((camera, view_of(camera, scene)?));
            }
            let seen = item.camera.and(view.as_ref()).map(|(_, view)| view);
            let mut shape = place::shape_seen(width, height, item.placement, item.crop, seen)?;
            if !item.visible || item.opacity == 0.0 {
                continue;
            }
            let Some(shape) = shape.as_mut() else {
                continue;
            };
            let rect = if blend::reads_backdrop(item.blend) {
                match covered(shape, scene.canvas.width(), scene.canvas.height()) {
                    Some(rect) => Some(rect),
                    // Wholly off the target: nothing to draw.
                    None => continue,
                }
            } else {
                None
            };
            let too_many = || RenderError::TooManyItems {
                count: scene.items.len(),
            };
            let first_vertex = u32::try_from(self.vertices.len() as u64 / VERTEX_STRIDE)
                .ok()
                .filter(|first| first.checked_add(MAX_VERTICES_PER_ITEM).is_some())
                .ok_or_else(too_many)?;
            // A 3D item, and a 2D one drawn small (#1217), samples a chain of its crop,
            // so its texture coordinates are mapped from the source's into the chain's.
            let texels = (item.image.width(), item.image.height());
            let (least, most) = if shape.is_3d() {
                (0.0, f32::INFINITY)
            } else {
                shape.minification(texels)
            };
            // How much of its frame's resolution the image needed, for an upload drawn
            // at the size it is shown (`ResourceCache::upload_drawn`, #1139).
            item.image.record_draw(least);
            // A layer that is transparent outside what its scene drew is drawn only over
            // that (#1070): the rest adds nothing in the modes that leave what is below
            // a transparent texel as it was. Not with a chain, whose levels spread the
            // drawn texels past the margin, nor in a mode that reads the backdrop.
            if most <= MINIFIED
                && matches!(
                    item.blend,
                    BlendMode::Normal | BlendMode::Additive | BlendMode::Screen
                )
                && let Some(content) = item.image.content()
            {
                let (w, h) = (texels.0 as f32, texels.1 as f32);
                let keep = [
                    content.x as f32 / w,
                    content.y as f32 / h,
                    (content.x + content.width) as f32 / w,
                    (content.y + content.height) as f32 / h,
                ];
                if !shape.trim_to(keep) {
                    continue;
                }
            }
            // A 2D item minified `2^k` times or more along both axes never samples
            // the levels finer than `k`.
            let skip = if least >= 2.0 {
                // `as` saturates, so even an infinite ratio (a degenerate quad) is in range.
                (least.log2().floor() as u32).min(MAX_SKIP)
            } else {
                0
            };
            let mips = (most > MINIFIED).then(|| {
                let image = item.image;
                // The crop counts in the extent; the chain is built from texels.
                let region = shape.crop.map_or_else(
                    || Region::whole(texels.0, texels.1),
                    |crop| crop.in_texels(image.extent(), texels),
                );
                let chain = self.chain_of(image, region, skip);
                let (w, h) = (image.width() as f32, image.height() as f32);
                shape.remap_uv([
                    region.x as f32 / w,
                    region.y as f32 / h,
                    region.width as f32 / w,
                    region.height as f32 / h,
                ]);
                chain
            });
            // A chain holds premultiplied linear whatever the source held.
            let alpha = if mips.is_some() {
                Alpha::Premultiplied
            } else {
                item.image.alpha()
            };
            if let Some(reach) = covered(shape, scene.canvas.width(), scene.canvas.height()) {
                self.drawn = Some(self.drawn.map_or(reach, |drawn| drawn.union(reach)));
            }
            let vertex_count = push_shape(
                &mut self.vertices,
                shape,
                canvas_w,
                canvas_h,
                item.opacity,
                alpha,
            );
            self.draws.push(Draw {
                index,
                first_vertex,
                vertex_count,
                mips,
                blend: item.blend,
                rect,
            });
        }
        let limit = self.gpu.max_texture_dimension_2d;
        let width = scene.canvas.width();
        let height = scene.canvas.height();
        if width > limit || height > limit {
            return Err(RenderError::TooLarge {
                what: "canvas",
                width,
                height,
                limit,
            });
        }
        Ok(())
    }

    fn stage_sources(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        self.staged.clear();
        for draw in &self.draws {
            let image = scene.items[draw.index].image;
            let Some(sync) = image.sync() else {
                continue;
            };
            if self.staged.iter().any(|(held, _)| Arc::ptr_eq(held, sync)) {
                continue;
            }
            sync.stage(self.queue()).map_err(RenderError::Sync)?;
            self.staged
                .push((Arc::clone(sync), image.texture().clone()));
        }
        Ok(())
    }

    /// Ends the submit by moving every staged texture to `PRESENT`, which on D3D12 is
    /// the `COMMON` state the platform's other APIs read a shared texture in
    /// ([`FrameSync::stage`]). Nothing to do for a scene without frame holds.
    fn record_release(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.staged.is_empty() {
            return;
        }
        encoder.transition_resources(
            std::iter::empty(),
            self.staged
                .iter()
                .map(|(_, texture)| wgpu::TextureTransition {
                    texture,
                    selector: None,
                    state: wgpu::TextureUses::PRESENT,
                }),
        );
    }

    fn ensure_vertex_buffer(&mut self) -> Result<(), RenderError> {
        let needed = (self.vertices.len() as u64).max(4);
        if self.vertex_buffer.size() >= needed {
            return Ok(());
        }
        let size = needed
            .next_power_of_two()
            .max(VERTEX_STRIDE * u64::from(MAX_VERTICES_PER_ITEM));
        if size > self.gpu.max_buffer_size {
            return Err(RenderError::TooManyItems {
                count: self.draws.len(),
            });
        }
        self.vertex_buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-vertices"),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(())
    }

    fn ensure_targets(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return Ok(());
        }
        let working = texture(
            &self.gpu.device,
            "jstream-render-working",
            width,
            height,
            wgpu::TextureFormat::Rgba16Float,
            // `COPY_SRC` for the backdrop (ADR-0082).
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let working_view = working.create_view(&wgpu::TextureViewDescriptor::default());
        let output = texture(
            &self.gpu.device,
            "jstream-render-output",
            width,
            height,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_bind = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-resolve"),
                layout: &self.resolve_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&working_view),
                }],
            });
        self.targets = Some(Targets {
            width,
            height,
            working,
            working_view,
            output,
            output_view,
            resolve_bind,
        });
        Ok(())
    }

    /// Validates `scene`, builds its vertices and writes the vertex buffer. The caller
    /// stages the frame holds after this and every other step that can fail, so an
    /// error never leaves a hold staged without the submit it arms.
    fn load(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        color::validate(scene.background)?;
        self.prepare(scene)?;
        self.ensure_vertex_buffer()?;
        if self.draws.iter().any(|draw| draw.rect.is_some()) {
            self.ensure_backdrop(scene.canvas.width(), scene.canvas.height());
        }
        if !self.vertices.is_empty() {
            self.gpu
                .queue
                .write_buffer(&self.vertex_buffer, 0, &self.vertices);
        }
        Ok(())
    }

    /// `image`'s mip chain of `region` starting `skip` levels down, made the first time
    /// it is placed in 3D or drawn small with it.
    fn chain_of(&self, image: &Image, region: Region, skip: u32) -> Arc<Mips> {
        image.chain(region, skip, || {
            Mips::new(
                &self.gpu.device,
                &self.gpu.queue,
                &self.image_layout,
                &self.mip_pipelines,
                region,
                skip,
            )
        })
    }

    /// Rebuilds each mip chain a 3D item draws whose image changed since its last
    /// build (an upload rewritten, a layer drawn into, an import handed a new frame),
    /// once however many items and calls draw it (ADR-0050 decision 5).
    fn record_mips(&self, encoder: &mut wgpu::CommandEncoder, scene: &Scene<'_>) {
        for draw in &self.draws {
            let Some(mips) = &draw.mips else { continue };
            let image = scene.items[draw.index].image;
            if mips.claim(image) {
                mips.record(encoder, &self.mip_pipelines, image);
            }
        }
    }

    /// Grows the backdrop to hold `width` by `height` texels at least.
    fn ensure_backdrop(&mut self, width: u32, height: u32) {
        let (have_w, have_h) = self.backdrop.as_ref().map_or((0, 0), |backdrop| {
            (backdrop.texture.width(), backdrop.texture.height())
        });
        if have_w >= width && have_h >= height {
            return;
        }
        let texture = texture(
            &self.gpu.device,
            "jstream-render-backdrop",
            have_w.max(width),
            have_h.max(height),
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-backdrop"),
                layout: &self.resolve_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
        self.backdrop = Some(Backdrop { texture, bind });
    }

    /// The composite pass: clear `target` (`Rgba16Float`) to the background and draw
    /// the items, premultiplied linear. An item whose mode reads the backdrop ends the
    /// pass: the texels it covers are copied into the backdrop, and a new pass draws it
    /// and the items after it (ADR-0082).
    fn record_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        (view, texture): (&wgpu::TextureView, &wgpu::Texture),
        scene: &Scene<'_>,
    ) {
        let clear = premultiplied_linear(scene.background);
        let clear = wgpu::Color {
            r: f64::from(clear[0]),
            g: f64::from(clear[1]),
            b: f64::from(clear[2]),
            a: f64::from(clear[3]),
        };
        let reads = |draw: &Draw| draw.rect.is_some();
        let count = self.draws.len();
        let mut cleared = false;
        let mut first = 0;
        loop {
            if let Some(rect) = self.draws.get(first).and_then(|draw| draw.rect) {
                if !cleared {
                    self.record_draws(encoder, view, Some(clear), &[], scene);
                    cleared = true;
                }
                self.record_backdrop_copy(encoder, texture, rect);
            }
            let end = (first + 1..count)
                .find(|&index| reads(&self.draws[index]))
                .unwrap_or(count.max(first));
            let load = (!cleared).then_some(clear);
            self.record_draws(encoder, view, load, &self.draws[first..end], scene);
            cleared = true;
            if end >= count {
                break;
            }
            first = end;
        }
    }

    /// One render pass over `target` drawing `draws`, after clearing it to `clear` when
    /// that is set.
    fn record_draws(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clear: Option<wgpu::Color>,
        draws: &[Draw],
        scene: &Scene<'_>,
    ) {
        let load = clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-composite"),
            color_attachments: &[Some(color_attachment(target, load))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if draws.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        for draw in draws {
            let image = scene.items[draw.index].image;
            let bind_group = match &draw.mips {
                Some(mips) => &mips.bind_group,
                None => image.bind_group(),
            };
            let vertices = draw.first_vertex..draw.first_vertex + draw.vertex_count;
            // `prepare` checked every item's mode.
            let Ok(steps) = self.blends.steps(draw.blend) else {
                continue;
            };
            match steps {
                Steps::Fixed(pipelines) => {
                    for pipeline in pipelines {
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, bind_group, &[]);
                        pass.draw(vertices.clone(), 0..1);
                    }
                }
                Steps::Backdrop(pipeline) => {
                    let Some(backdrop) = &self.backdrop else {
                        continue;
                    };
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.set_bind_group(1, &backdrop.bind, &[]);
                    pass.draw(vertices, 0..1);
                }
            }
        }
    }

    /// Copies `rect` of `target` into the same texels of the backdrop.
    fn record_backdrop_copy(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        rect: Rect,
    ) {
        let Some(backdrop) = &self.backdrop else {
            return;
        };
        let origin = wgpu::Origin3d {
            x: rect.x,
            y: rect.y,
            z: 0,
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &backdrop.texture,
                mip_level: 0,
                origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// The resolve pass: the FP16 working target to the straight sRGB output.
    fn record_resolve(&self, encoder: &mut wgpu::CommandEncoder, targets: &Targets) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-resolve"),
            color_attachments: &[Some(color_attachment(
                &targets.output_view,
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            ))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.resolve);
        pass.set_bind_group(0, &targets.resolve_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Records the copy of `output`, this compositor's, into `buffer`, a
    /// [`Self::readback_buffer`] of its size, rows padded as the readback's are.
    pub(crate) fn record_copy_out(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        output: &Output,
        buffer: &wgpu::Buffer,
    ) {
        let (width, height) = (output.width(), output.height());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: output.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes(width)),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub(crate) fn readback_buffer(
        &self,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Buffer, RenderError> {
        let stride = u64::from(padded_row_bytes(width));
        let size = stride
            .checked_mul(u64::from(height))
            .filter(|size| *size <= self.gpu.max_buffer_size)
            .ok_or(RenderError::Readback {
                detail: format!("the {width}x{height} readback does not fit in a buffer"),
            })?;
        Ok(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }))
    }
}

#[cfg(test)]
impl Compositor {
    /// Lowers the buffer limit, as a smaller adapter's would be.
    pub(crate) fn cap_buffer_size(&mut self, bytes: u64) {
        self.gpu.max_buffer_size = bytes;
    }

    /// Lowers the storage-binding limit, as a smaller adapter's would be.
    pub(crate) fn cap_storage_binding(&mut self, bytes: u64) {
        self.gpu.max_storage_binding = bytes;
    }
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("adapter", &self.adapter_description())
            .finish_non_exhaustive()
    }
}

/// `camera` seen on `scene`'s canvas, or [`RenderError::Invalid`] for a camera the
/// collection would refuse.
fn view_of(
    camera: jstream_protocol::SceneCamera,
    scene: &Scene<'_>,
) -> Result<jstream_protocol::CameraView, RenderError> {
    camera
        .view(jstream_protocol::Canvas {
            width: scene.canvas.width(),
            height: scene.canvas.height(),
        })
        .map_err(|error| RenderError::Invalid {
            what: "camera",
            detail: error.to_string(),
        })
}

/// The texels of a `width` by `height` target that `shape` can touch, a texel of margin
/// around its drawn polygon, or `None` when it is wholly off the target.
fn covered(shape: &Shape, width: u32, height: u32) -> Option<Rect> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for vertex in shape.vertices() {
        for axis in 0..2 {
            min[axis] = min[axis].min(vertex.canvas[axis]);
            max[axis] = max[axis].max(vertex.canvas[axis]);
        }
    }
    let clamp = |value: f32, limit: u32| -> u32 {
        // Not finite only for a degenerate polygon, which then covers nothing.
        if value.is_nan() {
            0
        } else {
            value.clamp(0.0, limit as f32) as u32
        }
    };
    let x0 = clamp((min[0] - 1.0).floor(), width);
    let y0 = clamp((min[1] - 1.0).floor(), height);
    let x1 = clamp((max[0] + 1.0).ceil(), width);
    let y1 = clamp((max[1] + 1.0).ceil(), height);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// Where a filter that reaches `reach` input pixels past what it reads can draw, in its
/// output of `size`: `input`'s content moved in by the `padding` and grown by the reach
/// and a pixel, clipped (#1070). `None` when the input's content isn't known, the kind
/// gives no reach, or it doesn't keep its padded input's size. An input with nothing in it
/// gives an empty bound.
fn bound_of(
    input: &Image,
    padding: u32,
    size: (u32, u32),
    reach: Option<f32>,
) -> Option<filter::Bound> {
    let content = input.content()?;
    let reach = reach.filter(|reach| reach.is_finite() && *reach >= 0.0)?;
    let grow = |side: u32| side.saturating_add(padding.saturating_mul(2));
    if size != (grow(input.width()), grow(input.height())) {
        return None;
    }
    if content.width == 0 || content.height == 0 {
        return Some(filter::Bound {
            rect: [0; 4],
            frame: size,
        });
    }
    let reach = (reach.ceil() as u32).saturating_add(1);
    let low = |start: u32| start.saturating_add(padding).saturating_sub(reach);
    let high = |end: u32, side: u32| end.saturating_add(padding).saturating_add(reach).min(side);
    Some(filter::Bound {
        rect: [
            low(content.x),
            low(content.y),
            high(content.x + content.width, size.0),
            high(content.y + content.height, size.1),
        ],
        frame: size,
    })
}

/// Appends one item's triangles, a fan from its first vertex, and returns how many
/// vertices that is.
fn push_shape(
    buf: &mut Vec<u8>,
    shape: &Shape,
    canvas_w: f32,
    canvas_h: f32,
    opacity: f32,
    alpha: Alpha,
) -> u32 {
    // The shader's `mode`: how it turns a filtered sample into premultiplied linear.
    let mode = alpha.shader_mode();
    let vertices = shape.vertices();
    let mut count = 0;
    for second in 1..vertices.len().saturating_sub(1) {
        for vertex in [vertices[0], vertices[second], vertices[second + 1]] {
            let [x, y] = vertex.canvas;
            push_f32(buf, x / canvas_w * 2.0 - 1.0);
            push_f32(buf, 1.0 - y / canvas_h * 2.0);
            push_f32(buf, vertex.uv[0]);
            push_f32(buf, vertex.uv[1]);
            push_f32(buf, opacity);
            push_f32(buf, mode);
            push_f32(buf, vertex.w);
            for value in shape.edges.as_flattened() {
                push_f32(buf, *value);
            }
            for value in shape.uv_bounds {
                push_f32(buf, value);
            }
            for value in shape.dof {
                push_f32(buf, value);
            }
            count += 1;
        }
    }
    count
}

fn push_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn padded_row_bytes(width: u32) -> u32 {
    let row = width.saturating_mul(4);
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    row.div_ceil(align) * align
}

fn map_snapshot(
    device: &wgpu::Device,
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
) -> Result<Snapshot, RenderError> {
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
    recv.recv_timeout(Duration::from_secs(30))
        .map_err(|_| RenderError::Readback {
            detail: "the readback map did not finish within 30s".into(),
        })?
        .map_err(|e| RenderError::Readback {
            detail: e.to_string(),
        })?;
    let stride = padded_row_bytes(width) as usize;
    let snapshot = {
        let view = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| RenderError::Readback {
                detail: e.to_string(),
            })?;
        Snapshot::from_padded(width, height, &view, stride)
    };
    buffer.unmap();
    Ok(snapshot)
}

fn shader(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn image_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("jstream-render-image"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn resolve_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("jstream-render-resolve"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    })
}

fn resolve_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("jstream-render-resolve"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("jstream-render-resolve"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
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
        usage,
        view_formats: &[],
    })
}

fn color_attachment(
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load,
            store: wgpu::StoreOp::Store,
        },
    }
}

/// The wgpu errors a thread raises on a compositor's device between
/// [`Compositor::catch_errors`] and [`Self::finish`]. Dropping it without `finish` drops
/// them. It stays on the thread that made it: wgpu's error scopes are per thread.
#[derive(Debug)]
pub struct CaughtErrors {
    validation: wgpu::ErrorScopeGuard,
    internal: wgpu::ErrorScopeGuard,
}

impl CaughtErrors {
    /// Stops catching, and returns the first error caught: a validation error first,
    /// then an internal one. wgpu has reported each by the time its call returned.
    pub fn finish(self) -> Option<String> {
        let validation = crate::gpu::ready(self.validation.pop()).flatten();
        let internal = crate::gpu::ready(self.internal.pop()).flatten();
        validation.or(internal).map(|error| error.to_string())
    }
}
