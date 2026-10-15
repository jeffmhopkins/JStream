//! A baseline JPEG encoder for the preview stream's fallback codec (ADR-0045): JFIF,
//! YCbCr 4:2:0, the example quantization and Huffman tables of ITU-T T.81 Annex K,
//! scaled for a quality. Small pictures only (a preview is at most 1920 × 1080), so it
//! favours being short and obviously right over speed: a plain separable DCT, no SIMD.
//!
//! Written here rather than taken as a dependency: the maintained pure-Rust encoders
//! carry the IJG licence, which is not on the `deny.toml` allow list (R-DEP-001).

use std::f32::consts::PI;
use std::sync::OnceLock;

/// The example luminance quantization table (T.81 Table K.1), in natural order.
const LUMA_Q: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// The example chrominance quantization table (T.81 Table K.2), in natural order.
const CHROMA_Q: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// The natural-order index of each zig-zag position.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// A Huffman table as T.81 writes it: how many codes of each length 1 to 16, then the
/// symbols in code order.
struct Spec {
    bits: [u8; 16],
    values: &'static [u8],
}

/// T.81 Table K.3.
const DC_LUMA: Spec = Spec {
    bits: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// T.81 Table K.4.
const DC_CHROMA: Spec = Spec {
    bits: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// T.81 Table K.5.
const AC_LUMA: Spec = Spec {
    bits: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    values: &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
};

/// T.81 Table K.6.
const AC_CHROMA: Spec = Spec {
    bits: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    values: &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
};

/// The code and its length for each symbol, built from a [`Spec`] (T.81 Annex C).
struct Codes {
    code: [u16; 256],
    len: [u8; 256],
}

impl Codes {
    fn new(spec: &Spec) -> Self {
        let mut codes = Codes {
            code: [0; 256],
            len: [0; 256],
        };
        let mut code: u16 = 0;
        let mut values = spec.values.iter();
        for (i, &count) in spec.bits.iter().enumerate() {
            for _ in 0..count {
                if let Some(&value) = values.next() {
                    codes.code[usize::from(value)] = code;
                    codes.len[usize::from(value)] = (i + 1) as u8;
                }
                code = code.wrapping_add(1);
            }
            code <<= 1;
        }
        codes
    }
}

/// The four Huffman tables, built once.
struct Tables {
    dc: [Codes; 2],
    ac: [Codes; 2],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| Tables {
        dc: [Codes::new(&DC_LUMA), Codes::new(&DC_CHROMA)],
        ac: [Codes::new(&AC_LUMA), Codes::new(&AC_CHROMA)],
    })
}

/// `cos((2x + 1) u π / 16)`, with the DCT's `C(u)` scale folded in: `[u][x]`.
fn dct_basis() -> &'static [[f32; 8]; 8] {
    static BASIS: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    BASIS.get_or_init(|| {
        let mut basis = [[0.0; 8]; 8];
        for (u, row) in basis.iter_mut().enumerate() {
            let scale = if u == 0 { 0.5_f32.sqrt() } else { 1.0 } * 0.5;
            for (x, value) in row.iter_mut().enumerate() {
                *value = scale * (((2 * x + 1) * u) as f32 * PI / 16.0).cos();
            }
        }
        basis
    })
}

/// A quantization table for `quality` (1 to 100), scaled as libjpeg's
/// `jpeg_quality_scaling` does, in natural order.
fn scaled(base: &[u8; 64], quality: u8) -> [u8; 64] {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - 2 * quality
    };
    base.map(|q| ((u32::from(q) * scale + 50) / 100).clamp(1, 255) as u8)
}

/// Appends whole bytes and stuffs a 0 after every 0xFF, as entropy-coded data needs.
struct Bits<'a> {
    out: &'a mut Vec<u8>,
    acc: u32,
    count: u32,
}

impl Bits<'_> {
    /// Writes the low `len` bits of `value`, most significant first.
    fn put(&mut self, value: u16, len: u8) {
        let len = u32::from(len);
        self.acc = (self.acc << len) | (u32::from(value) & ((1 << len) - 1));
        self.count += len;
        while self.count >= 8 {
            self.count -= 8;
            let byte = (self.acc >> self.count) as u8;
            self.out.push(byte);
            if byte == 0xff {
                self.out.push(0);
            }
        }
        self.acc &= (1 << self.count) - 1;
    }

    /// Pads the last byte with 1 bits.
    fn flush(&mut self) {
        if self.count > 0 {
            let pad = 8 - self.count as u8;
            self.put((1 << pad) - 1, pad);
        }
    }
}

/// The bit length of `|value|`: the JPEG magnitude category.
fn category(value: i32) -> u8 {
    (32 - value.unsigned_abs().leading_zeros()) as u8
}

