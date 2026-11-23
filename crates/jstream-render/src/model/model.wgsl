// A 3D model's triangles (ADR-0107 decisions 3, 4 and 6), drawn into a layer with their
// own depth test, shaded with glTF's metallic-roughness model in linear light.
//
// `frame.clip` takes a model point to clip space with reversed depth (larger nearer);
// `frame.view` takes it to the viewing frame: x right, y down, z toward the viewer, the
// viewer at z = frame.perspective (0 for none: orthographic, looking along -z). Both
// were composed on the CPU in f64. The fragment writes premultiplied linear colour.
//
// The studio rig is fixed to the viewing frame, so it moves with the camera: a key light
// above and to the left, a fill from the right, and a vertical gradient for ambient light
// and reflections. Back faces are discarded unless the material is double-sided, where
// they are lit from their own side; `frame.mirrored` swaps which face is the front.
//
// With scene lights (ADR-0143 decision 3) the rig is replaced by the scene's point, spot
// and directional lights, placed in the same viewing frame on the CPU, and the
// environment is scaled by the scene's ambient light. A scene's environment light
// (ADR-0151 decision 9) replaces the studio gradient with its sky, in the scene's space:
// a gradient from its ground through its horizon to its sky straight up, or its
// picture as an equirectangular panorama, oriented by its turn, tilt and roll. A model's
// own `reflection` (ADR-0107 decision 5) is such a panorama too, with or without scene
// lights, in place of either. A rough
// reflection, and the diffuse light, read the panorama over a ring of taps around the
// direction, wider the rougher. With no ambient light the sky lights a model on its own.
//
// Inside a reflection probe's box (ADR-0170 decision 3) a scene-lit surface reflects the
// probe's capture of the scene around it in place of the environment: its reflected
// direction is corrected to where it meets the box, so nearby walls line up, and the
// capture's panorama is read with the same blur as the sky. Where the capture saw
// nothing, the environment shows through. Near the box's faces the probe fades into the
// environment. Diffuse light still comes from the environment.

const MAX_LIGHTS: u32 = 8u;
const MAX_SHADOWS: u32 = 2u;

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

struct Frame {
    clip: mat4x4<f32>,
    view: mat4x4<f32>,
    normal: mat3x3<f32>,
    // x: perspective distance (0 for none), y: mirrored (0 or 1), z: unlit (0 or 1),
    // w: scene lights (0 or 1).
    params: vec4<f32>,
    // rgb: the scene's ambient light; w: how many of `lights` are set.
    ambient: vec4<f32>,
    lights: array<Light, MAX_LIGHTS>,
    shadows: array<Shadow, MAX_SHADOWS>,
    environment: Environment,
    // A floor the model is cut at (ADR-0151 decision 4): a fragment whose dot(xyz, seen)
    // + w is at or above 0 is at or below it, and isn't drawn. (0, 0, 0, -1) cuts nothing.
    cut: vec4<f32>,
    probe: Probe,
}

// The reflection probe a surface reads (ADR-0170 decision 3), in the viewing frame.
struct Probe {
    // xyz: the box's centre; w: 1 when there is one.
    centre: vec4<f32>,
    // xyz: the scene's x, y and z axes; w: the box's half size along each.
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
    // x: how far inside its faces it fades to the environment; y: how its picture stores
    // alpha.
    fade: vec4<f32>,
}

fn below_floor(seen: vec3<f32>) -> bool {
    return dot(frame.cut.xyz, seen) + frame.cut.w >= 0.0;
}

// The scene's sky (ADR-0151 decision 9).
struct Environment {
    // rgb: straight up, linear light times intensity; w: 1 when there is one.
    sky: vec4<f32>,
    // rgb: at the horizon; w: unused (the axes below carry the sky's orientation).
    horizon: vec4<f32>,
    // rgb: below the horizon; w: how its picture stores alpha, -1 for no picture.
    ground: vec4<f32>,
    // xyz: the camera's right axis in the scene; w: 1 when an ambient light scales it.
    right: vec4<f32>,
    // xyz: its down axis; w: the intensity the picture is scaled by.
    down: vec4<f32>,
    // xyz: its forward axis.
    forward: vec4<f32>,
}

