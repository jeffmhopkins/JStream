// Jump flood, the seeding and the first step in one pass: of the nine input pixels
// `step` apart around this one (itself included), the nearest that is a seed. Seeds are
// the input's pixels at least half opaque, or with `inside`, the ones less than half
// opaque. It writes what `jfa_step.wgsl` writes: rg the vector from this pixel to its
// seed, in pixels, b the seed's input alpha, and alpha 1 when it has one.

struct Params {
    inside: u32,
    step: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let size = vec2<i32>(frame.out_size);
    var best = vec2<f32>(0.0);
    var best_alpha = 0.0;
    var best_d = 3.4e38;
    var found = false;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let q = px + vec2<i32>(dx, dy) * p.step;
            // A pixel past the frame held no seed after the seeding pass.
            if (any(q < vec2<i32>(0)) || any(q >= size)) {
                continue;
            }
            let a = alpha_edge(q);
            if (select(a >= 0.5, a < 0.5, p.inside == 1u)) {
                let to_seed = vec2<f32>(q - px);
                let d = dot(to_seed, to_seed);
                if (d < best_d) {
                    best_d = d;
                    best = to_seed;
                    best_alpha = a;
                    found = true;
                }
            }
        }
    }
    return vec4<f32>(best, best_alpha, select(0.0, 1.0, found));
}
