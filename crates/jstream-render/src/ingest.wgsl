// CPU frames into premultiplied linear `Rgba16Float` on the GPU (ADR-0117 §2).
//
// The bytes arrive as written, in a storage buffer: a 16-word header, then the planes.
// Each invocation converts one texel with the same arithmetic the CPU conversions used:
// the sRGB decode and the alpha are read from tables the CPU built (`lut`), so the
// floats multiplied are the CPU's, and NV12 is converted in the same 16.16 fixed point.
// Only the final rounding to half precision is the GPU's.

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
        bitcast<i32>(src[12]),
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

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let h = header();
    if (id.x >= h.width || id.y >= h.height) {
        return;
    }
    var out: vec4<f32>;
    if (h.mode == 3u) {
        let y = i32(byte_at(id.y * h.stride0 + id.x));
        let c = h.offset1 + (id.y / 2u) * h.stride1 + (id.x / 2u) * 2u;
        let cb = ((i32(byte_at(c)) - 128) * h.c_scale) >> 16u;
        let cr = ((i32(byte_at(c + 1u)) - 128) * h.c_scale) >> 16u;
        let l = (y - h.y_offset) * h.y_scale;
        out = opaque(
            channel(l + cr * h.cr_r),
            channel(l - cb * h.cb_g - cr * h.cr_g),
            channel(l + cb * h.cb_b),
        );
    } else {
        let at = id.y * h.stride0 + id.x * 4u;
        let p0 = byte_at(at);
        let p1 = byte_at(at + 1u);
        let p2 = byte_at(at + 2u);
        if (h.mode == 0u) {
            let a = lut[256u + byte_at(at + 3u)];
            out = vec4<f32>(lut[p0] * a, lut[p1] * a, lut[p2] * a, a);
        } else if (h.mode == 1u) {
            out = opaque(p0, p1, p2);
        } else {
            out = opaque(p2, p1, p0);
        }
    }
    textureStore(dst, vec2<u32>(id.xy), out);
}
