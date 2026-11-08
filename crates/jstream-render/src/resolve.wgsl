// Fullscreen copy from the FP16 working target into the sRGB output. Un-premultiplies so
// the sRGB encode applies to straight colour. Alpha is stored as-is.

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    return vec4<f32>(p, 0.0, 1.0);
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var nearest: sampler;

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Sampled at the texel's centre with a nearest sampler: the same value a load
    // gives, and on a software renderer (llvmpipe) much less work (#1449).
    let c = textureSampleLevel(src, nearest, pos.xy / vec2<f32>(textureDimensions(src)), 0.0);
    return vec4<f32>(select(vec3<f32>(0.0), c.rgb / c.a, c.a > 0.0), c.a);
}
