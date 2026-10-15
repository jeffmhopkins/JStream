//! R-RT-005: what one mixed block costs on the mixer thread, for the send matrix and the
//! filter chains (#228 N3). Not a pass/fail test: it is ignored by default and prints
//! its numbers. Run it in release, on the machine the numbers are for:
//!
//! ```text
//! cargo test --release -p jstream-audio --test cost -- --ignored --nocapture
//! ```
//!
//! Each scenario feeds every input a 48 kHz stereo tone ahead of the mixer, then times
//! `Mixer::mix` alone over a few seconds of blocks: the inputs' audio callbacks
//! (resampling) are not counted. A block is 1024 frames, 21.3 ms of audio.

use std::io::Write;
use std::time::{Duration, Instant};

use jstream_audio::filter::{CompressorParams, GateParams, LimiterParams};
use jstream_audio::{FilterSpec, InputProducer, Mixer, MixerConfig};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
use jstream_platform::{AudioPacket, AudioPacketFlags};

const RATE: u32 = 48_000;
const BLOCK: usize = 1024;
/// Blocks timed per scenario after the warm-up: 20 s of audio.
const TIMED: usize = 940;
const WARMUP: usize = 50;
const INPUTS: usize = 8;
/// Buses besides the master.
const AUX: usize = 7;

struct Scenario {
    name: &'static str,
    /// Every input sends to every aux bus.
    sends: bool,
    /// Gate and compressor per input, compressor and 5 ms limiter per bus, a gain and
    /// 3 ms limiter master chain, and an output tap per aux bus with a gain and 5 ms
    /// limiter of its own.
    chains: bool,
}

fn limiter(lookahead_ms: f32) -> FilterSpec {
    FilterSpec::Limiter {
        lookahead_ms,
        params: LimiterParams::default(),
    }
}

fn build(scenario: &Scenario) -> (Mixer, Vec<InputProducer>) {
    let config = MixerConfig::default();
    assert_eq!(config.block_frames, BLOCK);
    let mut mixer = Mixer::new(config).expect("valid config");
    let format = AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("format");
    let mut producers = Vec::new();
    let mut inputs = Vec::new();
    for _ in 0..INPUTS {
        let (producer, input) = mixer.add_input(format).expect("room for an input");
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

/// Pushes `[from, to)` frames of a tone to every producer, stamped exactly.
fn feed(producers: &mut [InputProducer], from: u64, to: u64) {
    let tb = Timebase::per_second(RATE).expect("rate");
    let samples: Vec<f32> = (from..to)
        .flat_map(|n| {
            let s =
                (0.3 * (std::f64::consts::TAU * 440.0 * n as f64 / f64::from(RATE)).sin()) as f32;
            [s, s]
        })
        .collect();
    for p in producers.iter_mut() {
        p.push_packet(&AudioPacket {
            samples: &samples,
            device_position: from,
            time: Some(MediaTime::new(from as i64, tb)),
            flags: AudioPacketFlags::default(),
        });
    }
}

fn measure(scenario: &Scenario) -> (Duration, Duration, Duration) {
    let (mut mixer, mut producers) = build(scenario);
    let mut out = vec![0.0; 2 * BLOCK];
    let block = BLOCK as u64;
    // Start a few blocks in, with audio fed ahead of each block mixed.
    let first = 4 * block;
    feed(&mut producers, 0, first + 2 * block);
    let mut times = Vec::with_capacity(TIMED);
    for k in 0..(WARMUP + TIMED) as u64 {
        let frame = first + k * block;
        feed(&mut producers, frame + 2 * block, frame + 3 * block);
        let start = Instant::now();
        mixer.mix(frame as i64, &mut out).expect("right length");
        let took = start.elapsed();
        std::hint::black_box(&out);
        if k >= WARMUP as u64 {
            times.push(took);
        }
    }
    times.sort_unstable();
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    let p99 = times[times.len() * 99 / 100];
    let max = times[times.len() - 1];
    (mean, p99, max)
}

#[test]
#[ignore = "a measurement: run in release with --ignored --nocapture"]
fn mix_cost_per_block() {
    let scenarios = [
        Scenario {
            name: "8 inputs, master only",
            sends: false,
            chains: false,
        },
        Scenario {
            name: "8 inputs, 8 buses, full send matrix",
            sends: true,
            chains: false,
        },
        Scenario {
            name: "send matrix + input, bus, master and 7 output-tap chains",
            sends: true,
            chains: true,
        },
    ];
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    // Straight to stdout: a measurement's report, not test output to capture.
    let mut report = std::io::stdout().lock();
    writeln!(
        report,
        "mixer cost per {BLOCK}-frame block ({period:.1?} of audio):"
    )
    .expect("stdout");
    for scenario in &scenarios {
        let (mean, p99, max) = measure(scenario);
        writeln!(
            report,
            "  {:<58} mean {:>9.1?}  p99 {:>9.1?}  max {:>9.1?}  ({:.2}% of the period, mean)",
            scenario.name,
            mean,
            p99,
            max,
            100.0 * mean.as_secs_f64() / period.as_secs_f64(),
        )
        .expect("stdout");
    }
}
