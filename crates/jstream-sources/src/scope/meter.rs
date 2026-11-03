//! [`AudioMeter`]: what a shader source (ADR-0093) hears of an [`AudioScope`] each frame:
//! levels, an onset, [`METER_BANDS`] log-spaced spectrum bands and a short waveform.
//!
//! It is made once per drawing filter, with its FFT planned and its buffers allocated
//! then, so [`AudioMeter::measure`] allocates nothing and takes no lock: it runs on the
//! thread that draws (the engine's render thread). Like the scope sources, it hears
//! silence once the ring has stood still for a quarter second.

use super::spectrum::{HIGH_HZ, LOW_HZ};
use super::{AudioScope, STILL_MS, WINDOW};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// The bands of a meter's spectrum, and the samples of its waveform.
pub const METER_BANDS: usize = 64;

/// How much of a band, the level and the onset is left after a sixtieth of a second
/// once its input falls: they rise at once and fall over a few frames.
const FALL: f32 = 0.85;
/// How much of the slow level, which an onset is measured against, a sixtieth of a
/// second keeps.
const SLOW: f32 = 0.97;

/// What an [`AudioMeter`] measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metered {
    /// RMS level of the window, 0 to 1.
    pub level: f32,
    /// Peak absolute sample of the window, 0 to 1.
    pub peak: f32,
    /// 1 when the level jumps above its recent average, falling back to 0.
    pub onset: f32,
    /// Each band's level on the dB scale from -90 to 0 dBFS, 0 to 1, lowest first.
    pub bands: [f32; METER_BANDS],
    /// The window's samples at [`METER_BANDS`] evenly spaced points, oldest first, -1 to 1.
    pub wave: [f32; METER_BANDS],
}

impl Default for Metered {
    fn default() -> Self {
        Self {
            level: 0.0,
            peak: 0.0,
            onset: 0.0,
            bands: [0.0; METER_BANDS],
            wave: [0.0; METER_BANDS],
        }
    }
}

/// Measures an [`AudioScope`] frame by frame.
pub struct AudioMeter {
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// The ring's count when it last moved, and when (ms on the caller's clock).
    seen: u64,
    moved_at: u64,
    /// When the last measure was, for the falls.
    last: Option<u64>,
    /// The slow level an onset is measured against.
    slow: f32,
    metered: Metered,
}

impl std::fmt::Debug for AudioMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioMeter")
            .field("metered", &self.metered)
            .finish_non_exhaustive()
    }
}

impl Default for AudioMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMeter {
    /// A meter that has heard silence. It plans its FFT and allocates its buffers here.
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let hann = (0..WINDOW)
            .map(|n| {
                let phase = std::f32::consts::TAU * n as f32 / WINDOW as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Self {
            hann,
            window: vec![0.0; WINDOW],
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            seen: 0,
            moved_at: 0,
            last: None,
            slow: 0.0,
            metered: Metered::default(),
        }
    }

    /// Measures `scope`'s latest window at `ms`, a millisecond clock that only counts
    /// up. Allocates nothing.
    pub fn measure(&mut self, scope: &AudioScope, ms: u64) -> &Metered {
        let (written, rate) = scope.latest(&mut self.window);
        if written != self.seen {
            self.seen = written;
            self.moved_at = ms;
        }
        let heard = rate != 0 && ms.saturating_sub(self.moved_at) < STILL_MS;
        if !heard {
            self.window.fill(0.0);
        }
        // How many sixtieths of a second since the last measure, for the falls.
        let frames = self
            .last
            .map_or(1.0, |last| ms.saturating_sub(last) as f32 * 60.0 / 1000.0);
        self.last = Some(ms);
        let fall = FALL.powf(frames);
        let m = &mut self.metered;

        let (mut sum, mut peak) = (0.0f32, 0.0f32);
        for &s in &self.window {
            sum += s * s;
            peak = peak.max(s.abs());
        }
        let level = (sum / WINDOW as f32).sqrt().min(1.0);
        m.level = level.max(m.level * fall);
        m.peak = peak.min(1.0).max(m.peak * fall);
        let jumped = level > self.slow * 1.5 + 0.02;
        m.onset = if jumped { 1.0 } else { m.onset * fall };
        self.slow = level + (self.slow - level) * SLOW.powf(frames);

        for (n, sample) in m.wave.iter_mut().enumerate() {
            *sample = self.window[n * WINDOW / METER_BANDS].clamp(-1.0, 1.0);
        }

        let transformed = heard && {
            for ((x, &s), &w) in self.input.iter_mut().zip(&self.window).zip(&self.hann) {
                *x = s * w;
            }
            self.fft
                .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
                .is_ok()
        };
        if !transformed {
            for band in &mut m.bands {
                *band *= fall;
            }
            return &self.metered;
        }
        // Bin k is k * rate / WINDOW hertz; the bands are log-spaced from 20 Hz up.
        let bin_hz = rate as f32 / WINDOW as f32;
        let high = HIGH_HZ.min(rate as f32 / 2.0);
        let low = LOW_HZ.min(high / 2.0);
        let edge = |i: usize| low * (high / low).powf(i as f32 / METER_BANDS as f32);
        let last = self.output.len() - 1;
        for (i, band) in m.bands.iter_mut().enumerate() {
            let (from, to) = (edge(i) / bin_hz, edge(i + 1) / bin_hz);
            let first = (from.round() as usize).min(last);
            let end = (to.ceil() as usize).clamp(first + 1, last + 1);
            let power = self.output[first..end]
                .iter()
                .map(|c| c.norm_sqr())
                .fold(0.0, f32::max);
            *band = super::spectrum::height(power).max(*band * fall);
        }
        &self.metered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A meter hears a tone: its band, a level and an onset; then silence once the ring
    /// stands still.
    #[test]
    fn a_meter_hears_a_tone_then_settles_on_silence() {
        let scope = AudioScope::new();
        let mut meter = AudioMeter::new();
        assert_eq!(meter.measure(&scope, 0), &Metered::default());
        // A full-scale 1 kHz sine at 48 kHz, stereo.
        let samples: Vec<f32> = (0..WINDOW)
            .flat_map(|n| {
                let s = (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0).sin();
                [s, s]
            })
            .collect();
        scope.push(48_000, &samples);
        let heard = *meter.measure(&scope, 16);
        assert!(heard.level > 0.6 && heard.peak > 0.99, "{heard:?}");
        assert!((heard.onset - 1.0).abs() < f32::EPSILON);
        // 1 kHz on 20 Hz to 20 kHz, log-spaced: band 64 * log(50) / log(1000), about 36.
        let loudest = (0..METER_BANDS)
            .max_by(|&a, &b| heard.bands[a].total_cmp(&heard.bands[b]))
            .expect("bands");
        assert!((34..=38).contains(&loudest), "{loudest}");
        assert!(heard.wave.iter().any(|&s| s.abs() > 0.5));
        // The ring stands still: after a quarter second it is silence, falling.
        let later = *meter.measure(&scope, 16 + STILL_MS);
        assert!(later.level < heard.level && later.wave.iter().all(|&s| s == 0.0));
    }
}
