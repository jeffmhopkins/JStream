//! A 3D scene's sky backdrop (ADR-0167): its environment light's sky drawn behind the
//! scene, infinitely far away, through the scene's camera.
//!
//! Each canvas pixel shows the sky along its ray from the camera, so the backdrop turns
//! with where the camera looks and never moves with where it is. It draws into a layer
//! of the canvas size, which the scene then places under every item. The pipeline is
//! built the first time a backdrop draws, so a compositor that never draws one pays
//! nothing for it.
//!
//! The nebulae and the galaxy band are dozens of noise octaves a direction, and depend
//! only on the direction and the sky's seed. So they are baked once per seed into two
//! cube maps ([`Bake`], ADR-0174), and a frame's sky reads them: a turning camera, a
//! warp and a change of colour or amount all cost a few texture reads a pixel.
//!
//! What is left still costs a full-canvas pass, and depends only on where the camera
//! looks and on the sky's own settings. So a layer that still holds a sky is not drawn
//! again while the same sky is asked for ([`Kept`]): a scene whose camera holds still
//! pays for its sky once.

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::image::{Image, LAYER_FORMAT, Owner};
use crate::model::Environment;
use crate::scene::Color;
use jstream_protocol::{Canvas, SceneCamera};
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
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
    /// How much the warp is dropping toward a lower setting, 0 to 1: as it comes out of
    /// warp the nebulae and band rush back out from straight ahead and settle, rather
    /// than fade. 1 while it drops, easing to 0 as it arrives at a setting above 0, so a
    /// drop to a cruise doesn't snap.
    pub dropping: f32,
    /// The pictures pinned in the sky over everything else it draws (ADR-0188), bottom
    /// first, each over those before it.
    pub pictures: [Option<SkyPicture<'a>>; MAX_SKY_PICTURES],
}

/// The most pictures one backdrop pins in its sky at once (ADR-0198).
pub const MAX_SKY_PICTURES: usize = 4;

/// A flat picture pinned in a backdrop's sky, infinitely far away (ADR-0188): a planet or
/// a nebula to arrive at. It covers the sky, the nebulae and the stars behind it where it
/// is opaque, at the sky's intensity, and fades as the deep sky does at warp.
#[derive(Clone, Copy, Debug)]
pub struct SkyPicture<'a> {
    /// The picture, a layer or upload of this compositor's.
    pub image: &'a Image,
    /// Where its centre is, in the sky's own frame (x right, y down, z toward the viewer
    /// at rest): the frame the sky's layout is in, before its orientation. Need not be
    /// unit length; a zero direction draws nothing.
    pub direction: [f32; 3],
    /// How wide it is across the sky, in degrees, 1 to 120; its height follows its
    /// shape. Its top is toward the sky's up.
    pub width_deg: f32,
    /// How opaque it is, 0 to 1.
    pub opacity: f32,
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

/// The backdrop's pipeline, its samplers, the stand-in picture of a sky without one,
/// and the deep skies baked for the seeds drawn lately.
pub(crate) struct SkyRuntime {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
    /// The skies the layers drawn last still hold, most recently drawn first, at most
    /// [`MAX_KEPT`].
    kept: Vec<Kept>,
    /// Writes the bright stars' table (`sky.wgsl`'s `fs_stars`).
    star_pipeline: wgpu::RenderPipeline,
    /// The bright stars' table: which cells of their layer hold a star, written for
    /// each sky with stars just before the pass that reads it.
    star_table: wgpu::TextureView,
    /// The stand-in table of the pass that writes the table, and of a sky with no stars.
    blank_table: wgpu::TextureView,
    /// Reads a bake: linear, so a view finer than its texels shows no texel.
    deep_sampler: wgpu::Sampler,
    /// The stand-in bake of a sky with no nebula and no band: one black texel a face.
    blank_deep: wgpu::TextureView,
    /// The bake's pipeline, built the first time a sky with nebulae or a band draws.
    baker: Option<Baker>,
    /// The seeds' bakes, most recently drawn first, at most [`MAX_BAKES`].
    bakes: Vec<Bake>,
    /// How many skies were drawn, not counting the ones a layer already held: the clock
    /// a bake's last use is told by.
    pub(crate) draws: u64,
    /// How many bakes were made.
    #[cfg(test)]
    pub(crate) baked: u64,
}

