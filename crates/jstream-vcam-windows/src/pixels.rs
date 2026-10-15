//! CPU pixel work for the samples: the fallback test pattern, strided BGRA copies and
//! BGRA to NV12. Platform-independent, so its tests run on any host.

/// Draws the DLL's own test pattern, shown while no producer is live, into a tightly packed
/// BGRA frame: 75% colour bars, plus a vertical black-and-white line that moves with
/// `frame`. A viewer can tell it apart from the engine's picture at a glance.
///
/// Rows beyond `bgra.len()` are left out, so a short buffer draws a partial frame.
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
    if width == 0 {
        return;
    }
    let line_x = usize::try_from(frame.wrapping_mul(8) % width as u64).unwrap_or(0);
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

/// Copies a `width` x `height` BGRA image with row stride `src_stride` into `dst` with row
/// stride `dst_stride`. Returns `false`, copying nothing, if either buffer is too short.
pub fn copy_bgra(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
) -> bool {
    let row = width * 4;
    if height == 0 {
        return true;
    }
    let need = |stride: usize| {
        stride
            .checked_mul(height - 1)
            .and_then(|n| n.checked_add(row))
    };
    let fits = |len: usize, stride: usize| stride >= row && need(stride).is_some_and(|n| n <= len);
    if !fits(src.len(), src_stride) || !fits(dst.len(), dst_stride) {
        return false;
    }
    for y in 0..height {
        dst[y * dst_stride..][..row].copy_from_slice(&src[y * src_stride..][..row]);
    }
    true
}

/// The size in bytes of a tightly packed NV12 frame.
pub fn nv12_size(width: usize, height: usize) -> usize {
    width * height * 3 / 2
}

/// Converts BGRA with row stride `src_stride` to tightly packed NV12, BT.709 limited range.
/// `width` and `height` must be even. The 8-bit chroma coefficients are rounded so each
/// row sums to zero, which keeps greys exactly neutral (128). Returns `false`, writing
/// nothing, if a size is odd or a buffer is too short.
pub fn bgra_to_nv12(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    width: usize,
    height: usize,
) -> bool {
    let src_ok = height == 0
        || (src_stride >= width * 4 && src.len() >= src_stride * (height - 1) + width * 4);
    if !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || !src_ok
        || dst.len() < nv12_size(width, height)
    {
        return false;
    }
    let (y_plane, uv_plane) = dst.split_at_mut(width * height);
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let (mut su, mut sv) = (0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let o = (y + dy) * src_stride + (x + dx) * 4;
                let (b, g, r) = (
                    i32::from(src[o]),
                    i32::from(src[o + 1]),
                    i32::from(src[o + 2]),
                );
                y_plane[(y + dy) * width + x + dx] = luma(r, g, b);
                su += (-26 * r - 86 * g + 112 * b + 128) >> 8;
                sv += (112 * r - 102 * g - 10 * b + 128) >> 8;
            }
            let uv = (y / 2) * width + x;
            uv_plane[uv] = clamp_u8(128 + su / 4, 16, 240);
            uv_plane[uv + 1] = clamp_u8(128 + sv / 4, 16, 240);
        }
    }
    true
}

fn luma(r: i32, g: i32, b: i32) -> u8 {
    clamp_u8(16 + ((47 * r + 157 * g + 16 * b + 128) >> 8), 16, 235)
}

/// `v` clamped to `lo..=hi`, both within `u8`.
fn clamp_u8(v: i32, lo: u8, hi: u8) -> u8 {
    u8::try_from(v.clamp(i32::from(lo), i32::from(hi))).unwrap_or(hi)
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
        assert!(bgra_to_nv12(&src, w * 4, &mut dst, w, h));
        assert_eq!(&dst[..4], &[235, 235, 16, 16]);
        // Neutral chroma for both greys.
        assert_eq!(&dst[w * h..], &[128, 128, 128, 128]);
    }

    #[test]
    fn nv12_primaries_land_in_range() {
        // Pure red, green and blue stay within limited range and move chroma the right way.
        let (w, h) = (2, 2);
        for (bgr, u_dir, v_dir) in [
            ([0u8, 0, 255], -1, 1),
            ([0, 255, 0], -1, -1),
            ([255, 0, 0], 1, -1),
        ] {
            let src: Vec<u8> = [bgr[0], bgr[1], bgr[2], 255].repeat(w * h);
            let mut dst = vec![0u8; nv12_size(w, h)];
            assert!(bgra_to_nv12(&src, w * 4, &mut dst, w, h));
            assert!(dst[..w * h].iter().all(|&y| (16..=235).contains(&y)));
            let (u, v) = (i32::from(dst[w * h]), i32::from(dst[w * h + 1]));
            assert_eq!((u - 128).signum(), u_dir, "{bgr:?}");
            assert_eq!((v - 128).signum(), v_dir, "{bgr:?}");
        }
    }

    #[test]
    fn nv12_rejects_odd_sizes_and_short_buffers() {
        let src = vec![0u8; 4 * 4 * 4];
        let mut dst = vec![7u8; nv12_size(4, 4)];
        assert!(!bgra_to_nv12(&src, 16, &mut dst, 3, 4));
        assert!(!bgra_to_nv12(&src, 16, &mut dst[..10], 4, 4));
        assert!(!bgra_to_nv12(&src[..20], 16, &mut dst, 4, 4));
        assert!(!bgra_to_nv12(&src, 8, &mut dst, 4, 4));
        assert!(dst.iter().all(|&b| b == 7));
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
        // Huge frame numbers and a zero width neither panic nor draw out of bounds.
        test_pattern(&mut a, w, h, u64::MAX);
        test_pattern(&mut a, 0, h, 3);
    }

    #[test]
    fn copy_respects_strides() {
        let src = [1u8, 2, 3, 4, 9, 9, 5, 6, 7, 8, 9, 9];
        let mut dst = [0u8; 8];
        assert!(copy_bgra(&src, 6, &mut dst, 4, 1, 2));
        assert_eq!(dst, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn copy_refuses_short_buffers() {
        let src = [1u8; 9];
        let mut dst = [0u8; 8];
        assert!(!copy_bgra(&src, 6, &mut dst, 4, 1, 2));
        assert!(!copy_bgra(&[1u8; 12], 6, &mut dst[..7], 4, 1, 2));
        assert!(!copy_bgra(&[1u8; 12], 2, &mut dst, 4, 1, 2));
        assert_eq!(dst, [0u8; 8]);
        assert!(copy_bgra(&[], 0, &mut [], 0, 5, 0));
    }
}
