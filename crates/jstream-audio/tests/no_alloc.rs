//! R-RT-001: in steady state, the audio callback and the mixer do not allocate.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator. It
//! counts every allocation and free made on a thread inside an `assert_no_alloc` scope,
//! in debug and release builds alike. Its own binary, so no other test runs under it.
//!
//! The simulation sets everything up, then runs warm-up seconds with no checks (the
//! drift fit fills, the controller settles). After that, each simulated 10 ms tick does
//! what the real-time threads do inside a checked scope: every input's audio callback
//! (`AudioInputHandler::on_packet`), then `Mixer::poll` until no block is due, then the
//! filter kernels and meters on each mixed block, as the mixer thread will run them
//! (ADR-0028). Control-plane work (adding and removing inputs, gain and mute changes)
//! runs between ticks, outside the scope, as it does on the control thread.
//!
//! The checked phase walks through the paths a session hits besides plain steady state:
//! drift, jitter, timestamp outliers and missing stamps, lost packets, a discontinuity,
//! a stalled audio thread that catches up, a device gap long enough to reset the
//! mapping, silent and non-finite packets, a stalled mixer (overrun and skipped
//! frames), and an input added and another removed while the mixer runs. The test
//! asserts afterwards that each path actually ran.
//!
//! Dropping an input's producer frees its resampler state, so a producer is dropped
//! outside the checked scope: that is stream teardown, not steady state.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use jstream_audio::filter::{
    AudioFilter, Compressor, CompressorParams, Delay, Gain, GateParams, Limiter, LimiterParams,
    NoiseGate,
};
use jstream_audio::meter::{LoudnessMeter, TruePeakMeter};
use jstream_audio::{InputControl, InputProducer, Mixer, MixerConfig};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
use jstream_platform::{AudioInputHandler, AudioPacket, AudioPacketFlags};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// The mixer's rate.
const ENGINE: u32 = 48_000;
/// One simulated tick: a device period and a mixer wake-up.
const TICK_S: f64 = 0.01;
/// Media clock time at the start (a media clock counts from boot).
const T0: f64 = 1_000.0;
/// Ticks run before checking starts.
const WARMUP: u64 = 1_500;
/// Ticks run under the check.
const CHECKED: u64 = 3_000;

/// Media clock time `s` seconds, in 100 ns ticks like QPC.
fn clock(s: f64) -> MediaTime {
    MediaTime::new(
        (s * 1e7).round() as i64,
        Timebase::new(1, 10_000_000).expect("valid timebase"),
    )
}

/// A deterministic PRNG (xorshift64*) giving uniform values in `[-1, 1)`.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// What a device does differently on one tick.
#[derive(Clone, Copy, PartialEq)]
enum Event {
    /// The packet is lost; its device position still advances.
    Lose,
    /// The packet is flagged as discontinuous.
    Discontinuity,
    /// The packet's stamp is 50 ms off the clock line.
    Outlier,
    /// The packet has no usable stamp.
    NoStamp,
    /// The packet is flagged silent.
    Silent,
    /// The packet's samples are NaN and infinities.
    NonFinite,
    /// The audio thread stalls: this tick's packet and the next ones wait.
    Stall,
    /// The audio thread catches up: every packet that waited is delivered now.
    CatchUp,
    /// The device stops for 3 s (beyond the 2 s that resets the mapping), its
    /// position advancing, then resumes.
    LongGap,
}

/// A simulated capture device on its own audio thread.
struct Device {
    producer: InputProducer,
    control: InputControl,
    channels: usize,
    rate: u32,
    /// The device's true rate against the media clock.
    true_rate: f64,
    jitter_s: f64,
    /// The next frame's device position.
    position: u64,
    /// Frames per packet.
    packet: usize,
    /// The packets the stalled audio thread has not delivered yet.
    pending: usize,
    /// The ticks the device is silent for, after a long gap starts.
    gap_ticks: u64,
    noise: Noise,
    phase: f64,
    /// Room for a backlog of packets, filled in place.
    buf: Vec<f32>,
    events: &'static [(u64, Event)],
}

/// The most packets a stalled audio thread holds back.
const MAX_BACKLOG: usize = 16;

