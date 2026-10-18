// Rounded corners: the frame's own rectangle with rounded corners, antialiased over
// one pixel by its signed distance; outside it is transparent. An optional border is
// drawn inside the shape, over the picture, concentric with the corners.

struct Params {
    // Premultiplied linear light.
    border_color: vec4<f32>,
    radius: f32,
    border: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

// Signed distance from `q` (relative to the centre) to a rectangle of half-size `half`
// with corner radius `r`: negative inside.
fn rounded_rect(q: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let d = abs(q) - half + vec2<f32>(r);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let half = frame.out_size * 0.5;
    let q = in.pos.xy - half;
    let r = min(p.radius, min(half.x, half.y));
    let outer = saturate(0.5 - rounded_rect(q, half, r));
    let c = src(in.uv);
    var layer = c;
    if (p.border > 0.0) {
        let inner_half = half - vec2<f32>(p.border);
        let inner = saturate(0.5 - rounded_rect(q, inner_half, max(r - p.border, 0.0)));
        let over = p.border_color + c * (1.0 - p.border_color.a);
        layer = mix(over, c, inner);
    }
    return layer * outer;
}
