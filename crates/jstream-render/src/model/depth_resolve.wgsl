// A model's view depth (ADR-0143 decision 4) read from the depth its layer's pass
// tested, in place of drawing the model again for it: with a camera the clip depth is
// `NEAR_W / w`, so the view depth, `1 / w`, is it over `NEAR_W`. Multisampled, a texel
// takes the mean of the samples the model covers, which on a plane is its centre's
// depth, as the single-sampled depth draw and the shading (once per texel, at its
// centre) see it, where the model covers at least three quarters of them. Where
// nothing was drawn the depth stays cleared at 0: infinitely far.

override NEAR_W: f32;

@group(0) @binding(0) var single: texture_depth_2d;
@group(0) @binding(1) var multi: texture_depth_multisampled_2d;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_single(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let depth = textureLoad(single, vec2<i32>(at.xy), 0);
    return vec4<f32>(depth / NEAR_W, 0.0, 0.0, 1.0);
}

@fragment
fn fs_multi(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(at.xy);
    let count = textureNumSamples(multi);
    var sum = 0.0;
    var covered = 0u;
    for (var s = 0u; s < count; s++) {
        let d = textureLoad(multi, texel, i32(s));
        if d > 0.0 {
            sum += d;
            covered += 1u;
        }
    }
    // An edge texel the model covers less than three quarters of holds no depth, as
    // the centre a single-sampled draw tests would mostly miss it: what is behind shows
    // through the layer's soft edge there, and a depth read between texels doesn't bleed
    // the model's past its edge.
    if covered * 4u < count * 3u {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sum / f32(covered) / NEAR_W, 0.0, 0.0, 1.0);
}