impl Device {
    fn new(
        (producer, control): (InputProducer, InputControl),
        layout: ChannelLayout,
        rate: u32,
        ppm: f64,
        seed: u64,
        events: &'static [(u64, Event)],
    ) -> Self {
        let channels = layout.channel_count();
        let packet = rate as usize / 100;
        Self {
            producer,
            control,
            channels,
            rate,
            true_rate: f64::from(rate) * (1.0 + ppm * 1e-6),
            jitter_s: 0.000_5,
            position: 0,
            packet,
            pending: 0,
            gap_ticks: 0,
            noise: Noise(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            phase: 0.0,
            buf: vec![0.0; packet * channels * MAX_BACKLOG],
            events,
        }
    }

    fn event(&self, tick: u64) -> Option<Event> {
        self.events
            .iter()
            .find(|&&(t, _)| t == tick)
            .map(|&(_, e)| e)
    }

    /// The audio thread's work for one tick: the callbacks for the packets due by
    /// then. Real-time code only.
    fn run(&mut self, tick: u64) {
        let event = self.event(tick);
        if event == Some(Event::LongGap) {
            self.gap_ticks = 300;
        }
        if self.gap_ticks > 0 {
            self.gap_ticks -= 1;
            self.position += self.packet as u64;
            return;
        }
        self.pending += 1;
        if event == Some(Event::Stall) {
            return;
        }
        let n = self.pending.min(MAX_BACKLOG);
        self.pending = 0;
        for _ in 0..n {
            self.deliver(event);
        }
    }

    fn deliver(&mut self, event: Option<Event>) {
        let samples = self.packet * self.channels;
        let first = self.position;
        self.position += self.packet as u64;
        if event == Some(Event::Lose) {
            return;
        }
        let step = std::f64::consts::TAU * 440.0 / f64::from(self.rate);
        for frame in self.buf[..samples].chunks_exact_mut(self.channels) {
            let s = (0.25 * self.phase.sin()) as f32;
            self.phase = (self.phase + step) % std::f64::consts::TAU;
            frame.fill(s);
        }
        if event == Some(Event::NonFinite) {
            self.buf[0] = f32::NAN;
            self.buf[1] = f32::INFINITY;
            self.buf[2] = f32::NEG_INFINITY;
        }
        let stamp = T0 + first as f64 / self.true_rate + self.jitter_s * self.noise.next();
        let time = match event {
            Some(Event::NoStamp) => None,
            Some(Event::Outlier) => Some(clock(stamp + 0.05)),
            _ => Some(clock(stamp)),
        };
        self.producer.on_packet(&AudioPacket {
            samples: &self.buf[..samples],
            device_position: first,
            time,
            flags: AudioPacketFlags {
                discontinuity: event == Some(Event::Discontinuity),
                silent: event == Some(Event::Silent),
            },
        });
    }
}

/// The filter chain and meters the mixer thread will run on the bus.
struct BusChain {
    filters: [Box<dyn AudioFilter>; 5],
    key: Vec<f32>,
    true_peak: TruePeakMeter,
    loudness: LoudnessMeter,
}

impl BusChain {
    fn new() -> Self {
        let rate = ENGINE;
        let mut delay = Delay::new(rate, 100.0).expect("valid delay");
        delay.set_delay_ms(20.0).expect("in range");
        Self {
            filters: [
                Box::new(Gain::new(rate, -3.0).expect("valid gain")),
                Box::new(NoiseGate::new(rate, GateParams::default()).expect("valid gate")),
                Box::new(
                    Compressor::new(rate, CompressorParams::default())
                        .expect("valid compressor")
                        .with_key(true),
                ),
                Box::new(delay),
                Box::new(Limiter::new(rate, 5.0, LimiterParams::default()).expect("valid limiter")),
            ],
            key: vec![0.0; 2 * MixerConfig::default().block_frames],
            true_peak: TruePeakMeter::new(),
            loudness: LoudnessMeter::new(rate).expect("valid rate"),
        }
    }

    /// The mixer thread's work on one block. Real-time code only.
    fn process(&mut self, block: &mut [f32]) {
        self.key.copy_from_slice(block);
        for f in &mut self.filters {
            if f.wants_key() {
                f.process_keyed(block, &self.key);
            } else {
                f.process(block);
            }
        }
        self.true_peak.process(block);
        self.loudness.process(block);
        let _ = (
            self.true_peak.take_peak(),
            self.loudness.momentary(),
            self.loudness.short_term(),
            self.loudness.integrated(),
        );
    }
}

struct Sim {
    mixer: Mixer,
    devices: Vec<Device>,
    removed: Vec<Device>,
    out: Vec<f32>,
    chain: BusChain,
    /// The mixer thread does not run until this tick (a stall).
    mixer_stalled_until: u64,
}

impl Sim {
    /// The real-time threads' work for one tick.
    fn run_rt(&mut self, tick: u64) {
        for d in &mut self.devices {
            d.run(tick);
        }
        if tick < self.mixer_stalled_until {
            return;
        }
        let now = clock(T0 + (tick + 1) as f64 * TICK_S);
        while self
            .mixer
            .poll(now, &mut self.out)
            .expect("right length")
            .is_some()
        {
            self.chain.process(&mut self.out);
        }
    }
}

// R-RT-001 names the test `jstream_audio::rt::callback_does_not_allocate`.
mod rt {
    use super::*;

