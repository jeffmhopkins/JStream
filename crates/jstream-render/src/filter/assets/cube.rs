//! 3D colour lookup tables: a pure-Rust `.cube` parser, the tiled PNG layout, and the 2D
//! layout `apply_lut` samples.
//!
//! A table maps sRGB-encoded straight colour to sRGB-encoded straight colour, the usual
//! `.cube` convention for display-referred looks. Values are kept as `f32` and clamped
//! to `0..=1` only when the table becomes an image.

/// Largest `LUT_3D_SIZE` accepted: 64 points per side, as OBS and most tools export.
pub(crate) const MAX_SIZE: usize = 64;

/// Smallest `LUT_3D_SIZE` accepted.
pub(crate) const MIN_SIZE: usize = 2;

/// Why a lookup table could not be read.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LutError {
    /// A `.cube` file declares a 1D table (`LUT_1D_SIZE`); only 3D tables are applied.
    #[error("line {line}: 1D LUTs are not supported, only LUT_3D_SIZE")]
    OneDimensional {
        /// One-based line number.
        line: usize,
    },
    /// No `LUT_3D_SIZE` came before the data.
    #[error("the file has no LUT_3D_SIZE before its data")]
    MissingSize,
    /// `LUT_3D_SIZE` is outside `2..=64`.
    #[error("LUT_3D_SIZE {size} is outside {MIN_SIZE}..={MAX_SIZE}")]
    Size {
        /// The declared size.
        size: usize,
    },
    /// A line could not be read.
    #[error("line {line}: {detail}")]
    Line {
        /// One-based line number.
        line: usize,
        /// What is wrong with it.
        detail: String,
    },
    /// The number of data rows is not `size³`.
    #[error("expected {expected} table rows for the declared size, found {found}")]
    Count {
        /// `size³`.
        expected: usize,
        /// Rows read.
        found: usize,
    },
    /// `DOMAIN_MIN` is not below `DOMAIN_MAX` in every channel.
    #[error("DOMAIN_MIN {min:?} must be below DOMAIN_MAX {max:?} in every channel")]
    Domain {
        /// The minimum read.
        min: [f32; 3],
        /// The maximum read.
        max: [f32; 3],
    },
    /// A PNG table is not a square of `k³` pixels (512x512 for 64 points) with its RGBA8
    /// bytes.
    #[error("a {width}x{height} image ({len} bytes) is not a tiled PNG LUT")]
    Tiled {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Bytes given.
        len: usize,
    },
}

/// A 3D lookup table over the unit cube: `size³` RGB outputs, red varying fastest, then
/// green, then blue (the `.cube` order).
#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    size: usize,
    data: Vec<[f32; 3]>,
}

