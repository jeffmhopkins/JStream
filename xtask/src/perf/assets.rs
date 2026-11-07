//! The files the perf scenes load, written fresh for each run so the repository holds no
//! binary media: a 1080p still and a mask (PNG), an animated GIF, a 3D LUT (`.cube`) and
//! two HTML pages. The encoders are the smallest that make valid files: PNG rows stored
//! without a filter and deflated, and GIF frames as 9-bit LZW codes with a clear code
//! often enough that the code size never grows.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::ZlibEncoder;

/// The folder, beside the run's collection file, that the scenes' `$asset:` names live in.
pub const DIR: &str = "perf-assets";

/// Writes every asset into `dir`.
pub fn write_all(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let put = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
    };
    put("still.png", &png(1920, 1080, Pixels::Rgb(&still_pixel))?)?;
    put("mask.png", &png(512, 512, Pixels::Gray(&mask_pixel))?)?;
    put("anim.gif", &gif(320, 180, 8, 5))?;
    put("warm.cube", cube(17).as_bytes())?;
    put("page_anim.html", PAGE_ANIM.as_bytes())?;
    put("page_static.html", PAGE_STATIC.as_bytes())?;
    Ok(())
}

/// A colourful, detailed still: gradients crossed with a fine checker, so filters and
/// scaling have edges to work on.
fn still_pixel(x: u32, y: u32) -> [u8; 3] {
    let check = if (x / 8 + y / 8).is_multiple_of(2) {
        0
    } else {
        40
    };
    [
        u8::try_from((x * 255 / 1920 + check) % 256).unwrap_or(0),
        u8::try_from((y * 255 / 1080 + check) % 256).unwrap_or(0),
        u8::try_from(((x + y) / 12 + check) % 256).unwrap_or(0),
    ]
}

/// A radial mask: white in the middle, black at the corners.
fn mask_pixel(x: u32, y: u32) -> u8 {
    let dx = f64::from(x) - 256.0;
    let dy = f64::from(y) - 256.0;
    let d = (dx * dx + dy * dy).sqrt() / 362.0;
    // In 0..=255 by construction: clamped first.
    ((1.0 - d).clamp(0.0, 1.0) * 255.0) as u8
}

/// What a PNG's pixels are.
enum Pixels<'a> {
    /// 8-bit RGB.
    Rgb(&'a dyn Fn(u32, u32) -> [u8; 3]),
    /// 8-bit grey.
    Gray(&'a dyn Fn(u32, u32) -> u8),
}

/// Encodes a PNG.
fn png(width: u32, height: u32, pixels: Pixels<'_>) -> Result<Vec<u8>> {
    let (colour_type, channels) = match pixels {
        Pixels::Rgb(_) => (2u8, 3usize),
        Pixels::Gray(_) => (0u8, 1usize),
    };
    let row = 1 + channels * usize::try_from(width)?;
    let mut raw = Vec::with_capacity(row * usize::try_from(height)?);
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            match &pixels {
                Pixels::Rgb(f) => raw.extend_from_slice(&f(x, y)),
                Pixels::Gray(f) => raw.push(f(x, y)),
            }
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    z.write_all(&raw)?;
    let idat = z.finish()?;
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, colour_type, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr)?;
    chunk(&mut out, b"IDAT", &idat)?;
    chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

/// Appends one PNG chunk: length, type, data and the CRC of type and data.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<()> {
    out.extend_from_slice(&u32::try_from(data.len())?.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
    Ok(())
}

/// Pixel codes between clear codes: after a clear, each code but the first adds a table
/// entry, so 250 codes take the table to 507 entries, short of 512 where the code size
/// would grow to 10 bits.
const CODES_PER_CLEAR: usize = 250;

/// Encodes a looping animated GIF of `frames` frames, `delay` hundredths of a second
/// each: diagonal colour bands that move every frame.
fn gif(width: u16, height: u16, frames: u8, delay: u16) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0xF7, 0, 0]); // a 256-colour global table
    for i in 0..=255u8 {
        out.extend_from_slice(&[i, 255 - i, i.wrapping_mul(3)]);
    }
    out.extend_from_slice(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00"); // loop forever
    for frame in 0..frames {
        out.extend_from_slice(&[0x21, 0xF9, 0x04, 0x04]);
        out.extend_from_slice(&delay.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.push(0x2C);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.push(0);
        out.push(8); // LZW minimum code size
        let mut bits = BitWriter::default();
        let mut since_clear = CODES_PER_CLEAR;
        for y in 0..height {
            for x in 0..width {
                if since_clear == CODES_PER_CLEAR {
                    bits.put(256, 9);
                    since_clear = 0;
                }
                let index = (u32::from(x) + u32::from(y) + u32::from(frame) * 32) / 4 % 256;
                bits.put(index, 9);
                since_clear += 1;
            }
        }
        bits.put(257, 9); // end of information
        for block in bits.finish().chunks(255) {
            out.push(u8::try_from(block.len()).unwrap_or(255));
            out.extend_from_slice(block);
        }
        out.push(0);
    }
    out.push(0x3B);
    out
}

/// Packs codes least significant bit first, as GIF's LZW stream wants.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u32,
    len: u32,
}

