// Jump flood, first pass: every seed pixel holds a zero vector to itself (alpha 1);
// every other pixel holds nothing (alpha 0). Seeds are the input's pixels at least half
// opaque, or with `inside`, the ones less than half opaque.

struct Params {
    inside: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let a = load_edge(out_px(in.pos), 1u).a;
    let seed = select(a >= 0.5, a < 0.5, p.inside == 1u);
    return vec4<f32>(0.0, 0.0, 0.0, select(0.0, 1.0, seed));
}
