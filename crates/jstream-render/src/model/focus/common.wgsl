// Depth of field on a model's layer (ADR-0165). Shared by its four passes: the camera's
// focus as a `w` (`focus_px / f`) and its far blur in canvas pixels. A view depth texel
// holds `1 / w = f / D`, 0 where the model isn't; there the circle of confusion is 0 and
// the pixel is infinitely far.

struct Params {
    focus_w: f32,
    blur: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

// The largest blur radius, in canvas pixels: ADR-0108's cap.
const MAX_BLUR: f32 = 64.0;
// Pixels on a side of a tile the reach is kept per.
const TILE: i32 = 8;

// The circle of confusion of a surface at `inverse_w`: blur * |D - focus| / D, which is
// blur * |1 - focus_w / w|.
fn coc(inverse_w: f32) -> f32 {
    if inverse_w <= 0.0 {
        return 0.0;
    }
    return min(p.blur * abs(1.0 - p.focus_w * inverse_w), MAX_BLUR);
}
