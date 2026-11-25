//! The Run view's notes as a teleprompter: a toggle, a text size and a scroll rate in
//! words per second, kept by the browser for the next session. Off, the notes are plain
//! text to glance at; on, they are large and roll up at the rate set while Roll is on.
//!
//! Where the text is after so many seconds of reading is a pure function
//! ([`scroll_top`]), tested natively; the component only measures and sets `scrollTop`.
//!
//! Docs: crates/jstream-ui/README.md#design

/// The teleprompter's settings: the operator's own, never in the collection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prompter {
    /// Large rolling text instead of plain notes.
    pub on: bool,
    /// The text size, in pixels, one of [`Prompter::SIZES`].
    pub size: u32,
    /// How fast it rolls, in words per second.
    pub wps: f64,
}

/// Where the browser keeps the settings.
const KEY: &str = "jstream.show.prompter";

impl Prompter {
    /// The text sizes A− and A+ step through, in pixels.
    pub const SIZES: [u32; 8] = [20, 24, 28, 32, 40, 48, 56, 64];
    /// The slowest and fastest scroll rates, in words per second, and the step between.
    pub const WPS: (f64, f64, f64) = (0.5, 6.0, 0.25);

    /// The next text size up or down, staying on the scale's ends.
    pub fn sized(self, up: bool) -> Prompter {
        let last = Self::SIZES.len() - 1;
        let at = Self::SIZES
            .iter()
            .position(|s| *s >= self.size)
            .unwrap_or(last);
        let to = if up {
            (at + 1).min(last)
        } else {
            at.saturating_sub(1)
        };
        Prompter {
            size: Self::SIZES[to],
            ..self
        }
    }

    /// The scroll rate a step faster or slower, within [`Prompter::WPS`].
    pub fn paced(self, faster: bool) -> Prompter {
        let (lo, hi, step) = Self::WPS;
        let wps = if faster {
            self.wps + step
        } else {
            self.wps - step
        };
        Prompter {
            wps: (wps / step).round() * step,
            ..self
        }
        .clamped(lo, hi)
    }

    fn clamped(self, lo: f64, hi: f64) -> Prompter {
        Prompter {
            wps: self.wps.clamp(lo, hi),
            ..self
        }
    }

    /// The settings as the browser keeps them: "on;size;wps".
    pub fn encode(self) -> String {
        format!("{};{};{}", u8::from(self.on), self.size, self.wps)
    }

    /// Settings read back, or the defaults for anything unreadable.
    pub fn decode(text: &str) -> Prompter {
        let mut parts = text.split(';');
        let base = Prompter::default();
        let on = parts.next().map_or(base.on, |p| p == "1");
        let size = parts
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|s| Self::SIZES.contains(s))
            .unwrap_or(base.size);
        let wps = parts
            .next()
            .and_then(|p| p.parse::<f64>().ok())
            .filter(|w| w.is_finite())
            .unwrap_or(base.wps);
        Prompter { on, size, wps }.clamped(Self::WPS.0, Self::WPS.1)
    }

    /// The settings the browser kept, or the defaults.
    pub fn restore() -> Prompter {
        leptos::prelude::window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .map_or_else(Prompter::default, |t| Prompter::decode(&t))
    }

    /// Keeps the settings for the next session.
    pub fn keep(self) {
        if let Ok(Some(storage)) = leptos::prelude::window().local_storage() {
            let _ = storage.set_item(KEY, &self.encode());
        }
    }
}

impl Default for Prompter {
    fn default() -> Prompter {
        Prompter {
            on: false,
            size: 40,
            wps: 2.5,
        }
    }
}

/// How many words a note has, as the scroll rate counts them.
pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// How far down the notes are scrolled after `read_s` seconds of rolling at `wps`: the
/// share of the words read so far, as a share of the text's height, set a quarter of the
/// view below the top so the line being read sits at the reading line. Never above the
/// top, and never past the text's end.
pub fn scroll_top(read_s: f64, wps: f64, words: usize, text_h: f64, view_h: f64) -> f64 {
    if words == 0 || text_h <= 0.0 {
        return 0.0;
    }
    let share = (read_s.max(0.0) * wps / words as f64).min(1.0);
    (share * text_h - view_h * 0.25).clamp(0.0, text_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_rates_step_and_stop_at_their_ends() {
        let p = Prompter::default();
        assert_eq!(p.sized(true).size, 48);
        assert_eq!(p.sized(false).size, 32);
        let big = Prompter { size: 64, ..p };
        assert_eq!(big.sized(true).size, 64);
        let small = Prompter { size: 20, ..p };
        assert_eq!(small.sized(false).size, 20);
        assert_eq!(p.paced(true).wps, 2.75);
        assert_eq!(p.paced(false).wps, 2.25);
        assert_eq!(Prompter { wps: 6.0, ..p }.paced(true).wps, 6.0);
        assert_eq!(Prompter { wps: 0.5, ..p }.paced(false).wps, 0.5);
    }

    #[test]
    fn settings_round_trip_and_bad_ones_fall_back() {
        let p = Prompter {
            on: true,
            size: 56,
            wps: 3.25,
        };
        assert_eq!(Prompter::decode(&p.encode()), p);
        assert_eq!(Prompter::decode(""), Prompter::default());
        let odd = Prompter::decode("1;33;99");
        assert_eq!((odd.on, odd.size, odd.wps), (true, 40, 6.0));
        assert_eq!(Prompter::decode("0;24;NaN").wps, 2.5);
    }

    #[test]
    fn the_scroll_follows_the_words_read() {
        assert_eq!(words("  Welcome   everyone,\nand the guest. "), 5);
        // 100 words at 2 a second, 1000 px of text in a 400 px view: after 25 s, half
        // read, so the middle of the text sits a quarter of the view from the top.
        assert_eq!(scroll_top(25.0, 2.0, 100, 1000.0, 400.0), 400.0);
        // Before the reading line is reached it stays at the top.
        assert_eq!(scroll_top(1.0, 2.0, 100, 1000.0, 400.0), 0.0);
        // Past the end it stops there.
        assert_eq!(scroll_top(500.0, 2.0, 100, 1000.0, 400.0), 900.0);
        assert_eq!(scroll_top(5.0, 2.0, 0, 1000.0, 400.0), 0.0);
    }
}
