//! The `timer` source: a countdown, a stopwatch or the time of day, drawn as text and
//! redrawn when the time it shows changes.
//!
//! A countdown or stopwatch counts on the media clock from the source's time zero, the
//! `at` its frames are asked for at. The engine keeps that zero when it builds a timer
//! again for a change that doesn't restart it (every setting but `mode`, `duration`
//! and the `restart` counter, [`SourceKind::keeps_time`]) and when it draws it at another density, so
//! editing a timer's look or resizing its item keeps the count going. A clock shows the
//! wall-clock time in the machine's time zone, or the one `time_zone` names.
//!
//! Every frame has one size: the text is laid out in a fixed box, measured when the
//! source is created or updated to hold the widest value the count can show (each
//! digit position at the font's widest digit) and `end_text`, so a count that turns to
//! LIVE, or from 59:59 to 1:00:00, never changes the frame the engine placed. The next
//! value is drawn in the source's [`Painter`], off any lock; `frame` hands out the one
//! drawn last.

use crate::color::rgba_frame;
use crate::describe::{kind, param};
use crate::source::{at_millis, changed_non_live, millis, setting};
use crate::text::raster::{Bounds, measure, render_within};
use crate::text::{self, MAX_DENSITY, Style};
use crate::{Context, Limits, Painter, Source, SourceError, SourceKind};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;
use std::borrow::Cow;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The modes, by option value, with their label.
const MODES: [(&str, &str); 3] = [
    ("countdown", "Countdown"),
    ("stopwatch", "Stopwatch"),
    ("clock", "Clock"),
];

/// The count's formats, by option value, with their label.
const FORMATS: [(&str, &str); 3] = [
    ("auto", "Auto (h:mm:ss from an hour)"),
    ("mm:ss", "mm:ss"),
    ("hh:mm:ss", "hh:mm:ss"),
];

/// The clock's formats, by option value, with their label.
const CLOCK_FORMATS: [(&str, &str); 4] = [
    ("24h", "24-hour (14:05:09)"),
    ("12h", "12-hour (2:05:09 PM)"),
    ("24h_short", "24-hour, no seconds (14:05)"),
    ("12h_short", "12-hour, no seconds (2:05 PM)"),
];

/// The longest `duration`, in seconds: a day.
const MAX_DURATION: i64 = 86_400;

/// Where a stopwatch stops, in seconds: 99:59:59, or 99:59 written `mm:ss`, the widest
/// its box holds.
const MAX_STOPWATCH: u64 = 99 * 3600 + 59 * 60 + 59;
const MAX_STOPWATCH_MM_SS: u64 = 99 * 60 + 59;

/// The longest `end_text`, in characters.
const MAX_END_TEXT: u32 = 256;

/// The longest `time_zone`, in characters: IANA names are far shorter.
const MAX_ZONE: u32 = 64;

