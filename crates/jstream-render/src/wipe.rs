//! Making a layer transparent over only the texels that may hold anything (#1449).
//!
//! A layer's [`content`](crate::Image::content) says where it can be other than
//! transparent. A pass that would clear the whole layer before drawing into it instead
//! loads it and writes transparent over that region alone, which on a software renderer
//! is most of a sparse layer's cost.

use crate::image::LAYER_FORMAT;
use crate::mips::Region;

const SHADER: &str = "
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
";

/// The pipeline that writes transparent into a layer, under a scissor.
pub(crate) struct Wipe {
    pipeline: wgpu::RenderPipeline,
}

impl Wipe {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-wipe"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-wipe"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-wipe"),
            layout: Some(&layout),
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
        Self { pipeline }
    }

    /// Writes transparent over `region` of the layer `pass` draws into, then leaves the
    /// scissor at the whole `width` by `height` layer. An empty region writes nothing.
    pub(crate) fn record(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        region: Region,
        (width, height): (u32, u32),
    ) {
        let Some([x0, y0, x1, y1]) = clamp(region, width, height) else {
            return;
        };
        pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
        pass.set_pipeline(&self.pipeline);
        pass.draw(0..3, 0..1);
        pass.set_scissor_rect(0, 0, width, height);
    }
}

/// `region` as `[x0, y0, x1, y1]` inside a `width` by `height` layer, `None` when none
/// of it is.
fn clamp(region: Region, width: u32, height: u32) -> Option<[u32; 4]> {
    let x0 = region.x.min(width);
    let y0 = region.y.min(height);
    let x1 = region.x.saturating_add(region.width).min(width);
    let y1 = region.y.saturating_add(region.height).min(height);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
}

/// Whether nothing of `region` is outside `rect`, `[x0, y0, x1, y1]`: an empty region
/// never is.
pub(crate) fn within(region: Region, [x0, y0, x1, y1]: [u32; 4]) -> bool {
    region.width == 0
        || region.height == 0
        || (region.x >= x0
            && region.y >= y0
            && region.x.saturating_add(region.width) <= x1
            && region.y.saturating_add(region.height) <= y1)
}

/// `[x0, y0, x1, y1]` as a region.
pub(crate) fn region([x0, y0, x1, y1]: [u32; 4]) -> Region {
    Region {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

/// The region of nothing.
pub(crate) const EMPTY: Region = Region {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
};

/// Whether `region` holds no texels.
pub(crate) fn is_empty(region: Region) -> bool {
    region.width == 0 || region.height == 0
}

/// The smallest region holding both; an empty one adds nothing.
pub(crate) fn union(a: Region, b: Region) -> Region {
    if is_empty(a) {
        return b;
    }
    if is_empty(b) {
        return a;
    }
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.width).max(b.x + b.width);
    let y1 = (a.y + a.height).max(b.y + b.height);
    region([x0, y0, x1, y1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn regions_compare_and_join_as_rectangles() {
        assert!(within(at(2, 2, 3, 3), [2, 2, 5, 5]));
        assert!(!within(at(2, 2, 4, 3), [2, 2, 5, 5]));
        assert!(within(EMPTY, [0, 0, 0, 0]));
        assert_eq!(union(at(1, 1, 2, 2), at(5, 0, 1, 1)), at(1, 0, 5, 3));
        assert_eq!(union(EMPTY, at(5, 0, 1, 1)), at(5, 0, 1, 1));
        assert_eq!(clamp(at(6, 6, 9, 9), 8, 8), Some([6, 6, 8, 8]));
        assert_eq!(clamp(at(9, 0, 1, 1), 8, 8), None);
    }
}
