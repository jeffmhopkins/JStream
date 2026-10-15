//! Decoding PNG, JPEG, WebP and GIF files into straight RGBA8 frames, with pure-Rust
//! decoders (ADR-0031, docs/adr/0031-source-types-and-decoding-scope.md).
//!
//! Colour metadata (gamma, ICC profiles) is not applied and JPEG EXIF orientation is
//! not followed yet: pixels are taken as sRGB, as stored.

use crate::{Limits, SourceError};
use std::io::Cursor;
use std::num::NonZeroU64;

/// A decoded image: every frame composited onto the full canvas.
#[derive(Debug)]
pub(crate) struct Decoded {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Each frame's straight RGBA8 pixels, row by row with no padding, and how long it
    /// shows in milliseconds (0 for a still image).
    pub(crate) frames: Vec<(Vec<u8>, u32)>,
    /// How many times the animation plays; `None` is forever.
    pub(crate) plays: Option<u32>,
}

/// The file formats the image source reads, told apart by their first bytes rather
/// than the file's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Png,
    Jpeg,
    WebP,
    Gif,
}

impl Format {
    pub(crate) fn sniff(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Format::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Format::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Format::WebP)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::WebP => "WebP",
            Format::Gif => "GIF",
        }
    }
}

/// A frame of this or less shows for [`SHORT_FRAME_MS`] instead. Browsers do the same,
/// because many files store 0 or 10 ms and were made to be shown at 100 ms.
const MAX_SHORT_FRAME_MS: u32 = 10;
const SHORT_FRAME_MS: u32 = 100;

/// Decodes `bytes`, which were read from the asset at `path` (named in errors).
pub(crate) fn decode(bytes: &[u8], path: &str, limits: &Limits) -> Result<Decoded, SourceError> {
    let format = Format::sniff(bytes).ok_or_else(|| SourceError::UnsupportedFormat {
        path: path.to_owned(),
    })?;
    let mut d = Decoder {
        path,
        format,
        limits,
        decoded_bytes: 0,
    };
    match format {
        Format::Png => d.png(bytes),
        Format::Jpeg => d.jpeg(bytes),
        Format::WebP => d.webp(bytes),
        Format::Gif => d.gif(bytes),
    }
}

struct Decoder<'a> {
    path: &'a str,
    format: Format,
    limits: &'a Limits,
    /// Bytes of the frames kept so far, against `limits.max_decoded_bytes`.
    decoded_bytes: u64,
}

