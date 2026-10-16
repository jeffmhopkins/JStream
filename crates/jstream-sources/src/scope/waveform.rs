//! The `waveform` source: the latest audio as an oscilloscope trace.

use super::{Scope, bad, color, create, ease, smoothing, string};
use crate::describe::{kind, param, size};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;

/// The widest frame a source may have (`describe::size`'s maximum).
const MAX_SIDE: usize = 16384;

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
        let width = setting(settings, "width")?.as_u64().unwrap_or(1);
        let scope = Waveform {
            style: Waveform::style(settings)?,
            // `create` checks the size before it allocates a frame; a column per pixel.
            columns: vec![(0.0, 0.0); usize::try_from(width).unwrap_or(1).min(MAX_SIDE)],
        };
        create(&self.0, settings, ctx, scope)
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

/// What a `waveform` source measures: each pixel column's lowest and highest level,
/// -1 to 1, as drawn last.
struct Waveform {
    style: Style,
    columns: Vec<(f32, f32)>,
}

impl Scope for Waveform {
    type Style = Style;

    fn style(settings: &Settings) -> Result<Style, SourceError> {
        let db = match string(settings, "scale")? {
            "linear" => false,
            "db" => true,
            _ => return Err(bad("scale", "must be `linear` or `db`")),
        };
        Ok(Style {
            color: color(settings, "color")?,
            background: color(settings, "background")?,
            smoothing: smoothing(settings)?,
            db,
        })
    }

    fn restyle(&mut self, style: Style) {
        self.style = style;
    }

    fn colors(&self) -> ([u8; 4], [u8; 4]) {
        (self.style.color, self.style.background)
    }

    /// Each column's range over its share of the window, eased from the last frame's.
    fn measure(&mut self, window: &[f32], _: Option<u32>, ticks: u64) {
        let width = self.columns.len();
        let style = self.style;
        for (x, column) in self.columns.iter_mut().enumerate() {
            let from = x * window.len() / width;
            // A column narrower than a sample shows the one it starts at.
            let to = ((x + 1) * window.len() / width).max(from + 1);
            let (low, high) = window[from..to]
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)));
            let (low, high) = (level(low, style.db), level(high, style.db));
            *column = (
                -ease(-column.0, -low, style.smoothing, ticks),
                ease(column.1, high, style.smoothing, ticks),
            );
        }
    }

    fn spans(&self, height: u32, spans: &mut [(u32, u32)]) {
        // Level 1 is the top row and -1 the bottom one.
        let half = (height - 1) as f32 / 2.0;
        let row = |level: f32| ((1.0 - level) * half).round() as u32;
        for (span, &(low, high)) in spans.iter_mut().zip(&self.columns) {
            *span = (row(high), row(low) + 1);
        }
    }

    fn settled(&self) -> bool {
        self.columns
            .iter()
            .all(|&(low, high)| low == 0.0 && high == 0.0)
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
