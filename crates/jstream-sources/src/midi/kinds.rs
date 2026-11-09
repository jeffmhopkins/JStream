//! The MIDI visualization kinds (ADR-0137): `midi_keyboard`, `midi_piano_roll`,
//! `midi_pads`, `midi_particles`, `midi_cc` and `midi_beat`. Each draws one input's state ([`MidiState`]) on the CPU at
//! 30 fps, and only while something changes.

use super::{
    CHANNELS, Canvas, Draw, KEYS, MidiState, NoteEvent, bad, by_velocity, channel, channels, color,
    create, int,
};
use crate::describe::{kind, param, size};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;

/// A parameter whose change applies without a new source.
fn live(p: ParamDescriptor) -> ParamDescriptor {
    ParamDescriptor { live: true, ..p }
}

fn described(p: ParamDescriptor, text: &str) -> ParamDescriptor {
    ParamDescriptor {
        description: Some(text.into()),
        ..p
    }
}

fn whole(min: i64, max: i64) -> ParamType {
    ParamType::Int {
        min: Some(min),
        max: Some(max),
        step: Some(1),
        unit: None,
    }
}

fn rgba() -> ParamType {
    ParamType::Color { alpha: true }
}

/// The parameters every MIDI kind starts with: which input and channel it draws, and
/// its size.
fn common(width: u32, height: u32, default_channel: u8) -> Vec<ParamDescriptor> {
    vec![
        described(
            param(
                "input",
                "MIDI input",
                ParamType::String {
                    multiline: false,
                    max_len: super::MAX_INPUT_NAME as u32,
                },
                json!(""),
            ),
            "The input it draws: a MIDI port's or JTunnel sender's name. Empty draws \
             every input at once.",
        ),
        live(described(
            param("channel", "Channel", whole(0, 16), json!(default_channel)),
            "The MIDI channel it draws, 1 to 16, as a device's panel shows it; 0 draws \
             every channel.",
        )),
        param("width", "Width", size(), json!(width)),
        param("height", "Height", size(), json!(height)),
    ]
}

/// A key range's two parameters.
fn key_range(low: u8, high: u8) -> [ParamDescriptor; 2] {
    [
        live(described(
            param("low_key", "Lowest key", whole(0, 127), json!(low)),
            "The lowest key drawn: 21 is a piano's lowest A, 60 middle C.",
        )),
        live(param("high_key", "Highest key", whole(0, 127), json!(high))),
    ]
}

/// The `low_key` and `high_key` settings, low first.
fn keys(settings: &Settings) -> Result<(u8, u8), SourceError> {
    let low = int(settings, "low_key", 0, 127)? as u8;
    let high = int(settings, "high_key", 0, 127)? as u8;
    if high < low {
        return Err(bad("high_key", "must not be below the lowest key"));
    }
    Ok((low, high))
}

/// Whether `key` is a black key on a piano.
fn black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// The highest velocity `key` is held at on the channels drawn.
fn held(state: &MidiState, channel: Option<u8>, key: u8) -> u8 {
    channels(channel)
        .map(|c| state.held(c, key))
        .max()
        .unwrap_or(0)
}

// ---- midi_keyboard -------------------------------------------------------------------

/// The `midi_keyboard` kind: a piano keyboard whose keys light while held.
pub(crate) struct KeyboardKind(KindDescriptor);

impl KeyboardKind {
    pub(crate) fn new() -> Self {
        let mut params = common(1280, 160, 0);
        params.extend(key_range(21, 108));
        params.extend([
            live(described(
                param("color", "Held key", rgba(), json!("#ff5a36ff")),
                "A held key's colour, brighter the harder it was struck.",
            )),
            live(param("white_key", "White key", rgba(), json!("#f2f2f2ff"))),
            live(param("black_key", "Black key", rgba(), json!("#1e1e1eff"))),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_keyboard", "MIDI keyboard", true, false)
        })
    }
}

impl SourceKind for KeyboardKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        create(
            &self.0,
            settings,
            ctx,
            Keyboard(KeyboardStyle::read(settings)?),
        )
    }
}

