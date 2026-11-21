// Pass 5: smooths the gather's grain. The input is the gathered layer, `aux` the packed
// `1 / w` and reach. A pixel whose own circle of confusion is under 1.5 px is copied, so
// what is sharp stays sharp; a softer one becomes the mean of the 5 x 5 pixels around
// it, each weighted by how soft it is itself (0 at 1.5 px, 1 from 3 px), so a sharp
// neighbour's colour isn't spread into the blur. The window is far smaller than the
// blur it smooths, so it adds no visible softness of its own.

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let size = vec2<i32>(frame.in_size);
    let own = finite(textureLoad(input_tex, px, 0));
    let here = textureLoad(aux_tex, px, 0);
    // Empty pixels are softened too where a blur reaches them, so a soft silhouette is
    // smoothed along with what it spreads over.
    var soft = coc(here.r);
    if here.r <= 0.0 {
        soft = here.g;
    }
    if soft < 1.5 {
        return own;
    }
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let at = clamp(px + vec2<i32>(x, y), vec2<i32>(0), size - vec2<i32>(1));
            let there = textureLoad(aux_tex, at, 0);
            var r = coc(there.r);
            if there.r <= 0.0 {
                r = there.g;
            }
            let w = clamp((r - 1.5) / 1.5, 0.0, 1.0);
            sum = sum + finite(textureLoad(input_tex, at, 0)) * w;
            weight = weight + w;
        }
    }
    if weight <= 0.0 {
        return own;
    }
    return sum / weight;
}
