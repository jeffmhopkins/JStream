// Scanlines: the CRT and monitor-screen look (#1406). Horizontal dark lines every
// `spacing` output pixels, each `thickness` of its period, rolling down by `roll`
// pixels, darken the picture by up to `strength` in linear light. `rgb_offset` reads red
// that many pixels to the left and blue to the right, a CRT's colour fringe. `grain`
// adds noise that changes every frame, seeded on the CPU from the animation time, and
// `flicker` dims the whole picture by the CPU's random amount for this step. Coverage is
// kept: a transparent pixel stays transparent, and grain is scaled by alpha.

struct Params {
    spacing: f32,
    thickness: f32,
    strength: f32,
    // Pixels the lines have rolled, already wrapped to `spacing`.
    roll: f32,
    rgb_offset: f32,
    grain: f32,
    // The noise's seed for this frame.
    seed: f32,
    // How much this step dims, `0..1`.
    dim: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

// A hash of a pixel and the seed in `0..1`.
fn noise(px: vec2<f32>, seed: f32) -> f32 {
    let q = px + vec2<f32>(seed * 17.13, seed * 3.71);
    return fract(sin(dot(q, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// How much of the output row centred at `y` the dark part of a line covers.
fn line(y: f32) -> f32 {
    let spacing = max(p.spacing, 1.0);
    let dark = p.thickness * spacing;
    // The pixel spans [y - 0.5, y + 0.5) in the period's frame.
    let a = (y - 0.5 - p.roll) - floor((y - 0.5 - p.roll) / spacing) * spacing;
    // Overlap of [a, a + 1) with [0, dark) and with [spacing, spacing + dark).
    let first = max(0.0, min(a + 1.0, dark) - a);
    let second = max(0.0, min(a + 1.0, spacing + dark) - max(a, spacing));
    return clamp(first + second, 0.0, 1.0);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    var rgb = c.rgb;
    if (p.rgb_offset > 0.0) {
        let dx = vec2<f32>(p.rgb_offset / frame.in_size.x, 0.0);
        rgb = vec3<f32>(src(in.uv - dx).r, c.g, src(in.uv + dx).b);
        // Keep premultiplied colour inside its own coverage.
        rgb = min(rgb, vec3<f32>(c.a));
    }
    rgb = rgb * (1.0 - p.strength * line(in.pos.y));
    if (p.grain > 0.0) {
        let n = noise(floor(in.pos.xy), p.seed) - 0.5;
        rgb = max(rgb + vec3<f32>(n * p.grain * 0.5 * c.a), vec3<f32>(0.0));
        rgb = min(rgb, vec3<f32>(c.a));
    }
    rgb = rgb * (1.0 - p.dim);
    return vec4<f32>(rgb, c.a);
}
