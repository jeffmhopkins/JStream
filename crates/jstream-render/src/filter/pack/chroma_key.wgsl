// Chroma key (OBS's v2): distance in BT.709 limited-range CbCr from the key colour,
// box-filtered over 3x3 texels, then a smooth ramp and spill desaturation.

struct Params {
    // The key colour's (Cb, Cr), without the 128/255 offset (it cancels).
    key: vec2<f32>,
    similarity: f32,
    smoothness: f32,
    spill: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn chroma(rgb: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(
        dot(rgb, vec3<f32>(-0.100644, -0.338572, 0.439216)),
        dot(rgb, vec3<f32>(0.439216, -0.398942, -0.040274)),
    );
}

// x^1.5 without pow, which is undefined at 0 on some backends.
fn pow15(x: f32) -> f32 {
    return x * sqrt(x);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = here(in);
    let c = encoded_at(px);
    var dist = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            dist += distance(chroma(encoded_at(px + vec2<i32>(dx, dy)).rgb), p.key);
        }
    }
    let base = dist / 9.0 - p.similarity;
    let mask = pow15(saturate(base / p.smoothness));
    let spill = pow15(saturate(base / p.spill));
    let grey = luma(c.rgb);
    return keyed(mix(vec3<f32>(grey), c.rgb, spill), c.a * mask);
}
