// Box downsample: each output texel is the mean of a `block` x `block` square of the
// frame, sampled on a `samples` x `samples` grid (every texel when samples == block).
// The input sits at `origin` in the frame; `edge` is load_edge's.

struct Params {
    origin_x: i32,
    origin_y: i32,
    block: i32,
    samples: i32,
    frame_w: i32,
    frame_h: i32,
    edge: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let origin = vec2<i32>(p.origin_x, p.origin_y);
    let lo = out_px(in.pos) * p.block;
    let hi = min(lo + vec2<i32>(p.block), vec2<i32>(p.frame_w, p.frame_h));
    let len = max(hi - lo, vec2<i32>(1));
    let n = p.samples;
    var sum = vec4<f32>(0.0);
    for (var j = 0; j < n; j++) {
        for (var i = 0; i < n; i++) {
            // floor((k + 0.5) * len / n), the centre of the k-th of n equal strata.
            let at = lo + ((vec2<i32>(i, j) * 2 + 1) * len) / (2 * n);
            sum += load_edge(at - origin, p.edge);
        }
    }
    return sum / f32(n * n);
}
