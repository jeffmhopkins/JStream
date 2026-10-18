// Shared by the three keys: they compare straight sRGB-encoded colour, as OBS does, and
// multiply the input's alpha by their mask.

// The input texel at `px` as straight sRGB-encoded colour.
fn encoded_at(px: vec2<i32>) -> vec4<f32> {
    let c = unpremul(texel(px));
    return vec4<f32>(linear_to_srgb(c.rgb), c.a);
}

// Straight sRGB-encoded `rgb` with alpha `a`, back to premultiplied linear light.
fn keyed(rgb: vec3<f32>, a: f32) -> vec4<f32> {
    return premul(vec4<f32>(srgb_to_linear(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))), a));
}

// The output pixel's input texel (the keys keep the size).
fn here(in: VsOut) -> vec2<i32> {
    return vec2<i32>(floor(in.pos.xy));
}