#[derive(Clone, Copy)]
struct KeyboardStyle {
    channel: Option<u8>,
    low: u8,
    high: u8,
    color: [u8; 4],
    white: [u8; 4],
    black: [u8; 4],
    background: [u8; 4],
}

impl KeyboardStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let (low, high) = keys(settings)?;
        Ok(Self {
            channel: channel(settings)?,
            low,
            high,
            color: color(settings, "color")?,
            white: color(settings, "white_key")?,
            black: color(settings, "black_key")?,
            background: color(settings, "background")?,
        })
    }
}

struct Keyboard(KeyboardStyle);

impl Draw for Keyboard {
    type Style = KeyboardStyle;

    fn style(settings: &Settings) -> Result<KeyboardStyle, SourceError> {
        KeyboardStyle::read(settings)
    }

    fn restyle(&mut self, style: KeyboardStyle) {
        self.0 = style;
    }

    fn draw(&mut self, state: &MidiState, _: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = self.0;
        canvas.clear(style.background);
        let (w, h) = (i64::from(canvas.width()), i64::from(canvas.height()));
        let range = style.low..=style.high;
        let whites = range.clone().filter(|&k| !black(k)).count().max(1) as i64;
        // White key `n`'s left edge.
        let edge = |n: i64| n * w / whites;
        let gap = i64::from(w / whites > 6);
        let mut n = 0;
        for key in range.clone() {
            if black(key) {
                continue;
            }
            let velocity = held(state, style.channel, key);
            let fill = if velocity > 0 {
                by_velocity(style.color, velocity)
            } else {
                style.white
            };
            canvas.fill(edge(n), 0, edge(n + 1) - gap, h, fill);
            n += 1;
        }
        // Black keys sit over the line between two white keys, 60% as wide and tall.
        let mut n = 0;
        let width = (w / whites * 3 / 5).max(1);
        for key in range {
            if !black(key) {
                n += 1;
                continue;
            }
            let velocity = held(state, style.channel, key);
            let fill = if velocity > 0 {
                by_velocity(style.color, velocity)
            } else {
                style.black
            };
            let x = edge(n) - width / 2;
            canvas.fill(x, 0, x + width, h * 3 / 5, fill);
        }
        false
    }
}

// ---- midi_pads -----------------------------------------------------------------------

/// The longest a pad stays lit after its note.
const MAX_DECAY_MS: u64 = 5000;

/// The `midi_pads` kind: a grid of drum pads, each lit by its note.
pub(crate) struct PadsKind(KindDescriptor);

impl PadsKind {
    pub(crate) fn new() -> Self {
        let mut params = common(480, 240, 10);
        params.extend([
            live(param("rows", "Rows", whole(1, 8), json!(2))),
            live(param("columns", "Columns", whole(1, 8), json!(4))),
            live(described(
                param("first_key", "First key", whole(0, 127), json!(36)),
                "The bottom-left pad's key; each pad to the right, then each row up, is the \
                 next key. 36 is General MIDI's bass drum.",
            )),
            live(described(
                param(
                    "decay_ms",
                    "Decay",
                    whole(0, MAX_DECAY_MS as i64),
                    json!(250),
                ),
                "How long a pad fades after its note starts, in milliseconds, so a short \
                 drum hit still shows. A held note stays lit.",
            )),
            live(param("color", "Hit pad", rgba(), json!("#36c3ffff"))),
            live(param("pad", "Pad", rgba(), json!("#2a2a2aff"))),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_pads", "MIDI drum pads", true, false)
        })
    }
}

impl SourceKind for PadsKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let pads = Pads {
            style: PadsStyle::read(settings)?,
            events: Vec::with_capacity(super::LOG),
            hits: [None; KEYS],
        };
        create(&self.0, settings, ctx, pads)
    }
}

#[derive(Clone, Copy)]
struct PadsStyle {
    channel: Option<u8>,
    rows: u8,
    columns: u8,
    first: u8,
    decay_ms: u64,
    color: [u8; 4],
    pad: [u8; 4],
    background: [u8; 4],
}

