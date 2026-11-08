// CPU frames into premultiplied linear `Rgba16Float` on the GPU (ADR-0117 §2).
//
// The bytes arrive as written, in a storage buffer: a 16-word header, then the planes.
// Each invocation converts one texel with the same arithmetic the CPU conversions used:
// the sRGB decode and the alpha are read from tables the CPU built (`lut`), so the
// floats multiplied are the CPU's, and NV12 is converted in the same 16.16 fixed point.
// Only the final rounding to half precision is the GPU's.
//
// The texture written can be smaller than the frame (#1139): each texel is then the
// mean of the frame's texels under it, in premultiplied linear light.

struct Header {
    width: u32,
    height: u32,
    // 0: straight sRGB RGBA8, premultiplied here. 1: opaque RGBA8. 2: opaque BGRA8.
    // 3: NV12 (opaque).
    mode: u32,
    // Bytes from one row of plane 0 to the next.
    stride0: u32,
    // Where plane 1 starts, in bytes from the first plane byte, and its row stride.
    offset1: u32,
    stride1: u32,
    // NV12 in 16.16 fixed point: luma offset and scale, chroma scale, then Cr to R,
    // Cb to G, Cr to G and Cb to B.
    y_offset: i32,
    y_scale: i32,
    c_scale: i32,
    cr_r: i32,
    cb_g: i32,
    cr_g: i32,
    cb_b: i32,
    // The size of the texture written: the frame's, or smaller each way.
    to_width: u32,
    to_height: u32,
}

@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(1) @binding(0) var<storage, read> src: array<u32>;
// 0..256: an sRGB byte's linear value. 256..512: an alpha byte over 255.
@group(1) @binding(1) var<storage, read> lut: array<f32, 512>;

const DATA: u32 = 16u;

fn header() -> Header {
    return Header(
        src[0], src[1], src[2], src[3], src[4], src[5],
        bitcast<i32>(src[6]), bitcast<i32>(src[7]), bitcast<i32>(src[8]),
        bitcast<i32>(src[9]), bitcast<i32>(src[10]), bitcast<i32>(src[11]),
        bitcast<i32>(src[12]), src[13], src[14],
    );
}

fn byte_at(i: u32) -> u32 {
    return (src[DATA + i / 4u] >> ((i % 4u) * 8u)) & 0xffu;
}

fn channel(v: i32) -> u32 {
    return u32(clamp((v + 32768) >> 16u, 0, 255));
}

fn opaque(r: u32, g: u32, b: u32) -> vec4<f32> {
    return vec4<f32>(lut[r], lut[g], lut[b], 1.0);
}

// The frame's texel at (x, y), premultiplied linear.
fn texel(h: Header, x: u32, y: u32) -> vec4<f32> {
    if (h.mode == 3u) {
        let luma = i32(byte_at(y * h.stride0 + x));
        let c = h.offset1 + (y / 2u) * h.stride1 + (x / 2u) * 2u;
        let cb = ((i32(byte_at(c)) - 128) * h.c_scale) >> 16u;
        let cr = ((i32(byte_at(c + 1u)) - 128) * h.c_scale) >> 16u;
        let l = (luma - h.y_offset) * h.y_scale;
        return opaque(
            channel(l + cr * h.cr_r),
            channel(l - cb * h.cb_g - cr * h.cr_g),
            channel(l + cb * h.cb_b),
        );
    }
    let at = y * h.stride0 + x * 4u;
    let p0 = byte_at(at);
    let p1 = byte_at(at + 1u);
    let p2 = byte_at(at + 2u);
    if (h.mode == 0u) {
        let a = lut[256u + byte_at(at + 3u)];
        return vec4<f32>(lut[p0] * a, lut[p1] * a, lut[p2] * a, a);
    } else if (h.mode == 1u) {
        return opaque(p0, p1, p2);
    }
    return opaque(p2, p1, p0);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let h = header();
    if (id.x >= h.to_width || id.y >= h.to_height) {
        return;
    }
    if (h.to_width == h.width && h.to_height == h.height) {
        textureStore(dst, vec2<u32>(id.xy), texel(h, id.x, id.y));
        return;
    }
    // The frame texels under this one, at least one each way. A side is at most the
    // adapter's texture limit, so the products fit a `u32`.
    let x0 = (id.x * h.width) / h.to_width;
    let x1 = max(x0 + 1u, ((id.x + 1u) * h.width) / h.to_width);
    let y0 = (id.y * h.height) / h.to_height;
    let y1 = max(y0 + 1u, ((id.y + 1u) * h.height) / h.to_height);
    var sum = vec4<f32>(0.0);
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            sum += texel(h, x, y);
        }
    }
    textureStore(dst, vec2<u32>(id.xy), sum / f32((x1 - x0) * (y1 - y0)));
}
