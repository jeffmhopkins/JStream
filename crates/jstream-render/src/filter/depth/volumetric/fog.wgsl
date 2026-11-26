// Volumetric fog at the picture's size (`march.wgsl` before it): each pixel marches its
// own view and lays the fog over the picture there.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let max_px = vec2<i32>(textureDimensions(depth_tex)) - vec2<i32>(1);
    let px = clamp(vec2<i32>(in.pos.xy), vec2<i32>(0), max_px);
    return over(picture(in.uv), march(in.pos.xy, textureLoad(depth_tex, px, 0).r));
}
