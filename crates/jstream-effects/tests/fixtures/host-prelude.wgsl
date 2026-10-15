// A copy of the host prelude from the render filter harness (jstream-render,
// `src/filter/prelude.wgsl`, roadmap V0), for tests until this crate can read the
// harness's own. Effects validate against whatever prelude the host passes.

// Shared by every built-in video filter: a fullscreen triangle, the bindings, and colour
// helpers. A filter's own WGSL is appended after this and defines `fn fs(in: VsOut)`
// with its parameters at group 0, binding 4 (`var<uniform> p: Params;`).
//
// Images are premultiplied linear light (`Rgba16Float`). An imported input is straight
// sRGB-decoded; `src` and `aux` premultiply it, so filter code always sees premultiplied
// linear colour.

struct Frame {
    // Input size in pixels.
    in_size: vec2<f32>,
    // Output (target) size in pixels.
    out_size: vec2<f32>,
    // Auxiliary image size in pixels (the input's when no aux image is bound).
    aux_size: vec2<f32>,
    // Media-clock seconds, wrapped to a day so f32 keeps sub-millisecond precision.
    time: f32,
    // 1 when the input is straight alpha (an import), else 0.
    straight: f32,
    // 1 when the aux image is straight alpha, else 0.
    aux_straight: f32,
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

fn premul_if(c: vec4<f32>, straight: f32) -> vec4<f32> {
    return select(c, vec4<f32>(c.rgb * c.a, c.a), straight > 0.5);
}

// The input, bilinear, premultiplied linear. Outside 0..1 clamps to the edge.
fn src(uv: vec2<f32>) -> vec4<f32> {
    return premul_if(textureSampleLevel(input_tex, lin, uv, 0.0), frame.straight);
}

// One input texel, clamped to the image.
fn texel(px: vec2<i32>) -> vec4<f32> {
    let max_px = vec2<i32>(frame.in_size) - vec2<i32>(1);
    return premul_if(textureLoad(input_tex, clamp(px, vec2<i32>(0), max_px), 0), frame.straight);
}

// The auxiliary image (a mask, a second pass's input), bilinear, premultiplied linear.
fn aux(uv: vec2<f32>) -> vec4<f32> {
    return premul_if(textureSampleLevel(aux_tex, lin, uv, 0.0), frame.aux_straight);
}

fn unpremul(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(c.rgb / c.a, c.a);
}

fn premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow(max((c + 0.055) / 1.055, vec3<f32>(0.0)), vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// BT.709 luma of linear RGB.
fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}
