//! Laying text out in lines and drawing it: glyph coverage, the outline and the
//! background, composited into straight-alpha RGBA.
//!
//! There is no shaping and no kerning: each character is its font's glyph for that
//! code point, placed by its advance. Ligatures, kerning, complex scripts and
//! right-to-left text need a shaper, which ADR-0061 leaves for when they are wanted.

use super::{Align, Style};
use crate::{Limits, SourceError};
use ab_glyph_rasterizer::{Point, Rasterizer, point};
use jstream_media::FrameSize;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::ops::Range;

/// Lays `text` out with `style` in `font`, then draws it. Returns the frame's size and
/// its packed RGBA8 pixels, straight alpha.
///
/// # Errors
/// [`SourceError::Setting`] naming `text` when the drawn text would be larger than
/// `limits` allow.
pub(crate) fn render(
    font: &FontRef<'_>,
    text: &str,
    style: &Style,
    limits: &Limits,
) -> Result<(FrameSize, Vec<u8>), SourceError> {
    // `size` is the em size in pixels, which is skrifa's pixels per em.
    let size = Size::new(style.size);
    let metrics = font.metrics(size, LocationRef::default());
    let advances = font.glyph_metrics(size, LocationRef::default());
    let charmap = font.charmap();
    let advance = |id: GlyphId| advances.advance_width(id).unwrap_or(0.0);
    // `descent` is below the baseline, so negative.
    let line_height = metrics.ascent - metrics.descent;
    let line_step = line_height + metrics.leading;

    // Lay out every line at x = 0, then align them.
    let wrap = style.wrap_width.map(|w| w as f32);
    let mut lines: Vec<(Vec<(GlyphId, f32)>, f32)> = Vec::new();
    for paragraph in text.split('\n') {
        let glyphs: Vec<(GlyphId, f32, bool)> = paragraph
            .chars()
            .filter_map(|c| match c {
                '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .map(|c| {
                let id = charmap.map(c).unwrap_or(GlyphId::NOTDEF);
                (id, advance(id), c == ' ')
            })
            .collect();
        for range in wrap_lines(&glyphs, wrap) {
            lines.push(place(&glyphs[range]));
        }
    }
    let widest = lines.iter().map(|(_, w)| *w).fold(0.0, f32::max);
    let box_width = wrap.unwrap_or(widest);
    let box_height = line_height + line_step * (lines.len().saturating_sub(1)) as f32;
    let outlines = font.outline_glyphs();
    let mut glyphs = Vec::new();
    for (i, (line, width)) in lines.into_iter().enumerate() {
        let offset = match style.align {
            Align::Left => 0.0,
            Align::Center => (box_width - width) / 2.0,
            Align::Right => box_width - width,
        };
        let baseline = metrics.ascent + line_step * i as f32;
        for (id, x) in line {
            let mut pen = Pen::new(x + offset, baseline);
            // A glyph with no outline (a space) or one the font can't draw adds no ink.
            let drawn = outlines.get(id).map(|g| {
                g.draw(
                    DrawSettings::unhinted(size, LocationRef::default()),
                    &mut pen,
                )
            });
            if matches!(drawn, Some(Ok(_))) {
                pen.close();
                if !pen.segments.is_empty() {
                    glyphs.push(pen);
                }
            }
        }
    }

    // The frame holds the text box with its padding, and every glyph's ink with the
    // outline around it, which may reach past the box.
    let pad = style.padding as f32;
    let reach = style.outline_width as f32;
    let (mut left, mut top) = (-pad, -pad);
    let (mut right, mut bottom) = (box_width + pad, box_height + pad);
    for glyph in &glyphs {
        left = left.min(glyph.min.x - reach);
        top = top.min(glyph.min.y - reach);
        right = right.max(glyph.max.x + reach);
        bottom = bottom.max(glyph.max.y + reach);
    }
    let (left, top) = (left.floor(), top.floor());
    let width = side(right.ceil() - left, limits)?;
    let height = side(bottom.ceil() - top, limits)?;
    // The output and the fill coverage, and with an outline its distance field. The
    // distance transform's working rows and each glyph's raster are smaller.
    let per_pixel: u64 = if style.outline_width > 0 { 12 } else { 8 };
    let pixels = u64::from(width) * u64::from(height);
    if pixels * per_pixel > limits.max_decoded_bytes {
        return Err(too_large(width, height));
    }
    let (w, h) = (width as usize, height as usize);

    let mut fill = vec![0f32; w * h];
    for glyph in &glyphs {
        glyph.draw(&mut fill, w, h, point(left, top));
    }
    let outline = (style.outline_width > 0).then(|| outline(&fill, w, h, reach));

    let mut rgba = Vec::with_capacity(w * h * 4);
    let background = premultiplied(style.background, 1.0);
    for (i, &coverage) in fill.iter().enumerate() {
        let mut px = background;
        if let Some(outline) = &outline {
            px = over(premultiplied(style.outline_color, outline[i]), px);
        }
        px = over(premultiplied(style.color, coverage), px);
        rgba.extend_from_slice(&straight(px));
    }
    Ok((FrameSize::new(width, height)?, rgba))
}

