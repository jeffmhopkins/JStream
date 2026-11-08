//! Drawing 3D models (ADR-0107 decisions 3, 4 and 6): a [`jstream_media::model::Model`]
//! uploaded once per version, then drawn through a scene camera (or an item's own
//! viewer) into a canvas-sized layer with its own depth test.
//!
//! The layer is premultiplied linear, like every layer, so filters run over it and a
//! scene places it with [`crate::Placement::Layer`], which sorts it with the scene's other
//! items at the model's centre.

use std::sync::Arc;

use jstream_media::model::{AlphaMode, Model, ModelTexture, TextureColor, TextureRef};
use jstream_protocol::{Canvas, ItemGeometry, SceneCamera, Solid, SolidBounds};
use wgpu::util::DeviceExt;

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::image::{Image, LAYER_FORMAT, Owner};
use crate::scene::Color;

/// The depth format: reversed, so `f32`'s precision near 0 is spent far away.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Bytes of one vertex: position, normal, tangent, texture coordinate and colour.
const VERTEX_STRIDE: u64 = jstream_media::model::VERTEX_BYTES;
/// The most point, spot and directional lights a `scene`-lit model is shaded with
/// (ADR-0137 decision 3); any after them are ignored.
pub const MAX_SCENE_LIGHTS: usize = 8;
/// Bytes of one light in the frame uniform: four `vec4`.
const LIGHT_BYTES: u64 = 64;
/// Bytes of the frame uniform: two `mat4x4`, a `mat3x3` (three padded columns), a
/// `vec4` of parameters, a `vec4` of ambient light, and the lights.
const FRAME_BYTES: u64 = 64 + 64 + 48 + 16 + 16 + MAX_SCENE_LIGHTS as u64 * LIGHT_BYTES;
/// Bytes of a material's uniform: four `vec4`.
const MATERIAL_BYTES: u64 = 64;

pub use jstream_media::model::Lighting;

/// A model on the GPU, made by [`crate::Compositor::upload_model`]. Cheap to clone; a
/// clone shares the buffers and textures.
#[derive(Clone)]
pub struct GpuModel {
    inner: Arc<Uploaded>,
}

struct Uploaded {
    owner: Owner,
    version: u64,
    bounds: SolidBounds,
    primitives: Vec<GpuPrimitive>,
    materials: Vec<GpuMaterial>,
}

struct GpuPrimitive {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    material: usize,
    /// The middle of its vertices' bounds, which a blended primitive sorts by.
    center: [f32; 3],
}

struct GpuMaterial {
    bind: wgpu::BindGroup,
    blended: bool,
}

impl GpuModel {
    /// The version of the model it was uploaded from
    /// ([`jstream_media::model::Model::version`]).
    pub fn version(&self) -> u64 {
        self.inner.version
    }

    /// The model's bounds, in its own space.
    pub fn bounds(&self) -> SolidBounds {
        self.inner.bounds
    }
}

impl std::fmt::Debug for GpuModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuModel")
            .field("version", &self.inner.version)
            .field("primitives", &self.inner.primitives.len())
            .finish_non_exhaustive()
    }
}

/// One placement of a model: what [`crate::Compositor::draw_model`] draws.
#[derive(Clone, Copy, Debug)]
pub struct ModelDraw<'a> {
    /// The model. It must have been uploaded by the compositor that draws it.
    pub model: &'a GpuModel,
    /// The item's geometry: the model's pose (ADR-0107 decision 3).
    pub geometry: ItemGeometry,
    /// The camera of the 3D scene the item is in, or `None` in a 2D scene.
    pub camera: Option<SceneCamera>,
    /// How it is lit.
    pub lighting: Lighting,
    /// The lights of the scene the item is in, which light the model in place of the
    /// studio rig (ADR-0137 decision 3), or `None` for `lighting`'s own. A caller passes
    /// `Some` only for a model whose source asks to be lit by its scene, in the scene's
    /// stacking order before any depth sort; the first [`MAX_SCENE_LIGHTS`] that aren't
    /// ambient shade it, lit or not, and the caller logs any it drops. Only a 3D scene
    /// has them: without a `camera` they are ignored and `lighting` applies. An `unlit`
    /// model, and an unlit material, ignore them too.
    pub lights: Option<&'a [SceneLight]>,
}

