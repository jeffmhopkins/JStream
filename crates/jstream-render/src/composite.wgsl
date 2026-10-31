// One textured polygon. Positions are clip-space (y up). `fs` outputs premultiplied
// linear colour, which the pipeline's blend state composites (ADR-0082): One,
// OneMinusSrcAlpha for `normal`. The `fs_<mode>` entry points read the backdrop, a copy of
// the target's texels under the item, and write the composited result in place.
//
// `w` is ADR-0050's homogeneous coordinate, 1 for a 2D item. The vertex writes
// clip = (pos * w, 0, w): the GPU divides by w, giving back the projected point, and
// interpolates uv / w and 1 / w linearly on screen, which is perspective-correct for a
// plane. With w = 1 it is the plain affine 2D path.
//
// `e0` to `e4` are the item's edge lines, the same at every vertex: (nx, ny, c), where
// nx * x + ny * y + c is the signed distance in framebuffer pixels, positive inside. The
// fragment multiplies by its coverage, clamp(d + 0.5, 0, 1) for the distance d to the
// nearest edge (ADR-0050 decision 6); a 3D item's polygon is drawn pushed out by a pixel so
// those fringe pixels rasterize. A 2D item's lines are never crossed, so its coverage is 1.
// `uv_bounds` (u_min, v_min, u_max, v_max) clamps a fringe pixel's texture coordinate to
// the content, so it never samples a cropped-away texel or one `fill` clips. A 2D item's
// bounds never clamp. The sample takes the clamped coordinate's own derivatives, which
// differ from the unclamped one's only at fringe pixels (coverage under a half) and their
// quad neighbours along the edge, since derivatives are taken across a 2x2 quad: about one
// row of pixels along a 3D item's edge, where they pick a finer level. `textureSampleGrad` with the unclamped derivatives would keep
// the fringe's level too, but lavapipe filters explicit gradients isotropically: scenario
// C measured 18 with it, as with no anisotropy, against 6.2 this way.
//
// `mode` says what a filtered sample holds. 0: an uploaded or rendered image, stored
// premultiplied linear, so the linear filter never mixes in the colour of a transparent
// texel. 1: an imported sRGB texture, straight alpha, decoded by the sampler and
// premultiplied after the filter: exact where alpha is 1, a fringe at partly transparent
// edges. 2: an imported texture of sRGB code values already multiplied by alpha (a
// browser page), read raw: divided by alpha, decoded, and multiplied by alpha again.
// 3: an opaque import (a captured frame), decoded by the sampler, its alpha read as 1.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
    @location(2) mode: f32,
    @location(3) @interpolate(flat) e0: vec3<f32>,
    @location(4) @interpolate(flat) e1: vec3<f32>,
    @location(5) @interpolate(flat) e2: vec3<f32>,
    @location(6) @interpolate(flat) e3: vec3<f32>,
    @location(7) @interpolate(flat) e4: vec3<f32>,
    @location(8) @interpolate(flat) uv_bounds: vec4<f32>,
}

@vertex
fn vs(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) opacity: f32,
    @location(3) mode: f32,
    @location(4) w: f32,
    @location(5) e0: vec3<f32>,
    @location(6) e1: vec3<f32>,
    @location(7) e2: vec3<f32>,
    @location(8) e3: vec3<f32>,
    @location(9) e4: vec3<f32>,
    @location(10) uv_bounds: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos * w, 0.0, w);
    out.uv = uv;
    out.opacity = opacity;
    out.mode = mode;
    out.e0 = e0;
    out.e1 = e1;
    out.e2 = e2;
    out.e3 = e3;
    out.e4 = e4;
    out.uv_bounds = uv_bounds;
    return out;
}

// IEC 61966-2-1 decode.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn edge(e: vec3<f32>, at: vec2<f32>) -> f32 {
    return dot(e.xy, at) + e.z;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// The item's premultiplied linear colour at this fragment, coverage and opacity applied.