/// Splits one paragraph's glyphs (ID, advance, whether it is a space) into lines no
/// wider than `wrap`, breaking at spaces where it can and inside a word only when the
/// word alone is too wide. A paragraph always has at least one line, empty if the
/// paragraph is.
fn wrap_lines(glyphs: &[(GlyphId, f32, bool)], wrap: Option<f32>) -> Vec<Range<usize>> {
    let Some(wrap) = wrap else {
        return std::iter::once(0..glyphs.len()).collect();
    };
    let mut lines = Vec::new();
    let mut start = 0;
    while start < glyphs.len() || lines.is_empty() {
        let mut end = glyphs.len();
        let mut x = 0.0;
        let mut last_space = None;
        for (i, &(_, advance, space)) in glyphs.iter().enumerate().skip(start) {
            if space {
                last_space = Some(i);
            } else if i > start && x + advance > wrap {
                end = match last_space {
                    Some(s) if s > start => s,
                    _ => i,
                };
                break;
            }
            x += advance;
        }
        lines.push(start..end);
        // The spaces a line breaks at belong to neither line.
        start = end;
        while glyphs.get(start).is_some_and(|&(_, _, space)| space) {
            start += 1;
        }
    }
    lines
}

/// The glyphs of one line with their x positions from the line's start, and the line's
/// width without its trailing spaces.
fn place(glyphs: &[(GlyphId, f32, bool)]) -> (Vec<(GlyphId, f32)>, f32) {
    let mut x = 0.0;
    let mut width = 0.0;
    let mut placed = Vec::with_capacity(glyphs.len());
    for &(id, advance, space) in glyphs {
        placed.push((id, x));
        x += advance;
        if !space {
            width = x;
        }
    }
    (placed, width)
}

/// One segment of a glyph's outline, in frame pixels with y down.
#[derive(Clone, Copy)]
enum Segment {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

/// Collects a glyph's outline as segments placed at its origin, and their bounds.
/// The bounds include control points, so they hold the curves.
struct Pen {
    origin: Point,
    segments: Vec<Segment>,
    start: Point,
    at: Point,
    min: Point,
    max: Point,
}

impl Pen {
    fn new(x: f32, baseline: f32) -> Self {
        Self {
            origin: point(x, baseline),
            segments: Vec::new(),
            start: point(x, baseline),
            at: point(x, baseline),
            min: point(f32::INFINITY, f32::INFINITY),
            max: point(f32::NEG_INFINITY, f32::NEG_INFINITY),
        }
    }

    /// A font point as a frame point: placed at the origin, with y flipped down.
    fn map(&mut self, x: f32, y: f32) -> Point {
        let p = point(self.origin.x + x, self.origin.y - y);
        self.min = point(self.min.x.min(p.x), self.min.y.min(p.y));
        self.max = point(self.max.x.max(p.x), self.max.y.max(p.y));
        p
    }

    /// Rasterises the glyph and adds its coverage to `fill`, a `w` by `h` frame whose
    /// top left is at `frame` in layout pixels.
    fn draw(&self, fill: &mut [f32], w: usize, h: usize, frame: Point) {
        // Whole pixels, so each raster pixel lands on one frame pixel.
        let corner = point(self.min.x.floor(), self.min.y.floor());
        let gw = (self.max.x.ceil() - corner.x) as usize;
        let gh = (self.max.y.ceil() - corner.y) as usize;
        if gw == 0 || gh == 0 {
            // A flat outline covers nothing.
            return;
        }
        let mut raster = Rasterizer::new(gw, gh);
        let local = |p: Point| p - corner;
        for &segment in &self.segments {
            match segment {
                Segment::Line(a, b) => raster.draw_line(local(a), local(b)),
                Segment::Quad(a, b, c) => raster.draw_quad(local(a), local(b), local(c)),
                Segment::Cubic(a, b, c, d) => {
                    raster.draw_cubic(local(a), local(b), local(c), local(d));
                }
            }
        }
        let (gx, gy) = ((corner.x - frame.x) as usize, (corner.y - frame.y) as usize);
        raster.for_each_pixel_2d(|x, y, c| {
            let (x, y) = (gx + x as usize, gy + y as usize);
            if x < w && y < h {
                let cell = &mut fill[y * w + x];
                // Overlapping contours can take one glyph's coverage past 1.
                *cell = (*cell + c).min(1.0);
            }
        });
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        let p = self.map(x, y);
        self.start = p;
        self.at = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.segments.push(Segment::Line(self.at, p));
        self.at = p;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx0, cy0), self.map(x, y));
        self.segments.push(Segment::Quad(self.at, c, p));
        self.at = p;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.segments.push(Segment::Cubic(self.at, c0, c1, p));
        self.at = p;
    }

    /// Closes the open contour with a line back to its start; the rasteriser needs
    /// closed contours.
    fn close(&mut self) {
        if self.at != self.start {
            self.segments.push(Segment::Line(self.at, self.start));
            self.at = self.start;
        }
    }
}

