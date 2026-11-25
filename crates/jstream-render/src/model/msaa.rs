//! Multisampled anti-aliasing of a model's layer (ADR-0242): the samples a model is drawn
//! with, the multisampled targets its pass draws into before they resolve into its
//! layer, and the pass that resolves its surface buffer's three layers.
//!
//! The colour resolves in the pass itself, the samples averaged by the GPU into the
//! layer, which is premultiplied linear light, so an edge's texel holds the model's
//! share of it with that share as its alpha, and the layer's filters (bloom among them)
//! run on the smoothed picture. The surface buffer can't be averaged: a normal or a
//! depth half one surface and half another is neither, so [`SurfaceResolve`] keeps one
//! sample of each texel.

use crate::gpu::Gpu;
use crate::image::LAYER_FORMAT;

use super::DEPTH_FORMAT;

/// The sample counts a model may be drawn with, as `RenderFeatures::model_antialiasing`
/// names them, most first: 1 draws it without anti-aliasing.
const COUNTS: [u32; 4] = [8, 4, 2, 1];

/// The targets of the sizes and counts used last that are kept: as the depth targets
/// are, two, since a model drawn with fewer pixels than its canvas (ADR-0197) alternates
/// with one drawn at the canvas's size.
const KEPT: usize = 2;

/// The most samples no more than `asked` that this device draws a model's layer and its
/// depth with, and resolves: 1 when `asked` is 1 or less. A count the device can't draw
/// steps down to the next it can, so a setting made on one machine draws on another.
pub(super) fn supported(gpu: &Gpu, asked: u32) -> u32 {
    // Without the adapter's own format features a device takes only the counts every
    // WebGPU device takes: 1 and 4.
    let specific = gpu
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    let takes = |count: u32| {
        if count == 1 {
            return true;
        }
        if !specific {
            return count == 4;
        }
        let colour = gpu.adapter.get_texture_format_features(LAYER_FORMAT).flags;
        let depth = gpu.adapter.get_texture_format_features(DEPTH_FORMAT).flags;
        colour.sample_count_supported(count)
            && colour.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
            && depth.sample_count_supported(count)
    };
    COUNTS
        .into_iter()
        .filter(|&count| count <= asked.max(1))
        .find(|&count| takes(count))
        .unwrap_or(1)
}

/// The multisampled targets of one size and sample count.
#[derive(Clone)]
pub(super) struct Drawn {
    /// The layer's colour, resolved into the layer by the pass.
    pub colour: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    /// The surface buffer's three, when a draw had one, and the bind group that reads
    /// them into [`SurfaceResolve`].
    pub surface: Option<([wgpu::TextureView; 3], wgpu::BindGroup)>,
}

/// The multisampled targets of the sizes and counts drawn last.
#[derive(Default)]
pub(super) struct Targets {
    kept: Vec<((u32, u32), u32, Drawn)>,
}

impl Targets {
    /// The targets of `size` and `samples`, with a surface buffer's when `surface`: the
    /// ones kept, else made, the latest used kept first.
    pub(super) fn get(
        &mut self,
        gpu: &Gpu,
        (size, samples): ((u32, u32), u32),
        surface: Option<&SurfaceResolve>,
    ) -> Drawn {
        let at = self
            .kept
            .iter()
            .position(|(s, n, _)| (*s, *n) == (size, samples));
        let mut drawn = match at {
            Some(at) => {
                self.kept[..=at].rotate_right(1);
                self.kept[0].2.clone()
            }
            None => {
                let drawn = Drawn {
                    colour: target(
                        gpu,
                        "jstream-render-model-msaa",
                        size,
                        samples,
                        LAYER_FORMAT,
                    ),
                    depth: target(
                        gpu,
                        "jstream-render-model-msaa-depth",
                        size,
                        samples,
                        DEPTH_FORMAT,
                    ),
                    surface: None,
                };
                self.kept.truncate(KEPT - 1);
                self.kept.insert(0, (size, samples, drawn.clone()));
                drawn
            }
        };
        if let (Some(resolve), None) = (surface, &drawn.surface) {
            let views = std::array::from_fn(|_| {
                target(
                    gpu,
                    "jstream-render-model-msaa-surface",
                    size,
                    samples,
                    LAYER_FORMAT,
                )
            });
            let bind = resolve.bind(gpu, &views);
            drawn.surface = Some((views, bind));
            self.kept[0].2 = drawn.clone();
        }
        drawn
    }
}

/// One multisampled attachment.
fn target(
    gpu: &Gpu,
    label: &str,
    (width, height): (u32, u32),
    samples: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        // The depth too is read, for a model's view depth (`ModelRuntime::record_view_depth`).
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The pass of `surface_resolve.wgsl`: a multisampled surface buffer into its three
/// layers, each texel the nearest of its samples.
pub(super) struct SurfaceResolve {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl SurfaceResolve {
    pub(super) fn new(gpu: &Gpu) -> Self {
        const SOURCE: &str = include_str!("surface_resolve.wgsl");
        let device = &gpu.device;
        let entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: true,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
            entries: &[entry(0), entry(1), entry(2)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let plain = Some(wgpu::ColorTargetState {
            format: LAYER_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        });
        let desc = wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
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
                targets: &[plain.clone(), plain.clone(), plain],
            }),
            multiview_mask: None,
            cache: None,
        };
        let pipeline = gpu.built.render(device, SOURCE, &(), &desc);
        Self { layout, pipeline }
    }

    fn bind(&self, gpu: &Gpu, views: &[wgpu::TextureView; 3]) -> wgpu::BindGroup {
        let entry = |binding: u32| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(&views[binding as usize]),
        };
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
            layout: &self.layout,
            entries: &[entry(0), entry(1), entry(2)],
        })
    }

    /// Records the resolve of `bind`'s multisampled surface into `layers`, every texel
    /// of them replaced.
    pub(super) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        layers: [&wgpu::TextureView; 3],
    ) {
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-model-surface-resolve"),
            color_attachments: &[
                attachment(layers[0]),
                attachment(layers[1]),
                attachment(layers[2]),
            ],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