impl Lut {
    /// Points per side.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The output at grid point `(r, g, b)`.
    pub(crate) fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + self.size * (g + self.size * b)]
    }

    /// The identity table with `size` points per side.
    pub(crate) fn identity(size: usize) -> Self {
        Self::from_fn(size, |rgb| rgb)
    }

    /// A table whose output at each grid point is `f` of that point's input in `0..=1`.
    pub(crate) fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Self {
        let step = |i: usize| i as f32 / (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(f([step(r), step(g), step(b)]));
                }
            }
        }
        Self { size, data }
    }

    /// Parses an Adobe / Resolve `.cube` file: `#` comments, `TITLE`, `LUT_3D_SIZE`,
    /// `DOMAIN_MIN`, `DOMAIN_MAX`, `LUT_3D_INPUT_RANGE`, other keywords ignored, then
    /// `size³` rows of three numbers. A domain other than `0..1` is resampled onto
    /// `0..1`, where input outside the domain clamps to its edge.
    pub fn parse_cube(text: &str) -> Result<Self, LutError> {
        let mut size = None;
        let mut min = [0.0f32; 3];
        let mut max = [1.0f32; 3];
        let mut data: Vec<[f32; 3]> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = raw.split('#').next().unwrap_or_default().trim();
            if content.is_empty() {
                continue;
            }
            let bad = |detail: String| LutError::Line { line, detail };
            let mut words = content.split_whitespace();
            let first = words.next().unwrap_or_default();
            let starts_number = first
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'));
            if starts_number {
                let Some(n) = size else {
                    return Err(LutError::MissingSize);
                };
                let row = numbers::<3>(content.split_whitespace())
                    .map_err(|detail| bad(format!("table row: {detail}")))?;
                if data.len() == n * n * n {
                    return Err(LutError::Count {
                        expected: n * n * n,
                        found: data.len() + 1,
                    });
                }
                data.push(row);
                continue;
            }
            if !data.is_empty() {
                return Err(bad(format!("keyword {first} after the table data")));
            }
            match first {
                "LUT_1D_SIZE" => return Err(LutError::OneDimensional { line }),
                "LUT_3D_SIZE" => {
                    let [n] = numbers::<1>(words).map_err(|d| bad(format!("LUT_3D_SIZE: {d}")))?;
                    if n.fract() != 0.0 || !(MIN_SIZE as f32..=MAX_SIZE as f32).contains(&n) {
                        return Err(if n.fract() == 0.0 && n >= 0.0 {
                            LutError::Size { size: n as usize }
                        } else {
                            bad(format!("LUT_3D_SIZE {n} is not a whole number in range"))
                        });
                    }
                    size = Some(n as usize);
                }
                "DOMAIN_MIN" => {
                    min = numbers::<3>(words).map_err(|d| bad(format!("DOMAIN_MIN: {d}")))?;
                }
                "DOMAIN_MAX" => {
                    max = numbers::<3>(words).map_err(|d| bad(format!("DOMAIN_MAX: {d}")))?;
                }
                "LUT_3D_INPUT_RANGE" => {
                    let [lo, hi] =
                        numbers::<2>(words).map_err(|d| bad(format!("LUT_3D_INPUT_RANGE: {d}")))?;
                    min = [lo; 3];
                    max = [hi; 3];
                }
                // TITLE and vendor keywords (LUT_IN_VIDEO_RANGE, ...) carry nothing the
                // table needs.
                _ => {}
            }
        }
        let n = size.ok_or(LutError::MissingSize)?;
        if data.len() != n * n * n {
            return Err(LutError::Count {
                expected: n * n * n,
                found: data.len(),
            });
        }
        if (0..3).any(|c| min[c] >= max[c]) {
            return Err(LutError::Domain { min, max });
        }
        let lut = Self { size: n, data };
        if min == [0.0; 3] && max == [1.0; 3] {
            return Ok(lut);
        }
        Ok(Self::from_fn(n, |x| {
            lut.sample(std::array::from_fn(|c| (x[c] - min[c]) / (max[c] - min[c])))
        }))
    }

    /// A table from the common tiled PNG layout, as straight sRGB RGBA8 bytes: a square
    /// of `k * k` tiles of `k² x k²` pixels (512x512 for 64 points, `k` = 8), blue
    /// selecting the tile left to right then top to bottom, red along x and green along y
    /// inside it. Alpha is ignored. `k` is 2 to 8.
    pub fn from_tiled_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Self, LutError> {
        let refused = || LutError::Tiled {
            width,
            height,
            len: rgba.len(),
        };
        let k = (2..=8usize)
            .find(|k| k * k * k == width as usize)
            .filter(|_| width == height)
            .ok_or_else(refused)?;
        let side = width as usize;
        if rgba.len() != side * side * 4 {
            return Err(refused());
        }
        let n = k * k;
        let mut data = Vec::with_capacity(n * n * n);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let x = (b % k) * n + r;
                    let y = (b / k) * n + g;
                    let px = &rgba[(y * side + x) * 4..][..3];
                    data.push(std::array::from_fn(|c| f32::from(px[c]) / 255.0));
                }
            }
        }
        Ok(Self { size: n, data })
    }

    /// Trilinear lookup at `x` in `0..=1` (clamped), as the shader does it.
    pub(crate) fn sample(&self, x: [f32; 3]) -> [f32; 3] {
        let top = (self.size - 1) as f32;
        let mut i0 = [0usize; 3];
        let mut i1 = [0usize; 3];
        let mut f = [0.0f32; 3];
        for c in 0..3 {
            let s = x[c].clamp(0.0, 1.0) * top;
            let lo = s.floor().min(top - 1.0);
            i0[c] = lo as usize;
            i1[c] = i0[c] + 1;
            f[c] = s - lo;
        }
        let mut out = [0.0f32; 3];
        for corner in 0..8 {
            let pick = |c: usize| corner >> c & 1 == 1;
            let index = |c: usize| if pick(c) { i1[c] } else { i0[c] };
            let weight: f32 = (0..3)
                .map(|c| if pick(c) { f[c] } else { 1.0 - f[c] })
                .product();
            let value = self.at(index(0), index(1), index(2));
            for c in 0..3 {
                out[c] += weight * value[c];
            }
        }
        out
    }
}

