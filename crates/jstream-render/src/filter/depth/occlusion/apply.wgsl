// Ambient occlusion, second pass (ADR-0151 decision 5), drawn at the picture's size with
// the shader-source prelude for its three textures: the picture; the first pass's
// output, at half the size: for one pixel of each block of two by two, red the occluded
// share and green that pixel's 1 / w (0 where nothing was drawn); and the scene's view
// depth. The share is averaged over the 4 x 4 texels around the pixel, the tile the
// first pass rotated its pattern over, keeping to those whose pixel is about as far from
// the camera as this one, so an edge doesn't bleed. The picture is multiplied toward
// `color` by the share times `strength`, at most fully: c * (1 - k) + color * c.a * k,
// which keeps its alpha. Where nothing was drawn, or no texel near is as far, it is
// unchanged.

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    strength: f32,
    radius: f32,
    focal: f32,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var share_tex: texture_2d<f32>;
@group(0) @binding(6) var depth_tex: texture_2d<f32>;

fn share_at(texel: vec2<i32>) -> vec2<f32> {
    let last = vec2<i32>(textureDimensions(share_tex)) - vec2<i32>(1);
    return textureLoad(share_tex, clamp(texel, vec2<i32>(0), last), 0).rg;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let c = to_premul_linear(textureLoad(input_tex, px, 0), input_alpha(0u));
    let own = textureLoad(depth_tex, px, 0).r;
    if own <= 0.0 || p.focal <= 0.0 {
        return c;
    }
    let d = p.focal / own;
    let near = max(0.01 * d, 0.25 * p.radius);
    let block = px / 2;
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -2; y < 2; y = y + 1) {
        for (var x = -2; x < 2; x = x + 1) {
            let s = share_at(block + vec2<i32>(x, y));
            if s.y > 0.0 && abs(p.focal / s.y - d) <= near {
                sum = sum + s.x;
                weight = weight + 1.0;
            }
        }
    }
    if weight <= 0.0 {
        return c;
    }
    let k = clamp(sum / weight * p.strength, 0.0, 1.0);
    let shade = vec4<f32>(p.color.rgb * c.a, c.a);
    return c * (1.0 - k) + shade * k;
}
