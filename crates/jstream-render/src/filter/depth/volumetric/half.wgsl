// Volumetric fog's march at half the picture's size (`march.wgsl` before it, its four
// textures bound as there, the first being the view depth again, unread): each texel
// marches the view through the top-left pixel of its block of two by two, to that
// pixel's depth, and keeps what `march` gives (the scattered light in rgb, what is let
// through in alpha) for `apply.wgsl` to lay over the picture.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let max_px = vec2<i32>(textureDimensions(depth_tex)) - vec2<i32>(1);
    let px = clamp(vec2<i32>(in.pos.xy) * 2, vec2<i32>(0), max_px);
    return march(vec2<f32>(px) + vec2<f32>(0.5), textureLoad(depth_tex, px, 0).r);
}
