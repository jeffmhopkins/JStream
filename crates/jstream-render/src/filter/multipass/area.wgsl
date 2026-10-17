// Where an effect applies: the effect (aux, a downsampled image) inside the area, the
// input elsewhere. The edge is hard.

struct Params {
    // x, y, width, height as fractions of the frame.
    rect: vec4<f32>,
    // 0 whole frame, 1 inside the rectangle, 2 outside it.
    mode: u32,
    // 0: read aux bilinear (a blur); 1: one aux texel per block (pixelate).
    blocks: u32,
    // Full-size pixels per aux texel.
    scale: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let inside = all(in.uv >= p.rect.xy) && all(in.uv < p.rect.xy + p.rect.zw);
    let applies = p.mode == 0u || (p.mode == 1u && inside) || (p.mode == 2u && !inside);
    if (!applies) {
        return texel(out_px(in.pos));
    }
    var at = in.pos.xy / (p.scale * frame.aux_size);
    if (p.blocks == 1u) {
        at = (floor(in.pos.xy / p.scale) + 0.5) / frame.aux_size;
    }
    return aux(at);
}