// One light's shadow map (ADR-0151 decision 3), in the viewing frame.
struct Shadow {
    // xyz: the light's position; w: the light slot it shadows, -1 for none.
    at: vec4<f32>,
    // xyz: the light view's right axis; w: its focal length in map pixels.
    right: vec4<f32>,
    // xyz: its down axis; w: its principal point, both coordinates.
    down: vec4<f32>,
    // xyz: its forward axis; w: the filter's radius in map pixels.
    forward: vec4<f32>,
}

struct Material {
    base: vec4<f32>,
    // rgb: emitted light; a: has a normal map (0 or 1).
    emissive: vec4<f32>,
    // metallic, roughness, normal scale, occlusion strength.
    surface: vec4<f32>,
    // alpha mode (0 opaque, 1 mask, 2 blend), cutoff, double-sided, unlit.
    flags: vec4<f32>,
    // x: 1 for a lit card (ADR-0151 decision 10), whose base and emissive textures are
    // its picture in premultiplied linear light, so it emits its own colour. y: 1 when
    // the base texture is another source's picture (ADR-0107 decision 5), a mip chain in
    // premultiplied linear light.
    card: vec4<f32>,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow0: texture_2d<f32>;
@group(0) @binding(2) var shadow1: texture_2d<f32>;
@group(0) @binding(3) var sky_tex: texture_2d<f32>;
@group(0) @binding(4) var sky_sampler: sampler;
@group(0) @binding(5) var probe_tex: texture_2d<f32>;
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var tex_sampler: sampler;
@group(1) @binding(2) var base_tex: texture_2d<f32>;
@group(1) @binding(3) var surface_tex: texture_2d<f32>;
@group(1) @binding(4) var normal_tex: texture_2d<f32>;
@group(1) @binding(5) var occlusion_tex: texture_2d<f32>;
@group(1) @binding(6) var emissive_tex: texture_2d<f32>;
// The sampler of every map but the base colour: less anisotropy, as their detail shapes
// the light and is lost in it across a grazing surface.
@group(1) @binding(7) var detail_sampler: sampler;
// A rigged model's palette (ADR-0151 decision 7): a vertex stands at the weighted sum of
// its joints' matrices times its position. A model without a rig weighs none of it.
@group(2) @binding(0) var<storage, read> pose: array<mat4x4<f32>>;

// The matrix that places a vertex: its joints' palette matrices, weighed.
fn skin_matrix(joints: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    return pose[joints.x] * weights.x + pose[joints.y] * weights.y
        + pose[joints.z] * weights.z + pose[joints.w] * weights.w;
}

struct VsOut {
    // Invariant, so the depth prepass and the shading pass place a fragment at the same
    // depth, and the shading pass's equal test keeps exactly the nearest.
    @builtin(position) @invariant clip: vec4<f32>,
    @location(0) seen: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) color: vec4<f32>,
}

@vertex
fn vs(
    @location(0) bound: vec3<f32>,
    @location(1) bound_normal: vec3<f32>,
    @location(2) bound_tangent: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) joints: vec4<u32>,
    @location(6) weights: vec4<f32>,
) -> VsOut {
    var position = bound;
    var normal = bound_normal;
    var tangent = bound_tangent;
    if (any(weights != vec4<f32>(0.0))) {
        let m = skin_matrix(joints, weights);
        let a = m[0].xyz;
        let b = m[1].xyz;
        let c = m[2].xyz;
        // det × the inverse transpose, for the normal; its sign keeps it outward.
        let side = select(1.0, -1.0, dot(a, cross(b, c)) < 0.0);
        position = (m * vec4<f32>(bound, 1.0)).xyz;
        normal = normalize(mat3x3<f32>(cross(b, c), cross(c, a), cross(a, b)) * bound_normal) * side;
        tangent = vec4<f32>(normalize(mat3x3<f32>(a, b, c) * bound_tangent.xyz), bound_tangent.w * side);
    }
    var out: VsOut;
    out.clip = frame.clip * vec4<f32>(position, 1.0);
    out.seen = (frame.view * vec4<f32>(position, 1.0)).xyz;
    out.normal = frame.normal * normal;
    let linear = mat3x3<f32>(frame.view[0].xyz, frame.view[1].xyz, frame.view[2].xyz);
    out.tangent = vec4<f32>(linear * tangent.xyz, tangent.w);
    out.uv = uv;
    out.color = color;
    return out;
}

const PI: f32 = 3.14159265;

