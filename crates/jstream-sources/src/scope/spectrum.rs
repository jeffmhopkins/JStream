//! The `spectrum` source: the latest audio's frequency spectrum as bars.

use super::waveform::{audio_param, audio_source_param, enum_type, live, smoothing_type};
use super::{Scope, WINDOW, bad, color, create, ease, smoothing, string};
use crate::describe::{kind, param, size};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde_json::json;
use std::sync::Arc;

/// The most bars.
const MAX_BARS: u32 = 1024;
/// The quietest level drawn, in dBFS (a full-scale sine is 0).
const FLOOR_DB: f32 = -90.0;
/// The lowest frequency the `log` scale shows, in hertz.
const LOW_HZ: f32 = 20.0;
/// The highest frequency shown, in hertz, when the sample rate allows it.
const HIGH_HZ: f32 = 20_000.0;

/// The `spectrum` kind.
pub(crate) struct SpectrumKind(KindDescriptor);

impl SpectrumKind {
    pub(crate) fn new() -> Self {
        Self(KindDescriptor {
            params: vec![
                audio_param(),
                audio_source_param(),
                param("width", "Width", size(), json!(640)),
                param("height", "Height", size(), json!(240)),
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
                live(param(
                    "bars",
                    "Bars",
                    ParamType::Int {
                        min: Some(1),
                        max: Some(i64::from(MAX_BARS)),
                        step: Some(1),
                        unit: None,
                    },
                    json!(64),
                )),
                live(ParamDescriptor {
                    description: Some(
                        "How slowly a bar falls back: 0 follows the audio exactly.".into(),
                    ),
                    ..param("smoothing", "Smoothing", smoothing_type(), json!(0.7))
                }),
                live(ParamDescriptor {
                    description: Some(format!(
                        "How frequencies spread across the bars: log gives each octave \
                         the same width from {LOW_HZ} Hz, as the ear hears them; linear \
                         gives each hertz the same width from 0 Hz."
                    )),
                    ..param(
                        "scale",
                        "Scale",
                        enum_type(&[("log", "Logarithmic"), ("linear", "Linear")]),
                        json!("log"),
                    )
                }),
            ],
            ..kind("spectrum", "Audio spectrum", true, false)
        })
    }
}

impl SourceKind for SpectrumKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let style = Spectrum::style(settings)?;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        // The Hann window: it keeps a tone from smearing across the whole spectrum.
        let hann = (0..WINDOW)
            .map(|n| {
                let phase = std::f32::consts::TAU * n as f32 / WINDOW as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let scope = Spectrum {
            bars: vec![0.0; style.bars],
            style,
            hann,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
        };
        create(&self.0, settings, ctx, scope)
    }
}

/// The live settings.
#[derive(Clone, Copy, Debug)]
struct Style {
    color: [u8; 4],
    background: [u8; 4],
    bars: usize,
    smoothing: f32,
    log: bool,
}

/// What a `spectrum` source measures. The FFT is planned and its buffers made on create;
/// the bars are made again only by an update that changes their count.
struct Spectrum {
    style: Style,
    hann: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Each bar's height, 0 to 1, as drawn last.
    bars: Vec<f32>,
}

impl Scope for Spectrum {
    type Style = Style;

    fn style(settings: &Settings) -> Result<Style, SourceError> {
        let bars = setting(settings, "bars")?
            .as_u64()
            .filter(|b| (1..=u64::from(MAX_BARS)).contains(b))
            .ok_or_else(|| {
                bad(
                    "bars",
                    &format!("must be a whole number from 1 to {MAX_BARS}"),
                )
            })?;
        let log = match string(settings, "scale")? {
            "log" => true,
            "linear" => false,
            _ => return Err(bad("scale", "must be `log` or `linear`")),
        };
        Ok(Style {
            color: color(settings, "color")?,
            background: color(settings, "background")?,
            // At most MAX_BARS.
            bars: bars as usize,
            smoothing: smoothing(settings)?,
            log,
        })
    }

    fn restyle(&mut self, style: Style) {
        if style.bars != self.bars.len() {
            self.bars = vec![0.0; style.bars];
        }
        self.style = style;
    }

    fn colors(&self) -> ([u8; 4], [u8; 4]) {
        (self.style.color, self.style.background)
    }

    /// Each bar's level from the window's spectrum (`None`: silence), eased from the
    /// last frame's.
    fn measure(&mut self, window: &[f32], rate: Option<u32>, ticks: u64) {
        let style = self.style;
        let Some(rate) = rate.filter(|_| self.transform(window)) else {
            for bar in &mut self.bars {
                *bar = ease(*bar, 0.0, style.smoothing, ticks);
            }
            return;
        };
        // Bin k is k * rate / WINDOW hertz.
        let bin_hz = rate as f32 / WINDOW as f32;
        let high = HIGH_HZ.min(rate as f32 / 2.0);
        let low = if style.log {
            LOW_HZ.min(high / 2.0)
        } else {
            0.0
        };
        let count = self.bars.len();
        let edge = |i: usize| {
            let t = i as f32 / count as f32;
            if style.log {
                low * (high / low).powf(t)
            } else {
                low + (high - low) * t
            }
        };
        let last = self.output.len() - 1;
        for (i, bar) in self.bars.iter_mut().enumerate() {
            let (from, to) = (edge(i) / bin_hz, edge(i + 1) / bin_hz);
            // At least the bin nearest the bar's lower edge, so a bar narrower than a
            // bin (low bars on the log scale) still shows.
            let first = (from.round() as usize).min(last);
            let end = (to.ceil() as usize).clamp(first + 1, last + 1);
            let peak = self.output[first..end]
                .iter()
                .map(|c| c.norm_sqr())
                .fold(0.0, f32::max);
            *bar = ease(*bar, height(peak), style.smoothing, ticks);
        }
    }

    fn spans(&self, height: u32, spans: &mut [(u32, u32)]) {
        let width = spans.len() as u64;
        let count = self.bars.len() as u64;
        for (x, span) in (0u64..).zip(spans.iter_mut()) {
            // The bar this column belongs to, and where that bar starts and ends.
            let bar = x * count / width;
            let start = (bar * width).div_ceil(count);
            let end = ((bar + 1) * width).div_ceil(count);
            // A one-pixel gap after every bar at least three pixels wide.
            *span = if end - start >= 3 && x + 1 == end {
                (0, 0)
            } else {
                let level = self.bars[bar as usize];
                (((1.0 - level) * height as f32).round() as u32, height)
            };
        }
    }

    fn settled(&self) -> bool {
        self.bars.iter().all(|&bar| bar == 0.0)
    }
}

impl Spectrum {
    /// Windows the samples and runs the FFT into `output`; false if it failed.
    fn transform(&mut self, window: &[f32]) -> bool {
        for ((x, &s), &w) in self.input.iter_mut().zip(window).zip(&self.hann) {
            *x = s * w;
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_ok()
    }
}

/// A bin's squared magnitude as a bar height from 0 to 1, on the dB scale from
/// [`FLOOR_DB`] to 0 dBFS.
fn height(norm_sqr: f32) -> f32 {
    // A full-scale sine at a bin's frequency peaks at sum(hann) / 2 = WINDOW / 4.
    let full = (WINDOW / 4) as f32;
    let db = 10.0 * (norm_sqr / (full * full)).max(1e-12).log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}
