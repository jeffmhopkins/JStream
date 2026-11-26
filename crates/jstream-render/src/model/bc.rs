//! Block compression of a model's textures on upload: BC1 for a texture whose every
//! texel is opaque, BC3 for one with alpha, each 4 × 4 block of texels in 8 or 16 bytes
//! where it held 64 (one eighth or one quarter of the memory), as every desktop GPU
//! samples them natively.
//!
//! The colour of a block is fit along the main axis its texels spread on (the covariance's
//! largest eigenvector, by power iteration), its two ends at the texels' furthest
//! projections, rounded to 5:6:5, and each texel takes the nearest of the four colours
//! those ends give. BC3's alpha takes its block's least and greatest alpha as its ends
//! and the nearest of their eight steps. A texel's code values are fit as they are
//! (sRGB-encoded for a colour texture), as a GPU decodes them before it filters.

/// Bytes of a BC1 block.
pub(crate) const BC1_BLOCK: usize = 8;
/// Bytes of a BC3 block.
pub(crate) const BC3_BLOCK: usize = 16;

/// `rgba` (`width` × `height` texels of RGBA8, row after row) compressed: as BC3 when
/// `alpha`, else BC1. A block past an edge repeats the edge's texels.
pub(crate) fn compress(rgba: &[u8], width: u32, height: u32, alpha: bool) -> Vec<u8> {
    let (bw, bh) = (width.div_ceil(4), height.div_ceil(4));
    let mut out =
        Vec::with_capacity((bw * bh) as usize * if alpha { BC3_BLOCK } else { BC1_BLOCK });
    let mut block = [[0u8; 4]; 16];
    for by in 0..bh {
        for bx in 0..bw {
            for (i, texel) in block.iter_mut().enumerate() {
                let x = (bx * 4 + (i % 4) as u32).min(width - 1);
                let y = (by * 4 + (i / 4) as u32).min(height - 1);
                let at = ((y * width + x) * 4) as usize;
                texel.copy_from_slice(&rgba[at..at + 4]);
            }
            if alpha {
                out.extend_from_slice(&alpha_block(&block));
            }
            out.extend_from_slice(&colour_block(&block));
        }
    }
    out
}

/// Whether every texel of `rgba` is opaque.
pub(crate) fn opaque(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|texel| texel[3] == 255)
}

fn to_565(c: [f32; 3]) -> u16 {
    let q = |v: f32, max: f32| (v.clamp(0.0, 255.0) / 255.0 * max).round() as u16;
    (q(c[0], 31.0) << 11) | (q(c[1], 63.0) << 5) | q(c[2], 31.0)
}

fn from_565(c: u16) -> [f32; 3] {
    let e = |v: u16, max: f32| f32::from(v) * 255.0 / max;
    [e(c >> 11, 31.0), e((c >> 5) & 63, 63.0), e(c & 31, 31.0)]
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum()
}

/// A block's colour, in BC1's four-colour mode.
fn colour_block(block: &[[u8; 4]; 16]) -> [u8; 8] {
    let texels = block.map(|t| [f32::from(t[0]), f32::from(t[1]), f32::from(t[2])]);
    let mean = texels
        .iter()
        .fold([0.0f32; 3], |s, t| [s[0] + t[0], s[1] + t[1], s[2] + t[2]])
        .map(|v| v / 16.0);
    let mut cov = [[0.0f32; 3]; 3];
    for t in &texels {
        let d = [t[0] - mean[0], t[1] - mean[1], t[2] - mean[2]];
        for (i, row) in cov.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c += d[i] * d[j];
            }
        }
    }
    // Started from the covariance's largest row, which leans toward the main axis
    // (a fixed start can be square to it, and stay so).
    let norm = |v: &[f32; 3]| v.iter().map(|c| c * c).sum::<f32>();
    let mut axis = *cov
        .iter()
        .max_by(|a, b| norm(a).total_cmp(&norm(b)))
        .unwrap_or(&[1.0, 1.0, 1.0]);
    for _ in 0..8 {
        let next = [0, 1, 2].map(|i| (0..3).map(|j| cov[i][j] * axis[j]).sum::<f32>());
        let len = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-6 {
            break;
        }
        axis = next.map(|v| v / len);
    }
    let len = norm(&axis).sqrt();
    axis = if len < 1e-6 {
        [0.0; 3]
    } else {
        axis.map(|v| v / len)
    };
    let along = |t: &[f32; 3]| (0..3).map(|i| (t[i] - mean[i]) * axis[i]).sum::<f32>();
    let (lo, hi) = texels.iter().fold((f32::MAX, f32::MIN), |(lo, hi), t| {
        let p = along(t);
        (lo.min(p), hi.max(p))
    });
    let at = |p: f32| [0, 1, 2].map(|i| mean[i] + axis[i] * p);
    let (mut c0, mut c1) = (to_565(at(hi)), to_565(at(lo)));
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    if c0 == c1 {
        // Every index 0: the one colour (an equal pair is three-colour mode, where 0 is
        // still the first end).
        return out;
    }
    let (e0, e1) = (from_565(c0), from_565(c1));
    let mix = |a: f32, b: f32| [0, 1, 2].map(|i| (e0[i] * a + e1[i] * b) / 3.0);
    let palette = [e0, e1, mix(2.0, 1.0), mix(1.0, 2.0)];
    let mut indices = 0u32;
    for (i, t) in texels.iter().enumerate() {
        let best = (0..4)
            .min_by(|&a, &b| distance(*t, palette[a]).total_cmp(&distance(*t, palette[b])))
            .unwrap_or(0);
        indices |= (best as u32) << (i * 2);
    }
    out[4..8].copy_from_slice(&indices.to_le_bytes());
    out
}

