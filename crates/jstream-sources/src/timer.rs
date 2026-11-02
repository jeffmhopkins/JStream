//! The `timer` source: a countdown or a stopwatch, drawn as text and redrawn each
//! second.
//!
//! It counts on the media clock from when the source starts, or from its last restart:
//! an update that changes `mode` or `duration`. A countdown shows the seconds left,
//! rounded up, so a five-minute one shows 05:00 at its start and 00:00 at its end, then
//! `end_text` if that is set. A stopwatch shows the seconds since its start. Every
//! other setting is the `text` kind's, and draws the same way.

use crate::describe::{kind, param};
use crate::source::{at_millis, changed_non_live, millis, setting};
use crate::text::{self, MAX_DENSITY};
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, VideoFrame};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::Arc;

/// The `timer` kind.
pub(crate) struct TimerKind(KindDescriptor);

/// The modes, by option value, with their label.
const MODES: [(&str, &str); 2] = [("countdown", "Countdown"), ("stopwatch", "Stopwatch")];

/// The formats, by option value, with their label.
const FORMATS: [(&str, &str); 3] = [
    ("auto", "Auto (h:mm:ss from an hour)"),
    ("mm:ss", "mm:ss"),
    ("hh:mm:ss", "hh:mm:ss"),
];

/// The longest `duration`, in seconds: a day.
const MAX_DURATION: i64 = 86_400;

/// The longest `end_text`, in characters.
const MAX_END_TEXT: u32 = 256;

impl TimerKind {
    pub(crate) fn new() -> Self {
        let options = |list: &[(&str, &str)]| ParamType::Enum {
            options: list
                .iter()
                .map(|(value, label)| EnumOption {
                    value: (*value).into(),
                    label: (*label).into(),
                })
                .collect(),
        };
        let p = |id: &str, label: &str, ty: ParamType, default, text: &str| ParamDescriptor {
            live: true,
            description: Some(text.into()),
            ..param(id, label, ty, default)
        };
        let mut params = vec![
            p(
                "mode",
                "Mode",
                options(&MODES),
                json!("countdown"),
                "Count down from Duration, or up from zero. Changing it restarts the count.",
            ),
            p(
                "duration",
                "Duration",
                ParamType::Int {
                    min: Some(0),
                    max: Some(MAX_DURATION),
                    step: Some(1),
                    unit: Some("s".into()),
                },
                json!(300),
                "A countdown's length. Changing it restarts the count.",
            ),
            p(
                "format",
                "Format",
                options(&FORMATS),
                json!("auto"),
                "How the time is written. Auto is mm:ss under an hour and h:mm:ss from one.",
            ),
            p(
                "end_text",
                "End text",
                ParamType::String {
                    multiline: false,
                    max_len: MAX_END_TEXT,
                },
                json!(""),
                "Shown once a countdown reaches zero, such as LIVE; empty keeps 00:00.",
            ),
        ];
        params.extend(text::style_params());
        Self(KindDescriptor {
            params,
            ..kind("timer", "Timer", true, false)
        })
    }
}

impl SourceKind for TimerKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Reads and checks the font file, as the `text` kind does, then draws the start.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let count = Count::read(settings)?;
        let font = text::load_font(settings, ctx)?;
        let shown = count.text(0);
        let (frame, extent) = text::draw(&font, &with_text(settings, &shown), 1.0, &ctx.limits)?;
        Ok(Box::new(TimerSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            limits: ctx.limits,
            font,
            count,
            start: Start::Pending,
            shown,
            frame,
            density: 1.0,
            extent,
        }))
    }
}

/// When the count started, on the media clock.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Start {
    /// It starts at the next frame asked for: the source's first, or the first after a
    /// restart, since an update doesn't know the time.
    Pending,
    /// The media time it started at, in milliseconds.
    At(u64),
}

/// A running `timer` source. Its frame is drawn when the text it shows changes.
struct TimerSource {
    descriptor: KindDescriptor,
    settings: Settings,
    limits: Limits,
    font: Cow<'static, [u8]>,
    count: Count,
    start: Start,
    /// The text the frame shows.
    shown: String,
    frame: Arc<VideoFrame>,
    /// As the `text` kind's (ADR-0073).
    density: f32,
    extent: FrameSize,
}

impl TimerSource {
    /// Milliseconds since the count started, at `at`, starting it if it is pending.
    fn elapsed(&mut self, at: MediaTime) -> u64 {
        let now = millis(at);
        let start = match self.start {
            Start::Pending => {
                self.start = Start::At(now);
                now
            }
            Start::At(start) => start,
        };
        now.saturating_sub(start)
    }

    fn redraw(&mut self) -> Result<(), SourceError> {
        let settings = with_text(&self.settings, &self.shown);
        (self.frame, self.extent) = text::draw(&self.font, &settings, self.density, &self.limits)?;
        Ok(())
    }
}

