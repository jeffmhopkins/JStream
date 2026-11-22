//! The `grid` kind (ADR-0171 decision 9): cells drawn from live data, as a picture an
//! item shows flat or a model takes as a texture.
//!
//! **Cells** (`mode = "cells"`). Each column is one key's message (`columns_key`, with
//! `{column}` replaced by the column's number from `column_offset`), and each row one
//! entry of a list in it (`cells_field`, from `row_offset`). A cell reads its state from
//! `state_field`, a comma-separated list of fields where the first whose text has a look
//! decides (so `queued,state` shows a queued clip's blink over its state), and its own
//! colour from `color_field`. `looks` maps a state's text to a colour (or the cell's
//! own), a brightness and a blink rate; a cell the data doesn't have takes the `absent`
//! look, and a state with no look shows the cell's colour (or `pad`) at `dim`.
//!
//! **Bars** (`mode = "bars"`). One message (`key`) holds a list (`field`); each entry is
//! one column's bar: a number, or a list of numbers drawn side by side (a stereo
//! meter). A value at `low` draws nothing and at `high` the full height, from
//! `color_low` at the foot to `color_high` at the top, in `segments` steps when it is
//! not 0.
//!
//! Nothing here knows what the data depicts: a DAW's clip launcher and its meters are
//! one way to fill it. The painter reads the messages from [`LiveData`] on the engine's
//! paint thread at 30 fps, and draws only when the data changed or a look blinks, on the
//! CPU as the MIDI kinds do. The render thread only takes the painted frame.

use crate::color::parse_color;
use crate::data::{LiveData, field};
use crate::describe::{kind, param, size};
use crate::midi::{Canvas, Frames, frame_size, next_tick, tick, tick_start};
use crate::source::{changed_non_live, millis, setting};
use crate::{Context, Painter, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    EnumOption, KindDescriptor, LOOK_ABSENT, LOOK_CELL_COLOR, MAX_LOOKS, ParamDescriptor,
    ParamType, Settings, VisibleIf,
};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The most columns and rows a grid draws.
pub const MAX_CELLS_SIDE: u32 = 64;
/// The furthest a grid's first column or row may be from the data's first.
pub const MAX_OFFSET: u32 = 4096;
/// The longest key template, in bytes: a key (63) with room for `{column}`.
const MAX_KEY_TEMPLATE: u32 = 72;
/// The longest field path a setting names, in bytes.
const MAX_FIELD: u32 = 255;
/// The most bars side by side in one column (a list entry's numbers).
const MAX_SUB_BARS: usize = 8;
/// The most segments a bar is cut into.
const MAX_SEGMENTS: i64 = 64;

/// The `grid` kind.
pub(crate) struct GridKind(KindDescriptor);

fn live(p: ParamDescriptor) -> ParamDescriptor {
    ParamDescriptor { live: true, ..p }
}

fn described(p: ParamDescriptor, text: &str) -> ParamDescriptor {
    ParamDescriptor {
        description: Some(text.into()),
        ..p
    }
}

fn when(p: ParamDescriptor, mode: &str) -> ParamDescriptor {
    ParamDescriptor {
        visible_if: Some(VisibleIf {
            param: "mode".into(),
            equals: json!(mode),
        }),
        ..p
    }
}

fn whole(min: i64, max: i64, unit: Option<&str>) -> ParamType {
    ParamType::Int {
        min: Some(min),
        max: Some(max),
        step: Some(1),
        unit: unit.map(Into::into),
    }
}

fn fraction() -> ParamType {
    ParamType::Float {
        min: Some(0.0),
        max: Some(1.0),
        step: Some(0.01),
        unit: None,
    }
}

fn text(max_len: u32) -> ParamType {
    ParamType::String {
        multiline: false,
        max_len,
    }
}

fn rgba() -> ParamType {
    ParamType::Color { alpha: true }
}