/// `N` numbers from `words`, and no more.
fn numbers<'a, const N: usize>(
    mut words: impl Iterator<Item = &'a str>,
) -> Result<[f32; N], String> {
    let mut out = [0.0f32; N];
    for slot in &mut out {
        let word = words.next().ok_or(format!("expected {N} numbers"))?;
        let value: f32 = word
            .parse()
            .map_err(|_| format!("{word:?} is not a number"))?;
        if !value.is_finite() {
            return Err(format!("{word:?} is not finite"));
        }
        *slot = value;
    }
    if words.next().is_some() {
        return Err(format!("expected {N} numbers, found more"));
    }
    Ok(out)
}

/// Tiles across and down of the 2D layout for a table of `size` points per side: the
/// smallest near-square grid of `size` tiles, each `size x size` (8x8 tiles of 64, a
/// 512x512 image, for 64 points), so the image stays far below any adapter limit.
pub(crate) fn tiles(size: usize) -> (usize, usize) {
    let cols = (1..=size).find(|c| c * c >= size).unwrap_or(size);
    (cols, size.div_ceil(cols))
}

/// The table size an image of `width x height` lays out, if it is one.
pub(crate) fn size_of_layout(width: u32, height: u32) -> Option<usize> {
    (MIN_SIZE..=MAX_SIZE).find(|&n| {
        let (cols, rows) = tiles(n);
        (cols * n, rows * n) == (width as usize, height as usize)
    })
}

impl Lut {
    /// The 2D layout: blue slice `b` is the tile at column `b % cols`, row `b / cols`;
    /// red runs along x and green along y inside it. Alpha is 1; unused tiles are black.
    /// Returns the width, height and RGBA texels, row-major.
    pub(crate) fn layout(&self) -> (u32, u32, Vec<[f32; 4]>) {
        let n = self.size;
        let (cols, rows) = tiles(n);
        let (width, height) = (cols * n, rows * n);
        let mut texels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let x = (b % cols) * n + r;
                    let y = (b / cols) * n + g;
                    let [vr, vg, vb] = self.at(r, g, b);
                    texels[y * width + x] = [vr, vg, vb, 1.0];
                }
            }
        }
        (width as u32, height as u32, texels)
    }
}

#[cfg(test)]
mod tests {
    use super::{Lut, LutError, size_of_layout, tiles};

