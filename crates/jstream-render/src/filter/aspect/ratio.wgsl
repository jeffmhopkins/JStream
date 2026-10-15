// aspect_ratio: crop or pad map output pixels 1:1 onto input pixels with an offset;
// stretch resamples the whole input onto the output.

struct Params {
    // Pad colour, premultiplied linear.
    pad: vec4<f32>,
    // Input pixel = output pixel + offset (crop: >= 0; pad: <= 0).
    offset: vec2<i32>,
    // 0: crop or pad (1:1), 1: stretch.
    mode: u32,
    // Stretch only. 0: bilinear, 1: bicubic (Catmull-Rom), 2: lanczos-3.
    resampling: u32,
}

@group(0) @binding(4) var<uniform> p: Params;

const PI: f32 = 3.14159265;

// Catmull-Rom (B = 0, C = 0.5): interpolating, so a 1:1 resample is the identity.
fn catmull_rom(x: f32) -> f32 {
    let a = abs(x);
    if (a < 1.0) {
        return (1.5 * a - 2.5) * a * a + 1.0;
    }
    if (a < 2.0) {
        return ((-0.5 * a + 2.5) * a - 4.0) * a + 2.0;
    }
    return 0.0;
}

// Lanczos with a = 3.
fn lanczos3(x: f32) -> f32 {
    let a = abs(x);
    if (a < 1e-5) {
        return 1.0;
    }
    if (a >= 3.0) {
        return 0.0;
    }
    let px = PI * a;
    return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}

fn kernel(x: f32) -> f32 {
    if (p.resampling == 1u) {
        return catmull_rom(x);
    }
    return lanczos3(x);
}

// A separable kernel of `radius` texels around the output pixel's centre, mapped into the
// input, with clamp-to-edge texel loads. The result is normalised by its weight sum
// (Lanczos weights don't sum to exactly 1) and clamped to a valid premultiplied colour,
// which cuts the kernels' overshoot at hard edges.
fn resample(pos: vec2<f32>, radius: i32) -> vec4<f32> {
    let t = pos * frame.in_size / frame.out_size - vec2<f32>(0.5);
    let base = floor(t);
    let f = t - base;
    let origin = vec2<i32>(base);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var j = 1 - radius; j <= radius; j = j + 1) {
        let wy = kernel(f.y - f32(j));
        for (var i = 1 - radius; i <= radius; i = i + 1) {
            let w = kernel(f.x - f32(i)) * wy;
            sum = sum + texel(origin + vec2<i32>(i, j)) * w;
            total = total + w;
        }
    }
    let c = sum / select(total, 1.0, abs(total) < 1e-6);
    let a = clamp(c.a, 0.0, 1.0);
    return vec4<f32>(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(a)), a);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if (p.mode == 0u) {
        let px = vec2<i32>(floor(in.pos.xy)) + p.offset;
        let size = vec2<i32>(frame.in_size);
        if (any(px < vec2<i32>(0)) || any(px >= size)) {
            return p.pad;
        }
        return texel(px);
    }
    if (p.resampling == 0u) {
        return src(in.uv);
    }
    if (p.resampling == 1u) {
        return resample(in.pos.xy, 2);
    }
    return resample(in.pos.xy, 3);
}
