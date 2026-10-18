// One direction of a separable gaussian: 2 * taps + 1 texels along (dir_x, dir_y),
// weighted by exp(-i^2 / 2 sigma^2) and normalised. `edge` is load_edge's.

struct Params {
    dir_x: i32,
    dir_y: i32,
    taps: i32,
    edge: u32,
    sigma: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let dir = vec2<i32>(p.dir_x, p.dir_y);
    let k = 1.0 / (2.0 * p.sigma * p.sigma);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -p.taps; i <= p.taps; i++) {
        let w = exp(-f32(i * i) * k);
        sum += w * load_edge(px + dir * i, p.edge);
        total += w;
    }
    return sum / total;
}
