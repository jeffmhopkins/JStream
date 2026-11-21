//! A 3D scene's sky backdrop (ADR-0167): its environment light's sky drawn behind the
//! scene, infinitely far away, through the scene's camera.
//!
//! Each canvas pixel shows the sky along its ray from the camera, so the backdrop turns
//! with where the camera looks and never moves with where it is. It draws into a layer
//! of the canvas size, which the scene then places under every item. The pipeline is
//! built the first time a backdrop draws, so a compositor that never draws one pays
//! nothing for it.

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::image::{Image, LAYER_FORMAT, Owner};
use crate::model::Environment;
use crate::scene::Color;
use jstream_protocol::{Canvas, SceneCamera};
use wgpu::util::DeviceExt;

/// The most a star layer's brightness setting scales its stars.
pub const MAX_STAR_BRIGHTNESS: f32 = 8.0;

/// A sky backdrop to draw (ADR-0167): a scene's environment light, the camera it is seen
/// through, and its stars.
#[derive(Clone, Copy, Debug)]
pub struct SkyDraw<'a> {
    /// The sky: its gradient or picture, its intensity and its turn.
    pub environment: Environment<'a>,
    /// The camera the scene is seen through. Only where it looks matters: the backdrop
    /// is infinitely far away.
    pub camera: SceneCamera,
    /// How many stars to scatter over the sky, 0 (none) to 1.
    pub stars: f32,
    /// How bright the stars are, 0 to [`MAX_STAR_BRIGHTNESS`], before the intensity.
    pub star_brightness: f32,
    /// How opaque the backdrop is, 0 to 1: its item's opacity.
    pub opacity: f32,
    /// The deep sky: nebulae, a galaxy band and the layout's seed.
    pub deep: DeepSky,
    /// How fast the sky streams past, 0 (still) to 1: the streaks' length.
    pub warp: f32,
    /// How far the stars have moved away from the direction of travel, in the log-tan
    /// units [`SkyDraw::travel_rate`] advances: the engine adds the rate times each
    /// frame's time, so a warp that ramps up and down moves them smoothly.
    pub travel: f64,
    /// Whether the warp is dropping toward a lower setting: as it comes out of warp the
    /// nebulae and band rush back out from straight ahead and settle, rather than fade.
    pub dropping: bool,
}

/// A backdrop's nebulae and galaxy band (ADR-0167), drawn over its sky, under its stars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeepSky {
    /// How much nebula, 0 (none) to 1.
    pub nebula: f32,
    /// The nebulae's two colours, straight sRGB. Alpha is ignored.
    pub nebula_colors: [Color; 2],
    /// How bright the galaxy band is, 0 (none) to 1.
    pub galaxy: f32,
    /// Which sky: each seed lays out the stars, nebulae and band's texture anew.
    pub seed: u32,
}

impl DeepSky {
    /// No nebula and no galaxy band.
    pub const NONE: Self = Self {
        nebula: 0.0,
        nebula_colors: [Color::BLACK, Color::BLACK],
        galaxy: 0.0,
        seed: 0,
    };
}

/// The streaks' length at warp 1, in log-tan units: about 70 degrees at right angles to
/// the travel.
const WARP_STREAK: f32 = 1.2;

/// The speed, as a fraction of light's, that warp 1 shifts the sky's light by: how far
/// stars crowd toward the direction of travel (aberration) and how much bluer and
/// brighter ahead, redder and dimmer behind (Doppler) they look.
const MAX_WARP_BETA: f32 = 0.9;

/// How fast the stars move at warp 1, in log-tan units a second.
const WARP_SPEED: f64 = 6.0;

/// Columns of faint and bright stars around the travel, as `sky.wgsl` has them.
const FAINT_COLUMNS: f64 = 640.0;
const BRIGHT_COLUMNS: f64 = 280.0;

impl SkyDraw<'_> {
    /// How fast the stars move away from the direction of travel at `warp`, in the
    /// units of [`SkyDraw::travel`] a second: none at 0, rising with its square.
    pub fn travel_rate(warp: f32) -> f64 {
        let warp = f64::from(finite(warp, 0.0, 1.0));
        WARP_SPEED * warp * warp
    }
}

/// The backdrop's pipeline, its sampler and the stand-in picture of a sky without one.
pub(crate) struct SkyRuntime {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
}

