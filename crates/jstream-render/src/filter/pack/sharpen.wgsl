// Sharpen: an unsharp mask, `c + amount * (c - box blur)`, on premultiplied
// sRGB-encoded colour so the halo is even across the tones and transparent texels
// add nothing.

struct Params {
    amount: f32,
    // Box half-width in texels, 1..=3.
    radius: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn tap(px: vec2<i32>) -> vec4<f32> {
    let c = unpremul(texel(px));
    return vec4<f32>(linear_to_srgb(c.rgb) * c.a, c.a);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(floor(in.pos.xy));
    let c = tap(px);
    var sum = vec4<f32>(0.0);
    for (var dy = -p.radius; dy <= p.radius; dy++) {
        for (var dx = -p.radius; dx <= p.radius; dx++) {
            sum += tap(px + vec2<i32>(dx, dy));
        }
    }
    let side = f32(2 * p.radius + 1);
    let s = c + p.amount * (c - sum / (side * side));
    let a = saturate(s.a);
    if (a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let rgb = clamp(s.rgb, vec3<f32>(0.0), vec3<f32>(a)) / a;
    return premul(vec4<f32>(srgb_to_linear(rgb), a));
}
