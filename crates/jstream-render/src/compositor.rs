//! The two passes: composite in FP16 linear light, then resolve to straight sRGB.

use std::sync::Arc;
use std::time::Duration;

use jstream_platform::FrameSync;

use crate::color::{self, premultiplied_linear};
use crate::error::RenderError;
use crate::filter::{self, FilterClock, VideoFilter};
use crate::gpu::{Gpu, OpenOptions};
use crate::image::{self, Alpha, Image, Maker, Owner};
use crate::place::{self, Quad};
use crate::scene::{Output, Scene, Snapshot};

/// Bytes from one vertex to the next: six `f32`s (position, UV, opacity, alpha mode),
/// tightly packed. wgpu only needs the stride and attribute offsets to be multiples of 4.
const VERTEX_STRIDE: u64 = 24;
const VERTICES_PER_ITEM: u32 = 6;

struct Draw {
    index: usize,
    first_vertex: u32,
}

struct Targets {
    width: u32,
    height: u32,
    /// The view holds the FP16 texture alive; the handle itself is not read again.
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
    composite: wgpu::RenderPipeline,
    resolve_layout: wgpu::BindGroupLayout,
    resolve: wgpu::RenderPipeline,
    targets: Option<Targets>,
    vertices: Vec<u8>,
    vertex_buffer: wgpu::Buffer,
    draws: Vec<Draw>,
    /// The holds staged for the submit being built, and their textures, which the
    /// submit hands back to the platform (`PRESENT`) after its last use.
    staged: Vec<(Arc<dyn FrameSync>, wgpu::Texture)>,
    filters: filter::Runtime,
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
    /// The bytes are premultiplied in linear light on the CPU and stored as
    /// `Rgba16Float`, so the linear filter cannot darken transparent edges.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a zero dimension, [`RenderError::ImageBytes`] when
    /// `rgba` is not `width * height * 4` bytes, and [`RenderError::TooLarge`] when a
    /// side exceeds the adapter limit.
    pub fn upload_rgba8(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Image, RenderError> {
        image::upload(&self.maker(), width, height, rgba)
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
        self.load(scene)?;
        let width = scene.canvas.width();
        let height = scene.canvas.height();
        self.ensure_targets(width, height)?;

        let readback_buffer = if readback {
            Some(self.readback_buffer(width, height)?)
        } else {
            None
        };
        // Last, so no error between the staging and the submit it arms (#391).
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
        self.record_composite(&mut encoder, &targets.working_view, scene);
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
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        // wgpu validates the commands as they are recorded and submitted, so an error in
        // this frame has been raised by now; one from an earlier call fails this frame.
        if let Some(detail) = self.take_error() {
            return Err(RenderError::Uncaptured { detail });
        }

        let output = targets.output.clone();
        let pixels = match readback_buffer {
            Some(buffer) => Some(map_snapshot(&self.gpu.device, buffer, width, height)?),
            None => None,
        };
        Ok(Output::new(output, width, height, pixels))
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
        self.stage_sources(scene)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-layer"),
            });
        self.record_composite(&mut encoder, view, scene);
        self.record_release(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        // The platform reuses a frame's texture once nothing holds it.
        self.staged.clear();
        Ok(())
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
    /// reused, so a chain that runs every frame allocates no output texture (a
    /// multi-pass kind's scratch layers are still its own). Earlier submits that read
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
        let mut ctx = filter::Ctx {
            gpu: &self.gpu,
            maker: Maker {
                gpu: &self.gpu,
                layout: &self.image_layout,
                sampler: &self.sampler,
                owner: &self.owner,
            },
            runtime: &mut self.filters,
            encoder: &mut encoder,
            clock,
            holds: &mut self.staged,
        };
        ctx.holds.clear();
        if let Err(err) = filter.pass_mut().encode(&mut ctx, input, output) {
            self.staged.clear();
            return Err(err);
        }
        ctx.hold(input);
        // Staged only once recording succeeded, right before the submit it arms, and
        // handed back to the platform at the end of that submit, as `render` does: the
        // input's hold and those of the images the filter reads besides it, each once.
        for (sync, _) in &self.staged {
            if let Err(err) = sync.stage(&self.gpu.queue) {
                self.staged.clear();
                return Err(RenderError::Sync(err));
            }
        }
        self.record_release(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
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
            context: self.gpu.context.clone(),
            faults: Arc::clone(&self.gpu.faults),
        })
    }

    pub(crate) fn maker(&self) -> Maker<'_> {
        Maker {
            gpu: &self.gpu,
            layout: &self.image_layout,
            sampler: &self.sampler,
            owner: &self.owner,
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
        let composite = composite_pipeline(&gpu.device, &image_layout, &composite_shader);
        let resolve_layout = resolve_layout(&gpu.device);
        let resolve_shader = shader(
            &gpu.device,
            "jstream-render-resolve",
            include_str!("resolve.wgsl"),
        );
        let resolve = resolve_pipeline(&gpu.device, &resolve_layout, &resolve_shader);
        let filters = filter::Runtime::new(&gpu.device);
        let vertex_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-vertices"),
            size: VERTEX_STRIDE * u64::from(VERTICES_PER_ITEM),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            gpu,
            owner: Owner::default(),
            sampler,
            image_layout,
            composite,
            resolve_layout,
            resolve,
            targets: None,
            vertices: Vec::new(),
            vertex_buffer,
            draws: Vec::new(),
            staged: Vec::new(),
            filters,
        }
    }

    fn prepare(&mut self, scene: &Scene<'_>) -> Result<(), RenderError> {
        self.vertices.clear();
        self.draws.clear();
        let canvas_w = scene.canvas.width() as f32;
        let canvas_h = scene.canvas.height() as f32;
        for (index, item) in scene.items.iter().enumerate() {
            // Checked for hidden items too, so a scene is valid or not regardless of
            // visibility. Another compositor's bind group would reach a wgpu panic.
            if !item.image.owner().is(&self.owner) {
                return Err(RenderError::Invalid {
                    what: "image",
                    detail: format!("item {index} was made by another compositor"),
                });
            }
            if !item.opacity.is_finite() || !(0.0..=1.0).contains(&item.opacity) {
                return Err(RenderError::Invalid {
                    what: "opacity",
                    detail: format!("{} is not a finite value in 0..=1", item.opacity),
                });
            }
            let (width, height) = item.image.extent();
            let quad = place::quad(width, height, item.placement, item.crop)?;
            if !item.visible || item.opacity == 0.0 {
                continue;
            }
            let Some(quad) = quad else { continue };
            let first_vertex = u32::try_from(self.draws.len())
                .ok()
                .and_then(|n| n.checked_mul(VERTICES_PER_ITEM))
                .ok_or(RenderError::TooManyItems {
                    count: scene.items.len(),
                })?;
            push_quad(
                &mut self.vertices,
                &quad,
                canvas_w,
                canvas_h,
                item.opacity,
                item.image.alpha(),
            );
            self.draws.push(Draw {
                index,
                first_vertex,
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
            .max(VERTEX_STRIDE * u64::from(VERTICES_PER_ITEM));
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
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
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
        if !self.vertices.is_empty() {
            self.gpu
                .queue
                .write_buffer(&self.vertex_buffer, 0, &self.vertices);
        }
        Ok(())
    }

    /// The composite pass: clear `target` (`Rgba16Float`) to the background and draw
    /// the items, premultiplied linear.
    fn record_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        scene: &Scene<'_>,
    ) {
        let clear = premultiplied_linear(scene.background);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-composite"),
            color_attachments: &[Some(color_attachment(
                target,
                wgpu::LoadOp::Clear(wgpu::Color {
                    r: f64::from(clear[0]),
                    g: f64::from(clear[1]),
                    b: f64::from(clear[2]),
                    a: f64::from(clear[3]),
                }),
            ))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !self.draws.is_empty() {
            pass.set_pipeline(&self.composite);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            for draw in &self.draws {
                pass.set_bind_group(0, scene.items[draw.index].image.bind_group(), &[]);
                pass.draw(
                    draw.first_vertex..draw.first_vertex + VERTICES_PER_ITEM,
                    0..1,
                );
            }
        }
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

    fn readback_buffer(&self, width: u32, height: u32) -> Result<wgpu::Buffer, RenderError> {
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
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("adapter", &self.adapter_description())
            .finish_non_exhaustive()
    }
}

fn push_quad(
    buf: &mut Vec<u8>,
    quad: &Quad,
    canvas_w: f32,
    canvas_h: f32,
    opacity: f32,
    alpha: Alpha,
) {
    // The shader's `mode`: how it turns a filtered sample into premultiplied linear.
    let mode = alpha.shader_mode();
    let corner = |i: usize| {
        let [x, y] = quad.corners[i];
        let [u, v] = quad.uvs[i];
        (x / canvas_w * 2.0 - 1.0, 1.0 - y / canvas_h * 2.0, u, v)
    };
    // Two triangles: 0-1-2 and 0-2-3.
    for index in [0, 1, 2, 0, 2, 3] {
        let (x, y, u, v) = corner(index);
        push_f32(buf, x);
        push_f32(buf, y);
        push_f32(buf, u);
        push_f32(buf, v);
        push_f32(buf, opacity);
        push_f32(buf, mode);
    }
}

fn push_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn padded_row_bytes(width: u32) -> u32 {
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

fn composite_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32,
        3 => Float32,
    ];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: VERTEX_STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("jstream-render-composite"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("jstream-render-composite"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &buffers,
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
                format: wgpu::TextureFormat::Rgba16Float,
                blend: Some(wgpu::BlendState {
                    color: premultiplied_blend(),
                    alpha: premultiplied_blend(),
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
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

fn premultiplied_blend() -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    }
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
