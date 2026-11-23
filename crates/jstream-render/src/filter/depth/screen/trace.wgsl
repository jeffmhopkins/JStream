// Screen-space reflections, the march (ADR-0170 decision 4, ADR-0189), drawn at half
// the picture's size with the shader-source prelude for its five textures: the scene's
// picture; its view depth (red: 1 / w = f / D, 0 where nothing was drawn); and its
// surface buffer, which the engine draws of the scene's models with the view depth: the
// shading normal in the viewing frame and the surface's own 1 / w; the split-sum BRDF
// its environment's reflection was weighed by and its roughness; and that reflection as
// it was added, with 1 where one found on screen may replace it.
//
// Each texel here stands for a block of two by two pixels of the picture and marches
// the ray of one of them, `chosen` (resolve.wgsl names the same one). It writes what the
// ray found, times how sure the hit is, premultiplied: nothing where it found nothing.
// resolve.wgsl spreads that over the block's pixels.
//
// The viewing frame has x right and y down on the canvas, z toward the viewer, and the
// viewer at z = f: the point at distance D through pixel q is ((q - c) D / f, f - D),
// and a point (x, y, z) is seen at pixel c + (x, y) f / (f - z), with 1 / w = f / (f - z),
// which runs linearly across the screen along a ray's image.
//
// The pixel's reflected ray is followed across the screen in `steps` steps, from a
// per-texel offset so neighbours sample between each other's steps, until it passes
// behind what the view depth holds there by less than a step's depth (thicker, it went
// behind a thin thing, and goes on). Five halvings then refine the hit. The picture
// there is blurred over a disc as wide as the roughness spreads the ray by the time it
// gets there. How sure the hit is fades near the frame's edges, toward the end of a
// step's depth, for rays turned toward the viewer, between roughness 0.4 and 0.6, and by
// the hit's alpha. A hit brighter than `MAX_HIT` is scaled down, so a bright emissive
// spot doesn't sparkle.

struct Params {
    // x: focal length f; yz: the principal point; w: the steps a ray takes.
    view: vec4<f32>,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var depth_tex: texture_2d<f32>;
@group(0) @binding(6) var normal_tex: texture_2d<f32>;
@group(0) @binding(7) var brdf_tex: texture_2d<f32>;
@group(0) @binding(8) var reflected_tex: texture_2d<f32>;

const MAX_HIT: f32 = 8.0;

fn inverse_w(px: vec2<i32>) -> f32 {
    let size = vec2<i32>(textureDimensions(depth_tex));
    if any(px < vec2<i32>(0)) || any(px >= size) {
        return 0.0;
    }
    return textureLoad(depth_tex, px, 0).r;
}

// The picture at `at`, in pixels, bilinear, premultiplied linear.
fn picture(at: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(input_tex));
    return to_premul_linear(textureSampleLevel(input_tex, lin, at / size, 0.0), input_alpha(0u));
}

// Where the ray is seen at screen fraction `t`: its pixel and 1 / w.
struct Along {
    at: vec2<f32>,
    inv: f32,
}

fn along(a: vec3<f32>, b: vec3<f32>, t: f32) -> Along {
    let q = mix(a, b, t);
    return Along(q.xy, q.z);
}

// How far behind the view depth at `s` the ray is there, in distance: positive behind.
fn behind(s: Along) -> vec2<f32> {
    let there = inverse_w(vec2<i32>(floor(s.at)));
    if there <= 0.0 {
        return vec2<f32>(-1.0, 0.0);
    }
    let f = p.view.x;
    return vec2<f32>(f / s.inv - f / there, f / there);
}