    fn cube(size: usize, header: &str, lut: &Lut) -> String {
        let mut text = format!("# a comment\nTITLE \"test\"\n{header}LUT_3D_SIZE {size}\n\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let [x, y, z] = lut.at(r, g, b);
                    text.push_str(&format!("{x:.6} {y:.6} {z:.6}\n"));
                }
            }
        }
        text
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() < 1e-5)
    }

    #[test]
    fn a_two_point_cube_parses_in_red_fastest_order() {
        let text = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = Lut::parse_cube(text).expect("valid");
        assert_eq!(lut, Lut::identity(2));
        assert_eq!(lut.at(1, 0, 0), [1.0, 0.0, 0.0]);
        assert!(close(lut.sample([0.25, 0.5, 0.75]), [0.25, 0.5, 0.75]));
    }

    #[test]
    fn a_seventeen_point_cube_with_comments_round_trips() {
        let swap = Lut::from_fn(17, |[r, g, b]| [b, r, g]);
        let text = cube(17, "LUT_IN_VIDEO_RANGE\n", &swap);
        let lut = Lut::parse_cube(&text).expect("valid");
        assert_eq!(lut.size(), 17);
        assert!(close(lut.at(16, 0, 3), swap.at(16, 0, 3)));
        assert!(close(lut.sample([0.1, 0.2, 0.3]), [0.3, 0.1, 0.2]));
    }

    #[test]
    fn a_domain_is_resampled_onto_the_unit_cube() {
        // The identity table's rows over a 0..2 domain halve their input.
        let lut = Lut::identity(2);
        let text = cube(2, "DOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n", &lut);
        let parsed = Lut::parse_cube(&text).expect("valid");
        assert!(close(parsed.at(1, 1, 1), [0.5, 0.5, 0.5]));
        let range = cube(2, "LUT_3D_INPUT_RANGE 0 2\n", &lut);
        assert_eq!(Lut::parse_cube(&range), Ok(parsed));
    }

    #[test]
    fn bad_cubes_are_refused_with_a_reason() {
        let one_d = "LUT_1D_SIZE 4\n0 0 0\n";
        assert_eq!(
            Lut::parse_cube(one_d),
            Err(LutError::OneDimensional { line: 1 })
        );
        assert_eq!(Lut::parse_cube("0 0 0\n"), Err(LutError::MissingSize));
        assert_eq!(
            Lut::parse_cube("LUT_3D_SIZE 65\n"),
            Err(LutError::Size { size: 65 })
        );
        assert_eq!(
            Lut::parse_cube("LUT_3D_SIZE 1\n0 0 0\n"),
            Err(LutError::Size { size: 1 })
        );
        assert!(matches!(
            Lut::parse_cube("LUT_3D_SIZE 2.5\n"),
            Err(LutError::Line { line: 1, .. })
        ));
        assert_eq!(
            Lut::parse_cube("LUT_3D_SIZE 2\n0 0 0\n1 0 0\n"),
            Err(LutError::Count {
                expected: 8,
                found: 2
            })
        );
        assert!(matches!(
            Lut::parse_cube("LUT_3D_SIZE 2\n0 0 x\n"),
            Err(LutError::Line { line: 2, .. })
        ));
        assert!(matches!(
            Lut::parse_cube("LUT_3D_SIZE 2\n0 0 0 0\n"),
            Err(LutError::Line { line: 2, .. })
        ));
        let identity = cube(2, "DOMAIN_MIN 1 0 0\nDOMAIN_MAX 1 1 1\n", &Lut::identity(2));
        assert!(matches!(
            Lut::parse_cube(&identity),
            Err(LutError::Domain { .. })
        ));
        let late = format!("{}TITLE \"late\"\n", cube(2, "", &Lut::identity(2)));
        assert!(matches!(
            Lut::parse_cube(&late),
            Err(LutError::Line { line: 13, .. })
        ));
    }

    #[test]
    fn the_tiled_png_layout_reads_blue_as_the_tile() {
        // k = 2: four 4x4 tiles in an 8x8 image, 4 points per side.
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let (r, g, b) = (x % 4, y % 4, (y / 4) * 2 + x / 4);
                let px = &mut rgba[(y * 8 + x) * 4..][..4];
                px.copy_from_slice(&[(r * 85) as u8, (g * 85) as u8, (b * 85) as u8, 255]);
            }
        }
        let lut = Lut::from_tiled_rgba8(8, 8, &rgba).expect("valid");
        assert_eq!(lut.size(), 4);
        assert!(close(lut.at(1, 2, 3), [85.0 / 255.0, 170.0 / 255.0, 1.0]));
        assert!(Lut::from_tiled_rgba8(8, 4, &rgba[..128]).is_err());
        assert!(Lut::from_tiled_rgba8(10, 10, &[0; 400]).is_err());
    }

    #[test]
    fn the_layout_is_near_square_and_its_size_is_recovered() {
        assert_eq!(tiles(64), (8, 8));
        assert_eq!(tiles(17), (5, 4));
        assert_eq!(tiles(2), (2, 1));
        for n in 2..=64 {
            let (w, h, texels) = Lut::identity(n).layout();
            assert_eq!(texels.len(), (w * h) as usize);
            assert_eq!(size_of_layout(w, h), Some(n), "size {n}");
        }
        assert_eq!(size_of_layout(512, 512), Some(64));
        assert_eq!(size_of_layout(64, 36), None);
        // Slice 6 of a 17-point table is tile (1, 1).
        let (w, _, texels) = Lut::from_fn(17, |[r, g, b]| [r, g, b]).layout();
        let [_, _, b, _] = texels[(17 + 3) * w as usize + 17 + 2];
        assert!((b - 6.0 / 16.0).abs() < 1e-6);
    }
}
