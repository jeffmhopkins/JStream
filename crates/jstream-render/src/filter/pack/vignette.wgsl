// Vignette: darkens (or tints) the picture towards the frame's edges. The distance is
// measured in the frame's own UV, so the shape is an ellipse that follows the frame's
// aspect: 0 at the centre, 1 at the corners. Inside `radius` the picture is untouched;
// over the next `softness` it fades in to `strength` of the colour. Coverage is kept:
// a transparent pixel stays transparent.

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    strength: f32,
    radius: f32,
    softness: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    let q = (in.uv - vec2<f32>(0.5)) * 2.0;
    let d = length(q) * 0.70710678;
    let fade = smoothstep(p.radius, p.radius + max(p.softness, 1e-5), d);
    let k = fade * p.strength * p.color.a;
    return vec4<f32>(mix(c.rgb, p.color.rgb * c.a, k), c.a);
}
