// A model's surface (ADR-0170 decision 4), drawn with its layer by `fs_with_surface`,
// merged into its scene's surface buffer where it is nearer than what the buffer holds:
// one full-screen triangle, its depth `surface_depth` of the texel's 1 / w, as
// `fs_surface` writes it. The model's layers are the buffer's size, or smaller for a
// model drawn with fewer pixels than its canvas (ADR-0197): each of the buffer's texels
// then takes the layers' texel it falls in, unfiltered, so no normal or depth is a blend
// of two surfaces.

@group(0) @binding(0) var normal_layer: texture_2d<f32>;
@group(0) @binding(1) var brdf_layer: texture_2d<f32>;
@group(0) @binding(2) var reflected_layer: texture_2d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    // Across the buffer, 0 to 1, y down.
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    let x = f32(index & 1u) * 4.0 - 1.0;
    let y = f32(index >> 1u) * 4.0 - 1.0;
    var out: VsOut;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return out;
}

struct Merged {
    @location(0) normal: vec4<f32>,
    @location(1) brdf: vec4<f32>,
    @location(2) reflected: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs(in: VsOut) -> Merged {
    let size = vec2<i32>(textureDimensions(normal_layer));
    let texel = clamp(vec2<i32>(in.uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
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
