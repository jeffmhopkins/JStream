// A 3D scene's sky backdrop (ADR-0167): the sky of its environment light seen through
// the camera, infinitely far away. Each pixel looks along its ray from the camera, so the
// sky turns with where the camera looks and never moves with where it is. The sky is the
// environment's gradient, or its picture as an equirectangular panorama, with nebulae
// and a galaxy band read from their bake (ADR-0174) and procedural stars over it. Stars
// are laid out around the direction of travel, the sky's -z, so at warp each one streaks
// away from it along its own line, the same from any angle the camera looks. Out:
// premultiplied linear light, at the backdrop's opacity.

struct Sky {
    // xyz: the camera's right axis in the scene's space; w: the principal point's x.
    right: vec4<f32>,
    // xyz: its down axis; w: the principal point's y.
    down: vec4<f32>,
    // xyz: its forward axis; w: the focal length in canvas pixels.
    forward: vec4<f32>,
    // rgb: straight up, linear light times intensity; w: the backdrop's opacity.
    sky: vec4<f32>,
    // rgb: at the horizon; w: unused. The camera's axes above are already in the sky's
    // own frame, its orientation undone (ADR-0182).
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
    // crowds the stars ahead and shifts their light; z: how much the warp is dropping,
    // 0 to 1; w: the warp, 0 to 1.
    seed: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Sky;
@group(0) @binding(1) var sky_tex: texture_2d<f32>;
@group(0) @binding(2) var sky_sampler: sampler;
// The seed's baked nebulae and galaxy band (`sky_deep.wgsl`), each a cube map read along
// a direction of the sky.
@group(0) @binding(3) var nebula_tex: texture_cube<f32>;
@group(0) @binding(4) var galaxy_tex: texture_cube<f32>;
@group(0) @binding(5) var deep_sampler: sampler;
// Which cells of the bright stars' layer hold a star, as `fs_stars` writes it for the
// frame: a column across, 32 rows to a texel down, a bit a row.
@group(0) @binding(6) var star_table: texture_2d<u32>;

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
// sky's own frame (x right, y down, z toward the viewer at rest): the axes carry its turn,
// tilt and roll.
fn ray(p: vec2<f32>) -> vec3<f32> {
    let f = max(u.forward.w, 1e-3);
    let x = (p.x - u.right.w) / f;
    let y = (p.y - u.down.w) / f;
    return normalize(u.right.xyz * x + u.down.xyz * y + u.forward.xyz);
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

fn seed() -> u32 {
    return bitcast<u32>(u.seed.x);
}

// ---- Nebulae and the galaxy band, read from their bake. ----

// The nebulae and the galaxy band along `d`: light added to the sky. Their shapes are
// read from the seed's bake (`sky_deep.wgsl`, ADR-0174), and the settings applied here:
// the nebulae's two colours and their amount, and how bright the band is.
fn deep_sky(d: vec3<f32>) -> vec3<f32> {
    var c = vec3<f32>(0.0);
    let nebula = u.nebula.w;
    if nebula > 0.0 {
        let w = textureSampleLevel(nebula_tex, deep_sampler, d, 0.0);
        let grey = dot(u.nebula.rgb, vec3<f32>(1.0)) * w.z + dot(u.nebula_2.rgb, vec3<f32>(1.0)) * w.w;
        c += (u.nebula.rgb * w.x + u.nebula_2.rgb * w.y + vec3<f32>(grey)) * nebula;
    }
    let galaxy = u.nebula_2.w;
    if galaxy > 0.0 {
        c += vec3<f32>(1.0, 0.9, 0.78) * 0.16 * galaxy * textureSampleLevel(galaxy_tex, deep_sampler, d, 0.0).r;
    }
    return c * u.stars.z;
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

// The Doppler factor along `d` at the warp: above 1 toward the travel, below 1 away.
fn doppler_factor(d: vec3<f32>) -> f32 {
    let b = beta();
    return (1.0 + b * -d.z) / sqrt(1.0 - b * b);
}

// Light seen along `d` at the warp, shifted by its Doppler factor: bluer and brighter
// toward the travel, redder and dimmer away from it. The colour leans cool, more than
// the real shift would, so the side and most of the sky read blue and only the light
// well behind turns red. The brightness follows the factor squared, a softened form of
// the real cube, held between a fifth and four times so a star ahead doesn't burn out
// and one behind still shows red.
fn doppler(c: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let b = beta();
    if b <= 0.0 {
        return c;
    }
    let factor = doppler_factor(d);
    let x = clamp(log2(factor) + 0.8 * b, -3.0, 3.0);
    // Shift the colour: toward blue ahead, losing blue then green behind.
    var shifted = c;
    if x > 0.0 {
        shifted = c * vec3<f32>(exp2(-1.0 * x), exp2(-0.45 * x), 1.0 + 0.45 * x);
        // Light shifted past the blue still shows as a blue-white glow.
        shifted += vec3<f32>(0.2, 0.4, 0.9) * dot(c, vec3<f32>(0.33)) * 0.35 * x;
    } else {
        shifted = c * vec3<f32>(1.0 + 0.15 * -x, exp2(0.7 * x), exp2(1.6 * x));
    }
    return shifted * clamp(factor * factor, 0.2, 4.0);
}

// The background radiation seen at the warp: the faint glow of the whole sky, shifted
// from the invisible into blue light where the Doppler factor is above 1: brightest
// toward the travel, faint at the sides and gone behind.
fn radiation(d: vec3<f32>) -> vec3<f32> {
    let b = beta();
    if b <= 0.0 {
        return vec3<f32>(0.0);
    }
    let ahead = clamp((doppler_factor(d) - 1.0) / 2.5, 0.0, 1.0);
    let glow = pow(ahead, 1.6);
    // A deep blue haze, with a paler heart dead ahead.
    return (vec3<f32>(0.02, 0.07, 0.3) * glow + vec3<f32>(0.08, 0.14, 0.3) * pow(ahead, 6.0))
        * smoothstep(0.0, 0.6, b);
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

// A cell's star, by the cell's column and row of its layer's grid, once the grid has
// moved `whole` cells.
fn star_id(column: u32, row: i32, whole: u32, layer: u32) -> u32 {
    return hash(column ^ hash(bitcast<u32>(row) - whole) ^ (layer * 0x9e3779b9u) ^ seed());
}

// Whether the cell of star `id` in `row` holds a star: with chance `odds` times the
// cell's share of the sky, so stars are as dense everywhere.
fn holds_star(id: u32, row: i32, frac: f32, cell: f32, odds: f32) -> bool {
    // A cell's share of the sky is at most 1, so most cells are empty whatever theirs
    // is, and are passed over before it is worked out.
    let dice = f32(id & 0xffffffu) / 16777216.0;
    if dice >= odds {
        return false;
    }
    // The cell's share of the sky: sin^2 of its angle from the travel.
    let s = 1.0 / cosh((f32(row) + 0.5 + frac) * cell);
    return dice < odds * s * s;
}

// A pixel's place on one layer of stars, and what every star of its column shares.
struct Seat {
    // How far across its column the pixel is, 0 to 1.
    within: f32,
    // Its place along the column, in cells.
    g: f32,
    // The first row a star's streak could reach the pixel from, and how many more.
    first: i32,
    count: i32,
    // The column, and its hash.
    col: i32,
    column: u32,
    // A cell's width at the pixel, as an angle.
    width: f32,
    // A star's width, as an angle.
    sigma: f32,
    // How much a streak's length dims its star.
    spread: f32,
    // Whether no star of the column can reach the pixel.
    empty: bool,
}

// The pixel along `d` on a layer of `columns` around the travel that has moved `frac`
// of a cell, each star drawing a streak `streak` cells long behind it. `pixel` is one
// canvas pixel's angle.
fn seat(d: vec3<f32>, columns: f32, frac: f32, streak: f32, pixel: f32) -> Seat {
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
    // A star sits between 0.3 and 0.7 of the way across its column, so a pixel further
    // than six of a star's widths outside that part gets under a hundred-millionth of
    // any star's light: nothing, without looking for one.
    let within = at.x / cell - col;
    let reach = 6.0 * sigma / (cell * sin_t);
    // A long streak spreads its star's light: dimmer the longer it is on screen.
    let spread = 1.0 / (1.0 + 0.03 * streak * cell * sin_t / max(pixel, 1e-6));
    return Seat(
        within,
        g,
        first,
        count,
        i32(col),
        u32(i32(col) + 1000000) * 2654435761u,
        cell * sin_t,
        sigma,
        spread,
        within < 0.3 - reach || within > 0.7 + reach,
    );
}

// The light the star `id` of `row` gives the pixel at `at`, its streak `streak` cells
// long.
fn star_light(id: u32, row: i32, at: Seat, streak: f32) -> vec3<f32> {
    let jx = 0.3 + 0.4 * unit(id ^ 0x68e31da4u);
    // Across the streak: the angle about the travel, at the pixel's distance from it.
    let across = (at.within - jx) * at.width;
    // Six widths to the side of a star, as in `seat`, there is nothing of it.
    if abs(across) > 6.0 * at.sigma {
        return vec3<f32>(0.0);
    }
    let jy = unit(id ^ 0xb5297a4du);
    let star_g = f32(row) + jy;
    // Along it: ahead of the star's head, behind its tail, or on it.
    var along = 0.0;
    var fade = 1.0;
    if at.g > star_g {
        along = (at.g - star_g) * at.width;
    } else if at.g < star_g - streak {
        along = (star_g - streak - at.g) * at.width;
        fade = 0.0;
    } else if streak > 0.0 {
        fade = 1.0 - (star_g - at.g) / streak;
    }
    let tail = select(fade * fade, 1.0, streak <= 0.0);
    let glow = exp(-(across * across + along * along) / (2.0 * at.sigma * at.sigma)) * max(tail, 0.0);
    if glow <= 0.0 {
        return vec3<f32>(0.0);
    }
    let bright = 0.15 + 2.5 * pow(unit(id ^ 0x1b873593u), 6.0);
    let colour = mix(vec3<f32>(1.0, 0.82, 0.66), vec3<f32>(0.74, 0.84, 1.0), unit(id ^ 0xcc9e2d51u));
    return colour * bright * glow * at.spread;
}

// One layer of stars along `d`: the sky is cut into `columns` around the travel and as
// many log-tan rows as make each cell square, and a cell holds a star with chance
// `chance` times the cell's share of the sky, so stars are as dense everywhere. The
// grid has moved `travel` cells away from the travel (`whole` and `frac`), and each star
// draws a streak `streak` cells long behind it. `pixel` is one canvas pixel's angle.
// Every cell a streak could reach the pixel from is looked at, so this is for a layer
// whose streaks are short.
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
    let at = seat(d, columns, frac, streak, pixel);
    if at.empty {
        return vec3<f32>(0.0);
    }
    var sum = vec3<f32>(0.0);
    for (var k = 0; k <= at.count; k++) {
        let row = at.first + k;
        let id = star_id(at.column, row, whole, layer);
        if holds_star(id, row, frac, TAU / columns, chance * crowd) {
            sum += star_light(id, row, at, streak);
        }
    }
    return sum;
}

// ---- The bright stars' table. ----
//
// At warp a bright star's streak is up to 64 cells long, so a pixel would look at up to
// 65 cells of its column for the few that hold a star. Which cells hold one depends on
// the column and the row only, not on the pixel. So `fs_stars` first writes that out
// for every cell a pixel can ask about, 32 rows of a column to a texel, one bit a row,
// and a pixel then reads two or three texels and visits only the cells with a star.

// The bright layer, as `fs` and `fs_stars` both have it.
const BRIGHT_LAYER: u32 = 2u;
// The table's rows start this far below row 0, and there are twice as many: a row is
// the log-tan of half the angle from the travel over a cell, which the 1e-7 floor of
// `about_travel`, the warp's rapidity and a streak's length keep within -720 and 850.
const TABLE_ROW_ZERO: i32 = 1024;
const TABLE_ROWS: i32 = 2048;
// A column is the angle about the travel over a cell, floored: -140 to 140.
const TABLE_COLUMN_ZERO: i32 = 140;

// The bright layer's chance of a star in a cell.
fn bright_chance() -> f32 {
    return u.stars.x * 0.3;
}

// One texel of the bright stars' table: the column `p.x`, and 32 rows from row
// `32 p.y`, a bit set for each cell that holds a star.
@fragment
fn fs_stars(@builtin(position) p: vec4<f32>) -> @location(0) vec4<u32> {
    let column = u32(i32(p.x) - TABLE_COLUMN_ZERO + 1000000) * 2654435761u;
    let base = i32(p.y) * 32 - TABLE_ROW_ZERO;
    let whole = bitcast<u32>(u.travel.y);
    var bits = 0u;
    for (var j = 0; j < 32; j++) {
        let row = base + j;
        if holds_star(star_id(column, row, whole, BRIGHT_LAYER), row, u.travel.x, TAU / BRIGHT_COLUMNS, bright_chance()) {
            bits |= 1u << u32(j);
        }
    }
    return vec4<u32>(bits, 0u, 0u, 0u);
}

// The bright stars along `d`, each drawing a streak `streak` cells long behind it:
// what `stars` gives for their layer, visiting only the cells the table says hold one.
fn bright_stars(d: vec3<f32>, streak: f32, pixel: f32) -> vec3<f32> {
    let at = seat(d, BRIGHT_COLUMNS, u.travel.x, streak, pixel);
    // The rows to look at, as the table counts them.
    let low = max(at.first + TABLE_ROW_ZERO, 0);
    let high = min(at.first + at.count + TABLE_ROW_ZERO, TABLE_ROWS - 1);
    if at.empty || low > high {
        return vec3<f32>(0.0);
    }
    let x = clamp(at.col + TABLE_COLUMN_ZERO, 0, 2 * TABLE_COLUMN_ZERO);
    let whole = bitcast<u32>(u.travel.y);
    var sum = vec3<f32>(0.0);
    for (var block = low >> 5u; block <= high >> 5u; block++) {
        let base = block * 32;
        var bits = textureLoad(star_table, vec2<i32>(x, block), 0).r;
        // Only the rows from `low` to `high`.
        if low > base {
            bits &= 0xffffffffu << u32(low - base);
        }
        if high < base + 31 {
            bits &= 0xffffffffu >> u32(base + 31 - high);
        }
        for (var n = 0; n < 32 && bits != 0u; n++) {
            let row = base + i32(firstTrailingBit(bits)) - TABLE_ROW_ZERO;
            bits &= bits - 1u;
            sum += star_light(star_id(at.column, row, whole, BRIGHT_LAYER), row, at, streak);
        }
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
    let warp = u.seed.w;
    let dropping = clamp(u.seed.z, 0.0, 1.0);
    // Going into warp the deep sky blurs along the streaks and is gone by a quarter of
    // the way. Coming out it returns sooner, squeezed toward straight ahead, and rushes
    // back out to its place as the warp collapses: a zoom, blurred along its rush. The
    // two blend by how much it is dropping, so arriving at a cruise doesn't snap.
    let deep = mix(1.0 - smoothstep(0.0, 0.25, warp), 1.0 - smoothstep(0.15, 0.55, warp), dropping);
    let zoom = 3.0 * smoothstep(0.0, 0.55, warp) * dropping;
    let rushing = dropping > 0.0;
    if (u.nebula.w > 0.0 || u.nebula_2.w > 0.0) && deep > 0.0 {
        let reach = mix(length_v, zoom * 0.6, dropping);
        if reach > 0.01 || zoom > 0.0 {
            let at = about_travel(d);
            var sum = vec3<f32>(0.0);
            // More taps for the long rush out of warp, so it smears rather than ghosts.
            let taps = select(5, 9, rushing);
            for (var k = 0; k < taps; k++) {
                sum += deep_sky(from_travel(at.x, at.y + zoom - reach * f32(k) / f32(taps - 1)));
            }
            c += sum / f32(taps) * deep;
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
        let bright = bright_stars(d, length_v / bright_cell, pixel) * 1.6;
        c += doppler(faint + bright, d) * u.stars.y;
    }
    if amount > 0.0 || u.nebula.w > 0.0 || u.nebula_2.w > 0.0 {
        c += radiation(d) * u.stars.z;
    }
    let a = clamp(u.sky.w, 0.0, 1.0);
    return vec4<f32>(c * a, a);
}