impl GridKind {
    pub(crate) fn new() -> Self {
        let params = vec![
            live(described(
                param("input", "Sender", text(63), json!("")),
                "The JTunnel sender whose data it draws, by name. Empty draws whichever \
                 sender has the keys.",
            )),
            live(described(
                param(
                    "mode",
                    "Draws",
                    ParamType::Enum {
                        options: vec![
                            EnumOption {
                                value: "cells".into(),
                                label: "Cells (a column per key, a row per list entry)".into(),
                            },
                            EnumOption {
                                value: "bars".into(),
                                label: "Bars (one list of levels)".into(),
                            },
                        ],
                    },
                    json!("cells"),
                ),
                "Cells draw a pad per list entry, coloured by its state, such as a clip \
                 launcher. Bars draw a level per list entry, such as meters.",
            )),
            param("width", "Width", size(), json!(960)),
            param("height", "Height", size(), json!(360)),
            live(param(
                "columns",
                "Columns",
                whole(1, i64::from(MAX_CELLS_SIDE), None),
                json!(8),
            )),
            live(when(
                param(
                    "rows",
                    "Rows",
                    whole(1, i64::from(MAX_CELLS_SIDE), None),
                    json!(8),
                ),
                "cells",
            )),
            live(described(
                param(
                    "column_offset",
                    "First column",
                    whole(0, i64::from(MAX_OFFSET), None),
                    json!(0),
                ),
                "The data's column drawn at the left, from 0.",
            )),
            live(when(
                described(
                    param(
                        "row_offset",
                        "First row",
                        whole(0, i64::from(MAX_OFFSET), None),
                        json!(0),
                    ),
                    "The list entry drawn at the top, from 0.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param(
                        "columns_key",
                        "Column key",
                        text(MAX_KEY_TEMPLATE),
                        json!("track/{column}"),
                    ),
                    "The key of each column's message; `{column}` is the column's number.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param(
                        "cells_field",
                        "Cells field",
                        text(MAX_FIELD),
                        json!("slots"),
                    ),
                    "The list in each column's message, one entry a row: a dotted path.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param(
                        "state_field",
                        "State field",
                        text(MAX_FIELD),
                        json!("state"),
                    ),
                    "The field in each entry whose text picks the cell's look. Several, \
                     separated by commas, are tried in order, and the first whose text \
                     has a look decides.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param(
                        "color_field",
                        "Colour field",
                        text(MAX_FIELD),
                        json!("color"),
                    ),
                    "The field in each entry holding the cell's own `#rrggbb` colour.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param(
                        "looks",
                        "Looks",
                        ParamType::Looks,
                        json!({ LOOK_ABSENT: {"color": "#1a1a1a", "brightness": 1.0} }),
                    ),
                    "How a cell in each state looks: a colour or the cell's own, a \
                     brightness, and a blink rate. `absent` is a cell the data doesn't \
                     have.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param("dim", "Unlisted brightness", fraction(), json!(0.2)),
                    "The brightness of a cell whose state has no look.",
                ),
                "cells",
            )),
            live(when(
                described(
                    param("key", "Key", text(63), json!("meters")),
                    "The key of the message holding the levels.",
                ),
                "bars",
            )),
            live(when(
                described(
                    param("field", "Levels field", text(MAX_FIELD), json!("tracks")),
                    "The list of levels in it, one entry a column: a number, or a list \
                     of numbers drawn side by side.",
                ),
                "bars",
            )),
            live(when(
                param(
                    "low",
                    "Lowest level",
                    ParamType::Float {
                        min: Some(-1e6),
                        max: Some(1e6),
                        step: None,
                        unit: None,
                    },
                    json!(0.0),
                ),
                "bars",
            )),
            live(when(
                param(
                    "high",
                    "Highest level",
                    ParamType::Float {
                        min: Some(-1e6),
                        max: Some(1e6),
                        step: None,
                        unit: None,
                    },
                    json!(1.0),
                ),
                "bars",
            )),
            live(when(
                param("color_low", "Foot colour", rgba(), json!("#2fd46bff")),
                "bars",
            )),
            live(when(
                param("color_high", "Top colour", rgba(), json!("#ff4a3dff")),
                "bars",
            )),
            live(when(
                described(
                    param(
                        "segments",
                        "Segments",
                        whole(0, MAX_SEGMENTS, None),
                        json!(0),
                    ),
                    "Cuts each bar into this many lit steps, as an LED meter; 0 draws it \
                     smooth.",
                ),
                "bars",
            )),
            live(described(
                param("pad", "Pad", rgba(), json!("#2a2a2aff")),
                "A cell with no colour of its own, and a bar's unlit part.",
            )),
            live(param("gap", "Gap", whole(0, 64, Some("px")), json!(4))),
            live(param(
                "radius",
                "Corner radius",
                whole(0, 128, Some("px")),
                json!(4),
            )),
            live(param(
                "background",
                "Background",
                rgba(),
                json!("#00000000"),
            )),
        ];
        Self(KindDescriptor {
            params,
            ..kind("grid", "Data grid", true, false)
        })
    }
}