/// The most layers whose skies are remembered: a scene has one, and a transition
/// between two 3D scenes draws both.
const MAX_KEPT: usize = 4;

/// A sky a layer holds: what was drawn into it, and the state it was left in. While the
/// layer and the sky's picture are as they were then, drawing the same values again
/// would write the same texels.
struct Kept {
    /// The layer, by the counter its clones share. `Weak`, so a layer the cache let go
    /// of is forgotten, not kept alive.
    target: Weak<AtomicU64>,
    /// The layer's [`Image::generation`] once the sky was drawn: any later write moves
    /// it on.
    generation: u64,
    /// The shader's values ([`sky_values`]), compared bit for bit.
    values: [u32; VALUES],
    /// The sky's picture and its generation when it was read, for a sky with one.
    picture: Option<(Weak<AtomicU64>, u64)>,
    /// The pictures pinned in it (ADR-0188) and their generations, where it has them.
    pinned: [Option<(Weak<AtomicU64>, u64)>; MAX_SKY_PICTURES],
}

/// How many numbers the shader's uniform holds ([`sky_values`]).
const VALUES: usize = 44 + 16 * MAX_SKY_PICTURES;

impl Kept {
    /// Whether `target` still holds the sky of `values` and `picture`.
    fn holds(
        &self,
        target: &Image,
        values: &[u32; VALUES],
        (picture, pinned): (Option<&Image>, [Option<&Image>; MAX_SKY_PICTURES]),
    ) -> bool {
        let same = |kept: &Weak<AtomicU64>, image: &Image| {
            std::ptr::eq(kept.as_ptr(), std::sync::Arc::as_ptr(image.identity()))
        };
        let read =
            |kept: &Option<(Weak<AtomicU64>, u64)>, image: Option<&Image>| match (kept, image) {
                (None, None) => true,
                (Some((kept, generation)), Some(image)) => {
                    same(kept, image) && *generation == image.generation()
                }
                _ => false,
            };
        same(&self.target, target)
            && self.generation == target.generation()
            && self.values == *values
            && read(&self.picture, picture)
            && self
                .pinned
                .iter()
                .zip(pinned)
                .all(|(kept, image)| read(kept, image))
    }
}

#[cfg(test)]
impl SkyRuntime {
    /// How many seeds' bakes are kept now.
    pub(crate) fn kept_bakes(&self) -> usize {
        self.bakes.len()
    }
}

/// The bright stars' table, as `sky.wgsl` has it: a column of their layer across (281,
/// one for each angle about the travel a pixel can floor to), and 32 rows to a texel
/// down (64 texels, 2048 rows), a bit a row.
const STAR_TABLE: wgpu::Extent3d = wgpu::Extent3d {
    width: 281,
    height: 64,
    depth_or_array_layers: 1,
};
const STAR_TABLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

/// A bake's face is this many texels square. A face spans a right angle, so a bake has
/// about 510 texels a radian at a face's centre and up to twice that toward its edges,
/// across noise whose finest octave is 5 to 7 texels wide: a 1080-line canvas at a 45
/// degree view, about 1300 pixels a radian, enlarges it under three times.
const BAKE_FACE: u32 = 1024;

/// The most seeds whose bakes are kept: a scene has one, a transition between two skies
/// draws two, and a scene nested in another can add its own.
const MAX_BAKES: usize = 4;

/// A bake no sky has read for this many draws is let go: about half a minute of one
/// scene's frames.
const BAKE_IDLE_DRAWS: u64 = 900;

