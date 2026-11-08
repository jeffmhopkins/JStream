// Depth fog (ADR-0137 decision 6). `aux` is the scene's view depth, the input's size:
// red holds 1 / w = f / D, 0 where nothing was drawn. At distance D the fog amount is
// (D - start) / (end - start) for linear, 1 - exp(-density (D - start) / 1000) for
// exponential, clamped to 0..1 and then to `most`, and 0 nearer than `start`; where
// nothing was drawn it is `most` with `background` and 0 without. The fog is laid over
// the pixel source-over, premultiplied linear, at `amount` times its own alpha:
// fog * amount + pixel * (1 - fog.a * amount), so a translucent fog colour covers the
// picture and never thins it. Without a depth (`has_depth` 0) the input passes through.

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    start: f32,
    end: f32,
    density: f32,
    most: f32,
    focal: f32,
    exponential: u32,
    background: u32,
    has_depth: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    if p.has_depth == 0u {
        return c;
    }
    let max_px = vec2<i32>(frame.aux_size) - vec2<i32>(1);
    let px = clamp(vec2<i32>(in.pos.xy), vec2<i32>(0), max_px);
    let inverse_w = textureLoad(aux_tex, px, 0).r;
    var amount = 0.0;
    if inverse_w <= 0.0 {
        amount = select(0.0, p.most, p.background == 1u);
    } else {
        let past = p.focal / inverse_w - p.start;
        if past > 0.0 {
            if p.exponential == 1u {
                amount = 1.0 - exp(-p.density * past / 1000.0);
            } else {
                amount = past / max(p.end - p.start, 1e-3);
            }
            amount = min(clamp(amount, 0.0, 1.0), p.most);
        }
    }
    let fog = vec4<f32>(p.color.rgb * p.color.a, p.color.a);
    return fog * amount + c * (1.0 - fog.a * amount);
}
