//! A CPU reference: every pixel is the average of an N×N grid of point samples, each
//! mapped back into each item's plane (bottom item first) and sampled bilinearly from
//! the full-resolution texture. With N large this approaches the ideal box-filtered
//! image, so it is the target the GPU paths are measured against.

use std::thread;

use jstream_protocol::{ContentRect, ItemGeometry};

use crate::geom::{Plane, W_MIN};
use crate::image::{Image, encode};

pub struct Item {
    pub geometry: ItemGeometry,
    pub content: ContentRect,
}

pub struct Frame {
    pub rgba: Vec<u8>,
    /// Fraction of the pixel any item covers.
    pub coverage: Vec<f32>,
}

pub fn render(
    width: u32,
    height: u32,
    background: [f32; 3],
    items: &[Item],
    texture: &Image,
    n: u32,
) -> Frame {
    let planes: Vec<Plane> = items.iter().map(|i| Plane::of(&i.geometry)).collect();
    let threads = thread::available_parallelism().map_or(4, |n| n.get()) as u32;
    let rows_per = height.div_ceil(threads);
    let chunks: Vec<(Vec<u8>, Vec<f32>)> = thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let planes = &planes;
                scope.spawn(move || {
                    let y0 = t * rows_per;
                    let y1 = ((t + 1) * rows_per).min(height);
                    let mut rgba = Vec::new();
                    let mut coverage = Vec::new();
                    for y in y0..y1 {
                        for x in 0..width {
                            let (c, cov) = pixel(x, y, background, items, planes, texture, n);
                            rgba.extend([encode(c[0]), encode(c[1]), encode(c[2]), 255]);
                            coverage.push(cov);
                        }
                    }
                    (rgba, coverage)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker"))
            .collect()
    });
    let mut frame = Frame {
        rgba: Vec::new(),
        coverage: Vec::new(),
    };
    for (rgba, coverage) in chunks {
        frame.rgba.extend(rgba);
        frame.coverage.extend(coverage);
    }
    frame
}

fn pixel(
    x: u32,
    y: u32,
    background: [f32; 3],
    items: &[Item],
    planes: &[Plane],
    texture: &Image,
    n: u32,
) -> ([f32; 3], f32) {
    let mut sum = [0.0f32; 3];
    let mut covered = 0u32;
    for sy in 0..n {
        for sx in 0..n {
            let px = x as f32 + (sx as f32 + 0.5) / n as f32;
            let py = y as f32 + (sy as f32 + 0.5) / n as f32;
            let mut c = background;
            let mut hit = false;
            for (item, plane) in items.iter().zip(planes) {
                let Some(s) = sample(item, plane, [px, py], texture) else {
                    continue;
                };
                hit = true;
                for i in 0..3 {
                    c[i] = s[i] + c[i] * (1.0 - s[3]);
                }
            }
            covered += u32::from(hit);
            for i in 0..3 {
                sum[i] += c[i];
            }
        }
    }
    let samples = (n * n) as f32;
    (sum.map(|s| s / samples), covered as f32 / samples)
}

fn sample(item: &Item, plane: &Plane, canvas: [f32; 2], texture: &Image) -> Option<[f32; 4]> {
    let [x, y] = plane.unplace(canvas)?;
    if plane.w(x, y) < W_MIN {
        return None;
    }
    let g = &item.geometry;
    let c = &item.content;
    let (half_w, half_h) = (g.size.width.get() * 0.5, g.size.height.get() * 0.5);
    let inside_box = x.abs() <= half_w && y.abs() <= half_h;
    let inside_content =
        x >= c.left && x <= c.left + c.width && y >= c.top && y <= c.top + c.height;
    if !inside_box || !inside_content {
        return None;
    }
    let u = c.u0 + (x - c.left) / c.width * (c.u1 - c.u0);
    let v = c.v0 + (y - c.top) / c.height * (c.v1 - c.v0);
    Some(texture.bilinear(u, v))
}
