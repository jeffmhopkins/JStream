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

/// What [`render`] draws.
pub(crate) struct Drawn {
    /// The frame's size in pixels.
    pub(crate) size: FrameSize,
    /// The size the frame stands for: its size at a density of 1, which it is placed at
    /// (ADR-0071).
    pub(crate) extent: FrameSize,
    /// Packed RGBA8, straight alpha.
    pub(crate) rgba: Vec<u8>,
}

/// Lays `text` out with `style` in `font`, then draws it `density` times as many pixels
/// each way as `style` sizes it: the layout, its line breaks and the frame's own size
/// (its [`Drawn::extent`]) are the same at any density, and only the drawing is finer.
///
/// # Errors
/// [`SourceError::Setting`] naming `text` when the drawn text would be larger than
/// `limits` allow.
pub(crate) fn render(
    font: &FontRef<'_>,
    text: &str,
    style: &Style,
    density: f32,
    limits: &Limits,
) -> Result<Drawn, SourceError> {
    let k = density;
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
    // Every glyph's outline, placed, at a density `d`: positions and the em size scaled
    // alike.
    let pens = |d: f32| {
        let mut glyphs = Vec::new();
        for (i, (line, width)) in lines.iter().enumerate() {
            let offset = match style.align {
                Align::Left => 0.0,
                Align::Center => (box_width - width) / 2.0,
                Align::Right => box_width - width,
            };
            let baseline = metrics.ascent + line_step * i as f32;
            for &(id, x) in line {
                let mut pen = Pen::new((x + offset) * d, baseline * d);
                // A glyph with no outline (a space) or one the font can't draw adds no
                // ink.
                let drawn = outlines.get(id).map(|g| {
                    g.draw(
                        DrawSettings::unhinted(Size::new(style.size * d), LocationRef::default()),
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
        glyphs
    };
    // The bounds come from the outlines at a density of 1, so the extent is the frame's
    // size at 1 whatever the density: dividing the dense bounds back by it can land a
    // pixel over (review B1 on #781).
    let plain = pens(1.0);
    let dense = (k != 1.0).then(|| pens(k));

    // The frame holds the text box with its padding, and every glyph's ink with the
    // outline around it, which may reach past the box. Its bounds are whole pixels at a
    // density of 1, its extent, and the drawing covers them `k` times over.
    let pad = style.padding as f32;
    let reach = style.outline_width as f32;
    let (mut left, mut top) = (-pad, -pad);
    let (mut right, mut bottom) = (box_width + pad, box_height + pad);
    for glyph in &plain {
        left = left.min(glyph.min.x - reach);
        top = top.min(glyph.min.y - reach);
        right = right.max(glyph.max.x + reach);
        bottom = bottom.max(glyph.max.y + reach);
    }
    let (left, top) = (left.floor(), top.floor());
    let extent = FrameSize::new(
        side(right.ceil() - left, limits)?,
        side(bottom.ceil() - top, limits)?,
    )?;
    let width = side((extent.width() as f32 * k).ceil(), limits)?;
    let height = side((extent.height() as f32 * k).ceil(), limits)?;
    let (left, top) = (left * k, top * k);
    let reach = reach * k;
    // The output and the fill coverage, and with an outline its distance field. The
    // distance transform's working rows and each glyph's raster are smaller.
    let per_pixel: u64 = if style.outline_width > 0 { 12 } else { 8 };
    let pixels = u64::from(width) * u64::from(height);
    if pixels * per_pixel > limits.max_decoded_bytes {
        return Err(too_large(width, height));
    }
    let (w, h) = (width as usize, height as usize);

    let mut fill = vec![0f32; w * h];
    for glyph in dense.as_ref().unwrap_or(&plain) {
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
    Ok(Drawn {
        size: FrameSize::new(width, height)?,
        extent,
        rgba,
    })
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
        // Whole pixels from the frame's corner, which a density puts between pixels,
        // so each raster pixel lands on one frame pixel.
        let corner = point(
            (self.min.x - frame.x).floor() + frame.x,
            (self.min.y - frame.y).floor() + frame.y,
        );
        let gw = (self.max.x - corner.x).ceil() as usize;
        let gh = (self.max.y - corner.y).ceil() as usize;
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
        // Signed: a dense glyph's ink may start a rounding error before the frame.
        let (gx, gy) = (
            (corner.x - frame.x).round() as i64,
            (corner.y - frame.y).round() as i64,
        );
        raster.for_each_pixel_2d(|x, y, c| {
            let (x, y) = (gx + i64::from(x), gy + i64::from(y));
            if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                && x < w
                && y < h
            {
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

/// Stacks drawn blocks (each a size and straight RGBA8 pixels, as [`render`] returns)
/// top to bottom, left-aligned, `gap` pixels apart, on `background` with `padding`
/// around them.
///
/// # Errors
/// [`SourceError::Setting`] naming `text` when the result would be larger than `limits`
/// allow.
pub(crate) fn stack(
    blocks: &[(FrameSize, Vec<u8>)],
    gap: u32,
    background: [u8; 4],
    padding: u32,
    limits: &Limits,
) -> Result<(FrameSize, Vec<u8>), SourceError> {
    let inner_w = blocks.iter().map(|(s, _)| s.width()).max().unwrap_or(0);
    let gaps =
        gap.saturating_mul(u32::try_from(blocks.len().saturating_sub(1)).unwrap_or(u32::MAX));
    let inner_h = blocks
        .iter()
        .fold(gaps, |h, (s, _)| h.saturating_add(s.height()));
    let width = side(
        inner_w.saturating_add(padding.saturating_mul(2)).max(1) as f32,
        limits,
    )?;
    let height = side(
        inner_h.saturating_add(padding.saturating_mul(2)).max(1) as f32,
        limits,
    )?;
    // The output, composited in place, plus the blocks it is drawn from, which are
    // still held.
    let blocks_bytes: u64 = blocks.iter().map(|(_, rgba)| rgba.len() as u64).sum();
    if (u64::from(width) * u64::from(height) * 4).saturating_add(blocks_bytes)
        > limits.max_decoded_bytes
    {
        return Err(too_large(width, height));
    }
    let (w, h) = (width as usize, height as usize);
    let mut rgba = background.repeat(w * h);
    let mut y0 = padding as usize;
    for (size, block) in blocks {
        let (bw, bh) = (size.width() as usize, size.height() as usize);
        for y in 0..bh {
            for x in 0..bw {
                let i = (y * bw + x) * 4;
                let src = [block[i], block[i + 1], block[i + 2], block[i + 3]];
                let o = ((y0 + y) * w + padding as usize + x) * 4;
                let below = [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]];
                let px = straight(over(premultiplied(src, 1.0), premultiplied(below, 1.0)));
                rgba[o..o + 4].copy_from_slice(&px);
            }
        }
        y0 += bh + gap as usize;
    }
    Ok((FrameSize::new(width, height)?, rgba))
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
    use super::{distance_1d, stack};
    use crate::{Limits, SourceError};
    use jstream_media::FrameSize;

    /// `stack` counts its output and the blocks it still holds against
    /// `max_decoded_bytes` (review B1 on #363).
    #[test]
    fn stack_counts_its_output_and_its_blocks() {
        let block = (FrameSize::new(10, 10).expect("size"), vec![255; 400]);
        let limits = |max_decoded_bytes| Limits {
            max_decoded_bytes,
            ..Limits::default()
        };
        // 10x10 output (400 bytes) plus the 400-byte block.
        let (size, rgba) =
            stack(std::slice::from_ref(&block), 0, [0; 4], 0, &limits(800)).expect("fits");
        assert_eq!((size.width(), size.height(), rgba.len()), (10, 10, 400));
        assert!(matches!(
            stack(&[block], 0, [0; 4], 0, &limits(799)),
            Err(SourceError::Setting { .. })
        ));
    }

    /// Blocks are drawn over the background, the lower one under the gap.
    #[test]
    fn stack_draws_blocks_over_the_background() {
        let red = (FrameSize::new(1, 1).expect("size"), vec![255, 0, 0, 255]);
        let clear = (FrameSize::new(1, 1).expect("size"), vec![0; 4]);
        let (size, rgba) =
            stack(&[red, clear], 1, [0, 0, 255, 255], 1, &Limits::default()).expect("fits");
        assert_eq!((size.width(), size.height()), (3, 5));
        let px = |x: usize, y: usize| &rgba[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4];
        assert_eq!(px(1, 1), [255, 0, 0, 255]);
        assert_eq!(px(1, 3), [0, 0, 255, 255]);
        assert_eq!(px(0, 0), [0, 0, 255, 255]);
    }

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
