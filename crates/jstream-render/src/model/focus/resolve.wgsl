// Pass 6: the blurred layer, at the layer's size (ADR-0193), drawn with the shader-source
// prelude for its three textures: the model's layer, premultiplied linear; each pixel's
// `1 / w` (red) and reach R (green); and what the gather found spreads over one pixel of
// each block of two by two, at half the size.
//
// A pixel no blur reaches is copied, so what is in focus draws exactly as without depth
// of field. Any other is two layers. The wide one is what the gather spread, taken from
// the gathered pixels of the 3 x 3 blocks around it, each weighed by how near it is and
// by how alike its own circle of confusion is to this pixel's, so what was gathered for
// a soft part isn't laid over a sharp one beside it, nor the reverse. The narrow one is
// what the gather left out, which a half-size picture can't hold: the share `1 - wide`
// of each of the 3 x 3 pixels around it (all of one whose blur is under the gather's tap
// spacing), spread over up to those 3 x 3 by its own radius, a farther one no farther
// than this pixel's own radius. The narrow layer goes under the wide one, into the
// alpha the wide one left.

struct Params {
    focus_w: f32,
    blur: f32,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var packed_tex: texture_2d<f32>;
@group(0) @binding(6) var gathered_tex: texture_2d<f32>;

// common.wgsl's, which this pass's prelude can't share.
const MAX_BLUR: f32 = 64.0;
const TAPS: f32 = 48.0;

fn coc(inverse_w: f32) -> f32 {
    if inverse_w <= 0.0 {
        return 0.0;
    }
    return min(p.blur * abs(1.0 - p.focus_w * inverse_w), MAX_BLUR);
}

fn finite(c: vec4<f32>) -> vec4<f32> {
    let bits = bitcast<vec4<u32>>(c);
    let all_ones = (bits & vec4<u32>(0x7f800000u)) == vec4<u32>(0x7f800000u);
    let fraction = (bits & vec4<u32>(0x007fffffu)) != vec4<u32>(0u);
    return select(clamp(c, vec4<f32>(-65504.0), vec4<f32>(65504.0)), vec4<f32>(0.0), all_ones & fraction);
}

// gather.wgsl's.
fn wide(r: f32) -> f32 {
    return smoothstep(0.75, 2.0, r);
}

// gather.wgsl's.
fn chosen(texel: vec2<i32>, size: vec2<i32>) -> vec2<i32> {
    let corner = (texel.x + texel.y) & 1;
    return min(texel * 2 + vec2<i32>(corner), size - vec2<i32>(1));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let size = vec2<i32>(textureDimensions(input_tex));
    let own = finite(textureLoad(input_tex, px, 0));
    let here = textureLoad(packed_tex, px, 0);
    let reach = here.g;
    if reach < 0.5 {
        return own;
    }
    let r_own = coc(here.r);
    let spacing = max(reach / sqrt(TAPS), 0.5);

    // The narrow layer.
    var narrow = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let at = px + vec2<i32>(x, y);
            if any(at < vec2<i32>(0)) || any(at >= size) {
                continue;
            }
            var colour = own;
            var inverse_w = here.r;
            if x != 0 || y != 0 {
                inverse_w = textureLoad(packed_tex, at, 0).r;
            }
            var r = coc(inverse_w);
            var share = 1.0;
            if r >= spacing {
                share = 1.0 - wide(r);
            }
            if share <= 0.0 {
                continue;
            }
            if inverse_w < here.r {
                r = min(r, r_own);
            }
            // From itself alone at radius 0 to the whole 3 x 3 at about 1.8.
            let grown = r * 0.8;
            let edge = clamp(grown, 0.0, 1.0);
            let corner = clamp(grown - 0.414, 0.0, 1.0);
            var cover = 1.0;
            if x != 0 && y != 0 {
                cover = corner;
            } else if x != 0 || y != 0 {
                cover = edge;
            }
            if cover <= 0.0 {
                continue;
            }
            if x != 0 || y != 0 {
                colour = finite(textureLoad(input_tex, at, 0));
            }
            narrow = narrow + colour * (share * cover / (1.0 + 4.0 * edge + 4.0 * corner));
        }
    }

    // The wide layer.
    let blocks = vec2<i32>(textureDimensions(gathered_tex));
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let texel = px / 2 + vec2<i32>(x, y);
            if any(texel < vec2<i32>(0)) || any(texel >= blocks) {
                continue;
            }
            let q = chosen(texel, size);
            let r = coc(textureLoad(packed_tex, q, 0).r);
            let apart = abs(vec2<f32>(q - px));
            let near = max(1.0 - apart.x / 2.5, 0.0) * max(1.0 - apart.y / 2.5, 0.0);
            let alike = clamp(1.0 - abs(r - r_own) / (0.5 * max(r, r_own) + 0.75), 0.0, 1.0);
            let w = near * alike;
            sum = sum + finite(textureLoad(gathered_tex, texel, 0)) * w;
            weight = weight + w;
        }
    }
    var spread = vec4<f32>(0.0);
    if weight >= 1e-4 {
        spread = sum / weight;
    }
    let room = max(1.0 - spread.a, 0.0);
    if narrow.a > room {
        narrow = narrow * (room / narrow.a);
    }
    return spread + narrow;
}
