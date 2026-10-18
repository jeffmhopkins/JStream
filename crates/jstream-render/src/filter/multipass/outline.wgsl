// The stroke from the jump flood's distances (aux, the output's size). Outside: the
// stroke covers pixels up to `width` from the shape and is drawn under the input.
// Inside: it covers the shape's pixels up to `width` from its edge (or the frame's) and
// is drawn over the input, clipped to its alpha. Coverage is antialiased over one pixel.

struct Params {
    // Premultiplied linear.
    color: vec4<f32>,
    inset_x: i32,
    inset_y: i32,
    width: f32,
    inside: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let top = placed(px, vec2<i32>(p.inset_x, p.inset_y));
    let v = textureLoad(aux_tex, px, 0);
    var d = 3.4e38;
    if (v.a > 0.5) {
        d = length(v.xy);
    }
    if (p.inside == 1u) {
        // Past the frame counts as outside the shape.
        let size = vec2<i32>(frame.out_size);
        let edge = min(min(px.x + 1, px.y + 1), min(size.x - px.x, size.y - px.y));
        d = min(d, f32(edge));
        let stroke = p.color * (clamp(p.width + 1.0 - d, 0.0, 1.0) * top.a);
        return stroke + top * (1.0 - stroke.a);
    }
    let under = p.color * clamp(p.width + 1.0 - d, 0.0, 1.0);
    return top + under * (1.0 - top.a);
}