/// The low `category` bits that encode `value` (negative values one less, T.81 F.1.2.1).
fn magnitude(value: i32, category: u8) -> u16 {
    let value = if value < 0 { value - 1 } else { value };
    (value & ((1 << category) - 1)) as u16
}

/// One 8 × 8 block of samples, level-shifted (−128 to 127), in natural order.
type Block = [f32; 64];

/// Transforms, quantizes and entropy-codes one block. `predictor` is the component's
/// previous DC value; `table` picks the luma (0) or chroma (1) tables.
fn encode_block(
    bits: &mut Bits<'_>,
    block: &Block,
    quant: &[u8; 64],
    table: usize,
    predictor: &mut i32,
) {
    let basis = dct_basis();
    // Rows, then columns.
    let mut rows = [0.0_f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            rows[y * 8 + u] = (0..8).map(|x| basis[u][x] * block[y * 8 + x]).sum();
        }
    }
    let mut coefficients = [0_i32; 64];
    for (v, basis_v) in basis.iter().enumerate() {
        for u in 0..8 {
            let value: f32 = (0..8).map(|y| basis_v[y] * rows[y * 8 + u]).sum();
            let i = v * 8 + u;
            coefficients[i] = (value / f32::from(quant[i])).round() as i32;
        }
    }
    let tables = tables();
    let (dc, ac) = (&tables.dc[table], &tables.ac[table]);
    let diff = coefficients[0] - *predictor;
    *predictor = coefficients[0];
    let size = category(diff);
    bits.put(dc.code[usize::from(size)], dc.len[usize::from(size)]);
    if size > 0 {
        bits.put(magnitude(diff, size), size);
    }
    let mut run = 0;
    for &index in &ZIGZAG[1..] {
        let value = coefficients[index];
        if value == 0 {
            run += 1;
            continue;
        }
        while run >= 16 {
            bits.put(ac.code[0xf0], ac.len[0xf0]);
            run -= 16;
        }
        let size = category(value);
        let symbol = usize::from((run << 4) | size);
        bits.put(ac.code[symbol], ac.len[symbol]);
        bits.put(magnitude(value, size), size);
        run = 0;
    }
    if run > 0 {
        bits.put(ac.code[0], ac.len[0]);
    }
}

/// Appends a marker segment: `FF marker`, its length, `body`.
fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

fn huffman_segment(out: &mut Vec<u8>, class_id: u8, spec: &Spec) {
    let mut body = vec![class_id];
    body.extend_from_slice(&spec.bits);
    body.extend_from_slice(spec.values);
    segment(out, 0xc4, &body);
}

