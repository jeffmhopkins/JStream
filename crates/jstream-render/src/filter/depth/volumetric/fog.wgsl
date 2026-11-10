// Volumetric fog (ADR-0151 decision 6), drawn with the shader-source prelude for its
// four textures: the picture, the scene's view depth (the input's size: red holds
// 1 / w = f / D, 0 where nothing was drawn) and two shadow maps. Work is in the viewing frame
// a scene-lit model uses (x right, y down, z toward the viewer, the viewer at z = f): the
// point at distance D along the view through pixel q is ((q - c) D / f, D, f - D) with
// c the principal point, and the scene's space is position + right x + down y +
// forward (f - z), where the height and the noise are taken.
//
// The ray marches from `start` to the depth (or `farthest` where nothing was drawn) in
// `steps` equal steps of D, each sampled at a jittered point, the jitter differing per
// pixel and per frame. At each the extinction is density / 1000 per pixel of ray,
// halving every `falloff` above `base_y` (y down: above is smaller y) and even below
// it, times 1 + noise (2 n - 1) for value noise n of two octaves drifting by `wind`
// (already in noise features, wrapped). Its light is the fog colour plus `scattering`
// times each scene light reaching the point (as a model receives it, with falloff and
// cone, and where it casts shadows only if its map sees the point) times a Henyey-Greenstein phase of g = 0.4 relative to an even one. Each step
// adds its light times (1 - exp(-sigma ds)) times what the fog lets through before it,
// and the picture is multiplied by what it lets through to the end. The fog's alpha
// scales both. Pixels nearer than `start` are unchanged.

const MAX_LIGHTS: u32 = 8u;
const MAX_SHADOWS: u32 = 2u;
const PI: f32 = 3.14159265;

struct Light {
    // xyz: position in the viewing frame; w: kind (0 point, 1 spot, 2 directional).
    at: vec4<f32>,
    // xyz: the unit direction it shines; w: range (0 never fades).
    aim: vec4<f32>,
    // rgb: linear radiance (colour times intensity).
    color: vec4<f32>,
    // x: cosine of the cone's half angle, y: of the half angle where its fade starts.
    cone: vec4<f32>,
}

// A shadow map's view, as `model.wgsl`'s `Shadow`: the light's position (w: the light
// slot it shadows, negative when unused) and axes in the viewing frame, its focal
// length (right.w) and its principal point (down.w).
struct Shadow {
    at: vec4<f32>,
    right: vec4<f32>,
    down: vec4<f32>,
    forward: vec4<f32>,
}

struct Params {
    // Straight-alpha linear light.
    color: vec4<f32>,
    right: vec4<f32>,
    down: vec4<f32>,
    forward: vec4<f32>,
    // xyz: the camera's position in the scene's space; w: focal length.
    position: vec4<f32>,
    // xy: the principal point; z: start; w: the farthest where nothing was drawn.
    view: vec4<f32>,
    // x: extinction per pixel at the base; y: base height; z: falloff; w: scattering.
    density: vec4<f32>,
    // x: noise amount; y: 1 / noise size; z: the frame's jitter seed.
    noise: vec4<f32>,
    // xyz: the noise's drift, in its own features.
    wind: vec4<f32>,
    steps: u32,
    light_count: u32,
    lights: array<Light, MAX_LIGHTS>,
    shadows: array<Shadow, MAX_SHADOWS>,
}

@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var input_tex: texture_2d<f32>;
@group(0) @binding(5) var depth_tex: texture_2d<f32>;
@group(0) @binding(6) var shadow0: texture_2d<f32>;
@group(0) @binding(7) var shadow1: texture_2d<f32>;

// The 1 / w shadow map `s` holds at `px`, 0 (nothing) outside its `size`.
fn shadow_texel(s: u32, px: vec2<i32>, size: vec2<i32>) -> f32 {
    if any(px < vec2<i32>(0)) || any(px >= size) {
        return 0.0;
    }
    if s == 0u {
        return textureLoad(shadow0, px, 0).r;
    }
    return textureLoad(shadow1, px, 0).r;
}