/// The bake's formats: the nebulae's four weights, and the band's light.
const NEBULA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const GALAXY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// The pipeline that bakes a deep sky (`sky_deep.wgsl`).
struct Baker {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// One seed's nebulae and galaxy band (ADR-0174), as cube maps of [`BAKE_FACE`] read
/// along a direction of the sky. They hold shapes only: the nebulae's colours and
/// amount, the band's brightness and the light's intensity scale what is read, so one
/// bake serves every setting of its seed.
struct Bake {
    seed: u32,
    /// The nebulae: the light of each of the two colours, then of each one's sum of
    /// channels.
    nebula: wgpu::TextureView,
    /// The band's light, in red.
    galaxy: wgpu::TextureView,
    /// [`SkyRuntime::draws`] when a sky last read it.
    used: u64,
}

/// Each face of a cube map, in layer order (+x, -x, +y, -y, +z, -z): the direction a
/// step right across it adds, the one a step down adds, and the one at its centre, as
/// a cube map's lookup places them.
const FACES: [[[f32; 3]; 3]; 6] = [
    [[0.0, 0.0, -1.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0]],
    [[0.0, 0.0, 1.0], [0.0, -1.0, 0.0], [-1.0, 0.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]],
    [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]],
    [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]],
];

impl Baker {
    fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        const SOURCE: &str = concat!(
            include_str!("sky_common.wgsl"),
            include_str!("sky_deep.wgsl")
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-sky-bake"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-sky-bake"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-sky-bake"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-sky-bake"),
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
                targets: &[target(NEBULA_FORMAT), target(GALAXY_FORMAT)],
            }),
            multiview_mask: None,
            cache: None,
        };
        let pipeline = gpu.built.render(device, SOURCE, &(), &pipeline);
        Self { pipeline, layout }
    }

    /// Bakes `seed`'s deep sky: every face of both cube maps, one pass a face.
    fn bake(&self, gpu: &Gpu, seed: u32, used: u64) -> Bake {
        let device = &gpu.device;
        let cube = |label, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: BAKE_FACE,
                    height: BAKE_FACE,
                    depth_or_array_layers: 6,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let nebula = cube("jstream-render-sky-nebula", NEBULA_FORMAT);
        let galaxy = cube("jstream-render-sky-galaxy", GALAXY_FORMAT);
        let face_of = |texture: &wgpu::Texture, face: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: face,
                array_layer_count: Some(1),
                ..Default::default()
            })
        };
        fn attachment(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The triangle writes every texel.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })
        }
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render-sky-bake"),
        });
        for (face, [right, down, forward]) in (0u32..).zip(FACES) {
            let values = [
                right[0],
                right[1],
                right[2],
                2.0 / BAKE_FACE as f32,
                down[0],
                down[1],
                down[2],
                0.0,
                forward[0],
                forward[1],
                forward[2],
                f32::from_bits(seed),
            ];
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-sky-bake"),
                contents: &values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<u8>>(),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-sky-bake"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            });
            let (nebula_face, galaxy_face) = (face_of(&nebula, face), face_of(&galaxy, face));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-sky-bake"),
                color_attachments: &[attachment(&nebula_face), attachment(&galaxy_face)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.submit_work(encoder.finish());
        drop(submitting);
        let whole = |texture: &wgpu::Texture| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            })
        };
        Bake {
            seed,
            nebula: whole(&nebula),
            galaxy: whole(&galaxy),
            used,
        }
    }
}

