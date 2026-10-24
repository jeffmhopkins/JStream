//! Encoder-level tests: every codeword through an independent decoder, framing and
//! timing, errors, and inputs that must not panic or overrun the frame size.
//!
//! The decoder is Symphonia's pure-Rust AAC-LC decoder, a dev-dependency only
//! (ADR-0062: no decoder ships).

mod quality;

use std::f64::consts::PI;

use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

use crate::bits::BitWriter;
use crate::encoder::{Sequence, write_ics};
use crate::huffman::{ESC_HCB, Section, ZERO_HCB};
use crate::mdct::WindowShape;
use crate::quant::{Channel, Shape};
use crate::tables::SWB_LONG;
use crate::{Config, DELAY, Encoder, Error, FRAME_LEN};

/// Decodes raw access units with `asc`, returning interleaved samples.
pub(crate) fn decode(asc: [u8; 2], units: &[Vec<u8>]) -> Result<Vec<f32>, String> {
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_AAC)
        .with_extra_data(asc.to_vec().into_boxed_slice());
    let mut dec =
        AacDecoder::try_new(&params, &AudioDecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut frame = Vec::new();
    for (i, au) in units.iter().enumerate() {
        let packet = Packet::new(
            0,
            Timestamp::new((i * FRAME_LEN) as i64),
            Duration::new(FRAME_LEN as u64),
            au.clone(),
        );
        let buf = dec.decode(&packet).map_err(|e| format!("unit {i}: {e}"))?;
        buf.copy_to_vec_interleaved::<f32>(&mut frame);
        out.extend_from_slice(&frame);
    }
    Ok(out)
}

/// Encodes `input` (interleaved) whole and returns the access units.
pub(crate) fn encode(config: Config, input: &[f32]) -> Vec<Vec<u8>> {
    let mut enc = Encoder::new(config).expect("valid config");
    enc.push_interleaved(input).expect("whole frames");
    enc.finish();
    let mut units = Vec::new();
    let mut au = Vec::new();
    while enc.encode_frame(&mut au) {
        units.push(std::mem::take(&mut au));
    }
    units
}

/// A long-window SCE whose first bands hold `values` coded with `book`, at
/// `global_gain` throughout.
fn sce_with(book: u8, values: &[i32], global_gain: i32) -> Vec<u8> {
    let bands = 49;
    let mut ch = Channel::new();
    ch.q[..values.len()].copy_from_slice(values);
    let coded = (0..bands)
        .position(|b| usize::from(SWB_LONG[b + 1]) >= values.len())
        .expect("fits in a frame")
        + 1;
    ch.sections[0][0] = Section {
        book,
        start: 0,
        end: coded as u8,
    };
    ch.sections[0][1] = Section {
        book: ZERO_HCB,
        start: coded as u8,
        end: bands as u8,
    };
    ch.section_count[0] = 2;
    ch.global_gain = global_gain;
    ch.sf[..bands].fill(global_gain);
    let frame = Shape {
        offsets: &SWB_LONG,
        bands,
        windows: 1,
        lines: 1024,
    };
    let mut out = Vec::new();
    let mut w = BitWriter::new(&mut out);
    w.put(0, 3); // ID_SCE
    w.put(0, 4);
    write_ics(
        &mut w,
        &ch,
        &frame,
        Some((Sequence::OnlyLong, WindowShape::Sine)),
    );
    w.put(7, 3); // ID_END
    w.finish();
    out
}

/// What a decoder outputs for a lone long frame of dequantized `spectrum` (sine
/// window, both halves), over two frames, by the standard's IMDCT sum.
fn synthesis(spectrum: &[f64]) -> Vec<f64> {
    let n = 2048;
    let n0 = (n as f64 / 2.0 + 1.0) / 2.0;
    (0..n)
        .map(|i| {
            let y: f64 = spectrum
                .iter()
                .enumerate()
                .map(|(k, &x)| x * (2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5)).cos())
                .sum::<f64>()
                * 2.0
                / n as f64;
            let w = (PI / n as f64 * (i as f64 + 0.5)).sin();
            w * y / 32768.0
        })
        .collect()
}