// The studio environment: a cool sky above, a warm floor below, brightest just above
// the horizon. `dir` is in the viewing frame, so up is -y.
fn environment(dir: vec3<f32>) -> vec3<f32> {
    let up = clamp(-dir.y, -1.0, 1.0);
    let sky = mix(vec3<f32>(0.62, 0.64, 0.68), vec3<f32>(0.42, 0.47, 0.56), max(up, 0.0));
    let floor = mix(vec3<f32>(0.30, 0.28, 0.26), vec3<f32>(0.16, 0.15, 0.14), max(-up, 0.0));
    return select(floor, sky, up >= 0.0);
}

// `dir`, in the viewing frame, as a direction in the sky's own frame: the environment's
// axes are the camera's with the sky's turn, tilt and roll undone (ADR-0182). x right,
// y down, z toward where the camera looks from at rest.
fn sky_dir(dir: vec3<f32>) -> vec3<f32> {
    let e = frame.environment;
    return normalize(e.right.xyz * dir.x + e.down.xyz * dir.y - e.forward.xyz * dir.z);
}

// The mip level a ring blur over `spread` (as `sky` takes it) reads of a panorama
// `width` texels around with `levels` levels (ADR-0170 decision 3): the one whose texels
// are about as wide as the ring's 8 taps are apart, `atan(spread)` out, so the taps
// together cover the ring rather than land as 9 sharp points. 0 for a single sample.
fn blur_level(spread: f32, width: f32, levels: u32) -> f32 {
    if spread <= 0.01 {
        return 0.0;
    }
    let gap = width * sin(atan(spread)) / 8.0;
    return clamp(log2(max(gap, 1.0)), 0.0, f32(levels - 1u));
}

// A panorama's texture coordinates along `d`, a scene direction, its centre ahead along
// -z, kept half a texel of a `height`-texel level inside its top and bottom rows, so a
// filtered read never wraps to the other pole.
fn panorama_uv(d: vec3<f32>, height: f32) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let edge = max(0.001, 0.5 / max(height, 1.0));
    let v = clamp(0.5 - asin(clamp(-d.y, -1.0, 1.0)) / PI, edge, 1.0 - edge);
    return vec2<f32>(u, v);
}

// The panorama's light along `d`, a scene direction, at mip level `level`.
fn panorama(d: vec3<f32>, level: f32) -> vec3<f32> {
    let height = f32(textureDimensions(sky_tex, u32(ceil(level))).y);
    let c = to_premul_linear(
        textureSampleLevel(sky_tex, sky_sampler, panorama_uv(d, height), level),
        frame.environment.ground.w,
    );
    return c.rgb * frame.environment.down.w;
}

// The scene's sky along `dir` (in the viewing frame), blurred over `spread`: 0 a single
// sample, 1 a ring about 45 degrees out.
fn sky(dir: vec3<f32>, spread: f32) -> vec3<f32> {
    let e = frame.environment;
    let d = sky_dir(dir);
    if e.ground.w < 0.0 {
        let up = clamp(-d.y, -1.0, 1.0);
        if up >= 0.0 {
            return mix(e.horizon.rgb, e.sky.rgb, sqrt(up));
        }
        return mix(e.horizon.rgb, e.ground.rgb, min(-up * 4.0, 1.0));
    }
    let level = blur_level(
        spread,
        f32(textureDimensions(sky_tex).x),
        textureNumLevels(sky_tex),
    );
    if spread <= 0.01 {
        return panorama(d, level);
    }
    var side = vec3<f32>(0.0, 1.0, 0.0);
    if abs(d.y) > 0.99 {
        side = vec3<f32>(1.0, 0.0, 0.0);
    }
    let t = normalize(cross(d, side));
    let b = cross(d, t);
    var sum = panorama(d, level);
    for (var k = 0u; k < 8u; k++) {
        let a = f32(k) * PI * 0.25;
        sum += panorama(normalize(d + spread * (t * cos(a) + b * sin(a))), level);
    }
    return sum / 9.0;
}

// The scene's sky as a matte surface facing `dir` (in the viewing frame) takes it: one
// read of the panorama one level above the one `sky` reads its widest ring at, where a
// filtered texel spans about what that ring's nine reads did together.
fn sky_matte(dir: vec3<f32>) -> vec3<f32> {
    let e = frame.environment;
    if e.ground.w < 0.0 {
        return sky(dir, 1.0);
    }
    let levels = textureNumLevels(sky_tex);
    let level = blur_level(1.0, f32(textureDimensions(sky_tex).x), levels) + 1.0;
    return panorama(sky_dir(dir), min(level, f32(levels - 1u)));
}

