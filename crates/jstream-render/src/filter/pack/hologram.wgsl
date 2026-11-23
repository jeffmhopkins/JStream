// Hologram: any picture as a projected hologram. The picture's luminance takes the
// hologram's `color` (by `tint`), its dark parts turn see-through (by `see_through`), and
// it is drawn `brightness` times as bright. On top go rolling scanlines, a bright sweep
// band, a glow along the edges, grain, a colour fringe, a flicker and an occasional
// glitch band that shifts sideways. `reveal` materialises it from one side, row by row,
// behind a bright leading edge: 0 draws nothing, 1 the whole picture. Every time-driven
// value (the roll, the sweep's place, the flicker, the glitch band, the grain's seed) is
// worked out on the CPU from the clock. Coverage is kept: nothing is drawn where the
// input is transparent, and light is added only inside the input's coverage, so it may
// exceed it (an emissive glow).

struct Params {
    // The hologram's colour, linear, alpha ignored.
    color: vec4<f32>,
    tint: f32,
    see_through: f32,
    // A gain on the picture's light.
    brightness: f32,
    // How dark the scanlines are.
    lines: f32,
    spacing: f32,
    // Pixels the lines have rolled, already wrapped to `spacing`.
    roll: f32,
    // The sweep band's strength and where its centre is, in output UV y.
    sweep: f32,
    sweep_y: f32,
    // The edge glow's strength and its falloff, px.
    edge: f32,
    edge_width: f32,
    // How much this step dims, `0..1`.
    dim: f32,
    // The glitch band: its centre and half height in UV y, and its shift in UV x (0 is
    // none this step).
    glitch_y: f32,
    glitch_h: f32,
    glitch_dx: f32,
    // Red read this far left and blue this far right, px.
    fringe: f32,
    grain: f32,
    seed: f32,
    // How much is shown, `0..1`, and from where: 0 bottom, 1 top, 2 the middle out.
    reveal: f32,
    origin: u32,
    // How bright the leading edge is, already faded in and out with `reveal`.
    lead: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

// A hash of a pixel and the seed in `0..1`.
fn noise(px: vec2<f32>, seed: f32) -> f32 {
    let q = px + vec2<f32>(seed * 17.13, seed * 3.71);
    return fract(sin(dot(q, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// How much of the output row centred at `y` a dark scanline covers (half of each period).
fn line(y: f32) -> f32 {
    let spacing = max(p.spacing, 1.0);
    let dark = 0.5 * spacing;
    let a = (y - 0.5 - p.roll) - floor((y - 0.5 - p.roll) / spacing) * spacing;
    let first = max(0.0, min(a + 1.0, dark) - a);
    let second = max(0.0, min(a + 1.0, spacing + dark) - max(a, spacing));
    return clamp(first + second, 0.0, 1.0);
}

// How far into the shown part `uv_y` is, in UV (negative: not shown yet), for `reveal`.
fn inside(uv_y: f32) -> f32 {
    if (p.origin == 1u) {
        return p.reveal - uv_y;
    }
    if (p.origin == 2u) {
        return 0.5 * p.reveal - abs(uv_y - 0.5);
    }
    return uv_y - (1.0 - p.reveal);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let size = frame.out_size;
    let px = in.pos.xy;
    // The reveal: rows past the leading edge are not drawn; the rows just behind it
    // fill in at random, a row at a time, as the edge passes.
    let depth = inside(in.uv.y);
    let row_px = 1.0 / max(size.y, 1.0);
    if (p.reveal <= 0.0 || depth < -2.0 * row_px * 4.0) {
        return vec4<f32>(0.0);
    }
    var uv = in.uv;
    if (p.glitch_dx != 0.0 && abs(uv.y - p.glitch_y) < p.glitch_h) {
        uv.x = uv.x + p.glitch_dx;
    }
    let c = src(uv);
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    var rgb = c.rgb;
    if (p.fringe > 0.0) {
        let dx = vec2<f32>(p.fringe / frame.in_size.x, 0.0);
        rgb = min(vec3<f32>(src(uv - dx).r, c.g, src(uv + dx).b), vec3<f32>(c.a));
    }
    let l = dot(rgb, LUMA);
    let hue = p.color.rgb;
    // The tint keeps the luminance: white becomes the colour scaled to white's luminance.
    let held = hue / max(dot(hue, LUMA), 0.05);
    rgb = mix(rgb, held * l, p.tint);
    // Dark parts turn see-through, by the picture's straight luminance.
    let straight = l / c.a;
    let keep = mix(1.0, smoothstep(0.0, 0.5, straight), p.see_through);
    var a = c.a * keep;
    rgb = rgb * keep;
    rgb = rgb * p.brightness * (1.0 - p.lines * line(px.y));
    // Light added inside the coverage: the sweep, the edge glow, the grain.
    var light = 0.0;
    let sweep_d = (in.uv.y - p.sweep_y) / 0.08;
    light = light + p.sweep * exp(-sweep_d * sweep_d) * 0.6;
    let edge_d = min(min(px.x, size.x - px.x), min(px.y, size.y - px.y));
    light = light + p.edge * exp(-edge_d / max(p.edge_width, 0.5));
    if (p.grain > 0.0) {
        light = light + (noise(floor(px), p.seed) - 0.5) * p.grain * 0.5;
    }
    rgb = max(rgb + held * light * c.a * 0.5, vec3<f32>(0.0));
    a = clamp(a + max(light, 0.0) * c.a * 0.5, 0.0, c.a);
    rgb = rgb * (1.0 - p.dim);
    a = a * (1.0 - 0.5 * p.dim);
    // The reveal's leading edge and the rows filling in behind it.
    if (p.reveal < 1.0) {
        let band = 0.12;
        let fill = clamp(depth / band, 0.0, 1.0);
        let row = floor(px.y / 2.0);
        if (depth < 0.0 || noise(vec2<f32>(row, 1.0), p.seed) > fill) {
            rgb = vec3<f32>(0.0);
            a = 0.0;
        }
        let lead_d = depth * size.y / 3.0;
        let glow = p.lead * exp(-lead_d * lead_d) * c.a;
        rgb = rgb + mix(held, vec3<f32>(1.0), 0.6) * glow;
        a = max(a, min(glow, c.a));
    }
    return vec4<f32>(rgb, a);
}
