// One textured quad. Positions are clip-space (y up). The fragment outputs premultiplied
// linear colour; the FP16 target blends with One, OneMinusSrcAlpha.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
}

@vertex
fn vs(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) opacity: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.opacity = opacity;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let a = c.a * in.opacity;
    return vec4<f32>(c.rgb * a, a);
}
