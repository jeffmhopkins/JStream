// Pass 1: the largest circle of confusion in each TILE x TILE block of the view depth
// (`aux`, which is also the input). Red holds it.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(frame.aux_size);
    let origin = vec2<i32>(in.pos.xy) * TILE;
    var most = 0.0;
    for (var y = 0; y < TILE; y = y + 1) {
        for (var x = 0; x < TILE; x = x + 1) {
            let px = origin + vec2<i32>(x, y);
            if px.x < size.x && px.y < size.y {
                most = max(most, coc(textureLoad(aux_tex, px, 0).r));
            }
        }
    }
    return vec4<f32>(most, 0.0, 0.0, 1.0);
}
