// Luma key (OBS's v2): keeps luma between min and max, each edge softened by its
// smoothing width. A zero width is a hard edge that keeps the limit itself, with a
// little slack so white (luma 1 give or take rounding) survives the default maximum.

const SLACK: f32 = 1e-4;

struct Params {
    luma_min: f32,
    min_smooth: f32,
    luma_max: f32,
    max_smooth: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = encoded_at(here(in));
    let l = luma(c.rgb);
    var low = step(p.luma_min - SLACK, l);
    if (p.min_smooth > 0.0) {
        low = smoothstep(p.luma_min, p.luma_min + p.min_smooth, l);
    }
    var high = step(l, p.luma_max + SLACK);
    if (p.max_smooth > 0.0) {
        high = 1.0 - smoothstep(p.luma_max - p.max_smooth, p.luma_max, l);
    }
    return keyed(c.rgb, c.a * low * high);
}
