//! End-to-end simulations: devices with their own rates, clock drift, timestamp jitter
//! and glitches feed the mixer, which is driven by a simulated media clock (100 ns ticks,
//! like QPC on Windows). Deterministic: the jitter comes from a seeded generator.
//!
//! Every device plays a sine defined in *real* time. If an input is resampled onto the
//! engine time line correctly, output frame `n` holds the signal at real time
//! `n / 48000`, so the sine's phase in each one-second window of the mix gives the
//! input's timing error directly, independent of the controller's own estimate.

use jstream_audio::{InputProducer, Mixer, MixerConfig};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
use jstream_platform::{AudioPacket, AudioPacketFlags};

const ENGINE: u32 = 48_000;
/// Simulated time step: the mixer thread's wake-up period.
const TICK_S: f64 = 0.001;
/// Delay between a packet's last frame and its delivery to the handler.
const DELIVERY_S: f64 = 0.001;
/// The real time the simulation starts at (a media clock counts from boot).
const T0: f64 = 1_000.0;

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

/// What to do differently with one packet.
#[derive(Clone, Copy, Default)]
struct Tweak {
    /// Lose the packet (its device position still advances).
    lose: bool,
    /// Deliver it this much later (the packets after it queue behind it, then catch
    /// up, as after a stalled audio thread).
    delay_s: f64,
    /// Add this to its timestamp.
    stamp_offset_s: f64,
    /// From this packet on, device positions restart at 0 (a stream restart),
    /// flagged as a discontinuity.
    restart: bool,
}

struct Device {
    producer: InputProducer,
    channels: usize,
    /// The device's true rate against real time.
    true_rate: f64,
    jitter_s: f64,
    freq: f64,
    amp: f64,
    /// Frames per packet.
    packet: u64,
    next: u64,
    /// Subtracted from frame indices to give device positions (after a restart).
    pos_base: u64,
    noise: Noise,
    buf: Vec<f32>,
    tweak: Box<dyn Fn(u64) -> Tweak>,
    /// When the previous packet was delivered: packets are delivered in order.
    last_due: f64,
}

/// A simulated device: its format, clock error, timestamp jitter and test tone.
struct Spec {
    rate: u32,
    layout: ChannelLayout,
    /// Clock error against real time, ppm (positive: fast).
    ppm: f64,
    /// Uniform timestamp jitter, ± seconds.
    jitter_s: f64,
    freq: f64,
    amp: f64,
    seed: u64,
}

