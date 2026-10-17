// The input, texel for texel (the output is the input's size).

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return texel(out_px(in.pos));
}
