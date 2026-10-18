// image_mask and scene_mask: scale the input by a coverage read from the aux image,
// stretched over the frame.

struct Params {
    // 0 pass through (no mask), 1 alpha, 2 luma, 3 clip (alpha at least one half).
    mode: u32,
    // 1 inverts the coverage.
    invert: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    if (p.mode == 0u) {
        return c;
    }
    let m = aux(in.uv);
    var k = m.a;
    if (p.mode == 2u) {
        // BT.709 luma of the sRGB-encoded colour, times alpha: transparent is black.
        let e = clamp(linear_to_srgb(unpremul(m).rgb), vec3<f32>(0.0), vec3<f32>(1.0));
        k = dot(e, vec3<f32>(0.2126, 0.7152, 0.0722)) * m.a;
    } else if (p.mode == 3u) {
        k = select(0.0, 1.0, m.a >= 0.5);
    }
    if (p.invert == 1u) {
        k = 1.0 - k;
    }
    return c * clamp(k, 0.0, 1.0);
}