impl Device {
    fn new(mixer: &mut Mixer, spec: Spec) -> (Self, jstream_audio::InputControl) {
        let format = AudioFormat::new(spec.rate, spec.layout, SampleFormat::F32Interleaved)
            .expect("valid format");
        let (producer, control) = mixer.add_input(format).expect("supported input");
        let channels = format.channels();
        let packet = u64::from(spec.rate / 100);
        let dev = Self {
            producer,
            channels,
            true_rate: f64::from(spec.rate) * (1.0 + spec.ppm * 1e-6),
            jitter_s: spec.jitter_s,
            freq: spec.freq,
            amp: spec.amp,
            packet,
            next: 0,
            pos_base: 0,
            noise: Noise(spec.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            buf: vec![0.0; packet as usize * channels],
            tweak: Box::new(|_| Tweak::default()),
            last_due: f64::NEG_INFINITY,
        };
        (dev, control)
    }

    fn frame_time(&self, k: u64) -> f64 {
        T0 + k as f64 / self.true_rate
    }

    /// Delivers every packet whose time has come by real time `now`.
    fn run_until(&mut self, now: f64) {
        loop {
            let i = self.next;
            let first = i * self.packet;
            let tweak = (self.tweak)(i);
            let due = (self.frame_time(first + self.packet) + DELIVERY_S + tweak.delay_s)
                .max(self.last_due);
            if due > now {
                return;
            }
            self.last_due = due;
            self.next += 1;
            if tweak.restart {
                self.pos_base = first;
            }
            if tweak.lose {
                continue;
            }
            for f in 0..self.packet {
                let t = self.frame_time(first + f);
                let s = (self.amp * (std::f64::consts::TAU * self.freq * t).sin()) as f32;
                let frame = &mut self.buf[f as usize * self.channels..][..self.channels];
                frame.fill(0.0);
                // Mono: the one channel; stereo and 5.1: front left and right.
                frame[0] = s;
                if self.channels > 1 {
                    frame[1] = s;
                }
            }
            let stamp =
                self.frame_time(first) + self.noise.next() * self.jitter_s + tweak.stamp_offset_s;
            let hns = Timebase::new(1, 10_000_000).expect("valid time base");
            self.producer.push_packet(&AudioPacket {
                samples: &self.buf,
                device_position: first - self.pos_base,
                time: Some(MediaTime::new((stamp * 1e7).round() as i64, hns)),
                flags: AudioPacketFlags {
                    discontinuity: i == 0 || tweak.restart,
                    silent: false,
                },
            });
        }
    }
}

/// The least-squares sine `a·sin(ωn) + b·cos(ωn) + c` at a known frequency: returns
/// amplitude and phase at `n = 0`.
fn fit_sine(x: &[f32], freq: f64, rate: f64) -> (f64, f64) {
    let w = std::f64::consts::TAU * freq / rate;
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (n, &s) in x.iter().enumerate() {
        let b = [(w * n as f64).sin(), (w * n as f64).cos(), 1.0];
        for i in 0..3 {
            v[i] += b[i] * f64::from(s);
            for j in 0..3 {
                m[i][j] += b[i] * b[j];
            }
        }
    }
    // Gaussian elimination (the matrix is well conditioned for whole-second windows).
    for col in 0..3 {
        let (pivot, vp) = (m[col], v[col]);
        for (mr, vr) in m.iter_mut().zip(v.iter_mut()).skip(col + 1) {
            let f = mr[col] / pivot[col];
            for (a, p) in mr.iter_mut().zip(pivot.iter()).skip(col) {
                *a -= f * p;
            }
            *vr -= f * vp;
        }
    }
    let mut sol = [0.0; 3];
    for row in (0..3).rev() {
        let mut s = v[row];
        for k in row + 1..3 {
            s -= m[row][k] * sol[k];
        }
        sol[row] = s / m[row][row];
    }
    (sol[0].hypot(sol[1]), sol[1].atan2(sol[0]))
}

/// One second of mix output, analysed at one tone's frequency.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// Real (engine) time of the window's start, seconds since `T0`.
    t: f64,
    amplitude: f64,
    /// How late the tone sits on the engine time line, seconds.
    late_s: f64,
}

