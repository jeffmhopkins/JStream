// Floor reflection (ADR-0151 decision 4), drawn with the shader-source prelude for its
// four textures: the picture; the scene's view depth (red: 1 / w = f / D, 0 where
// nothing was drawn); the scene drawn through its camera mirrored in the floor, as a
// right-handed camera, so its x runs the other way about the principal point; and that
// picture's own view depth.
//
// Work is in the scene's space through the camera's view: the point at distance D
// along the view through pixel q is position + D (right (q - c).x / f + down (q - c).y
// / f + forward). A pixel is on the floor when its point's height is within a
// footprint of `floor`: two pixels' size at its distance, or the depth's half-float
// precision there, whichever is more. Its reflection is at the mirrored pixel
// (2 c.x - q.x, q.y). The mirror image of what it shows lies along the same ray at that
// pixel's distance D' (the mirror keeps distances), which puts the reflected thing at
// height (V.y - floor) above the floor (y down) for V that point: it shows only if
// that is above the footprint, so nothing under the floor is reflected, faded linearly to nothing
// at `fade` above it. The reflection is laid source-over the picture at `strength`
// times that fade, blurred over a disc of 12 taps of radius `blur` when it is above 0.

struct Params {
    right: vec4<f32>,
    down: vec4<f32>,
    forward: vec4<f32>,
    // xyz: the camera's position in the scene's space; w: focal length.
    position: vec4<f32>,
    // xy: the principal point; z: the floor's height; w: strength.
    view: vec4<f32>,
    // x: fade; y: blur radius.
    look: vec4<f32>,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var depth_tex: texture_2d<f32>;
@group(0) @binding(6) var mirrored_tex: texture_2d<f32>;
@group(0) @binding(7) var mirrored_depth: texture_2d<f32>;

fn inverse_w(t: texture_2d<f32>, px: vec2<i32>) -> f32 {
    let size = vec2<i32>(textureDimensions(t));
    if any(px < vec2<i32>(0)) || any(px >= size) {
        return 0.0;
    }
    return textureLoad(t, px, 0).r;
}

// The mirrored picture at `px`, premultiplied linear, transparent outside it.
fn mirrored(px: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(mirrored_tex));
    if any(px < vec2<i32>(0)) || any(px >= size) {
        return vec4<f32>(0.0);
    }
    return to_premul_linear(textureLoad(mirrored_tex, px, 0), input_alpha(2u));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = to_premul_linear(textureSampleLevel(input_tex, lin, in.uv, 0.0), input_alpha(0u));
    let focal = p.position.w;
    let px = vec2<i32>(in.pos.xy);
    let here = inverse_w(depth_tex, px);
    if focal <= 0.0 || here <= 0.0 {
        return c;
    }
    let d = focal / here;
    let q = (in.pos.xy - p.view.xy) / focal;
    let ray = p.right.xyz * q.x + p.down.xyz * q.y + p.forward.xyz;
    let floor_y = p.view.z;
    let point = p.position.xyz + ray * d;
    let footprint = max(2.0 * d / focal * length(ray), d * 2e-3 * abs(ray.y));
    if abs(point.y - floor_y) > footprint {
        return c;
    }
    let at = vec2<i32>(i32(floor(2.0 * p.view.x - in.pos.x)), px.y);
    let there = inverse_w(mirrored_depth, at);
    if there <= 0.0 {
        return c;
    }
    let virtual_y = p.position.y + ray.y * (focal / there);
    let height = virtual_y - floor_y;
    // The floor's own mirror image is on the plane: only what is clearly above it.
    if height <= footprint {
        return c;
    }
    var fade = 1.0;
    if p.look.x > 0.0 {
        fade = clamp(1.0 - height / p.look.x, 0.0, 1.0);
    }
    var r = vec4<f32>(0.0);
    let blur = p.look.y;
    if blur > 0.0 {
        let spin = fract(52.9829189 * fract(dot(in.pos.xy, vec2<f32>(0.06711056, 0.00583715))))
            * 6.2831853;
        for (var k = 0u; k < 12u; k++) {
            let radius = sqrt((f32(k) + 0.5) / 12.0) * blur;
            let angle = f32(k) * 2.3999632 + spin;
            let tap = vec2<f32>(at) + vec2<f32>(cos(angle), sin(angle)) * radius;
            r += mirrored(vec2<i32>(floor(tap)));
        }
        r = r / 12.0;
    } else {
        r = mirrored(at);
    }
    let k = p.view.w * fade;
    return c * (1.0 - r.a * k) + r * k;
}
