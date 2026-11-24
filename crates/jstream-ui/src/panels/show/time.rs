//! Show time on screen: how a time reads ("5:30"), the ruler's ticks, and the part of the
//! show in view, which the zoom bar's two handles and its middle move.
//!
//! Docs: crates/jstream-ui/README.md#design

/// The shortest span the view zooms in to, in ms.
pub const MIN_SPAN_MS: f64 = 5_000.0;

/// A time as the panel writes it: `m:ss`, or `h:mm:ss` from an hour.
pub fn clock(ms: u32) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A signed difference as the clocks write it: `+2:14` or `-0:30`.
pub fn signed(ms: i64) -> String {
    let sign = if ms < 0 { '-' } else { '+' };
    format!("{sign}{}", clock(u32::try_from(ms.unsigned_abs()).unwrap_or(u32::MAX)))
}

/// The part of the show in view: from `start_ms`, `span_ms` long, over a show `len_ms`
/// long. Plain data; the Edit view and the zoom bar both read and change it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// The time at the left edge.
    pub start_ms: f64,
    /// How much time fits across.
    pub span_ms: f64,
    /// The whole show's length; never less than [`MIN_SPAN_MS`].
    pub len_ms: f64,
}

impl Viewport {
    /// The whole show in view.
    pub fn fit(len_ms: u32) -> Viewport {
        let len = f64::from(len_ms).max(MIN_SPAN_MS);
        Viewport {
            start_ms: 0.0,
            span_ms: len,
            len_ms: len,
        }
    }

    /// The same view over a show now `len_ms` long, kept inside it.
    pub fn with_len(self, len_ms: u32) -> Viewport {
        Viewport {
            len_ms: f64::from(len_ms).max(MIN_SPAN_MS),
            ..self
        }
        .clamped()
    }

    /// Kept inside the show, and no narrower than [`MIN_SPAN_MS`].
    pub fn clamped(self) -> Viewport {
        let span = self.span_ms.clamp(MIN_SPAN_MS.min(self.len_ms), self.len_ms);
        let start = self.start_ms.clamp(0.0, self.len_ms - span);
        Viewport {
            start_ms: start,
            span_ms: span,
            len_ms: self.len_ms,
        }
    }

    /// The time at the right edge.
    pub fn end_ms(self) -> f64 {
        self.start_ms + self.span_ms
    }

    /// Where `ms` falls across a view `width` px wide.
    pub fn x(self, ms: f64, width: f64) -> f64 {
        (ms - self.start_ms) / self.span_ms * width
    }

    /// The time at `x` px across a view `width` px wide.
    pub fn ms(self, x: f64, width: f64) -> f64 {
        self.start_ms + x / width.max(1.0) * self.span_ms
    }

    /// Zoomed by `factor` (above 1 zooms out) about the time at `x` px, which stays put:
    /// Ctrl+wheel.
    pub fn zoom_at(self, factor: f64, x: f64, width: f64) -> Viewport {
        let pivot = self.ms(x, width);
        let span = (self.span_ms * factor).clamp(MIN_SPAN_MS.min(self.len_ms), self.len_ms);
        let start = pivot - (pivot - self.start_ms) * span / self.span_ms;
        Viewport {
            start_ms: start,
            span_ms: span,
            ..self
        }
        .clamped()
    }

    /// Scrolled by `ms`: Shift+wheel, or the zoom bar's middle dragged.
    pub fn scrolled(self, ms: f64) -> Viewport {
        Viewport {
            start_ms: self.start_ms + ms,
            ..self
        }
        .clamped()
    }

    /// The zoom bar's thumb, as fractions of the bar: its left and right ends.
    pub fn thumb(self) -> (f64, f64) {
        (self.start_ms / self.len_ms, self.end_ms() / self.len_ms)
    }

    /// The left handle dragged to `fraction` of the bar; the right end stays.
    pub fn with_left(self, fraction: f64) -> Viewport {
        let end = self.end_ms();
        let start = (fraction * self.len_ms).clamp(0.0, end - MIN_SPAN_MS.min(self.len_ms));
        Viewport {
            start_ms: start,
            span_ms: end - start,
            ..self
        }
        .clamped()
    }

    /// The right handle dragged to `fraction` of the bar; the left end stays.
    pub fn with_right(self, fraction: f64) -> Viewport {
        let end = (fraction * self.len_ms)
            .clamp(self.start_ms + MIN_SPAN_MS.min(self.len_ms), self.len_ms);
        Viewport {
            span_ms: end - self.start_ms,
            ..self
        }
        .clamped()
    }

    /// The ruler's labelled ticks across a view `width` px wide: the times, a round step
    /// apart, with labels at least about 90 px apart.
    pub fn ticks(self, width: f64) -> Vec<u32> {
        const STEPS: [u32; 12] = [
            1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000,
            900_000, 1_800_000,
        ];
        let per_px = self.span_ms / width.max(1.0);
        let step = STEPS
            .into_iter()
            .find(|s| f64::from(*s) / per_px >= 90.0)
            .unwrap_or(3_600_000);
        let first = (self.start_ms / f64::from(step)).ceil() as u32;
        (first..)
            .map(|n| n * step)
            .take_while(|t| f64::from(*t) <= self.end_ms())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_reads_minutes_and_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(330_000), "5:30");
        assert_eq!(clock(3_725_000), "1:02:05");
        assert_eq!(signed(134_000), "+2:14");
        assert_eq!(signed(-30_000), "-0:30");
    }

    #[test]
    fn fit_shows_the_whole_show() {
        let v = Viewport::fit(660_000);
        assert_eq!(v.thumb(), (0.0, 1.0));
        assert_eq!(v.x(330_000.0, 1000.0), 500.0);
        assert_eq!(v.ms(500.0, 1000.0), 330_000.0);
    }

    #[test]
    fn zooming_keeps_the_time_under_the_pointer() {
        let v = Viewport::fit(600_000).zoom_at(0.5, 250.0, 1000.0);
        assert_eq!(v.span_ms, 300_000.0);
        assert!((v.ms(250.0, 1000.0) - 150_000.0).abs() < 1e-6);
    }

    #[test]
    fn the_view_stays_inside_the_show() {
        let v = Viewport::fit(600_000).zoom_at(0.5, 0.0, 1000.0).scrolled(1e9);
        assert_eq!(v.end_ms(), 600_000.0);
        let v = v.scrolled(-1e9);
        assert_eq!(v.start_ms, 0.0);
        assert_eq!(Viewport::fit(600_000).zoom_at(1e-9, 0.0, 10.0).span_ms, MIN_SPAN_MS);
    }

    #[test]
    fn dragging_a_handle_zooms_from_the_other_end() {
        let v = Viewport::fit(600_000).with_left(0.25);
        assert_eq!((v.start_ms, v.end_ms()), (150_000.0, 600_000.0));
        let v = v.with_right(0.5);
        assert_eq!((v.start_ms, v.end_ms()), (150_000.0, 300_000.0));
        // A handle can't cross the other.
        let v = v.with_left(0.9);
        assert_eq!(v.span_ms, MIN_SPAN_MS);
    }

    #[test]
    fn ticks_are_round_and_spaced() {
        let ticks = Viewport::fit(660_000).ticks(1760.0);
        assert_eq!(ticks.first(), Some(&0));
        assert_eq!(ticks[1], 60_000);
        assert_eq!(ticks.last(), Some(&660_000));
    }
}
