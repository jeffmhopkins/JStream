// Ambient occlusion, second pass (ADR-0151 decision 5). `aux` is the first pass's
// output: red the occluded share, green the pixel's 1 / w (0 where nothing was drawn).
// The share is averaged over the 4 x 4 tile the first pass rotated its pattern over,
// keeping to pixels about as far from the camera as this one, so an edge doesn't bleed.
// The picture is multiplied toward `color` by the share times `strength`, at most fully:
// c * (1 - k) + color * c.a * k, which keeps its alpha. Where nothing was drawn it is
// unchanged.

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    strength: f32,
    radius: f32,
    focal: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn share_at(px: vec2<i32>) -> vec2<f32> {
    let max_px = vec2<i32>(frame.aux_size) - vec2<i32>(1);
    return textureLoad(aux_tex, clamp(px, vec2<i32>(0), max_px), 0).rg;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    let px = vec2<i32>(in.pos.xy);
    let own = share_at(px);
    if own.y <= 0.0 || p.focal <= 0.0 {
        return c;
    }
    let d = p.focal / own.y;
    let near = max(0.01 * d, 0.25 * p.radius);
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -2; y < 2; y = y + 1) {
        for (var x = -2; x < 2; x = x + 1) {
            let s = share_at(px + vec2<i32>(x, y));
            if s.y > 0.0 && abs(p.focal / s.y - d) <= near {
                sum = sum + s.x;
                weight = weight + 1.0;
            }
        }
    }
    let share = select(own.x, sum / weight, weight > 0.0);
    let k = clamp(share * p.strength, 0.0, 1.0);
    let shade = vec4<f32>(p.color.rgb * c.a, c.a);
    return c * (1.0 - k) + shade * k;
}