impl PadsStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        Ok(Self {
            channel: channel(settings)?,
            rows: int(settings, "rows", 1, 8)? as u8,
            columns: int(settings, "columns", 1, 8)? as u8,
            first: int(settings, "first_key", 0, 127)? as u8,
            decay_ms: int(settings, "decay_ms", 0, MAX_DECAY_MS)?,
            color: color(settings, "color")?,
            pad: color(settings, "pad")?,
            background: color(settings, "background")?,
        })
    }
}

struct Pads {
    style: PadsStyle,
    /// The log, read each frame; made on create.
    events: Vec<NoteEvent>,
    /// Each key's latest note-on on the channels drawn: when, and how hard.
    hits: [Option<(u64, u8)>; KEYS],
}

impl Draw for Pads {
    type Style = PadsStyle;

    fn style(settings: &Settings) -> Result<PadsStyle, SourceError> {
        PadsStyle::read(settings)
    }

    fn restyle(&mut self, style: PadsStyle) {
        // Another channel's hits are not this one's.
        if style.channel != self.style.channel {
            self.hits = [None; KEYS];
        }
        self.style = style;
    }

    fn draw(&mut self, state: &MidiState, now_ms: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = self.style;
        state.recent(&mut self.events, super::LOG);
        for event in &self.events {
            let drawn = style.channel.is_none_or(|c| c == event.channel);
            if drawn && event.velocity > 0 {
                let hit = &mut self.hits[usize::from(event.key)];
                if hit.is_none_or(|(at, _)| event.at_ms >= at) {
                    *hit = Some((event.at_ms, event.velocity));
                }
            }
        }
        canvas.clear(style.background);
        let (w, h) = (i64::from(canvas.width()), i64::from(canvas.height()));
        let (rows, columns) = (i64::from(style.rows), i64::from(style.columns));
        let gap = (w.min(h) / 40).max(1);
        let mut moving = false;
        for row in 0..rows {
            for column in 0..columns {
                let key = i64::from(style.first) + row * columns + column;
                let Ok(key) = u8::try_from(key) else { continue };
                if usize::from(key) >= KEYS {
                    continue;
                }
                let velocity = held(state, style.channel, key);
                // A held note shows its velocity; a released one fades over the decay.
                // A note can arrive after the tick it is drawn in starts: its age is 0.
                let fade = self.hits[usize::from(key)].and_then(|(at, v)| {
                    let age = now_ms.saturating_sub(at);
                    (age < style.decay_ms).then(|| {
                        let left = 1.0 - age as f32 / style.decay_ms as f32;
                        (f32::from(v) * left).round() as u8
                    })
                });
                moving |= fade.is_some_and(|v| v > 0);
                let level = velocity.max(fade.unwrap_or(0));
                let fill = if level > 0 {
                    by_velocity(style.color, level)
                } else {
                    style.pad
                };
                // Row 0 is the bottom one.
                let (x0, x1) = (column * w / columns, (column + 1) * w / columns);
                let (y1, y0) = (h - row * h / rows, h - (row + 1) * h / rows);
                canvas.fill(x0 + gap, y0 + gap, x1 - gap, y1 - gap, fill);
            }
        }
        moving
    }
}

// ---- midi_beat -----------------------------------------------------------------------

/// The most beats a bar a `midi_beat` source counts.
const MAX_BEATS_PER_BAR: u64 = 16;

/// The `midi_beat` kind: a row of a bar's beats, the one playing lit, from an input's
/// MIDI clock (ADR-0142 §4).
pub(crate) struct BeatKind(KindDescriptor);

impl BeatKind {
    pub(crate) fn new() -> Self {
        let mut params = common(480, 80, 0);
        // A clock is for the whole input, not one channel.
        params.retain(|p| p.id != "channel");
        params.extend([
            live(described(
                param(
                    "beats_per_bar",
                    "Beats per bar",
                    whole(1, MAX_BEATS_PER_BAR as i64),
                    json!(4),
                ),
                "How many beats a bar has: MIDI clock counts beats but carries no meter.",
            )),
            live(param("color", "Beat", rgba(), json!("#36c3ffff"))),
            live(described(
                param("downbeat", "First beat", rgba(), json!("#ffb000ff")),
                "The colour of each bar's first beat.",
            )),
            live(param("off", "Other beats", rgba(), json!("#2a2a2aff"))),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_beat", "MIDI beat", true, false)
        })
    }
}

