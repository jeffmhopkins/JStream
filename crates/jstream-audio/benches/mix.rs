//! R-RT-005: what the audio threads cost per block. Run on the machine the numbers are
//! for, and state that machine with them:
//!
//! ```text
//! cargo bench -p jstream-audio --bench mix
//! ```
//!
//! A block is 1024 frames at 48 kHz, 21.3 ms of audio: a mean near that would miss the
//! real-time deadline. `tests/cost.rs` measures the same mixer scenarios with p99 and max
//! per block; these benchmarks track the mean over time.
//!
//! - `mix_block`: `Mixer::mix` alone on the mixer thread, with every input fed ahead of
//!   the block outside the timing, for the send matrix and the filter chains (#228 N3).
//! - `input_push`: one device packet through `InputProducer::push_packet` on the input's
//!   audio thread (downmix and adaptive resampling onto the engine time line), at the
//!   engine rate and at 44.1 kHz. The mixer drains the ring outside the timing.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, criterion_main};
use jstream_audio::filter::{CompressorParams, GateParams, LimiterParams};
use jstream_audio::{FilterSpec, InputProducer, Mixer, MixerConfig};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
use jstream_platform::{AudioPacket, AudioPacketFlags};

const RATE: u32 = 48_000;
const BLOCK: usize = 1024;
const INPUTS: usize = 8;
/// Buses besides the master.
const AUX: usize = 7;

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    /// Every input sends to every aux bus.
    sends: bool,
    /// Gate and compressor per input, compressor and 5 ms limiter per bus, a gain and
    /// 3 ms limiter master chain, and an output tap per aux bus with a gain and 5 ms
    /// limiter of its own.
    chains: bool,
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "master_only",
        sends: false,
        chains: false,
    },
    Scenario {
        name: "send_matrix",
        sends: true,
        chains: false,
    },
    Scenario {
        name: "send_matrix_and_chains",
        sends: true,
        chains: true,
    },
];

fn limiter(lookahead_ms: f32) -> FilterSpec {
    FilterSpec::Limiter {
        lookahead_ms,
        params: LimiterParams::default(),
    }
}

fn format(rate: u32) -> AudioFormat {
    AudioFormat::new(rate, ChannelLayout::Stereo, SampleFormat::F32Interleaved).expect("format")
}

fn mixer() -> Mixer {
    let config = MixerConfig::default();
    assert_eq!(config.block_frames, BLOCK);
    Mixer::new(config).expect("valid config")
}

fn build(scenario: Scenario) -> (Mixer, Vec<InputProducer>) {
    let mut mixer = mixer();
    let mut producers = Vec::new();
    let mut inputs = Vec::new();
    for _ in 0..INPUTS {
        let (producer, input) = mixer.add_input(format(RATE)).expect("room for an input");
        if scenario.chains {
            input
                .add_filter(FilterSpec::NoiseGate(GateParams::default()))
                .expect("gate");
            input
                .add_filter(FilterSpec::Compressor(CompressorParams::default()))
                .expect("compressor");
        }
        producers.push(producer);
        inputs.push(input);
    }
    let mut buses = vec![mixer.bus()];
    if scenario.sends {
        for _ in 0..AUX {
            let bus = mixer.add_bus().expect("room for a bus");
            for input in &inputs {
                input.set_send_db(bus.id(), -6.0);
            }
            buses.push(bus);
        }
    }
    if scenario.chains {
        for bus in &buses {
            bus.add_filter(FilterSpec::Compressor(CompressorParams::default()))
                .expect("compressor");
            bus.add_filter(limiter(5.0)).expect("limiter");
        }
        let master = mixer.master();
        master
            .add_filter(FilterSpec::Gain { gain_db: -1.0 })
            .expect("gain");
        master.add_filter(limiter(3.0)).expect("limiter");
        for bus in &buses[1..] {
            let tap = mixer.add_output_tap(bus.id()).expect("room for a tap");
            tap.add_filter(FilterSpec::Gain { gain_db: -2.0 })
                .expect("gain");
            tap.add_filter(limiter(5.0)).expect("limiter");
        }
    }
    // The inputs stay alive in the mixer; their handles are not needed.
    drop(inputs);
    (mixer, producers)
}

