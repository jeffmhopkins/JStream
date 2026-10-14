// One textured quad. Positions are clip-space (y up). The fragment outputs premultiplied
// linear colour; the FP16 target blends with One, OneMinusSrcAlpha.
//
// `straight` is 0 for an uploaded image, which is stored premultiplied, so the linear
// filter never mixes in the colour of a transparent texel. It is 1 for an imported sRGB
// texture, which is straight alpha and is premultiplied after the filter: exact where
// alpha is 1, a fringe at partly transparent edges.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
    @location(2) straight: f32,
}

@vertex
fn vs(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) opacity: f32,
    @location(3) straight: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.opacity = opacity;
    out.straight = straight;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let rgb_scale = select(1.0, c.a, in.straight > 0.5);
    return vec4<f32>(c.rgb * rgb_scale, c.a) * in.opacity;
}