impl Source for TimerSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        let elapsed = self.elapsed(at);
        let shown = self.count.text(elapsed);
        if shown != self.shown {
            self.shown = shown;
            self.redraw()?;
        }
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        let Start::At(start) = self.start else {
            // Not started: the first frame starts it.
            return Some(at);
        };
        let now = millis(at);
        let elapsed = now.saturating_sub(start);
        if self.count.mode == Mode::Countdown && elapsed >= self.count.duration_ms {
            return None;
        }
        // The next whole second of the count.
        Some(at_millis(start + (elapsed / 1000 + 1) * 1000))
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let count = Count::read(settings)?;
        let font = match text::bundled_face(settings)? {
            Some(face) => Cow::Borrowed(face),
            None => self.font.clone(),
        };
        let restart = count.mode != self.count.mode || count.duration_ms != self.count.duration_ms;
        let (start, shown) = match self.start {
            Start::At(_) if !restart => (self.start, self.shown.clone()),
            _ => (Start::Pending, count.text(0)),
        };
        let drawn = text::draw(
            &font,
            &with_text(settings, &shown),
            self.density,
            &self.limits,
        )?;
        (self.frame, self.extent) = drawn;
        (self.font, self.count, self.start, self.shown) = (font, count, start, shown);
        self.settings = settings.clone();
        Ok(())
    }

    /// As the `text` kind's: draws again at `density`, halving it until the frame fits.
    fn set_density(&mut self, density: f32) -> Option<FrameSize> {
        let mut density = if density.is_finite() {
            density.clamp(1.0, MAX_DENSITY)
        } else {
            1.0
        };
        while density != self.density {
            let settings = with_text(&self.settings, &self.shown);
            match text::draw(&self.font, &settings, density, &self.limits) {
                Ok((frame, extent)) => {
                    (self.frame, self.extent, self.density) = (frame, extent, density);
                    break;
                }
                Err(_) => density = (density / 2.0).max(1.0),
            }
        }
        Some(self.extent)
    }
}

/// `settings` with `text` set to `shown`, for the `text` kind's drawing.
fn with_text(settings: &Settings, shown: &str) -> Settings {
    let mut settings = settings.clone();
    settings
        .0
        .insert("text".into(), Value::String(shown.into()));
    settings
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Countdown,
    Stopwatch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    Auto,
    MinutesSeconds,
    HoursMinutesSeconds,
}

/// What the timer counts and how it writes it: every setting but the text style.
#[derive(Clone, Debug, PartialEq)]
struct Count {
    mode: Mode,
    duration_ms: u64,
    format: Format,
    end_text: String,
}

impl Count {
    fn read(settings: &Settings) -> Result<Count, SourceError> {
        let mode = match setting(settings, "mode")?.as_str() {
            Some("countdown") => Mode::Countdown,
            Some("stopwatch") => Mode::Stopwatch,
            _ => return Err(text::bad("mode", "must be `countdown` or `stopwatch`")),
        };
        let duration = setting(settings, "duration")?
            .as_i64()
            .filter(|d| (0..=MAX_DURATION).contains(d))
            .ok_or_else(|| {
                text::bad(
                    "duration",
                    &format!("must be a whole number of seconds from 0 to {MAX_DURATION}"),
                )
            })?;
        let format = match setting(settings, "format")?.as_str() {
            Some("auto") => Format::Auto,
            Some("mm:ss") => Format::MinutesSeconds,
            Some("hh:mm:ss") => Format::HoursMinutesSeconds,
            _ => {
                return Err(text::bad("format", "must be `auto`, `mm:ss` or `hh:mm:ss`"));
            }
        };
        let end_text = setting(settings, "end_text")?
            .as_str()
            .filter(|t| t.chars().count() <= MAX_END_TEXT as usize)
            .ok_or_else(|| {
                text::bad(
                    "end_text",
                    &format!("must be a string of at most {MAX_END_TEXT} characters"),
                )
            })?;
        Ok(Count {
            mode,
            // Within a day, so the product is far inside a u64.
            duration_ms: duration as u64 * 1000,
            format,
            end_text: end_text.to_owned(),
        })
    }

    /// The text shown `elapsed_ms` after the start.
    fn text(&self, elapsed_ms: u64) -> String {
        let seconds = match self.mode {
            Mode::Countdown => {
                let left = self.duration_ms.saturating_sub(elapsed_ms);
                if left == 0 && !self.end_text.is_empty() {
                    return self.end_text.clone();
                }
                left.div_ceil(1000)
            }
            Mode::Stopwatch => elapsed_ms / 1000,
        };
        let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
        match self.format {
            Format::Auto if h == 0 => format!("{m:02}:{s:02}"),
            Format::Auto => format!("{h}:{m:02}:{s:02}"),
            // Minutes past an hour keep counting: 75:00, not 15:00.
            Format::MinutesSeconds => format!("{:02}:{s:02}", seconds / 60),
            Format::HoursMinutesSeconds => format!("{h:02}:{m:02}:{s:02}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(mode: Mode, seconds: u64, format: Format, end: &str) -> Count {
        Count {
            mode,
            duration_ms: seconds * 1000,
            format,
            end_text: end.into(),
        }
    }

    #[test]
    fn a_countdown_rounds_up_and_ends_on_its_end_text() {
        let c = count(Mode::Countdown, 300, Format::Auto, "LIVE");
        assert_eq!(c.text(0), "05:00");
        assert_eq!(c.text(1), "05:00");
        assert_eq!(c.text(999), "05:00");
        assert_eq!(c.text(1000), "04:59");
        assert_eq!(c.text(299_001), "00:01");
        assert_eq!(c.text(300_000), "LIVE");
        assert_eq!(c.text(999_999), "LIVE");
        let plain = count(Mode::Countdown, 300, Format::Auto, "");
        assert_eq!(plain.text(300_000), "00:00");
    }

    #[test]
    fn a_stopwatch_rounds_down() {
        let c = count(Mode::Stopwatch, 0, Format::Auto, "");
        assert_eq!(c.text(0), "00:00");
        assert_eq!(c.text(999), "00:00");
        assert_eq!(c.text(61_000), "01:01");
        assert_eq!(c.text(3_661_000), "1:01:01");
    }

    #[test]
    fn the_formats_write_hours_their_way() {
        let at = |format| count(Mode::Stopwatch, 0, format, "").text(4_500_000);
        assert_eq!(at(Format::Auto), "1:15:00");
        assert_eq!(at(Format::MinutesSeconds), "75:00");
        assert_eq!(at(Format::HoursMinutesSeconds), "01:15:00");
    }
}
