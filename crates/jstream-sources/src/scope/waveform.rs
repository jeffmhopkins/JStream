//! The `waveform` source: the latest audio as an oscilloscope trace.

use super::{
    Canvas, Feed, Frames, WINDOW, bad, color, ease, frame_size, next_tick, smoothing, string, tick,
    tick_start,
};
use crate::describe::{kind, param, size};
use crate::source::{changed_non_live, millis};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The quietest level the `db` scale draws, in dBFS; quieter draws on the centre line.
const FLOOR_DB: f32 = -60.0;

/// The `waveform` kind.
pub(crate) struct WaveformKind(KindDescriptor);

impl WaveformKind {
    pub(crate) fn new() -> Self {
        Self(KindDescriptor {
            params: vec![
                param("width", "Width", size(), json!(640)),
                param("height", "Height", size(), json!(160)),
                live(param(
                    "color",
                    "Colour",
                    ParamType::Color { alpha: true },
                    json!("#ffffffff"),
                )),
                live(param(
                    "background",
                    "Background",
                    ParamType::Color { alpha: true },
                    json!("#00000000"),
                )),
                live(ParamDescriptor {
                    description: Some(
                        "How slowly a peak falls back: 0 follows the audio exactly.".into(),
                    ),
                    ..param("smoothing", "Smoothing", smoothing_type(), json!(0.0))
                }),
                live(ParamDescriptor {
                    description: Some(format!(
                        "Linear draws the samples as they are; dB draws their level from \
                         {FLOOR_DB} dBFS, so quiet audio shows."
                    )),
                    ..param(
                        "scale",
                        "Scale",
                        enum_type(&[("linear", "Linear"), ("db", "dB")]),
                        json!("linear"),
                    )
                }),
            ],
            ..kind("waveform", "Audio waveform", true, false)
        })
    }
}

impl SourceKind for WaveformKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let size = frame_size(settings, &ctx.limits)?;
        let style = Style::read(settings)?;
        Ok(Box::new(WaveformSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            style,
            feed: Feed::new(ctx.audio.clone()),
            frames: Frames::new(size)?,
            window: vec![0.0; WINDOW],
            columns: vec![(0.0, 0.0); size.width() as usize],
            drawn: None,
        }))
    }
}

/// A parameter whose change applies without a new source.
pub(super) fn live(p: ParamDescriptor) -> ParamDescriptor {
    ParamDescriptor { live: true, ..p }
}

/// The `smoothing` parameter's type.
pub(super) fn smoothing_type() -> ParamType {
    ParamType::Float {
        min: Some(0.0),
        max: Some(super::MAX_SMOOTHING),
        step: Some(0.01),
        unit: None,
    }
}

/// An enum parameter's type from `(value, label)` pairs.
pub(super) fn enum_type(options: &[(&str, &str)]) -> ParamType {
    ParamType::Enum {
        options: options
            .iter()
            .map(|&(value, label)| EnumOption {
                value: value.into(),
                label: label.into(),
            })
            .collect(),
    }
}

/// The live settings.
#[derive(Clone, Copy, Debug)]
struct Style {
    color: [u8; 4],
    background: [u8; 4],
    smoothing: f32,
    db: bool,
}

impl Style {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
        let db = match string(settings, "scale")? {
            "linear" => false,
            "db" => true,
            _ => return Err(bad("scale", "must be `linear` or `db`")),
        };
        Ok(Self {
            color: color(settings, "color")?,
            background: color(settings, "background")?,
            smoothing: smoothing(settings)?,
            db,
        })
    }
}

/// A running `waveform` source. Every buffer it draws with is made on create, so a frame
/// allocates only when every pooled frame is still held.
struct WaveformSource {
    descriptor: KindDescriptor,
    settings: Settings,
    style: Style,
    feed: Feed,
    frames: Frames,
    window: Vec<f32>,
    /// Each pixel column's lowest and highest level, -1 to 1, as drawn last.
    columns: Vec<(f32, f32)>,
    /// The frame tick drawn last.
    drawn: Option<u64>,
}

impl Source for WaveformSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        let now = tick(at);
        if self.drawn == Some(now) {
            return Ok(self.frames.current());
        }
        let ticks = self
            .drawn
            .map_or(1, |drawn| now.saturating_sub(drawn).max(1));
        self.drawn = Some(now);
        self.feed.read(millis(at), &mut self.window);
        self.measure(ticks);
        let (style, columns) = (self.style, &self.columns);
        self.frames
            .draw(tick_start(now), |canvas| draw(canvas, style, columns))
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.feed.scope.as_ref().map(|_| next_tick(at))
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        self.style = Style::read(settings)?;
        self.settings = settings.clone();
        // Redraw in the new style at the next frame.
        self.drawn = None;
        Ok(())
    }
}

impl WaveformSource {
    /// Each column's range over its share of the window, eased from the last frame's.
    fn measure(&mut self, ticks: u64) {
        let width = self.columns.len();
        let style = self.style;
        for (x, column) in self.columns.iter_mut().enumerate() {
            let from = x * WINDOW / width;
            // A column narrower than a sample shows the one it starts at.
            let to = ((x + 1) * WINDOW / width).max(from + 1);
            let (low, high) = self.window[from..to]
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)));
            let (low, high) = (level(low, style.db), level(high, style.db));
            *column = (
                -ease(-column.0, -low, style.smoothing, ticks),
                ease(column.1, high, style.smoothing, ticks),
            );
        }
    }
}

/// A sample as a height from -1 to 1 on the scale.
fn level(sample: f32, db: bool) -> f32 {
    let sample = sample.clamp(-1.0, 1.0);
    if !db || sample == 0.0 {
        return sample;
    }
    let height = ((20.0 * sample.abs().log10() - FLOOR_DB) / -FLOOR_DB).max(0.0);
    height.copysign(sample)
}

fn draw(canvas: &mut Canvas<'_>, style: Style, columns: &[(f32, f32)]) {
    canvas.fill(style.background);
    let height = canvas.height();
    // Level 1 is the top row and -1 the bottom one.
    let half = (height - 1) as f32 / 2.0;
    let row = |level: f32| ((1.0 - level) * half).round() as u32;
    for (x, &(low, high)) in (0..canvas.width()).zip(columns) {
        canvas.column(x, row(high), row(low) + 1, style.color);
    }
}
