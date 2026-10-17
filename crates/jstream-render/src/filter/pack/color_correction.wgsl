// Colour correction: gamma, then one affine colour matrix (contrast, brightness,
// saturation, hue, multiply), then opacity, on straight sRGB-encoded colour.

struct Params {
    // The matrix's rows; `w` is the row's offset.
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
    // The exponent each channel is raised to.
    gamma: f32,
    // Multiplies alpha.
    opacity: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = unpremul(src(in.uv));
    let e = pow(clamp(linear_to_srgb(c.rgb), vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(p.gamma));
    let v = vec4<f32>(e, 1.0);
    let o = clamp(vec3<f32>(dot(p.r0, v), dot(p.r1, v), dot(p.r2, v)), vec3<f32>(0.0), vec3<f32>(1.0));
    return premul(vec4<f32>(srgb_to_linear(o), c.a * p.opacity));
}
