//! The `spectrum` source: the latest audio's frequency spectrum as bars.

use super::waveform::{enum_type, live, smoothing_type};
use super::{
    Canvas, Feed, Frames, WINDOW, bad, color, ease, frame_size, next_tick, smoothing, string, tick,
    tick_start,
};
use crate::describe::{kind, param, size};
use crate::source::{changed_non_live, millis, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
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
        let size = frame_size(settings, &ctx.limits)?;
        let style = Style::read(settings)?;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        // The Hann window: it keeps a tone from smearing across the whole spectrum.
        let hann = (0..WINDOW)
            .map(|n| {
                let phase = std::f32::consts::TAU * n as f32 / WINDOW as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Ok(Box::new(SpectrumSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            bars: vec![0.0; style.bars],
            style,
            feed: Feed::new(ctx.audio.clone()),
            frames: Frames::new(size)?,
            window: vec![0.0; WINDOW],
            hann,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            drawn: None,
        }))
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

impl Style {
    fn read(settings: &Settings) -> Result<Self, SourceError> {
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
        Ok(Self {
            color: color(settings, "color")?,
            background: color(settings, "background")?,
            // At most MAX_BARS.
            bars: bars as usize,
            smoothing: smoothing(settings)?,
            log,
        })
    }
}

/// A running `spectrum` source. The FFT is planned and every buffer it and the drawing
/// use is made on create (or, for the bars, on an update that changes their count), so
/// a frame allocates only when every pooled frame is still held.
struct SpectrumSource {
    descriptor: KindDescriptor,
    settings: Settings,
    style: Style,
    feed: Feed,
    frames: Frames,
    window: Vec<f32>,
    hann: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Each bar's height, 0 to 1, as drawn last.
    bars: Vec<f32>,
    /// The frame tick drawn last.
    drawn: Option<u64>,
}

impl Source for SpectrumSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        let now = tick(at);
        if self.drawn == Some(now) {
            return Ok(self.frames.current());
        }
        let ticks = self
            .drawn
            .map_or(1, |drawn| now.saturating_sub(drawn).max(1));
        self.drawn = Some(now);
        let rate = self.feed.read(millis(at), &mut self.window);
        self.measure(rate, ticks);
        let (style, bars) = (self.style, &self.bars);
        self.frames
            .draw(tick_start(now), |canvas| draw(canvas, style, bars))
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
        let style = Style::read(settings)?;
        if style.bars != self.bars.len() {
            self.bars = vec![0.0; style.bars];
        }
        self.style = style;
        self.settings = settings.clone();
        // Redraw in the new style at the next frame.
        self.drawn = None;
        Ok(())
    }
}

impl SpectrumSource {
    /// Each bar's level from the window's spectrum at `rate` hertz (`None`: silence),
    /// eased from the last frame's.
    fn measure(&mut self, rate: Option<u32>, ticks: u64) {
        let style = self.style;
        let Some(rate) = rate.filter(|_| self.transform()) else {
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

    /// Windows the samples and runs the FFT into `output`; false if it failed.
    fn transform(&mut self) -> bool {
        for ((x, &s), &w) in self.input.iter_mut().zip(&self.window).zip(&self.hann) {
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

fn draw(canvas: &mut Canvas<'_>, style: Style, bars: &[f32]) {
    canvas.fill(style.background);
    let (width, height) = (canvas.width(), canvas.height());
    let count = bars.len() as u64;
    for x in 0..width {
        // The bar this column belongs to, and where that bar starts and ends.
        let bar = u64::from(x) * count / u64::from(width);
        let start = (bar * u64::from(width)).div_ceil(count);
        let end = ((bar + 1) * u64::from(width)).div_ceil(count);
        // A one-pixel gap after every bar at least three pixels wide.
        if end - start >= 3 && u64::from(x) + 1 == end {
            continue;
        }
        let level = bars[bar as usize];
        let top = ((1.0 - level) * height as f32).round() as u32;
        canvas.column(x, top, height, style.color);
    }
}
