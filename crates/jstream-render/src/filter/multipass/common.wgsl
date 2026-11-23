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

// One input texel's alpha, transparent outside the input: `load_edge(q, 1u).a` without
// working out its colour, for a pass that reads many texels' alpha alone. However the
// input stores alpha (the prelude's `Frame`), its alpha channel is its alpha, but for an
// opaque input, whose alpha reads as 1.
fn alpha_edge(q: vec2<i32>) -> f32 {
    if (any(q < vec2<i32>(0)) || any(q >= vec2<i32>(frame.in_size))) {
        return 0.0;
    }
    return select(textureLoad(input_tex, q, 0).a, 1.0, frame.alpha > 2.5);
}

// The output pixel this fragment covers.
fn out_px(pos: vec4<f32>) -> vec2<i32> {
    return vec2<i32>(floor(pos.xy));
}