impl SkyRuntime {
    pub(crate) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        const SOURCE: &str = concat!(
            include_str!("filter/colour.wgsl"),
            include_str!("sky_common.wgsl"),
            include_str!("sky.wgsl")
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-sky"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let cube = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::Cube,
                multisampled: false,
            },
            count: None,
        };
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
                cube(3),
                cube(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // The pictures pinned in the sky (ADR-0188).
                pinned_entry(7),
                pinned_entry(8),
                pinned_entry(9),
                pinned_entry(10),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-sky"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline_of = |entry_point, format| {
            let desc = wgpu::RenderPipelineDescriptor {
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
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            };
            gpu.built.render(device, SOURCE, &(), &desc)
        };
        let pipeline = pipeline_of("fs", LAYER_FORMAT);
        let star_pipeline = pipeline_of("fs_stars", STAR_TABLE_FORMAT);
        let table = |label, size, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: STAR_TABLE_FORMAT,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let star_table = table(
            "jstream-render-sky-stars",
            STAR_TABLE,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let blank_table = table(
            "jstream-render-sky-stars-blank",
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
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
        let deep_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-sky-deep"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank_deep = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-render-sky-deep-blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 6,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            });
        Self {
            pipeline,
            layout,
            sampler,
            blank,
            kept: Vec::new(),
            star_pipeline,
            star_table,
            blank_table,
            deep_sampler,
            blank_deep,
            baker: None,
            bakes: Vec::new(),
            draws: 0,
            #[cfg(test)]
            baked: 0,
        }
    }

    /// Makes the bake's pipeline now, not at the first sky with nebulae or a band.
    pub(crate) fn warm(&mut self, gpu: &Gpu) {
        self.baker.get_or_insert_with(|| Baker::new(gpu));
    }

    /// Puts `seed`'s bake first in [`Self::bakes`], as the one drawn last, making it now
    /// if no kept one is its. The one read longest ago past [`MAX_BAKES`] is let go.
    fn bake(&mut self, gpu: &Gpu, seed: u32) {
        let now = self.draws;
        let bake = match self.bakes.iter().position(|bake| bake.seed == seed) {
            Some(at) => self.bakes.remove(at),
            None => {
                #[cfg(test)]
                {
                    self.baked += 1;
                }
                self.baker
                    .get_or_insert_with(|| Baker::new(gpu))
                    .bake(gpu, seed, now)
            }
        };
        self.bakes.truncate(MAX_BAKES - 1);
        self.bakes.insert(0, Bake { used: now, ..bake });
    }

    /// Draws `sky` into `target`, a layer of this compositor, replacing every texel.
    /// A layer that holds this sky already, drawn here and not written since, is left
    /// as it is.
    pub(crate) fn draw(
        &mut self,
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
        let shown = sky
            .pictures
            .map(|picture| picture.filter(|picture| picture.image.owner().is(owner)));
        let values = sky_values(sky, (image, &shown), &camera);
        let pinned = shown.map(|picture| picture.map(|picture| picture.image));
        let bits = values.map(f32::to_bits);
        self.kept.retain(|kept| kept.target.strong_count() > 0);
        if self
            .kept
            .iter()
            .any(|kept| kept.holds(target, &bits, (image, pinned)))
        {
            return Ok(());
        }
        self.draws += 1;
        // Only a sky that shows nebulae or a band needs its seed's bake.
        let deep = sky.deep.nebula > 0.0 || sky.deep.galaxy > 0.0;
        // Bakes no sky has read for a while are let go, whatever sky this one is.
        let now = self.draws;
        self.bakes.retain(|bake| {
            (deep && bake.seed == sky.deep.seed) || now - bake.used < BAKE_IDLE_DRAWS
        });
        if deep {
            self.bake(gpu, sky.deep.seed);
        }
        let (nebula, galaxy) = match self.bakes.first().filter(|_| deep) {
            Some(bake) => (&bake.nebula, &bake.galaxy),
            None => (&self.blank_deep, &self.blank_deep),
        };
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
        let arrivals = pinned.map(|image| {
            image.map(|image| {
                image
                    .texture()
                    .create_view(&wgpu::TextureViewDescriptor::default())
            })
        });
        let arrival = |i: usize| arrivals[i].as_ref().unwrap_or(&self.blank);
        // A sky with stars reads the bright ones' table, written first by a pass that
        // can't read what it writes.
        let stars = sky.stars > 0.0;
        let bind_with = |table: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(nebula),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(galaxy),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.deep_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(table),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(arrival(0)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::TextureView(arrival(1)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 9,
                        resource: wgpu::BindingResource::TextureView(arrival(2)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 10,
                        resource: wgpu::BindingResource::TextureView(arrival(3)),
                    },
                ],
            })
        };
        let writing = stars.then(|| bind_with(&self.blank_table));
        let bind = bind_with(if stars {
            &self.star_table
        } else {
            &self.blank_table
        });
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render-sky"),
        });
        let passes = [
            writing
                .as_ref()
                .map(|bind| (&self.star_pipeline, bind, &self.star_table)),
            Some((&self.pipeline, &bind, view)),
        ];
        for (pipeline, bind, view) in passes.into_iter().flatten() {
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.submit_work(encoder.finish());
        target.mark_changed();
        drop(submitting);
        let same = std::sync::Arc::as_ptr(target.identity());
        self.kept
            .retain(|kept| !std::ptr::eq(kept.target.as_ptr(), same));
        self.kept.truncate(MAX_KEPT - 1);
        self.kept.insert(
            0,
            Kept {
                target: std::sync::Arc::downgrade(target.identity()),
                generation: target.generation(),
                values: bits,
                picture: image.map(|image| {
                    (
                        std::sync::Arc::downgrade(image.identity()),
                        image.generation(),
                    )
                }),
                pinned: pinned.map(|image| {
                    image.map(|image| {
                        (
                            std::sync::Arc::downgrade(image.identity()),
                            image.generation(),
                        )
                    })
                }),
            },
        );
        Ok(())
    }
}