/// The `timer` kind.
pub(crate) struct TimerKind(KindDescriptor);

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
        let p = |id: &str, label: &str, ty: ParamType, default, live, text: &str| ParamDescriptor {
            live,
            description: Some(text.into()),
            ..param(id, label, ty, default)
        };
        let mut params = vec![
            p(
                "mode",
                "Mode",
                options(&MODES),
                json!("countdown"),
                false,
                "Count down from Duration, count up from zero, or show the time of day. \
                 Changing it restarts the count.",
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
                false,
                "A countdown's length. Changing it restarts the count.",
            ),
            p(
                "restart",
                "Restart",
                ParamType::Int {
                    min: Some(0),
                    max: None,
                    step: Some(1),
                    unit: None,
                },
                json!(0),
                false,
                "A counter: any change to it starts the count again from its start, \
                 keeping Mode and Duration. Its value means nothing else.",
            ),
            p(
                "format",
                "Format",
                options(&FORMATS),
                json!("auto"),
                true,
                "How a countdown or stopwatch is written. Auto is mm:ss under an hour and \
                 h:mm:ss from one. A stopwatch stops at 99:59:59, or 99:59 as mm:ss.",
            ),
            p(
                "end_text",
                "End text",
                ParamType::String {
                    multiline: false,
                    max_len: MAX_END_TEXT,
                },
                json!(""),
                true,
                "Shown once a countdown reaches zero, such as LIVE. Empty keeps 00:00.",
            ),
            p(
                "clock_format",
                "Clock format",
                options(&CLOCK_FORMATS),
                json!("24h"),
                true,
                "How a clock writes the time of day.",
            ),
            p(
                "time_zone",
                "Time zone",
                ParamType::String {
                    multiline: false,
                    max_len: MAX_ZONE,
                },
                json!(""),
                true,
                "A clock's IANA time zone, such as Europe/London. Empty is this computer's.",
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

    /// Reads and checks the font file, as the `text` kind does, measures the box, then
    /// draws the value at its time zero (a clock: now).
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let font = Arc::new(text::load_font(settings, ctx)?);
        let plan = Plan::new(font, settings, 1.0, &ctx.limits)?;
        let key = plan.count.key(0, wall_millis());
        let frame = plan.draw(key, &ctx.limits)?;
        Ok(Box::new(TimerSource {
            descriptor: self.0.clone(),
            painter: Arc::new(TimerPainter {
                limits: ctx.limits,
                drawing: Mutex::new(()),
                ready: Mutex::new(Ready {
                    settings: settings.clone(),
                    plan: Arc::new(plan),
                    revision: 0,
                    key,
                    frame,
                }),
            }),
        }))
    }

    /// Only `mode`, `duration` and the `restart` counter restart the count; any other
    /// change, the font included, keeps it.
    fn keeps_time(&self, old: &Settings, new: &Settings) -> bool {
        ["mode", "duration", "restart"]
            .iter()
            .all(|id| old.get(id) == new.get(id))
    }
}

/// A running `timer` source. Its [`Painter`] draws a value when the time shown changes;
/// `frame` only hands out the frame drawn last, so a caller holding a lock while it asks
/// never waits on drawing.
struct TimerSource {
    descriptor: KindDescriptor,
    painter: Arc<TimerPainter>,
}

impl Source for TimerSource {
    /// Hands out the frame drawn last. It neither draws nor allocates, and never fails.
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(self.painter.frame())
    }

    /// Now, when the frame drawn last doesn't show `at`'s value (the painter hasn't
    /// caught up yet); else when the value next changes: the next second of the count,
    /// or of the wall clock (its next minute for a clock without seconds). `None` once
    /// a countdown has ended or a stopwatch has stopped.
    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.painter.next_change(at)
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        let painter = &self.painter;
        let _drawing = lock(&painter.drawing);
        let (old, plan) = {
            let ready = lock(&painter.ready);
            (ready.settings.clone(), Arc::clone(&ready.plan))
        };
        if let Some(param) = changed_non_live(&self.descriptor, &old, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let font = match text::bundled_face(settings)? {
            Some(face) => Arc::new(Cow::Borrowed(face)),
            None => Arc::clone(&plan.font),
        };
        let shown = lock(&painter.ready).key;
        let plan = Plan::new(font, settings, plan.density, &painter.limits)?;
        let frame = plan.draw(shown, &painter.limits)?;
        let mut ready = lock(&painter.ready);
        ready.settings = settings.clone();
        ready.plan = Arc::new(plan);
        ready.revision = ready.revision.wrapping_add(1);
        ready.frame = frame;
        Ok(())
    }

    /// As the `text` kind's: draws the value shown again at `density`, from 1 to
    /// [`MAX_DENSITY`], halving it until the frame fits the limits, down to 1. The box
    /// is the same at every density.
    fn set_density(&mut self, density: f32) -> Option<FrameSize> {
        let painter = &self.painter;
        let _drawing = lock(&painter.drawing);
        let (plan, shown) = {
            let ready = lock(&painter.ready);
            (Arc::clone(&ready.plan), ready.key)
        };
        let mut density = if density.is_finite() {
            density.clamp(1.0, MAX_DENSITY)
        } else {
            1.0
        };
        while density != plan.density {
            let dense = Plan {
                density,
                ..(*plan).clone()
            };
            match dense.draw(shown, &painter.limits) {
                Ok(frame) => {
                    let mut ready = lock(&painter.ready);
                    ready.plan = Arc::new(dense);
                    ready.revision = ready.revision.wrapping_add(1);
                    ready.frame = frame;
                    break;
                }
                Err(_) => density = (density / 2.0).max(1.0),
            }
        }
        Some(plan.extent)
    }

    fn painter(&self) -> Option<Arc<dyn Painter>> {
        let painter: Arc<dyn Painter> = self.painter.clone();
        Some(painter)
    }
}