/// What kind of light a [`SceneLight`] is (ADR-0137 decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// From its position in every direction, fading out to its range.
    Point,
    /// From its position along its aim, within its cone, fading out to its range.
    Spot,
    /// Along its aim from infinitely far, with no falloff.
    Directional,
    /// From everywhere: scales the model's environment for its ambient and reflected
    /// light. Ambient lights add up.
    Ambient,
}

/// One light of a scene, as its item places it (ADR-0137 decisions 1 and 2), in the
/// scene's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneLight {
    /// What kind it is.
    pub kind: LightKind,
    /// Where it is, in the scene's space (x right, y down, z toward the viewer), for a
    /// point or spot light: its item's centre and `depth_px`.
    pub position: [f64; 3],
    /// The direction it shines, in the scene's space, for a spot or directional light:
    /// its item's back normal. Need not be unit length; a zero aim lights nothing.
    pub aim: [f64; 3],
    /// Its colour, straight sRGB. Alpha is ignored.
    pub color: Color,
    /// Irradiance on a surface facing it, before falloff: the kind's `intensity` times its
    /// item's opacity. At or under 0 it lights nothing but still takes its slot. A light
    /// with a non-finite intensity, range, cone, softness or (point or spot) position
    /// lights nothing too.
    pub intensity: f32,
    /// Where a point or spot light has faded to nothing, in scene pixels; 0 never fades.
    pub range_px: f32,
    /// A spot light's full cone angle, in degrees.
    pub cone_deg: f32,
    /// The share of a spot light's cone, from its edge inward, over which it fades, 0 to 1.
    pub softness: f32,
}

/// The pipelines, layouts and shared resources the model draw needs, and the depth
/// target it reuses.
pub(crate) struct ModelRuntime {
    material_layout: wgpu::BindGroupLayout,
    opaque: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    /// The model's view depth (ADR-0137 decision 4), keeping the larger value.
    depth_only: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    frame: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,
    /// 1x1 stand-ins for a texture a material doesn't have: white for colour, occlusion
    /// and metal-roughness, and a flat normal.
    white: wgpu::TextureView,
    flat_normal: wgpu::TextureView,
    depth: Option<(u32, u32, wgpu::TextureView)>,
    /// Blended primitives in the order they draw, kept to reuse the allocation.
    order: Vec<(f64, usize)>,
}

