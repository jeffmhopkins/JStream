// Scroll: the picture moved by `offset` pixels (right and down are positive), sampled
// bilinearly by hand so a looping axis wraps across its seam and a non-looping axis
// reads transparent past the edge.

struct Params {
    // Pixels, already wrapped to the image on a looping axis.
    offset: vec2<f32>,
    loop_x: f32,
    loop_y: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

fn fetch(px: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(frame.in_size);
    var q = px;
    if (p.loop_x > 0.5) {
        q.x = ((q.x % size.x) + size.x) % size.x;
    } else if (q.x < 0 || q.x >= size.x) {
        return vec4<f32>(0.0);
    }
    if (p.loop_y > 0.5) {
        q.y = ((q.y % size.y) + size.y) % size.y;
    } else if (q.y < 0 || q.y >= size.y) {
        return vec4<f32>(0.0);
    }
    return texel(q);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let pos = in.pos.xy - p.offset - vec2<f32>(0.5);
    let base = floor(pos);
    let f = pos - base;
    let i = vec2<i32>(base);
    let top = mix(fetch(i), fetch(i + vec2<i32>(1, 0)), f.x);
    let bottom = mix(fetch(i + vec2<i32>(0, 1)), fetch(i + vec2<i32>(1, 1)), f.x);
    return mix(top, bottom, f.y);
}
