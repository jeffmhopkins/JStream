// A multisampled model's surface (ADR-0242 decision 3), resolved into its three layers:
// at each texel, every target takes the one sample whose 1 / w is the largest, the
// nearest surface the texel's samples saw, so no normal, BRDF or depth is a blend of two
// surfaces or of a surface and nothing, as `surface_merge.wgsl` needs. A texel none of
// whose samples saw a surface is zero, as a single-sampled one would be.

@group(0) @binding(0) var normal_ms: texture_multisampled_2d<f32>;
@group(0) @binding(1) var brdf_ms: texture_multisampled_2d<f32>;
@group(0) @binding(2) var reflected_ms: texture_multisampled_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(index & 1u) * 4.0 - 1.0;
    let y = f32(index >> 1u) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

struct Resolved {
    @location(0) normal: vec4<f32>,
    @location(1) brdf: vec4<f32>,
    @location(2) reflected: vec4<f32>,
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> Resolved {
    let px = vec2<i32>(position.xy);
    let count = i32(textureNumSamples(normal_ms));
    var best = 0;
    var nearest = textureLoad(normal_ms, px, 0).w;
    for (var s = 1; s < count; s++) {
        let w = textureLoad(normal_ms, px, s).w;
        if w > nearest {
            nearest = w;
            best = s;
        }
    }
    var out: Resolved;
    out.normal = textureLoad(normal_ms, px, best);
    out.brdf = textureLoad(brdf_ms, px, best);
    out.reflected = textureLoad(reflected_ms, px, best);
    return out;
}
