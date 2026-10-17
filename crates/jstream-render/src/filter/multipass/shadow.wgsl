// The input over a tinted copy of its blurred alpha: a drop shadow or a glow. aux is
// the blurred shape, covering a frame of `shape_size` pixels whose top-left sits at
// `shape_origin` output pixels, `scale` pixels per aux texel.

struct Params {
    // Premultiplied linear.
    color: vec4<f32>,
    inset_x: i32,
    inset_y: i32,
    shape_origin: vec2<f32>,
    shape_size: vec2<f32>,
    scale: f32,
    // The blurred alpha's multiplier, clamped to 1 after.
    strength: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let top = placed(out_px(in.pos), vec2<i32>(p.inset_x, p.inset_y));
    let f = in.pos.xy - p.shape_origin;
    var a = 0.0;
    if (all(f >= vec2<f32>(0.0)) && all(f < p.shape_size)) {
        a = min(aux(f / (p.scale * frame.aux_size)).a * p.strength, 1.0);
    }
    let under = p.color * a;
    return top + under * (1.0 - top.a);
}
