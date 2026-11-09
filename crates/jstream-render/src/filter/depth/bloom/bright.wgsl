// Depth bloom's bright pass (ADR-0151 decision 8): the input's light above a threshold
// of linear luma of its straight colour, fading in over a soft knee, premultiplied so a
// transparent pixel gives none. It is drawn at half the input's size: each texel the
// mean of the 2 x 2 it covers, its distance the first's. `aux` is the scene's view depth (red: 1 / w = f / D, 0
// where nothing was drawn) when `focal` is above 0: then light at distance D fades
// linearly to nothing at `far` when that is above 0.

struct Params {
    threshold: f32,
    knee: f32,
    focal: f32,
    far: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy) * 2;
    var sum = vec4<f32>(0.0);
    for (var k = 0; k < 4; k++) {
        let c = texel(px + vec2<i32>(k % 2, k / 2));
        let s = unpremul(c);
        sum += c * smoothstep(p.threshold - p.knee, p.threshold + p.knee, luma(s.rgb));
    }
    let c = sum * 0.25;
    var w = 1.0;
    if p.focal > 0.0 && p.far > 0.0 {
        let inverse_w = textureLoad(aux_tex, min(px, vec2<i32>(frame.aux_size) - 1), 0).r;
        if inverse_w > 0.0 {
            w = w * clamp(1.0 - p.focal / inverse_w / p.far, 0.0, 1.0);
        }
    }
    return c * w;
}
