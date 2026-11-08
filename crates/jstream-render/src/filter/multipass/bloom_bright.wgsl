// Bloom's bright pass: the input's light above a threshold. The threshold is on the
// sRGB-encoded luma of the straight colour, so it reads as the picture looks, and fades
// in over a soft knee instead of switching on hard. The result is premultiplied, so a
// transparent pixel gives none.

struct Params {
    threshold: f32,
    knee: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = texel(out_px(in.pos));
    let s = unpremul(c);
    let l = linear_to_srgb(vec3<f32>(luma(s.rgb))).x;
    let w = smoothstep(p.threshold - p.knee, p.threshold + p.knee, l);
    return c * w;
}
