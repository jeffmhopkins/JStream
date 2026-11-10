// Ambient occlusion, first pass (ADR-0151 decision 5). The input is the scene's view
// depth: red holds 1 / w = f / D, 0 where nothing was drawn. A pixel's point in the
// camera's space is ((px - c) D / f, D), with c the canvas centre, x right, y down and
// D along the view. Its normal comes from its neighbours, each side taking the nearer
// in depth so an edge doesn't bend it. `samples` points of a hemisphere of `radius`
// about the normal, nearer ones more often, rotated per pixel over a 4 x 4 tile, are
// projected back to the screen: one counts as occluded when the depth there is nearer
// than the point by more than a small bias, fading out where the occluder is much
// farther away than the radius. Output red is the occluded share (0 where nothing was
// drawn), green the pixel's own 1 / w, for the second pass.

struct Params {
    radius: f32,
    focal: f32,
    samples: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn inverse_w(px: vec2<i32>) -> f32 {
    let max_px = vec2<i32>(frame.in_size) - vec2<i32>(1);
    if any(px < vec2<i32>(0)) || any(px > max_px) {
        return 0.0;
    }
    return textureLoad(input_tex, px, 0).r;
}

// The point the depth puts at pixel centre `at`, `1 / w` given.
fn point(at: vec2<f32>, iw: f32) -> vec3<f32> {
    let d = p.focal / iw;
    return vec3<f32>((at - frame.in_size * 0.5) * d / p.focal, d);
}

// The point at pixel `px`, or `fallback` where nothing was drawn.
fn point_at(px: vec2<i32>, fallback: vec3<f32>) -> vec3<f32> {
    let iw = inverse_w(px);
    if iw <= 0.0 {
        return fallback;
    }
    return point(vec2<f32>(px) + 0.5, iw);
}

// Of the steps to `a` and `b` from `o`, the shorter in depth.
fn nearer(o: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    let da = a - o;
    let db = o - b;
    return select(db, da, abs(da.z) < abs(db.z));
}

// A 4 x 4 ordered pattern, 0 to 15, so the blur over the tile meets every rotation.
fn tile(px: vec2<i32>) -> f32 {
    let bayer = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0);
    let i = (px.y & 3) * 4 + (px.x & 3);
    return bayer[i];
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let iw = inverse_w(px);
    if iw <= 0.0 || p.focal <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let o = point(vec2<f32>(px) + 0.5, iw);
    // Two-pixel steps: the half-float depth is too coarse for one.
    let dx = nearer(o, point_at(px + vec2<i32>(2, 0), o), point_at(px - vec2<i32>(2, 0), o));
    let dy = nearer(o, point_at(px + vec2<i32>(0, 2), o), point_at(px - vec2<i32>(0, 2), o));
    var n = cross(dx, dy);
    if dot(n, n) < 1e-12 {
        n = vec3<f32>(0.0, 0.0, -1.0);
    }
    n = normalize(n);
    // Facing the camera, which looks along +z.
    if n.z > 0.0 {
        n = -n;
    }
    let angle = tile(px) / 16.0 * 6.2831853;
    let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.9);
    let t0 = normalize(cross(helper, n));
    let b0 = cross(n, t0);
    let t = t0 * cos(angle) + b0 * sin(angle);
    let b = cross(n, t);
    let bias = max(0.03 * p.radius, 0.004 * o.z);
    let count = max(p.samples, 1u);
    var occluded = 0.0;
    for (var i = 0u; i < count; i = i + 1u) {
        let u = (f32(i) + 0.5) / f32(count);
        let phi = f32(i) * 2.3999632;
        let r = sqrt(u);
        let k = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(1.0 - u));
        let s = f32(i + 1u) / f32(count);
        let reach = p.radius * mix(0.1, 1.0, s * s);
        let at = o + (t * k.x + b * k.y + n * k.z) * reach;
        if at.z <= 1.0 {
            continue;
        }
        let screen = at.xy * p.focal / at.z + frame.in_size * 0.5;
        let there = inverse_w(vec2<i32>(floor(screen)));
        if there <= 0.0 {
            continue;
        }
        let d = p.focal / there;
        let range = smoothstep(0.0, 1.0, p.radius / max(abs(o.z - d), 1e-3));
        if d < at.z - bias {
            occluded = occluded + range;
        }
    }
    return vec4<f32>(occluded / f32(count), iw, 0.0, 1.0);
}
