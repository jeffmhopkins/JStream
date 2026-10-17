// apply_lut: a 3D table laid out as 2D tiles in the aux image (see `cube::Lut::layout`).

struct Params {
    // Points per side; 0 passes the input through.
    size: u32,
    // Tiles across the aux image.
    cols: u32,
    // 0 keeps the input, 1 is the table's output.
    amount: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

// Bilinear tap in blue slice `b` at red/green grid coordinates `rg` (0..size-1). The
// half-texel offset lands on texel centres, so a tap never reaches a neighbouring tile.
fn slice_tap(rg: vec2<f32>, b: u32) -> vec3<f32> {
    let tile = vec2<f32>(f32(b % p.cols), f32(b / p.cols));
    let px = tile * f32(p.size) + rg + vec2<f32>(0.5);
    return aux(px / frame.aux_size).rgb;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    if (p.size == 0u || c.a <= 0.0) {
        return c;
    }
    let s = unpremul(c);
    // The table maps sRGB-encoded straight colour.
    let e = clamp(linear_to_srgb(s.rgb), vec3<f32>(0.0), vec3<f32>(1.0));
    let top = f32(p.size - 1u);
    let x = e * top;
    let b0 = min(floor(x.z), top - 1.0);
    let lo = slice_tap(x.xy, u32(b0));
    let hi = slice_tap(x.xy, u32(b0) + 1u);
    let graded = mix(lo, hi, x.z - b0);
    let out = mix(e, clamp(graded, vec3<f32>(0.0), vec3<f32>(1.0)), p.amount);
    return premul(vec4<f32>(srgb_to_linear(out), s.a));
}