impl SourceKind for GridKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let size = frame_size(settings, &ctx.limits)?;
        let style = Style::read(settings)?;
        let mut drawing = Drawing {
            style,
            frames: Frames::new(size)?,
            seen: None,
            drawn: None,
            restyled: true,
            key: String::with_capacity(MAX_KEY_TEMPLATE as usize + 8),
        };
        drawing.paint(ctx.data.as_ref(), MediaTime::zero(Timebase::MILLIS));
        let ready = Ready {
            frame: drawing.frames.current(),
            restyle: None,
        };
        Ok(Box::new(GridSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            painter: Arc::new(GridPainter {
                data: ctx.data.clone(),
                drawing: Mutex::new(drawing),
                ready: Mutex::new(ready),
            }),
        }))
    }
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

fn string<'s>(settings: &'s Settings, id: &str) -> Result<&'s str, SourceError> {
    setting(settings, id)?
        .as_str()
        .ok_or_else(|| bad(id, "must be a string"))
}

fn int(settings: &Settings, id: &str, min: u64, max: u64) -> Result<u32, SourceError> {
    setting(settings, id)?
        .as_u64()
        .filter(|v| (min..=max).contains(v))
        .map(|v| v as u32)
        .ok_or_else(|| bad(id, &format!("must be a whole number from {min} to {max}")))
}

fn number(settings: &Settings, id: &str, min: f64, max: f64) -> Result<f64, SourceError> {
    setting(settings, id)?
        .as_f64()
        .filter(|v| (min..=max).contains(v))
        .ok_or_else(|| bad(id, &format!("must be a number from {min} to {max}")))
}

fn color(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a color as `#rrggbb` or `#rrggbbaa`"))
}

/// Where a look takes its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hue {
    Fixed([u8; 4]),
    /// The cell's own colour, or `pad` without one.
    Cell,
}

/// How a cell in one state looks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    hue: Hue,
    brightness: f32,
    blink_hz: f32,
}

/// The live settings.
#[derive(Clone, Debug)]
struct Style {
    input: Option<String>,
    bars: bool,
    columns: u32,
    rows: u32,
    column_offset: u32,
    row_offset: u32,
    columns_key: String,
    cells_field: String,
    state_fields: Vec<String>,
    color_field: String,
    /// The looks by state text, `absent` among them if it is set.
    looks: Vec<(String, Look)>,
    dim: f32,
    key: String,
    field: String,
    low: f64,
    high: f64,
    color_low: [u8; 4],
    color_high: [u8; 4],
    segments: u32,
    pad: [u8; 4],
    gap: f32,
    radius: f32,
    background: [u8; 4],
}

impl Style {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let input = string(settings, "input")?;
        let looks = looks(setting(settings, "looks")?)?;
        let low = number(settings, "low", -1e6, 1e6)?;
        let high = number(settings, "high", -1e6, 1e6)?;
        if (high - low).abs() < f64::EPSILON {
            return Err(bad("high", "must differ from the lowest level"));
        }
        Ok(Self {
            input: (!input.is_empty()).then(|| input.to_owned()),
            bars: string(settings, "mode")? == "bars",
            columns: int(settings, "columns", 1, u64::from(MAX_CELLS_SIDE))?,
            rows: int(settings, "rows", 1, u64::from(MAX_CELLS_SIDE))?,
            column_offset: int(settings, "column_offset", 0, u64::from(MAX_OFFSET))?,
            row_offset: int(settings, "row_offset", 0, u64::from(MAX_OFFSET))?,
            columns_key: string(settings, "columns_key")?.to_owned(),
            cells_field: string(settings, "cells_field")?.to_owned(),
            state_fields: string(settings, "state_field")?
                .split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect(),
            color_field: string(settings, "color_field")?.to_owned(),
            looks,
            dim: number(settings, "dim", 0.0, 1.0)? as f32,
            key: string(settings, "key")?.to_owned(),
            field: string(settings, "field")?.to_owned(),
            low,
            high,
            color_low: color(settings, "color_low")?,
            color_high: color(settings, "color_high")?,
            segments: int(settings, "segments", 0, MAX_SEGMENTS as u64)?,
            pad: color(settings, "pad")?,
            gap: int(settings, "gap", 0, 64)? as f32,
            radius: int(settings, "radius", 0, 128)? as f32,
            background: color(settings, "background")?,
        })
    }

    fn look(&self, state: &str) -> Option<&Look> {
        self.looks.iter().find(|(s, _)| s == state).map(|(_, l)| l)
    }

    /// Whether anything it may draw blinks.
    fn blinks(&self) -> bool {
        !self.bars && self.looks.iter().any(|(_, l)| l.blink_hz > 0.0)
    }
}