    #[test]
    fn callback_does_not_allocate() {
        let mut mixer = Mixer::new(MixerConfig::default()).expect("valid config");
        let fmt = |rate, layout| {
            AudioFormat::new(rate, layout, SampleFormat::F32Interleaved).expect("valid format")
        };
        // Ticks count from the start of the run; checking starts at WARMUP (1500).
        const MIC: &[(u64, Event)] = &[
            (1_600, Event::Outlier),
            (1_650, Event::NoStamp),
            (1_700, Event::Lose),
            (1_701, Event::Lose),
            (1_800, Event::Discontinuity),
            (2_000, Event::Stall),
            (2_001, Event::Stall),
            (2_002, Event::Stall),
            (2_003, Event::Stall),
            (2_004, Event::Stall),
            (2_005, Event::CatchUp),
        ];
        const LOOPBACK: &[(u64, Event)] = &[
            (1_600, Event::Silent),
            (1_610, Event::NonFinite),
            (2_200, Event::LongGap),
        ];
        let mic = mixer
            .add_input(fmt(44_100, ChannelLayout::Mono))
            .expect("supported");
        let loopback = mixer
            .add_input(fmt(48_000, ChannelLayout::Stereo))
            .expect("supported");
        let surround = mixer
            .add_input(fmt(48_000, ChannelLayout::Surround51))
            .expect("supported");
        let hires = mixer
            .add_input(fmt(96_000, ChannelLayout::Stereo))
            .expect("supported");
        let mut sim = Sim {
            out: vec![0.0; 2 * mixer.config().block_frames],
            mixer,
            devices: vec![
                Device::new(mic, ChannelLayout::Mono, 44_100, 120.0, 1, MIC),
                Device::new(loopback, ChannelLayout::Stereo, 48_000, -80.0, 2, LOOPBACK),
                Device::new(surround, ChannelLayout::Surround51, 48_000, 40.0, 3, &[]),
                Device::new(hires, ChannelLayout::Stereo, 96_000, -200.0, 4, &[]),
            ],
            removed: Vec::new(),
            chain: BusChain::new(),
            mixer_stalled_until: 0,
        };
        let control = sim.mixer.control();
        let bus = sim.mixer.bus();

        reset_violation_count();
        let mut violations = Vec::new();
        for tick in 0..WARMUP + CHECKED {
            // The control thread, between ticks.
            match tick {
                1_900 => {
                    // Longer than a ring (341 ms): the inputs overrun, the mixer skips.
                    sim.mixer_stalled_until = tick + 40;
                }
                2_100 => {
                    let added = control
                        .add_input(fmt(32_000, ChannelLayout::Stereo))
                        .expect("supported");
                    sim.devices.push(Device::new(
                        added,
                        ChannelLayout::Stereo,
                        32_000,
                        60.0,
                        5,
                        &[],
                    ));
                }
                2_400 => {
                    let gone = sim.devices.remove(2);
                    assert!(gone.control.remove(), "removal queued");
                    sim.removed.push(gone);
                }
                _ => {}
            }
            if tick % 100 == 0 {
                let db = if tick % 200 == 0 { -6.0 } else { 0.0 };
                sim.devices[0].control.set_gain_db(db);
                sim.devices[1].control.set_muted(tick % 300 == 0);
                bus.set_gain_db(db / 2.0);
                for d in &sim.devices {
                    let _ = (d.control.take_meter(), d.control.stats());
                }
                let _ = (bus.take_meter(), bus.stats());
            }

            if tick < WARMUP {
                sim.run_rt(tick);
            } else {
                let before = violation_count();
                assert_no_alloc(|| sim.run_rt(tick));
                if violation_count() > before {
                    violations.push(tick);
                }
            }
        }
        assert!(
            violations.is_empty(),
            "the real-time path allocated or freed on {} ticks, first {:?}",
            violations.len(),
            &violations[..violations.len().min(10)]
        );

        // Every path above ran.
        let mic = sim.devices[0].control.stats();
        let loopback = sim.devices[1].control.stats();
        let added = sim.devices[3].control.stats();
        let removed = sim.removed[0].control.stats();
        let bus = bus.stats();
        assert!(mic.outliers > 0, "{mic:?}");
        assert!(mic.timestamps_missing > 0, "{mic:?}");
        assert!(mic.gap_frames > 0, "{mic:?}");
        assert!(mic.discontinuities > 0, "{mic:?}");
        assert!(mic.reanchors > 0, "{mic:?}");
        assert!(mic.overrun_frames > 0, "{mic:?}");
        assert!(mic.drift_ppm.is_some(), "{mic:?}");
        assert!(loopback.resets > 0, "{loopback:?}");
        assert!(loopback.underrun_frames > 0, "{loopback:?}");
        assert!(
            added.packets > 0 && added.timing_error_s.is_some(),
            "{added:?}"
        );
        assert!(removed.packets > 0, "{removed:?}");
        assert!(bus.skipped_frames > 0, "{bus:?}");
        // 45 s of 1024-frame blocks at 48 kHz, minus the stall's.
        assert!(bus.blocks > 2_000, "{bus:?}");
    }
}

/// The allocator is installed and an allocation in a checked scope is counted, so the
/// test above can fail.
#[test]
fn an_allocation_in_a_checked_scope_is_counted() {
    let before = violation_count();
    let v = assert_no_alloc(|| std::hint::black_box(vec![0u8; 64]));
    assert!(violation_count() > before);
    drop(v);
}
