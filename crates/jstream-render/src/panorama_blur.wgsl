// A panorama's blurred reflections, made once per build of its mip chain (ADR-0170
// decision 3) rather than on every fragment that reflects it.
//
// Level `m` of the output is what `model.wgsl`'s `sky` and `probe_light` would read at a
// spread whose `blur_level` is exactly `m`: the 9-tap ring around each texel's direction
// at that spread, every tap read at level `m` of the chain. That spread is the one whose
// taps are `2^m` level-0 texels apart, `sin(atan(spread)) = 8 * 2^m / width`, capped at 1
// (the widest ring a surface reads, which `sky_matte` reads a level coarser). Level 0 is
// the chain's level 0, unblurred, so a mirror stays sharp. A surface then reads one
// trilinear sample at `blur_level` where it read nine, between two levels of rings next
// to its own.
//
// A pass draws one level, which its uniform names.

const PI: f32 = 3.14159265358979;

@group(0) @binding(0) var chain: texture_2d<f32>;
@group(0) @binding(1) var chain_sampler: sampler;
// The level this pass draws, in `x`.
@group(0) @binding(2) var<uniform> target_level: vec4<u32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    var out: VsOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    return out;
}

// As `model.wgsl`'s `panorama_uv`: a scene direction's coordinates, kept half a texel
// of a `height`-texel level inside its top and bottom rows.
fn panorama_uv(d: vec3<f32>, height: f32) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let edge = max(0.001, 0.5 / max(height, 1.0));
    let v = clamp(0.5 - asin(clamp(-d.y, -1.0, 1.0)) / PI, edge, 1.0 - edge);
    return vec2<f32>(u, v);
}

// The direction `panorama_uv` maps to `uv`.
fn panorama_dir(uv: vec2<f32>) -> vec3<f32> {
    let around = (uv.x - 0.5) * 2.0 * PI;
    let up = (0.5 - uv.y) * PI;
    return vec3<f32>(cos(up) * sin(around), -sin(up), -cos(up) * cos(around));
}

// The spread whose ring `blur_level` reads at `level` of a chain `width` texels wide.
fn level_spread(level: u32, width: f32) -> f32 {
    let x = min(8.0 * exp2(f32(level)) / width, sqrt(0.5));
    return x / sqrt(1.0 - x * x);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let at = target_level.x;
    let size = vec2<f32>(textureDimensions(chain, at));
    if at == 0u {
        return textureLoad(chain, vec2<i32>(in.clip.xy), 0);
    }
    let d = panorama_dir(in.clip.xy / size);
    let spread = level_spread(at, f32(textureDimensions(chain).x));
    let level = f32(at);
    var side = vec3<f32>(0.0, 1.0, 0.0);
    if abs(d.y) > 0.99 {
        side = vec3<f32>(1.0, 0.0, 0.0);
    }
    let t = normalize(cross(d, side));
    let b = cross(d, t);
    var sum = textureSampleLevel(chain, chain_sampler, panorama_uv(d, size.y), level);
    for (var k = 0u; k < 8u; k++) {
        let a = f32(k) * PI * 0.25;
        let tap = normalize(d + spread * (t * cos(a) + b * sin(a)));
        sum += textureSampleLevel(chain, chain_sampler, panorama_uv(tap, size.y), level);
    }
    return sum / 9.0;
}
