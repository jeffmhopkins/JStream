//! Checks on what CEF delivered: the page's frame counter, the transparency probes and
//! the test tones. Pure functions, so they are unit-tested on every platform.

use crate::json::{self, Obj};
use crate::page::{BIT_CELL, CELL, CLEAR_X, COUNTER_BITS, GREEN_X, RED_X, ROW_PIXELS};

/// One pixel, from a BGRA8 row (the layout CEF's shared textures use on Windows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Px {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Px {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Pixel `x` of a BGRA8 row.
    pub fn at(row: &[u8], x: u32) -> Option<Px> {
        let i = x as usize * 4;
        let p = row.get(i..i + 4)?;
        Some(Px::rgba(p[2], p[1], p[0], p[3]))
    }

    fn near(self, other: Px, tolerance: u8) -> bool {
        let close = |a: u8, b: u8| a.abs_diff(b) <= tolerance;
        close(self.r, other.r)
            && close(self.g, other.g)
            && close(self.b, other.b)
            && close(self.a, other.a)
    }

    pub fn json(self) -> String {
        json::array([self.r, self.g, self.b, self.a].map(|c| c.to_string()))
    }
}

/// Reads the page's 16-bit frame counter from the probe row. `None` if a cell isn't opaque
/// (the page hasn't drawn yet, or the texture isn't the test page).
pub fn decode_counter(row: &[u8]) -> Option<u16> {
    if row.len() < ROW_PIXELS as usize * 4 {
        return None;
    }
    let mut n = 0u16;
    for bit in 0..COUNTER_BITS {
        let p = Px::at(row, bit * BIT_CELL + BIT_CELL / 2)?;
        if p.a < 200 {
            return None;
        }
        let luma = (u16::from(p.r) + u16::from(p.g) + u16::from(p.b)) / 3;
        if luma >= 128 {
            n |= 1 << bit;
        }
    }
    Some(n)
}

/// Counts page frames across consecutive paints, from the decoded counter.
#[derive(Default)]
pub struct FrameTracker {
    last: Option<u16>,
    /// Paints whose counter was decoded.
    pub decoded: u64,
    /// Paints that carried a page frame not seen before.
    pub new_frames: u64,
    /// Paints that repeated the previous page frame.
    pub repeats: u64,
    /// Page frames that were never painted (counter jumped by more than one).
    pub skipped: u64,
    /// The largest single jump minus one.
    pub max_skip: u64,
    /// Paints whose counter went backwards (out-of-order delivery).
    pub backwards: u64,
    /// Paints whose counter couldn't be read.
    pub undecodable: u64,
}

impl FrameTracker {
    pub fn record(&mut self, counter: Option<u16>) {
        let Some(n) = counter else {
            self.undecodable += 1;
            return;
        };
        self.decoded += 1;
        if let Some(last) = self.last {
            match n.wrapping_sub(last) {
                0 => self.repeats += 1,
                d if d < 0x8000 => {
                    self.new_frames += 1;
                    let skipped = u64::from(d - 1);
                    self.skipped += skipped;
                    self.max_skip = self.max_skip.max(skipped);
                }
                _ => self.backwards += 1,
            }
        }
        self.last = Some(n);
    }

    pub fn json(&self) -> String {
        Obj::new()
            .int("decoded", self.decoded)
            .int("new_frames", self.new_frames)
            .int("repeats", self.repeats)
            .int("skipped", self.skipped)
            .int("max_skip", self.max_skip)
            .int("backwards", self.backwards)
            .int("undecodable", self.undecodable)
            .build()
    }
}

/// Where each probe is read: the middle of its cell.
const fn probe_x(cell_x: u32) -> u32 {
    cell_x + CELL / 2
}

/// Checks that the page arrives with real alpha and that the compositor blends it.
#[derive(Default)]
pub struct ProbeTally {
    pub frames: u64,
    /// The opaque red cell arrived opaque red.
    pub page_opaque_ok: u64,
    /// The 50 % green cell arrived with alpha near 128.
    pub page_half_alpha_ok: u64,
    /// The undrawn cell arrived fully transparent.
    pub page_clear_ok: u64,
    /// The 50 % green cell's colour equals its alpha (premultiplied, as CEF documents).
    pub page_premultiplied: u64,
    /// After compositing, the undrawn cell shows the compositor's background.
    pub composite_clear_ok: u64,
    /// After compositing, the 50 % cell is green blended over the background.
    pub composite_blend_ok: u64,
    /// Last observed probes: page (opaque, half, clear), then composite (half, clear).
    pub last: Option<[Px; 5]>,
}

