// Sharpen at a radius of 2 or more, first pass: the mean of each pixel's row of
// `2 radius + 1` taps, as premultiplied sRGB-encoded colour. The box blur is a mean of
// rows of means, so `sharpen_columns.wgsl` then reads one of these per row in place of a row
// of taps, each of which it would have to encode.

struct Params {
    amount: f32,
    // Box half-width in texels, 2..=3.
    radius: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(floor(in.pos.xy));
    var sum = vec4<f32>(0.0);
    for (var dx = -p.radius; dx <= p.radius; dx++) {
        let c = unpremul(texel(px + vec2<i32>(dx, 0)));
        sum += vec4<f32>(linear_to_srgb(c.rgb) * c.a, c.a);
    }
    return sum / f32(2 * p.radius + 1);
}
