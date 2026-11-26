// Volumetric fog marched at half size laid over the picture at its own size, drawn with
// the shader-source prelude for its three textures: the picture; what `half.wgsl` marched
// for the top-left pixel of each block of two by two (the scattered light in rgb, what is
// let through in alpha); and the scene's view depth. Each pixel takes the four marched
// texels around it, bilinearly, each weighted again by how near its pixel's 1 / w is to
// this pixel's, so fog marched to a near model doesn't spread onto the far sky beside it
// and the sky's doesn't spread onto the model.

struct Params {
    // The fog's alpha, in x.
    alpha: vec4<f32>,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var half_tex: texture_2d<f32>;
@group(0) @binding(6) var depth_tex: texture_2d<f32>;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let c = to_premul_linear(textureLoad(input_tex, px, 0), input_alpha(0u));
    let own = textureLoad(depth_tex, px, 0).r;
    let last_half = vec2<i32>(textureDimensions(half_tex)) - vec2<i32>(1);
    let last_px = vec2<i32>(textureDimensions(depth_tex)) - vec2<i32>(1);
    // This pixel's centre among the half texels' centres, each at its block's top-left
    // pixel's centre.
    let at = (in.pos.xy - vec2<f32>(0.5)) / 2.0;
    let first = vec2<i32>(floor(at));
    let f = at - floor(at);
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var y = 0; y < 2; y = y + 1) {
        for (var x = 0; x < 2; x = x + 1) {
            let texel = clamp(first + vec2<i32>(x, y), vec2<i32>(0), last_half);
            let theirs = textureLoad(depth_tex, min(texel * 2, last_px), 0).r;
            let bilinear = select(1.0 - f.x, f.x, x == 1) * select(1.0 - f.y, f.y, y == 1);
            // Alike within about a percent of the distance keeps nearly all its weight;
            // twice as far or near, or one of them nothing, about a thousandth.
            let apart = abs(own - theirs) / max(max(own, theirs), 1e-6);
            let w = (bilinear + 1e-4) / (1.0 + apart * apart * 1e4);
            sum = sum + textureLoad(half_tex, texel, 0) * w;
            weight = weight + w;
        }
    }
    let marched = sum / weight;
    let a = p.alpha.x;
    let lets = mix(1.0, marched.a, a);
    return vec4<f32>(c.rgb * lets + marched.rgb * a, c.a * lets + (1.0 - marched.a) * a);
}