/// A frame side of `extent` pixels, from 1 to the limit.
fn side(extent: f32, limits: &Limits) -> Result<u32, SourceError> {
    let extent = extent.max(1.0);
    if extent > limits.max_side as f32 {
        // Report the side as a whole number; it may be far past u32 for absurd input.
        return Err(SourceError::Setting {
            param: "text".into(),
            reason: format!(
                "it draws {extent:.0} pixels across, over the {} pixel limit",
                limits.max_side
            ),
        });
    }
    Ok(extent as u32)
}

fn too_large(width: u32, height: u32) -> SourceError {
    SourceError::Setting {
        param: "text".into(),
        reason: format!("it draws a {width}x{height} frame, over the decoded-memory limit"),
    }
}

/// How much of each pixel the outline covers: the fill grown by `radius` pixels.
///
/// A pixel at least half covered counts as inside. Each pixel's distance to the
/// nearest inside pixel centre comes from an exact Euclidean distance transform
/// (Felzenszwalb and Huttenlocher), linear in the frame's size whatever the radius.
/// The edge is anti-aliased over one pixel, and the fill's own coverage is kept where
/// it is larger, so thin strokes that no pixel half covers still get their outline's
/// inner part.
fn outline(fill: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    const FAR: f32 = 1e20;
    let mut dist: Vec<f32> = fill
        .iter()
        .map(|&c| if c >= 0.5 { 0.0 } else { FAR })
        .collect();
    let n = w.max(h);
    let mut f = vec![0f32; n];
    let mut d = vec![0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0f32; n + 1];
    for x in 0..w {
        for y in 0..h {
            f[y] = dist[y * w + x];
        }
        distance_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            dist[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        let row = &mut dist[y * w..(y + 1) * w];
        f[..w].copy_from_slice(row);
        distance_1d(&f[..w], &mut d[..w], &mut v, &mut z);
        row.copy_from_slice(&d[..w]);
    }
    // An inside pixel centre sits about half a pixel inside the edge, so a pixel
    // `radius + 0.5` from it is `radius` outside the edge.
    // In place, so the distances and the coverage never take two buffers.
    for (cell, &c) in dist.iter_mut().zip(fill) {
        *cell = (radius + 1.0 - cell.sqrt()).clamp(0.0, 1.0).max(c);
    }
    dist
}

/// One row of the squared Euclidean distance transform: `d[q]` is the least
/// `(q - p)^2 + f[p]` over every `p`. `v` and `z` are working space at least `f.len()`
/// and `f.len() + 1` long.
fn distance_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    if f.is_empty() {
        return;
    }
    let sq = |q: usize| (q * q) as f32;
    // Where the parabolas from `q` and `p` cross.
    let cross = |q: usize, p: usize| ((f[q] + sq(q)) - (f[p] + sq(p))) / (2.0 * (q - p) as f32);
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..f.len() {
        let mut s = cross(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = cross(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        *out = dq * dq + f[p];
    }
}

/// A straight sRGB colour at `coverage`, premultiplied, in 0 to 1.
fn premultiplied(rgba: [u8; 4], coverage: f32) -> [f32; 4] {
    let a = f32::from(rgba[3]) / 255.0 * coverage;
    let c = |i: usize| f32::from(rgba[i]) / 255.0 * a;
    [c(0), c(1), c(2), a]
}

/// `top` over `below`, both premultiplied.
fn over(top: [f32; 4], below: [f32; 4]) -> [f32; 4] {
    let keep = 1.0 - top[3];
    std::array::from_fn(|i| top[i] + below[i] * keep)
}

/// A premultiplied colour as straight RGBA8.
fn straight(px: [f32; 4]) -> [u8; 4] {
    let a = px[3];
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    if a <= 0.0 {
        return [0; 4];
    }
    [byte(px[0] / a), byte(px[1] / a), byte(px[2] / a), byte(a)]
}

#[cfg(test)]
mod tests {
    use super::distance_1d;

    #[test]
    fn the_distance_transform_is_squared_distance_to_the_nearest_zero() {
        const FAR: f32 = 1e20;
        let f = [FAR, FAR, 0.0, FAR, FAR, FAR, FAR, 0.0];
        let mut d = [0.0; 8];
        let (mut v, mut z) = ([0; 8], [0.0; 9]);
        distance_1d(&f, &mut d, &mut v, &mut z);
        assert_eq!(d, [4.0, 1.0, 0.0, 1.0, 4.0, 4.0, 1.0, 0.0]);
    }
}