/// The layout entry of pinned picture texture `binding` (ADR-0188).
fn pinned_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
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
/// turn, how the picture stores alpha (-1 for none), the stars, and the picture pinned
/// in it ([`picture_values`]).
fn sky_values(
    sky: &SkyDraw<'_>,
    (image, pinned): (Option<&Image>, &[Option<SkyPicture<'_>>; MAX_SKY_PICTURES]),
    view: &jstream_protocol::CameraView,
) -> [f32; VALUES] {
    let environment = &sky.environment;
    let intensity = finite(environment.intensity, 0.0, f32::MAX);
    let linear = |color: Color| {
        let [r, g, b, _] = crate::color::premultiplied_linear(Color { a: 1.0, ..color });
        [r, g, b].map(|c| c * intensity)
    };
    let [top, horizon, ground] =
        [environment.sky, environment.horizon, environment.ground].map(linear);
    let [right, down, forward] = environment.sky_axes(view.axes());
    let [cx, cy] = view.principal();
    let deep = &sky.deep;
    let [nebula_a, nebula_b] = deep.nebula_colors.map(linear);
    let warp = finite(sky.warp, 0.0, 1.0);
    let bright = cells(sky.travel, BRIGHT_COLUMNS);
    let faint = cells(sky.travel, FAINT_COLUMNS);
    let head = [
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
        0.0,
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
        finite(sky.dropping, 0.0, 1.0),
        warp,
    ];
    let mut values = [0.0; VALUES];
    values[..head.len()].copy_from_slice(&head);
    for (slot, picture) in values[head.len()..].chunks_exact_mut(16).zip(pinned) {
        let p = picture_values(picture.as_ref());
        let flat = [
            p[0][0], p[0][1], p[0][2], p[3][0], p[1][0], p[1][1], p[1][2], p[3][1], p[2][0],
            p[2][1], p[2][2], p[3][2], p[3][3], 0.0, 0.0, 0.0,
        ];
        slot.copy_from_slice(&flat);
    }
    values
}

/// A pinned picture in the shader's terms (four of `sky.wgsl`'s `pictures`): its
/// centre's direction, its right and down axes, all
/// unit length in the sky's frame, then the tangents of half its width and half its
/// height, how opaque it is and how it stores alpha; all 0 for none.
fn picture_values(picture: Option<&SkyPicture<'_>>) -> [[f32; 4]; 4] {
    let none = [[0.0; 4]; 4];
    let Some(picture) = picture else {
        return none;
    };
    let d = picture
        .direction
        .map(|v| if v.is_finite() { v } else { 0.0 });
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let opacity = finite(picture.opacity, 0.0, 1.0);
    if length < 1e-6 || opacity <= 0.0 {
        return none;
    }
    let f = d.map(|v| v / length);
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let unit = |v: [f32; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        v.map(|c| c / l)
    };
    // Its top toward the sky's up (-y); straight up or down, toward ahead (-z).
    let up = if f[1].abs() > 0.999 {
        [0.0, 0.0, -1.0]
    } else {
        [0.0, -1.0, 0.0]
    };
    let right = unit(cross(up, f));
    let down = cross(right, f);
    let half = finite(picture.width_deg, 1.0, 120.0).to_radians() * 0.5;
    let wide = half.tan();
    let (w, h) = (picture.image.width().max(1), picture.image.height().max(1));
    let tall = wide * h as f32 / w as f32;
    let four = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
    [
        four(f),
        four(right),
        four(down),
        [wide, tall, opacity, picture.image.alpha().shader_mode()],
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
