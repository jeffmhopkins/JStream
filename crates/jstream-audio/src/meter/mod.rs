//! Broadcast meters, as pure DSP over interleaved stereo blocks: the true-peak meter
//! ([`TruePeakMeter`], ITU-R BS.1770 Annex 2, 4× oversampled) and the loudness meter
//! ([`LoudnessMeter`], BS.1770 K-weighting with momentary, short-term and gated integrated
//! loudness). ADR-0028 puts them on the mixer thread, so each allocates its fixed-size
//! state in its constructor and nothing after that allocates, locks or blocks (R-RT-001).
//! Wiring them into the mixer's meters and the meter stream comes with the audio protocol.
//!
//! Docs: crates/jstream-audio/README.md#meters, docs/adr/0028-audio-model-v2.md

mod kweight;
mod loudness;
mod true_peak;

pub use loudness::LoudnessMeter;
pub use true_peak::TruePeakMeter;

#[cfg(test)]
pub(crate) mod testsig {
    //! Test signals: stereo sine tones at a level, as the EBU Tech 3341 vectors use.

    use std::f64::consts::TAU;

    /// Appends `seconds` of a stereo sine (both channels equal) at `freq` Hz and `dbfs`
    /// peak level, starting at `phase` radians, to `out`. Returns the phase after it.
    pub(crate) fn sine(
        out: &mut Vec<f32>,
        rate: u32,
        freq: f64,
        dbfs: f64,
        seconds: f64,
        phase: f64,
    ) -> f64 {
        let amp = 10f64.powf(dbfs / 20.0);
        let frames = (seconds * f64::from(rate)).round() as usize;
        let w = TAU * freq / f64::from(rate);
        for n in 0..frames {
            let s = (amp * (phase + w * n as f64).sin()) as f32;
            out.extend_from_slice(&[s, s]);
        }
        phase + w * frames as f64
    }
}
