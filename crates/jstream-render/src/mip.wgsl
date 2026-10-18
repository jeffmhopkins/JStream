// The mip chain of a source placed in 3D (ADR-0050 decision 5), premultiplied linear in
// `Rgba16Float` like an upload.
//
// Level 0 is the chain's region of the source (the item's crop) texel for texel: the
// pass is the region's size and `vs_region` maps it onto the region, so each fragment
// samples a source texel centre exactly and nothing past the crop is read. `fs_straight`
// and `fs_srgb` convert an import to premultiplied linear on the way, as the composite
// shader's modes 1 and 2 do. Each further level samples the one above at its own texel
// centre, which falls between four texels, so the linear filter averages them
// (`fs_premultiplied`).

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn cover(index: u32) -> VsOut {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    var out: VsOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    return out;
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    return cover(index);
}

// The region level 0 reads: `[u, v, width, height]` in the source's texture coordinates.
@group(1) @binding(0) var<uniform> region: vec4<f32>;

@vertex
fn vs_region(@builtin(vertex_index) index: u32) -> VsOut {
    var out = cover(index);
    out.uv = region.xy + out.uv * region.zw;
    return out;
}

// IEC 61966-2-1 decode.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_premultiplied(in: VsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, samp, in.uv, 0.0);
}

@fragment
fn fs_straight(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(src, samp, in.uv, 0.0);
    return vec4<f32>(c.rgb * c.a, c.a);
}

@fragment
fn fs_srgb(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(src, samp, in.uv, 0.0);
    let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
    return vec4<f32>(srgb_to_linear(straight) * c.a, c.a);
}
