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

#[test]
#[ignore]
fn tns_experiment() {
    let clicks = |period: usize, seconds: f32| -> Vec<f32> {
        let n = (RATE as f32 * seconds) as usize;
        let mut v = Vec::with_capacity(2 * n);
        let mut rng = Rng(3);
        for i in 0..n {
            let p = i % period;
            let s = if p < 24 {
                (-(p as f32) / 4.0).exp() * 0.7 * if p % 2 == 0 { 1.0 } else { -0.6 }
            } else {
                0.0
            } + 0.002 * rng.unit();
            v.push(s);
            v.push(s * 0.9);
        }
        v
    };
    let gap_noise = |input: &[f32], output: &[f32], period: usize| -> f64 {
        let (mut e, mut s) = (0.0f64, 0.0f64);
        for i in 4800..input.len() / 2 {
            let p = i % period;
            s += f64::from(input[2 * i]).powi(2);
            if p > 64 && p < period - 64 {
                e += f64::from(output[2 * i] - input[2 * i]).powi(2);
            }
        }
        10.0 * (e / s).log10()
    };
    for kbps in [64u32, 128] {
        for kind in ["tone", "two-tone", "music", "noise", "castanets", "clicks480", "clicks1200"] {
            let input = match kind {
                "clicks480" => clicks(480, 3.0),
                "clicks1200" => clicks(1200, 3.0),
                k => signal(k, 3.0),
            };
            let mut res = Vec::new();
            for tns in [false, true] {
                let mut c = Config::new(RATE, 2, kbps * 1000);
                c.tns = tns;
                let units = encode(c, &input);
                let pcm = decode([0x11, 0x90], &units).expect("decodes");
                let out = &pcm[2 * DELAY..2 * DELAY + input.len()];
                let bytes: usize = units.iter().map(Vec::len).sum();
                let seq = sequences(&units).iter().filter(|&&s| s != 0).count();
                let extra = match kind {
                    "clicks480" => gap_noise(&input, out, 480),
                    "clicks1200" => gap_noise(&input, out, 1200),
                    "castanets" => pre_echo(&input, out),
                    _ => 0.0,
                };
                res.push(format!(
                    "tns={tns} snr={:.1} extra={:.1} bytes={bytes} nonlong={seq}",
                    snr_db(&input, out),
                    extra
                ));
            }
            println!("{kbps} {kind}: {}", res.join(" | "));
        }
    }
}