/// A `timer` source's drawing, shared between the source and whoever paints it.
struct TimerPainter {
    limits: Limits,
    /// Held while a frame is drawn, so paints, updates and densities take turns.
    drawing: Mutex<()>,
    /// What the frame drawn last shows. Held only to copy or swap it, so
    /// [`Source::frame`] never waits on drawing.
    ready: Mutex<Ready>,
}

struct Ready {
    settings: Settings,
    plan: Arc<Plan>,
    /// Bumped by every update and density, so a paint that started under an older plan
    /// is dropped.
    revision: u64,
    /// The value the frame shows ([`Count::key`]).
    key: Key,
    frame: Arc<VideoFrame>,
}

impl Painter for TimerPainter {
    fn frame(&self) -> Arc<VideoFrame> {
        Arc::clone(&lock(&self.ready).frame)
    }

    /// As [`TimerSource`]'s `next_change`.
    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        let (count, shown) = {
            let ready = lock(&self.ready);
            (ready.plan.count.mode_timing(), ready.key)
        };
        let elapsed = millis(at);
        let wall = count.needs_wall().then(wall_millis).unwrap_or(0);
        if count.key(elapsed, wall) != shown {
            return Some(at);
        }
        count.next(elapsed, wall).map(at_millis)
    }

    /// Draws `at`'s value if the frame drawn last shows another. Only that redraw
    /// allocates, once a second at most (a minute for a clock without seconds).
    fn paint(&self, at: MediaTime) {
        let elapsed = millis(at);
        let (plan, shown) = {
            let ready = lock(&self.ready);
            (Arc::clone(&ready.plan), ready.key)
        };
        let key = plan.count.key(elapsed, wall_millis());
        if key == shown {
            return;
        }
        let _drawing = lock(&self.drawing);
        let (plan, revision) = {
            let ready = lock(&self.ready);
            (Arc::clone(&ready.plan), ready.revision)
        };
        // An update may have swapped the plan meanwhile (a new `end_text` changes
        // which value an ended countdown shows), so the value is read from this one.
        let key = plan.count.key(elapsed, wall_millis());
        // The box holds every value, so a draw that fits once always fits; were it
        // ever to fail, the frame drawn last stays.
        let Ok(frame) = plan.draw(key, &self.limits) else {
            return;
        };
        let mut ready = lock(&self.ready);
        if ready.revision == revision {
            ready.key = key;
            ready.frame = frame;
        }
    }
}

/// Locks `mutex`; a panic while it was held leaves nothing half-done that matters for
/// drawing, so a poisoned lock is used as it is.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The wall clock, in milliseconds since the Unix epoch.
fn wall_millis() -> i64 {
    Timestamp::now().as_millisecond()
}

/// How a timer draws: its font, its fixed box and its count, at a density.
#[derive(Clone)]
struct Plan {
    /// The font file, already checked. Shared, so a paint takes it without copying.
    font: Arc<Cow<'static, [u8]>>,
    /// The text style, wrapping at the box's width so every value aligns in it.
    style: Style,
    /// The frame every value is drawn into.
    within: Bounds,
    /// The frame's size at a density of 1, which it is placed at.
    extent: FrameSize,
    density: f32,
    count: Count,
}

