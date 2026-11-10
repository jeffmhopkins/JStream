// Depth bloom's composite: the blurred glow (`aux`, half the input's size, read
// bilinear) times `strength`
// added over the input, its coverage its brightest channel, as bloom's is.

struct Params {
    strength: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let c = texel(px);
    let light = aux(in.pos.xy / (2.0 * frame.aux_size)).rgb * p.strength;
    let cover = min(max(light.r, max(light.g, light.b)), 1.0);
    return vec4<f32>(c.rgb + light, c.a + cover * (1.0 - c.a));
}
