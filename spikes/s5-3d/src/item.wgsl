// One 3D item: a convex polygon whose vertices carry canvas position, w and uv.
//
// The vertex shader writes clip = (ndc * w, 0, w). The GPU divides by w (giving back the
// projected canvas point) and interpolates uv / w and 1 / w linearly in screen space,
// which is exactly perspective-correct for a plane. With w = 1 it is the 2D compositor's
// existing affine path.
//
// With `analytic` set, the fragment multiplies by its coverage of the polygon, computed
// from the signed distance to each projected edge, so edges are antialiased without
// MSAA. The polygon is drawn pushed out by a pixel so those fringe pixels rasterize.

struct Item {
    canvas: vec2<f32>,
    edge_count: u32,
    analytic: u32,
    edges: array<vec4<f32>, 8>,
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> item: Item;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) w: f32, @location(2) uv: vec2<f32>) -> VsOut {
    let ndc = vec2<f32>(pos.x / item.canvas.x * 2.0 - 1.0, 1.0 - pos.y / item.canvas.y * 2.0);
    var out: VsOut;
    out.clip = vec4<f32>(ndc * w, 0.0, w);
    out.uv = uv;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    var coverage = 1.0;
    if (item.analytic != 0u) {
        var d = 1e9;
        for (var i = 0u; i < item.edge_count; i++) {
            let e = item.edges[i];
            d = min(d, dot(e.xy, in.clip.xy) + e.z);
        }
        coverage = clamp(d + 0.5, 0.0, 1.0);
    }
    // Texels are opaque, so straight and premultiplied agree.
    return c * coverage;
}