impl Plan {
    /// Measures the box every value `settings` can show fits in, `end_text` included,
    /// and checks that a frame that size fits `limits`, so a value too large fails here
    /// and not when it is reached.
    fn new(
        font: Arc<Cow<'static, [u8]>>,
        settings: &Settings,
        density: f32,
        limits: &Limits,
    ) -> Result<Self, SourceError> {
        let count = Count::read(settings)?;
        let style = text::text_style(settings, None)?;
        let face = text::parse(&font).map_err(|reason| SourceError::Decode {
            path: "(font)".into(),
            format: "a TrueType or OpenType font",
            reason,
        })?;
        let candidates = count.candidates();
        // The box is as wide as the widest candidate; the letter spacing after a line's
        // last character counts towards wrapping, so it is added, and a pixel spare.
        let widths: Vec<f32> = candidates
            .iter()
            .map(|c| measure(&face, c, &style).1)
            .collect();
        let widest = widths.iter().copied().fold(0.0, f32::max);
        let wrap = (widest + style.letter_spacing.max(0.0)).ceil() + 1.0;
        // What a box too large for the limits is blamed on: the end text when it is
        // the widest candidate (it is the last one, when there is one), else the size.
        let culprit = match (count.ends_with_text(), widths.last()) {
            (true, Some(&end)) if end >= widest => "end_text",
            _ => "size",
        };
        if wrap > limits.max_side as f32 {
            return Err(text::bad(
                culprit,
                &format!(
                    "it draws {wrap:.0} pixels across, over the {} pixel limit",
                    limits.max_side
                ),
            ));
        }
        // At most `max_side`, so it fits a u32.
        let style = Style {
            wrap_width: Some(wrap.max(1.0) as u32),
            ..style
        };
        let within = candidates
            .iter()
            .map(|c| measure(&face, c, &style).0)
            .reduce(Bounds::union)
            .unwrap_or_else(|| measure(&face, "", &style).0);
        let [left, top, right, bottom] = within.frame;
        let side = |v: f32| v.clamp(1.0, u32::MAX as f32) as u32;
        let extent = FrameSize::new(side(right - left), side(bottom - top))?;
        let plan = Self {
            font,
            style,
            within,
            extent,
            density,
            count,
        };
        // Every frame is this size, so drawing one checks them all. The text renderer
        // names its own `text` setting, which a timer doesn't have.
        plan.draw(Key::End, limits).map_err(|e| match e {
            SourceError::Setting { param, reason } if param == "text" => SourceError::Setting {
                param: culprit.to_owned(),
                reason,
            },
            other => other,
        })?;
        Ok(plan)
    }

    /// Draws the value `key` names into the box, at the plan's density.
    fn draw(&self, key: Key, limits: &Limits) -> Result<Arc<VideoFrame>, SourceError> {
        let face = text::parse(&self.font).map_err(|reason| SourceError::Decode {
            path: "(font)".into(),
            format: "a TrueType or OpenType font",
            reason,
        })?;
        let shown = self.count.text(key);
        let drawn = render_within(
            &face,
            &shown,
            &self.style,
            self.density,
            limits,
            Some(self.within),
        )?;
        let frame = rgba_frame(drawn.size, drawn.rgba, MediaTime::zero(Timebase::MILLIS))?;
        Ok(Arc::new(frame))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Countdown,
    Stopwatch,
    Clock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Auto,
    MinutesSeconds,
    HoursMinutesSeconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockFormat {
    H24,
    H12,
    H24Short,
    H12Short,
}

impl ClockFormat {
    fn seconds(self) -> bool {
        matches!(self, Self::H24 | Self::H12)
    }

    fn twelve(self) -> bool {
        matches!(self, Self::H12 | Self::H12Short)
    }
}

/// The value a frame shows, compared without drawing or allocating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// A countdown's seconds left, or a stopwatch's seconds since its start.
    Seconds(u64),
    /// A countdown that has ended with an `end_text`.
    End,
    /// A clock: the wall-clock second (or minute, without seconds) since the epoch.
    Wall(i64),
}

/// What the timer counts and how it writes it: every setting but the text style.
#[derive(Clone)]
struct Count {
    mode: Mode,
    duration_ms: u64,
    format: Format,
    end_text: String,
    clock: ClockFormat,
    zone: TimeZone,
}

/// The parts of a [`Count`] its timing needs, copied out under the lock without
/// allocating.
#[derive(Clone, Copy)]
struct Timing {
    mode: Mode,
    duration_ms: u64,
    format: Format,
    end: bool,
    clock: ClockFormat,
}

impl Count {
    fn read(settings: &Settings) -> Result<Count, SourceError> {
        let choice = |id: &str, list: &[(&str, &str)]| -> Result<usize, SourceError> {
            let value = setting(settings, id)?.as_str();
            list.iter()
                .position(|(v, _)| Some(*v) == value)
                .ok_or_else(|| {
                    let names: Vec<String> = list.iter().map(|(v, _)| format!("`{v}`")).collect();
                    text::bad(id, &format!("must be one of {}", names.join(", ")))
                })
        };
        let mode = [Mode::Countdown, Mode::Stopwatch, Mode::Clock][choice("mode", &MODES)?];
        let format = [
            Format::Auto,
            Format::MinutesSeconds,
            Format::HoursMinutesSeconds,
        ][choice("format", &FORMATS)?];
        let clock = [
            ClockFormat::H24,
            ClockFormat::H12,
            ClockFormat::H24Short,
            ClockFormat::H12Short,
        ][choice("clock_format", &CLOCK_FORMATS)?];
        let duration = setting(settings, "duration")?
            .as_i64()
            .filter(|d| (0..=MAX_DURATION).contains(d))
            .ok_or_else(|| {
                text::bad(
                    "duration",
                    &format!("must be a whole number of seconds from 0 to {MAX_DURATION}"),
                )
            })?;
        let end_text = setting(settings, "end_text")?
            .as_str()
            .filter(|t| t.chars().count() <= MAX_END_TEXT as usize)
            .ok_or_else(|| {
                text::bad(
                    "end_text",
                    &format!("must be a string of at most {MAX_END_TEXT} characters"),
                )
            })?;
        let zone_name = setting(settings, "time_zone")?
            .as_str()
            .ok_or_else(|| text::bad("time_zone", "must be a string"))?;
        let zone = if zone_name.trim().is_empty() {
            // No zone found for this machine draws UTC rather than failing the source.
            TimeZone::try_system().unwrap_or(TimeZone::UTC)
        } else {
            TimeZone::get(zone_name.trim()).map_err(|_| {
                text::bad(
                    "time_zone",
                    "must be an IANA time zone name, such as Europe/London, or empty",
                )
            })?
        };
        Ok(Count {
            mode,
            // Within a day, so the product is far inside a u64.
            duration_ms: duration.unsigned_abs() * 1000,
            format,
            // A line break would make the box two lines; it is drawn as a space.
            end_text: end_text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
            clock,
            zone,
        })
    }