impl SourceKind for BeatKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        create(&self.0, settings, ctx, Beat(BeatStyle::read(settings)?))
    }
}

#[derive(Clone, Copy)]
struct BeatStyle {
    beats_per_bar: u8,
    color: [u8; 4],
    downbeat: [u8; 4],
    off: [u8; 4],
    background: [u8; 4],
}

impl BeatStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        Ok(Self {
            beats_per_bar: int(settings, "beats_per_bar", 1, MAX_BEATS_PER_BAR)? as u8,
            color: color(settings, "color")?,
            downbeat: color(settings, "downbeat")?,
            off: color(settings, "off")?,
            background: color(settings, "background")?,
        })
    }
}

struct Beat(BeatStyle);

impl Draw for Beat {
    type Style = BeatStyle;

    fn style(settings: &Settings) -> Result<BeatStyle, SourceError> {
        BeatStyle::read(settings)
    }

    fn restyle(&mut self, style: BeatStyle) {
        self.0 = style;
    }

    fn draw(&mut self, state: &MidiState, now_ms: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = self.0;
        canvas.clear(style.background);
        let per_bar = i64::from(style.beats_per_bar);
        // Playing while the clock runs, or stopped: the beat the song is on, and how far
        // through it. A sender may stop its clock with its transport, so a stopped song
        // keeps showing where it is; a playing one whose clock went quiet shows nothing.
        let at = state
            .tempo()
            .filter(|t| !t.playing || t.clock(now_ms))
            .map(|t| {
                let beat = t.beat_at(now_ms).max(0.0);
                let in_bar = beat % f64::from(style.beats_per_bar);
                (in_bar.floor() as i64, in_bar.fract(), t.playing)
            });
        let (w, h) = (i64::from(canvas.width()), i64::from(canvas.height()));
        let gap = (w.min(h) / 20).max(1);
        for beat in 0..per_bar {
            let lit = match at {
                Some((on, _, _)) if on != beat => None,
                // Playing, the beat flashes at full and dims to 35% as it passes;
                // stopped, it shows where the song is at 35%.
                Some((_, through, true)) => Some(127 - (through * 127.0).round() as u8),
                Some((_, _, false)) => Some(0),
                None => None,
            };
            let base = if beat == 0 {
                style.downbeat
            } else {
                style.color
            };
            let fill = lit.map_or(style.off, |v| by_velocity(base, v));
            let (x0, x1) = (beat * w / per_bar, (beat + 1) * w / per_bar);
            canvas.fill(x0 + gap, gap, x1 - gap, h - gap, fill);
        }
        // A playing song moves on, or its clock goes quiet; either way the next tick may
        // differ. A stopped one waits for its next message.
        at.is_some_and(|(_, _, playing)| playing)
    }
}

// ---- midi_cc -------------------------------------------------------------------------

/// The most controllers a `midi_cc` source draws.
const MAX_CONTROLLERS: usize = 16;

/// The `midi_cc` kind: a bar for each chosen controller, and pitch bend if asked.
pub(crate) struct CcKind(KindDescriptor);

impl CcKind {
    pub(crate) fn new() -> Self {
        let mut params = common(320, 160, 0);
        params.extend([
            live(described(
                param(
                    "controllers",
                    "Controllers",
                    ParamType::String {
                        multiline: false,
                        max_len: 128,
                    },
                    json!("1, 7, 11, 74"),
                ),
                "The controller numbers drawn, left to right, separated by commas: 1 is the \
                 mod wheel, 7 volume, 11 expression, 74 brightness. At most 16.",
            )),
            live(described(
                param("pitch_bend", "Pitch bend", ParamType::Bool, json!(false)),
                "Adds a bar for pitch bend after the controllers, drawn from the middle.",
            )),
            live(param("color", "Bar", rgba(), json!("#7ee081ff"))),
            live(param("track", "Track", rgba(), json!("#2a2a2aff"))),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_cc", "MIDI controller meters", true, false)
        })
    }
}

