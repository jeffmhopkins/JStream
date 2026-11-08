// Border: a rounded rectangle, the frame inset by `pad`, with a corner radius each
// (top-left, top-right, bottom-right, bottom-left). `clip` cuts the picture to it. An
// inner stroke runs inside its edge and an outer stroke past it, after a gap, each
// solid, double, dashed or dotted, in a flat colour or a linear or conic gradient that
// can travel around. Coverage is antialiased over one pixel by the shape's signed
// distance. Colours are premultiplied linear light.

struct Params {
    color: vec4<f32>,
    color2: vec4<f32>,
    outer_color: vec4<f32>,
    // top-left, top-right, bottom-right, bottom-left, px.
    radii: vec4<f32>,
    // The linear gradient's direction.
    dir: vec2<f32>,
    width: f32,
    dash: f32,
    gap: f32,
    outer_width: f32,
    outer_gap: f32,
    // How far the inner stroke's dashes or dots have travelled along the edge, px,
    // within one of its periods.
    along: f32,
    // How far the gradient has turned, in turns.
    turn: f32,
    pad: f32,
    // 0 solid, 1 double, 2 dashed, 3 dotted.
    style: u32,
    // 0 a colour, 1 linear, 2 conic.
    fill: u32,
    clip: u32,
    // The same for the outer stroke, whose dots repeat at its own width.
    outer_along: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

const TAU: f32 = 6.2831853;

// The radius of the corner `q`'s quadrant: x right, y down.
fn corner(q: vec2<f32>) -> f32 {
    if (q.x < 0.0) {
        return select(p.radii.w, p.radii.x, q.y < 0.0);
    }
    return select(p.radii.z, p.radii.y, q.y < 0.0);
}

// Signed distance from `q` (from the centre) to the rounded rectangle of half-size
// `half` with radius `r`: negative inside.
fn rounded(q: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let d = abs(q) - half + vec2<f32>(r);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

// How far along the edge `q` is, clockwise from the top-left corner's end, px: each
// straight side by its own length, each corner by its arc.
fn along_edge(q: vec2<f32>, half: vec2<f32>, r: vec4<f32>) -> f32 {
    let quarter = TAU * 0.25;
    let top = max(2.0 * half.x - r.x - r.y, 0.0);
    let right = max(2.0 * half.y - r.y - r.z, 0.0);
    let bottom = max(2.0 * half.x - r.z - r.w, 0.0);
    let left = max(2.0 * half.y - r.w - r.x, 0.0);
    let a1 = top;
    let a2 = a1 + quarter * r.y;
    let a3 = a2 + right;
    let a4 = a3 + quarter * r.z;
    let a5 = a4 + bottom;
    let a6 = a5 + quarter * r.w;
    let a7 = a6 + left;
    // The corner zones, then the side nearest the point.
    let tr = vec2<f32>(half.x - r.y, -half.y + r.y);
    if (q.x > tr.x && q.y < tr.y) {
        let v = q - tr;
        return a1 + r.y * atan2(v.x, -v.y);
    }
    let br = vec2<f32>(half.x - r.z, half.y - r.z);
    if (q.x > br.x && q.y > br.y) {
        let v = q - br;
        return a3 + r.z * atan2(v.y, v.x);
    }
    let bl = vec2<f32>(-half.x + r.w, half.y - r.w);
    if (q.x < bl.x && q.y > bl.y) {
        let v = q - bl;
        return a5 + r.w * atan2(-v.x, v.y);
    }
    let tl = vec2<f32>(-half.x + r.x, -half.y + r.x);
    if (q.x < tl.x && q.y < tl.y) {
        let v = q - tl;
        return a7 + r.x * atan2(-v.y, -v.x);
    }
    let to = half - abs(q);
    if (to.y < to.x) {
        if (q.y < 0.0) {
            return q.x + half.x - r.x;
        }
        return a4 + (half.x - r.z - q.x);
    }
    if (q.x > 0.0) {
        return a2 + (q.y + half.y - r.y);
    }
    return a6 + (half.y - r.w - q.y);
}

// A triangle wave over turns: 0 at 0, 1 at a half, 0 at 1, so a gradient loops
// without a seam.
fn tri(t: f32) -> f32 {
    return 1.0 - abs(2.0 * fract(t) - 1.0);
}

// The stroke's colour at `q`.
fn paint(q: vec2<f32>, half: vec2<f32>) -> vec4<f32> {
    if (p.fill == 1u) {
        let reach = abs(half.x * p.dir.x) + abs(half.y * p.dir.y);
        let t = 0.5 + 0.5 * dot(q, p.dir) / max(reach, 1e-3);
        return mix(p.color, p.color2, tri(t * 0.5 + p.turn));
    }
    if (p.fill == 2u) {
        let t = atan2(q.x, -q.y) / TAU;
        return mix(p.color, p.color2, tri(t + p.turn));
    }
    return p.color;
}

// How much of a stroke `w` wide covers a point `across` into it and `s` along the
// edge, for the style.
fn pattern(across: f32, s: f32, w: f32, along: f32) -> f32 {
    if (p.style == 1u) {
        // Two lines of a third each, a third apart.
        let third = w / 3.0;
        return saturate(third + 0.5 - across) + saturate(across - 2.0 * third + 0.5);
    }
    if (p.style == 2u) {
        let period = max(p.dash + p.gap, 1.0);
        let at = (s - along) - floor((s - along) / period) * period;
        return saturate(min(at, p.dash - at) + 0.5) + saturate(at - period + 0.5);
    }
    if (p.style == 3u) {
        let period = max(w + p.gap, 1.0);
        let at = (s - along) - floor((s - along) / period) * period;
        let v = vec2<f32>(at - 0.5 * w, across - 0.5 * w);
        return saturate(0.5 * w - length(v) + 0.5);
    }
    return 1.0;
}

// A source-over b, premultiplied.
fn over(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return a + b * (1.0 - a.a);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let half = frame.out_size * 0.5 - vec2<f32>(p.pad);
    let q = in.pos.xy - frame.out_size * 0.5;
    let shortest = max(min(half.x, half.y), 0.0);
    let r4 = min(p.radii, vec4<f32>(shortest));
    let r = min(corner(q), shortest);
    let d = rounded(q, half, r);
    var picture = src(in.uv);
    if (p.clip == 1u) {
        picture = picture * saturate(0.5 - d);
    }
    let s = along_edge(q, half, r4);
    var out = picture;
    if (p.width > 0.0) {
        // Inside the edge: from the edge (across 0) to `width` in.
        let band = saturate(0.5 - d) - saturate(0.5 - d - p.width);
        let cover = band * pattern(-d, s, p.width, p.along);
        out = over(paint(q, half) * cover, out);
    }
    if (p.outer_width > 0.0) {
        let past = d - p.outer_gap;
        let band = saturate(past + 0.5) - saturate(past - p.outer_width + 0.5);
        let cover = band * pattern(past, s, p.outer_width, p.outer_along);
        out = over(p.outer_color * cover, out);
    }
    return out;
}