// The probe's capture along `d`, a direction in the scene's axes, at mip level
// `level`: premultiplied, its alpha where the capture saw something.
fn probe_panorama(d: vec3<f32>, level: f32) -> vec4<f32> {
    let height = f32(textureDimensions(probe_tex, u32(ceil(level))).y);
    return to_premul_linear(
        textureSampleLevel(probe_tex, sky_sampler, panorama_uv(d, height), level),
        frame.probe.fade.y,
    );
}

// The probe's capture along `d`, blurred over `spread` as `sky` blurs the sky.
fn probe_light(d: vec3<f32>, spread: f32) -> vec4<f32> {
    let level = blur_level(
        spread,
        f32(textureDimensions(probe_tex).x),
        textureNumLevels(probe_tex),
    );
    if spread <= 0.01 {
        return probe_panorama(d, level);
    }
    var side = vec3<f32>(0.0, 1.0, 0.0);
    if abs(d.y) > 0.99 {
        side = vec3<f32>(1.0, 0.0, 0.0);
    }
    let t = normalize(cross(d, side));
    let b = cross(d, t);
    var sum = probe_panorama(d, level);
    for (var k = 0u; k < 8u; k++) {
        let a = f32(k) * PI * 0.25;
        sum += probe_panorama(normalize(d + spread * (t * cos(a) + b * sin(a))), level);
    }
    return sum / 9.0;
}

// How much of the probe a fragment at `seen` takes, 0 outside its box, rising to 1
// `fade` inside its faces.
fn probe_weight(local: vec3<f32>) -> f32 {
    let p = frame.probe;
    let half = vec3<f32>(p.x.w, p.y.w, p.z.w);
    let inside = half - abs(local);
    let nearest = min(inside.x, min(inside.y, inside.z));
    if nearest <= 0.0 {
        return 0.0;
    }
    return clamp(nearest / max(p.fade.x, 1e-3), 0.0, 1.0);
}

// `dir` (in the viewing frame) from `local` (in the box's frame), as the direction in the
// scene's axes from the box's centre to where it meets the box.
fn probe_dir(local: vec3<f32>, dir: vec3<f32>) -> vec3<f32> {
    let p = frame.probe;
    let d = vec3<f32>(dot(dir, p.x.xyz), dot(dir, p.y.xyz), dot(dir, p.z.xyz));
    let half = vec3<f32>(p.x.w, p.y.w, p.z.w);
    let safe = select(d, vec3<f32>(1e-6), abs(d) < vec3<f32>(1e-6));
    let far = max((half - local) / safe, (-half - local) / safe);
    let t = max(min(far.x, min(far.y, far.z)), 0.0);
    return normalize(local + d * t);
}

// What a surface at `seen` reflects along `dir` of its probe (ADR-0170 decision 5).
struct Probed {
    // The capture's light along the ray, premultiplied by its alpha.
    found: vec4<f32>,
    // How much of the probe the surface takes: 0 outside its box or without one.
    weight: f32,
}

fn probe_of(seen: vec3<f32>, dir: vec3<f32>, roughness: f32) -> Probed {
    let p = frame.probe;
    if p.centre.w < 0.5 {
        return Probed(vec4<f32>(0.0), 0.0);
    }
    let off = seen - p.centre.xyz;
    let local = vec3<f32>(dot(off, p.x.xyz), dot(off, p.y.xyz), dot(off, p.z.xyz));
    let weight = probe_weight(local);
    if weight <= 0.0 {
        return Probed(vec4<f32>(0.0), 0.0);
    }
    return Probed(probe_light(probe_dir(local, dir), roughness * roughness), weight);
}

// Whether `probe` leaves nothing of the environment's reflection to show: the surface
// is wholly its probe's and the capture saw something all along the ray, to a thousandth.
// The environment then need not be read at all.
fn probe_covers(probe: Probed) -> bool {
    return probe.weight >= 1.0 && probe.found.a >= 0.999;
}

