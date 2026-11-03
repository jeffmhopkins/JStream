// Noise helpers for shader sources: hashes and value noise, the same on every adapter.

// A hash of `p` in 0..1.
fn hash12(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(123.34, 456.21));
    q = q + dot(q, q + 45.32);
    return fract(q.x * q.y);
}

// Two hashes of `p` in 0..1.
fn hash22(p: vec2<f32>) -> vec2<f32> {
    let a = hash12(p);
    return vec2<f32>(a, hash12(p + a + 17.0));
}

// Smooth value noise in 0..1, one cell per unit.
fn noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Fractal noise in 0..1: `octaves` layers of `noise` (at most 8), each at twice the
// frequency and half the weight of the one before.
fn fbm(p: vec2<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var weight = 0.5;
    var total = 0.0;
    var q = p;
    for (var i = 0; i < min(octaves, 8); i = i + 1) {
        sum = sum + weight * noise(q);
        total = total + weight;
        q = q * 2.0 + vec2<f32>(17.3, 9.1);
        weight = weight * 0.5;
    }
    return select(0.0, sum / total, total > 0.0);
}