impl Decoder<'_> {
    fn err(&self, e: impl std::fmt::Display) -> SourceError {
        SourceError::Decode {
            path: self.path.to_owned(),
            format: self.format.name(),
            reason: e.to_string(),
        }
    }

    fn too_large(&self, what: String) -> SourceError {
        SourceError::TooLarge {
            path: self.path.to_owned(),
            what,
        }
    }

    /// Checks the canvas size before anything is allocated for it, and returns the
    /// bytes one RGBA frame of it takes.
    fn canvas(&self, width: u32, height: u32) -> Result<usize, SourceError> {
        let max = self.limits.max_side;
        if width == 0 || height == 0 {
            return Err(self.err("the image has no pixels"));
        }
        if width > max || height > max {
            return Err(self.too_large(format!("{width}x{height} has a side over {max} px")));
        }
        let bytes = u64::from(width) * u64::from(height) * 4;
        self.room(bytes)?;
        usize::try_from(bytes).map_err(|_| self.too_large(format!("{width}x{height}")))
    }

    /// Fails when `more` bytes of frames would pass the decoded-size limit.
    fn room(&self, more: u64) -> Result<(), SourceError> {
        let max = self.limits.max_decoded_bytes;
        if self.decoded_bytes.saturating_add(more) > max {
            return Err(self.too_large(format!("its frames decode to over {max} bytes")));
        }
        Ok(())
    }

    /// Counts a working buffer the decoder keeps until it returns (a canvas, a
    /// scanline buffer) against the decoded-size limit, so peak memory is bounded and
    /// not only the frames kept.
    fn working(&mut self, bytes: usize) -> Result<(), SourceError> {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.room(bytes)?;
        self.decoded_bytes += bytes;
        Ok(())
    }

    /// Keeps one RGBA frame, counting it against the decoded-size limit.
    fn keep(
        &mut self,
        frames: &mut Vec<(Vec<u8>, u32)>,
        rgba: Vec<u8>,
        delay_ms: u32,
    ) -> Result<(), SourceError> {
        let len = u64::try_from(rgba.len()).unwrap_or(u64::MAX);
        self.room(len)?;
        self.decoded_bytes += len;
        let delay_ms = if delay_ms <= MAX_SHORT_FRAME_MS {
            SHORT_FRAME_MS
        } else {
            delay_ms
        };
        frames.push((rgba, delay_ms));
        Ok(())
    }

    fn still(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> Result<Decoded, SourceError> {
        let mut frames = Vec::with_capacity(1);
        self.keep(&mut frames, rgba, 0)?;
        frames[0].1 = 0;
        Ok(Decoded {
            width,
            height,
            frames,
            plays: Some(1),
        })
    }

    /// The first image of a PNG (an APNG's animation is not played).
    fn png(&mut self, bytes: &[u8]) -> Result<Decoded, SourceError> {
        // The decoder's own allocations (chunks, the decoded frame); at least its
        // default of 64 MiB, since the frame's size is checked against ours below.
        let budget = usize::try_from(self.limits.max_decoded_bytes)
            .unwrap_or(usize::MAX)
            .max(64 << 20);
        let mut decoder =
            png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: budget });
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|e| self.err(e))?;
        let (width, height) = (reader.info().width, reader.info().height);
        let out_len = self.canvas(width, height)?;
        self.working(reader.output_buffer_size())?;
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).map_err(|e| self.err(e))?;
        let channels = match info.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err(self.err("palette was not expanded")),
        };
        let row = width as usize * channels;
        let mut rgba = Vec::with_capacity(out_len);
        for line in buf.chunks(info.line_size.max(1)).take(height as usize) {
            let pixels = line.get(..row).ok_or_else(|| self.err("short scanline"))?;
            for px in pixels.chunks_exact(channels) {
                rgba.extend_from_slice(&match *px {
                    [l] => [l, l, l, 0xff],
                    [l, a] => [l, l, l, a],
                    [r, g, b] => [r, g, b, 0xff],
                    [r, g, b, a] => [r, g, b, a],
                    _ => unreachable!("chunks_exact of 1 to 4"),
                });
            }
        }
        if rgba.len() != out_len {
            return Err(self.err("the image is truncated"));
        }
        self.still(width, height, rgba)
    }

    fn jpeg(&mut self, bytes: &[u8]) -> Result<Decoded, SourceError> {
        use zune_core::colorspace::ColorSpace;
        use zune_core::options::DecoderOptions;
        let max = self.limits.max_side as usize;
        let options = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(max)
            .set_max_height(max);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), options);
        decoder.decode_headers().map_err(|e| self.err(e))?;
        let info = decoder.info().ok_or_else(|| self.err("no image header"))?;
        let (width, height) = (u32::from(info.width), u32::from(info.height));
        let out_len = self.canvas(width, height)?;
        let rgba = decoder.decode().map_err(|e| self.err(e))?;
        if rgba.len() != out_len {
            return Err(self.err("the decoder returned the wrong number of pixels"));
        }
        self.still(width, height, rgba)
    }

    /// A still or animated WebP. The decoder composites animation frames itself.
    fn webp(&mut self, bytes: &[u8]) -> Result<Decoded, SourceError> {
        let mut decoder =
            image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| self.err(e))?;
        let (width, height) = decoder.dimensions();
        self.canvas(width, height)?;
        decoder
            .set_memory_limit(usize::try_from(self.limits.max_decoded_bytes).unwrap_or(usize::MAX));
        let alpha = decoder.has_alpha();
        let buf_len = decoder
            .output_buffer_size()
            .ok_or_else(|| self.too_large(format!("{width}x{height}")))?;
        let to_rgba = |buf: &[u8]| -> Vec<u8> {
            if alpha {
                buf.to_vec()
            } else {
                buf.chunks_exact(3)
                    .flat_map(|p| [p[0], p[1], p[2], 0xff])
                    .collect()
            }
        };
        self.working(buf_len)?;
        let mut buf = vec![0; buf_len];
        if !decoder.is_animated() {
            decoder.read_image(&mut buf).map_err(|e| self.err(e))?;
            return self.still(width, height, to_rgba(&buf));
        }
        let plays = match decoder.loop_count() {
            image_webp::LoopCount::Forever => None,
            image_webp::LoopCount::Times(n) => Some(u32::from(n.get())),
        };
        let mut frames = Vec::new();
        for _ in 0..decoder.num_frames() {
            let delay = decoder.read_frame(&mut buf).map_err(|e| self.err(e))?;
            self.keep(&mut frames, to_rgba(&buf), delay)?;
        }
        if frames.is_empty() {
            return Err(self.err("the animation has no frames"));
        }
        Ok(Decoded {
            width,
            height,
            frames,
            plays,
        })
    }

    /// A still or animated GIF, composited onto its logical screen with each frame's
    /// disposal method. "Restore to background" clears to transparent, as browsers do.
    fn gif(&mut self, bytes: &[u8]) -> Result<Decoded, SourceError> {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        // Caps each frame's buffer before the screen size is known: a frame header can
        // claim up to 65535x65535 pixels whatever the screen is.
        if let Some(limit) = NonZeroU64::new(self.limits.max_decoded_bytes) {
            options.set_memory_limit(gif::MemoryLimit::Bytes(limit));
        }
        let mut decoder = options
            .read_info(Cursor::new(bytes))
            .map_err(|e| self.err(e))?;
        let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
        let canvas_len = self.canvas(width, height)?;
        // A GIF with no NETSCAPE loop extension plays once; one with a count of `n`
        // repeats `n` times after the first play, as browsers read it.
        let plays = match decoder.repeat() {
            gif::Repeat::Infinite => None,
            gif::Repeat::Finite(n) => Some(u32::from(n) + 1),
        };
        self.working(canvas_len)?;
        let mut canvas = vec![0u8; canvas_len];
        let mut frames = Vec::new();
        let canvas_bytes = canvas_len as u64;
        while let Some(frame) = decoder.read_next_frame().map_err(|e| self.err(e))? {
            let rect = Rect::clip(frame, width, height);
            // "Restore to previous" needs a copy of the canvas until the frame is kept;
            // the copy and the kept frame are both checked before they are allocated.
            let restore = frame.dispose == gif::DisposalMethod::Previous;
            let copies = if restore { 2 } else { 1 };
            self.room(canvas_bytes * copies)?;
            let saved = restore.then(|| canvas.clone());
            draw(&mut canvas, width, frame, rect);
            self.keep(&mut frames, canvas.clone(), u32::from(frame.delay) * 10)?;
            match frame.dispose {
                gif::DisposalMethod::Background => clear(&mut canvas, width, rect),
                gif::DisposalMethod::Previous => {
                    if let Some(saved) = saved {
                        canvas = saved;
                    }
                }
                gif::DisposalMethod::Any | gif::DisposalMethod::Keep => {}
            }
        }
        match frames.len() {
            0 => Err(self.err("the file has no frames")),
            1 => {
                let (rgba, _) = frames.remove(0);
                self.decoded_bytes = 0;
                self.still(width, height, rgba)
            }
            _ => Ok(Decoded {
                width,
                height,
                frames,
                plays,
            }),
        }
    }
}