// The pixel of the picture that the march's texel `texel` stands for: one corner of its
// block of two by two, the other corner on every other texel, so a row or a column of
// blocks doesn't always march the same line of the picture. Inside the picture.
fn chosen(texel: vec2<i32>, size: vec2<i32>) -> vec2<i32> {
    let corner = (texel.x + texel.y) & 1;
    return min(texel * 2 + vec2<i32>(corner), size - vec2<i32>(1));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(input_tex));
    let px = chosen(vec2<i32>(in.pos.xy), vec2<i32>(size));
    let at = vec2<f32>(px) + vec2<f32>(0.5);
    let none = vec4<f32>(0.0);
    let f = p.view.x;
    let reflected = textureLoad(reflected_tex, px, 0);
    let surface = textureLoad(normal_tex, px, 0);
    let brdf = textureLoad(brdf_tex, px, 0);
    let roughness = brdf.a;
    let inv = surface.w;
    let here = inverse_w(px);
    if reflected.a < 0.5 || f <= 0.0 || inv <= 0.0 || roughness >= 0.6 || dot(surface.xyz, surface.xyz) < 0.25 {
        return none;
    }
    // Something nearer than the surface covers it here: a card in front of the model.
    if here > inv * 1.002 {
        return none;
    }
    let centre = p.view.yz;
    let d = f / inv;
    let point = vec3<f32>((at - centre) * d / f, f - d);
    let v = normalize(vec3<f32>(0.0, 0.0, f) - point);
    let n = normalize(surface.xyz);
    let r = reflect(-v, n);
    // Toward the viewer the ray soon leaves the screen through its near side.
    let toward = clamp((dot(r, v) - 0.4) / 0.4, 0.0, 1.0);
    if toward >= 1.0 {
        return none;
    }
    // Its far end: as far again as the surface is, short of the viewer's plane.
    var reach = 2.0 * d;
    if r.z > 0.0 {
        reach = min(reach, (f - 0.05 * d - point.z) / r.z);
    }
    let end = point + r * reach;
    let a = vec3<f32>(centre + point.xy * inv, inv);
    let end_inv = f / (f - end.z);
    var b = vec3<f32>(centre + end.xy * end_inv, end_inv);
    // Cut at the frame's edges, so every step lands on the picture.
    let span = b.xy - a.xy;
    var cut = 1.0;
    for (var axis = 0; axis < 2; axis++) {
        if span[axis] > 1e-4 {
            cut = min(cut, (size[axis] - 0.5 - a[axis]) / span[axis]);
        } else if span[axis] < -1e-4 {
            cut = min(cut, (0.5 - a[axis]) / span[axis]);
        }
    }
    b = mix(a, b, clamp(cut, 0.0, 1.0));
    let pixels = length(b.xy - a.xy);
    if pixels < 2.0 {
        return none;
    }
    let steps = max(p.view.w, 1.0);
    let jitter = fract(52.9829189 * fract(dot(in.pos.xy, vec2<f32>(0.06711056, 0.00583715))));
    var last = 0.0;
    var found = -1.0;
    var thickness = 0.0;
    for (var i = 1.0; i <= steps; i += 1.0) {
        let t = (i - 1.0 + jitter) / steps + 1.5 / pixels;
        if t > 1.0 {
            break;
        }
        let s = along(a, b, t);
        let gap = behind(s);
        // A step's depth along the ray here, at least a little of the distance.
        let step_depth = max(abs(f / along(a, b, max(t - 1.0 / steps, 0.0)).inv - f / s.inv), 0.01 * gap.y);
        if gap.x > 0.0 && gap.x < step_depth * 1.5 {
            found = t;
            thickness = step_depth * 1.5;
            break;
        }
        // Only a step in front of the depth starts the halving: one behind a thick
        // occluder would leave both ends behind.
        if gap.x <= 0.0 {
            last = t;
        }
    }
    if found < 0.0 {
        return none;
    }
    var lo = last;
    var hi = found;
    for (var k = 0; k < 5; k++) {
        let mid = 0.5 * (lo + hi);
        let gap = behind(along(a, b, mid));
        if gap.x > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let hit = along(a, b, hi);
    let gap = behind(hit);
    if gap.x < 0.0 || gap.x > thickness {
        return none;
    }
    // How sure the hit is.
    let edge = min(min(hit.at.x, size.x - hit.at.x), min(hit.at.y, size.y - hit.at.y));
    var sure = clamp(edge / (0.08 * min(size.x, size.y)), 0.0, 1.0);
    sure *= 1.0 - clamp(gap.x / thickness, 0.0, 1.0) * 0.5;
    sure *= 1.0 - toward;
    sure *= 1.0 - clamp((roughness - 0.4) / 0.2, 0.0, 1.0);
    // The picture there, blurred as wide as the roughness has spread the ray.
    let travelled = length(hit.at - a.xy);
    let radius = min(travelled * roughness * roughness, 32.0);
    var seen = vec4<f32>(0.0);
    if radius > 0.75 {
        let spin = jitter * 6.2831853;
        for (var k = 0u; k < 12u; k++) {
            let ring = sqrt((f32(k) + 0.5) / 12.0) * radius;
            let angle = f32(k) * 2.3999632 + spin;
            seen += picture(hit.at + vec2<f32>(cos(angle), sin(angle)) * ring);
        }
        seen = seen / 12.0;
    } else {
        seen = picture(hit.at);
    }
    let bright = dot(seen.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    if bright > MAX_HIT * max(seen.a, 1e-4) {
        seen = vec4<f32>(seen.rgb * (MAX_HIT * max(seen.a, 1e-4) / bright), seen.a);
    }
    return seen * sure;
}