    /// Whether the timer can show `end_text`, which [`candidates`](Self::candidates)
    /// then lists last.
    fn ends_with_text(&self) -> bool {
        self.mode == Mode::Countdown && !self.end_text.is_empty()
    }

    fn mode_timing(&self) -> Timing {
        Timing {
            mode: self.mode,
            duration_ms: self.duration_ms,
            format: self.format,
            end: !self.end_text.is_empty(),
            clock: self.clock,
        }
    }

    fn key(&self, elapsed_ms: u64, wall_ms: i64) -> Key {
        self.mode_timing().key(elapsed_ms, wall_ms)
    }

    /// The text `key` shows.
    fn text(&self, key: Key) -> String {
        match key {
            Key::End => self.end_text.clone(),
            Key::Seconds(seconds) => count_text(seconds, self.format),
            Key::Wall(at) => {
                let unit = if self.clock.seconds() { 1 } else { 60 };
                let ts = Timestamp::from_second(at.saturating_mul(unit))
                    .unwrap_or(Timestamp::UNIX_EPOCH);
                let civil = ts.to_zoned(self.zone.clone());
                clock_text(
                    civil.hour().unsigned_abs().into(),
                    civil.minute().unsigned_abs().into(),
                    civil.second().unsigned_abs().into(),
                    self.clock,
                )
            }
        }
    }

    /// Texts as wide as the widest the timer can show, for its box: each shape a value
    /// can take with every digit set to each of 0 to 9 in turn, and `end_text`.
    fn candidates(&self) -> Vec<String> {
        let mut shapes: Vec<String> = match self.mode {
            // Midnight and one o'clock, morning and afternoon: every width the hour
            // and its AM or PM take.
            Mode::Clock => [0, 1, 12, 13]
                .into_iter()
                .map(|hour| clock_text(hour, 0, 0, self.clock))
                .collect(),
            Mode::Countdown | Mode::Stopwatch => {
                let max = match self.mode {
                    Mode::Countdown => self.duration_ms / 1000,
                    _ if self.format == Format::MinutesSeconds => MAX_STOPWATCH_MM_SS,
                    _ => MAX_STOPWATCH,
                };
                [0, 59, 599, 3599, 35_999, max]
                    .into_iter()
                    .filter(|&v| v <= max)
                    .map(|v| count_text(v, self.format))
                    .collect()
            }
        };
        shapes.sort();
        shapes.dedup();
        let mut candidates: Vec<String> = shapes
            .iter()
            .flat_map(|shape| {
                ('0'..='9').map(move |d| {
                    shape
                        .chars()
                        .map(|c| if c.is_ascii_digit() { d } else { c })
                        .collect()
                })
            })
            .collect();
        if self.ends_with_text() {
            candidates.push(self.end_text.clone());
        }
        candidates
    }
}

impl Timing {
    fn needs_wall(&self) -> bool {
        self.mode == Mode::Clock
    }

