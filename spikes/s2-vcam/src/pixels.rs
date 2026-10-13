//! CPU pixel work in the DLL: the fallback test pattern and BGRA → NV12.
//!
//! Platform-independent, so its tests run on any host.

/// Draws the DLL's own test pattern (shown when no producer is live) into a tightly
/// packed BGRA frame: 75% colour bars, plus a vertical black-and-white line that moves
/// with `frame`. It is deliberately unlike the producer's picture (dark teal with a white
/// bar), so a viewer can tell which one Frame Server is showing.
pub fn test_pattern(bgra: &mut [u8], width: usize, height: usize, frame: u64) {
    // White, yellow, cyan, green, magenta, red, blue, black at 75% (as B, G, R).
    const BARS: [[u8; 3]; 8] = [
        [191, 191, 191],
        [0, 191, 191],
        [191, 191, 0],
        [0, 191, 0],
        [191, 0, 191],
        [0, 0, 191],
        [191, 0, 0],
        [0, 0, 0],
    ];
    let line_x = (frame as usize * 8) % width.max(1);
    for (y, row) in bgra.chunks_exact_mut(width * 4).take(height).enumerate() {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let [b, g, r] = if x.abs_diff(line_x) < 4 {
                if (y / 32) % 2 == 0 { [255; 3] } else { [0; 3] }
            } else {
                BARS[x * BARS.len() / width]
            };
            px.copy_from_slice(&[b, g, r, 255]);
        }
    }
}

/// Copies a BGRA frame with source stride `src_stride` into `dst` with stride `dst_stride`.
pub fn copy_bgra(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
) {
    let row = width * 4;
    for y in 0..height {
        let s = &src[y * src_stride..y * src_stride + row];
        dst[y * dst_stride..y * dst_stride + row].copy_from_slice(s);
    }
}

/// The size in bytes of a tightly packed NV12 frame.
pub fn nv12_size(width: usize, height: usize) -> usize {
    width * height * 3 / 2
}

/// Converts BGRA (stride `src_stride`) to tightly packed NV12, BT.709 limited range.
/// `width` and `height` must be even. The 8-bit chroma coefficients are rounded so each
/// row sums to zero, which keeps greys exactly neutral (128).
pub fn bgra_to_nv12(src: &[u8], src_stride: usize, dst: &mut [u8], width: usize, height: usize) {
    let (y_plane, uv_plane) = dst.split_at_mut(width * height);
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let (mut su, mut sv) = (0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let o = (y + dy) * src_stride + (x + dx) * 4;
                let (b, g, r) = (src[o] as i32, src[o + 1] as i32, src[o + 2] as i32);
                y_plane[(y + dy) * width + x + dx] = luma(r, g, b);
                su += (-26 * r - 86 * g + 112 * b + 128) >> 8;
                sv += (112 * r - 102 * g - 10 * b + 128) >> 8;
            }
            let uv = (y / 2) * width + x;
            uv_plane[uv] = (128 + su / 4).clamp(16, 240) as u8;
            uv_plane[uv + 1] = (128 + sv / 4).clamp(16, 240) as u8;
        }
    }
}

fn luma(r: i32, g: i32, b: i32) -> u8 {
    (16 + ((47 * r + 157 * g + 16 * b + 128) >> 8)).clamp(16, 235) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_black_and_white() {
        let (w, h) = (4, 2);
        let mut src = vec![0u8; w * h * 4];
        // Left half white, right half black.
        for y in 0..h {
            for x in 0..2 {
                src[(y * w + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let mut dst = vec![0u8; nv12_size(w, h)];
        bgra_to_nv12(&src, w * 4, &mut dst, w, h);
        assert_eq!(&dst[..4], &[235, 235, 16, 16]);
        // Neutral chroma for both greys.
        assert_eq!(&dst[w * h..], &[128, 128, 128, 128]);
    }

    #[test]
    fn pattern_fills_frame_and_moves() {
        let (w, h) = (64, 8);
        let mut a = vec![0u8; w * h * 4];
        let mut b = a.clone();
        test_pattern(&mut a, w, h, 0);
        test_pattern(&mut b, w, h, 1);
        assert!(a.chunks_exact(4).all(|p| p[3] == 255));
        assert_ne!(a, b);
    }

    #[test]
    fn copy_respects_strides() {
        let src = [1u8, 2, 3, 4, 9, 9, 5, 6, 7, 8, 9, 9];
        let mut dst = [0u8; 8];
        copy_bgra(&src, 6, &mut dst, 4, 1, 2);
        assert_eq!(dst, [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
