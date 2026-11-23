// Pass 5: smooths the gather's grain, at the gather's half size (ADR-0193). The input is
// the gathered layer, `aux` the packed `1 / w` and reach at full size. A texel whose
// pixel's own circle of confusion is under 1.5 px is copied, so what spreads over a
// sharp part keeps its shape. A softer one becomes a mean of the 5 x 5 texels around it,
// each weighted by how soft its pixel is (0 at 1.5 px, 1 from 3 px), so what was gathered
// for a sharp neighbour isn't spread into the blur, and by a cone as wide as the grain:
// one and a half of the gather's tap spacings, and at most 0.4 of the pixel's own blur,
// so the window adds no visible softness of its own.

// How soft the pixel at `at` is: its circle of confusion, or its tile's reach where the
// model isn't, so a soft silhouette is smoothed along with what it spreads over.
fn soft(at: vec2<i32>) -> vec2<f32> {
    let there = textureLoad(aux_tex, at, 0);
    if there.r <= 0.0 {
        return vec2<f32>(there.g, there.g);
    }
    return vec2<f32>(coc(there.r), there.g);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(in.pos.xy);
    let blocks = vec2<i32>(textureDimensions(input_tex));
    let size = vec2<i32>(textureDimensions(aux_tex));
    let own = finite(textureLoad(input_tex, texel, 0));
    let here = soft(chosen(texel, size));
    if here.x < 1.5 {
        return own;
    }
    // In texels of this half-size layer.
    let cone = min(1.5 * here.y / sqrt(48.0), 0.4 * here.x) * 0.5 + 0.5;
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let fall = max(1.0 - length(vec2<f32>(f32(x), f32(y))) / cone, 0.0);
            let at = texel + vec2<i32>(x, y);
            if fall <= 0.0 || any(at < vec2<i32>(0)) || any(at >= blocks) {
                continue;
            }
            let w = fall * clamp((soft(chosen(at, size)).x - 1.5) / 1.5, 0.0, 1.0);
            sum = sum + finite(textureLoad(input_tex, at, 0)) * w;
            weight = weight + w;
        }
    }
    if weight <= 0.0 {
        return own;
    }
    return sum / weight;
}
