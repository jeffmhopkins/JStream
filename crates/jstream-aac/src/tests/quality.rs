//! Objective quality through the reference decoder: waveform SNR on tonal signals, the
//! spectral envelope on noise, pre-echo around attacks, and the window sequences chosen.
//!
//! The bounds are floors with a margin under what the encoder measured when they were
//! written (the PR that added them lists the measurements), so a regression in the model,
//! the loops or the bitstream fails them.

use std::f32::consts::TAU;

use super::{Rng, decode, encode};
use crate::{Config, DELAY};

const RATE: u32 = 48_000;

/// Stereo test signals, interleaved, `seconds` long.
pub(super) fn signal(kind: &str, seconds: f32) -> Vec<f32> {
    let n = (RATE as f32 * seconds) as usize;
    let mut rng = Rng(7);
    let mut out = Vec::with_capacity(2 * n);
    // A one-pole low-pass for "pink-ish" noise.
    let mut lp = [0.0f32; 2];
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let (l, r) = match kind {
            "tone" => {
                let s = 0.5 * (TAU * 1000.0 * t).sin();
                (s, s)
            }
            "two-tone" => (
                0.3 * (TAU * 440.0 * t).sin() + 0.2 * (TAU * 3000.0 * t).sin(),
                0.3 * (TAU * 660.0 * t).sin() + 0.2 * (TAU * 5000.0 * t).sin(),
            ),
            "music" => {
                // A vibrato note with twelve harmonics, a second voice, an envelope, and
                // soft noise.
                let f = 220.0 * (1.0 + 0.004 * (TAU * 5.0 * t).sin());
                let mut v = 0.0;
                for h in 1..=12 {
                    v += (TAU * f * h as f32 * t).sin() / h as f32;
                }
                let env = 0.6 + 0.4 * (TAU * 0.7 * t).sin();
                let second = 0.15 * (TAU * 330.0 * t).sin();
                for x in &mut lp {
                    *x = 0.95 * *x + 0.05 * rng.unit();
                }
                (
                    0.25 * env * v + second + 0.3 * lp[0],
                    0.2 * env * v - second + 0.3 * lp[1],
                )
            }
            "noise" => (0.1 * rng.unit(), 0.1 * rng.unit()),
            _ => {
                // Castanet-like bursts every 250 ms over a quiet tone.
                let phase = i % (RATE as usize / 4);
                let burst = if phase < 2400 {
                    (-(phase as f32) / 300.0).exp() * rng.unit() * 0.8
                } else {
                    0.0
                };
                let bed = 0.02 * (TAU * 300.0 * t).sin();
                (burst + bed, burst * 0.8 + bed)
            }
        };
        out.push(l);
        out.push(r);
    }
    out
}

/// The decoded stream, aligned with the input (the priming delay removed).
fn round_trip(input: &[f32], bit_rate: u32) -> (Vec<f32>, Vec<Vec<u8>>) {
    let units = encode(Config::new(RATE, 2, bit_rate), input);
    let pcm = decode([0x11, 0x90], &units).expect("decodes");
    let aligned = pcm[2 * DELAY..2 * DELAY + input.len()].to_vec();
    (aligned, units)
}

/// Waveform SNR in dB, skipping the first 100 ms.
pub(super) fn snr_db(input: &[f32], output: &[f32]) -> f64 {
    let skip = 2 * 4800;
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (x, y) in input[skip..].iter().zip(&output[skip..]) {
        sig += f64::from(*x).powi(2);
        err += f64::from(x - y).powi(2);
    }
    10.0 * (sig / err.max(1e-30)).log10()
}

/// The window sequence of each unit (bits 9 and 10 of a CPE: id, tag, common_window,
/// ics_reserved_bit, then window_sequence).
pub(super) fn sequences(units: &[Vec<u8>]) -> Vec<u8> {
    units
        .iter()
        .map(|u| {
            let bits = u16::from_be_bytes([u[0], u[1]]);
            ((bits >> 5) & 3) as u8
        })
        .collect()
}

#[test]
fn tonal_signals_keep_a_high_snr() {
    for (kind, kbps, floor) in [
        ("tone", 128, 33.0),
        ("tone", 192, 43.0),
        ("two-tone", 128, 37.0),
        ("two-tone", 192, 44.0),
        ("music", 128, 10.0),
        ("music", 192, 12.5),
    ] {
        let input = signal(kind, 3.0);
        let (output, units) = round_trip(&input, kbps * 1000);
        let snr = snr_db(&input, &output);
        assert!(
            snr > floor,
            "{kind} at {kbps} kbps: SNR {snr:.1} dB, floor {floor}"
        );
        // Steady signals stay on long windows.
        assert!(
            sequences(&units).iter().all(|&s| s == 0),
            "{kind}: short windows on a steady signal"
        );
    }
}

