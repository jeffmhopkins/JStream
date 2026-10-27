//! R-RT-001: in steady state, the audio callback and the mixer do not allocate.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator. It
//! counts every allocation and free made on a thread inside an `assert_no_alloc` scope,
//! in debug and release builds alike. Its own binary, so no other test runs under it.
//!
//! The simulation sets everything up, then runs warm-up seconds with no checks (the
//! drift fit fills, the controller settles). After that, each simulated 10 ms tick does
//! what the real-time threads do inside a checked scope: every input's audio callback
//! (`AudioInputHandler::on_packet`), then `Mixer::poll` until no block is due (which runs
//! the input, bus, master and output-tap filter chains) and a read of each output tap's
//! block and of each input's own block (a drawn source's scope, #459), then a keyed
//! compressor and the meters on each mixed block, as the mixer thread will run them
//! (ADR-0028; sidechain keys and meters aren't in the mixer yet). Two bus monitors take
//! each block, one before the master's filters and one after, and their devices' audio
//! threads (`AudioOutputHandler::render`, on the socket each player is plugged into)
//! play a tick's worth each tick.
//! Control-plane work (adding and removing inputs, buses, output taps, sends and
//! filters, filter edits, gain and mute changes) runs between ticks, outside the scope,
//! as it does on the control thread.
//!
//! The checked phase walks through the paths a session hits besides plain steady state:
//! drift, jitter, timestamp outliers and missing stamps, lost packets, a discontinuity
//! (and one whose stamp is off the clock line),
//! a stalled audio thread that catches up, a device gap long enough to reset the
//! mapping, silent and non-finite packets, a stalled mixer (overrun and skipped
//! frames), an input added and another removed while the mixer runs, a bus added, sent
//! to, filtered and removed, filters added, edited and removed (changing the chains'
//! latency, so the compensation delays move), master filters added to every bus's copy,
//! edited and removed, output taps added (one with its own chain, on a bus that is
//! later removed) and removed, and a CLAP filter whose host keeps up, then goes missing
//! and comes back (its blocks bypassed and crossfaded), then is removed. The test
//! asserts afterwards that each path actually ran.
//!
//! Dropping an input's producer frees its resampler state, so a producer is dropped
//! outside the checked scope: that is stream teardown, not steady state.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use jstream_audio::filter::{AudioFilter, Compressor, CompressorParams, GateParams, LimiterParams};
use jstream_audio::meter::{LoudnessMeter, TruePeakMeter};
use jstream_audio::{
    BusId, FilterParams, FilterSpec, HostMemory, InputControl, InputProducer, Mixer, MixerConfig,
    Monitor, MonitorControl, MonitorFeed, MonitorSocket, MonitorTap, OutputTapId, monitor_socket,
};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
use jstream_platform::{
    AudioInputHandler, AudioOutputHandler, AudioPacket, AudioPacketFlags, AudioRenderBuffer,
};
use jstream_plugin_proto::{Config, DEFAULT_DEPTH, EngineBlock, Shared, first_useful};
use std::sync::Arc;

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
    /// The packet is flagged as discontinuous and its stamp is 50 ms off the clock line,
    /// so the drift fit's segment restarts with it at once.
    DiscontinuityOffLine,
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
            Some(Event::Outlier | Event::DiscontinuityOffLine) => Some(clock(stamp + 0.05)),
            _ => Some(clock(stamp)),
        };
        self.producer.on_packet(&AudioPacket {
            samples: &self.buf[..samples],
            device_position: first,
            time,
            flags: AudioPacketFlags {
                discontinuity: matches!(
                    event,
                    Some(Event::Discontinuity | Event::DiscontinuityOffLine)
                ),
                silent: event == Some(Event::Silent),
            },
        });
    }
}

/// What the mixer thread will also run on the bus once sidechain keys and meters are in
/// the mixer: a keyed compressor and the meters.
struct BusChain {
    keyed: Compressor,
    key: Vec<f32>,
    true_peak: TruePeakMeter,
    loudness: LoudnessMeter,
}