impl ModelRuntime {
    pub(crate) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-frame"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(FRAME_BYTES),
                },
                count: None,
            }],
        });
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(MATERIAL_BYTES),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                texture_entry(5),
                texture_entry(6),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-model"),
            source: wgpu::ShaderSource::Wgsl(include_str!("model.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-model"),
            bind_group_layouts: &[Some(&frame_layout), Some(&material_layout)],
            immediate_size: 0,
        });
        let pipeline = |blended: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(if blended {
                    "jstream-render-model-blended"
                } else {
                    "jstream-render-model-opaque"
                }),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: VERTEX_STRIDE,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3,
                            1 => Float32x3,
                            2 => Float32x4,
                            3 => Float32x2,
                            4 => Float32x4,
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // A front face winds clockwise on the y-down canvas, which is
                    // clockwise in clip space too. The shader discards back faces, since
                    // a mirrored placement swaps them.
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(!blended),
                    depth_compare: Some(wgpu::CompareFunction::Greater),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = pipeline(false);
        let blended = pipeline(true);
        let largest = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let depth_only = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-model-view-depth"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x4,
                        3 => Float32x2,
                        4 => Float32x4,
                    ],
                })],
            },
            // Both faces: the nearest of them wins the blend.
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_depth"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LAYER_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: largest,
                        alpha: largest,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-model-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let frame = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-model-frame"),
            size: FRAME_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-model-frame"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame.as_entire_binding(),
            }],
        });
        let pixel = |label: &str, rgba: [u8; 4]| {
            let texture = device.create_texture_with_data(
                &gpu.queue,
                &wgpu::TextureDescriptor {
                    label: Some(label),
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
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let white = pixel("jstream-render-model-white", [255; 4]);
        let flat_normal = pixel("jstream-render-model-normal", [128, 128, 255, 255]);
        Self {
            material_layout,
            opaque,
            blended,
            depth_only,
            sampler,
            frame,
            frame_bind,
            white,
            flat_normal,
            depth: None,
            order: Vec::new(),
        }
    }

    /// Uploads `model`: its vertex and index buffers, its textures with their mips, and a
    /// bind group per material.
    pub(crate) fn upload(
        &self,
        gpu: &Gpu,
        owner: &Owner,
        model: &Model,
    ) -> Result<GpuModel, RenderError> {
        let device = &gpu.device;
        // Every primitive is one vertex and one index buffer, which wgpu panics on, rather
        // than refuses, past the device's limit: check before uploading anything.
        for p in model.primitives() {
            let vertex_bytes = p.vertices.len() as u64 * VERTEX_STRIDE;
            let index_bytes = p.indices.len() as u64 * 4;
            if vertex_bytes.max(index_bytes) > gpu.max_buffer_size {
                return Err(RenderError::Invalid {
                    what: "model",
                    detail: format!(
                        "a primitive of {} vertices and {} indices needs a {} byte buffer; this GPU takes {}",
                        p.vertices.len(),
                        p.indices.len(),
                        vertex_bytes.max(index_bytes),
                        gpu.max_buffer_size
                    ),
                });
            }
        }
        let mut textures = Vec::with_capacity(model.textures().len());
        for texture in model.textures() {
            textures.push(upload_texture(gpu, texture)?);
        }
        let view =
            |texture: Option<TextureRef>, fallback: &wgpu::TextureView| -> wgpu::TextureView {
                texture
                    .and_then(|t| textures.get(t.texture))
                    .unwrap_or(fallback)
                    .clone()
            };
        let mut materials = Vec::with_capacity(model.materials().len());
        for m in model.materials() {
            let (mode, cutoff) = match m.alpha_mode {
                AlphaMode::Opaque => (0.0, 0.0),
                AlphaMode::Mask { cutoff } => (1.0, cutoff),
                AlphaMode::Blend => (2.0, 0.0),
            };
            let values: [f32; 16] = [
                m.base_color[0],
                m.base_color[1],
                m.base_color[2],
                m.base_color[3],
                m.emissive[0],
                m.emissive[1],
                m.emissive[2],
                if m.normal_texture.is_some() { 1.0 } else { 0.0 },
                m.metallic,
                m.roughness,
                m.normal_scale,
                m.occlusion_strength,
                mode,
                cutoff,
                if m.double_sided { 1.0 } else { 0.0 },
                if m.unlit { 1.0 } else { 0.0 },
            ];
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-material"),
                contents: &f32_bytes(&values),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let views = [
                view(m.base_color_texture, &self.white),
                view(m.metallic_roughness_texture, &self.white),
                view(m.normal_texture, &self.flat_normal),
                view(m.occlusion_texture, &self.white),
                view(m.emissive_texture, &self.white),
            ];
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ];
            for (i, view) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 2 + i as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-model-material"),
                layout: &self.material_layout,
                entries: &entries,
            });
            materials.push(GpuMaterial {
                bind,
                blended: matches!(m.alpha_mode, AlphaMode::Blend),
            });
        }
        let mut primitives = Vec::with_capacity(model.primitives().len());
        for p in model.primitives() {
            if p.indices.is_empty() {
                continue;
            }
            let mut bytes = Vec::with_capacity(p.vertices.len() * VERTEX_STRIDE as usize);
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for v in &p.vertices {
                for value in v
                    .position
                    .iter()
                    .chain(&v.normal)
                    .chain(&v.tangent)
                    .chain(&v.uv)
                    .chain(&v.color)
                {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                for c in 0..3 {
                    min[c] = min[c].min(v.position[c]);
                    max[c] = max[c].max(v.position[c]);
                }
            }
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-vertices"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_bytes: Vec<u8> = p.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-indices"),
                contents: &index_bytes,
                usage: wgpu::BufferUsages::INDEX,
            });
            primitives.push(GpuPrimitive {
                vertices,
                indices,
                index_count: u32::try_from(p.indices.len()).map_err(|_| RenderError::Invalid {
                    what: "model",
                    detail: "a primitive has more indices than a draw can take".into(),
                })?,
                material: p.material,
                center: [0, 1, 2].map(|c| (min[c] + max[c]) * 0.5),
            });
        }
        let bounds = model.bounds();
        Ok(GpuModel {
            inner: Arc::new(Uploaded {
                owner: owner.clone(),
                version: model.version(),
                bounds: SolidBounds {
                    min: bounds.min,
                    max: bounds.max,
                },
                primitives,
                materials,
            }),
        })
    }

    /// Draws `draw` into `target`, replacing what it held: transparent wherever the
    /// model isn't.
    pub(crate) fn draw(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let model = &draw.model.inner;
        if !model.owner.is(owner) {
            return Err(RenderError::Invalid {
                what: "model",
                detail: "the model was uploaded by another compositor".into(),
            });
        }
        let Some(view) = target.target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the target is not a layer this compositor made".into(),
            });
        };
        let (width, height) = (target.width(), target.height());
        let canvas = Canvas { width, height };
        let camera = match draw.camera {
            Some(camera) => Some(camera.view(canvas).map_err(placement)?),
            None => None,
        };
        let solid = draw
            .geometry
            .solid(model.bounds, camera.as_ref())
            .map_err(placement)?;
        // Orthographic depth: scaled so the model's own depth stays well inside 0..1.
        let reach = (0..8)
            .map(|i| {
                let pick = |bit: usize, axis: usize| {
                    if i & (1 << bit) == 0 {
                        model.bounds.min[axis]
                    } else {
                        model.bounds.max[axis]
                    }
                };
                solid.in_view([pick(0, 0), pick(1, 1), pick(2, 2)])[2].abs()
            })
            .fold(1.0f64, f64::max);
        let clip = solid.clip_matrix(canvas, 0.45 / reach);
        let lights = match (draw.lights, camera.as_ref()) {
            (Some(lights), Some(view)) if draw.lighting != Lighting::Unlit => {
                Some(SceneLighting::new(lights, view))
            }
            _ => None,
        };
        let frame = frame_bytes(&solid, &clip, draw.lighting, lights.as_ref());
        let depth = self.depth_view(gpu, width, height);
        // Blended primitives far to near by their centres.
        self.order.clear();
        for (index, p) in model.primitives.iter().enumerate() {
            if model.materials.get(p.material).is_some_and(|m| m.blended) {
                self.order.push((solid.in_view(p.center)[2], index));
            }
        }
        self.order
            .sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        gpu.queue.write_buffer(&self.frame, 0, &frame);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.frame_bind, &[]);
            let draw_one = |pass: &mut wgpu::RenderPass<'_>, p: &GpuPrimitive| {
                let Some(material) = model.materials.get(p.material) else {
                    return;
                };
                pass.set_bind_group(1, &material.bind, &[]);
                pass.set_vertex_buffer(0, p.vertices.slice(..));
                pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.index_count, 0, 0..1);
            };
            pass.set_pipeline(&self.opaque);
            for p in &model.primitives {
                if !model.materials.get(p.material).is_some_and(|m| m.blended) {
                    draw_one(&mut pass, p);
                }
            }
            if !self.order.is_empty() {
                pass.set_pipeline(&self.blended);
                for &(_, index) in &self.order {
                    draw_one(&mut pass, &model.primitives[index]);
                }
            }
        }
        gpu.queue.submit([encoder.finish()]);
        target.mark_changed();
        drop(submitting);
        Ok(())
    }

    /// Adds `draw`'s view depth to `target` (ADR-0137 decision 4), keeping what is there
    /// where it is nearer. Nothing without a camera.
    pub(crate) fn draw_depth(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let model = &draw.model.inner;
        if !model.owner.is(owner) {
            return Err(RenderError::Invalid {
                what: "model",
                detail: "the model was uploaded by another compositor".into(),
            });
        }
        let Some(view) = target.target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the target is not a layer this compositor made".into(),
            });
        };
        let Some(camera) = draw.camera else {
            return Ok(());
        };
        let canvas = Canvas {
            width: target.width(),
            height: target.height(),
        };
        let camera = camera.view(canvas).map_err(placement)?;
        let solid = draw
            .geometry
            .solid(model.bounds, Some(&camera))
            .map_err(placement)?;
        // With perspective the orthographic depth scale is unused.
        let clip = solid.clip_matrix(canvas, 0.0);
        let frame = frame_bytes(&solid, &clip, Lighting::Unlit, None);
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        gpu.queue.write_buffer(&self.frame, 0, &frame);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model-view-depth"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-view-depth"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.depth_only);
            pass.set_bind_group(0, &self.frame_bind, &[]);
            for p in &model.primitives {
                let Some(material) = model.materials.get(p.material) else {
                    continue;
                };
                pass.set_bind_group(1, &material.bind, &[]);
                pass.set_vertex_buffer(0, p.vertices.slice(..));
                pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.index_count, 0, 0..1);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        target.mark_changed();
        drop(submitting);
        Ok(())
    }

    fn depth_view(&mut self, gpu: &Gpu, width: u32, height: u32) -> wgpu::TextureView {
        if let Some((w, h, view)) = &self.depth
            && (*w, *h) == (width, height)
        {
            return view.clone();
        }
        // A pass's attachments must all be one size, so a layer of another size makes a
        // new one (a canvas resize, or a second canvas through the same compositor).
        let (w, h) = (width, height);
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-model-depth"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some((w, h, view.clone()));
        view
    }
}

