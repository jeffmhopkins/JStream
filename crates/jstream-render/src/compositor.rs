//! The two passes: composite in FP16 linear light, then resolve to straight sRGB.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_platform::FrameSync;

use crate::blend::{self, BlendPipelines, Layouts, Steps};
use crate::color::{self, premultiplied_linear, premultiplied_srgb};
use crate::error::RenderError;
use crate::filter::{self, FilterClock, VideoFilter};
use crate::gpu::{Faults, Gpu, OpenOptions};
use crate::image::{self, Alpha, Image, Maker, Owner};
use crate::ingest::Ingest;
use crate::mips::{MAX_SKIP, MipPipelines, Mips, Region};
use crate::model::{CardDraw, GpuModel, ModelDraw, ModelRuntime, Panoramas};
use crate::place::{self, MAX_VERTICES, Shape};
use crate::scene::{Output, Placement, Scene, Snapshot};
use crate::wipe::Wipe;
use jstream_protocol::{BlendMode, Blending};

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
    /// Whether it is a premultiplied linear image of the canvas's size laid over the
    /// whole canvas texel for texel, opaque and in `normal`: a scene of that alone is
    /// resolved straight from the image ([`Compositor::record_layer_resolve`]).
    whole: bool,
}

/// What a layer's last [`Compositor::render_into`] drew into it, kept on the layer
/// ([`Image::drawn_as`]) so the same draw of unchanged images is not made again
/// (ADR-0194).
#[derive(Default)]
pub(crate) struct Drawn {
    /// The layer's generation once it was drawn: any write since moves it on.
    at: u64,
    blending: Blending,
    background: [u32; 4],
    /// The vertices as they were written: each item's place, texture coordinates,
    /// opacity, alpha mode, edges and blur.
    vertices: Vec<u8>,
    draws: Vec<DrawnDraw>,
}

/// One draw of a [`Drawn`]: what its vertices don't say.
#[derive(PartialEq)]
struct DrawnDraw {
    first_vertex: u32,
    vertex_count: u32,
    /// The image and the texels it held.
    image: image::Seen,
    /// Its mip chain, by identity: kept weakly, so no other chain takes its place.
    mips: Option<WeakMips>,
    blend: BlendMode,
    rect: Option<Rect>,
}

/// A mip chain remembered without keeping it, equal only to itself.
struct WeakMips(std::sync::Weak<Mips>);

