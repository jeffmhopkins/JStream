// Pass 4: what spreads over each pixel, at half the layer's size (ADR-0193). Each texel
// stands for a block of two by two pixels and gathers for one of them, `chosen`
// (resolve.wgsl names the same one). The input is the model's layer, premultiplied
// linear; `aux` holds each pixel's `1 / w` (red) and reach R (green), both at full size.
// A spiral of TAPS taps over a disc
// of radius R, turned per pixel, stands in for every pixel within R. A tap at distance d
// spreads to this pixel when d is within its own radius r (a farther tap no farther than
// this pixel's own radius, so a sharp near part keeps its edge), weighted by
// R^2 / (TAPS r^2): its colour spread evenly over its disc. Taps narrower than the tap
// spacing R / sqrt(TAPS) don't spread; the pixel's own colour stands for them, under
// what spreads over it, which the resolve adds. Where taps land on a sharp nearer part, a
// farther pixel's sum is scaled by the share of taps left, for the background that part
// hides.

const TAPS: u32 = 48u;
const GOLDEN: f32 = 2.39996323;

// A hash of the pixel, 0..1, to turn its spiral.
fn turn(px: vec2<i32>) -> f32 {
    let q = vec2<u32>(px);
    var h = q.x * 1664525u + q.y * 1013904223u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    h = h ^ (h >> 13u);
    return f32(h & 65535u) / 65536.0;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(input_tex));
    let px = chosen(vec2<i32>(in.pos.xy), size);
    let here = textureLoad(aux_tex, px, 0);
    let inverse_w = here.r;
    let reach = here.g;
    if reach < 0.5 {
        return vec4<f32>(0.0);
    }
    let r_own = coc(inverse_w);
    let spacing = max(reach / sqrt(f32(TAPS)), 0.5);
    let start = turn(px) * 6.2831853;
    var sum = vec4<f32>(0.0);
    var hidden = 0u;
    for (var i = 0u; i < TAPS; i = i + 1u) {
        let d = reach * sqrt((f32(i) + 0.5) / f32(TAPS));
        let a = start + f32(i) * GOLDEN;
        let at = px + vec2<i32>(round(vec2<f32>(cos(a), sin(a)) * d));
        if at.x < 0 || at.y < 0 || at.x >= size.x || at.y >= size.y {
            continue;
        }
        let tap_w = textureLoad(aux_tex, at, 0).r;
        if tap_w <= 0.0 {
            continue;
        }
        var r = coc(tap_w);
        let farther = tap_w < inverse_w;
        if farther {
            r = min(r, r_own);
        }
        if r < spacing {
            // A sharp nearer part: it hides what a farther pixel would gather here.
            if !farther && inverse_w > 0.0 && tap_w > inverse_w {
                hidden = hidden + 1u;
            }
            continue;
        }
        if d > r {
            continue;
        }
        let colour = finite(textureLoad(input_tex, at, 0));
        sum = sum + colour * (wide(r) * reach * reach / (f32(TAPS) * r * r));
    }
    if inverse_w > 0.0 && r_own >= spacing && hidden > 0u && hidden < TAPS {
        sum = sum * (f32(TAPS) / f32(TAPS - hidden));
    }
    if sum.a > 1.0 {
        sum = sum / sum.a;
    }
    return sum;
}