/// Runs the mixer and devices for `secs` of simulated time and analyses each whole
/// second of output at each of `freqs`.
fn run(mixer: &mut Mixer, devices: &mut [Device], secs: f64, freqs: &[f64]) -> Vec<Vec<Window>> {
    let rate = f64::from(ENGINE);
    let hns = Timebase::new(1, 10_000_000).expect("valid time base");
    let mut block = vec![0.0f32; 2 * mixer.config().block_frames];
    let mut second: Vec<f32> = Vec::with_capacity(ENGINE as usize);
    let mut second_start: Option<i64> = None;
    let mut windows = vec![Vec::new(); freqs.len()];
    let steps = (secs / TICK_S) as u64;
    for step in 0..=steps {
        let now = T0 + step as f64 * TICK_S;
        for d in devices.iter_mut() {
            d.run_until(now);
        }
        let clock = MediaTime::new((now * 1e7).round() as i64, hns);
        while let Some(b) = mixer.poll(clock, &mut block).expect("block-sized buffer") {
            for (i, f) in block.chunks_exact(2).enumerate() {
                let n = b.frame + i as i64;
                if n % i64::from(ENGINE) == 0 {
                    if let Some(start) = second_start
                        && second.len() == ENGINE as usize
                    {
                        for (k, &freq) in freqs.iter().enumerate() {
                            let (amplitude, phase) = fit_sine(&second, freq, rate);
                            // The tone sin(2π f t) at t = start / rate has this phase.
                            let t = start as f64 / rate;
                            let want = (std::f64::consts::TAU * freq * t)
                                .rem_euclid(std::f64::consts::TAU);
                            let mut d = phase - want;
                            d = (d + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
                                - std::f64::consts::PI;
                            windows[k].push(Window {
                                t: t - T0,
                                amplitude,
                                late_s: -d / (std::f64::consts::TAU * freq),
                            });
                        }
                    }
                    second.clear();
                    second_start = Some(n);
                }
                second.push(f[0]);
                assert_eq!(f[0], f[1], "both inputs are centred");
            }
        }
    }
    windows
}

/// ROADMAP M1a's case: a 44.1 kHz mono microphone that runs fast and a 48 kHz 5.1
/// desktop loopback that runs slow, both with timestamp jitter, mixed for four minutes.
/// After the controller settles, each input must sit within 0.05 ms of the engine time
/// line (measured from the audio itself; S3 held real devices within 0.06 ms), keep its
/// gain, and the controller must have found each device's drift. Nothing underruns, overruns or re-anchors after the start.
#[test]
fn drifting_devices_stay_on_the_engine_time_line() {
    let mut mixer = Mixer::new(MixerConfig::default()).unwrap();
    let (mic, mic_ctl) = Device::new(
        &mut mixer,
        Spec {
            rate: 44_100,
            layout: ChannelLayout::Mono,
            ppm: 80.0,
            jitter_s: 0.3e-3,
            freq: 250.0,
            amp: 0.4,
            seed: 1,
        },
    );
    let (desk, desk_ctl) = Device::new(
        &mut mixer,
        Spec {
            rate: 48_000,
            layout: ChannelLayout::Surround51,
            ppm: -150.0,
            jitter_s: 0.06e-3,
            freq: 330.0,
            amp: 0.25,
            seed: 2,
        },
    );
    mic_ctl.set_gain(0.5);
    let mut devices = [mic, desk];
    let windows = run(&mut mixer, &mut devices, 240.0, &[250.0, 330.0]);
    let settled = |w: &&Window| w.t >= 120.0;
    for (k, want_amp) in [(0, 0.2), (1, 0.25)] {
        let ws: Vec<&Window> = windows[k].iter().filter(settled).collect();
        assert!(ws.len() >= 110, "{} windows", ws.len());
        for w in &ws {
            assert!(
                w.late_s.abs() < 0.05e-3,
                "input {k} at {} s: {} ms off",
                w.t,
                w.late_s * 1e3
            );
            assert!(
                (w.amplitude - want_amp).abs() < 0.001 * want_amp,
                "input {k} amplitude {}",
                w.amplitude
            );
        }
        // Even while settling, the error stays within a millisecond (the start-up
        // transient peaks near 0.46 · drift / (2π · 0.02 Hz): 0.55 ms at 150 ppm).
        for w in windows[k].iter().filter(|w| w.t >= 2.0) {
            assert!(
                w.late_s.abs() < 1e-3,
                "input {k} at {} s: {} ms off",
                w.t,
                w.late_s * 1e3
            );
        }
    }
    for (ctl, ppm, jitter) in [(&mic_ctl, 80.0, 0.3e-3), (&desk_ctl, -150.0, 0.06e-3)] {
        let st = ctl.stats();
        let drift = st.drift_ppm.unwrap();
        assert!((drift - ppm).abs() < 1.0, "drift {drift} vs {ppm}");
        // The fit's jitter of uniform ±j is j / sqrt(3), plus the 100 ns stamp rounding.
        let sd = st.jitter_s.unwrap();
        assert!(
            (sd - jitter / 3f64.sqrt()).abs() < 0.2 * jitter,
            "jitter {sd}"
        );
        // The correction's proportional term carries the smoothed stamp jitter, a few
        // ppm for the mic.
        assert!(
            (st.correction_ppm - ppm).abs() < 5.0,
            "correction {} vs {ppm}",
            st.correction_ppm
        );
        assert!(st.timing_error_s.unwrap().abs() < 0.1e-3);
        assert_eq!((st.resets, st.reanchors, st.outliers), (0, 0, 0));
        assert_eq!(
            (st.overrun_frames, st.late_frames, st.gap_frames),
            (0, 0, 0)
        );
        assert_eq!(st.unanchored_frames, 0);
        // The mixer starts 30 ms before the first packet arrives: only frames before
        // each input's first sample can underrun.
        assert!(
            st.underrun_frames < 2_400,
            "underrun {}",
            st.underrun_frames
        );
        assert!(!st.failed);
    }
    let bus = mixer.bus().stats();
    assert_eq!(bus.skipped_frames, 0);
    assert!(bus.blocks > 240 * 46);
}

/// Glitches on one 48 kHz stereo input: a lost packet (filled with silence), a 150 ms
/// stall (the mixer plays silence, counts underrun and drops the late audio), a single
/// timestamp 100 ms off (rejected), and a stream restart with positions back at 0 and a
/// discontinuity flag (re-anchored). After each, the audio is back on the time line.
#[test]
fn glitches_are_counted_and_the_input_recovers() {
    let mut mixer = Mixer::new(MixerConfig::default()).unwrap();
    let (mut dev, ctl) = Device::new(
        &mut mixer,
        Spec {
            rate: 48_000,
            layout: ChannelLayout::Stereo,
            ppm: 40.0,
            jitter_s: 0.1e-3,
            freq: 250.0,
            amp: 0.5,
            seed: 3,
        },
    );
    // Packets are 10 ms: packet 1000 starts at 10 s.
    dev.tweak = Box::new(|i| match i {
        1000 => Tweak {
            lose: true,
            ..Tweak::default()
        },
        2000 => Tweak {
            delay_s: 0.150,
            ..Tweak::default()
        },
        2500 => Tweak {
            stamp_offset_s: 0.100,
            ..Tweak::default()
        },
        3000 => Tweak {
            restart: true,
            ..Tweak::default()
        },
        _ => Tweak::default(),
    });
    let mut devices = [dev];
    let windows = run(&mut mixer, &mut devices, 60.0, &[250.0]);
    let st = ctl.stats();
    assert_eq!(st.gap_frames, 480);
    assert_eq!(st.outliers, 1);
    assert_eq!(st.discontinuities, 1);
    assert_eq!(st.resets, 1);
    assert_eq!(st.reanchors, 1);
    // The stall: packet 2000 (20.00 to 20.01 s) arrives 150 ms late, at 20.161 s, and
    // the mixer has by then mixed everything up to 30 ms earlier. Those frames are
    // silence (underrun) and their audio arrives late; give or take one block.
    let stall = (0.010 + DELIVERY_S + 0.150 - 0.030) * 48_000.0;
    // Before the first packet, the mixer's first block reaches 30 ms back.
    let startup = 0.030 * 48_000.0;
    let underrun = st.underrun_frames as f64;
    assert!(
        (underrun - stall - startup).abs() < 1_100.0,
        "underrun {underrun}"
    );
    let late = st.late_frames as f64;
    assert!((late - stall).abs() < 1_100.0, "late {late}");
    assert_eq!(st.overrun_frames, 0);
    // Every second without a glitch is on the time line; the glitch seconds are
    // allowed their damage.
    let glitched = |t: f64| [10.0, 20.0, 30.0].iter().any(|g| (t - g).abs() < 1.5);
    for w in windows[0].iter().filter(|w| w.t >= 2.0 && !glitched(w.t)) {
        assert!(
            w.late_s.abs() < 0.5e-3,
            "at {} s: {} ms off",
            w.t,
            w.late_s * 1e3
        );
        assert!(
            (w.amplitude - 0.5).abs() < 0.005,
            "at {} s: amplitude {}",
            w.t,
            w.amplitude
        );
    }
}
