// Colour key (OBS's v2): RGB distance from the key colour, then a linear ramp.

struct Params {
    // The key colour, sRGB-encoded; `w` unused.
    key: vec4<f32>,
    similarity: f32,
    smoothness: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = encoded_at(here(in));
    let mask = saturate((distance(c.rgb, p.key.rgb) - p.similarity) / p.smoothness);
    return keyed(c.rgb, c.a * mask);
}
