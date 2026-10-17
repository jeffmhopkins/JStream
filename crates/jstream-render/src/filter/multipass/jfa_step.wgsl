// Jump flood, one step: of the nine pixels `step` apart around this one (itself
// included), keep the seed nearest to this pixel. rg is the vector from this pixel to
// its seed, in pixels; alpha 1 when it has one. Vectors stay small whole numbers, which
// Rgba16Float holds exactly.

struct Params {
    step: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let size = vec2<i32>(frame.in_size);
    var best = vec2<f32>(0.0);
    var best_d = 3.4e38;
    var found = false;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let q = px + vec2<i32>(dx, dy) * p.step;
            if (any(q < vec2<i32>(0)) || any(q >= size)) {
                continue;
            }
            let v = textureLoad(input_tex, q, 0);
            if (v.a > 0.5) {
                let to_seed = v.xy + vec2<f32>(q - px);
                let d = dot(to_seed, to_seed);
                if (d < best_d) {
                    best_d = d;
                    best = to_seed;
                    found = true;
                }
            }
        }
    }
    return vec4<f32>(best, 0.0, select(0.0, 1.0, found));
}
