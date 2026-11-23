// The nearest seed along each pixel's own row, the first of the two passes that find
// every pixel's nearest seed exactly when the reach is short. Seeds are the input's
// pixels at least half opaque, or with `inside`, the ones less than half opaque. rg is
// the vector to the nearest seed within `reach` pixels left or right (y is 0), b that
// seed's input alpha, and alpha 1 when there is one. The scan runs outward from the
// pixel, so the first seed it meets is the nearest and ends it.

struct Params {
    inside: u32,
    reach: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

// The input's alpha at `q` when it is a seed, and -1 when it isn't. Nothing past the
// frame is one.
fn seed(q: vec2<i32>) -> f32 {
    if (any(q < vec2<i32>(0)) || any(q >= vec2<i32>(frame.out_size))) {
        return -1.0;
    }
    let a = alpha_edge(q);
    return select(-1.0, a, select(a >= 0.5, a < 0.5, p.inside == 1u));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let own = seed(px);
    if (own >= 0.0) {
        return vec4<f32>(0.0, 0.0, own, 1.0);
    }
    for (var k = 1; k <= p.reach; k++) {
        let left = seed(px - vec2<i32>(k, 0));
        if (left >= 0.0) {
            return vec4<f32>(f32(-k), 0.0, left, 1.0);
        }
        let right = seed(px + vec2<i32>(k, 0));
        if (right >= 0.0) {
            return vec4<f32>(f32(k), 0.0, right, 1.0);
        }
    }
    return vec4<f32>(0.0);
}