    /// The value shown `elapsed_ms` after the time zero, or at `wall_ms` for a clock.
    fn key(&self, elapsed_ms: u64, wall_ms: i64) -> Key {
        match self.mode {
            Mode::Countdown => {
                let left = self.duration_ms.saturating_sub(elapsed_ms);
                if left == 0 && self.end {
                    Key::End
                } else {
                    Key::Seconds(left.div_ceil(1000))
                }
            }
            Mode::Stopwatch => Key::Seconds((elapsed_ms / 1000).min(self.stop())),
            Mode::Clock => Key::Wall(wall_ms.div_euclid(self.wall_unit())),
        }
    }

    /// The time zero's offset in milliseconds at which the value next changes.
    fn next(&self, elapsed_ms: u64, wall_ms: i64) -> Option<u64> {
        match self.mode {
            Mode::Countdown => {
                let left = self
                    .duration_ms
                    .checked_sub(elapsed_ms)
                    .filter(|&l| l > 0)?;
                // The value is `left` rounded up to a second; it drops when `left`
                // reaches the second below.
                let shown = left.div_ceil(1000);
                Some(self.duration_ms - (shown - 1) * 1000)
            }
            Mode::Stopwatch => {
                let seconds = elapsed_ms / 1000;
                (seconds < self.stop()).then(|| (seconds + 1) * 1000)
            }
            Mode::Clock => {
                let unit = self.wall_unit();
                let wait = unit - wall_ms.rem_euclid(unit);
                Some(elapsed_ms.saturating_add(wait.unsigned_abs()))
            }
        }
    }

    fn stop(&self) -> u64 {
        if self.format == Format::MinutesSeconds {
            MAX_STOPWATCH_MM_SS
        } else {
            MAX_STOPWATCH
        }
    }

    /// A clock's step in milliseconds: a second, or a minute without seconds.
    fn wall_unit(&self) -> i64 {
        if self.clock.seconds() { 1000 } else { 60_000 }
    }
}

/// `seconds` written in `format`.
fn count_text(seconds: u64, format: Format) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match format {
        Format::Auto if h == 0 => format!("{m:02}:{s:02}"),
        Format::Auto => format!("{h}:{m:02}:{s:02}"),
        // Minutes past an hour keep counting: 75:00, not 15:00.
        Format::MinutesSeconds => format!("{:02}:{s:02}", seconds / 60),
        Format::HoursMinutesSeconds => format!("{h:02}:{m:02}:{s:02}"),
    }
}

