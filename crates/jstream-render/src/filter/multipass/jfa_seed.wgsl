// Jump flood, first pass: every seed pixel holds a zero vector to itself (alpha 1);
// every other pixel holds nothing (alpha 0). Seeds are the input's pixels at least half
// opaque, or with `inside`, the ones less than half opaque. The input sits at `inset`.

struct Params {
    inset_x: i32,
    inset_y: i32,
    inside: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let a = placed(out_px(in.pos), vec2<i32>(p.inset_x, p.inset_y)).a;
    let seed = select(a >= 0.5, a < 0.5, p.inside == 1u);
    return vec4<f32>(0.0, 0.0, 0.0, select(0.0, 1.0, seed));
}
