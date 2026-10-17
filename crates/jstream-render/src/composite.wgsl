// One textured polygon. Positions are clip-space (y up). The fragment outputs premultiplied
// linear colour; the FP16 target blends with One, OneMinusSrcAlpha.
//
// `w` is ADR-0050's homogeneous coordinate, 1 for a 2D item. The vertex writes
// clip = (pos * w, 0, w): the GPU divides by w, giving back the projected point, and
// interpolates uv / w and 1 / w linearly on screen, which is perspective-correct for a
// plane. With w = 1 it is the plain affine 2D path.
//
// `e0` to `e4` are the item's edge lines, the same at every vertex: (nx, ny, c), where
// nx * x + ny * y + c is the signed distance in framebuffer pixels, positive inside. The
// fragment multiplies by its coverage, clamp(d + 0.5, 0, 1) for the distance d to the
// nearest edge (ADR-0050 decision 6); a 3D item's polygon is drawn pushed out by a pixel so
// those fringe pixels rasterize. A 2D item's lines are never crossed, so its coverage is 1.
//
// `mode` says what a filtered sample holds. 0: an uploaded or rendered image, stored
// premultiplied linear, so the linear filter never mixes in the colour of a transparent
// texel. 1: an imported sRGB texture, straight alpha, decoded by the sampler and
// premultiplied after the filter: exact where alpha is 1, a fringe at partly transparent
// edges. 2: an imported texture of sRGB code values already multiplied by alpha (a
// browser page), read raw: divided by alpha, decoded, and multiplied by alpha again.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
    @location(2) mode: f32,
    @location(3) @interpolate(flat) e0: vec3<f32>,
    @location(4) @interpolate(flat) e1: vec3<f32>,
    @location(5) @interpolate(flat) e2: vec3<f32>,
    @location(6) @interpolate(flat) e3: vec3<f32>,
    @location(7) @interpolate(flat) e4: vec3<f32>,
}

@vertex
fn vs(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) opacity: f32,
    @location(3) mode: f32,
    @location(4) w: f32,
    @location(5) e0: vec3<f32>,
    @location(6) e1: vec3<f32>,
    @location(7) e2: vec3<f32>,
    @location(8) e3: vec3<f32>,
    @location(9) e4: vec3<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos * w, 0.0, w);
    out.uv = uv;
    out.opacity = opacity;
    out.mode = mode;
    out.e0 = e0;
    out.e1 = e1;
    out.e2 = e2;
    out.e3 = e3;
    out.e4 = e4;
    return out;
}

// IEC 61966-2-1 decode.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn edge(e: vec3<f32>, at: vec2<f32>) -> f32 {
    return dot(e.xy, at) + e.z;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    var rgb = c.rgb;
    if in.mode > 1.5 {
        // Premultiplied code values: a filtered texel keeps rgb <= a, up to rounding.
        let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
        rgb = srgb_to_linear(straight) * c.a;
    } else if in.mode > 0.5 {
        rgb = c.rgb * c.a;
    }
    let at = in.clip.xy;
    let d = min(
        min(min(edge(in.e0, at), edge(in.e1, at)), min(edge(in.e2, at), edge(in.e3, at))),
        edge(in.e4, at),
    );
    let coverage = clamp(d + 0.5, 0.0, 1.0);
    return vec4<f32>(rgb, c.a) * (in.opacity * coverage);
}
