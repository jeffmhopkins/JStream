// Shared by every built-in video filter: a fullscreen triangle, the bindings, and colour
// helpers (`colour.wgsl`, which the host appends to this file). A filter's own WGSL is
// appended after both and defines `fn fs(in: VsOut)` with its parameters at group 0,
// binding 4 (`var<uniform> p: Params;`).
//
// Images are premultiplied linear light (`Rgba16Float`). An imported input is straight
// sRGB-decoded, or (a browser page) sRGB code values already multiplied by alpha; `src`,
// `texel` and `aux` convert both, so filter code always sees premultiplied linear colour.

struct Frame {
    // Input size in pixels.
    in_size: vec2<f32>,
    // Output (target) size in pixels.
    out_size: vec2<f32>,
    // Auxiliary image size in pixels (the input's when no aux image is bound).
    aux_size: vec2<f32>,
    // Media-clock seconds, wrapped to a day so f32 keeps better than 10 ms precision.
    // It jumps back to 0 once a day: a kind that moves with time computes its phase
    // on the CPU in f64 and passes it in its Params instead.
    time: f32,
    // How the input stores alpha: 0 premultiplied linear, 1 straight (an import),
    // 2 premultiplied sRGB code values (an imported browser page), 3 opaque (an
    // imported capture, its alpha read as 1).
    alpha: f32,
    // The same for the aux image.
    aux_alpha: f32,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Output UV: (0, 0) top-left, (1, 1) bottom-right.
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var lin: sampler;
@group(0) @binding(2) var<uniform> frame: Frame;
@group(0) @binding(3) var aux_tex: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

// The input, bilinear, premultiplied linear. Outside 0..1 clamps to the edge.
fn src(uv: vec2<f32>) -> vec4<f32> {
    return to_premul_linear(textureSampleLevel(input_tex, lin, uv, 0.0), frame.alpha);
}

// One input texel, clamped to the image.
fn texel(px: vec2<i32>) -> vec4<f32> {
    let max_px = vec2<i32>(frame.in_size) - vec2<i32>(1);
    return to_premul_linear(textureLoad(input_tex, clamp(px, vec2<i32>(0), max_px), 0), frame.alpha);
}

// The auxiliary image (a mask, a second pass's input), bilinear, premultiplied linear.
fn aux(uv: vec2<f32>) -> vec4<f32> {
    return to_premul_linear(textureSampleLevel(aux_tex, lin, uv, 0.0), frame.aux_alpha);
}
