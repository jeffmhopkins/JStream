// The mip chain of a source placed in 3D or drawn small (ADR-0050 decision 5, #1217),
// premultiplied linear in `Rgba16Float` like an upload.
//
// Every level is the exact box average of the texels its own texels cover. A target
// texel `p` of a `dst`-sized level covers `[origin + p * s, origin + (p + 1) * s)` of
// the input, `s = span / dst`, and its value is the mean of the input texels there,
// each weighted by how much of it is covered: read with `textureLoad`, never filtered,
// so nothing past the span is read. That is one texel at `s = 1` (level 0 of a chain
// at the region's own size), 2×2 at `s = 2`, and up to 3 per axis where a level's size
// is odd (a 135-texel level has a 67-texel next one, `s` just over 2), which one linear
// tap at the texel centre would only half cover, leaving coarse levels aliased.
//
// Level 0 reads the source through the region `uniforms` holds: the item's crop, in the
// source's texels, and the level's size. `fs_straight`, `fs_srgb` and `fs_opaque`
// convert each import texel to premultiplied linear before it is averaged, as the
// composite shader's modes 1 to 3 do. Each further level reads the whole level above
// (`fs_next`), whose size it halves, rounded down, at least 1, as wgpu sizes levels.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
}

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
    out.clip = vec4<f32>(p, 0.0, 1.0);
    return out;
}

// Level 0's region: `origin_span` is `[x, y, width, height]` in the source's texels and
// `dst.xy` the level's size.
struct Region {
    origin_span: vec4<f32>,
    dst: vec4<f32>,
}

@group(1) @binding(0) var<uniform> region: Region;

// IEC 61966-2-1 decode.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

const PREMULTIPLIED: u32 = 0u;
const STRAIGHT: u32 = 1u;
const SRGB: u32 = 2u;
const OPAQUE: u32 = 3u;

// One source texel as premultiplied linear.
fn convert(c: vec4<f32>, mode: u32) -> vec4<f32> {
    switch mode {
        case 1u: {
            return vec4<f32>(c.rgb * c.a, c.a);
        }
        case 2u: {
            let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
            return vec4<f32>(srgb_to_linear(straight) * c.a, c.a);
        }
        case 3u: {
            return vec4<f32>(c.rgb, 1.0);
        }
        default: {
            return c;
        }
    }
}

// The box average of `span` of the input from `origin`, over target texel `p` of `dst`.
fn box(p: vec2<f32>, origin: vec2<f32>, span: vec2<f32>, dst: vec2<f32>, mode: u32) -> vec4<f32> {
    let s = span / dst;
    let a = origin + p * s;
    let b = origin + (p + vec2<f32>(1.0)) * s;
    let last = vec2<i32>(textureDimensions(src, 0)) - vec2<i32>(1);
    let first = vec2<i32>(floor(a));
    let end = vec2<i32>(ceil(b));
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y < end.y; y = y + 1) {
        let wy = min(b.y, f32(y + 1)) - max(a.y, f32(y));
        for (var x = first.x; x < end.x; x = x + 1) {
            let wx = min(b.x, f32(x + 1)) - max(a.x, f32(x));
            let w = wx * wy;
            if w > 0.0 {
                let at = clamp(vec2<i32>(x, y), vec2<i32>(0), last);
                sum = sum + convert(textureLoad(src, at, 0), mode) * w;
                total = total + w;
            }
        }
    }
    return sum / max(total, 1e-12);
}

fn first(in: VsOut, mode: u32) -> vec4<f32> {
    return box(floor(in.clip.xy), region.origin_span.xy, region.origin_span.zw, region.dst.xy, mode);
}

@fragment
fn fs_premultiplied(in: VsOut) -> @location(0) vec4<f32> {
    return first(in, PREMULTIPLIED);
}

@fragment
fn fs_straight(in: VsOut) -> @location(0) vec4<f32> {
    return first(in, STRAIGHT);
}

@fragment
fn fs_srgb(in: VsOut) -> @location(0) vec4<f32> {
    return first(in, SRGB);
}

@fragment
fn fs_opaque(in: VsOut) -> @location(0) vec4<f32> {
    return first(in, OPAQUE);
}

@fragment
fn fs_next(in: VsOut) -> @location(0) vec4<f32> {
    let span = vec2<f32>(textureDimensions(src, 0));
    let dst = max(floor(span * 0.5), vec2<f32>(1.0));
    return box(floor(in.clip.xy), vec2<f32>(0.0), span, dst, PREMULTIPLIED);
}
