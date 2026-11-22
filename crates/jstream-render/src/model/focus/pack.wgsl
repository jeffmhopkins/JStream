// Pass 3: each pixel's `1 / w` (the input, the view depth) beside the reach of its tile
// (`aux`), so the gather reads both from one image.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let inverse_w = textureLoad(input_tex, px, 0).r;
    let tiles = vec2<i32>(frame.aux_size) - vec2<i32>(1);
    let reach = textureLoad(aux_tex, clamp(px / TILE, vec2<i32>(0), tiles), 0).r;
    return vec4<f32>(inverse_w, reach, 0.0, 1.0);
}