impl BusChain {
    fn new() -> Self {
        let rate = ENGINE;
        Self {
            keyed: Compressor::new(rate, CompressorParams::default())
                .expect("valid compressor")
                .with_key(true),
            key: vec![0.0; 2 * MixerConfig::default().block_frames],
            true_peak: TruePeakMeter::new(),
            loudness: LoudnessMeter::new(rate).expect("valid rate"),
        }
    }

    /// The mixer thread's work on one block. Real-time code only.
    fn process(&mut self, block: &mut [f32]) {
        self.key.copy_from_slice(block);
        self.keyed.process_keyed(block, &self.key);
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

/// A CLAP plugin host, as its process would run it between the mixer's blocks: each
/// step processes every input block the mixer can still read (a gain of 0.5).
struct FakeHost {
    shared: Arc<Shared>,
    /// The engine's side of the block, where the mixer counts its misses.
    memory: HostMemory,
    next: u64,
    buf: Vec<f32>,
    processed: u64,
}

impl FakeHost {
    fn step(&mut self) {
        let head = self.shared.input.head();
        self.next = self.next.max(first_useful(head, DEFAULT_DEPTH));
        while self.next < head {
            if self.shared.input.read(self.next, &mut self.buf) {
                self.buf.iter_mut().for_each(|s| *s *= 0.5);
                self.shared.output.write(self.next, &self.buf);
                self.processed += 1;
            }
            self.next += 1;
        }
    }
}

struct Sim {
    mixer: Mixer,
    devices: Vec<Device>,
    removed: Vec<Device>,
    out: Vec<f32>,
    chain: BusChain,
    /// The output taps an encoder reads after each block, and how many blocks it read.
    taps: Vec<OutputTapId>,
    tap_blocks: u64,
    /// The input blocks a scope read after each block (#459), as the mixer thread
    /// reads a drawn source's.
    input_blocks: u64,
    /// The mixer thread does not run until this tick (a stall).
    mixer_stalled_until: u64,
    /// Bus monitors (ADR-0028, D22): each one's feed and counters, the socket its
    /// device stream plays (its player plugged in), its tap, and its device's buffer
    /// for one tick, which the socket fills every tick.
    monitors: Vec<(
        MonitorFeed,
        MonitorControl,
        MonitorSocket,
        MonitorTap,
        Vec<f32>,
    )>,
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
        while let Some(mixed) = self.mixer.poll(now, &mut self.out).expect("right length") {
            self.chain.process(&mut self.out);
            for (feed, _, _, tap, _) in &mut self.monitors {
                if let Some(block) = self.mixer.monitor_block(BusId::MASTER, *tap) {
                    feed.push(mixed.frame, block);
                }
            }
            for &tap in &self.taps {
                if let Some((_, block)) = self.mixer.output_block(tap) {
                    std::hint::black_box(block);
                    self.tap_blocks += 1;
                }
            }
            for d in &self.devices {
                if let Some(block) = self.mixer.input_block(d.control.id()) {
                    std::hint::black_box(block);
                    self.input_blocks += 1;
                }
            }
        }
        // Each monitor device's audio thread, once a tick.
        for (_, _, socket, _, buf) in &mut self.monitors {
            socket.render(&mut AudioRenderBuffer {
                samples: buf,
                device_position: tick,
                time: None,
            });
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
            (1_900, Event::DiscontinuityOffLine),
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
            taps: Vec::new(),
            tap_blocks: 0,
            input_blocks: 0,
            mixer_stalled_until: 0,
            monitors: Vec::new(),
        };
        // Monitors on the master, before and after its filters, on a 44.1 kHz mono
        // device and a 48 kHz 5.1 one.
        for (rate, layout, tap) in [
            (44_100, ChannelLayout::Mono, MonitorTap::PreFilters),
            (48_000, ChannelLayout::Surround51, MonitorTap::PostFilters),
        ] {
            let format = fmt(rate, layout);
            let Monitor {
                feed,
                player,
                control,
            } = sim.mixer.control().add_monitor(format).expect("supported");
            let (socket, plug) = monitor_socket();
            assert!(plug.plug(player));
            let buf = vec![0.0; rate as usize / 100 * format.channels()];
            sim.monitors.push((feed, control, socket, tap, buf));
        }
        let control = sim.mixer.control();
        let bus = sim.mixer.bus();
        let limiter = |lookahead_ms| FilterSpec::Limiter {
            lookahead_ms,
            params: LimiterParams::default(),
        };
        // Filter chains on an input and on the master, built on the control thread.
        let mic_gate = sim.devices[0]
            .control
            .add_filter(FilterSpec::NoiseGate(GateParams::default()))
            .expect("valid gate");
        sim.devices[0]
            .control
            .add_filter(FilterSpec::Compressor(CompressorParams::default()))
            .expect("valid compressor");
        let master_gain = bus
            .add_filter(FilterSpec::Gain { gain_db: -3.0 })
            .expect("valid gain");
        let master_delay = bus
            .add_filter(FilterSpec::Delay {
                max_ms: 100.0,
                delay_ms: 20.0,
            })
            .expect("valid delay");
        bus.add_filter(limiter(5.0)).expect("valid limiter");
        // The master chain, one copy per bus that feeds an output.
        let master = sim.mixer.master();
        let master_gain_db = master
            .add_filter(FilterSpec::Gain { gain_db: -1.0 })
            .expect("valid gain");
        master.add_filter(limiter(3.0)).expect("valid limiter");
        let mut master_comp = None;
        let mut aux_tap = None;
        let mut tap_gain = None;
        let mut master_tap = None;
        let mut aux = None;
        let mut aux_limiter = None;
        let mut extra = None;
        let mut clap = None;
        let mut host: Option<FakeHost> = None;
        let mut latencies = std::collections::BTreeSet::new();

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
                // A bus, sent to and filtered while the mixer runs: the bus chains'
                // latency changes, so every bus's compensation delay moves.
                1_550 => {
                    let added = control.add_bus().expect("room for a bus");
                    sim.devices[1].control.set_send(added.id(), 0.5);
                    sim.devices[0].control.set_send_db(added.id(), -6.0);
                    aux_limiter = Some(added.add_filter(limiter(2.0)).expect("valid limiter"));
                    aux = Some(added);
                }
                // A filter with latency on an input: every other input is delayed to it.
                2_300 => {
                    extra = Some(
                        sim.devices[1]
                            .control
                            .insert_filter(0, limiter(10.0))
                            .expect("valid limiter"),
                    );
                }
                2_500 => assert!(extra.take().expect("added").remove(), "removal queued"),
                // A CLAP filter on the loopback input, its host ready.
                1_580 => {
                    let block = sim.mixer.config().block_frames;
                    let shared: Arc<Shared> = Arc::from(Shared::new_boxed());
                    shared
                        .init(Config {
                            sample_rate: ENGINE,
                            block_frames: block as u32,
                            depth: DEFAULT_DEPTH,
                        })
                        .expect("valid config");
                    shared.publish_ready(0);
                    let memory: HostMemory = Arc::new(EngineBlock::new(Arc::clone(&shared)));
                    clap = Some(
                        sim.devices[1]
                            .control
                            .insert_host_filter(usize::MAX, Arc::clone(&memory))
                            .expect("a ready host"),
                    );
                    host = Some(FakeHost {
                        shared,
                        memory,
                        next: 0,
                        buf: vec![0.0; 2 * block],
                        processed: 0,
                    });
                }
                2_450 => assert!(clap.take().expect("added").remove(), "removal queued"),
                2_600 => assert!(
                    aux_limiter.take().expect("added").remove(),
                    "removal queued"
                ),
                2_700 => assert!(aux.as_ref().expect("added").remove(), "removal queued"),
                // An output tap on the aux bus, with its own chain: the master chain now
                // runs on the aux bus too. The tap outlives its bus and reads silence.
                1_560 => {
                    let tap = control
                        .add_output_tap(aux.as_ref().expect("added").id())
                        .expect("room for a tap");
                    tap_gain = Some(
                        tap.add_filter(FilterSpec::Gain { gain_db: -2.0 })
                            .expect("valid gain"),
                    );
                    tap.add_filter(limiter(4.0)).expect("valid limiter");
                    sim.taps.push(tap.id());
                    aux_tap = Some(tap);
                }
                // A master filter added while two buses feed outputs: one copy each.
                1_575 => {
                    master_comp = Some(
                        master
                            .insert_filter(0, FilterSpec::Compressor(CompressorParams::default()))
                            .expect("valid compressor"),
                    );
                }
                2_550 => {
                    let tap = control.add_output_tap(bus.id()).expect("room for a tap");
                    sim.taps.push(tap.id());
                    master_tap = Some(tap);
                }
                2_620 => assert!(
                    master_comp.take().expect("added").remove(),
                    "removal queued"
                ),
                2_650 => assert!(master_tap.take().expect("added").remove(), "removal queued"),
                _ => {}
            }
            if tick % 50 == 0 {
                let open = if tick % 100 == 0 { -30.0 } else { -40.0 };
                mic_gate
                    .set(FilterParams::NoiseGate(GateParams {
                        open_threshold_db: open,
                        close_threshold_db: open - 6.0,
                        ..GateParams::default()
                    }))
                    .expect("valid gate");
                master_gain
                    .set(FilterParams::Gain {
                        gain_db: -3.0 - (tick % 200) as f32 / 100.0,
                    })
                    .expect("valid gain");
                master_delay
                    .set(FilterParams::Delay {
                        delay_ms: if tick % 100 == 0 { 20.0 } else { 0.0 },
                    })
                    .expect("valid delay");
                master_gain_db
                    .set(FilterParams::Gain {
                        gain_db: if tick % 100 == 0 { -1.0 } else { -2.0 },
                    })
                    .expect("valid gain");
                if let Some(comp) = &master_comp {
                    comp.set(FilterParams::Compressor(CompressorParams {
                        threshold_db: if tick % 100 == 0 { -18.0 } else { -24.0 },
                        ..CompressorParams::default()
                    }))
                    .expect("valid compressor");
                }
                if let Some(gain) = &tap_gain {
                    gain.set(FilterParams::Gain {
                        gain_db: -2.0 - (tick % 200) as f32 / 100.0,
                    })
                    .expect("valid gain");
                }
            }
            latencies.insert(sim.mixer.latency_frames());
            if tick % 100 == 0 {
                let db = if tick % 200 == 0 { -6.0 } else { 0.0 };
                sim.devices[0].control.set_gain_db(db);
                sim.devices[1].control.set_muted(tick % 300 == 0);
                // A scene switch moves activity gains (ADR-0028's `when_shown`).
                sim.devices[0]
                    .control
                    .set_activity_gain(if tick % 400 == 0 { 0.0 } else { 1.0 });
                bus.set_gain_db(db / 2.0);
                for d in &sim.devices {
                    let _ = (d.control.take_meter(), d.control.stats());
                }
                let _ = (bus.take_meter(), bus.stats());
            }

            // The CLAP host's process: it keeps up, except for 0.5 s in every 2 s.
            if let Some(host) = &mut host
                && (tick / 50) % 4 != 3
            {
                host.step();
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
        // Only the off-line discontinuity restarts the mic's mapping.
        assert!(mic.resets > 0, "{mic:?}");
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
        let host = host.expect("added");
        let misses = host
            .memory
            .misses()
            .total
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            host.processed > 100 && misses > 10,
            "{} {misses}",
            host.processed
        );
        let aux = aux.expect("added").stats();
        // 11.5 s of blocks, minus the stall's.
        assert!(aux.blocks > 400, "{aux:?}");
        // No latency, the master limiter's, plus the aux limiter's, plus the input one's.
        assert!(latencies.len() >= 3, "{latencies:?}");
        // The aux tap from 15.6 s to the end (its bus removed at 27 s), the master
        // tap for a second; minus the stall's.
        assert!(sim.tap_blocks > 1_300, "{}", sim.tap_blocks);
        assert!(aux_tap.expect("added").remove(), "removal queued");
        // Every input's block, each block: at least the four inputs' for 30 s.
        assert!(sim.input_blocks > 4 * 1_300, "{}", sim.input_blocks);
        // 45 s of 1024-frame blocks at 48 kHz, minus the stall's.
        assert!(bus.blocks > 2_000, "{bus:?}");
        for (_, control, _, _, _) in &sim.monitors {
            let stats = control.stats();
            assert!(stats.blocks > 2_000 && stats.renders > 4_000, "{stats:?}");
        }
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
