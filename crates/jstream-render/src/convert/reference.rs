//! The CPU reference the GPU conversion is checked against: the same formulas in `f64`,
//! written independently of the shader.

use jstream_media::ColorRange;

const KR: f64 = 0.2126;
const KB: f64 = 0.0722;
const KG: f64 = 1.0 - KR - KB;

fn decode(byte: u8) -> f64 {
    let c = f64::from(byte) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// R'G'B' of a straight-alpha sRGB pixel composited over black.
fn rgb_prime(px: [u8; 4]) -> [f64; 3] {
    let a = f64::from(px[3]) / 255.0;
    [0, 1, 2].map(|i| encode((decode(px[i]) * a).clamp(0.0, 1.0)))
}

fn luma(rgb: [f64; 3]) -> f64 {
    KR * rgb[0] + KG * rgb[1] + KB * rgb[2]
}

fn chroma(rgb: [f64; 3]) -> [f64; 2] {
    let y = luma(rgb);
    [
        (rgb[2] - y) / (2.0 * (1.0 - KB)),
        (rgb[0] - y) / (2.0 * (1.0 - KR)),
    ]
}

fn code(v: f64) -> u8 {
    // In range by the clamp, so the cast cannot truncate.
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

/// Tightly packed straight-alpha sRGB RGBA8 to packed NV12, BT.709, left-sited chroma
/// with a `[1 2 1]` tap across columns, averaged over the block's two rows.
pub(super) fn rgba8_to_nv12(
    rgba: &[u8],
    width: usize,
    height: usize,
    range: ColorRange,
) -> Vec<u8> {
    let (ys, yo, cs, co) = match range {
        ColorRange::Limited => (219.0, 16.0, 224.0, 128.0),
        ColorRange::Full => (255.0, 0.0, 255.0, 128.0),
    };
    let px = |x: usize, y: usize| {
        let o = (y * width + x) * 4;
        rgb_prime([rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]])
    };
    let mut out = Vec::with_capacity(width * height * 3 / 2);
    for y in 0..height {
        for x in 0..width {
            out.push(code(yo + ys * luma(px(x, y))));
        }
    }
    for by in (0..height).step_by(2) {
        for bx in (0..width).step_by(2) {
            let mut sum = [0.0; 2];
            for row in [by, by + 1] {
                for (x, w) in [
                    (bx.saturating_sub(1), 1.0),
                    (bx, 2.0),
                    ((bx + 1).min(width - 1), 1.0),
                ] {
                    let c = chroma(px(x, row));
                    sum[0] += w * c[0];
                    sum[1] += w * c[1];
                }
            }
            out.push(code(co + cs * sum[0] / 8.0));
            out.push(code(co + cs * sum[1] / 8.0));
        }
    }
    out
}