/// Every codeword of every spectrum codebook, with signs and escapes, through the
/// reference decoder: the output must be the standard's synthesis of the values written.
#[test]
fn every_codeword_decodes_to_its_values() {
    let asc = [0x11, 0x88]; // AAC-LC, 48 kHz, mono
    for book in 1..=ESC_HCB {
        let (dim, signed, lav): (u32, bool, i32) = match book {
            1 | 2 => (4, true, 1),
            3 | 4 => (4, false, 2),
            5 | 6 => (2, true, 4),
            7 | 8 => (2, false, 7),
            9 | 10 => (2, false, 12),
            _ => (2, false, 16),
        };
        let modulus = if signed { 2 * lav + 1 } else { lav + 1 };
        let entries = modulus.pow(dim);
        let mut values = Vec::new();
        for idx in 0..entries {
            let mut digits = Vec::new();
            let mut rest = idx;
            for _ in 0..dim {
                digits.push(rest % modulus);
                rest /= modulus;
            }
            digits.reverse();
            for (j, d) in digits.into_iter().enumerate() {
                let v = if signed { d - lav } else { d };
                // Unsigned books: alternate signs so both sign bits are exercised.
                let v = if !signed && (idx + j as i32) % 2 == 1 {
                    -v
                } else {
                    v
                };
                values.push(v);
            }
        }
        if book == ESC_HCB {
            // Escapes of every length, 16 to 8191.
            values.extend([
                16, -17, 31, 32, -100, 255, 256, 1000, -4095, 4096, 8191, -8191,
            ]);
        }
        while values.len() % 4 != 0 {
            values.push(0);
        }
        // A gain of 2^15 (2^5 for the big escapes) keeps the output near full scale.
        let gain = if book == ESC_HCB { 120 } else { 160 };
        let units = vec![sce_with(book, &values, gain), sce_with(1, &[0; 4], gain)];
        let pcm = decode(asc, &units).unwrap_or_else(|e| panic!("book {book}: {e}"));
        let g = 2f64.powf(0.25 * f64::from(gain - 100));
        let mut spectrum = vec![0.0f64; 1024];
        for (s, &q) in spectrum.iter_mut().zip(&values) {
            *s = f64::from(q.signum()) * f64::from(q.unsigned_abs()).powf(4.0 / 3.0) * g;
        }
        let want = synthesis(&spectrum);
        let peak = want.iter().fold(0.0f64, |a, b| a.max(b.abs()));
        assert!(peak > 1e-4, "book {book}: silent test signal");
        let err = pcm
            .iter()
            .zip(&want)
            .fold(0.0f64, |a, (&p, &w)| a.max((f64::from(p) - w).abs()));
        assert!(
            err < 1e-5 * peak,
            "book {book}: error {err} against peak {peak}"
        );
    }
}

#[test]
fn audio_specific_configs() {
    let asc = |rate, ch| {
        Encoder::new(Config::new(rate, ch, 64_000 * u32::from(ch)))
            .expect("valid")
            .audio_specific_config()
    };
    // The bytes jstream-mux's AudioSpecificConfig::aac_lc writes for the same streams.
    assert_eq!(asc(48_000, 2), [0x11, 0x90]);
    assert_eq!(asc(44_100, 2), [0x12, 0x10]);
    assert_eq!(asc(44_100, 1), [0x12, 0x08]);
}

#[test]
fn refuses_what_it_does_not_support() {
    assert_eq!(
        Encoder::new(Config::new(32_000, 2, 128_000)).err(),
        Some(Error::SampleRate(32_000))
    );
    assert_eq!(
        Encoder::new(Config::new(48_000, 6, 384_000)).err(),
        Some(Error::Channels(6))
    );
    assert!(matches!(
        Encoder::new(Config::new(48_000, 2, 16_000)),
        Err(Error::BitRate { .. })
    ));
    assert!(matches!(
        Encoder::new(Config::new(48_000, 1, 320_000)),
        Err(Error::BitRate { .. })
    ));
    let mut enc = Encoder::new(Config::new(48_000, 2, 128_000)).expect("valid");
    assert!(matches!(
        enc.push_interleaved(&[0.0; 3]),
        Err(Error::PartialFrame {
            samples: 3,
            channels: 2
        })
    ));
    enc.finish();
    assert_eq!(enc.push_interleaved(&[0.0; 2]), Err(Error::Finished));
    assert_eq!(enc.push_planar(&[0.0; 2]), Err(Error::Finished));
}

/// One access unit per 1024 samples, plus one for the priming delay; units become ready
/// once the lookahead is in.
#[test]
fn framing_and_readiness() {
    for frames in [0usize, 1, 1023, 1024, 1025, 5000] {
        let units = encode(Config::new(48_000, 2, 128_000), &vec![0.0; frames * 2]);
        let want = if frames == 0 {
            0
        } else {
            frames.div_ceil(FRAME_LEN) + 1
        };
        assert_eq!(units.len(), want, "{frames} frames");
    }
    let mut enc = Encoder::new(Config::new(48_000, 1, 64_000)).expect("valid");
    enc.push_interleaved(&[0.0; 1599]).expect("ok");
    assert_eq!(enc.ready(), 0);
    enc.push_interleaved(&[0.0; 1]).expect("ok");
    // The priming frame of 1024 zeros, then 1600 samples: the first window and its
    // lookahead.
    assert_eq!(enc.ready(), 1);
    enc.push_interleaved(&[0.0; 2048]).expect("ok");
    assert_eq!(enc.ready(), 3);
    let mut au = Vec::new();
    assert!(enc.encode_frame(&mut au));
    assert_eq!(enc.ready(), 2);
    enc.finish();
    // 3648 samples: 4 frames' worth, plus the priming unit, less the one produced.
    assert_eq!(enc.ready(), 4);
}

