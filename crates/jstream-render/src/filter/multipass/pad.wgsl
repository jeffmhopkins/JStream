// The input placed `pad` pixels in from the top-left of a larger output, transparent
// around it: the room an effect that draws past its input needs on an item (ADR-0065
// §5).

struct Params {
    pad: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return load_edge(out_px(in.pos) - vec2<i32>(p.pad), 1u);
}
