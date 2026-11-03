// Colour helpers shared by the filter prelude and the shader-source prelude.

// `c` as premultiplied linear, given how its image stores alpha: 0 premultiplied linear,
// 1 straight (an import), 2 premultiplied sRGB code values (an imported browser page),
// 3 opaque (an imported capture, its alpha read as 1).
fn to_premul_linear(c: vec4<f32>, alpha: f32) -> vec4<f32> {
    if (alpha > 2.5) {
        return vec4<f32>(c.rgb, 1.0);
    }
    if (alpha > 1.5) {
        // Premultiplied code values: a filtered texel keeps rgb <= a, up to rounding.
        let straight = select(vec3<f32>(0.0), min(c.rgb / c.a, vec3<f32>(1.0)), c.a > 0.0);
        return vec4<f32>(srgb_to_linear(straight) * c.a, c.a);
    }
    return select(c, vec4<f32>(c.rgb * c.a, c.a), alpha > 0.5);
}

fn unpremul(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(c.rgb / c.a, c.a);
}

fn premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow(max((c + 0.055) / 1.055, vec3<f32>(0.0)), vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// BT.709 luma of linear RGB.
fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}
