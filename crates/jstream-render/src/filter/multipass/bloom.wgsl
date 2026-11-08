// Bloom's composite: the blurred bright pass (aux, `scale` pixels per texel), tinted and
// scaled by intensity, added over the input. The added light's coverage is its
// brightest channel, so where it spills past the input's shape it shows as light, not
// as a dark fringe.

struct Params {
    // Linear RGB times intensity; w unused.
    tint: vec4<f32>,
    scale: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = texel(out_px(in.pos));
    let b = aux(in.pos.xy / (p.scale * frame.aux_size));
    let light = b.rgb * p.tint.rgb;
    let cover = min(max(light.r, max(light.g, light.b)), 1.0);
    return vec4<f32>(c.rgb + light, c.a + cover * (1.0 - c.a));
}
