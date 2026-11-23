// A model's surface (ADR-0170 decision 4), drawn with its layer by `fs_with_surface`,
// merged into its scene's surface buffer where it is nearer than what the buffer holds:
// one full-screen triangle, its depth `surface_depth` of the texel's 1 / w, as
// `fs_surface` writes it. The model's layers are the buffer's size.

@group(0) @binding(0) var normal_layer: texture_2d<f32>;
@group(0) @binding(1) var brdf_layer: texture_2d<f32>;
@group(0) @binding(2) var reflected_layer: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(index & 1u) * 4.0 - 1.0;
    let y = f32(index >> 1u) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

struct Merged {
    @location(0) normal: vec4<f32>,
    @location(1) brdf: vec4<f32>,
    @location(2) reflected: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> Merged {
    let texel = vec2<i32>(position.xy);
    let normal = textureLoad(normal_layer, texel, 0);
    if normal.w <= 0.0 {
        discard;
    }
    var out: Merged;
    out.normal = normal;
    out.brdf = textureLoad(brdf_layer, texel, 0);
    out.reflected = textureLoad(reflected_layer, texel, 0);
    out.depth = normal.w / (1.0 + normal.w);
    return out;
}