fn placement(err: jstream_protocol::GeometryError) -> RenderError {
    RenderError::Invalid {
        what: "placement",
        detail: err.to_string(),
    }
}

/// A scene's lights in the shader's terms: the model's viewing frame, linear light.
struct SceneLighting {
    ambient: [f32; 3],
    /// Four `vec4` per light, as `model.wgsl`'s `Light`.
    lights: Vec<[f32; 16]>,
}

impl SceneLighting {
    fn new(lights: &[SceneLight], view: &jstream_protocol::CameraView) -> Self {
        let mut ambient = [0.0f32; 3];
        let mut packed = Vec::with_capacity(MAX_SCENE_LIGHTS);
        // Slots taken by the shading lights so far, lit or not.
        let mut slots = 0;
        for light in lights {
            let positive = |v: f32| v.is_finite() && v > 0.0;
            if light.kind != LightKind::Ambient {
                // Every shading light takes one of the 8 in order, lit or not, so fading
                // one of them out never switches a ninth on (ADR-0137 decision 3).
                slots += 1;
                if slots > MAX_SCENE_LIGHTS {
                    continue;
                }
            }
            // Unlit, or a value no settings check would let through: lights nothing.
            let placed = light.kind == LightKind::Directional
                || light.kind == LightKind::Ambient
                || light.position.iter().all(|c| c.is_finite());
            if !positive(light.intensity)
                || !placed
                || !light.cone_deg.is_finite()
                || !light.softness.is_finite()
                || !light.range_px.is_finite()
            {
                continue;
            }
            let [r, g, b, _] = crate::color::premultiplied_linear(Color {
                a: 1.0,
                ..light.color
            });
            let radiance = [r, g, b].map(|c| c * light.intensity);
            if light.kind == LightKind::Ambient {
                for (sum, c) in ambient.iter_mut().zip(radiance) {
                    *sum += c;
                }
                continue;
            }
            let aim = view.direction_in_view(light.aim);
            let length = aim.iter().map(|c| c * c).sum::<f64>().sqrt();
            let needs_aim = light.kind != LightKind::Point;
            if needs_aim && !(length > 0.0 && length.is_finite()) {
                continue;
            }
            let aim = if length > 0.0 {
                aim.map(|c| (c / length) as f32)
            } else {
                [0.0, 0.0, -1.0]
            };
            let at = view.in_view(light.position).map(|c| c as f32);
            let kind = match light.kind {
                LightKind::Point => 0.0,
                LightKind::Spot => 1.0,
                _ => 2.0,
            };
            let half = (light.cone_deg.clamp(1.0, 170.0) * 0.5).to_radians();
            let softness = light.softness.clamp(0.0, 1.0);
            let outer = half.cos();
            let inner = (half * (1.0 - softness)).cos();
            packed.push([
                at[0],
                at[1],
                at[2],
                kind,
                aim[0],
                aim[1],
                aim[2],
                light.range_px.max(0.0),
                radiance[0],
                radiance[1],
                radiance[2],
                0.0,
                outer,
                inner,
                0.0,
                0.0,
            ]);
        }
        Self {
            ambient,
            lights: packed,
        }
    }
}