impl SourceKind for CcKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        create(&self.0, settings, ctx, Cc(CcStyle::read(settings)?))
    }
}

#[derive(Clone)]
struct CcStyle {
    channel: Option<u8>,
    controllers: Vec<u8>,
    bend: bool,
    color: [u8; 4],
    track: [u8; 4],
    background: [u8; 4],
}

impl CcStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let list = setting(settings, "controllers")?
            .as_str()
            .ok_or_else(|| bad("controllers", "must be a string"))?;
        let controllers = controllers(list)?;
        Ok(Self {
            channel: channel(settings)?,
            controllers,
            bend: setting(settings, "pitch_bend")?
                .as_bool()
                .ok_or_else(|| bad("pitch_bend", "must be true or false"))?,
            color: color(settings, "color")?,
            track: color(settings, "track")?,
            background: color(settings, "background")?,
        })
    }
}

/// Parses a comma-separated list of controller numbers.
fn controllers(list: &str) -> Result<Vec<u8>, SourceError> {
    let reason = format!(
        "must be controller numbers from 0 to 127 separated by commas, at most \
         {MAX_CONTROLLERS}"
    );
    let numbers = list
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.parse::<u8>().ok().filter(|&n| usize::from(n) < KEYS))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("controllers", &reason))?;
    if numbers.len() > MAX_CONTROLLERS {
        return Err(bad("controllers", &reason));
    }
    Ok(numbers)
}

struct Cc(CcStyle);

impl Draw for Cc {
    type Style = CcStyle;

    fn style(settings: &Settings) -> Result<CcStyle, SourceError> {
        CcStyle::read(settings)
    }

    fn restyle(&mut self, style: CcStyle) {
        self.0 = style;
    }

    fn draw(&mut self, state: &MidiState, _: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = &self.0;
        canvas.clear(style.background);
        let bars = style.controllers.len() + usize::from(style.bend);
        if bars == 0 {
            return false;
        }
        let (w, h) = (i64::from(canvas.width()), i64::from(canvas.height()));
        let bars = bars as i64;
        let gap = (w / bars / 8).max(1);
        let bar = |n: i64| (n * w / bars + gap, (n + 1) * w / bars - gap);
        for (n, &controller) in style.controllers.iter().enumerate() {
            // On every channel at once, the highest value shows.
            let value = channels(style.channel)
                .map(|c| state.controller(c, controller))
                .max()
                .unwrap_or(0);
            let (x0, x1) = bar(n as i64);
            canvas.fill(x0, 0, x1, h, style.track);
            let top = h - i64::from(value) * h / 127;
            canvas.fill(x0, top, x1, h, style.color);
        }
        if style.bend {
            // The bend furthest from the middle, on the channels drawn.
            let bend = channels(style.channel)
                .map(|c| state.bend(c))
                .max_by_key(|b| b.unsigned_abs())
                .unwrap_or(0);
            let (x0, x1) = bar(bars - 1);
            canvas.fill(x0, 0, x1, h, style.track);
            let middle = h / 2;
            let to = middle - i64::from(bend) * middle / 8192;
            canvas.fill(
                x0,
                to.min(middle),
                x1,
                to.max(middle).max(middle + 1),
                style.color,
            );
        }
        false
    }
}

// ---- midi_piano_roll -----------------------------------------------------------------

/// The longest a piano roll shows, in seconds.
const MAX_SECONDS: f64 = 30.0;

/// The `midi_piano_roll` kind: notes scrolling left from a playhead at the right edge.
pub(crate) struct PianoRollKind(KindDescriptor);