/// A frame's rectangle clipped to the screen, in screen pixels.
#[derive(Clone, Copy)]
struct Rect {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

impl Rect {
    fn clip(frame: &gif::Frame<'_>, screen_w: u32, screen_h: u32) -> Rect {
        let (left, top) = (usize::from(frame.left), usize::from(frame.top));
        let width = usize::from(frame.width).min((screen_w as usize).saturating_sub(left));
        let height = usize::from(frame.height).min((screen_h as usize).saturating_sub(top));
        Rect {
            left,
            top,
            width,
            height,
        }
    }
}

/// Draws a frame's opaque pixels over the canvas. GIF transparency is all or nothing,
/// so a transparent pixel leaves what is beneath it.
fn draw(canvas: &mut [u8], screen_w: u32, frame: &gif::Frame<'_>, rect: Rect) {
    let src_stride = usize::from(frame.width) * 4;
    let dst_stride = screen_w as usize * 4;
    for y in 0..rect.height {
        let src = frame
            .buffer
            .get(y * src_stride..)
            .and_then(|r| r.get(..rect.width * 4));
        let at = (rect.top + y) * dst_stride + rect.left * 4;
        let dst = canvas
            .get_mut(at..)
            .and_then(|r| r.get_mut(..rect.width * 4));
        let (Some(src), Some(dst)) = (src, dst) else {
            return;
        };
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            if s[3] != 0 {
                d.copy_from_slice(s);
            }
        }
    }
}

/// Clears a rectangle of the canvas to transparent.
fn clear(canvas: &mut [u8], screen_w: u32, rect: Rect) {
    let dst_stride = screen_w as usize * 4;
    for y in 0..rect.height {
        let at = (rect.top + y) * dst_stride + rect.left * 4;
        if let Some(row) = canvas
            .get_mut(at..)
            .and_then(|r| r.get_mut(..rect.width * 4))
        {
            row.fill(0);
        }
    }
}