impl PartialEq for WeakMips {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl DrawnDraw {
    fn of(draw: &Draw, item: &crate::Item<'_>) -> Self {
        Self {
            first_vertex: draw.first_vertex,
            vertex_count: draw.vertex_count,
            image: item.image.seen(),
            mips: draw
                .mips
                .as_ref()
                .map(|mips| WeakMips(Arc::downgrade(mips))),
            blend: draw.blend,
            rect: draw.rect,
        }
    }
}

/// A background colour as the bits compared for [`Drawn`].
fn background_bits(color: crate::Color) -> [u32; 4] {
    [color.r, color.g, color.b, color.a].map(f32::to_bits)
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

/// How many sets of targets of sizes other than the last one rendered at are kept.
pub(crate) const SPARE_TARGETS: usize = 3;

/// How many renders a set of targets is kept for after the last render into it: a few
/// seconds of an output's picture and a preview's, and long enough for a preview that
/// gives way to the outputs to come back to its own.
pub(crate) const SPARE_RENDERS: u64 = 240;

/// The copy of what is below an item that the modes reading it sample (ADR-0082). It
/// only grows, so targets of different sizes in one frame share it, each copying to the
/// same texels it reads from.
struct Backdrop {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
}

/// What a scene pass with a model's depth draws with (ADR-0107 decision 7), made the
/// first time a scene has one: the blend pipelines of each space, tested against the
/// depth target (`tested`, for items that test) or not (`untested`, for a model's own
/// layer), and the pipeline that writes a model's depth.
struct DepthPipelines {
    tested: [BlendPipelines; 2],
    untested: [BlendPipelines; 2],
    writer: wgpu::RenderPipeline,
}

/// A scene pass's depth target, of the canvas size it was made for.
struct SceneDepth {
    size: (u32, u32),
    view: wgpu::TextureView,
}

struct Targets {
    width: u32,
    height: u32,
    /// For a set kept aside, [`Compositor::renders`] when it was last rendered into.
    used: u64,
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
    /// Adds a model's view depth layer to a scene's view depth ([`crate::Item::depth`]).
    depth_copier: wgpu::RenderPipeline,
    /// The same, drawing into an sRGB-encoded target (ADR-0126 §3).
    encoded_blends: BlendPipelines,
    /// The composite shader, which [`DepthPipelines`] are built from when first needed.
    composite_shader: wgpu::ShaderModule,
    depth_pipelines: Option<DepthPipelines>,
    /// The depth target of the scene being drawn, when it places a model's depth
    /// ([`crate::Item::depth`]); `None` for a scene that doesn't. Kept between scenes of
    /// one size.
    scene_depth: Option<SceneDepth>,
    /// Whether the scene [`Self::load`] prepared draws with `scene_depth`.
    depth_on: bool,
    /// The space every scene this compositor draws mixes in.
    blending: Blending,
    backdrop: Option<Backdrop>,
    resolve_layout: wgpu::BindGroupLayout,
    /// The resolve's input: the working target and a nearest sampler, since sampling
    /// it costs a software renderer less than loading its texels (#1449).
    resolve_input: wgpu::BindGroupLayout,
    resolve_sampler: wgpu::Sampler,
    resolve: wgpu::RenderPipeline,
    /// The resolve of a working target left sRGB-encoded (`fs_encoded`).
    resolve_encoded: wgpu::RenderPipeline,
    /// Decodes an sRGB-encoded target back to linear light, from the backdrop.
    decode: wgpu::RenderPipeline,
    /// Resolves a scene that is one whole layer straight from the layer (`fs_layer`),
    /// with the background and the space it is laid over in in `over`.
    resolve_layer: wgpu::RenderPipeline,
    over: wgpu::Buffer,
    over_bind: wgpu::BindGroup,
    /// Whether the last [`Self::render`] resolved straight from a layer.
    #[cfg(test)]
    resolved_layer: bool,
    /// The targets of the size last rendered at.
    targets: Option<Targets>,
    /// The targets of the other sizes rendered at lately, so a tick that renders at two
    /// sizes (an output's picture and a preview's) makes no texture: at most
    /// [`SPARE_TARGETS`], each let go [`SPARE_RENDERS`] renders after its last.
    spare: Vec<Targets>,
    /// How many times [`Self::render`] has readied its targets.
    renders: u64,
    /// How many sets of targets were made, for tests.
    #[cfg(test)]
    targets_made: usize,
    /// How many [`Self::render_into`] calls left their layer as it was, for tests.
    #[cfg(test)]
    kept_layers: usize,
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
    /// Writes transparent over part of a layer, in place of clearing all of it (#1449).
    wipe: Wipe,
    mip_pipelines: MipPipelines,
    models: ModelRuntime,
    /// Depth of field on models (ADR-0165): its passes and the layers a model with depth
    /// of field is drawn into first, made on first use.
    model_focus: ModelFocusRun,
    /// The sky backdrop's pipeline (ADR-0167), built the first time one draws.
    sky: Option<crate::sky::SkyRuntime>,
    /// The reflection probes' resampling pipeline (ADR-0170 decision 3), built the first
    /// time a probe resamples.
    probe: Option<crate::probe::ProbeRuntime>,
    /// How long the last [`Self::render`] waited for its readback (ADR-0103).
    readback_wait: std::time::Duration,
    /// Times effect-pack passes once [`Self::time_passes`] asked, on a device that can
    /// (#349, `timer.rs`).
    timer: Option<crate::timer::PassTimer>,
    /// The CPU frame conversion (ADR-0117 §2), built on the first upload. Behind a lock
    /// because uploads take `&self`; only the thread drawing with the compositor takes it.
    ingest: Mutex<Option<Ingest>>,
    /// Whether each call collects its own uncaptured wgpu errors ([`Self::keeping_own_errors`])
    /// instead of leaving them to the device's shared slot.
    own_errors: bool,
}

/// Where a call's uncaptured wgpu errors go.
enum Errors {
    /// The device's handler, which keeps the first for [`Compositor::take_error`].
    Shared,
    /// Error scopes this thread pushed for the call, which catch every error it raises.
    Scoped(Scopes),
}

/// The scopes of one call. Fields drop in order, so the innermost scope is popped first
/// when a call returns early, as wgpu requires.
struct Scopes {
    validation: wgpu::ErrorScopeGuard,
    internal: wgpu::ErrorScopeGuard,
    out_of_memory: wgpu::ErrorScopeGuard,
}

impl Errors {
    /// Opens scopes on `device` when `own`.
    fn open(device: &wgpu::Device, own: bool) -> Self {
        if !own {
            return Self::Shared;
        }
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        Self::Scoped(Scopes {
            validation,
            internal,
            out_of_memory,
        })
    }

    /// Pops the scopes, innermost first: the first error any of them caught. `None` for
    /// [`Errors::Shared`], whose errors are taken from the device's slot instead.
    fn end(self) -> Option<String> {
        let Self::Scoped(scopes) = self else {
            return None;
        };
        let Scopes {
            validation,
            internal,
            out_of_memory,
        } = scopes;
        let validation = crate::gpu::scope_error(validation);
        let internal = crate::gpu::scope_error(internal);
        let out_of_memory = crate::gpu::scope_error(out_of_memory);
        validation.or(internal).or(out_of_memory)
    }

    /// `result`, or [`RenderError::Uncaptured`] for an error the call's scopes caught
    /// when it succeeded. A shared-slot call's errors stay in the slot, for the next
    /// `render`.
    fn close<T>(self, result: Result<T, RenderError>) -> Result<T, RenderError> {
        let caught = self.end();
        let value = result?;
        match caught {
            Some(detail) => Err(RenderError::Uncaptured { detail }),
            None => Ok(value),
        }
    }
}

/// Uploads 3D models for the [`Compositor`] that made it
/// ([`Compositor::model_uploader`]), on whichever thread calls it. Cheap to clone.
#[derive(Clone)]
pub struct ModelUploader {
    kit: Arc<crate::model::UploadKit>,
    own_errors: bool,
}

impl ModelUploader {
    /// Uploads `model` as [`Compositor::upload_model`] does. It submits nothing: what it
    /// wrote goes with the next submit on the compositor's queue, so the model can be
    /// drawn as soon as this returns.
    ///
    /// # Errors
    /// What [`Compositor::upload_model`] reports.
    pub fn upload(&self, model: &jstream_media::model::Model) -> Result<GpuModel, RenderError> {
        let errors = Errors::open(self.kit.device(), self.own_errors);
        errors.close(self.kit.upload(model))
    }
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

    /// This compositor, with each call collecting the uncaptured wgpu errors it raises
    /// itself (#1268): error scopes on the calling thread around the call, so they come
    /// back as that call's [`RenderError::Uncaptured`] and never reach the device's
    /// shared slot, and `render` no longer takes the slot. For a compositor that shares
    /// a context with another one that renders on another thread (the preview's, the
    /// snapshot thread's, beside the program stage's): wgpu keeps one uncaptured-error
    /// handler per device, so without it either one's error fails the other's next
    /// frame. wgpu's error scopes are per thread, so the other threads' errors still
    /// reach the slot, and a loss is still seen by [`Self::lost`].
    #[must_use]
    pub fn keeping_own_errors(mut self) -> Self {
        self.own_errors = true;
        self
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
        let errors = self.errors();
        errors.close(image::upload(&self.maker(), width, height, rgba))
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
        let errors = self.errors();
        errors.close(image::upload_into(&self.maker(), width, height, rgba, to))
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
        let errors = self.errors();
        errors.close(image::import(&self.maker(), texture, sync, alpha))
    }

    /// [`Self::import`] of a [`jstream_platform::GpuFrame`], keeping its [`FrameSync`] hold.
    ///
    /// # Errors
    /// As for [`Self::import`].
    pub fn import_frame(&self, frame: &jstream_platform::GpuFrame) -> Result<Image, RenderError> {
        let errors = self.errors();
        errors.close(image::import_frame(&self.maker(), frame))
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
        let errors = self.errors();
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
        // A scene that is one finished layer over the whole canvas (a scene's filter
        // chain's output, a master or an output chain's) is resolved straight from the
        // layer: no composite of it into the working target first.
        let whole = match self.draws.as_slice() {
            [draw] if draw.whole => Some(scene.items[draw.index].image),
            _ => None,
        };
        #[cfg(test)]
        {
            self.resolved_layer = whole.is_some();
        }
        match whole {
            Some(layer) => self.record_layer_resolve(&mut encoder, targets, scene, layer),
            None => {
                self.record_mips(&mut encoder, scene);
                self.record_composite(
                    &mut encoder,
                    (&targets.working_view, &targets.working),
                    scene,
                    None,
                    // The resolve reads the encoded colour itself.
                    false,
                );
                self.record_resolve(&mut encoder, targets);
            }
        }
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
        self.gpu.submit_work(encoder.finish());
        drop(submitting);
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        // wgpu validates the commands as they are recorded and submitted, so an error in
        // this frame has been raised by now. On the shared slot, one from an earlier call
        // fails this frame too; a compositor that keeps its own errors sees only this
        // frame's.
        let pending = match errors {
            Errors::Shared => self.take_error(),
            scoped @ Errors::Scoped(..) => scoped.end(),
        };
        if let Some(detail) = pending {
            return Err(RenderError::Uncaptured { detail });
        }

        let output = targets.output.clone();
        let waiting = std::time::Instant::now();
        let pixels = match readback_buffer {
            Some(buffer) => Some(map_snapshot(&self.gpu, buffer, width, height)?),
            None => None,
        };
        self.readback_wait = waiting.elapsed();
        Ok(Output::new(output, width, height, pixels))
    }

    /// Sets the space every scene this compositor draws from now on mixes in: the
    /// project's [`Blending`] (ADR-0126). A new compositor mixes in linear light. In
    /// [`Blending::Srgb`] each composite target holds sRGB-encoded colour while its
    /// items draw. A layer's is decoded by one pass after the last, so a layer holds
    /// premultiplied linear light either way; a frame's own is decoded by its resolve.
    pub fn set_blending(&mut self, blending: Blending) {
        self.blending = blending;
    }

    /// The space set by [`Self::set_blending`].
    pub fn blending(&self) -> Blending {
        self.blending
    }

    /// How long the last [`Self::render`] spent waiting for its readback: the GPU
    /// finishing the frame and the copy to the CPU. Zero for a render with no readback.
    /// A profile counts it apart from the composite it follows (ADR-0103).
    pub fn readback_wait(&self) -> std::time::Duration {
        self.readback_wait
    }

    /// Whether the last [`Self::render`] resolved its scene straight from a layer.
    #[cfg(test)]
    pub(crate) fn resolved_layer(&self) -> bool {
        self.resolved_layer
    }

    /// How many [`Self::render_into`] calls left their layer as it was.
    #[cfg(test)]
    pub(crate) fn kept_layers(&self) -> usize {
        self.kept_layers
    }

    /// How many sets of targets [`Self::render`] has made, and how many are kept aside.
    #[cfg(test)]
    pub(crate) fn targets_kept(&self) -> (usize, usize) {
        (self.targets_made, self.spare.len())
    }

    /// How many texels the last composite's draws could reach (#1070).
    #[cfg(test)]
    pub(crate) fn drawn_texels(&self) -> u32 {
        self.drawn.map_or(0, |rect| rect.width * rect.height)
    }

    /// How many texels the last composite's decode pass covered in a `size` target
    /// cleared `transparent` or not, in browser-style blending.
    #[cfg(test)]
    pub(crate) fn decoded_texels(&self, transparent: bool, size: (u32, u32)) -> u32 {
        self.decoded(transparent, size)
            .map_or(0, |rect| rect.width * rect.height)
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
        let errors = self.errors();
        errors.close(image::layer(&self.maker(), width, height))
    }

    /// A new depth layer of `width` by `height`: an [`Image`] of one channel
    /// that [`Self::render_view_depth`] and [`Self::draw_model_depth`] draw `1 / w` into,
    /// for a scene's view depth, a model's own depth and a light's shadow map. It is
    /// read as any image is, red holding the value, and takes a quarter of a
    /// [`Self::layer`]'s memory. Nothing else draws into one: a colour draw given a depth
    /// layer, or a depth draw given a colour layer, is [`RenderError::Invalid`]. Its
    /// contents are undefined until the first draw.
    ///
    /// # Errors
    /// As for [`Self::layer`].
    pub fn depth_layer(&self, width: u32, height: u32) -> Result<Image, RenderError> {
        let errors = self.errors();
        errors.close(image::depth_layer(&self.maker(), width, height))
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
        let errors = self.errors();
        let drawn = self.draw_into(scene, target);
        errors.close(drawn)
    }

    /// [`Self::render_into`], its errors not yet collected.
    fn draw_into(&mut self, scene: &Scene<'_>, target: &Image) -> Result<(), RenderError> {
        let view = self.scene_target(scene, target, false)?;
        self.ready(scene)?;
        // A layer that still holds this very draw is left as it is (ADR-0194): the
        // same vertices over the same images, none of them changed since.
        if self.holds(scene, target) {
            #[cfg(test)]
            {
                self.kept_layers += 1;
            }
            return Ok(());
        }
        self.load_ready(scene)?;
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
        // Over a transparent background, a layer whose content is known need only be
        // made transparent where that is, not cleared whole (#1449).
        let prior = (scene.background.a == 0.0)
            .then(|| target.content())
            .flatten();
        self.record_composite(&mut encoder, (view, target.texture()), scene, prior, true);
        self.record_release(&mut encoder);
        self.gpu.submit_work(encoder.finish());
        target.mark_changed();
        self.note_drawn(scene, target);
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

    /// Draws `scene`'s view depth into `target`, a [`Self::depth_layer`] of the canvas size,
    /// replacing what it held (ADR-0143 decision 4): red holds `1 / w`, which is `f / D`
    /// for the nearest thing at `D` along its camera's viewing axis, and every channel is
    /// 0 where nothing is (infinitely far). Each card seen through a camera writes where
    /// its alpha, with its opacity and its sharp edge, is at least one half; the larger
    /// value wins, so draw order doesn't matter. A [`Placement::Layer`] (a model's
    /// layer) that carries the model's depth ([`crate::Item::depth`]) writes that depth where
    /// the model covers, as a scene's pass tests against it, so the model isn't drawn
    /// again; one without writes nothing here, and [`Self::draw_model_depth`] adds its
    /// model's depth.
    /// A scene with no camera has no depth, and leaves `target` all 0.
    ///
    /// It prepares the scene again (placement, vertices, mips) rather than reusing the
    /// colour pass's work; recording both in one encoder is left for when the engine
    /// draws it every frame.
    ///
    /// # Errors
    /// As for [`Self::render_into`].
    pub fn render_view_depth(
        &mut self,
        scene: &Scene<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        let drawn = self.draw_view_depth(scene, target);
        errors.close(drawn)
    }

    /// [`Self::render_view_depth`], its errors not yet collected.
    fn draw_view_depth(&mut self, scene: &Scene<'_>, target: &Image) -> Result<(), RenderError> {
        let view = self.scene_target(scene, target, true)?;
        self.load(scene)?;
        let lock = self.gpu.submit.clone();
        let submitting = lock.lock();
        self.stage_sources(scene)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-view-depth"),
            });
        self.record_mips(&mut encoder, scene);
        // A layer whose content is known need only be emptied where that is, as a
        // colour layer is (#1449).
        let prior = target.content();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-view-depth"),
                color_attachments: &[Some(color_attachment(
                    view,
                    match prior {
                        Some(_) => wgpu::LoadOp::Load,
                        None => wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    },
                ))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(region) = prior {
                let size = (scene.canvas.width(), scene.canvas.height());
                self.wipe.record(&mut pass, region, size, true);
            }
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            for draw in &self.draws {
                let item = &scene.items[draw.index];
                if item.camera.is_none() {
                    continue;
                }
                let bind_group = match (item.placement, item.depth) {
                    // A model's layer with its depth: the depth it was drawn with, over
                    // the layer's own triangles, as a scene's pass writes it.
                    // Only at half opacity or more, as a card and a model drawn here
                    // count (ADR-0143 decision 4): a model its group has faded further
                    // adds no depth.
                    (Placement::Layer(_), Some(depth))
                        if depth.owner().is(&self.owner) && item.opacity >= 0.5 =>
                    {
                        pass.set_pipeline(&self.depth_copier);
                        depth.bind_group()
                    }
                    (Placement::Layer(_), _) => continue,
                    _ => {
                        pass.set_pipeline(self.blends.depth());
                        match &draw.mips {
                            Some(mips) => &mips.bind_group,
                            None => item.image.bind_group(),
                        }
                    }
                };
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(
                    draw.first_vertex..draw.first_vertex + draw.vertex_count,
                    0..1,
                );
            }
        }
        self.record_release(&mut encoder);
        self.gpu.submit_work(encoder.finish());
        target.mark_changed();
        // Outside what the scene's items reach the layer holds nothing, as a colour
        // layer drawn over a transparent background does.
        target.set_content(Some(self.drawn.map_or(crate::wipe::EMPTY, |rect| Region {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        })));
        drop(submitting);
        self.staged.clear();
        Ok(())
    }

    /// Adds a model's own depth to a view depth that [`Self::render_view_depth`] drew
    /// into `target` (ADR-0143 decision 4): `1 / w` per fragment of the model where its
    /// material's alpha is at least one half, the larger value winning. Only a model seen
    /// through a camera has depth there; without one this draws nothing. It doesn't know
    /// its item's visibility or opacity: the caller skips a hidden item, and one whose
    /// opacity is under one half, as the cards' pass does. `target` is a
    /// [`Self::depth_layer`].
    ///
    /// # Errors
    /// As for [`Self::draw_model`].
    pub fn draw_model_depth(
        &mut self,
        draw: &ModelDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        errors.close(self.models.draw_depth(&self.gpu, &self.owner, draw, target))
    }

    /// Clears `targets`, a scene's surface buffer for its screen-space reflections
    /// (ADR-0170 decision 4), three layers of the canvas size, before
    /// [`Self::draw_model_surface`] draws its models into them. Cleared, they reflect
    /// nothing on screen anywhere.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when a target isn't a layer this compositor made, or the
    /// three aren't one size.
    pub fn clear_surface(&mut self, targets: [&Image; 3]) -> Result<(), RenderError> {
        let errors = self.errors();
        errors.close(self.models.clear_surface(&self.gpu, &self.owner, targets))
    }

    /// Draws what a screen-space reflection reads of `draw`'s model (ADR-0170 decision
    /// 4) into `targets` where it is nearer than what they hold since
    /// [`Self::clear_surface`], shaded as [`Self::draw_model`] shades it: its shading
    /// normal in its camera's viewing frame with `1 / w`, the split-sum BRDF its
    /// environment's reflection was weighed by with its roughness, and that reflection
    /// as it was added with 1 where a scene-lit, not blended surface may have it replaced.
    /// Its live texture and its materials' drives are read as the layer reads them.
    /// Where the material's alpha is under one half nothing is drawn, as for its view
    /// depth. Without a camera nothing is drawn.
    ///
    /// # Errors
    /// As for [`Self::draw_model`], and [`Self::clear_surface`]'s.
    pub fn draw_model_surface(
        &mut self,
        draw: &ModelDraw<'_>,
        targets: [&Image; 3],
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        let chain = draw
            .texture
            .and_then(|texture| texture.image)
            .filter(|image| image.owner().is(&self.owner))
            .map(|image| self.built_chain(image, "jstream-render-model-texture"));
        let live = chain.as_ref().map(|chain| (&chain.whole.0, &chain.whole.1));
        let lit = draw.lighting != jstream_media::model::Lighting::Unlit;
        let sky = draw
            .reflection
            .or(draw.environment.and_then(|environment| environment.image))
            .filter(|_| lit);
        let probe = draw
            .probe
            .map(|probe| probe.image)
            .filter(|_| draw.lighting == jstream_media::model::Lighting::Scene);
        let (sky, probe) = (self.panorama_chain(sky), self.panorama_chain(probe));
        let panoramas = Panoramas {
            sky: sky.as_ref().map(|(image, mips)| (*image, &**mips)),
            probe: probe.as_ref().map(|(image, mips)| (*image, &**mips)),
        };
        let drawn =
            self.models
                .draw_surface_of(&self.gpu, &self.owner, draw, (targets, (live, panoramas)));
        errors.close(drawn)
    }

    /// The view of `target` when it is a layer of this compositor, of `scene`'s canvas
    /// size, that `scene` doesn't place: a depth layer with `depth`, a colour layer
    /// without.
    fn scene_target<'t>(
        &self,
        scene: &Scene<'_>,
        target: &'t Image,
        depth: bool,
    ) -> Result<&'t wgpu::TextureView, RenderError> {
        let view = if depth {
            target.depth_target()
        } else {
            target.target()
        };
        let view = match view {
            Some(view) if target.owner().is(&self.owner) => view,
            _ => {
                return Err(RenderError::Invalid {
                    what: "layer",
                    detail: if depth {
                        "the target is not a depth layer this compositor made".into()
                    } else {
                        "the target is not a layer this compositor made".into()
                    },
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
        Ok(view)
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
        let errors = self.errors();
        errors.close(self.models.upload_kit(&self.gpu, &self.owner).upload(model))
    }

    /// This compositor's model uploads, for another thread to make (ADR-0177): a model
    /// it uploads is this compositor's to draw, as one from [`Self::upload_model`] is.
    /// It keeps the device alive, not the compositor.
    pub fn model_uploader(&self) -> ModelUploader {
        ModelUploader {
            kit: Arc::new(self.models.upload_kit(&self.gpu, &self.owner)),
            own_errors: self.own_errors,
        }
    }

    /// Poses a rigged model for its next draws (ADR-0151 decision 7): `palette` is its
    /// rig's palette now ([`jstream_media::model::rig::Rig::pose`]), one matrix a slot.
    /// A model without a rig, or a palette of another length, is left as it is. Every
    /// draw of the model that follows, in any layer, view depth or shadow map, takes
    /// this pose, until the next.
    pub fn pose_model(&self, model: &GpuModel, palette: &[jstream_media::model::rig::Mat4]) {
        crate::model::ModelRuntime::pose(&self.gpu, model, palette);
    }

    /// Draws a model placed by an item into `target`, a [`Self::layer`] of the canvas
    /// size, replacing what it held (ADR-0107 decision 6): transparent where the model
    /// isn't, the model with its own depth test where it is. A scene then places the
    /// layer with [`crate::Placement::Layer`], after any filters run over it.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when the model or the layer came from another compositor
    /// or the target is not a layer, or when the placement or camera can't be drawn.
    ///
    /// A live `texture` (ADR-0107 decision 5) is sampled through a mip chain of its
    /// whole picture, kept on its image and rebuilt here only when the image changed
    /// since, as a 3D item's is; a `reflection` is read as a scene's sky picture is.
    /// The sky picture and the probe's capture it reflects are read through mip chains
    /// of their own, made the same way, so a rough surface blurs them smoothly
    /// (ADR-0170 decision 3).
    pub fn draw_model(&mut self, draw: &ModelDraw<'_>, target: &Image) -> Result<(), RenderError> {
        self.draw_model_beside(draw, target, None)
    }

    /// Draws `draw` into `target` as [`Self::draw_model`] does, and what a screen-space
    /// reflection reads of it (ADR-0170 decision 4) into `surface`, three layers of
    /// `target`'s size, cleared first, from the same shading: its shading normal in its
    /// camera's viewing frame with `1 / w`, the split-sum BRDF its environment's
    /// reflection was weighed by with its roughness, and that reflection as it was added
    /// with 1 where a scene-lit, not blended surface may have it replaced; zero where its
    /// opaque surfaces aren't or their alpha is under one half.
    /// [`Self::merge_model_surface`] then puts it in its scene's surface buffer, so the
    /// model isn't drawn a second time for it.
    ///
    /// # Errors
    /// As for [`Self::draw_model`], and when a `surface` layer isn't a layer this
    /// compositor made of `target`'s size.
    pub fn draw_model_with_surface(
        &mut self,
        draw: &ModelDraw<'_>,
        target: &Image,
        surface: [&Image; 3],
    ) -> Result<(), RenderError> {
        self.draw_model_beside(draw, target, Some(surface))
    }

    /// Merges a model's surface that [`Self::draw_model_with_surface`] drew into
    /// `layers` into `targets`, its scene's surface buffer, where it is nearer than what
    /// they hold since [`Self::clear_surface`]: one full-screen pass, cut to the box the
    /// model drew in when it said which.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when a layer or target isn't a layer this compositor made,
    /// or the three of either aren't one size. The layers may be smaller than the
    /// targets, for a model drawn with fewer pixels than its canvas (ADR-0197): each of
    /// the buffer's texels takes the layers' texel it falls in.
    pub fn merge_model_surface(
        &mut self,
        layers: [&Image; 3],
        targets: [&Image; 3],
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        errors.close(
            self.models
                .merge_surface(&self.gpu, &self.owner, layers, targets),
        )
    }

    fn draw_model_beside(
        &mut self,
        draw: &ModelDraw<'_>,
        target: &Image,
        surface: Option<[&Image; 3]>,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        let chain = draw
            .texture
            .and_then(|texture| texture.image)
            .filter(|image| image.owner().is(&self.owner))
            .map(|image| self.built_chain(image, "jstream-render-model-texture"));
        let live = chain.as_ref().map(|chain| (&chain.whole.0, &chain.whole.1));
        // As the model reads them: nothing unlit, and a probe only lit by its scene.
        let lit = draw.lighting != jstream_media::model::Lighting::Unlit;
        let sky = draw
            .reflection
            .or(draw.environment.and_then(|environment| environment.image))
            .filter(|_| lit);
        let probe = draw
            .probe
            .map(|probe| probe.image)
            .filter(|_| draw.lighting == jstream_media::model::Lighting::Scene);
        let (sky, probe) = (self.panorama_chain(sky), self.panorama_chain(probe));
        let panoramas = Panoramas {
            sky: sky.as_ref().map(|(image, mips)| (*image, &**mips)),
            probe: probe.as_ref().map(|(image, mips)| (*image, &**mips)),
        };
        let drawn = self.draw_model_focused(draw, target, (live, panoramas), surface);
        errors.close(drawn)
    }

    /// `image`'s mip chain of its whole picture, rebuilt first when the image changed
    /// since its last build.
    fn built_chain(&self, image: &Image, label: &'static str) -> Arc<Mips> {
        let chain = self.chain_of(image, Region::whole(image.width(), image.height()), 0);
        if chain.claim(image) {
            let lock = self.gpu.submit.clone();
            let submitting = lock.lock();
            let mut encoder = self
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            chain.record(&mut encoder, &self.mip_pipelines, image);
            self.gpu.submit_work(encoder.finish());
            drop(submitting);
        }
        chain
    }

    /// The mip chain a blurred reflection reads of `image`, a sky picture or a probe's
    /// capture this compositor made, with the image.
    fn panorama_chain<'a>(&self, image: Option<&'a Image>) -> Option<(&'a Image, Arc<Mips>)> {
        let image = image.filter(|image| image.owner().is(&self.owner))?;
        Some((
            image,
            self.built_chain(image, "jstream-render-panorama-mips"),
        ))
    }

    /// [`Self::draw_model`], its errors not yet collected. When the model's camera has
    /// depth of field (ADR-0165), the model and its view depth are drawn into layers of
    /// their own first, and the model's focus passes blur them into `target`. `live` is
    /// the live texture's mip chain and the panoramas' chains, as [`Self::draw_model`]
    /// made them.
    fn draw_model_focused(
        &mut self,
        draw: &ModelDraw<'_>,
        target: &Image,
        live: (Option<(&wgpu::Texture, &wgpu::TextureView)>, Panoramas<'_>),
        surface: Option<[&Image; 3]>,
    ) -> Result<(), RenderError> {
        // The canvas the model is placed on, which the target may hold with fewer
        // pixels (ADR-0197): the layers the passes read are the target's size, and stand
        // for the same canvas.
        let canvas = crate::model::canvas_of(target);
        let view = draw
            .camera
            .and_then(|camera| camera.view(canvas).ok())
            .filter(|view| view.focus().is_some());
        let Some(view) = view else {
            return self
                .models
                .draw(&self.gpu, &self.owner, draw, target, live, surface);
        };
        let (width, height) = (target.width(), target.height());
        let (mut colour, mut depth) = self.model_focus_layers(width, height)?;
        if let Ok(extent) = jstream_media::FrameSize::new(canvas.width, canvas.height) {
            colour.set_extent(extent);
            depth.set_extent(extent);
        }
        self.models
            .draw(&self.gpu, &self.owner, draw, &colour, live, surface)?;
        let empty = Scene {
            canvas: jstream_media::FrameSize::new(width, height).map_err(|_| {
                RenderError::Invalid {
                    what: "layer",
                    detail: "the target has no pixels".into(),
                }
            })?,
            background: crate::scene::Color::rgb(0.0, 0.0, 0.0),
            items: &[],
        };
        self.draw_view_depth(&empty, &depth)?;
        self.models
            .draw_depth(&self.gpu, &self.owner, draw, &depth)?;
        let slot = self
            .model_focus
            .filter
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut focus = slot.take().unwrap_or_else(|| {
            filter::VideoFilter::of(
                crate::model::focus::KIND,
                Box::new(crate::model::focus::ModelFocus::default()),
            )
        });
        focus.set_depth(Some(filter::ViewDepth {
            view: Some(view),
            ..filter::ViewDepth::new(Arc::new(depth), view.focal_px())
        }));
        let drawn = self.filter_into(&mut focus, &colour, target, FilterClock::default());
        focus.set_depth(None);
        *self
            .model_focus
            .filter
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(focus);
        drawn
    }

    /// The colour and depth layers a model with depth of field is drawn into first, of
    /// `width` by `height`, kept while the size holds.
    fn model_focus_layers(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(Image, Image), RenderError> {
        if let Some((colour, depth)) = &self.model_focus.layers
            && (colour.width(), colour.height()) == (width, height)
        {
            return Ok((colour.clone(), depth.clone()));
        }
        let layers = (self.layer(width, height)?, self.depth_layer(width, height)?);
        self.model_focus.layers = Some(layers.clone());
        Ok(layers)
    }

    /// Draws a 3D scene's sky backdrop into `target`, a [`Self::layer`] of the canvas
    /// size, replacing every texel (ADR-0167): the sky of `sky.environment` along each
    /// pixel's ray from `sky.camera`, infinitely far away, with its stars, at
    /// `sky.opacity`. A scene then places the layer under its items.
    ///
    /// A layer that still holds the same sky from an earlier call (the same values
    /// through a camera looking the same way, and a picture that hasn't changed) is not
    /// drawn again, so a still camera pays for its sky once.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when `target` is not a layer this compositor made, or the
    /// camera can't be seen on it.
    pub fn draw_sky(
        &mut self,
        sky: &crate::SkyDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        let runtime = self
            .sky
            .get_or_insert_with(|| crate::sky::SkyRuntime::new(&self.gpu));
        errors.close(runtime.draw(&self.gpu, &self.owner, sky, target))
    }

    /// Resamples a reflection probe's six `faces` into `target`, the panorama a surface
    /// inside its box reads (ADR-0170 decision 3), replacing every texel: an
    /// equirectangular panorama in the scene's axes, its centre along -z and up at its
    /// top, premultiplied linear light with alpha where a face saw something. `target`
    /// is a [`Self::layer`] four faces wide and two high.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when a face or `target` is not a layer this compositor
    /// made, the faces aren't squares of one size, or `target` isn't four by two of them.
    pub fn resample_probe(
        &mut self,
        faces: &[crate::ProbeFace<'_>; 6],
        target: &Image,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        let runtime = self
            .probe
            .get_or_insert_with(|| crate::probe::ProbeRuntime::new(&self.gpu));
        errors.close(runtime.resample(&self.gpu, &self.owner, faces, target))
    }

    /// How many skies [`Self::draw_sky`] drew, not counting the calls a layer already
    /// held the sky of.
    #[cfg(test)]
    pub(crate) fn sky_draws(&self) -> u64 {
        self.sky.as_ref().map_or(0, |sky| sky.draws)
    }

    /// How many deep skies [`Self::draw_sky`] has baked (ADR-0174).
    #[cfg(test)]
    pub(crate) fn sky_bakes(&self) -> u64 {
        self.sky.as_ref().map_or(0, |sky| sky.baked)
    }

    /// How many seeds' deep skies are kept now (ADR-0174 decision 3).
    #[cfg(test)]
    pub(crate) fn sky_bakes_kept(&self) -> usize {
        self.sky.as_ref().map_or(0, |sky| sky.kept_bakes())
    }

    /// Lights a card's picture by its 3D scene (ADR-0151 decision 10): `card.image`, the
    /// picture flat in its box layer, shaded texel by texel where that layer sits in the
    /// scene (its lights, their shadows and its environment, seen from its camera's side)
    /// into `target`, a layer of the same size, replacing what it held.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when the picture or `target` is not a layer this
    /// compositor made, when the two differ in size, or when the geometry or camera
    /// can't be placed.
    pub fn light_card(&mut self, card: &CardDraw<'_>, target: &Image) -> Result<(), RenderError> {
        let errors = self.errors();
        if (card.image.width(), card.image.height()) != (target.width(), target.height()) {
            return errors.close(Err(RenderError::Invalid {
                what: "layer",
                detail: "a lit card's picture and its target differ in size".into(),
            }));
        }
        let sky = card.environment.and_then(|environment| environment.image);
        let probe = card.probe.map(|probe| probe.image);
        let (sky, probe) = (self.panorama_chain(sky), self.panorama_chain(probe));
        let panoramas = Panoramas {
            sky: sky.as_ref().map(|(image, mips)| (*image, &**mips)),
            probe: probe.as_ref().map(|(image, mips)| (*image, &**mips)),
        };
        errors.close(
            self.models
                .light_card(&self.gpu, &self.owner, card, target, panoramas),
        )
    }

    /// Refills `image`, an upload this compositor made, with new tightly packed
    /// straight-alpha sRGB RGBA8 of the same size, without allocating a texture. Frames
    /// already submitted keep sampling the old contents.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] when `image` is not an upload this compositor made, and
    /// [`RenderError::ImageBytes`] when `rgba` is not its `width * height * 4` bytes.
    pub fn rewrite_rgba8(&self, image: &Image, rgba: &[u8]) -> Result<(), RenderError> {
        let errors = self.errors();
        errors.close(image::rewrite(&self.maker(), image, rgba))
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
        let errors = self.errors();
        errors.close(image::upload_frame(&self.maker(), frame))
    }

    /// [`Self::upload_frame`] into a texture of `to` texels, smaller than the frame each
    /// way or its size, placed at the frame's size: each texel is the mean of the
    /// frame's texels under it, in premultiplied linear light (#1139).
    pub(crate) fn upload_frame_into(
        &self,
        frame: &jstream_media::VideoFrame,
        to: (u32, u32),
    ) -> Result<Image, RenderError> {
        let errors = self.errors();
        errors.close(image::upload_frame_into(&self.maker(), frame, to))
    }

    /// Refills `image`, an upload this compositor made of a frame of `frame`'s size
    /// (whole, or held smaller), with `frame` as [`Self::upload_frame`] reads it, without
    /// allocating a texture.
    ///
    /// # Errors
    /// As for [`Self::upload_frame`], and [`RenderError::Invalid`] when `image` is not
    /// an upload this compositor made of a frame of that size.
    pub fn rewrite_frame(
        &self,
        image: &Image,
        frame: &jstream_media::VideoFrame,
    ) -> Result<(), RenderError> {
        let errors = self.errors();
        errors.close(image::rewrite_frame(&self.maker(), image, frame))
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
        let errors = self.errors();
        let drawn = self.filter_into(filter, input, output, clock);
        errors.close(drawn)
    }

    /// [`Self::apply_filter_into`], its errors not yet collected.
    fn filter_into(
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
        let padding = filter.pad();
        let (pass, scratch) = filter.parts();
        let timing = match (&mut self.timer, pass.timed()) {
            (Some(timer), Some(kind)) => timer.begin(&mut encoder, kind),
            _ => None,
        };
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
            wipe: &self.wipe,
            written: Vec::new(),
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
        let written = std::mem::take(&mut ctx.written);
        if let Err(err) = encoded {
            self.staged.clear();
            if let (Some(timer), Some(timing)) = (&mut self.timer, timing) {
                timer.abandon(timing);
            }
            return Err(err);
        }
        if let (Some(timer), Some(timing)) = (&self.timer, &timing) {
            timer.end(&mut encoder, timing);
        }
        // Staged only once recording succeeded, right before the submit it arms, and
        // handed back to the platform at the end of that submit, as `render` does: the
        // input's hold and those of the images the filter reads besides it, each once.
        // The submit lock is held from staging through the submit, as in `render`.
        let submitting = self.gpu.submit.lock();
        for (sync, _) in &self.staged {
            if let Err(err) = sync.stage(&self.gpu.queue) {
                self.staged.clear();
                if let (Some(timer), Some(timing)) = (&mut self.timer, timing) {
                    timer.abandon(timing);
                }
                return Err(RenderError::Sync(err));
            }
        }
        self.record_release(&mut encoder);
        self.gpu.submit_work(encoder.finish());
        // What each image drawn into can hold now (#1070, #1449): a filter can draw
        // anywhere in its output, unless what it reads is bounded and its kind says how
        // far past that it draws, and every draw is then cut to that.
        // Each is marked changed first, as every write of an image's texels is: what
        // remembers an image by its change count (a mip chain, [`Image::seen`]) must
        // see a filter's output move on.
        let marked = written
            .iter()
            .any(|(image, _)| image.texture() == output.texture());
        for (image, content) in written {
            image.mark_changed();
            image.set_content(content);
        }
        if !marked {
            output.mark_changed();
        }
        drop(submitting);
        if let (Some(timer), Some(timing)) = (&self.timer, timing) {
            timer.submitted(timing);
        }
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

    /// Starts timing each effect-pack filter or shader source this compositor applies
    /// on the GPU (#349), for [`Self::gpu_times`] to collect, and says whether it does:
    /// `false` on a device without timestamp queries. A compositor times nothing until
    /// asked, so one whose times nobody collects (a preview's) spends nothing on them.
    pub fn time_passes(&mut self) -> bool {
        if self.timer.is_none() {
            self.timer = crate::timer::PassTimer::new(&self.gpu.device, &self.gpu.queue);
        }
        self.timer.is_some()
    }

    /// Hands `measured` the GPU time of each effect-pack filter or shader source applied
    /// since the last call whose time has come back, with its kind ID (#349). A time
    /// comes back a frame or two after its draw, read without waiting, and allocates
    /// nothing. Nothing before [`Self::time_passes`], or on a device without timestamp
    /// queries.
    pub fn gpu_times(&mut self, measured: impl FnMut(&str, std::time::Duration)) {
        if let Some(timer) = &mut self.timer {
            timer.collect(&self.gpu.device, measured);
        }
    }

    /// Opens a frame whose units' GPU time is measured, tagged `tag` for
    /// [`Self::gpu_units`] (ADR-0103, `spans.rs`): whether it is timed. It isn't on a
    /// device without `TIMESTAMP_QUERY`, or while every frame set is still on its way.
    pub fn begin_gpu_frame(&mut self, tag: u64) -> bool {
        self.gpu.spans.begin(tag)
    }

    /// Names the unit the compositor's work belongs to from now on, while a frame is
    /// timed; `None` times what follows as no unit's.
    pub fn gpu_unit(&mut self, unit: Option<u32>) {
        self.gpu.spans.unit(unit);
    }

    /// Closes the frame [`Self::begin_gpu_frame`] opened, sending its timestamps to be
    /// read without waiting.
    pub fn end_gpu_frame(&mut self) {
        let _submitting = self.gpu.submit.lock();
        self.gpu.spans.end(&self.gpu.device, &self.gpu.queue);
    }

    /// Hands each unit's GPU time in every timed frame read since the last call to
    /// `measured`, with the frame's tag: `(tag, unit, time)`. Polls the device without
    /// waiting, and allocates nothing.
    pub fn gpu_units(&mut self, measured: impl FnMut(u64, u32, std::time::Duration)) {
        self.gpu.spans.collect(&self.gpu.device, measured);
    }

    /// Whether the device can measure GPU time ([`Self::begin_gpu_frame`]).
    pub fn times_gpu(&self) -> bool {
        self.gpu.spans.supported()
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

    /// Submits `work` on [`Self::queue`], timed with the unit the drawer is timing, if
    /// any (`Gpu::submit_work`). The caller holds [`Self::submit_lock`].
    pub(crate) fn submit_work(&self, work: wgpu::CommandBuffer) -> wgpu::SubmissionIndex {
        self.gpu.submit_work(work)
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

    /// Why the device was lost, when wgpu has already said so ([`Self::lost`]), without
    /// polling: cheap enough to ask before every frame. The callback runs on a poll, and
    /// the compositor polls its device as it reads frames back.
    pub fn lost_already(&self) -> Option<&str> {
        self.gpu.faults.lost.get().map(String::as_str)
    }

    /// Destroys the device, as a driver reset would lose it: tests that stand in for a
    /// loss, and a host that gives a device up on purpose, call this and never
    /// `wgpu::Device::destroy` itself. The loss is recorded first ([`Self::lost`] says
    /// `destroyed`), once no thread is reading one of the device's mapped buffers, such
    /// as a readback's copier: the next poll of a destroyed device frees its buffers, so
    /// a read still under way would read freed memory.
    pub fn destroy_device(&self) {
        self.gpu.faults.mark_lost("destroyed".to_owned());
        self.gpu.device.destroy();
    }

    /// How this compositor's device has failed, shared with every compositor on it.
    pub(crate) fn faults(&self) -> &Arc<Faults> {
        &self.gpu.faults
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

    /// A one-line description of the adapter, for logs and test output.
    pub fn adapter_description(&self) -> String {
        self.gpu.description()
    }

    /// How many render pipelines were made on this compositor's device, by any
    /// compositor on it, not counting the ones a compositor found made already
    /// (`pipelines.rs`). Each one made compiled its shaders; a draw that leaves the
    /// count as it was waited for none.
    pub fn pipelines_made(&self) -> u64 {
        self.gpu
            .built
            .makes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A second compositor on this one's device and queue, as two `on_context` calls on
    /// one `GpuContext` would make. Tests use it to check that images do not cross over.
    #[cfg(test)]
    pub(crate) fn sibling(&self) -> Self {
        let mut sibling = Self::new(Gpu {
            device: self.gpu.device.clone(),
            queue: self.gpu.queue.clone(),
            adapter: self.gpu.adapter.clone(),
            max_texture_dimension_2d: self.gpu.max_texture_dimension_2d,
            max_buffer_size: self.gpu.max_buffer_size,
            max_storage_binding: self.gpu.max_storage_binding,
            software: self.gpu.software,
            context: self.gpu.context.clone(),
            faults: Arc::clone(&self.gpu.faults),
            submit: self.gpu.submit.clone(),
            spans: Arc::clone(&self.gpu.spans),
            built: Arc::clone(&self.gpu.built),
        });
        sibling.blending = self.blending;
        sibling
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
        let composite_shader = shader(&gpu.device, "jstream-render-composite", COMPOSITE_SOURCE);
        let resolve_layout = resolve_layout(&gpu.device);
        // The backdrop is bound as the resolve's input is: one unfiltered FP16 texture.
        let blends_for = |encoded| {
            BlendPipelines::new(
                &gpu.device,
                &Layouts {
                    image: &image_layout,
                    backdrop: &resolve_layout,
                    shader: &composite_shader,
                    encoded,
                    built: &gpu.built,
                },
                None,
            )
        };
        let blends = blends_for(false);
        let encoded_blends = blends_for(true);
        let depth_copier = blend::depth_copier(
            &gpu.device,
            &Layouts {
                image: &image_layout,
                backdrop: &resolve_layout,
                shader: &composite_shader,
                encoded: false,
                built: &gpu.built,
            },
        );
        let composite_shader_kept = composite_shader.clone();
        let resolve_shader = shader(
            &gpu.device,
            "jstream-render-resolve",
            include_str!("resolve.wgsl"),
        );
        let resolve_input = resolve_input_layout(&gpu.device);
        let resolve = resolve_pipeline(
            &gpu.device,
            &resolve_input,
            &resolve_shader,
            ("fs", wgpu::TextureFormat::Rgba8UnormSrgb),
        );
        let resolve_encoded = resolve_pipeline(
            &gpu.device,
            &resolve_input,
            &resolve_shader,
            ("fs_encoded", wgpu::TextureFormat::Rgba8UnormSrgb),
        );
        // The decode reads the backdrop with a load, so it binds the texture alone.
        let decode = resolve_pipeline(
            &gpu.device,
            &resolve_layout,
            &resolve_shader,
            ("fs_decode", wgpu::TextureFormat::Rgba16Float),
        );
        let over_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("jstream-render-resolve-over"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(OVER_BYTES),
                    },
                    count: None,
                }],
            });
        let resolve_layer = resolve_pipeline_of(
            &gpu.device,
            &[&image_layout, &over_layout],
            &resolve_shader,
            ("fs_layer", wgpu::TextureFormat::Rgba8UnormSrgb),
        );
        let over = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-resolve-over"),
            size: OVER_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let over_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-resolve-over"),
            layout: &over_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: over.as_entire_binding(),
            }],
        });
        // Nearest and clamped: the resolve samples each texel's centre.
        let resolve_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-resolve"),
            ..Default::default()
        });
        let filters = filter::Runtime::new(&gpu.device);
        let wipe = Wipe::new(&gpu.device);
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
            depth_copier,
            encoded_blends,
            composite_shader: composite_shader_kept,
            depth_pipelines: None,
            scene_depth: None,
            depth_on: false,
            blending: Blending::Linear,
            backdrop: None,
            resolve_layout,
            resolve_input,
            resolve_sampler,
            resolve,
            resolve_encoded,
            decode,
            resolve_layer,
            over,
            over_bind,
            #[cfg(test)]
            resolved_layer: false,
            targets: None,
            spare: Vec::new(),
            renders: 0,
            #[cfg(test)]
            targets_made: 0,
            #[cfg(test)]
            kept_layers: 0,
            vertices: Vec::new(),
            vertex_buffer,
            draws: Vec::new(),
            drawn: None,
            order: Vec::new(),
            staged: Vec::new(),
            filters,
            wipe,
            mip_pipelines,
            models,
            model_focus: ModelFocusRun::default(),
            sky: None,
            probe: None,
            readback_wait: std::time::Duration::ZERO,
            timer: None,
            ingest: Mutex::new(None),
            own_errors: false,
        }
    }

    /// The scopes for one call, when this compositor keeps its own errors.
    fn errors(&self) -> Errors {
        Errors::open(&self.gpu.device, self.own_errors)
    }

    /// Runs `call` inside [`Self::errors`]'s scopes, for a call made outside this file
    /// that creates resources on the device, such as `lut_image`'s texture.
    pub(crate) fn scoped<T>(
        &self,
        call: impl FnOnce() -> Result<T, RenderError>,
    ) -> Result<T, RenderError> {
        let errors = self.errors();
        errors.close(call())
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
            // Asked before the trim, which only leaves out texels that are transparent
            // in the image.
            let covers = covers_whole(shape, canvas_w, canvas_h);
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
            let whole = mips.is_none()
                && alpha == Alpha::Premultiplied
                && item.opacity == 1.0
                && item.blend == BlendMode::Normal
                && item.depth.is_none()
                && item.image.sync().is_none()
                && texels == (scene.canvas.width(), scene.canvas.height())
                && covers;
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
                whole,
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

    /// Readies the targets of `width` by `height`: the ones last rendered into, ones
    /// kept aside from an earlier render at that size, or new ones. The ones another
    /// size was rendered into are kept aside for it, a few and not for long, so
    /// renders that alternate between sizes reuse theirs.
    fn ensure_targets(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.renders += 1;
        let renders = self.renders;
        self.spare.retain(|set| renders - set.used <= SPARE_RENDERS);
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return Ok(());
        }
        // Taken out before the last set is set aside, so that never pushes it out.
        let kept = self
            .spare
            .iter()
            .position(|set| set.width == width && set.height == height)
            .map(|kept| self.spare.remove(kept));
        if let Some(mut last) = self.targets.take() {
            // Rendered into by the render before this one.
            last.used = renders - 1;
            if self.spare.len() == SPARE_TARGETS {
                // Oldest first: sets are pushed as they are set aside.
                self.spare.remove(0);
            }
            self.spare.push(last);
        }
        if kept.is_some() {
            self.targets = kept;
            return Ok(());
        }
        #[cfg(test)]
        {
            self.targets_made += 1;
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
                layout: &self.resolve_input,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&working_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.resolve_sampler),
                    },
                ],
            });
        self.targets = Some(Targets {
            width,
            height,
            used: renders,
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
        self.ready(scene)?;
        self.load_ready(scene)
    }

    /// The first half of [`Self::load`]: validates `scene` and builds its vertices and
    /// draws, writing nothing to the GPU.
    fn ready(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        color::validate(scene.background)?;
        self.prepare(scene)
    }

    /// Whether `target` still holds exactly what drawing `scene`, readied
    /// ([`Self::ready`]), would leave in it (ADR-0194): its last
    /// [`Self::render_into`] drew the same vertices, of the same images in the same
    /// modes over the same background in the same space, nothing has written to it
    /// since, and none of those images has changed. A scene with a model's depth is
    /// always drawn: its pass also writes the depth target.
    fn holds(&self, scene: &Scene<'_>, target: &Image) -> bool {
        let drawn = target.drawn_as();
        let Some(drawn) = drawn.as_ref() else {
            return false;
        };
        drawn.at == target.generation()
            && drawn.blending == self.blending
            && drawn.background == background_bits(scene.background)
            && drawn.vertices == self.vertices
            && drawn.draws.len() == self.draws.len()
            && drawn.draws.iter().zip(&self.draws).all(|(was, draw)| {
                let item = &scene.items[draw.index];
                item.depth.is_none() && *was == DrawnDraw::of(draw, item)
            })
    }

    /// Records on `target` what was just drawn into it and marked, for [`Self::holds`].
    /// The lists it kept before are refilled, so a steady frame allocates nothing.
    fn note_drawn(&self, scene: &Scene<'_>, target: &Image) {
        let mut kept = target.drawn_as();
        let mut drawn = kept.take().unwrap_or_default();
        drawn.at = target.generation();
        drawn.blending = self.blending;
        drawn.background = background_bits(scene.background);
        drawn.vertices.clear();
        drawn.vertices.extend_from_slice(&self.vertices);
        drawn.draws.clear();
        drawn.draws.extend(
            self.draws
                .iter()
                .map(|draw| DrawnDraw::of(draw, &scene.items[draw.index])),
        );
        *kept = Some(drawn);
    }

    /// The second half of [`Self::load`]: what the readied scene needs on the GPU.
    fn load_ready(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        self.ensure_vertex_buffer()?;
        if self.blending == Blending::Srgb || self.draws.iter().any(|draw| draw.rect.is_some()) {
            self.ensure_backdrop(scene.canvas.width(), scene.canvas.height());
        }
        // A model's depth gives the pass a depth target (ADR-0107 decision 7).
        self.depth_on = self
            .draws
            .iter()
            .any(|draw| scene.items[draw.index].depth.is_some());
        if self.depth_on {
            self.ensure_scene_depth(scene.canvas.width(), scene.canvas.height());
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

    /// Makes what a 3D scene draws with and a 2D one never needs, which is otherwise made
    /// at the first draw that needs it: the sky's pipeline, its bake's, and the scene
    /// passes' depth pipelines. The models' are made with the compositor. Each is found if a
    /// compositor on the device made it already (`pipelines.rs`), so a caller on a
    /// thread of its own makes them for every compositor on its device ahead of the
    /// first 3D frame, which would otherwise wait for their shaders to compile.
    pub fn warm_3d(&mut self) {
        self.sky
            .get_or_insert_with(|| crate::sky::SkyRuntime::new(&self.gpu))
            .warm(&self.gpu);
        self.ensure_depth_pipelines();
    }

    /// Makes the depth pipelines once, and a depth target of `width` by `height`.
    fn ensure_scene_depth(&mut self, width: u32, height: u32) {
        self.ensure_depth_pipelines();
        if self
            .scene_depth
            .as_ref()
            .is_some_and(|depth| depth.size == (width, height))
        {
            return;
        }
        self.make_scene_depth(width, height);
    }

    fn ensure_depth_pipelines(&mut self) {
        if self.depth_pipelines.is_none() {
            let device = &self.gpu.device;
            let layouts = |encoded| Layouts {
                image: &self.image_layout,
                backdrop: &self.resolve_layout,
                shader: &self.composite_shader,
                encoded,
                built: &self.gpu.built,
            };
            let set = |compare| {
                [false, true]
                    .map(|encoded| BlendPipelines::new(device, &layouts(encoded), Some(compare)))
            };
            self.depth_pipelines = Some(DepthPipelines {
                tested: set(wgpu::CompareFunction::GreaterEqual),
                untested: set(wgpu::CompareFunction::Always),
                writer: blend::depth_writer(device, &layouts(false)),
            });
        }
    }

    fn make_scene_depth(&mut self, width: u32, height: u32) {
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-scene-depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: blend::SCENE_DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.scene_depth = Some(SceneDepth {
            size: (width, height),
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        });
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
    /// the items, premultiplied linear. In the encoded space the target is decoded
    /// back to linear light after the last item when `decode` says so; without, it is
    /// left holding premultiplied sRGB-encoded colour for a reader that expects that
    /// ([`Self::record_resolve`]). An item whose mode reads the backdrop ends the
    /// pass: the texels it covers are copied into the backdrop, and a new pass draws it
    /// and the items after it (ADR-0082).
    fn record_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        (view, texture): (&wgpu::TextureView, &wgpu::Texture),
        scene: &Scene<'_>,
        prior: Option<Region>,
        decode: bool,
    ) {
        let encoded = self.blending == Blending::Srgb;
        let clear = if encoded {
            premultiplied_srgb(scene.background)
        } else {
            premultiplied_linear(scene.background)
        };
        let clear = wgpu::Color {
            r: f64::from(clear[0]),
            g: f64::from(clear[1]),
            b: f64::from(clear[2]),
            a: f64::from(clear[3]),
        };
        // A target known transparent outside `prior` is loaded and made transparent over
        // that alone; any other is cleared whole.
        // A pass with a depth target clears it, and its colour too: the wipe's pipeline
        // has no depth state.
        let start = match prior {
            Some(region) if !self.depth_on => Start::Wipe(region),
            _ => Start::Clear(clear),
        };
        let reads = |draw: &Draw| draw.rect.is_some();
        let count = self.draws.len();
        let mut cleared = false;
        let mut first = 0;
        loop {
            if let Some(rect) = self.draws.get(first).and_then(|draw| draw.rect) {
                if !cleared {
                    self.record_draws(encoder, view, Some(start), &[], scene);
                    cleared = true;
                }
                self.record_backdrop_copy(encoder, texture, rect);
            }
            let end = (first + 1..count)
                .find(|&index| reads(&self.draws[index]))
                .unwrap_or(count.max(first));
            let load = (!cleared).then_some(start);
            self.record_draws(encoder, view, load, &self.draws[first..end], scene);
            cleared = true;
            if end >= count {
                break;
            }
            first = end;
        }
        if encoded && decode {
            // Over a transparent background only what the items reached holds encoded
            // colour: every other texel is zero, which decodes to itself. So the decode
            // covers that box alone, and a pass that drew nothing has none.
            let transparent = clear.r == 0.0 && clear.g == 0.0 && clear.b == 0.0 && clear.a == 0.0;
            let reached = self.decoded(transparent, (texture.width(), texture.height()));
            if let Some(rect) = reached {
                self.record_decode(encoder, (view, texture), rect);
            }
        }
    }

    /// The texels of a `size` target the decode pass covers after the prepared draws:
    /// all of it over a background, and over a `transparent` one only what the draws
    /// could reach, `None` when that is nothing.
    fn decoded(&self, transparent: bool, (width, height): (u32, u32)) -> Option<Rect> {
        let reached = if transparent {
            self.drawn
        } else {
            Some(Rect {
                x: 0,
                y: 0,
                width,
                height,
            })
        };
        reached.filter(|rect| rect.width > 0 && rect.height > 0)
    }

    /// The decode pass of browser-style blending (ADR-0126 §3): `rect` of the target is
    /// copied into the backdrop, and drawn back as premultiplied linear light.
    fn record_decode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        (view, texture): (&wgpu::TextureView, &wgpu::Texture),
        rect: Rect,
    ) {
        let Some(backdrop) = &self.backdrop else {
            return;
        };
        self.record_backdrop_copy(encoder, texture, rect);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-decode"),
            color_attachments: &[Some(color_attachment(view, wgpu::LoadOp::Load))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
        pass.set_pipeline(&self.decode);
        pass.set_bind_group(0, &backdrop.bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// One render pass over `target` drawing `draws`, after making it ready as `start`
    /// says when that is set.
    fn record_draws(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        start: Option<Start>,
        draws: &[Draw],
        scene: &Scene<'_>,
    ) {
        let load = match start {
            Some(Start::Clear(clear)) => wgpu::LoadOp::Clear(clear),
            Some(Start::Wipe(_)) | None => wgpu::LoadOp::Load,
        };
        // The scene's depth target: cleared to far by the pass that starts the target.
        let depth = self
            .scene_depth
            .as_ref()
            .filter(|_| self.depth_on)
            .map(|depth| wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: if start.is_some() {
                        wgpu::LoadOp::Clear(0.0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-composite"),
            color_attachments: &[Some(color_attachment(target, load))],
            depth_stencil_attachment: depth,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(Start::Wipe(region)) = start {
            let size = (scene.canvas.width(), scene.canvas.height());
            self.wipe.record(&mut pass, region, size, false);
        }
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
            let encoded = self.blending == Blending::Srgb;
            let model_depth = scene.items[draw.index].depth;
            // With a depth target, a model's own layer draws untested and every other
            // item tests against the models drawn before it (ADR-0107 decision 7).
            let depths = self.depth_pipelines.as_ref().filter(|_| self.depth_on);
            let blends = match depths {
                Some(depths) if model_depth.is_some() => &depths.untested[usize::from(encoded)],
                Some(depths) => &depths.tested[usize::from(encoded)],
                None if encoded => &self.encoded_blends,
                None => &self.blends,
            };
            let Ok(steps) = blends.steps(draw.blend) else {
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
                    pass.draw(vertices.clone(), 0..1);
                }
            }
            // Then the model's depth, over the same triangles, for what draws after it.
            if let (Some(depths), Some(model_depth)) = (depths, model_depth) {
                pass.set_pipeline(&depths.writer);
                pass.set_bind_group(0, model_depth.bind_group(), &[]);
                pass.draw(vertices, 0..1);
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

    /// The resolve pass: the FP16 working target to the straight sRGB output. In the
    /// encoded space the target holds premultiplied sRGB-encoded colour, left so by
    /// the composite, and the pass decodes it as it resolves (`fs_encoded`): no
    /// backdrop copy and no decode pass of its own.
    ///
    /// Its triangle writes every texel, so what the output held before is never seen. A
    /// GPU starts it cleared, which costs it little and spares a tiled GPU reading the
    /// old texels in. A software rasterizer would write every texel twice for that
    /// clear, so it starts loaded instead (#1449).
    fn record_resolve(&self, encoder: &mut wgpu::CommandEncoder, targets: &Targets) {
        let load = if self.gpu.software {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-resolve"),
            color_attachments: &[Some(color_attachment(&targets.output_view, load))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(if self.blending == Blending::Srgb {
            &self.resolve_encoded
        } else {
            &self.resolve
        });
        pass.set_bind_group(0, &targets.resolve_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The composite of `scene`, which is `layer` alone over the whole canvas
    /// ([`Draw::whole`]), and its resolve in one pass straight from `layer` into the
    /// sRGB output: `fs_layer` lays it over the background in the scene's blending
    /// space as the composite pass, its decode and the resolve would, without the
    /// working target, the backdrop copy or their passes.
    fn record_layer_resolve(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
        scene: &Scene<'_>,
        layer: &Image,
    ) {
        let encoded = self.blending == Blending::Srgb;
        let background = if encoded {
            premultiplied_srgb(scene.background)
        } else {
            premultiplied_linear(scene.background)
        };
        let flag = if encoded { 1.0f32 } else { 0.0 };
        let mut over = [0u8; OVER_BYTES as usize];
        for (bytes, value) in over
            .chunks_exact_mut(4)
            .zip(background.into_iter().chain([flag, 0.0, 0.0, 0.0]))
        {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        // Ordered before the submit that reads it, as the vertex buffer's write is.
        self.gpu.queue.write_buffer(&self.over, 0, &over);
        let load = if self.gpu.software {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-resolve-layer"),
            color_attachments: &[Some(color_attachment(&targets.output_view, load))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.resolve_layer);
        pass.set_bind_group(0, layer.bind_group(), &[]);
        pass.set_bind_group(1, &self.over_bind, &[]);
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
        self.map_read_buffer(stride.checked_mul(u64::from(height)), width, height)
    }

    /// A map-read buffer of `size` bytes for a `width` x `height` readback, `None`
    /// being a size past `u64`.
    pub(crate) fn map_read_buffer(
        &self,
        size: Option<u64>,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Buffer, RenderError> {
        let size = size
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

/// Whether `shape` is a flat quad that lays its image over the whole canvas texel for
/// texel: the four corners of the canvas, each with its own corner of the image, with
/// no perspective, edge or blur.
fn covers_whole(shape: &Shape, width: f32, height: f32) -> bool {
    let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let vertices = shape.vertices();
    !shape.is_3d()
        && shape.dof == [0.0; 3]
        && vertices.len() == corners.len()
        && vertices.iter().zip(corners).all(|(vertex, [u, v])| {
            vertex.w == 1.0 && vertex.uv == [u, v] && vertex.canvas == [u * width, v * height]
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
    gpu: &Gpu,
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
    gpu.device
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
    let snapshot = gpu
        .faults
        .read_mapped(&buffer, |view| {
            Snapshot::from_padded(width, height, view, stride)
        })
        .ok_or_else(|| RenderError::Readback {
            detail: "the readback buffer can't be read: the device was lost".into(),
        });
    buffer.unmap();
    snapshot
}

/// The composite shader's source, which the blend pipelines are made from.
pub(crate) const COMPOSITE_SOURCE: &str = include_str!("composite.wgsl");

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

/// [`resolve_layout`]'s texture and a non-filtering sampler for it.
fn resolve_input_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("jstream-render-resolve-input"),
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
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    })
}

/// The bytes of `resolve.wgsl`'s `Over`: two `vec4<f32>`.
const OVER_BYTES: u64 = 32;

/// A full-target pass over one unfiltered FP16 texture: the resolve (`fs`, into the
/// sRGB output) or the decode (`fs_decode`, back into an FP16 target).
fn resolve_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    entry: (&str, wgpu::TextureFormat),
) -> wgpu::RenderPipeline {
    resolve_pipeline_of(device, &[layout], shader, entry)
}

/// [`resolve_pipeline`] with each of `layouts` as a bind group, in order.
fn resolve_pipeline_of(
    device: &wgpu::Device,
    layouts: &[&wgpu::BindGroupLayout],
    shader: &wgpu::ShaderModule,
    (entry, format): (&str, wgpu::TextureFormat),
) -> wgpu::RenderPipeline {
    let layouts: Vec<_> = layouts.iter().copied().map(Some).collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("jstream-render-resolve"),
        bind_group_layouts: &layouts,
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
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
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

/// How a composite readies its target before the first draw.
#[derive(Clone, Copy)]
enum Start {
    /// Cleared whole to the background.
    Clear(wgpu::Color),
    /// Loaded, transparent outside the region, and made transparent over it.
    Wipe(Region),
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

/// The compositor's state for depth of field on models (ADR-0165).
#[derive(Default)]
struct ModelFocusRun {
    /// The passes, as a filter so they draw through the filter runtime. A filter is
    /// `Send` but not `Sync`, and the compositor must be both; only `&mut self` reaches
    /// it, through `Mutex::get_mut`, so it never locks.
    filter: std::sync::Mutex<Option<filter::VideoFilter>>,
    /// The model's colour and its view depth, drawn before the passes blur them.
    layers: Option<(Image, Image)>,
}