impl PianoRollKind {
    pub(crate) fn new() -> Self {
        let mut params = common(1280, 360, 0);
        params.extend(key_range(36, 96));
        params.extend([
            live(described(
                param(
                    "seconds",
                    "Seconds shown",
                    ParamType::Float {
                        min: Some(0.5),
                        max: Some(MAX_SECONDS),
                        step: Some(0.5),
                        unit: Some("s".into()),
                    },
                    json!(4.0),
                ),
                "How much time the roll shows: a note scrolls from the right edge, where it \
                 sounds, to the left in this long. It keeps the last 1024 note events, so a \
                 very busy performance shows its newest notes.",
            )),
            live(described(
                param("color", "Note", rgba(), json!("#ffb000ff")),
                "A note's colour, brighter the harder it was struck.",
            )),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_piano_roll", "MIDI piano roll", true, false)
        })
    }
}

impl SourceKind for PianoRollKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let roll = PianoRoll {
            style: RollStyle::read(settings)?,
            events: Vec::with_capacity(super::LOG),
            open: vec![None; CHANNELS * KEYS],
        };
        create(&self.0, settings, ctx, roll)
    }
}

#[derive(Clone, Copy)]
struct RollStyle {
    channel: Option<u8>,
    low: u8,
    high: u8,
    span_ms: u64,
    color: [u8; 4],
    background: [u8; 4],
}

impl RollStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let (low, high) = keys(settings)?;
        let seconds = setting(settings, "seconds")?
            .as_f64()
            .filter(|s| (0.5..=MAX_SECONDS).contains(s))
            .ok_or_else(|| bad("seconds", "must be a number from 0.5 to 30"))?;
        Ok(Self {
            channel: channel(settings)?,
            low,
            high,
            span_ms: (seconds * 1000.0).round() as u64,
            color: color(settings, "color")?,
            background: color(settings, "background")?,
        })
    }
}

struct PianoRoll {
    style: RollStyle,
    events: Vec<NoteEvent>,
    /// Each channel's and key's note sounding as the log is walked: since when, and how
    /// hard. Made on create.
    open: Vec<Option<(u64, u8)>>,
}

impl Draw for PianoRoll {
    type Style = RollStyle;

    fn style(settings: &Settings) -> Result<RollStyle, SourceError> {
        RollStyle::read(settings)
    }

    fn restyle(&mut self, style: RollStyle) {
        self.style = style;
    }

    fn draw(&mut self, state: &MidiState, now_ms: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = self.style;
        canvas.clear(style.background);
        state.recent(&mut self.events, super::LOG);
        let (w, h) = (i64::from(canvas.width()), i64::from(canvas.height()));
        let start = now_ms.saturating_sub(style.span_ms);
        let lanes = i64::from(style.high - style.low) + 1;
        let x = |ms: u64| {
            w - (now_ms.saturating_sub(ms).min(style.span_ms) as i64 * w
                / style.span_ms.max(1) as i64)
        };
        let mut moving = false;
        let mut bar = |canvas: &mut Canvas<'_>, key: u8, from: u64, to: u64, velocity: u8| {
            if to < start || !(style.low..=style.high).contains(&key) {
                return;
            }
            // The highest key is the top lane.
            let lane = i64::from(style.high - key);
            let (y0, y1) = (lane * h / lanes, (lane + 1) * h / lanes);
            let (x0, x1) = (x(from.max(start)), x(to));
            canvas.fill(
                x0,
                y0,
                x1.max(x0 + 1),
                y1.max(y0 + 1),
                by_velocity(style.color, velocity),
            );
            moving = true;
        };
        self.open.fill(None);
        for event in &self.events {
            if style.channel.is_some_and(|c| c != event.channel) {
                continue;
            }
            let slot = &mut self.open[super::index(event.channel, event.key)];
            if let Some((from, velocity)) = slot.take() {
                // A note-off, or a note-on retriggering it: the note so far ends here.
                bar(canvas, event.key, from, event.at_ms, velocity);
            }
            if event.velocity > 0 {
                *slot = Some((event.at_ms, event.velocity));
            }
        }
        // What still sounds: from its note-on, or from the left edge when the log has
        // lost it.
        for channel in channels(style.channel) {
            for key in style.low..=style.high {
                let open = self.open[super::index(channel, key)];
                let held = state.held(channel, key);
                match open {
                    Some((from, velocity)) => bar(canvas, key, from, now_ms, velocity),
                    None if held > 0 => bar(canvas, key, start, now_ms, held),
                    None => {}
                }
            }
        }
        moving
    }
}

