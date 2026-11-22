// Pass 2: for each tile, the largest circle of confusion among the tiles whose blur
// reaches it. A tile `n` tiles away (by the larger of its offsets) reaches this one
// when its radius passes the (n - 1) tiles of pixels between them.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(frame.aux_size);
    let here = vec2<i32>(in.pos.xy);
    let span = i32(ceil(MAX_BLUR / f32(TILE)));
    var most = 0.0;
    for (var y = -span; y <= span; y = y + 1) {
        for (var x = -span; x <= span; x = x + 1) {
            let at = here + vec2<i32>(x, y);
            if at.x < 0 || at.y < 0 || at.x >= size.x || at.y >= size.y {
                continue;
            }
            let r = textureLoad(aux_tex, at, 0).r;
            let between = f32(max(max(abs(x), abs(y)) - 1, 0) * TILE);
            if r > between {
                most = max(most, r);
            }
        }
    }
    return vec4<f32>(most, 0.0, 0.0, 1.0);
}
