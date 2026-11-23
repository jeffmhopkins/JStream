// Sharpen at a radius of 2 or more, second pass: `sharpen.wgsl`'s unsharp mask with the
// box blur read as a column of the row means `sharpen_rows.wgsl` made (aux, the input's
// size), in place of every tap.

struct Params {
    amount: f32,
    // Box half-width in texels, 2..=3.
    radius: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(floor(in.pos.xy));
    let own = unpremul(texel(px));
    let c = vec4<f32>(linear_to_srgb(own.rgb) * own.a, own.a);
    // Rows clamp to the image as taps do.
    let last = i32(frame.in_size.y) - 1;
    var sum = vec4<f32>(0.0);
    for (var dy = -p.radius; dy <= p.radius; dy++) {
        sum += textureLoad(aux_tex, vec2<i32>(px.x, clamp(px.y + dy, 0, last)), 0);
    }
    let s = c + p.amount * (c - sum / f32(2 * p.radius + 1));
    let a = saturate(s.a);
    if (a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let rgb = clamp(s.rgb, vec3<f32>(0.0), vec3<f32>(a)) / a;
    return premul(vec4<f32>(srgb_to_linear(rgb), a));
}