// Whether map `s`, of `size`, sees `q`: 1 unless it holds something nearer to its light
// there (by a small bias), or `q` is behind the light. One tap: the steps' jitter
// softens it.
fn seen_by_map(s: u32, q: vec3<f32>, size: vec2<i32>) -> f32 {
    let sh = p.shadows[s];
    let focal = sh.right.w;
    let rel = q - sh.at.xyz;
    let d = dot(rel, sh.forward.xyz);
    if d <= 0.0 || focal <= 0.0 {
        return 1.0;
    }
    let at = vec2<f32>(dot(rel, sh.right.xyz), dot(rel, sh.down.xyz)) * focal / d
        + vec2<f32>(sh.down.w);
    let stored = shadow_texel(s, vec2<i32>(floor(at)), size);
    return select(0.0, 1.0, stored <= focal / d * 1.002);
}

// Whether map `s` shadows light slot `light`.
fn shadows_light(s: u32, light: u32) -> bool {
    let w = p.shadows[s].at.w;
    return w >= 0.0 && u32(w) == light;
}

// Whether light slot `light` reaches `q`: 1 unless a map of its own, of `sizes` (map 0's
// in xy, map 1's in zw), doesn't see it.
//
// This runs inside the march's loops, whose step count is a uniform, so it must not
// make FXC (the Direct3D 12 compiler) unroll them, or FXC refuses the whole shader
// (X3511) and the scene's chain draws nothing on Windows. A `textureDimensions` in
// a loop does, and so does a loop over the maps that picks a texture by its index:
// the sizes are read once before the march, and the MAX_SHADOWS (2) maps are tried one
// by one.
fn reaches(light: u32, q: vec3<f32>, sizes: vec4<i32>) -> f32 {
    if shadows_light(0u, light) {
        return seen_by_map(0u, q, sizes.xy);
    }
    if shadows_light(1u, light) {
        return seen_by_map(1u, q, sizes.zw);
    }
    return 1.0;
}

// The picture, premultiplied linear.
fn picture(uv: vec2<f32>) -> vec4<f32> {
    return to_premul_linear(textureSampleLevel(input_tex, lin, uv, 0.0), input_alpha(0u));
}

fn hash3(c: vec3<i32>) -> f32 {
    let w = vec3<u32>(c & vec3<i32>(255));
    var h = w.x * 73856093u ^ w.y * 19349663u ^ w.z * 83492791u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 65535u) / 65535.0;
}

// Value noise, 0 to 1, repeating every 256 features.
fn value_noise(x: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(x));
    let f = fract(x);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(hash3(i), hash3(i + vec3<i32>(1, 0, 0)), u.x);
    let b = mix(hash3(i + vec3<i32>(0, 1, 0)), hash3(i + vec3<i32>(1, 1, 0)), u.x);
    let c = mix(hash3(i + vec3<i32>(0, 0, 1)), hash3(i + vec3<i32>(1, 0, 1)), u.x);
    let d = mix(hash3(i + vec3<i32>(0, 1, 1)), hash3(i + vec3<i32>(1, 1, 1)), u.x);
    return mix(mix(a, b, u.y), mix(c, d, u.y), u.z);
}

// Interleaved gradient noise, 0 to 1, for the steps' jitter.
fn jitter(q: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(q, vec2<f32>(0.06711056, 0.00583715)) + p.noise.z));
}

fn extinction(scene: vec3<f32>) -> f32 {
    var sigma = p.density.x;
    let falloff = p.density.z;
    if falloff > 0.0 {
        let above = max(p.density.y - scene.y, 0.0);
        sigma = sigma * exp2(-above / falloff);
    }
    let amount = p.noise.x;
    if amount > 0.0 {
        let at = scene * p.noise.y - p.wind.xyz;
        let n = value_noise(at) * 0.65 + value_noise(at * 2.03 + vec3<f32>(17.0)) * 0.35;
        sigma = sigma * max(1.0 + amount * (2.0 * n - 1.0), 0.0);
    }
    return sigma;
}