/// A [`ParamType::Looks`] value, which the engine has checked already.
fn looks(value: &Value) -> Result<Vec<(String, Look)>, SourceError> {
    let invalid = || bad("looks", "must be an object of state texts to looks");
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() > MAX_LOOKS {
        return Err(bad("looks", &format!("names more than {MAX_LOOKS} looks")));
    }
    object
        .iter()
        .map(|(state, look)| {
            let look = look.as_object().ok_or_else(invalid)?;
            let hue = match look.get("color").and_then(Value::as_str) {
                None | Some(LOOK_CELL_COLOR) => Hue::Cell,
                Some(c) => Hue::Fixed(parse_color(c).ok_or_else(invalid)?),
            };
            let get = |name: &str, default: f64| {
                look.get(name).and_then(Value::as_f64).unwrap_or(default) as f32
            };
            Ok((
                state.clone(),
                Look {
                    hue,
                    brightness: get("brightness", 1.0).clamp(0.0, 1.0),
                    blink_hz: get("blink_hz", 0.0).clamp(0.0, 16.0),
                },
            ))
        })
        .collect()
}

/// A running `grid` source; its painter draws each frame.
struct GridSource {
    descriptor: KindDescriptor,
    settings: Settings,
    painter: Arc<GridPainter>,
}

impl Source for GridSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(self.painter.frame())
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.painter.next_change(at)
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let style = Style::read(settings)?;
        lock(&self.painter.ready).restyle = Some(style);
        self.settings = settings.clone();
        Ok(())
    }

    fn painter(&self) -> Option<Arc<dyn Painter>> {
        let painter: Arc<dyn Painter> = self.painter.clone();
        Some(painter)
    }
}

struct GridPainter {
    /// The data it draws; without any, the first frame never changes.
    data: Option<LiveData>,
    drawing: Mutex<Drawing>,
    ready: Mutex<Ready>,
}

struct Ready {
    frame: Arc<VideoFrame>,
    restyle: Option<Style>,
}

impl Painter for GridPainter {
    fn paint(&self, at: MediaTime) {
        let restyle = lock(&self.ready).restyle.take();
        let mut drawing = lock(&self.drawing);
        if let Some(style) = restyle {
            drawing.style = style;
            drawing.restyled = true;
        }
        drawing.paint(self.data.as_ref(), at);
        let frame = drawing.frames.current();
        drop(drawing);
        lock(&self.ready).frame = frame;
    }