/// A block's alpha, as BC3 (BC4) holds it, in its eight-step mode.
fn alpha_block(block: &[[u8; 4]; 16]) -> [u8; 8] {
    let (lo, hi) = block
        .iter()
        .fold((255u8, 0u8), |(lo, hi), t| (lo.min(t[3]), hi.max(t[3])));
    let mut out = [0u8; 8];
    out[0] = hi;
    out[1] = lo;
    if hi == lo {
        return out;
    }
    let (a0, a1) = (f32::from(hi), f32::from(lo));
    // Index 0 and 1 are the ends, 2 to 7 the steps between them from a0 toward a1.
    let step = |k: u32| match k {
        0 => a0,
        1 => a1,
        _ => (a0 * (8 - k) as f32 + a1 * (k - 1) as f32) / 7.0,
    };
    let mut bits = 0u64;
    for (i, t) in block.iter().enumerate() {
        let a = f32::from(t[3]);
        let best = (0..8u32)
            .min_by(|&x, &y| (a - step(x)).abs().total_cmp(&(a - step(y)).abs()))
            .unwrap_or(0);
        bits |= u64::from(best) << (i * 3);
    }
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
}

#[cfg(test)]
mod tests {
    use super::{BC1_BLOCK, BC3_BLOCK, compress, from_565};

    /// Decodes one BC1 colour block to its 16 texels' RGB, four-colour mode.
    fn decode_colour(b: &[u8]) -> Vec<[f32; 3]> {
        let c0 = u16::from_le_bytes([b[0], b[1]]);
        let c1 = u16::from_le_bytes([b[2], b[3]]);
        let (e0, e1) = (from_565(c0), from_565(c1));
        let palette = if c0 > c1 {
            let mix = |a: f32, b: f32| [0, 1, 2].map(|i| (e0[i] * a + e1[i] * b) / 3.0);
            [e0, e1, mix(2.0, 1.0), mix(1.0, 2.0)]
        } else {
            let half = [0, 1, 2].map(|i| (e0[i] + e1[i]) / 2.0);
            [e0, e1, half, [0.0; 3]]
        };
        let indices = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        (0..16)
            .map(|i| palette[((indices >> (i * 2)) & 3) as usize])
            .collect()
    }

    /// Decodes one BC3 alpha block to its 16 texels' alpha.
    fn decode_alpha(b: &[u8]) -> Vec<f32> {
        let (a0, a1) = (f32::from(b[0]), f32::from(b[1]));
        let mut bytes = [0u8; 8];
        bytes[..6].copy_from_slice(&b[2..8]);
        let bits = u64::from_le_bytes(bytes);
        (0..16)
            .map(|i| {
                let k = ((bits >> (i * 3)) & 7) as u32;
                if b[0] > b[1] {
                    match k {
                        0 => a0,
                        1 => a1,
                        _ => (a0 * (8 - k) as f32 + a1 * (k - 1) as f32) / 7.0,
                    }
                } else {
                    a0
                }
            })
            .collect()
    }

    /// A block of one colour comes back as that colour, to 5:6:5's precision, and a
    /// ramp between two colours within half a step of BC1's four along it: its colours
    /// lie on its ends' line, which the fit lays along the texels' own spread (here
    /// square to the grey axis, where a fixed start for the fit would miss it).
    #[test]
    fn a_compressed_block_decodes_near_its_texels() {
        let flat: Vec<u8> = [200u8, 120, 40, 255].repeat(16);
        let out = compress(&flat, 4, 4, false);
        assert_eq!(out.len(), BC1_BLOCK);
        for texel in decode_colour(&out) {
            for (got, want) in texel.iter().zip([200.0, 120.0, 40.0]) {
                assert!((got - want).abs() <= 4.5, "{texel:?}");
            }
        }
        let ramp: Vec<u8> = (0..16u8)
            .flat_map(|i| [i * 16, 255 - i * 16, 64, 255])
            .collect();
        let decoded = decode_colour(&compress(&ramp, 4, 4, false));
        for (i, texel) in decoded.iter().enumerate() {
            let want = [f32::from(i as u8 * 16), f32::from(255 - i as u8 * 16), 64.0];
            let err = (0..3)
                .map(|c| (texel[c] - want[c]).abs())
                .fold(0.0, f32::max);
            // Four colours span the ramp's 240 code values: a texel is within half a
            // step (40) of one, and a little for 5:6:5.
            assert!(err <= 44.0, "texel {i}: {texel:?} vs {want:?}");
        }
    }

    /// BC3 keeps a block's alpha within a step of its eight, its ends exact, and its
    /// colour as BC1 does; a texture of odd size compresses in whole blocks, the edge
    /// texels repeated.
    #[test]
    fn alpha_and_odd_sizes_compress_in_whole_blocks() {
        let texels: Vec<u8> = (0..16u8).flat_map(|i| [90, 90, 90, i * 17]).collect();
        let out = compress(&texels, 4, 4, true);
        assert_eq!(out.len(), BC3_BLOCK);
        let alpha = decode_alpha(&out[..8]);
        for (i, a) in alpha.iter().enumerate() {
            let want = f32::from(i as u8 * 17);
            assert!(
                (a - want).abs() <= 255.0 / 14.0 + 0.5,
                "texel {i}: {a} vs {want}"
            );
        }
        assert_eq!((alpha[0], alpha[15]), (0.0, 255.0));
        assert_eq!(
            compress(&[1u8, 2, 3, 255].repeat(6 * 5), 6, 5, false).len(),
            4 * BC1_BLOCK
        );
    }
}