// ---- midi_particles ------------------------------------------------------------------

/// The most particles one note bursts into.
const MAX_PARTICLES: u64 = 64;

/// The time constant of a burst's slowing, in milliseconds: a particle has covered 63%
/// of its way after this long and 99% after five times it.
const BURST_MS: f32 = 250.0;

/// The golden angle, in radians: successive particles of a burst spread evenly round it.
const GOLDEN_ANGLE: f32 = 2.399_963;

/// The `midi_particles` kind: each note-on bursts into particles, placed across the
/// width by pitch and sized by velocity, which fade once the note is released.
pub(crate) struct ParticlesKind(KindDescriptor);

impl ParticlesKind {
    pub(crate) fn new() -> Self {
        let mut params = common(960, 360, 0);
        params.extend(key_range(21, 108));
        params.extend([
            live(described(
                param(
                    "particles",
                    "Particles a note",
                    whole(1, MAX_PARTICLES as i64),
                    json!(16),
                ),
                "How many particles each note-on bursts into.",
            )),
            live(described(
                param("spread", "Spread", whole(4, 2000), json!(120)),
                "How far a full-velocity note's particles fly from where it starts, in \
                 pixels; a softer note's go less far.",
            )),
            live(described(
                param("size", "Particle size", whole(1, 64), json!(6)),
                "A full-velocity note's particle size in pixels; a softer note's are \
                 smaller.",
            )),
            live(described(
                param(
                    "decay_ms",
                    "Decay",
                    whole(0, MAX_DECAY_MS as i64),
                    json!(800),
                ),
                "How long a note's particles fade after its note-off, in milliseconds. \
                 They stay while the note is held.",
            )),
            live(described(
                param("color", "Particle", rgba(), json!("#ff4fd8ff")),
                "A particle's colour, brighter the harder its note was struck.",
            )),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ]);
        Self(KindDescriptor {
            params,
            ..kind("midi_particles", "MIDI note particles", true, false)
        })
    }
}

impl SourceKind for ParticlesKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let particles = Particles {
            style: ParticlesStyle::read(settings)?,
            events: Vec::with_capacity(super::LOG),
            open: vec![None; CHANNELS * KEYS],
        };
        create(&self.0, settings, ctx, particles)
    }
}

#[derive(Clone, Copy)]
struct ParticlesStyle {
    channel: Option<u8>,
    low: u8,
    high: u8,
    count: u32,
    spread: f32,
    size: f32,
    decay_ms: u64,
    color: [u8; 4],
    background: [u8; 4],
}

impl ParticlesStyle {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let (low, high) = keys(settings)?;
        Ok(Self {
            channel: channel(settings)?,
            low,
            high,
            count: int(settings, "particles", 1, MAX_PARTICLES)? as u32,
            spread: int(settings, "spread", 4, 2000)? as f32,
            size: int(settings, "size", 1, 64)? as f32,
            decay_ms: int(settings, "decay_ms", 0, MAX_DECAY_MS)?,
            color: color(settings, "color")?,
            background: color(settings, "background")?,
        })
    }
}

/// One note's burst, as the log is walked: when it started and ended, and how hard.
#[derive(Clone, Copy)]
struct Burst {
    key: u8,
    on_ms: u64,
    /// Its note-off, or `None` while it is held.
    off_ms: Option<u64>,
    velocity: u8,
}

struct Particles {
    style: ParticlesStyle,
    events: Vec<NoteEvent>,
    /// Each channel's and key's note sounding as the log is walked: since when, and how
    /// hard. Made on create.
    open: Vec<Option<(u64, u8)>>,
}

