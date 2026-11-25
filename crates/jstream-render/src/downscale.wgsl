// A frame the compositor drew, scaled down to a smaller size (`downscale.rs`): each target
// texel is the exact box average of the source texels it covers, each weighted by how
// much of it is covered, in linear light and premultiplied, so a thin line or small text
// keeps its weight instead of flickering in and out as a single tap would.
//
// The source and the target are both the compositor's straight-alpha sRGB output format:
// `textureLoad` decodes a texel to linear, and the target's view encodes on write.

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

// `dst.xy` is the target's size in texels.
struct Params {
    dst: vec4<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(src, 0));
    let s = size / params.dst.xy;
    let p = floor(in.clip.xy);
    let a = p * s;
    let b = (p + vec2<f32>(1.0)) * s;
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
                let c = textureLoad(src, clamp(vec2<i32>(x, y), vec2<i32>(0), last), 0);
                sum = sum + vec4<f32>(c.rgb * c.a, c.a) * w;
                total = total + w;
            }
        }
    }
    let mean = sum / max(total, 1e-12);
    // Back to straight alpha, as the compositor's output holds it.
    let rgb = select(vec3<f32>(0.0), min(mean.rgb / mean.a, vec3<f32>(1.0)), mean.a > 0.0);
    return vec4<f32>(rgb, mean.a);
}
