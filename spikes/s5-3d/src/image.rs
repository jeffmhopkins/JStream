//! The test texture, its mip chain, sRGB conversion, PNG output and error metrics.

use std::path::Path;

/// Linear premultiplied RGBA, row-major.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub px: Vec<[f32; 4]>,
}

impl Image {
    /// A 1024² pattern with detail at every scale: a 32 px checkerboard of two colours,
    /// 2 px white grid lines every 128 px, and 1 px diagonal hatching in one quadrant.
    /// Fine detail is what shows minification aliasing and texture swimming.
    pub fn pattern() -> Self {
        let size = 1024u32;
        let mut px = Vec::with_capacity((size * size) as usize);
        for y in 0..size {
            for x in 0..size {
                let checker = ((x / 32) + (y / 32)) % 2 == 0;
                let mut c = if checker {
                    [0.80, 0.30, 0.05]
                } else {
                    [0.05, 0.20, 0.60]
                };
                if x % 128 < 2 || y % 128 < 2 {
                    c = [1.0, 1.0, 1.0];
                }
                if x < 512 && y < 512 && (x + y) % 6 == 0 {
                    c = [0.0, 0.0, 0.0];
                }
                // A 24 px opaque frame of solid yellow, so the item's edges stand out.
                if x < 24 || y < 24 || x >= size - 24 || y >= size - 24 {
                    c = [0.9, 0.8, 0.1];
                }
                px.push([c[0], c[1], c[2], 1.0]);
            }
        }
        Image {
            width: size,
            height: size,
            px,
        }
    }

    pub fn at(&self, x: u32, y: u32) -> [f32; 4] {
        self.px[(y * self.width + x) as usize]
    }

    /// Bilinear sample, clamp to edge, texel centres at half-integers (as the GPU).
    pub fn bilinear(&self, u: f32, v: f32) -> [f32; 4] {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let (fx, fy) = (x - x0, y - y0);
        let clamp_x = |x: f32| x.clamp(0.0, (self.width - 1) as f32) as u32;
        let clamp_y = |y: f32| y.clamp(0.0, (self.height - 1) as f32) as u32;
        let (xa, xb) = (clamp_x(x0), clamp_x(x0 + 1.0));
        let (ya, yb) = (clamp_y(y0), clamp_y(y0 + 1.0));
        let (a, b, c, d) = (
            self.at(xa, ya),
            self.at(xb, ya),
            self.at(xa, yb),
            self.at(xb, yb),
        );
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            out[i] = top + (bottom - top) * fy;
        }
        out
    }

    /// The next mip level: a 2×2 box filter in linear light.
    pub fn half(&self) -> Self {
        let (w, h) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = self.at(
                        (2 * x + dx).min(self.width - 1),
                        (2 * y + dy).min(self.height - 1),
                    );
                    for i in 0..4 {
                        sum[i] += p[i] * 0.25;
                    }
                }
                px.push(sum);
            }
        }
        Image {
            width: w,
            height: h,
            px,
        }
    }

    pub fn mips(&self) -> Vec<Image> {
        let mut levels = vec![self.clone()];
        while levels.last().is_some_and(|l| l.width > 1 || l.height > 1) {
            let next = levels.last().map(Image::half).expect("non-empty");
            levels.push(next);
        }
        levels
    }

    /// Straight-alpha sRGB bytes. Every texel here is opaque, so un-premultiplying is a no-op.
    pub fn to_srgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            out.extend([
                encode(p[0]),
                encode(p[1]),
                encode(p[2]),
                (p[3] * 255.0).round() as u8,
            ]);
        }
        out
    }
}

/// sRGB bytes decoded to linear, so the CPU reference sees the texels the GPU sees.
pub fn quantize_like_gpu(image: &Image) -> Image {
    let bytes = image.to_srgb8();
    let px = bytes
        .chunks_exact(4)
        .map(|c| {
            [
                decode(c[0]),
                decode(c[1]),
                decode(c[2]),
                f32::from(c[3]) / 255.0,
            ]
        })
        .collect();
    Image {
        width: image.width,
        height: image.height,
        px,
    }
}

pub fn encode(linear: f32) -> u8 {
    let l = linear.clamp(0.0, 1.0);
    let s = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

pub fn decode(byte: u8) -> f32 {
    let s = f32::from(byte) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
}

/// Per-channel error between two sRGB8 images over the pixels `mask` selects.
#[derive(Clone, Copy, Debug)]
pub struct Error {
    pub pixels: usize,
    pub mean: f64,
    pub p99: u8,
    pub max: u8,
}

pub fn compare(a: &[u8], b: &[u8], mask: &[bool]) -> Error {
    let mut diffs = Vec::new();
    for (i, selected) in mask.iter().enumerate() {
        if !selected {
            continue;
        }
        for c in 0..3 {
            diffs.push(a[i * 4 + c].abs_diff(b[i * 4 + c]));
        }
    }
    if diffs.is_empty() {
        return Error {
            pixels: 0,
            mean: 0.0,
            p99: 0,
            max: 0,
        };
    }
    let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
    diffs.sort_unstable();
    Error {
        pixels: mask.iter().filter(|m| **m).count(),
        mean,
        p99: diffs[(diffs.len() - 1) * 99 / 100],
        max: *diffs.last().expect("non-empty"),
    }
}

/// An absolute-difference image, amplified 4×, for looking at where two renders differ.
pub fn diff_image(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .flat_map(|(a, b)| {
            let d = |i: usize| (u16::from(a[i].abs_diff(b[i])) * 4).min(255) as u8;
            [d(0), d(1), d(2), 255]
        })
        .collect()
}