impl BitWriter {
    fn put(&mut self, code: u32, width: u32) {
        self.acc |= code << self.len;
        self.len += width;
        while self.len >= 8 {
            self.bytes.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.len -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.len > 0 {
            self.bytes.push((self.acc & 0xFF) as u8);
        }
        self.bytes
    }
}

/// A warm 3D LUT of `size`³ entries, red fastest, as `.cube` text.
fn cube(size: u32) -> String {
    let mut out = format!("TITLE \"perf warm\"\nLUT_3D_SIZE {size}\n");
    let max = f64::from(size - 1);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let (r, g, b) = (f64::from(r) / max, f64::from(g) / max, f64::from(b) / max);
                out.push_str(&format!(
                    "{:.4} {:.4} {:.4}\n",
                    (r * 1.08).min(1.0),
                    g,
                    b * 0.9
                ));
            }
        }
    }
    out
}

/// A page that redraws every animation frame.
const PAGE_ANIM: &str = r##"<!doctype html>
<html><body style="margin:0;background:#102030">
<canvas id="c" width="1280" height="720" style="width:100vw;height:100vh"></canvas>
<script>
const c = document.getElementById("c").getContext("2d");
function draw(t) {
  c.fillStyle = "#102030"; c.fillRect(0, 0, 1280, 720);
  for (let i = 0; i < 40; i++) {
    c.fillStyle = `hsl(${(i * 9 + t / 20) % 360} 80% 60%)`;
    const x = 640 + Math.cos(t / 900 + i) * (120 + i * 12);
    const y = 360 + Math.sin(t / 700 + i * 1.3) * (60 + i * 6);
    c.beginPath(); c.arc(x, y, 24, 0, Math.PI * 2); c.fill();
  }
  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);
</script></body></html>
"##;

/// A page that never changes after it loads.
const PAGE_STATIC: &str = r##"<!doctype html>
<html><body style="margin:0;background:#203040;color:#fff;font:48px sans-serif">
<p style="padding:40px">A page that never changes.</p>
</body></html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_has_a_valid_header_and_chunk_crcs() {
        let bytes = png(4, 3, Pixels::Rgb(&|x, y| [x as u8, y as u8, 0])).expect("encodes");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // Walk the chunks, checking each CRC, and end at IEND.
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            let kind = &bytes[at + 4..at + 8];
            let data = &bytes[at + 8..at + 8 + len];
            let mut crc = flate2::Crc::new();
            crc.update(kind);
            crc.update(data);
            let stored =
                u32::from_be_bytes(bytes[at + 8 + len..at + 12 + len].try_into().expect("4"));
            assert_eq!(crc.sum(), stored);
            kinds.push(String::from_utf8_lossy(kind).into_owned());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    }

    #[test]
    fn png_pixels_inflate_back() {
        use std::io::Read as _;
        let bytes = png(2, 2, Pixels::Gray(&|x, y| (x * 10 + y) as u8)).expect("encodes");
        let idat_len = u32::from_be_bytes(bytes[33..37].try_into().expect("4")) as usize;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[41..41 + idat_len])
            .read_to_end(&mut raw)
            .expect("inflates");
        assert_eq!(raw, [0, 0, 10, 0, 1, 11]);
    }

    /// Decodes one frame's LZW stream the way a GIF decoder does, so a code-size slip in
    /// the encoder shows up here rather than as a source that won't open.
    fn lzw_decode(data: &[u8]) -> Vec<u8> {
        let (mut acc, mut len, mut size) = (0u32, 0u32, 9u32);
        let mut table: Vec<Vec<u8>> = Vec::new();
        let reset = |table: &mut Vec<Vec<u8>>| {
            table.clear();
            table.extend((0..=255u8).map(|i| vec![i]));
            table.push(Vec::new());
            table.push(Vec::new());
        };
        reset(&mut table);
        let mut prev: Option<Vec<u8>> = None;
        let mut out = Vec::new();
        let mut bytes = data.iter();
        loop {
            while len < size {
                acc |= u32::from(*bytes.next().expect("stream ends with an end code")) << len;
                len += 8;
            }
            let code = (acc & ((1 << size) - 1)) as usize;
            acc >>= size;
            len -= size;
            match code {
                256 => {
                    reset(&mut table);
                    size = 9;
                    prev = None;
                }
                257 => return out,
                _ => {
                    let entry = if code < table.len() {
                        table[code].clone()
                    } else {
                        let p = prev.clone().expect("a previous code");
                        let mut e = p.clone();
                        e.push(p[0]);
                        e
                    };
                    if let Some(p) = prev {
                        let mut e = p;
                        e.push(entry[0]);
                        table.push(e);
                        if table.len() == 1 << size && size < 12 {
                            size += 1;
                        }
                    }
                    out.extend_from_slice(&entry);
                    prev = Some(entry);
                }
            }
        }
    }

    #[test]
    fn gif_frames_decode_to_their_pixels() {
        let bytes = gif(64, 48, 2, 5);
        assert_eq!(&bytes[..6], b"GIF89a");
        assert_eq!(bytes.last(), Some(&0x3B));
        // The first frame's image data starts after the header (13), palette (768),
        // loop extension (19), graphic control (8) and image descriptor (10), then the
        // minimum code size byte.
        let mut at = 13 + 768 + 19 + 8 + 10;
        assert_eq!(bytes[at], 8);
        at += 1;
        let mut data = Vec::new();
        loop {
            let n = bytes[at] as usize;
            at += 1;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&bytes[at..at + n]);
            at += n;
        }
        let pixels = lzw_decode(&data);
        assert_eq!(pixels.len(), 64 * 48);
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[64 * 10 + 30], ((30 + 10) / 4) as u8);
    }

    #[test]
    fn cube_has_every_entry() {
        let text = cube(3);
        assert!(text.contains("LUT_3D_SIZE 3"));
        assert_eq!(text.lines().count(), 2 + 27);
    }
}