// The light a surface reflects: its probe's capture where it has one, over `env`, the
// environment's, scaled by the ambient light, where it saw nothing, and `env` alone
// outside every box.
fn probed(probe: Probed, env: vec3<f32>) -> vec3<f32> {
    if probe.weight <= 0.0 {
        return env;
    }
    return mix(env, probe.found.rgb + (1.0 - probe.found.a) * env, probe.weight);
}

fn ggx(n_dot_h: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-6);
}

fn smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    let k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    let gv = n_dot_v / (n_dot_v * (1.0 - k) + k);
    let gl = n_dot_l / (n_dot_l * (1.0 - k) + k);
    return gv * gl;
}

fn fresnel(f0: vec3<f32>, v_dot_h: f32) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);
}

// One light's reflected radiance, for irradiance `strength` arriving along `l`.
fn light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, strength: f32, diffuse: vec3<f32>, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let n_dot_l = dot(n, l);
    if n_dot_l <= 0.0 {
        return vec3<f32>(0.0);
    }
    let h = normalize(l + v);
    let n_dot_v = max(dot(n, v), 1e-4);
    let alpha = max(roughness * roughness, 0.002);
    let f = fresnel(f0, max(dot(v, h), 0.0));
    let spec = ggx(max(dot(n, h), 0.0), alpha) * smith(n_dot_v, n_dot_l, roughness) * f
        / max(4.0 * n_dot_v * n_dot_l, 1e-4);
    let kd = (vec3<f32>(1.0) - f) * diffuse / PI;
    return (kd + spec) * strength * n_dot_l;
}

// One scene light's reflected radiance at `seen` (ADR-0143 decision 3): a point or spot
// light fades as (1 - (d / range)^2)^2 to nothing at its range, a spot light also over
// the outer part of its cone, and a directional light arrives along its aim unfaded.
fn scene_light(source: Light, seen: vec3<f32>, n: vec3<f32>, v: vec3<f32>, diffuse: vec3<f32>, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    var l = -source.aim.xyz;
    var strength = 1.0;
    if source.at.w < 1.5 {
        let to_light = source.at.xyz - seen;
        let d = length(to_light);
        if d <= 1e-4 {
            return vec3<f32>(0.0);
        }
        l = to_light / d;
        let range = source.aim.w;
        if range > 0.0 {
            let x = clamp(1.0 - (d / range) * (d / range), 0.0, 1.0);
            strength = x * x;
        }
        if source.at.w > 0.5 {
            let along = dot(-l, source.aim.xyz);
            let outer = source.cone.x;
            let inner = source.cone.y;
            if inner - outer > 1e-5 {
                strength *= smoothstep(outer, inner, along);
            } else {
                strength *= select(0.0, 1.0, along >= outer);
            }
        }
    }
    if strength <= 0.0 {
        return vec3<f32>(0.0);
    }
    return light(n, v, l, strength, diffuse, f0, roughness) * source.color.rgb;
}

// The 1 / w shadow map `s` holds at `px`, 0 (nothing) outside it.
fn shadow_texel(s: u32, px: vec2<i32>) -> f32 {
    var size: vec2<u32>;
    if s == 0u {
        size = textureDimensions(shadow0);
    } else {
        size = textureDimensions(shadow1);
    }
    if any(px < vec2<i32>(0)) || any(px >= vec2<i32>(size)) {
        return 0.0;
    }
    if s == 0u {
        return textureLoad(shadow0, px, 0).r;
    }
    return textureLoad(shadow1, px, 0).r;
}