/// Noise can't keep its waveform at these rates, but its spectrum must survive: the
/// energy in each third of an octave from 100 Hz to 14 kHz within 2.5 dB of the input's.
#[test]
fn noise_keeps_its_spectral_envelope() {
    let input = signal("noise", 2.0);
    let (output, _) = round_trip(&input, 128_000);
    let n = 4096;
    let mut worst = 0.0f64;
    let mut f = 100.0f64;
    while f < 14_000.0 {
        let (lo, hi) = (f, f * 2f64.powf(1.0 / 3.0));
        let band = |x: &[f32]| -> f64 {
            // Goertzel-free: a direct DFT of a 4096-sample block, summed over blocks.
            let mut e = 0.0;
            let k0 = (lo * n as f64 / f64::from(RATE)).ceil() as usize;
            let k1 = (hi * n as f64 / f64::from(RATE)).floor() as usize;
            for block in x.chunks_exact(2 * n).skip(1).take(2) {
                for k in k0..=k1.max(k0) {
                    let (mut re, mut im) = (0.0f64, 0.0f64);
                    for (j, frame) in block.chunks_exact(2).enumerate() {
                        let a = -std::f64::consts::TAU * (k * j) as f64 / n as f64;
                        let v = f64::from(frame[0]);
                        re += v * a.cos();
                        im += v * a.sin();
                    }
                    e += re * re + im * im;
                }
            }
            e
        };
        let diff = 10.0 * (band(&output) / band(&input)).log10();
        worst = worst.max(diff.abs());
        f = hi * 1.5;
    }
    assert!(worst < 2.5, "worst third-octave band off by {worst:.2} dB");
}

/// Attacks switch to short windows, and the noise before each attack stays well below
/// it (pre-echo).
#[test]
fn attacks_use_short_windows_and_limit_pre_echo() {
    let input = signal("castanets", 2.0);
    let (output, units) = round_trip(&input, 128_000);
    let seqs = sequences(&units);
    let short = seqs.iter().filter(|&&s| s == 2).count();
    assert!(short >= 6, "{short} short frames for 8 attacks: {seqs:?}");
    // Every short run is entered through LONG_START and left through LONG_STOP.
    for pair in seqs.windows(2) {
        match pair {
            [0 | 3, 2 | 3] | [1 | 2, 0 | 1] => {
                panic!("illegal transition {pair:?} in {seqs:?}")
            }
            _ => {}
        }
    }
    let worst = pre_echo(&input, &output);
    assert!(worst < -20.0, "pre-echo {worst:.1} dB below the attack");
}

/// The worst pre-echo of the castanets signal, in dB: the error energy in the 10 ms
/// before each burst against the burst's first 10 ms.
pub(super) fn pre_echo(input: &[f32], output: &[f32]) -> f64 {
    let period = RATE as usize / 4;
    let win = 480;
    let mut worst = f64::NEG_INFINITY;
    for onset in (period..input.len() / 2 - period).step_by(period) {
        let err: f64 = (onset - win..onset)
            .map(|i| f64::from(output[2 * i] - input[2 * i]).powi(2))
            .sum();
        let burst: f64 = (onset..onset + win)
            .map(|i| f64::from(input[2 * i]).powi(2))
            .sum();
        worst = worst.max(10.0 * (err / burst).log10());
    }
    worst
}

