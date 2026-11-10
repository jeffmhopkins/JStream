// One direction of depth bloom's blur (ADR-0151 decision 8): a Gaussian of `sigma` over
// `count` taps `step` apart on each side along `dir`, normalised by its full weight so
// a blocked tap takes its share away instead of moving it to others. With a view depth
// (`focal` above 0, `aux` holding 1 / w), a tap's light reaches the pixel less where
// the pixel is nearer than the tap by more than `depth`, and none by twice that: a glow
// stops at something in front of it. It runs at half the picture's size, so a texel's
// distance is the view depth's at twice its position. Where either has nothing drawn, it spreads as
// plain bloom's does. Light from past the frame is none.

struct Params {
    dir: vec2<f32>,
    sigma: f32,
    step: f32,
    count: u32,
    focal: f32,
    depth: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn distance_at(px: vec2<i32>) -> f32 {
    let full = min(px * 2, vec2<i32>(frame.aux_size) - 1);
    let inverse_w = textureLoad(aux_tex, full, 0).r;
    if inverse_w <= 0.0 {
        return -1.0;
    }
    return p.focal / inverse_w;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let size = vec2<i32>(frame.in_size);
    var here = -1.0;
    if p.focal > 0.0 {
        here = distance_at(px);
    }
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    let n = i32(p.count);
    for (var i = -n; i <= n; i++) {
        let offset = f32(i) * p.step;
        let w = exp(-offset * offset / (2.0 * p.sigma * p.sigma));
        total += w;
        let at = px + vec2<i32>(round(p.dir * offset));
        if any(at < vec2<i32>(0)) || any(at >= size) {
            continue;
        }
        var k = w;
        if here > 0.0 {
            let there = distance_at(at);
            if there > 0.0 {
                k = k * (1.0 - smoothstep(p.depth, 2.0 * p.depth, there - here));
            }
        }
        sum += texel(at) * k;
    }
    return sum / max(total, 1e-6);
}