impl ProbeTally {
    /// `bg` is the compositor's opaque background; it must have no green.
    pub fn record(&mut self, page_row: &[u8], composite_row: &[u8], bg: Px) {
        let probes = (
            Px::at(page_row, probe_x(RED_X)),
            Px::at(page_row, probe_x(GREEN_X)),
            Px::at(page_row, probe_x(CLEAR_X)),
            Px::at(composite_row, probe_x(GREEN_X)),
            Px::at(composite_row, probe_x(CLEAR_X)),
        );
        let (Some(opaque), Some(half), Some(clear), Some(c_half), Some(c_clear)) = probes else {
            return;
        };
        self.frames += 1;
        self.page_opaque_ok += u64::from(opaque.near(Px::rgba(255, 0, 0, 255), 8));
        self.page_half_alpha_ok += u64::from(half.a.abs_diff(128) <= 8 && half.r <= 8);
        self.page_clear_ok += u64::from(clear == Px::default());
        self.page_premultiplied += u64::from(half.g.abs_diff(half.a) <= 8);
        self.composite_clear_ok += u64::from(c_clear.near(bg, 2));
        self.composite_blend_ok += u64::from(c_half.near(blend_half_green(bg), 10));
        self.last = Some([opaque, half, clear, c_half, c_clear]);
    }

    pub fn json(&self) -> String {
        let last = self.last.map_or_else(
            || "null".to_owned(),
            |[opaque, half, clear, c_half, c_clear]| {
                Obj::new()
                    .raw("page_opaque_red", opaque.json())
                    .raw("page_half_green", half.json())
                    .raw("page_clear", clear.json())
                    .raw("composite_half_green", c_half.json())
                    .raw("composite_clear", c_clear.json())
                    .build()
            },
        );
        Obj::new()
            .int("frames", self.frames)
            .int("page_opaque_ok", self.page_opaque_ok)
            .int("page_half_alpha_ok", self.page_half_alpha_ok)
            .int("page_clear_ok", self.page_clear_ok)
            .int("page_premultiplied", self.page_premultiplied)
            .int("composite_clear_ok", self.composite_clear_ok)
            .int("composite_blend_ok", self.composite_blend_ok)
            .raw("last_rgba", last)
            .build()
    }
}

/// Premultiplied 50 % green over an opaque `bg`: `src + bg * (1 - a)`.
fn blend_half_green(bg: Px) -> Px {
    let half = |c: u8| (f64::from(c) * 0.5).round() as u8;
    Px::rgba(
        half(bg.r),
        128u8.saturating_add(half(bg.g)),
        half(bg.b),
        255,
    )
}

/// Level and pitch of one audio channel.
#[derive(Default, Clone)]
pub struct ChannelStats {
    pub frames: u64,
    sum_sq: f64,
    pub peak: f32,
    /// Upward zero crossings.
    crossings: u64,
    last: f32,
}

impl ChannelStats {
    pub fn record(&mut self, samples: &[f32]) {
        for &s in samples {
            self.sum_sq += f64::from(s) * f64::from(s);
            self.peak = self.peak.max(s.abs());
            if self.last < 0.0 && s >= 0.0 {
                self.crossings += 1;
            }
            self.last = s;
        }
        self.frames += samples.len() as u64;
    }

    pub fn rms(&self) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        (self.sum_sq / self.frames as f64).sqrt()
    }

    /// Estimated pitch from zero crossings, for a pure tone.
    pub fn frequency(&self, sample_rate: f64) -> f64 {
        if self.frames == 0 || sample_rate <= 0.0 {
            return f64::NAN;
        }
        self.crossings as f64 / (self.frames as f64 / sample_rate)
    }

    pub fn json(&self, sample_rate: f64) -> String {
        Obj::new()
            .int("frames", self.frames)
            .num("peak_dbfs", dbfs(f64::from(self.peak)))
            .num("rms_dbfs", dbfs(self.rms()))
            .num("frequency_hz", self.frequency(sample_rate))
            .build()
    }
}