/// Encodes a `width` × `height` picture of tightly packed RGBA8 (alpha is ignored) as a
/// baseline JFIF JPEG at `quality` (1 to 100), into `out` (replacing what it held).
///
/// Returns `false`, writing nothing, when a side is 0 or over 65535 or `rgba` is not
/// `width * height * 4` bytes.
pub(crate) fn encode(width: u32, height: u32, rgba: &[u8], quality: u8, out: &mut Vec<u8>) -> bool {
    out.clear();
    let (Ok(w16), Ok(h16)) = (u16::try_from(width), u16::try_from(height)) else {
        return false;
    };
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return false;
    }
    let luma_q = scaled(&LUMA_Q, quality);
    let chroma_q = scaled(&CHROMA_Q, quality);
    out.extend_from_slice(&[0xff, 0xd8]);
    segment(out, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    for (id, table) in [(0_u8, &luma_q), (1, &chroma_q)] {
        let mut body = vec![id];
        body.extend(ZIGZAG.iter().map(|&i| table[i]));
        segment(out, 0xdb, &body);
    }
    let [wh, wl] = w16.to_be_bytes();
    let [hh, hl] = h16.to_be_bytes();
    segment(
        out,
        0xc0,
        &[8, hh, hl, wh, wl, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1],
    );
    huffman_segment(out, 0x00, &DC_LUMA);
    huffman_segment(out, 0x10, &AC_LUMA);
    huffman_segment(out, 0x01, &DC_CHROMA);
    huffman_segment(out, 0x11, &AC_CHROMA);
    segment(out, 0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    // One sample of a channel at (x, y), clamped to the picture's edge, as YCbCr
    // (JFIF's full-range BT.601).
    let ycc = |x: usize, y: usize| {
        let at = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        let [r, g, b] = [rgba[at], rgba[at + 1], rgba[at + 2]].map(f32::from);
        [
            0.299 * r + 0.587 * g + 0.114 * b - 128.0,
            -0.168_736 * r - 0.331_264 * g + 0.5 * b,
            0.5 * r - 0.418_688 * g - 0.081_312 * b,
        ]
    };
    let mut bits = Bits {
        out,
        acc: 0,
        count: 0,
    };
    let mut predictors = [0_i32; 3];
    let mut luma = [0.0_f32; 64];
    let mut cb = [0.0_f32; 64];
    let mut cr = [0.0_f32; 64];
    for my in (0..h).step_by(16) {
        for mx in (0..w).step_by(16) {
            // The four luma blocks, and the chroma averaged over each 2 × 2.
            let mut chroma_sum = [[0.0_f32; 2]; 64];
            for by in 0..2 {
                for bx in 0..2 {
                    for y in 0..8 {
                        for x in 0..8 {
                            let (px, py) = (mx + bx * 8 + x, my + by * 8 + y);
                            let [l, b, r] = ycc(px, py);
                            luma[y * 8 + x] = l;
                            let c = (by * 4 + y / 2) * 8 + bx * 4 + x / 2;
                            chroma_sum[c][0] += b;
                            chroma_sum[c][1] += r;
                        }
                    }
                    encode_block(&mut bits, &luma, &luma_q, 0, &mut predictors[0]);
                }
            }
            for (i, [b, r]) in chroma_sum.into_iter().enumerate() {
                cb[i] = b / 4.0;
                cr[i] = r / 4.0;
            }
            encode_block(&mut bits, &cb, &chroma_q, 1, &mut predictors[1]);
            encode_block(&mut bits, &cr, &chroma_q, 1, &mut predictors[2]);
        }
    }
    bits.flush();
    out.extend_from_slice(&[0xff, 0xd9]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;

    fn decode(jpeg: &[u8]) -> (usize, usize, Vec<u8>) {
        let mut decoder = JpegDecoder::new(ZCursor::new(jpeg));
        let pixels = decoder.decode().expect("decodes");
        let info = decoder.info().expect("has info");
        (usize::from(info.width), usize::from(info.height), pixels)
    }

    fn picture(width: usize, height: usize, at: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&at(x, y));
                rgba.push(255);
            }
        }
        rgba
    }

    /// The largest difference of any channel between `rgba` and the decoded RGB.
    fn worst(rgba: &[u8], rgb: &[u8]) -> u8 {
        rgba.chunks_exact(4)
            .zip(rgb.chunks_exact(3))
            .flat_map(|(a, b)| (0..3).map(move |i| a[i].abs_diff(b[i])))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn the_huffman_specs_are_complete() {
        for spec in [&DC_LUMA, &DC_CHROMA, &AC_LUMA, &AC_CHROMA] {
            let count: usize = spec.bits.iter().map(|&b| usize::from(b)).sum();
            assert_eq!(count, spec.values.len());
        }
    }

    #[test]
    fn solid_colours_round_trip_closely() {
        for colour in [[0, 0, 0], [255, 255, 255], [200, 30, 90], [17, 160, 240]] {
            let rgba = picture(40, 24, |_, _| colour);
            let mut jpeg = Vec::new();
            assert!(encode(40, 24, &rgba, 75, &mut jpeg));
            let (w, h, rgb) = decode(&jpeg);
            assert_eq!((w, h), (40, 24));
            assert!(
                worst(&rgba, &rgb) <= 4,
                "{colour:?}: off by {}",
                worst(&rgba, &rgb)
            );
        }
    }

    #[test]
    fn a_gradient_with_odd_sides_round_trips() {
        let (w, h) = (37, 21);
        let rgba = picture(w, h, |x, y| {
            [(x * 6) as u8, (y * 12) as u8, ((x + y) * 4) as u8]
        });
        let mut jpeg = Vec::new();
        assert!(encode(w as u32, h as u32, &rgba, 90, &mut jpeg));
        let (dw, dh, rgb) = decode(&jpeg);
        assert_eq!((dw, dh), (w, h));
        // Chroma is subsampled, so allow for it at the sharpest edges.
        assert!(worst(&rgba, &rgb) <= 24, "off by {}", worst(&rgba, &rgb));
        let mean: f64 = rgba
            .chunks_exact(4)
            .zip(rgb.chunks_exact(3))
            .flat_map(|(a, b)| (0..3).map(move |i| f64::from(a[i].abs_diff(b[i]))))
            .sum::<f64>()
            / (w * h * 3) as f64;
        assert!(mean < 3.0, "mean error {mean}");
    }

    #[test]
    fn bad_input_writes_nothing() {
        let mut jpeg = vec![1, 2, 3];
        assert!(!encode(0, 4, &[], 75, &mut jpeg));
        assert!(jpeg.is_empty());
        assert!(!encode(2, 2, &[0; 15], 75, &mut jpeg));
        assert!(!encode(70_000, 1, &[0; 280_000], 75, &mut jpeg));
    }
}
