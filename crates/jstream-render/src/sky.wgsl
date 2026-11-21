// A 3D scene's sky backdrop (ADR-0167): the sky of its environment light seen through
// the camera, infinitely far away. Each pixel looks along its ray from the camera, so the
// sky turns with where the camera looks and never moves with where it is. The sky is the
// environment's gradient, or its picture as an equirectangular panorama, with procedural
// nebulae, a galaxy band and stars over it. Stars are laid out around the direction of
// travel, the sky's -z, so at warp each one streaks away from it along its own line, the
// same from any angle the camera looks. Out: premultiplied linear light, at the
// backdrop's opacity.

struct Sky {
    // xyz: the camera's right axis in the scene's space; w: the principal point's x.
    right: vec4<f32>,
    // xyz: its down axis; w: the principal point's y.
    down: vec4<f32>,
    // xyz: its forward axis; w: the focal length in canvas pixels.
    forward: vec4<f32>,
    // rgb: straight up, linear light times intensity; w: the backdrop's opacity.
    sky: vec4<f32>,
    // rgb: at the horizon; w: the turn about the vertical, in radians.
    horizon: vec4<f32>,
    // rgb: below the horizon; w: how the picture stores alpha, or -1 for none.
    ground: vec4<f32>,
    // x: how many stars, 0 to 1; y: their brightness times intensity; z: the picture's
    // scale, the intensity; w: the warp streak's length, in log-tan units.
    stars: vec4<f32>,
    // rgb: the nebulae's first colour, linear light times intensity; w: how much nebula.
    nebula: vec4<f32>,
    // rgb: their second colour; w: how bright the galaxy band is.
    nebula_2: vec4<f32>,
    // x: the travel's fraction of a bright cell; y: its whole bright cells, as u32 bits;
    // z: the same fraction of a faint cell; w: its whole faint cells, as u32 bits.
    travel: vec4<f32>,
    // x: the seed, as u32 bits; y: the warp's speed as a fraction of light's, which
    // crowds the stars ahead and shifts their light; zw: unused.
    seed: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Sky;
@group(0) @binding(1) var sky_tex: texture_2d<f32>;
@group(0) @binding(2) var sky_sampler: sampler;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
// Columns of stars around the direction of travel: faint, then bright. A cell is about
// 0.56 and 1.3 degrees square where it is square, at right angles to the travel.
const FAINT_COLUMNS: f32 = 640.0;
const BRIGHT_COLUMNS: f32 = 280.0;
// The most cells one streak is looked for in.
const MAX_STREAK_CELLS: i32 = 64;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// The sky's direction for the canvas pixel at `p`: its ray from the camera, in the
// scene's space (x right, y down, z toward the viewer), spun by the turn.
fn ray(p: vec2<f32>) -> vec3<f32> {
    let f = max(u.forward.w, 1e-3);
    let x = (p.x - u.right.w) / f;
    let y = (p.y - u.down.w) / f;
    let d = normalize(u.right.xyz * x + u.down.xyz * y + u.forward.xyz);
    let c = cos(u.horizon.w);
    let s = sin(u.horizon.w);
    return vec3<f32>(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
}

// The gradient along `d`, as a model's environment light reads it (ADR-0151 decision 9).
fn gradient(d: vec3<f32>) -> vec3<f32> {
    let up = clamp(-d.y, -1.0, 1.0);
    if up >= 0.0 {
        return mix(u.horizon.rgb, u.sky.rgb, sqrt(up));
    }
    return mix(u.horizon.rgb, u.ground.rgb, min(-up * 4.0, 1.0));
}

// The panorama along `d`: its centre ahead along -z, up at its top.
fn panorama(d: vec3<f32>) -> vec3<f32> {
    let uu = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let vv = clamp(0.5 - asin(clamp(-d.y, -1.0, 1.0)) / PI, 0.0005, 0.9995);
    let c = to_premul_linear(textureSampleLevel(sky_tex, sky_sampler, vec2<f32>(uu, vv), 0.0), u.ground.w);
    return c.rgb * u.stars.z;
}

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

fn unit(x: u32) -> f32 {
    return f32(hash(x) & 0xffffffu) / 16777216.0;
}

fn seed() -> u32 {
    return bitcast<u32>(u.seed.x);
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

// The nebulae and the galaxy band along `d`: light added to the sky.
fn deep_sky(d: vec3<f32>) -> vec3<f32> {
    var c = vec3<f32>(0.0);
    let nebula = u.nebula.w;
    if nebula > 0.0 {
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
        let tint = mix(u.nebula.rgb, u.nebula_2.rgb, smoothstep(0.3, 0.7, q.x));
        // A hot, paler heart where the gas is thickest.
        let heart = mix(tint, vec3<f32>(dot(tint, vec3<f32>(0.4))) + tint * 0.6, 0.45 * smoothstep(0.75, 1.0, density));
        // Dark dust that eats the glow in lanes and blots.
        let dark = smoothstep(0.5, 0.7, fbm(p * 1.7 + vec3<f32>(9.1, 4.4, 2.6) + q, 4)) * look.dust;
        c += heart * (0.6 * density * density + 0.5 * threads + 0.05 * n) * nebula * (1.0 - 0.9 * dark);
    }
    let galaxy = u.nebula_2.w;
    if galaxy > 0.0 {
        let b = dot(d, galaxy_axis());
        let band = exp(-b * b / 0.018);
        let core = pow(max(dot(normalize(d - galaxy_axis() * b), galaxy_core()), 0.0), 3.0);
        let glow = band * (0.35 + 0.65 * fbm(d * 7.0, 4)) * (0.35 + 1.4 * core);
        let lane = smoothstep(0.48, 0.68, fbm(d * 11.0 + vec3<f32>(3.3, 1.1, 7.7), 4)) * exp(-b * b / 0.0035);
        c += vec3<f32>(1.0, 0.9, 0.78) * 0.16 * galaxy * glow * (1.0 - 0.85 * lane);
    }
    return c * u.stars.z;
}

// The band's pole: the seed tilts it, so each sky's band crosses it somewhere else.
fn galaxy_axis() -> vec3<f32> {
    let k = hash(seed() ^ 0x27d4eb2du);
    let tilt = vec3<f32>(unit(k) - 0.5, unit(k ^ 0x9u) - 0.5, unit(k ^ 0x7u) - 0.5) * 1.2;
    return normalize(vec3<f32>(0.32, -0.86, 0.4) + tilt);
}

fn galaxy_core() -> vec3<f32> {
    return normalize(cross(galaxy_axis(), vec3<f32>(0.0, 0.0, 1.0)));
}

// How bright the galaxy band is along `d`, 0 to 1, for the stars it crowds.
fn in_band(d: vec3<f32>) -> f32 {
    let b = dot(d, galaxy_axis());
    return exp(-b * b / 0.012) * u.nebula_2.w;
}

// ---- Stars, around the direction of travel. ----

// The warp's speed as a fraction of light's.
fn beta() -> f32 {
    return clamp(u.seed.y, 0.0, 0.99);
}

// Its rapidity, by which aberration shifts the sky in log-tan units.
fn rapidity() -> f32 {
    let b = beta();
    return 0.5 * log((1.0 + b) / (1.0 - b));
}

// Light seen along `d` at the warp, shifted by its Doppler factor (above 1 ahead, below 1
// behind): bluer and brighter toward the travel, redder and dimmer away from it. The
// brightness follows the factor squared, a softened form of the real cube, held between
// a fifth and four times so a star ahead doesn't burn out and one behind still shows red.
fn doppler(c: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let b = beta();
    if b <= 0.0 {
        return c;
    }
    let factor = (1.0 + b * -d.z) / sqrt(1.0 - b * b);
    let x = clamp(log2(factor), -3.0, 3.0);
    // Shift the colour: toward blue ahead, losing blue then green behind.
    var shifted = c;
    if x > 0.0 {
        shifted = c * vec3<f32>(exp2(-0.6 * x), exp2(-0.2 * x), 1.0 + 0.25 * x);
        // Light shifted past the blue still shows as a blue-white glow.
        shifted += vec3<f32>(0.25, 0.35, 0.6) * dot(c, vec3<f32>(0.33)) * 0.3 * x;
    } else {
        shifted = c * vec3<f32>(1.0 + 0.15 * -x, exp2(0.7 * x), exp2(1.6 * x));
    }
    return shifted * clamp(factor * factor, 0.2, 4.0);
}

// `d` around the travel (-z): its angle about the axis and the log of the tangent of half
// its angle from it, in which a star moving away from the travel at any angle moves at
// one speed and a cell of `columns` around is square wherever it is.
fn about_travel(d: vec3<f32>) -> vec2<f32> {
    let cos_t = -d.z;
    let sin_t = max(length(d.xy), 1e-7);
    var tan_half = sin_t / (1.0 + cos_t);
    if cos_t < 0.0 {
        tan_half = (1.0 - cos_t) / sin_t;
    }
    return vec2<f32>(atan2(d.y, d.x), log(max(tan_half, 1e-7)));
}

// The direction at angle `phi` about the travel and log-tan `v` from it.
fn from_travel(phi: f32, v: f32) -> vec3<f32> {
    let t = 2.0 * atan(exp(v));
    let s = sin(t);
    return vec3<f32>(s * cos(phi), s * sin(phi), -cos(t));
}

// One layer of stars along `d`: the sky is cut into `columns` around the travel and as
// many log-tan rows as make each cell square, and a cell holds a star with chance
// `chance` times the cell's share of the sky, so stars are as dense everywhere. The
// grid has moved `travel` cells away from the travel (`whole` and `frac`), and each star
// draws a streak `streak` cells long behind it. `pixel` is one canvas pixel's angle.
fn stars(
    d: vec3<f32>,
    columns: f32,
    chance: f32,
    layer: u32,
    whole: u32,
    frac: f32,
    streak: f32,
    pixel: f32,
    crowd: f32,
) -> vec3<f32> {
    let cell = TAU / columns;
    // Aberration: a star seen at log-tan v is where the still sky has it at v plus the
    // warp's rapidity, so the stars crowd toward the direction of travel.
    let at = about_travel(d) + vec2<f32>(0.0, rapidity());
    let sin_t = max(length(d.xy), 1e-7);
    let col = floor(at.x / cell);
    let g = at.y / cell - frac;
    let first = i32(floor(g));
    let count = min(i32(floor(g + streak)) - first, MAX_STREAK_CELLS);
    let sigma = max(pixel * 0.6, 1e-5);
    let column = u32(i32(col) + 1000000) * 2654435761u;
    // A long streak spreads its star's light: dimmer the longer it is on screen.
    let spread = 1.0 / (1.0 + 0.03 * streak * cell * sin_t / max(pixel, 1e-6));
    var sum = vec3<f32>(0.0);
    for (var k = 0; k <= count; k++) {
        let row = first + k;
        let id = hash(column ^ hash(bitcast<u32>(row) - whole) ^ (layer * 0x9e3779b9u) ^ seed());
        // The cell's share of the sky: sin^2 of its angle from the travel.
        let s = 1.0 / cosh((f32(row) + 0.5 + frac) * cell);
        if f32(id & 0xffffffu) / 16777216.0 >= chance * crowd * s * s {
            continue;
        }
        let jx = 0.3 + 0.4 * unit(id ^ 0x68e31da4u);
        let jy = unit(id ^ 0xb5297a4du);
        let star_g = f32(row) + jy;
        // Across the streak: the angle about the travel, at the pixel's distance from it.
        let across = (at.x / cell - (col + jx)) * cell * sin_t;
        // Along it: ahead of the star's head, behind its tail, or on it.
        var along = 0.0;
        var fade = 1.0;
        if g > star_g {
            along = (g - star_g) * cell * sin_t;
        } else if g < star_g - streak {
            along = (star_g - streak - g) * cell * sin_t;
            fade = 0.0;
        } else if streak > 0.0 {
            fade = 1.0 - (star_g - g) / streak;
        }
        let tail = select(fade * fade, 1.0, streak <= 0.0);
        let glow = exp(-(across * across + along * along) / (2.0 * sigma * sigma)) * max(tail, 0.0);
        if glow <= 0.0 {
            continue;
        }
        let bright = 0.15 + 2.5 * pow(unit(id ^ 0x1b873593u), 6.0);
        let colour = mix(vec3<f32>(1.0, 0.82, 0.66), vec3<f32>(0.74, 0.84, 1.0), unit(id ^ 0xcc9e2d51u));
        sum += colour * bright * glow * spread;
    }
    return sum;
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let d = ray(p.xy);
    var c = gradient(d);
    if u.ground.w >= 0.0 {
        c = panorama(d);
    }
    let length_v = u.stars.w;
    // The deep sky blurs along the streaks as the warp builds, and fades out by the time
    // it's at speed: at warp only the stars are seen.
    let deep = 1.0 - smoothstep(0.1, 0.65, beta() / 0.9);
    if (u.nebula.w > 0.0 || u.nebula_2.w > 0.0) && deep > 0.0 {
        if length_v > 0.01 {
            let at = about_travel(d);
            var sum = vec3<f32>(0.0);
            for (var k = 0; k < 5; k++) {
                sum += deep_sky(from_travel(at.x, at.y - length_v * f32(k) * 0.25));
            }
            c += sum / 5.0 * deep;
        } else {
            c += deep_sky(d) * deep;
        }
    }
    let amount = u.stars.x;
    if amount > 0.0 {
        let pixel = 1.0 / max(u.forward.w, 1e-3);
        let crowd = 1.0 + 3.0 * in_band(d);
        let faint_cell = TAU / FAINT_COLUMNS;
        let bright_cell = TAU / BRIGHT_COLUMNS;
        // Faint stars blur out as the warp grows, and streak a little; bright ones streak.
        let faint_streak = min(length_v / faint_cell, 6.0);
        let faint = stars(d, FAINT_COLUMNS, amount * 0.25, 1u, bitcast<u32>(u.travel.w), u.travel.z, faint_streak, pixel, crowd)
            / (1.0 + length_v * 12.0);
        let bright = stars(d, BRIGHT_COLUMNS, amount * 0.3, 2u, bitcast<u32>(u.travel.y), u.travel.x, length_v / bright_cell, pixel, 1.0) * 1.6;
        c += doppler(faint + bright, d) * u.stars.y;
    }
    let a = clamp(u.sky.w, 0.0, 1.0);
    return vec4<f32>(c * a, a);
}
