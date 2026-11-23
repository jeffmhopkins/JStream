// Fullscreen copy from the FP16 working target into the sRGB output. Un-premultiplies so
// the sRGB encode applies to straight colour. Alpha is stored as-is.

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    return vec4<f32>(p, 0.0, 1.0);
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var nearest: sampler;

// IEC 61966-2-1 decode, the curve continued above 1 as the encode continues it.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    // Written so that 1 decodes to exactly 1, as the composite's encode is.
    let high = pow(vec3<f32>(1.0) + (max(c, vec3<f32>(0.0)) - vec3<f32>(1.0)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

// The decode pass of browser-style blending (ADR-0126 §3): `src` holds the target's
// premultiplied sRGB-encoded colour, and this writes it back as premultiplied linear
// light, into an FP16 target. Colour with no alpha is added light, decoded as it is.
@fragment
fn fs_decode(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(pos.xy), 0);
    if (c.a > 0.0) {
        return vec4<f32>(srgb_to_linear(c.rgb / c.a) * c.a, c.a);
    }
    return vec4<f32>(srgb_to_linear(c.rgb), 0.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Sampled at the texel's centre with a nearest sampler: the same value a load
    // gives, and on a software renderer (llvmpipe) much less work (#1449).
    let c = textureSampleLevel(src, nearest, pos.xy / vec2<f32>(textureDimensions(src)), 0.0);
    return vec4<f32>(select(vec3<f32>(0.0), c.rgb / c.a, c.a > 0.0), c.a);
}

// The resolve of a working target the composite left sRGB-encoded (ADR-0126 §3): the
// decode above and `fs` in one, with no linear copy of the target between them. `src`
// holds premultiplied sRGB-encoded colour; the output takes straight linear RGB, which
// its format encodes on store.
@fragment
fn fs_encoded(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(src, nearest, pos.xy / vec2<f32>(textureDimensions(src)), 0.0);
    return vec4<f32>(select(vec3<f32>(0.0), srgb_to_linear(c.rgb / c.a), c.a > 0.0), c.a);
}

// IEC 61966-2-1 encode, as the composite's: the curve continues above 1, and 1 encodes
// to exactly 1.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = vec3<f32>(1.0) + 1.055 * (pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(1.0));
    return select(high, low, c <= vec3<f32>(0.0031308));
}

// What `fs_layer` puts its layer over.
struct Over {
    // The background as the composite target holds it: premultiplied linear light, or
    // premultiplied sRGB-encoded colour in the encoded space.
    background: vec4<f32>,
    // x: 1 in the encoded space (ADR-0126 §3), else 0.
    encoded: vec4<f32>,
}

@group(1) @binding(0) var<uniform> over: Over;

// A scene that is one premultiplied linear layer over the whole canvas, straight into
// the sRGB output: the composite of that one item over the background, the decode of
// the encoded space, and the resolve, in one pass with none of their targets. Each step
// is the composite shader's, the blend state's or the passes' above, in their order.
@fragment
fn fs_layer(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var c = textureLoad(src, vec2<i32>(pos.xy), 0);
    let encoded = over.encoded.x > 0.5;
    if encoded {
        if c.a > 0.0 {
            c = vec4<f32>(linear_to_srgb(max(c.rgb / c.a, vec3<f32>(0.0))) * c.a, c.a);
        } else {
            c = vec4<f32>(linear_to_srgb(max(c.rgb, vec3<f32>(0.0))), 0.0);
        }
    }
    // Source-over, premultiplied.
    var o = c + over.background * (1.0 - c.a);
    if encoded {
        if o.a > 0.0 {
            o = vec4<f32>(srgb_to_linear(o.rgb / o.a) * o.a, o.a);
        } else {
            o = vec4<f32>(srgb_to_linear(o.rgb), 0.0);
        }
    }
    return vec4<f32>(select(vec3<f32>(0.0), o.rgb / o.a, o.a > 0.0), o.a);
}