pub fn dbfs(linear: f64) -> f64 {
    20.0 * linear.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe row as the test page draws it, after premultiplication.
    fn page_row(counter: u16) -> Vec<u8> {
        let mut row = vec![0u8; ROW_PIXELS as usize * 4];
        let mut fill = |x0: u32, w: u32, p: Px| {
            for x in x0..x0 + w {
                let i = x as usize * 4;
                row[i..i + 4].copy_from_slice(&[p.b, p.g, p.r, p.a]);
            }
        };
        for bit in 0..COUNTER_BITS {
            let v = if counter >> bit & 1 == 1 { 255 } else { 0 };
            fill(bit * BIT_CELL, BIT_CELL, Px::rgba(v, v, v, 255));
        }
        fill(RED_X, CELL, Px::rgba(255, 0, 0, 255));
        fill(GREEN_X, CELL, Px::rgba(0, 128, 0, 128));
        row
    }

    fn composite(page: &[u8], bg: Px) -> Vec<u8> {
        page.chunks(4)
            .flat_map(|p| {
                let inv = 1.0 - f64::from(p[3]) / 255.0;
                let mix = |s: u8, d: u8| (f64::from(s) + f64::from(d) * inv).round() as u8;
                [mix(p[0], bg.b), mix(p[1], bg.g), mix(p[2], bg.r), 255]
            })
            .collect()
    }

    #[test]
    fn counter_round_trips() {
        for n in [0u16, 1, 0x00ff, 0x8001, 0xffff] {
            assert_eq!(decode_counter(&page_row(n)), Some(n));
        }
        assert_eq!(decode_counter(&[0u8; 16]), None);
        // An undrawn (transparent) row has no counter.
        assert_eq!(decode_counter(&vec![0u8; ROW_PIXELS as usize * 4]), None);
    }

    #[test]
    fn tracker_counts_repeats_skips_and_wraps() {
        let mut t = FrameTracker::default();
        for n in [10u16, 11, 11, 14, 0xffff, 0, 1] {
            t.record(Some(n));
        }
        t.record(None);
        // 10->11 new, 11 repeat, 11->14 new (2 skipped), 14->0xffff backwards,
        // 0xffff->0 new (wrap), 0->1 new.
        assert_eq!(t.new_frames, 4);
        assert_eq!(t.repeats, 1);
        assert_eq!((t.skipped, t.max_skip), (2, 2));
        assert_eq!(t.backwards, 1);
        assert_eq!((t.decoded, t.undecodable), (7, 1));
    }

    #[test]
    fn probes_accept_premultiplied_alpha_and_blend() {
        let bg = Px::rgba(0, 0, 255, 255);
        let page = page_row(5);
        let mut tally = ProbeTally::default();
        tally.record(&page, &composite(&page, bg), bg);
        assert_eq!(tally.frames, 1);
        assert_eq!(tally.page_opaque_ok, 1);
        assert_eq!(tally.page_half_alpha_ok, 1);
        assert_eq!(tally.page_clear_ok, 1);
        assert_eq!(tally.page_premultiplied, 1);
        assert_eq!(tally.composite_clear_ok, 1);
        assert_eq!(tally.composite_blend_ok, 1);
    }

    #[test]
    fn probes_catch_an_opaque_page() {
        // A page delivered without alpha: the "clear" cell is opaque black.
        let bg = Px::rgba(0, 0, 255, 255);
        let page: Vec<u8> = page_row(5)
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        let mut tally = ProbeTally::default();
        tally.record(&page, &composite(&page, bg), bg);
        assert_eq!(tally.page_clear_ok, 0);
        assert_eq!(tally.composite_clear_ok, 0);
        assert_eq!(tally.page_half_alpha_ok, 0);
    }

    #[test]
    fn tone_level_and_pitch() {
        let rate = 48_000.0;
        let samples: Vec<f32> = (0..48_000)
            .map(|i| {
                (0.25 * (2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / rate).sin()) as f32
            })
            .collect();
        let mut ch = ChannelStats::default();
        for chunk in samples.chunks(480) {
            ch.record(chunk);
        }
        assert!((ch.frequency(rate) - 440.0).abs() <= 1.0);
        assert!((dbfs(f64::from(ch.peak)) - -12.04).abs() < 0.1);
        // A sine's RMS is 3 dB below its peak.
        assert!((dbfs(ch.rms()) - -15.05).abs() < 0.1);
    }
}
