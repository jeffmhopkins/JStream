// A 3D scene's sky backdrop (ADR-0167): the sky of its environment light seen through
// the camera, infinitely far away. Each pixel looks along its ray from the camera, so the
// sky turns with where the camera looks and never moves with where it is. The sky is the
// environment's gradient, or its picture as an equirectangular panorama, with procedural
// stars over it. Out: premultiplied linear light, at the backdrop's opacity.

struct Sky {
    // xyz: the camera's right axis in the scene's space; w: the principal point's x.
    right: vec4<f32>,
    // xyz: its down axis; w: the principal point's y.
    down: vec4<f32>,
    // xyz: its forward axis; w: the focal length in canvas pixels.
    forward: vec4<f32>,
    // rgb: straight up, linear light times intensity; w: the backdrop's opacity.
    sky: vec4<f32>,
    // rgb: at the horizon; w: the turn about the vertical, in radians.
    horizon: vec4<f32>,
    // rgb: below the horizon; w: how the picture stores alpha, or -1 for none.
    ground: vec4<f32>,
    // x: how many stars, 0 to 1; y: their brightness times intensity; z: the picture's
    // scale, the intensity; w: unused.
    stars: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Sky;
@group(0) @binding(1) var sky_tex: texture_2d<f32>;
@group(0) @binding(2) var sky_sampler: sampler;

const PI: f32 = 3.14159265;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// The sky's direction for the canvas pixel at `p`: its ray from the camera, in the
// scene's space (x right, y down, z toward the viewer), spun by the turn.
fn ray(p: vec2<f32>) -> vec3<f32> {
    let f = max(u.forward.w, 1e-3);
    let x = (p.x - u.right.w) / f;
    let y = (p.y - u.down.w) / f;
    let d = normalize(u.right.xyz * x + u.down.xyz * y + u.forward.xyz);
    let c = cos(u.horizon.w);
    let s = sin(u.horizon.w);
    return vec3<f32>(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
}

// The gradient along `d`, as a model's environment light reads it (ADR-0151 decision 9).
fn gradient(d: vec3<f32>) -> vec3<f32> {
    let up = clamp(-d.y, -1.0, 1.0);
    if up >= 0.0 {
        return mix(u.horizon.rgb, u.sky.rgb, sqrt(up));
    }
    return mix(u.horizon.rgb, u.ground.rgb, min(-up * 4.0, 1.0));
}

// The panorama along `d`: its centre ahead along -z, up at its top.
fn panorama(d: vec3<f32>) -> vec3<f32> {
    let uu = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let vv = clamp(0.5 - asin(clamp(-d.y, -1.0, 1.0)) / PI, 0.0005, 0.9995);
    let c = to_premul_linear(textureSampleLevel(sky_tex, sky_sampler, vec2<f32>(uu, vv), 0.0), u.ground.w);
    return c.rgb * u.stars.z;
}

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

fn unit(x: u32) -> f32 {
    return f32(hash(x) & 0xffffffu) / 16777216.0;
}

// The point of face `face` of a cube at face coordinates `uv` (each -1 to 1).
fn on_face(face: u32, uv: vec2<f32>) -> vec3<f32> {
    switch face {
        case 0u: { return vec3<f32>(1.0, uv.y, -uv.x); }
        case 1u: { return vec3<f32>(-1.0, uv.y, uv.x); }
        case 2u: { return vec3<f32>(uv.x, 1.0, -uv.y); }
        case 3u: { return vec3<f32>(uv.x, -1.0, uv.y); }
        case 4u: { return vec3<f32>(uv.x, uv.y, 1.0); }
        default: { return vec3<f32>(-uv.x, uv.y, -1.0); }
    }
}

// One layer of stars along `d`: the sky is cut into `cells` by `cells` squares on each
// face of a cube around the camera, and a square holds a star with chance `chance`, at a
// point of its own inside it, so the stars are fixed in the sky. `pixel` is one canvas
// pixel's angle, which sets how wide a star draws.
fn star_layer(d: vec3<f32>, cells: f32, chance: f32, seed: u32, pixel: f32) -> vec3<f32> {
    let a = abs(d);
    var face = 0u;
    var uv = vec2<f32>(0.0);
    if a.x >= a.y && a.x >= a.z {
        face = select(1u, 0u, d.x > 0.0);
        uv = vec2<f32>(select(d.z, -d.z, d.x > 0.0), d.y) / a.x;
    } else if a.y >= a.z {
        face = select(3u, 2u, d.y > 0.0);
        uv = vec2<f32>(d.x, select(d.z, -d.z, d.y > 0.0)) / a.y;
    } else {
        face = select(5u, 4u, d.z > 0.0);
        uv = vec2<f32>(select(-d.x, d.x, d.z > 0.0), d.y) / a.z;
    }
    let cell = min(floor((uv * 0.5 + 0.5) * cells), vec2<f32>(cells - 1.0));
    let id = (face * 65536u + u32(cell.y) * 256u + u32(cell.x)) * 7u + seed;
    if unit(id) >= chance {
        return vec3<f32>(0.0);
    }
    // Kept off the square's edges, so a star never draws cut by the next square.
    let jitter = vec2<f32>(unit(id + 1u), unit(id + 2u)) * 0.5 + 0.25;
    let at = normalize(on_face(face, ((cell + jitter) / cells) * 2.0 - 1.0));
    let angle = length(d - at);
    let sigma = max(pixel * 0.6, 1e-5);
    let glow = exp(-angle * angle / (2.0 * sigma * sigma));
    // Most stars are faint and a few are bright; their colour runs warm to cool.
    let bright = 0.15 + 2.5 * pow(unit(id + 3u), 6.0);
    let warmth = unit(id + 4u);
    let colour = mix(vec3<f32>(1.0, 0.82, 0.66), vec3<f32>(0.74, 0.84, 1.0), warmth);
    return colour * bright * glow;
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let d = ray(p.xy);
    var c = gradient(d);
    if u.ground.w >= 0.0 {
        c = panorama(d);
    }
    let amount = u.stars.x;
    if amount > 0.0 {
        let pixel = 1.0 / max(u.forward.w, 1e-3);
        let stars = star_layer(d, 160.0, amount * 0.25, 1u, pixel)
            + star_layer(d, 70.0, amount * 0.3, 3u, pixel) * 1.6;
        c += stars * u.stars.y;
    }
    let a = clamp(u.sky.w, 0.0, 1.0);
    return vec4<f32>(c * a, a);
}