    fn frame(&self) -> Arc<VideoFrame> {
        Arc::clone(&lock(&self.ready).frame)
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        // Data lands whenever a sender sends it, so it is looked for every tick; a tick
        // with nothing new costs one atomic load.
        self.data.as_ref().map(|_| next_tick(at))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Drawing {
    style: Style,
    frames: Frames,
    /// The data's [`LiveData::changes`] when it was drawn last.
    seen: Option<u64>,
    /// The tick drawn last.
    drawn: Option<u64>,
    /// Whether the style changed since the last frame.
    restyled: bool,
    /// The column key being read, reused.
    key: String,
}

impl Drawing {
    fn paint(&mut self, data: Option<&LiveData>, at: MediaTime) {
        let now = tick(at);
        if self.drawn == Some(now) {
            return;
        }
        let changes = data.map(LiveData::changes);
        if self.drawn.is_some() && !self.restyled && self.seen == changes && !self.style.blinks() {
            return;
        }
        self.drawn = Some(now);
        self.seen = changes;
        self.restyled = false;
        let pts = tick_start(now);
        let now_ms = millis(pts);
        let (style, key) = (&self.style, &mut self.key);
        self.frames.draw(pts, |canvas| {
            canvas.clear(style.background);
            if style.bars {
                draw_bars(style, data, canvas);
            } else {
                draw_cells(style, data, key, now_ms, canvas);
            }
        });
    }
}

/// Where column `i` of `n` (or row) lies along a side of `len` pixels, with `gap` around
/// and between: its start and end.
fn span(i: u32, n: u32, len: f32, gap: f32) -> (f32, f32) {
    let each = ((len - gap * (n as f32 + 1.0)) / n as f32).max(1.0);
    let start = gap + i as f32 * (each + gap);
    (start, start + each)
}

fn draw_cells(
    style: &Style,
    data: Option<&LiveData>,
    key: &mut String,
    now_ms: u64,
    canvas: &mut Canvas<'_>,
) {
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    let absent = style.look(LOOK_ABSENT).copied();
    for c in 0..style.columns {
        key.clear();
        let column = c + style.column_offset;
        let mut parts = style.columns_key.split("{column}");
        if let Some(first) = parts.next() {
            key.push_str(first);
        }
        for part in parts {
            let _ = write!(key, "{column}");
            key.push_str(part);
        }
        let message = data.and_then(|d| d.get_from(style.input.as_deref(), key));
        let list = message
            .as_deref()
            .and_then(|m| field(m, &style.cells_field))
            .and_then(Value::as_array);
        let (x0, x1) = span(c, style.columns, w, style.gap);
        for r in 0..style.rows {
            let entry = list.and_then(|l| l.get((r + style.row_offset) as usize));
            let own = entry
                .and_then(|e| field(e, &style.color_field))
                .and_then(Value::as_str)
                .and_then(parse_color);
            let mut stated = false;
            let mut look = None;
            if let Some(entry) = entry {
                for f in &style.state_fields {
                    if let Some(state) = field(entry, f).and_then(Value::as_str) {
                        stated = true;
                        if let Some(l) = style.look(state) {
                            look = Some(*l);
                            break;
                        }
                    }
                }
            }
            let look = match look {
                Some(l) => Some(l),
                None if !stated => absent,
                None => None,
            };
            let base = |hue: Hue| match hue {
                Hue::Fixed(c) => c,
                Hue::Cell => own.unwrap_or(style.pad),
            };
            let fill = match look {
                Some(l) => {
                    let lit = l.blink_hz <= 0.0 || {
                        let period = 1000.0 / f64::from(l.blink_hz);
                        (now_ms as f64 % period) < period / 2.0
                    };
                    let b = if lit {
                        l.brightness
                    } else {
                        l.brightness * 0.25
                    };
                    scale(base(l.hue), b)
                }
                None => scale(own.unwrap_or(style.pad), style.dim),
            };
            let (y0, y1) = span(r, style.rows, h, style.gap);
            rounded(canvas, (x0, y0, x1, y1), style.radius, fill);
        }
    }
}

fn draw_bars(style: &Style, data: Option<&LiveData>, canvas: &mut Canvas<'_>) {
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    let message = data.and_then(|d| d.get_from(style.input.as_deref(), &style.key));
    let list = message
        .as_deref()
        .and_then(|m| field(m, &style.field))
        .and_then(Value::as_array);
    let level = |v: &Value| {
        v.as_f64()
            .map(|v| ((v - style.low) / (style.high - style.low)).clamp(0.0, 1.0) as f32)
    };
    let (y0, y1) = (style.gap, (h - style.gap).max(style.gap + 1.0));
    for c in 0..style.columns {
        let entry = list.and_then(|l| l.get((c + style.column_offset) as usize));
        let mut levels = [None; MAX_SUB_BARS];
        let mut n = 1;
        match entry {
            Some(Value::Array(subs)) if !subs.is_empty() => {
                n = subs.len().min(MAX_SUB_BARS);
                for (slot, v) in levels.iter_mut().zip(subs) {
                    *slot = level(v);
                }
            }
            Some(v) => levels[0] = level(v),
            None => {}
        }
        let (x0, x1) = span(c, style.columns, w, style.gap);
        let sub_gap = if n > 1 {
            (style.gap / 2.0).max(1.0)
        } else {
            0.0
        };
        let each = ((x1 - x0) - sub_gap * (n as f32 - 1.0)) / n as f32;
        for (i, lvl) in levels.iter().take(n).enumerate() {
            let sx0 = x0 + i as f32 * (each + sub_gap);
            let sx1 = sx0 + each;
            let lvl = lvl.unwrap_or(0.0);
            bar(style, canvas, (sx0, y0, sx1, y1), lvl);
        }
    }
}

/// One bar in `rect`, lit to `level` from its foot.
fn bar(style: &Style, canvas: &mut Canvas<'_>, rect: (f32, f32, f32, f32), level: f32) {
    let (x0, y0, x1, y1) = rect;
    let height = y1 - y0;
    if style.segments == 0 {
        rounded(canvas, rect, style.radius, style.pad);
        if level > 0.0 {
            let top = y1 - height * level;
            // A smooth bar shades by height: its colour at each row is the gradient's.
            fill_gradient(canvas, (x0, top, x1, y1), (y0, y1), style);
        }
        return;
    }
    let n = style.segments;
    let seg_gap = (style.gap / 2.0).max(1.0);
    for s in 0..n {
        // Segment 0 is the foot.
        let (sy0, sy1) = span(n - 1 - s, n, height + 2.0 * seg_gap, seg_gap);
        let (sy0, sy1) = (y0 - seg_gap + sy0, y0 - seg_gap + sy1);
        let at = (s as f32 + 0.5) / n as f32;
        let lit = level >= (s as f32 + 1.0) / n as f32 - 0.5 / n as f32;
        let fill = if lit {
            mix(style.color_low, style.color_high, at)
        } else {
            style.pad
        };
        rounded(canvas, (x0, sy0, x1, sy1), style.radius, fill);
    }
}

/// `color`'s red, green and blue times `b`.
fn scale(color: [u8; 4], b: f32) -> [u8; 4] {
    let c = |v: u8| (f32::from(v) * b).round().clamp(0.0, 255.0) as u8;
    [c(color[0]), c(color[1]), c(color[2]), color[3]]
}

/// `a` to `b` at `t` from 0 to 1.
fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let m = |i: usize| (f32::from(a[i]) * (1.0 - t) + f32::from(b[i]) * t).round() as u8;
    [m(0), m(1), m(2), m(3)]
}

/// Draws `color` over the pixel at `px` (straight RGBA) with `coverage` 0 to 1.
fn over(px: &mut [u8], color: [u8; 4], coverage: f32) {
    let a = f32::from(color[3]) / 255.0 * coverage;
    if a <= 0.0 {
        return;
    }
    let ba = f32::from(px[3]) / 255.0;
    let out = a + ba * (1.0 - a);
    for i in 0..3 {
        let v = (f32::from(color[i]) * a + f32::from(px[i]) * ba * (1.0 - a)) / out;
        px[i] = v.round().clamp(0.0, 255.0) as u8;
    }
    px[3] = (out * 255.0).round() as u8;
}

/// Fills the rounded rectangle `rect` (left, top, right, bottom, in pixels) with
/// `color`, its edges anti-aliased.
fn rounded(canvas: &mut Canvas<'_>, rect: (f32, f32, f32, f32), radius: f32, color: [u8; 4]) {
    let (x0, y0, x1, y1) = rect;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let half = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let r = radius.min(half.0).min(half.1);
    let (width, height) = (canvas.width(), canvas.height());
    let ys = (y0.floor().max(0.0) as u32)..(y1.ceil().min(height as f32) as u32);
    let xs = (x0.floor().max(0.0) as u32)..(x1.ceil().min(width as f32) as u32);
    for y in ys {
        let Some(row) = canvas.row(y) else { break };
        let py = y as f32 + 0.5 - cy;
        for x in xs.clone() {
            let px = x as f32 + 0.5 - cx;
            let qx = px.abs() - (half.0 - r);
            let qy = py.abs() - (half.1 - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            let d = outside + qx.max(qy).min(0.0) - r;
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let i = x as usize * 4;
                over(&mut row[i..i + 4], color, coverage);
            }
        }
    }
}

/// Fills `rect` with the bars' gradient, which runs from `color_low` at `ramp.1` up to
/// `color_high` at `ramp.0`; the top edge is anti-aliased.
fn fill_gradient(
    canvas: &mut Canvas<'_>,
    rect: (f32, f32, f32, f32),
    ramp: (f32, f32),
    style: &Style,
) {
    let (x0, y0, x1, y1) = rect;
    let (width, height) = (canvas.width(), canvas.height());
    let ys = (y0.floor().max(0.0) as u32)..(y1.ceil().min(height as f32) as u32);
    let xs = (x0.round().max(0.0) as u32)..(x1.round().min(width as f32) as u32);
    for y in ys {
        let fy = y as f32 + 0.5;
        let cover = (fy - y0 + 0.5).clamp(0.0, 1.0) * (y1 - fy + 0.5).clamp(0.0, 1.0);
        let t = (ramp.1 - fy) / (ramp.1 - ramp.0).max(1.0);
        let color = mix(style.color_low, style.color_high, t);
        let Some(row) = canvas.row(y) else { break };
        for x in xs.clone() {
            let i = x as usize * 4;
            over(&mut row[i..i + 4], color, cover);
        }
    }
}

#[cfg(test)]
mod tests;
