// One direction of a separable gaussian: 2 * taps + 1 texels along (dir_x, dir_y),
// weighted by exp(-i^2 / 2 sigma^2) and normalised. The weights are made once on the
// CPU (`gauss.rs`), so no pixel pays for an `exp` per tap. `edge` is load_edge's.
//
// A premultiplied linear input is read in pairs: one bilinear sample between texels
// `i` and `i + 1`, placed by their weights, gives their weighted sum, so a pass costs
// about half the reads. A software rasterizer pays for every read (#1449). It holds
// wherever the clamped sampler reads what load_edge does: always for a clamped edge,
// and for a transparent one where the whole span lies inside the input. An input of
// any other alpha mode, whose texels are converted before they are weighted, is read
// a texel at a time.

struct Params {
    dir_x: i32,
    dir_y: i32,
    taps: i32,
    edge: u32,
    // Weight `i`, for `i` in 0..=taps: `weights[i / 4][i % 4]`.
    weights: array<vec4<f32>, 3>,
    // Pair `k` is texels `2k + 1` and `2k + 2` either side: `(weight, offset)` in
    // `pairs[k / 2].xy` or `.zw`. `pair_count` pairs cover the taps.
    pairs: array<vec4<f32>, 2>,
    pair_count: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn weight(i: i32) -> f32 {
    return p.weights[i / 4][i % 4];
}

fn pair(k: i32) -> vec2<f32> {
    let four = p.pairs[k / 2];
    return select(four.xy, four.zw, k % 2 == 1);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let dir = vec2<i32>(p.dir_x, p.dir_y);
    let size = vec2<i32>(frame.in_size);
    let first = px - dir * p.taps;
    let last = px + dir * p.taps;
    let inside = all(first >= vec2<i32>(0)) && all(last < size);
    if (frame.alpha == 0.0 && (p.edge == 0u || inside)) {
        let centre = vec2<f32>(px) + vec2<f32>(0.5);
        let step = vec2<f32>(dir) / frame.in_size;
        let at = centre / frame.in_size;
        var sum = weight(0) * textureSampleLevel(input_tex, lin, at, 0.0);
        for (var k = 0; k < p.pair_count; k++) {
            let wo = pair(k);
            let off = step * wo.y;
            sum += wo.x * (textureSampleLevel(input_tex, lin, at + off, 0.0)
                + textureSampleLevel(input_tex, lin, at - off, 0.0));
        }
        return sum;
    }
    var sum = weight(0) * load_edge(px, p.edge);
    for (var i = 1; i <= p.taps; i++) {
        sum += weight(i) * (load_edge(px + dir * i, p.edge) + load_edge(px - dir * i, p.edge));
    }
    return sum;
}
