// The deep sky's bake (ADR-0174): a sky backdrop's nebulae and galaxy band (ADR-0167
// decision 4) for one seed, drawn into one face of two cube maps that `sky.wgsl` then
// reads along each pixel's direction. They are dozens of noise octaves a direction, and
// depend only on the direction and the seed, so they are worked out once per seed here
// and not for every pixel of every frame.
//
// What is stored leaves out everything that is a setting: how much nebula, its two
// colours, how bright the band is and the light's intensity all multiply what is read,
// so changing them needs no new bake. Out 0, the nebulae, is four weights: the light of
// the first colour, of the second, and of each one's sum of channels (the pale heart
// where the gas is thickest). Out 1, the band, is its light in red.

struct Face {
    // xyz: the direction a step right across the face adds; w: two over the face's size.
    right: vec4<f32>,
    // xyz: the direction a step down it adds.
    down: vec4<f32>,
    // xyz: the direction at its centre; w: the seed, as u32 bits.
    forward: vec4<f32>,
}

@group(0) @binding(0) var<uniform> face: Face;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn seed() -> u32 {
    return bitcast<u32>(face.forward.w);
}

// ---- Nebulae and the galaxy band: smooth 3D value noise on the direction. ----

fn lattice(i: vec3<i32>) -> f32 {
    let h = hash(bitcast<u32>(i.x) ^ hash(bitcast<u32>(i.y) ^ hash(bitcast<u32>(i.z) ^ seed())));
    return f32(h & 0xffffffu) / 16777216.0;
}

fn noise(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    let f = fract(p);
    let w = f * f * (3.0 - 2.0 * f);
    let a = mix(lattice(i), lattice(i + vec3<i32>(1, 0, 0)), w.x);
    let b = mix(lattice(i + vec3<i32>(0, 1, 0)), lattice(i + vec3<i32>(1, 1, 0)), w.x);
    let c = mix(lattice(i + vec3<i32>(0, 0, 1)), lattice(i + vec3<i32>(1, 0, 1)), w.x);
    let e = mix(lattice(i + vec3<i32>(0, 1, 1)), lattice(i + vec3<i32>(1, 1, 1)), w.x);
    return mix(mix(a, b, w.y), mix(c, e, w.y), w.z);
}

fn fbm(p: vec3<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var k = 0; k < octaves; k++) {
        sum += amp * noise(q);
        q = q * 2.03 + vec3<f32>(17.1, 5.3, 11.7);
        amp *= 0.5;
    }
    return sum;
}

// The seed's style for its nebulae, each 0 to 1: how large, how much of the sky, how
// turbulent, how filamentary, how dusty and how gathered into a few regions they are.
struct Style {
    scale: f32,
    cover: f32,
    churn: f32,
    wisps: f32,
    dust: f32,
    gather: f32,
}

fn style() -> Style {
    let k = hash(seed() ^ 0x5bd1e995u);
    return Style(unit(k ^ 0x1u), unit(k ^ 0x2u), unit(k ^ 0x3u), unit(k ^ 0x4u), unit(k ^ 0x5u), unit(k ^ 0x6u));
}

// The seed's nebulae and galaxy band along `d`, with every setting left out.
struct Deep {
    // The nebulae: the light of the first colour, of the second, and of each one's sum
    // of channels.
    @location(0) nebula: vec4<f32>,
    // The band's light, in red.
    @location(1) galaxy: vec4<f32>,
}

fn deep_sky(d: vec3<f32>) -> Deep {
    let look = style();
    // From a few great clouds across the sky to many small knots.
    let p = d * mix(1.3, 4.2, look.scale * look.scale);
    // Warped once, so the clouds billow and stream rather than blot; more for a
    // churning, torn-up sky.
    let q = vec3<f32>(fbm(p + vec3<f32>(1.7, 9.2, 3.1), 4), fbm(p + vec3<f32>(8.3, 2.8, 5.9), 4), fbm(p + vec3<f32>(4.1, 6.6, 0.4), 4));
    let n = fbm(p + mix(0.9, 3.6, look.churn) * q, 5);
    // How much of the sky the gas fills: thin wisps to a full cloudscape.
    let edge = mix(0.56, 0.36, look.cover);
    var density = smoothstep(edge, edge + 0.34, n);
    // Gathered into a few regions, with open sky between, or spread everywhere.
    let region = smoothstep(0.38, 0.62, fbm(d * 1.1 + vec3<f32>(5.5, 2.2, 8.8), 3));
    density *= mix(1.0, region * 1.6, look.gather);
    // Bright filaments where the cloud folds, from soft edges to a veil of threads.
    let fold = pow(1.0 - abs(2.0 * fbm(p * 2.1 + 3.0 * q, 4) - 1.0), mix(10.0, 5.0, look.wisps));
    let threads = fold * mix(0.35, 1.4, look.wisps) * max(density, look.wisps * 0.35);
    // How far from the first colour to the second.
    let second = smoothstep(0.3, 0.7, q.x);
    // A hot, paler heart where the gas is thickest: 0.45 of the way from the colour to
    // 0.6 of it plus 0.4 of its sum of channels as grey, so 0.18 of each.
    let heart = 0.18 * smoothstep(0.75, 1.0, density);
    // Dark dust that eats the glow in lanes and blots.
    let dark = smoothstep(0.5, 0.7, fbm(p * 1.7 + vec3<f32>(9.1, 4.4, 2.6) + q, 4)) * look.dust;
    let light = (0.6 * density * density + 0.5 * threads + 0.05 * n) * (1.0 - 0.9 * dark);
    let nebula = light * vec4<f32>(
        (1.0 - second) * (1.0 - heart),
        second * (1.0 - heart),
        (1.0 - second) * heart,
        second * heart,
    );

    let b = dot(d, galaxy_axis());
    let band = exp(-b * b / 0.018);
    let core = pow(max(dot(normalize(d - galaxy_axis() * b), galaxy_core()), 0.0), 3.0);
    let glow = band * (0.35 + 0.65 * fbm(d * 7.0, 4)) * (0.35 + 1.4 * core);
    let lane = smoothstep(0.48, 0.68, fbm(d * 11.0 + vec3<f32>(3.3, 1.1, 7.7), 4)) * exp(-b * b / 0.0035);
    return Deep(nebula, vec4<f32>(glow * (1.0 - 0.85 * lane), 0.0, 0.0, 1.0));
}

fn galaxy_core() -> vec3<f32> {
    return normalize(cross(galaxy_axis(), vec3<f32>(0.0, 0.0, 1.0)));
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> Deep {
    // The texel's centre across the face, -1 to 1 each way.
    let at = p.xy * face.right.w - 1.0;
    return deep_sky(normalize(face.right.xyz * at.x + face.down.xyz * at.y + face.forward.xyz));
}