fn frame_bytes(
    solid: &Solid,
    clip: &[[f32; 4]; 4],
    lighting: Lighting,
    lights: Option<&SceneLighting>,
) -> Vec<u8> {
    let mut values = Vec::with_capacity((FRAME_BYTES / 4) as usize);
    values.extend(clip.iter().flatten());
    values.extend(solid.view_matrix().iter().flatten());
    for column in solid.normal_matrix() {
        values.extend(column);
        values.push(0.0);
    }
    let perspective = solid.perspective_px().unwrap_or(0.0) as f32;
    values.extend([
        perspective,
        if solid.mirrored() { 1.0 } else { 0.0 },
        if lighting == Lighting::Unlit {
            1.0
        } else {
            0.0
        },
        if lights.is_some() { 1.0 } else { 0.0 },
    ]);
    let (ambient, count) = lights.map_or(([0.0; 3], 0), |l| (l.ambient, l.lights.len()));
    values.extend(ambient);
    values.push(count as f32);
    for light in lights.into_iter().flat_map(|l| &l.lights) {
        values.extend(light);
    }
    values.resize((FRAME_BYTES / 4) as usize, 0.0);
    f32_bytes(&values)
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn upload_texture(gpu: &Gpu, texture: &ModelTexture) -> Result<wgpu::TextureView, RenderError> {
    let limit = gpu.max_texture_dimension_2d;
    if texture.width() > limit || texture.height() > limit {
        return Err(RenderError::TooLarge {
            what: "image",
            width: texture.width(),
            height: texture.height(),
            limit,
        });
    }
    let format = match texture.color() {
        TextureColor::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        TextureColor::Linear => wgpu::TextureFormat::Rgba8Unorm,
    };
    let data: Vec<u8> = texture.levels().iter().flatten().copied().collect();
    let uploaded = gpu.device.create_texture_with_data(
        &gpu.queue,
        &wgpu::TextureDescriptor {
            label: Some("jstream-render-model-texture"),
            size: wgpu::Extent3d {
                width: texture.width(),
                height: texture.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: texture.levels().len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &data,
    );
    Ok(uploaded.create_view(&wgpu::TextureViewDescriptor::default()))
}

#[cfg(test)]
mod tests;