/// A time of day, `hour` from 0 to 23, written in `format`.
fn clock_text(hour: u32, minute: u32, second: u32, format: ClockFormat) -> String {
    let (shown, suffix) = if format.twelve() {
        let twelve = match hour % 12 {
            0 => 12,
            h => h,
        };
        (twelve, if hour < 12 { " AM" } else { " PM" })
    } else {
        (hour, "")
    };
    let hour = if format.twelve() {
        format!("{shown}")
    } else {
        format!("{shown:02}")
    };
    if format.seconds() {
        format!("{hour}:{minute:02}:{second:02}{suffix}")
    } else {
        format!("{hour}:{minute:02}{suffix}")
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
            clock: ClockFormat::H24,
            zone: TimeZone::UTC,
        }
    }

    fn at(c: &Count, ms: u64) -> String {
        c.text(c.key(ms, 0))
    }

    #[test]
    fn a_countdown_rounds_up_and_ends_on_its_end_text() {
        let c = count(Mode::Countdown, 300, Format::Auto, "LIVE");
        assert_eq!(at(&c, 0), "05:00");
        assert_eq!(at(&c, 1), "05:00");
        assert_eq!(at(&c, 999), "05:00");
        assert_eq!(at(&c, 1000), "04:59");
        assert_eq!(at(&c, 299_001), "00:01");
        assert_eq!(at(&c, 300_000), "LIVE");
        assert_eq!(at(&c, 999_999), "LIVE");
        let plain = count(Mode::Countdown, 300, Format::Auto, "");
        assert_eq!(at(&plain, 300_000), "00:00");
    }

    #[test]
    fn a_countdown_changes_on_each_second_and_then_never() {
        let t = count(Mode::Countdown, 3, Format::Auto, "").mode_timing();
        assert_eq!(t.next(0, 0), Some(1000));
        assert_eq!(t.next(999, 0), Some(1000));
        assert_eq!(t.next(1000, 0), Some(2000));
        assert_eq!(t.next(2999, 0), Some(3000));
        assert_eq!(t.next(3000, 0), None);
    }

    #[test]
    fn a_stopwatch_rounds_down_and_stops_at_its_widest() {
        let c = count(Mode::Stopwatch, 0, Format::Auto, "");
        assert_eq!(at(&c, 0), "00:00");
        assert_eq!(at(&c, 999), "00:00");
        assert_eq!(at(&c, 61_000), "01:01");
        assert_eq!(at(&c, 3_661_000), "1:01:01");
        assert_eq!(at(&c, u64::MAX / 2), "99:59:59");
        let t = c.mode_timing();
        assert_eq!(t.next(1500, 0), Some(2000));
        assert_eq!(t.next(MAX_STOPWATCH * 1000, 0), None);
        let short = count(Mode::Stopwatch, 0, Format::MinutesSeconds, "");
        assert_eq!(at(&short, 10_000_000), "99:59");
    }

    #[test]
    fn the_formats_write_hours_their_way() {
        let c = |format| count(Mode::Stopwatch, 0, format, "");
        assert_eq!(at(&c(Format::Auto), 4_500_000), "1:15:00");
        assert_eq!(at(&c(Format::MinutesSeconds), 4_500_000), "75:00");
        assert_eq!(at(&c(Format::HoursMinutesSeconds), 4_500_000), "01:15:00");
    }

    #[test]
    fn a_clock_writes_the_time_of_day_in_its_zone() {
        // 2026-11-03 14:05:09 UTC.
        let wall = 1_793_714_709_000;
        let mut c = count(Mode::Clock, 0, Format::Auto, "");
        assert_eq!(at_wall(&c, wall), "14:05:09");
        c.clock = ClockFormat::H12;
        assert_eq!(at_wall(&c, wall), "2:05:09 PM");
        c.clock = ClockFormat::H12Short;
        assert_eq!(at_wall(&c, wall), "2:05 PM");
        c.clock = ClockFormat::H24Short;
        c.zone = TimeZone::fixed(jiff::tz::offset(-5));
        assert_eq!(at_wall(&c, wall), "09:05");
        assert_eq!(clock_text(0, 7, 0, ClockFormat::H12), "12:07:00 AM");
        assert_eq!(clock_text(12, 7, 0, ClockFormat::H12), "12:07:00 PM");
    }

    fn at_wall(c: &Count, wall: i64) -> String {
        c.text(c.key(0, wall))
    }

    #[test]
    fn a_clock_changes_on_the_wall_clocks_second_or_minute() {
        let mut c = count(Mode::Clock, 0, Format::Auto, "");
        assert_eq!(c.mode_timing().next(10_000, 5_250), Some(10_750));
        c.clock = ClockFormat::H24Short;
        assert_eq!(c.mode_timing().next(10_000, 59_000), Some(11_000));
    }

    #[test]
    fn the_candidates_cover_every_shape_and_the_end_text() {
        let c = count(Mode::Countdown, 3600, Format::Auto, "LIVE");
        let candidates = c.candidates();
        assert!(candidates.contains(&"8:88:88".to_owned()));
        assert!(candidates.contains(&"88:88".to_owned()));
        assert!(candidates.contains(&"LIVE".to_owned()));
        let mut clock = count(Mode::Clock, 0, Format::Auto, "");
        clock.clock = ClockFormat::H12Short;
        let candidates = clock.candidates();
        assert!(
            candidates.contains(&"88:88 PM".to_owned()),
            "{candidates:?}"
        );
        assert!(candidates.contains(&"8:88 AM".to_owned()), "{candidates:?}");
    }
}
