// A reflection probe's capture (ADR-0170 decision 3): six square faces of the scene, each
// drawn from the probe's centre through a 90-degree camera, resampled into one
// equirectangular panorama in the scene's axes, as `model.wgsl`'s `probe_panorama` reads
// it: its centre along -z, up (-y) at its top. Each texel takes the face its direction
// meets most squarely. In and out: premultiplied linear light, alpha where a face saw
// something.

struct Faces {
    // Per face: its right, down and forward axes in the scene's space.
    axes: array<mat3x3<f32>, 6>,
}

@group(0) @binding(0) var<uniform> u: Faces;
@group(0) @binding(1) var face_sampler: sampler;
@group(0) @binding(2) var face0: texture_2d<f32>;
@group(0) @binding(3) var face1: texture_2d<f32>;
@group(0) @binding(4) var face2: texture_2d<f32>;
@group(0) @binding(5) var face3: texture_2d<f32>;
@group(0) @binding(6) var face4: texture_2d<f32>;
@group(0) @binding(7) var face5: texture_2d<f32>;

const PI: f32 = 3.14159265;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn face(i: u32, uv: vec2<f32>) -> vec4<f32> {
    switch i {
        case 0u: { return textureSampleLevel(face0, face_sampler, uv, 0.0); }
        case 1u: { return textureSampleLevel(face1, face_sampler, uv, 0.0); }
        case 2u: { return textureSampleLevel(face2, face_sampler, uv, 0.0); }
        case 3u: { return textureSampleLevel(face3, face_sampler, uv, 0.0); }
        case 4u: { return textureSampleLevel(face4, face_sampler, uv, 0.0); }
        default: { return textureSampleLevel(face5, face_sampler, uv, 0.0); }
    }
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(face0)) * vec2<f32>(4.0, 2.0);
    let uv = at.xy / size;
    let lon = (uv.x - 0.5) * 2.0 * PI;
    let lat = (0.5 - uv.y) * PI;
    let d = vec3<f32>(cos(lat) * sin(lon), -sin(lat), -cos(lat) * cos(lon));
    var best = 0u;
    var most = -2.0;
    for (var i = 0u; i < 6u; i++) {
        let along = dot(d, u.axes[i][2]);
        if along > most {
            most = along;
            best = i;
        }
    }
    let axes = u.axes[best];
    let x = dot(d, axes[0]) / most;
    let y = dot(d, axes[1]) / most;
    return face(best, clamp(vec2<f32>(0.5 + 0.5 * x, 0.5 + 0.5 * y), vec2<f32>(0.0), vec2<f32>(1.0)));
}