impl SkyRuntime {
    pub(crate) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-sky"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("filter/colour.wgsl"), include_str!("sky.wgsl")).into(),
            ),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-sky"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-sky"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-sky"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
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
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LAYER_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // The panorama wraps around left to right; it is read at its own size.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-sky"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-render-sky-blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            pipeline,
            layout,
            sampler,
            blank,
        }
    }

    /// Draws `sky` into `target`, a layer of this compositor, replacing every texel.
    pub(crate) fn draw(
        &self,
        gpu: &Gpu,
        owner: &Owner,
        sky: &SkyDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let Some(view) = target.target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the target is not a layer this compositor made".into(),
            });
        };
        let canvas = Canvas {
            width: target.width(),
            height: target.height(),
        };
        let camera = sky
            .camera
            .view(canvas)
            .map_err(|err| RenderError::Invalid {
                what: "camera",
                detail: err.to_string(),
            })?;
        let image = sky
            .environment
            .image
            .filter(|image| image.owner().is(owner));
        let values = sky_values(sky, image, &camera);
        let device = &gpu.device;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("jstream-render-sky"),
            contents: &values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<u8>>(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let picture = image.map(|image| {
            image
                .texture()
                .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-sky"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        picture.as_ref().unwrap_or(&self.blank),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render-sky"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-sky"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The triangle writes every texel.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        target.mark_changed();
        drop(submitting);
        Ok(())
    }
}

/// `value` when finite, clamped to `low..=high`, or `low`.
fn finite(value: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        low
    }
}

/// `sky` in the shader's terms (`sky.wgsl`'s `Sky`): the camera's axes, principal point
/// and focal length, the colours as linear light times the intensity, the opacity, the
/// turn, how the picture stores alpha (-1 for none), and the stars.
fn sky_values(
    sky: &SkyDraw<'_>,
    image: Option<&Image>,
    view: &jstream_protocol::CameraView,
) -> [f32; 44] {
    let environment = &sky.environment;
    let intensity = finite(environment.intensity, 0.0, f32::MAX);
    let linear = |color: Color| {
        let [r, g, b, _] = crate::color::premultiplied_linear(Color { a: 1.0, ..color });
        [r, g, b].map(|c| c * intensity)
    };
    let [top, horizon, ground] =
        [environment.sky, environment.horizon, environment.ground].map(linear);
    let turn = finite(environment.turn_deg, -1.0e6, 1.0e6).to_radians();
    let [right, down, forward] = view.axes().map(|axis| axis.map(|c| c as f32));
    let [cx, cy] = view.principal();
    let deep = &sky.deep;
    let [nebula_a, nebula_b] = deep.nebula_colors.map(linear);
    let warp = finite(sky.warp, 0.0, 1.0);
    let bright = cells(sky.travel, BRIGHT_COLUMNS);
    let faint = cells(sky.travel, FAINT_COLUMNS);
    [
        right[0],
        right[1],
        right[2],
        cx,
        down[0],
        down[1],
        down[2],
        cy,
        forward[0],
        forward[1],
        forward[2],
        view.focal_px(),
        top[0],
        top[1],
        top[2],
        finite(sky.opacity, 0.0, 1.0),
        horizon[0],
        horizon[1],
        horizon[2],
        turn,
        ground[0],
        ground[1],
        ground[2],
        image.map_or(-1.0, |image| image.alpha().shader_mode()),
        finite(sky.stars, 0.0, 1.0),
        finite(sky.star_brightness, 0.0, MAX_STAR_BRIGHTNESS) * intensity,
        intensity,
        WARP_STREAK * warp * warp,
        nebula_a[0],
        nebula_a[1],
        nebula_a[2],
        finite(deep.nebula, 0.0, 1.0),
        nebula_b[0],
        nebula_b[1],
        nebula_b[2],
        finite(deep.galaxy, 0.0, 1.0),
        bright.1,
        f32::from_bits(bright.0),
        faint.1,
        f32::from_bits(faint.0),
        f32::from_bits(deep.seed),
        warp * MAX_WARP_BETA,
        if sky.dropping { 1.0 } else { 0.0 },
        0.0,
    ]
}

/// `travel` in cells of a layer of `columns`: its whole cells, wrapped to 32 bits, and
/// its fraction, so a long journey keeps every cell exact.
fn cells(travel: f64, columns: f64) -> (u32, f32) {
    let travel = if travel.is_finite() { travel } else { 0.0 };
    let cells = travel / (std::f64::consts::TAU / columns);
    let whole = cells.floor();
    let wrapped = whole.rem_euclid(4_294_967_296.0) as u32;
    (wrapped, (cells - whole) as f32)
}

#[cfg(test)]
mod tests;
