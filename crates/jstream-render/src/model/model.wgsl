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
// environment is scaled by the scene's ambient light.

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
    // A floor the model is cut at (ADR-0151 decision 4): a fragment whose dot(xyz, seen)
    // + w is at or above 0 is at or below it, and isn't drawn. (0, 0, 0, -1) cuts nothing.
    cut: vec4<f32>,
}

fn below_floor(seen: vec3<f32>) -> bool {
    return dot(frame.cut.xyz, seen) + frame.cut.w >= 0.0;
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
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow0: texture_2d<f32>;
@group(0) @binding(2) var shadow1: texture_2d<f32>;
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var tex_sampler: sampler;
@group(1) @binding(2) var base_tex: texture_2d<f32>;
@group(1) @binding(3) var surface_tex: texture_2d<f32>;
@group(1) @binding(4) var normal_tex: texture_2d<f32>;
@group(1) @binding(5) var occlusion_tex: texture_2d<f32>;
@group(1) @binding(6) var emissive_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) seen: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) color: vec4<f32>,
}

@vertex
fn vs(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) color: vec4<f32>,
) -> VsOut {
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

@fragment
fn fs(in: VsOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    if below_floor(in.seen) {
        discard;
    }
    let facing = front_facing != (frame.params.y > 0.5);
    let double_sided = material.flags.z > 0.5;
    if !facing && !double_sided {
        discard;
    }
    var base = material.base * textureSample(base_tex, tex_sampler, in.uv) * in.color;
    let mode = material.flags.x;
    if mode > 0.5 && mode < 1.5 {
        if base.a < material.flags.y {
            discard;
        }
        base.a = 1.0;
    } else if mode < 0.5 {
        base.a = 1.0;
    }
    let emitted = material.emissive.rgb * textureSample(emissive_tex, tex_sampler, in.uv).rgb;
    var color: vec3<f32>;
    if frame.params.z > 0.5 || material.flags.w > 0.5 {
        color = base.rgb + emitted;
    } else {
        let surface = textureSample(surface_tex, tex_sampler, in.uv);
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
            var m = textureSample(normal_tex, tex_sampler, in.uv).xyz * 2.0 - vec3<f32>(1.0);
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
        let r = reflect(-v, n);
        // The gradient is smooth, so a rough reflection samples it nearer the normal.
        let blurred = normalize(mix(r, n, roughness * roughness));
        lit += ambient * (diffuse * environment(n) + environment(blurred) * env_brdf(f0, roughness, n_dot_v));
        let occlusion = mix(1.0, textureSample(occlusion_tex, tex_sampler, in.uv).r, material.surface.w);
        color = lit * occlusion + emitted;
    }
    return vec4<f32>(color * base.a, base.a);
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