// The light of scene light `source` reaching `q` and scattered toward the viewer along
// `to_eye`.
fn in_light(source: Light, q: vec3<f32>, to_eye: vec3<f32>) -> vec3<f32> {
    var travel = source.aim.xyz;
    var strength = 1.0;
    if source.at.w < 1.5 {
        let from_light = q - source.at.xyz;
        let d = length(from_light);
        if d <= 1e-4 {
            return vec3<f32>(0.0);
        }
        travel = from_light / d;
        let range = source.aim.w;
        if range > 0.0 {
            let x = clamp(1.0 - (d / range) * (d / range), 0.0, 1.0);
            strength = x * x;
        }
        if source.at.w > 0.5 {
            let along = dot(travel, source.aim.xyz);
            let outer = source.cone.x;
            let inner = source.cone.y;
            if inner - outer > 1e-5 {
                strength = strength * smoothstep(outer, inner, along);
            } else {
                strength = strength * select(0.0, 1.0, along >= outer);
            }
        }
    }
    if strength <= 0.0 {
        return vec3<f32>(0.0);
    }
    let g = 0.4;
    let cos_theta = dot(travel, to_eye);
    let k = 1.0 + g * g - 2.0 * g * cos_theta;
    let phase = (1.0 - g * g) / (k * sqrt(k));
    return source.color.rgb * strength * phase;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = picture(in.uv);
    let focal = p.position.w;
    if focal <= 0.0 || p.steps == 0u {
        return c;
    }
    let max_px = vec2<i32>(textureDimensions(depth_tex)) - vec2<i32>(1);
    let px = clamp(vec2<i32>(in.pos.xy), vec2<i32>(0), max_px);
    let inverse_w = textureLoad(depth_tex, px, 0).r;
    let start = p.view.z;
    var end = p.view.w;
    if inverse_w > 0.0 {
        end = focal / inverse_w;
    }
    if end <= start {
        return c;
    }
    // The point at distance D along the view is `slope * D` from the viewer.
    let slope = vec3<f32>((in.pos.xy - p.view.xy) / focal, -1.0);
    let ray = length(slope);
    let to_eye = -slope / ray;
    let eye = vec3<f32>(0.0, 0.0, focal);
    let steps = f32(p.steps);
    let dd = (end - start) / steps;
    let ds = dd * ray;
    let offset = jitter(in.pos.xy);
    // Read here, not in the loops: see `reaches`.
    let sizes = vec4<i32>(
        vec2<i32>(textureDimensions(shadow0)),
        vec2<i32>(textureDimensions(shadow1)),
    );
    let fog = p.color.rgb;
    var through = 1.0;
    var light = vec3<f32>(0.0);
    for (var i = 0u; i < p.steps; i = i + 1u) {
        let d = start + (f32(i) + offset) * dd;
        let q = eye + slope * d;
        let scene = p.position.xyz + p.right.xyz * q.x + p.down.xyz * q.y
            + p.forward.xyz * (focal - q.z);
        let sigma = extinction(scene);
        if sigma <= 0.0 {
            continue;
        }
        var here = fog;
        for (var l = 0u; l < min(p.light_count, MAX_LIGHTS); l = l + 1u) {
            let lit = reaches(l, q, sizes);
            if lit > 0.0 {
                here = here + p.density.w * lit * in_light(p.lights[l], q, to_eye);
            }
        }
        let kept = exp(-sigma * ds);
        light = light + here * (1.0 - kept) * through;
        through = through * kept;
        if through < 1e-3 {
            break;
        }
    }
    let a = p.color.a;
    let lets = mix(1.0, through, a);
    // Source-over, premultiplied, as depth fog is: the fog's light and coverage over
    // the picture by what it lets through.
    return vec4<f32>(c.rgb * lets + light * a, c.a * lets + (1.0 - through) * a);
}
