// Composite (straight-alpha sRGB RGBA) to NV12 in two passes: `fs_y` writes the Y plane
// at the canvas size, `fs_uv` the interleaved CbCr plane at half size. BT.709 matrix;
// the range comes from `params`. Each output is rounded to its 8-bit code here, so the
// unorm store of `code / 255` is exact on every rasterizer.

struct Params {
    // code = offset + scale * component, then rounded and clamped to 0..=255.
    y_scale: f32,
    y_offset: f32,
    c_scale: f32,
    c_offset: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

const KR: f32 = 0.2126;
const KB: f32 = 0.0722;
const KG: f32 = 0.7152;

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

// The sRGB transfer (IEC 61966-2-1), linear light to encoded.
fn oetf(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// Gamma-encoded R'G'B' of one source texel, composited over black. The sRGB view
// decodes on load, so this is linear light until `oetf`.
fn rgb_prime(p: vec2<i32>) -> vec3<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let c = textureLoad(src, clamp(p, vec2<i32>(0), size - 1), 0);
    let lin = clamp(c.rgb * c.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(oetf(lin.r), oetf(lin.g), oetf(lin.b));
}

fn luma(rgb: vec3<f32>) -> f32 {
    return KR * rgb.r + KG * rgb.g + KB * rgb.b;
}

// (Cb, Cr) in -0.5..=0.5.
fn chroma(rgb: vec3<f32>) -> vec2<f32> {
    let y = luma(rgb);
    return vec2<f32>((rgb.b - y) / (2.0 * (1.0 - KB)), (rgb.r - y) / (2.0 * (1.0 - KR)));
}

fn code(v: f32) -> f32 {
    return clamp(round(v), 0.0, 255.0) / 255.0;
}

@fragment
fn fs_y(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let y = luma(rgb_prime(vec2<i32>(pos.xy)));
    return vec4<f32>(code(params.y_offset + params.y_scale * y), 0.0, 0.0, 1.0);
}

// Left-sited chroma (the H.264 default): the sample sits on the block's left column,
// half-way between its two rows. A [1 2 1] tap across the columns left of, on and right
// of it, averaged over both rows. Edges repeat the border texel.
@fragment
fn fs_uv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = 2 * i32(pos.x);
    let y = 2 * i32(pos.y);
    var sum = vec2<f32>(0.0);
    for (var row = 0; row < 2; row++) {
        sum += chroma(rgb_prime(vec2<i32>(x - 1, y + row)));
        sum += 2.0 * chroma(rgb_prime(vec2<i32>(x, y + row)));
        sum += chroma(rgb_prime(vec2<i32>(x + 1, y + row)));
    }
    let c = sum / 8.0;
    return vec4<f32>(
        code(params.c_offset + params.c_scale * c.x),
        code(params.c_offset + params.c_scale * c.y),
        0.0,
        1.0,
    );
}