// How much of light slot `light` reaches `seen`, with the surface normal `n`
// (ADR-0151 decision 3): 1 for a light that casts no shadow. The point is pushed off
// the surface by a map pixel's size there, projected into the light's view, and
// compared with the map over a disc of 12 samples rotated per pixel: a sample is lit
// when nothing nearer to the light than the point (by a small bias) is stored there.
// Where the disc's middle and four points of its rim agree, they answer for the disc.
fn visibility(light: u32, seen: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> f32 {
    for (var s = 0u; s < MAX_SHADOWS; s++) {
        let sh = frame.shadows[s];
        if sh.at.w < 0.0 || u32(sh.at.w) != light {
            continue;
        }
        let focal = sh.right.w;
        let d0 = dot(seen - sh.at.xyz, sh.forward.xyz);
        if d0 <= 0.0 || focal <= 0.0 {
            return 1.0;
        }
        let p = seen + n * (1.5 * d0 / focal);
        let rel = p - sh.at.xyz;
        let d = dot(rel, sh.forward.xyz);
        if d <= 0.0 {
            return 1.0;
        }
        let at = vec2<f32>(dot(rel, sh.right.xyz), dot(rel, sh.down.xyz)) * focal / d
            + vec2<f32>(sh.down.w);
        let inverse_w = focal / d;
        let spin = fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))))
            * 6.2831853;
        // Five samples first, the middle and four on the rim: where they agree the point
        // is far from a shadow's edge, and the other samples would say the same.
        var first = 0.0;
        for (var k = 0u; k < 5u; k++) {
            let angle = f32(k) * 1.5707963 + spin;
            let r = select(sh.forward.w, 0.0, k == 4u);
            let tap = at + vec2<f32>(cos(angle), sin(angle)) * r;
            if shadow_texel(s, vec2<i32>(floor(tap))) <= inverse_w * 1.002 {
                first += 1.0;
            }
        }
        if first == 0.0 || first == 5.0 {
            return first / 5.0;
        }
        var lit = 0.0;
        for (var k = 0u; k < 12u; k++) {
            let r = sqrt((f32(k) + 0.5) / 12.0) * sh.forward.w;
            let angle = f32(k) * 2.3999632 + spin;
            let tap = at + vec2<f32>(cos(angle), sin(angle)) * r;
            let stored = shadow_texel(s, vec2<i32>(floor(tap)));
            if stored <= inverse_w * 1.002 {
                lit += 1.0;
            }
        }
        return lit / 12.0;
    }
    return 1.0;
}

