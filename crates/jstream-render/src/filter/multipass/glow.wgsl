// The glow from the jump flood's nearest seeds (aux, the output's size), drawn under
// the input. A pixel `e` px past the shape's edge (its seed's distance less the half
// pixel to that seed's edge) takes `strength * seed alpha * profile(e)`, clamped to 1,
// and nothing from `radius` on. `profile(e)` is `erfc(e / (sigma * sqrt 2)) / 2` with
// sigma `radius / 3`: how much of a gaussian of that sigma a straight edge covers `e`
// px past it. So a straight edge lights as a blur of the shape would, and a corner as
// far and as brightly, rounded.

struct Params {
    // Premultiplied linear.
    color: vec4<f32>,
    radius: f32,
    strength: f32,
}

@group(0) @binding(4) var<uniform> p: Params;

// erfc(x) for x >= 0, within 1.5e-7 (Abramowitz and Stegun 7.1.26).
fn erfc_pos(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741
        + t * (-1.453152027 + t * 1.061405429))));
    return poly * exp(-x * x);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let top = load_edge(px, 1u);
    let v = textureLoad(aux_tex, px, 0);
    var a = 0.0;
    if (v.a > 0.5) {
        let e = max(length(v.xy) - 0.5, 0.0);
        if (e < p.radius) {
            // 3 / sqrt 2: e / (sigma * sqrt 2) with sigma radius / 3.
            let profile = 0.5 * erfc_pos(e * 2.1213203 / p.radius);
            a = min(p.strength * v.b * profile, 1.0);
        }
    }
    let under = p.color * a;
    return top + under * (1.0 - top.a);
}