/// `[from, to)` frames of a 440 Hz stereo tone at `rate`.
fn tone(rate: u32, from: u64, to: u64) -> Vec<f32> {
    (from..to)
        .flat_map(|n| {
            let s =
                (0.3 * (std::f64::consts::TAU * 440.0 * n as f64 / f64::from(rate)).sin()) as f32;
            [s, s]
        })
        .collect()
}

/// Pushes `samples`, which start at frame `from` of `rate`, to `producer`, stamped exactly.
fn push(producer: &mut InputProducer, rate: u32, from: u64, samples: &[f32]) {
    let tb = Timebase::per_second(rate).expect("rate");
    producer.push_packet(&AudioPacket {
        samples,
        device_position: from,
        time: Some(MediaTime::new(from as i64, tb)),
        flags: AudioPacketFlags::default(),
    });
}

/// Mixer time line: audio is fed this many blocks ahead of the block mixed.
const LEAD: u64 = 2;

fn mix_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("mix_block");
    for scenario in SCENARIOS {
        group.bench_function(BenchmarkId::from_parameter(scenario.name), |b| {
            let (mut mixer, mut producers) = build(scenario);
            let mut out = vec![0.0; 2 * BLOCK];
            let block = BLOCK as u64;
            // Start a few blocks in, with audio fed ahead of each block mixed.
            let mut frame = 4 * block;
            let lead = tone(RATE, 0, frame + LEAD * block);
            for p in &mut producers {
                push(p, RATE, 0, &lead);
            }
            b.iter_custom(|iters| {
                let mut took = Duration::ZERO;
                for _ in 0..iters {
                    let from = frame + LEAD * block;
                    let samples = tone(RATE, from, from + block);
                    for p in &mut producers {
                        push(p, RATE, from, &samples);
                    }
                    let start = Instant::now();
                    mixer.mix(frame as i64, &mut out).expect("right length");
                    took += start.elapsed();
                    black_box(&out);
                    frame += block;
                }
                took
            });
        });
    }
    group.finish();
}

/// Frames of `rate` that cover engine blocks `[0, k)`, rounded down.
fn frames_before(rate: u32, k: u64) -> u64 {
    k * BLOCK as u64 * u64::from(rate) / u64::from(RATE)
}

fn input_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_push");
    for rate in [RATE, 44_100] {
        group.bench_function(BenchmarkId::new("stereo_block", rate), |b| {
            let mut mixer = mixer();
            let (mut producer, input) = mixer.add_input(format(rate)).expect("room for an input");
            let mut out = vec![0.0; 2 * BLOCK];
            let block = BLOCK as u64;
            // Engine block `k` mixes engine frames `[k * BLOCK, (k + 1) * BLOCK)`; the
            // device packet pushed before it covers the same stretch of time at `rate`,
            // `LEAD` blocks ahead.
            let mut k = 4;
            let lead_to = frames_before(rate, k + LEAD);
            push(&mut producer, rate, 0, &tone(rate, 0, lead_to));
            b.iter_custom(|iters| {
                let mut took = Duration::ZERO;
                for _ in 0..iters {
                    let from = frames_before(rate, k + LEAD);
                    let to = frames_before(rate, k + LEAD + 1);
                    let samples = tone(rate, from, to);
                    let start = Instant::now();
                    push(&mut producer, rate, from, &samples);
                    took += start.elapsed();
                    mixer
                        .mix((k * block) as i64, &mut out)
                        .expect("right length");
                    black_box(&out);
                    k += 1;
                }
                took
            });
            drop(input);
        });
    }
    group.finish();
}

// `criterion_group!` would generate an undocumented `pub fn` (`missing_docs`).
fn benches() {
    let mut c = Criterion::default().configure_from_args();
    mix_block(&mut c);
    input_push(&mut c);
}

criterion_main!(benches);