/// Temporal noise shaping keeps the quantization noise near the clicks of a click train
/// that long windows code, so the gaps between them stay quieter than without it, and
/// leaves steady signals as they were.
#[test]
fn temporal_noise_shaping_keeps_noise_by_the_clicks() {
    // 100 clicks a second, each a 0.5 ms decaying burst, over a faint noise floor.
    let period = 480;
    let n = RATE as usize * 3 / 2;
    let mut rng = Rng(3);
    let mut clicks = Vec::with_capacity(2 * n);
    for i in 0..n {
        let p = i % period;
        let click = if p < 24 {
            (-(p as f32) / 4.0).exp() * 0.7 * if p % 2 == 0 { 1.0 } else { -0.6 }
        } else {
            0.0
        };
        let s = click + 0.002 * rng.unit();
        clicks.push(s);
        clicks.push(s * 0.9);
    }
    // The error energy in the gaps, 64 samples clear of each click, against the signal's.
    let gap_noise = |output: &[f32]| -> f64 {
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in 4800..n {
            let p = i % period;
            sig += f64::from(clicks[2 * i]).powi(2);
            if p > 64 && p < period - 64 {
                err += f64::from(output[2 * i] - clicks[2 * i]).powi(2);
            }
        }
        10.0 * (err / sig).log10()
    };
    let run = |input: &[f32], kbps: u32, tns: bool| -> Vec<f32> {
        let mut config = Config::new(RATE, 2, kbps * 1000);
        config.tns = tns;
        let units = encode(config, input);
        let pcm = decode([0x11, 0x90], &units).expect("decodes");
        pcm[2 * DELAY..2 * DELAY + input.len()].to_vec()
    };
    for kbps in [64, 128] {
        let without = gap_noise(&run(&clicks, kbps, false));
        let with = gap_noise(&run(&clicks, kbps, true));
        assert!(
            with < without - 2.0,
            "{kbps} kbps: gap noise {with:.1} dB with TNS, {without:.1} dB without"
        );
    }
    for kind in ["tone", "two-tone"] {
        let input = signal(kind, 1.5);
        let without = snr_db(&input, &run(&input, 128, false));
        let with = snr_db(&input, &run(&input, 128, true));
        assert!(
            with > without - 0.5,
            "{kind}: SNR {with:.1} dB with TNS, {without:.1} dB without"
        );
    }
}

/// Clicks every `period` samples over a quiet tone: each a 4 ms burst of decaying noise.
/// At 512 samples apart (about 94 a second) the attack detector sees a steady signal and
/// keeps long windows, with four clicks in each.
fn clicks(seconds: f32, period: usize) -> Vec<f32> {
    let n = (RATE as f32 * seconds) as usize;
    let mut rng = Rng(11);
    let mut out = Vec::with_capacity(2 * n);
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let phase = i % period;
        let click = if phase < 192 {
            (-(phase as f32) / 24.0).exp() * rng.unit() * 0.7
        } else {
            0.0
        };
        let bed = 0.01 * (TAU * 300.0 * t).sin();
        out.push(click + bed);
        out.push(0.9 * click + bed);
    }
    out
}

/// The temporal envelope's error, in dB: the mean absolute difference, over 64-sample
/// blocks of the left channel after the first 100 ms, between the input's and the
/// output's block energies.
fn envelope_error_db(input: &[f32], output: &[f32]) -> f64 {
    let block = 64;
    let n = input.len() / 2;
    let energy = |x: &[f32], b: usize| -> f64 {
        (b..b + block)
            .map(|i| f64::from(x[2 * i]).powi(2))
            .sum::<f64>()
            .max(1e-12)
    };
    let (mut total, mut count) = (0.0, 0u32);
    let mut b = 4800;
    while b + block <= n {
        total += (10.0 * (energy(output, b) / energy(input, b)).log10()).abs();
        count += 1;
        b += block;
    }
    total / f64::from(count)
}

/// TNS engages on clicks inside long windows (in nine units of ten at least, and in none
/// with it off), and keeps the decoded temporal envelope closer to the input's than the
/// same stream without it.
#[test]
fn tns_keeps_the_temporal_envelope_of_clicks_in_long_windows() {
    let input = clicks(2.0, 512);
    for kbps in [96, 128] {
        let mut errors = [0.0; 2];
        for (tns, error) in [false, true].into_iter().zip(&mut errors) {
            let config = Config {
                tns,
                ..Config::new(RATE, 2, kbps * 1000)
            };
            let mut enc = crate::Encoder::new(config).expect("valid config");
            enc.push_interleaved(&input).expect("whole frames");
            enc.finish();
            let (mut units, mut au, mut with_tns) = (Vec::new(), Vec::new(), 0);
            while enc.encode_frame(&mut au) {
                with_tns += usize::from(enc.used_tns());
                units.push(std::mem::take(&mut au));
            }
            let seqs = sequences(&units);
            assert!(
                seqs.iter().all(|&s| s == 0),
                "clicks at {kbps} kbps: short windows {seqs:?}"
            );
            let pcm = decode([0x11, 0x90], &units).expect("decodes");
            let output = &pcm[2 * DELAY..2 * DELAY + input.len()];
            *error = envelope_error_db(&input, output);
            if tns {
                assert!(
                    with_tns * 10 >= units.len() * 9,
                    "clicks at {kbps} kbps: TNS in {with_tns} of {} units",
                    units.len()
                );
            } else {
                assert_eq!(with_tns, 0, "TNS off sends none");
            }
        }
        // Measured: 5.49 dB without TNS and 3.12 with it at 96 kbps, 5.41 and 2.90 at
        // 128 kbps; TNS in 94 and 95 of 95 units.
        let [without, with] = errors;
        assert!(
            with < 3.6 && without - with > 1.8,
            "clicks at {kbps} kbps: envelope error {with:.2} dB with TNS, {without:.2} without"
        );
    }
}

