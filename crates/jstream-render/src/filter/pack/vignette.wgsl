// Vignette: darkens (or tints) the picture towards the frame's edges, around a centre.
// The distance `d` is 0 at the centre and 1 at the half-diagonal: with `round` 0 it is
// measured in the frame's own UV, so the shape is an ellipse that follows the frame's
// aspect; with `round` 1 it is measured in pixels, so the shape is a circle. Inside
// `radius` the picture is untouched; over the next `softness` it fades in (a smoothstep)
// to `amount` of the colour, the strength already mapped to perceptually even steps on
// the CPU. `multiply` 1 multiplies by the colour (darkening); 0 mixes towards it (a
// tint). Coverage is kept: a transparent pixel stays transparent.

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    // The centre, in UV.
    center: vec2<f32>,
    amount: f32,
    radius: f32,
    softness: f32,
    round: u32,
    multiply: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    var d = 0.0;
    if (p.round == 1u) {
        let size = frame.out_size;
        d = length((in.uv - p.center) * size) / (0.5 * length(size));
    } else {
        d = length((in.uv - p.center) * 2.0) * 0.70710678;
    }
    let fade = smoothstep(p.radius, p.radius + max(p.softness, 1e-5), d);
    let k = fade * p.amount * p.color.a;
    if (p.multiply == 1u) {
        return vec4<f32>(c.rgb * mix(vec3<f32>(1.0), p.color.rgb, k), c.a);
    }
    return vec4<f32>(mix(c.rgb, p.color.rgb * c.a, k), c.a);
}