fn source(in: VsOut) -> vec4<f32> {
    let uv = clamp(in.uv, in.uv_bounds.xy, in.uv_bounds.zw);
    let c = textureSample(tex, samp, uv);
    var rgb = c.rgb;
    var alpha = c.a;
    if in.mode > 2.5 {
        alpha = 1.0;
    } else if in.mode > 1.5 {
        // Premultiplied code values: a filtered texel keeps rgb <= a, up to rounding.
        let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
        rgb = srgb_to_linear(straight) * c.a;
    } else if in.mode > 0.5 {
        rgb = c.rgb * c.a;
    }
    let at = in.clip.xy;
    let d = min(
        min(min(edge(in.e0, at), edge(in.e1, at)), min(edge(in.e2, at), edge(in.e3, at))),
        edge(in.e4, at),
    );
    let coverage = clamp(d + 0.5, 0.0, 1.0);
    return vec4<f32>(rgb, alpha) * (in.opacity * coverage);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return source(in);
}

// The modes that read the backdrop. It is bound only to their pipelines.
@group(1) @binding(0) var backdrop: texture_2d<f32>;

// IEC 61966-2-1 encode, of a value in [0, 1].
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, c <= vec3<f32>(0.0031308));
}

fn unpremultiply(c: vec4<f32>) -> vec3<f32> {
    return select(vec3<f32>(0.0), c.rgb / c.a, c.a > 0.0);
}

const DARKEN: u32 = 0u;
const LIGHTEN: u32 = 1u;
const OVERLAY: u32 = 2u;
const DIFFERENCE: u32 = 3u;
const EXCLUSION: u32 = 4u;

// The mixed colour B(cs, cb) of the W3C compositing spec, straight. Darken and lighten
// take linear light; overlay, difference and exclusion take sRGB-encoded values in [0, 1].
fn mix_colour(mode: u32, cs: vec3<f32>, cb: vec3<f32>) -> vec3<f32> {
    switch mode {
        case DARKEN: {
            return min(cs, cb);
        }
        case LIGHTEN: {
            return max(cs, cb);
        }
        case OVERLAY: {
            // Hard light with the layers swapped: multiply below a backdrop of 0.5,
            // screen above it.
            let multiply = 2.0 * cs * cb;
            let screen = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - cs) * (vec3<f32>(1.0) - cb);
            return select(screen, multiply, cb <= vec3<f32>(0.5));
        }
        case DIFFERENCE: {
            return abs(cs - cb);
        }
        default: {
            return cs + cb - 2.0 * cs * cb;
        }
    }
}

// The W3C source-over composite of a separable mode, premultiplied:
// s·(1 − αb) + b·(1 − αs) + αs·αb·B(cs, cb), alpha αs + αb − αs·αb.
fn blended(in: VsOut, mode: u32) -> vec4<f32> {
    let s = source(in);
    let b = textureLoad(backdrop, vec2<i32>(in.clip.xy), 0);
    var cs = unpremultiply(s);
    var cb = unpremultiply(b);
    var mixed: vec3<f32>;
    if mode >= OVERLAY {
        // These blend in sRGB-encoded space, where their midpoint is perceptual, as in
        // other editors; the encoding needs [0, 1], so brighter-than-white is clamped.
        cs = linear_to_srgb(clamp(cs, vec3<f32>(0.0), vec3<f32>(1.0)));
        cb = linear_to_srgb(clamp(cb, vec3<f32>(0.0), vec3<f32>(1.0)));
        mixed = srgb_to_linear(mix_colour(mode, cs, cb));
    } else {
        mixed = mix_colour(mode, cs, cb);
    }
    let rgb = s.rgb * (1.0 - b.a) + b.rgb * (1.0 - s.a) + mixed * (s.a * b.a);
    return vec4<f32>(rgb, s.a + b.a - s.a * b.a);
}

@fragment
fn fs_darken(in: VsOut) -> @location(0) vec4<f32> {
    return blended(in, DARKEN);
}

@fragment
fn fs_lighten(in: VsOut) -> @location(0) vec4<f32> {
    return blended(in, LIGHTEN);
}

@fragment
fn fs_overlay(in: VsOut) -> @location(0) vec4<f32> {
    return blended(in, OVERLAY);
}

@fragment
fn fs_difference(in: VsOut) -> @location(0) vec4<f32> {
    return blended(in, DIFFERENCE);
}

@fragment
fn fs_exclusion(in: VsOut) -> @location(0) vec4<f32> {
    return blended(in, EXCLUSION);
}