/// Noise from 6.5 to 10 kHz, the same in both channels but `gain` times as loud (and of
/// its sign) on the right.
fn panned_high_band(seconds: f32, gain: f32) -> Vec<f32> {
    let n = (RATE as f32 * seconds) as usize;
    let mut rng = Rng(17);
    let partials: Vec<(f32, f32)> = (0..120)
        .map(|_| {
            let f = 8250.0 + 1750.0 * rng.unit();
            (TAU * f / RATE as f32, TAU * rng.unit())
        })
        .collect();
    let mut out = Vec::with_capacity(2 * n);
    for i in 0..n {
        let s: f32 = partials
            .iter()
            .map(|&(w, p)| 0.02 * (w * i as f32 + p).sin())
            .sum();
        out.push(s);
        out.push(gain * s);
    }
    out
}

/// Intensity stereo on a panned high band: it engages in nine units of ten at least (and
/// in none with it off), each channel keeps its level within 1 dB of the input's, the
/// right channel keeps its sign, and the bits the right channel no longer spends lift the
/// left channel's SNR by at least 3 dB at the same rate.
#[test]
fn intensity_stereo_keeps_each_channels_level() {
    let skip = 2 * 4800;
    let energy = |x: &[f32], c: usize| -> f64 {
        x[skip..]
            .iter()
            .skip(c)
            .step_by(2)
            .map(|&v| f64::from(v).powi(2))
            .sum()
    };
    for gain in [0.35f32, -0.35] {
        let input = panned_high_band(2.0, gain);
        for kbps in [64, 96] {
            let mut snr = [0.0f64; 2];
            for (on, snr) in [false, true].into_iter().zip(&mut snr) {
                let config = Config {
                    intensity: on,
                    ..Config::new(RATE, 2, kbps * 1000)
                };
                let mut enc = crate::Encoder::new(config).expect("valid config");
                enc.push_interleaved(&input).expect("whole frames");
                enc.finish();
                let (mut units, mut au, mut with_is) = (Vec::new(), Vec::new(), 0);
                while enc.encode_frame(&mut au) {
                    with_is += usize::from(enc.intensity_bands() > 0);
                    units.push(std::mem::take(&mut au));
                }
                if on {
                    assert!(
                        with_is * 10 >= units.len() * 9,
                        "gain {gain} at {kbps} kbps: intensity in {with_is} of {} units",
                        units.len()
                    );
                } else {
                    assert_eq!(with_is, 0, "intensity off sends none");
                }
                let pcm = decode([0x11, 0x90], &units).expect("decodes");
                let output = &pcm[2 * DELAY..2 * DELAY + input.len()];
                for c in 0..2 {
                    let db = 10.0 * (energy(output, c) / energy(&input, c)).log10();
                    assert!(
                        db.abs() < 1.0,
                        "gain {gain} at {kbps} kbps, intensity {on}: channel {c} off by {db:.2} dB"
                    );
                }
                let cross: f64 = output[skip..]
                    .chunks_exact(2)
                    .map(|f| f64::from(f[0]) * f64::from(f[1]))
                    .sum();
                let correlation = cross / (energy(output, 0) * energy(output, 1)).sqrt();
                assert!(
                    correlation * f64::from(gain.signum()) > 0.9,
                    "gain {gain} at {kbps} kbps: correlation {correlation:.3}"
                );
                let left: Vec<f32> = input.iter().step_by(2).copied().collect();
                let decoded: Vec<f32> = output.iter().step_by(2).copied().collect();
                let err: f64 = left[skip / 2..]
                    .iter()
                    .zip(&decoded[skip / 2..])
                    .map(|(&x, &y)| f64::from(x - y).powi(2))
                    .sum();
                *snr = 10.0 * (energy(&input, 0) / err).log10();
            }
            // Measured: the left channel's SNR 5.9 dB without and 10.9 with intensity
            // stereo at 64 kbps, 11.1 and 15.3 at 96 kbps; levels within 0.5 dB; intensity
            // in 93 and 95 of 95 units.
            let [without, with] = snr;
            assert!(
                with > without + 3.0,
                "gain {gain} at {kbps} kbps: left SNR {with:.1} dB with intensity, {without:.1} without"
            );
        }
    }
}