// Karis's analytic fit of the split-sum environment BRDF.
fn env_brdf(f0: vec3<f32>, roughness: f32, n_dot_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_dot_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

// What `fs` draws at a fragment, and what a screen-space reflection (ADR-0170 decision
// 4) reads of it: the shading normal in the viewing frame, the roughness, the split-sum
// BRDF the environment's reflection was weighed by, and that reflection as it was added,
// both times the occlusion. `reflects` is 1 where a scene-lit surface reflected its
// environment, which a reflection found on screen may replace.
struct Shaded {
    color: vec4<f32>,
    n: vec3<f32>,
    roughness: f32,
    brdf: vec3<f32>,
    reflected: vec3<f32>,
    reflects: f32,
}

fn shade(in: VsOut, front_facing: bool) -> Shaded {
    var out: Shaded;
    out.n = vec3<f32>(0.0);
    out.roughness = 1.0;
    out.brdf = vec3<f32>(0.0);
    out.reflected = vec3<f32>(0.0);
    out.reflects = 0.0;
    if below_floor(in.seen) {
        discard;
    }
    let facing = front_facing != (frame.params.y > 0.5);
    let double_sided = material.flags.z > 0.5;
    if !facing && !double_sided {
        discard;
    }
    var texel = textureSample(base_tex, tex_sampler, in.uv);
    let card = material.card.x > 0.5;
    if card || material.card.y > 0.5 {
        // A picture is premultiplied: its colour is what it holds over its alpha.
        texel = vec4<f32>(select(vec3<f32>(0.0), texel.rgb / texel.a, texel.a > 1e-6), texel.a);
    }
    var base = material.base * texel * in.color;
    let mode = material.flags.x;
    if mode > 0.5 && mode < 1.5 {
        if base.a < material.flags.y {
            discard;
        }
        base.a = 1.0;
    } else if mode < 0.5 {
        base.a = 1.0;
    }
    var emitted = material.emissive.rgb * textureSample(emissive_tex, detail_sampler, in.uv).rgb;
    if card {
        emitted = material.emissive.rgb * base.rgb;
    }
    var color: vec3<f32>;
    if frame.params.z > 0.5 || material.flags.w > 0.5 {
        color = base.rgb + emitted;
    } else {
        let surface = textureSample(surface_tex, detail_sampler, in.uv);
        let metallic = clamp(material.surface.x * surface.b, 0.0, 1.0);
        let roughness = clamp(material.surface.y * surface.g, 0.03, 1.0);
        var n = normalize(in.normal);
        var t = in.tangent.xyz - n * dot(n, in.tangent.xyz);
        // A back face turns its whole frame over, tangent with normal, so a normal map
        // tilts the back of a surface as it does the front, seen from the other side.
        if !facing {
            n = -n;
            t = -t;
        }
        // No normal map: the interpolated normal as it is, so a flat surface stays flat.
        if material.emissive.a > 0.5 && dot(t, t) > 1e-12 {
            t = normalize(t);
            // The bitangent is the model's turned by a mirrored map and turned over on a
            // back face. cross(n, t) misses both: a mirror reverses a cross product, and
            // n and t turned over together leave it as it was. The raw front_facing is
            // false with either one alone and true with both, so its sign covers them.
            let b = cross(n, t) * in.tangent.w * select(-1.0, 1.0, front_facing);
            var m = textureSample(normal_tex, detail_sampler, in.uv).xyz * 2.0 - vec3<f32>(1.0);
            m = vec3<f32>(m.xy * material.surface.z, m.z);
            // The scene's y is down, glTF's normal maps are y-up in texture space: the
            // texture's v runs down, so its green, which points up the texture, is -b.
            n = normalize(t * m.x - b * m.y + n * m.z);
        }
        var v = vec3<f32>(0.0, 0.0, 1.0);
        if frame.params.x > 0.0 {
            v = normalize(vec3<f32>(0.0, 0.0, frame.params.x) - in.seen);
        }
        let diffuse = base.rgb * (1.0 - metallic);
        let f0 = mix(vec3<f32>(0.04), base.rgb, metallic);
        var lit = vec3<f32>(0.0);
        var ambient = vec3<f32>(1.0);
        if frame.params.w > 0.5 {
            ambient = frame.ambient.rgb;
            let count = min(u32(frame.ambient.w), MAX_LIGHTS);
            var surface_n = normalize(in.normal);
            if !facing {
                surface_n = -surface_n;
            }
            for (var i = 0u; i < count; i++) {
                let seen_by = visibility(i, in.seen, surface_n, in.clip.xy);
                if seen_by > 0.0 {
                    lit += seen_by * scene_light(frame.lights[i], in.seen, n, v, diffuse, f0, roughness);
                }
            }
        } else {
            lit += light(n, v, normalize(vec3<f32>(-0.45, -0.65, 0.62)), 2.6, diffuse, f0, roughness);
            lit += light(n, v, normalize(vec3<f32>(0.75, -0.1, 0.65)), 0.7, diffuse, f0, roughness);
        }
        let n_dot_v = max(dot(n, v), 1e-4);
        let brdf = env_brdf(f0, roughness, n_dot_v);
        var reflected: vec3<f32>;
        let r = reflect(-v, n);
        // The gradient is smooth, so a rough reflection samples it nearer the normal.
        let blurred = normalize(mix(r, n, roughness * roughness));
        if frame.environment.sky.w > 0.5 {
            // The scene's sky, or the picture the model reflects (ADR-0107 decision 5),
            // scaled by the scene's ambient light when it has one.
            if frame.environment.right.w < 0.5 {
                ambient = vec3<f32>(1.0);
            }
            lit += ambient * diffuse * sky_matte(n);
            let probe = probe_of(in.seen, blurred, roughness);
            var env = vec3<f32>(0.0);
            if !probe_covers(probe) {
                env = ambient * sky(blurred, roughness * roughness);
            }
            reflected = probed(probe, env) * brdf;
        } else {
            lit += ambient * diffuse * environment(n);
            let probe = probe_of(in.seen, blurred, roughness);
            reflected = probed(probe, ambient * environment(blurred)) * brdf;
        }
        lit += reflected;
        let occlusion = mix(1.0, textureSample(occlusion_tex, detail_sampler, in.uv).r, material.surface.w);
        color = lit * occlusion + emitted;
        out.n = n;
        out.roughness = roughness;
        out.brdf = brdf * occlusion;
        out.reflected = reflected * occlusion;
        out.reflects = select(0.0, 1.0, frame.params.w > 0.5 && mode < 1.5);
    }
    out.color = vec4<f32>(color * base.a, base.a);
    return out;
}

@fragment
fn fs(in: VsOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    return shade(in, front_facing).color;
}

// The opaque primitives' depth prepass: what `fs` keeps, without its shading, so the
// shading pass after it shades each pixel once, at the nearest surface, however many
// surfaces cover it.
@fragment
fn fs_prepass(in: VsOut, @builtin(front_facing) front_facing: bool) {
    if below_floor(in.seen) {
        discard;
    }
    let facing = front_facing != (frame.params.y > 0.5);
    if !facing && material.flags.z <= 0.5 {
        discard;
    }
    let mode = material.flags.x;
    if mode > 0.5 && mode < 1.5 {
        // As `fs` masks: a picture's unpremultiplying leaves its alpha as it was.
        let alpha = material.base.a * textureSample(base_tex, tex_sampler, in.uv).a * in.color.a;
        if alpha < material.flags.y {
            discard;
        }
    }
}

// The model's view depth (ADR-0143 decision 4): red holds 1 / w, the viewer's distance
// over the fragment's along the viewing axis, where the material's alpha is at least one
// half. The pipeline keeps the larger value, so the nearest thing wins. Only a model seen
// with perspective (a camera) is drawn here.
@fragment
fn fs_depth(in: VsOut) -> @location(0) vec4<f32> {
    if below_floor(in.seen) {
        discard;
    }
    let base = material.base * textureSample(base_tex, tex_sampler, in.uv) * in.color;
    let mode = material.flags.x;
    var alpha = 1.0;
    if mode > 1.5 {
        alpha = base.a;
    } else if mode > 0.5 && base.a < material.flags.y {
        alpha = 0.0;
    }
    let p = frame.params.x;
    let w = (p - in.seen.z) / max(p, 1e-6);
    if alpha < 0.5 || p <= 0.0 || w <= 0.0 {
        discard;
    }
    return vec4<f32>(1.0 / w, 0.0, 0.0, 1.0);
}

// What a screen-space reflection reads of the nearest opaque surface (ADR-0170 decision
// 4), where `fs_depth` would write its view depth: the shading normal in the viewing
// frame and 1 / w; the BRDF and the roughness; the environment's reflection as `fs`
// added it and whether one found on screen may replace it. All zero where nothing
// that may be replaced is: past the camera, or under half opaque.
struct Surface {
    normal: vec4<f32>,
    brdf: vec4<f32>,
    reflected: vec4<f32>,
}

fn surface_of(shaded: Shaded, in: VsOut) -> Surface {
    var out: Surface;
    let p = frame.params.x;
    let w = (p - in.seen.z) / max(p, 1e-6);
    if shaded.color.a < 0.5 || p <= 0.0 || w <= 0.0 {
        out.normal = vec4<f32>(0.0);
        out.brdf = vec4<f32>(0.0);
        out.reflected = vec4<f32>(0.0);
        return out;
    }
    out.normal = vec4<f32>(shaded.n, 1.0 / w);
    out.brdf = vec4<f32>(shaded.brdf, shaded.roughness);
    out.reflected = vec4<f32>(shaded.reflected, shaded.reflects);
    return out;
}

// The depth a surface buffer keeps the nearest by: 1 / w, nearer larger, squeezed into
// 0..1. `surface_merge.wgsl` writes the same.
fn surface_depth(inv: f32) -> f32 {
    return inv / (1.0 + inv);
}

// The model's layer and its surface at once (ADR-0170 decision 4), so a scene whose
// screen-space reflections read the surface doesn't shade its models twice.
struct LayerAndSurface {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
    @location(2) brdf: vec4<f32>,
    @location(3) reflected: vec4<f32>,
}

@fragment
fn fs_with_surface(in: VsOut, @builtin(front_facing) front_facing: bool) -> LayerAndSurface {
    let shaded = shade(in, front_facing);
    let surface = surface_of(shaded, in);
    var out: LayerAndSurface;
    out.color = shaded.color;
    out.normal = surface.normal;
    out.brdf = surface.brdf;
    out.reflected = surface.reflected;
    return out;
}

// A model drawn into a surface buffer on its own, where its layer had no surface: the
// pipeline tests and writes `surface_depth`, so the nearest surface of every model
// drawn into the targets wins.
struct SurfaceOut {
    @location(0) normal: vec4<f32>,
    @location(1) brdf: vec4<f32>,
    @location(2) reflected: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs_surface(in: VsOut, @builtin(front_facing) front_facing: bool) -> SurfaceOut {
    let surface = surface_of(shade(in, front_facing), in);
    if surface.normal.w <= 0.0 {
        discard;
    }
    var out: SurfaceOut;
    out.normal = surface.normal;
    out.brdf = surface.brdf;
    out.reflected = surface.reflected;
    out.depth = surface_depth(surface.normal.w);
    return out;
}
