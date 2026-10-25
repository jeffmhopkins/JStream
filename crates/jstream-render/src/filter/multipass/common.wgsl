// Helpers shared by the multi-pass filters' shaders, between the prelude and a shader.

// One input texel. Outside the input it is the nearest edge texel when `edge` is 0 (a
// whole frame: a blur keeps its edges), and transparent when `edge` is 1 (a shape
// placed in a larger frame).
fn load_edge(q: vec2<i32>, edge: u32) -> vec4<f32> {
    let size = vec2<i32>(frame.in_size);
    if (edge == 1u && (any(q < vec2<i32>(0)) || any(q >= size))) {
        return vec4<f32>(0.0);
    }
    return texel(q);
}

// The output pixel this fragment covers.
fn out_px(pos: vec4<f32>) -> vec2<i32> {
    return vec2<i32>(floor(pos.xy));
}