/// Planar and interleaved input give the same stream.
#[test]
fn planar_matches_interleaved() {
    let frames = 3000;
    let inter: Vec<f32> = (0..frames)
        .flat_map(|i| {
            let t = i as f32 / 48_000.0;
            [(t * 3000.0).sin() * 0.3, (t * 5000.0).cos() * 0.2]
        })
        .collect();
    let planar: Vec<f32> = (0..2)
        .flat_map(|c| inter.iter().skip(c).step_by(2).copied().collect::<Vec<_>>())
        .collect();
    let a = encode(Config::new(48_000, 2, 128_000), &inter);
    let mut enc = Encoder::new(Config::new(48_000, 2, 128_000)).expect("valid");
    enc.push_planar(&planar).expect("ok");
    enc.finish();
    let mut b = Vec::new();
    let mut au = Vec::new();
    while enc.encode_frame(&mut au) {
        b.push(std::mem::take(&mut au));
    }
    assert_eq!(a, b);
}

/// A small deterministic generator for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn unit(&mut self) -> f32 {
        (self.next() as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Hostile input: NaN, infinities, values far past full scale, DC, full-scale square
/// waves, clicks, silence and noise, pushed in buffers of random sizes, at every rate and
/// layout. The encoder must not panic, every unit must fit the decoder's buffer (6144
/// bits per channel), and the stream must decode.
#[test]
fn hostile_input_never_panics_and_stays_in_bounds() {
    let mut rng = Rng(0x5eed);
    for round in 0..24 {
        let rate = [44_100, 48_000][round % 2];
        let channels = [1u8, 2][(round / 2) % 2];
        let per_channel = [16_000, 48_000, 96_000, 256_000][(round / 4) % 4];
        let config = Config {
            window_shape: if round % 3 == 0 {
                WindowShape::Kbd
            } else {
                WindowShape::Sine
            },
            ..Config::new(rate, channels, per_channel * u32::from(channels))
        };
        let mut enc = Encoder::new(config).expect("valid");
        let mut units = Vec::new();
        let mut au = Vec::new();
        for _ in 0..12 {
            let frames = rng.below(3000) as usize;
            let kind = rng.below(8);
            let mut buf = Vec::with_capacity(frames * usize::from(channels));
            for i in 0..frames * usize::from(channels) {
                buf.push(match kind {
                    0 => rng.unit(),
                    1 => [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30][i % 4],
                    2 => 1.0,
                    3 => {
                        if (i / 37) % 2 == 0 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    4 => {
                        if rng.below(500) == 0 {
                            rng.unit() * 50.0
                        } else {
                            0.0
                        }
                    }
                    5 => 0.0,
                    6 => rng.unit() * 1e-6,
                    _ => ((i as f32) * 0.37).sin() * 4.0,
                });
            }
            enc.push_interleaved(&buf).expect("whole frames");
            while enc.encode_frame(&mut au) {
                assert!(!au.is_empty());
                assert!(
                    au.len() * 8 <= 6144 * usize::from(channels),
                    "{} bytes",
                    au.len()
                );
                units.push(std::mem::take(&mut au));
            }
        }
        enc.finish();
        while enc.encode_frame(&mut au) {
            assert!(au.len() * 8 <= 6144 * usize::from(channels));
            units.push(std::mem::take(&mut au));
        }
        let pcm = decode(enc.audio_specific_config(), &units)
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_eq!(pcm.len(), units.len() * FRAME_LEN * usize::from(channels));
        assert!(pcm.iter().all(|s| s.is_finite()), "round {round}");
    }
}

/// Over a long, demanding signal (full-scale white noise), the stream's rate never
/// exceeds the target: the reservoir lends only what earlier frames saved.
#[test]
fn rate_stays_within_the_target() {
    let mut rng = Rng(42);
    for (channels, rate) in [(2u8, 128_000u32), (2, 192_000), (1, 64_000)] {
        let frames = 48_000 * 4;
        let input: Vec<f32> = (0..frames * usize::from(channels))
            .map(|_| rng.unit())
            .collect();
        let units = encode(Config::new(48_000, channels, rate), &input);
        let bits: usize = units.iter().map(|u| u.len() * 8).sum();
        let seconds = (units.len() * FRAME_LEN) as f64 / 48_000.0;
        let kbps = bits as f64 / seconds / 1000.0;
        let target = f64::from(rate) / 1000.0;
        assert!(kbps <= target * 1.001, "{kbps:.1} kbps against {target}");
        assert!(
            kbps >= target * 0.9,
            "{kbps:.1} kbps against {target}: bits unspent"
        );
        // And no window of 40 frames runs more than the reservoir over the mean.
        let mean = f64::from(rate) * 1024.0 / 48_000.0;
        for w in units.windows(40) {
            let b: usize = w.iter().map(|u| u.len() * 8).sum();
            assert!((b as f64) <= mean * 40.0 + 6144.0 * f64::from(channels));
        }
    }
}

/// The decoder lags by exactly DELAY samples: an impulse comes out DELAY samples late.
#[test]
fn delay_is_one_frame() {
    let mut input = vec![0.0f32; 8192];
    input[3000] = 0.9;
    let units = encode(Config::new(48_000, 1, 96_000), &input);
    let pcm = decode([0x11, 0x88], &units).expect("decodes");
    let peak = pcm
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map(|(i, _)| i)
        .expect("samples");
    assert_eq!(peak, 3000 + DELAY);
}
