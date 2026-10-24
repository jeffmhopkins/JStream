// One textured quad. Positions are clip-space (y up). The fragment outputs premultiplied
// linear colour; the FP16 target blends with One, OneMinusSrcAlpha.
//
// `mode` says what a filtered sample holds. 0: an uploaded or rendered image, stored
// premultiplied linear, so the linear filter never mixes in the colour of a transparent
// texel. 1: an imported sRGB texture, straight alpha, decoded by the sampler and
// premultiplied after the filter: exact where alpha is 1, a fringe at partly transparent
// edges. 2: an imported texture of sRGB code values already multiplied by alpha (a
// browser page), read raw: divided by alpha, decoded, and multiplied by alpha again.
// 3: an opaque import (a captured frame), decoded by the sampler, its alpha read as 1.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
    @location(2) mode: f32,
}

@vertex
fn vs(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) opacity: f32,
    @location(3) mode: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.opacity = opacity;
    out.mode = mode;
    return out;
}

// IEC 61966-2-1 decode.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    var rgb = c.rgb;
    if in.mode > 2.5 {
        return vec4<f32>(c.rgb, 1.0) * in.opacity;
    }
    if in.mode > 1.5 {
        // Premultiplied code values: a filtered texel keeps rgb <= a, up to rounding.
        let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
        rgb = srgb_to_linear(straight) * c.a;
    } else if in.mode > 0.5 {
        rgb = c.rgb * c.a;
    }
    return vec4<f32>(rgb, c.a) * in.opacity;
}