/// A number from 0 to 1 that only `seed` picks, so a burst is the same every frame.
fn noise(seed: u64) -> f32 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// Draws `burst` at `now_ms` in `style`, and says whether it is still flying out or
/// fading, so the next tick draws it otherwise.
fn draw_burst(style: ParticlesStyle, burst: Burst, now_ms: u64, canvas: &mut Canvas<'_>) -> bool {
    let fade = match burst.off_ms {
        None => 1.0,
        Some(_) if style.decay_ms == 0 => return false,
        Some(off) => {
            let since = now_ms.saturating_sub(off);
            if since >= style.decay_ms {
                return false;
            }
            1.0 - since as f32 / style.decay_ms as f32
        }
    };
    let age = now_ms.saturating_sub(burst.on_ms) as f32;
    let out = 1.0 - (-age / BURST_MS).exp();
    let strength = 0.35 + 0.65 * f32::from(burst.velocity) / 127.0;
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    // The lowest key at the left edge's lane, the highest at the right's, a third
    // of the way up.
    let lanes = f32::from(style.high - style.low) + 1.0;
    let x0 = (f32::from(burst.key - style.low) + 0.5) * w / lanes;
    let y0 = h * 2.0 / 3.0;
    let side = (style.size * strength).max(1.0);
    let mut color = by_velocity(style.color, burst.velocity);
    color[3] = (f32::from(color[3]) * fade).round() as u8;
    let seed = burst.on_ms << 7 | u64::from(burst.key);
    for i in 0..style.count {
        let jitter = noise(seed.wrapping_mul(u64::from(style.count)) + u64::from(i));
        let angle = i as f32 * GOLDEN_ANGLE + noise(seed) * std::f32::consts::TAU;
        let reach = style.spread * strength * (0.4 + 0.6 * jitter) * out;
        let (x, y) = (x0 + angle.cos() * reach, y0 - angle.sin() * reach);
        let (left, top) = ((x - side / 2.0).round(), (y - side / 2.0).round());
        canvas.fill(
            left as i64,
            top as i64,
            (left + side) as i64,
            (top + side) as i64,
            color,
        );
    }
    // Still flying out until it has covered nearly all its way.
    burst.off_ms.is_some() || age < BURST_MS * 5.0
}

impl Draw for Particles {
    type Style = ParticlesStyle;

    fn style(settings: &Settings) -> Result<ParticlesStyle, SourceError> {
        ParticlesStyle::read(settings)
    }

    fn restyle(&mut self, style: ParticlesStyle) {
        self.style = style;
    }

    fn draw(&mut self, state: &MidiState, now_ms: u64, canvas: &mut Canvas<'_>) -> bool {
        let style = self.style;
        canvas.clear(style.background);
        state.recent(&mut self.events, super::LOG);
        self.open.fill(None);
        let mut moving = false;
        let events = std::mem::take(&mut self.events);
        for event in &events {
            if style.channel.is_some_and(|c| c != event.channel)
                || !(style.low..=style.high).contains(&event.key)
            {
                continue;
            }
            let slot = &mut self.open[super::index(event.channel, event.key)];
            if let Some((on_ms, velocity)) = slot.take() {
                // A note-off, or a note-on retriggering it: its burst starts fading.
                let burst = Burst {
                    key: event.key,
                    on_ms,
                    off_ms: Some(event.at_ms),
                    velocity,
                };
                moving |= draw_burst(style, burst, now_ms, canvas);
            }
            if event.velocity > 0 {
                *slot = Some((event.at_ms, event.velocity));
            }
        }
        self.events = events;
        // The notes still held, drawn last so they are on top.
        for channel in channels(style.channel) {
            for key in style.low..=style.high {
                let Some((on_ms, velocity)) = self.open[super::index(channel, key)] else {
                    continue;
                };
                // A note whose note-off the state has but the log lost isn't held.
                if state.held(channel, key) == 0 {
                    continue;
                }
                let burst = Burst {
                    key,
                    on_ms,
                    off_ms: None,
                    velocity,
                };
                moving |= draw_burst(style, burst, now_ms, canvas);
            }
        }
        moving
    }
}
