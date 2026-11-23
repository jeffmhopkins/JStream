// Screen-space reflections, the resolve (ADR-0170 decision 4, ADR-0189), drawn at the
// picture's size with the shader-source prelude for its six textures: the five
// trace.wgsl reads, and what it wrote at half the size: for one pixel of each block of
// two by two, what that pixel's reflected ray found on screen times how sure the hit
// is, premultiplied.
//
// Each pixel of a surface that may have its environment's reflection replaced takes
// what the marched pixels of the four nearest blocks found, each weighed by how near it
// is on screen, how close it lies to this pixel's own plane, and how alike the two
// normals are, so a reflection doesn't spread over an edge onto another surface. What
// they found replaces this pixel's own environment's reflection under its own BRDF, as
// far as the hits are sure. A pixel with no marched pixel on its surface nearby keeps
// its environment's reflection. Everywhere else the picture passes through.

struct Params {
    // x: focal length f; yz: the principal point; w: the steps a ray takes.
    view: vec4<f32>,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var depth_tex: texture_2d<f32>;
@group(0) @binding(6) var normal_tex: texture_2d<f32>;
@group(0) @binding(7) var brdf_tex: texture_2d<f32>;
@group(0) @binding(8) var reflected_tex: texture_2d<f32>;
@group(0) @binding(9) var traced_tex: texture_2d<f32>;

// How far off this pixel's plane a marched pixel's point may lie, as a part of the
// distance: at this much it counts for nothing.
const OFF_PLANE: f32 = 0.02;

// The pixel of the picture that the march's texel `texel` stands for: trace.wgsl's.
fn chosen(texel: vec2<i32>, size: vec2<i32>) -> vec2<i32> {
    let corner = (texel.x + texel.y) & 1;
    return min(texel * 2 + vec2<i32>(corner), size - vec2<i32>(1));
}

// The point seen through the centre of pixel `px` at `inv`, its 1 / w, in the viewing
// frame (trace.wgsl describes it).
fn point_at(px: vec2<i32>, inv: f32) -> vec3<f32> {
    let f = p.view.x;
    let d = f / inv;
    return vec3<f32>((vec2<f32>(px) + vec2<f32>(0.5) - p.view.yz) * d / f, f - d);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let c = to_premul_linear(textureLoad(input_tex, px, 0), input_alpha(0u));
    let f = p.view.x;
    let reflected = textureLoad(reflected_tex, px, 0);
    let surface = textureLoad(normal_tex, px, 0);
    let brdf = textureLoad(brdf_tex, px, 0);
    let inv = surface.w;
    if reflected.a < 0.5 || f <= 0.0 || inv <= 0.0 || brdf.a >= 0.6 || dot(surface.xyz, surface.xyz) < 0.25 {
        return c;
    }
    // Something nearer than the surface covers it here: a card in front of the model.
    if textureLoad(depth_tex, px, 0).r > inv * 1.002 {
        return c;
    }
    let size = vec2<i32>(textureDimensions(input_tex));
    let blocks = vec2<i32>(textureDimensions(traced_tex));
    let n = normalize(surface.xyz);
    let point = point_at(px, inv);
    let d = f / inv;
    // The block whose centre is up and left of this pixel, and the three after it.
    let first = vec2<i32>(floor((vec2<f32>(px) - vec2<f32>(0.5)) * 0.5));
    var found = vec4<f32>(0.0);
    var weight = 0.0;
    for (var k = 0; k < 4; k++) {
        let texel = first + vec2<i32>(k & 1, k >> 1);
        if any(texel < vec2<i32>(0)) || any(texel >= blocks) {
            continue;
        }
        let q = chosen(texel, size);
        let there = textureLoad(normal_tex, q, 0);
        if there.w <= 0.0 || dot(there.xyz, there.xyz) < 0.25 {
            continue;
        }
        let apart = abs(vec2<f32>(q - px));
        let near = max(1.0 - apart.x / 3.0, 0.0) * max(1.0 - apart.y / 3.0, 0.0);
        let off = abs(dot(n, point_at(q, there.w) - point)) / (OFF_PLANE * d);
        let alike = max(dot(n, normalize(there.xyz)), 0.0);
        let w = near * max(1.0 - off, 0.0) * alike * alike * alike * alike;
        found += textureLoad(traced_tex, texel, 0) * w;
        weight += w;
    }
    if weight < 1e-4 {
        return c;
    }
    found = found / weight;
    let delta = found.rgb * brdf.rgb - found.a * reflected.rgb;
    return vec4<f32>(max(c.rgb + delta, vec3<f32>(0.0)), c.a);
}
