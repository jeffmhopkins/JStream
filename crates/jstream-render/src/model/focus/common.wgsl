// Depth of field on a model's layer (ADR-0165). Shared by its five passes: the camera's
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

// A colour read from the model's layer, made safe to spread: NaN becomes 0 and an
// infinite channel the half-float layer's largest value. A gather spreads one pixel over
// up to TAPS others and the smoothing averages it into 25, so one non-finite pixel, which
// draws as a single dot without depth of field, would otherwise mark a whole disc.
fn finite(c: vec4<f32>) -> vec4<f32> {
    let bits = bitcast<vec4<u32>>(c);
    let all_ones = (bits & vec4<u32>(0x7f800000u)) == vec4<u32>(0x7f800000u);
    let fraction = (bits & vec4<u32>(0x007fffffu)) != vec4<u32>(0u);
    return select(clamp(c, vec4<f32>(-65504.0), vec4<f32>(65504.0)), vec4<f32>(0.0), all_ones & fraction);
}
