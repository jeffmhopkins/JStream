//! The mixer: pulls a fixed-size block from every input's ring on the engine clock,
//! applies each input's gain, sums them into the stereo bus, and meters both.
//!
//! **Time line.** Engine frame `n` is media time `n / rate` (the media clock's zero,
//! docs/adr/0014-media-clock.md). A block is the frames `[n, n + block)`; its pts is `n`
//! ticks of `1 / rate` seconds. The mixer produces a block once the clock has passed
//! its end by `latency` frames ([`Mixer::poll`]), so every input's audio for that window
//! has had `latency` to arrive (device period, the resampler's look-ahead, and jitter).
//! Inputs that missed it read as silence and are counted as underrun.
//!
//! **Falling behind.** If the mixer thread is stalled so long that the block it is due
//! to mix is more than half a ring behind the clock, it jumps forward to the newest due
//! block and counts the frames it skipped, rather than mixing stale or overwritten data.
//!
//! **Gain** changes ramp linearly across one block, so fader moves don't click.
//!
//! **Meters** are post-gain (what each input contributes to the bus) and the bus output:
//! per channel, the peak held until the control plane takes it, and the latest block's
//! RMS.
//!
//! [`Mixer::mix`] and [`Mixer::poll`] do not allocate, lock, block or log (R-RT-001); the
//! buffers are sized in [`Mixer::new`] and [`Mixer::add_input`].

use std::sync::Arc;

use jstream_media::{AudioFormat, MediaTime, Rounding, SampleFormat, Timebase};

use super::input::{InputProducer, InputShared};
use super::ring::FrameRing;
use super::shared::{Counter, GainParam, Meter, levels};
use super::sinc::{MAX_DOWN_RATIO, MAX_UP_RATIO};
use crate::error::AudioError;
use crate::handle::{BusControl, InputControl};

/// How the mixer is set up. Fixed for the mixer's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    /// The engine sample rate, Hz: every input is resampled to it. Default 48 000.
    pub sample_rate: u32,
    /// Frames per mixed block. Default 1024, one AAC access unit.
    pub block_frames: usize,
    /// How long after a block's end the mixer waits before mixing it, in frames.
    /// Default 1440 (30 ms): one 10 ms device period, the resampler's look-ahead
    /// (1.3 to 1.5 ms), timestamp jitter and thread wake-up, with margin.
    pub latency_frames: usize,
    /// Each input ring's capacity, in frames: a power of two of at least
    /// `2 * (block_frames + latency_frames)`. Default 16384 (341 ms).
    pub ring_frames: usize,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            block_frames: 1024,
            latency_frames: 1440,
            ring_frames: 16_384,
        }
    }
}

impl MixerConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), AudioError> {
        let bad = |what| Err(AudioError::InvalidConfig { what });
        if !(8_000..=192_000).contains(&self.sample_rate) {
            return bad("sample_rate must be 8000..=192000");
        }
        if !(1..=16_384).contains(&self.block_frames) {
            return bad("block_frames must be 1..=16384");
        }
        if self.latency_frames > self.sample_rate as usize {
            return bad("latency_frames must be at most one second");
        }
        if !self.ring_frames.is_power_of_two()
            || self.ring_frames > 1 << 22
            || self.ring_frames < 2 * (self.block_frames + self.latency_frames)
        {
            return bad(
                "ring_frames must be a power of two, at least 2 * (block_frames + latency_frames), at most 2^22",
            );
        }
        Ok(())
    }
}

/// One mixed block's place on the engine time line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// The engine frame of the block's first frame.
    pub frame: i64,
    /// Frames in the block ([`MixerConfig::block_frames`]).
    pub frames: usize,
    /// The block's presentation time: `frame` ticks of `1 / sample_rate` seconds on the
    /// media clock.
    pub pts: MediaTime,
}

/// What the mixer thread and the control plane share for the bus.
#[derive(Debug)]
pub(crate) struct BusShared {
    pub(crate) gain: GainParam,
    pub(crate) meter: Meter,
    pub(crate) blocks: Counter,
    pub(crate) skipped_frames: Counter,
}

#[derive(Debug)]
struct Slot {
    ring: Arc<FrameRing>,
    shared: Arc<InputShared>,
    /// The gain applied at the end of the previous block, where the next ramp starts.
    gain_now: f32,
}

/// The stereo mix bus and its inputs. Owned by the mixer thread.
#[derive(Debug)]
pub struct Mixer {
    config: MixerConfig,
    timebase: Timebase,
    inputs: Vec<Slot>,
    bus: Arc<BusShared>,
    bus_gain_now: f32,
    /// The next block's first frame, once started.
    position: Option<i64>,
    scratch: Box<[[f32; 2]]>,
    acc: Box<[[f32; 2]]>,
}

impl Mixer {
    /// A mixer with no inputs.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `config` doesn't [validate](MixerConfig::validate).
    pub fn new(config: MixerConfig) -> Result<Self, AudioError> {
        config.validate()?;
        let timebase =
            Timebase::per_second(config.sample_rate).map_err(|_| AudioError::InvalidConfig {
                what: "sample_rate must be non-zero",
            })?;
        Ok(Self {
            config,
            timebase,
            inputs: Vec::new(),
            bus: Arc::new(BusShared {
                gain: GainParam::new(),
                meter: Meter::default(),
                blocks: Counter::default(),
                skipped_frames: Counter::default(),
            }),
            bus_gain_now: 1.0,
            position: None,
            scratch: vec![[0.0; 2]; config.block_frames].into_boxed_slice(),
            acc: vec![[0.0; 2]; config.block_frames].into_boxed_slice(),
        })
    }

    /// Adds an input of `format` (the device's own rate and layout, interleaved `f32`, as
    /// [`AudioStream::format`](jstream_platform::AudioStream::format) reports it). Returns
    /// the producer for the input's audio thread and a control handle. Allocates: call it
    /// while setting the mixer up, not from the mixer thread's loop.
    ///
    /// # Errors
    /// [`AudioError::UnsupportedFormat`] for planar samples, and
    /// [`AudioError::UnsupportedRate`] for a rate more than 4 times above or 6 times below
    /// the engine rate.
    pub fn add_input(
        &mut self,
        format: AudioFormat,
    ) -> Result<(InputProducer, InputControl), AudioError> {
        if format.sample_format() != SampleFormat::F32Interleaved {
            return Err(AudioError::UnsupportedFormat);
        }
        let rate = format.sample_rate();
        let engine = self.config.sample_rate;
        if u64::from(rate) > u64::from(engine) * u64::from(MAX_DOWN_RATIO)
            || u64::from(rate) * u64::from(MAX_UP_RATIO) < u64::from(engine)
        {
            return Err(AudioError::UnsupportedRate { rate, engine });
        }
        let ring = Arc::new(FrameRing::new(self.config.ring_frames));
        let shared = Arc::new(InputShared::new());
        if let Some(pos) = self.position {
            ring.set_read_pos(pos);
        }
        self.inputs.push(Slot {
            ring: Arc::clone(&ring),
            shared: Arc::clone(&shared),
            gain_now: shared.gain.effective(),
        });
        let producer = InputProducer::new(format, engine, Arc::clone(&ring), Arc::clone(&shared));
        Ok((producer, InputControl::new(shared, ring)))
    }

    /// The bus's control handle.
    pub fn bus(&self) -> BusControl {
        BusControl::new(Arc::clone(&self.bus))
    }

    /// The configuration.
    pub fn config(&self) -> MixerConfig {
        self.config
    }

    /// The first frame of the next block, once the mixer has started.
    pub fn position(&self) -> Option<i64> {
        self.position
    }

    /// The engine frame at media time `t` (rounded down), or `None` if it doesn't fit.
    pub fn engine_frame(&self, t: MediaTime) -> Option<i64> {
        t.rescale(self.timebase, Rounding::Down)
            .ok()
            .map(MediaTime::ticks)
    }

    /// Mixes the next block if the clock (`now`) has passed its end by the configured
    /// latency, into `out` (interleaved stereo, `2 * block_frames` samples). Returns
    /// `None` when no block is due yet; call it again later. The first call starts the
    /// time line at `now` minus the latency.
    ///
    /// # Errors
    /// [`AudioError::OutputLength`] when `out` has the wrong length.
    pub fn poll(&mut self, now: MediaTime, out: &mut [f32]) -> Result<Option<Block>, AudioError> {
        self.check_out(out)?;
        let Some(now) = self.engine_frame(now) else {
            return Ok(None);
        };
        let block = self.config.block_frames as i64;
        let latency = self.config.latency_frames as i64;
        let start = now.saturating_sub(latency);
        let pos = *self.position.get_or_insert(start);
        let due = pos.saturating_add(block).saturating_add(latency);
        if now < due {
            return Ok(None);
        }
        let max_lag = (self.config.ring_frames / 2) as i64;
        let pos = if now.saturating_sub(due) > max_lag {
            let newest = start.saturating_sub(block);
            self.bus
                .skipped_frames
                .add(newest.saturating_sub(pos).unsigned_abs());
            newest
        } else {
            pos
        };
        self.mix(pos, out).map(Some)
    }

    /// Mixes the block starting at engine frame `frame` into `out` (interleaved stereo,
    /// `2 * block_frames` samples), whatever the clock says, and continues from its end.
    ///
    /// # Errors
    /// [`AudioError::OutputLength`] when `out` has the wrong length.
    pub fn mix(&mut self, frame: i64, out: &mut [f32]) -> Result<Block, AudioError> {
        self.check_out(out)?;
        let n = self.config.block_frames;
        let end = frame.saturating_add(n as i64);
        self.acc.fill([0.0; 2]);
        for slot in &mut self.inputs {
            let rep = slot.ring.read(frame, &mut self.scratch);
            slot.ring.set_read_pos(end);
            if rep.started {
                slot.shared
                    .underrun_frames
                    .add((n - rep.valid.min(n)) as u64);
            }
            let target = slot.shared.gain.effective();
            apply_gain(&mut self.scratch, slot.gain_now, target);
            slot.gain_now = target;
            let (peak, rms) = levels(&self.scratch);
            slot.shared.meter.publish(peak, rms);
            for (a, s) in self.acc.iter_mut().zip(self.scratch.iter()) {
                a[0] += s[0];
                a[1] += s[1];
            }
        }
        let target = self.bus.gain.effective();
        apply_gain(&mut self.acc, self.bus_gain_now, target);
        self.bus_gain_now = target;
        for (o, a) in out.chunks_exact_mut(2).zip(self.acc.iter_mut()) {
            for c in 0..2 {
                if !a[c].is_finite() {
                    a[c] = 0.0;
                }
                o[c] = a[c];
            }
        }
        let (peak, rms) = levels(&self.acc);
        self.bus.meter.publish(peak, rms);
        self.bus.blocks.add(1);
        self.position = Some(end);
        Ok(Block {
            frame,
            frames: n,
            pts: MediaTime::new(frame, self.timebase),
        })
    }

    fn check_out(&self, out: &[f32]) -> Result<(), AudioError> {
        let expected = self.config.block_frames * 2;
        if out.len() == expected {
            Ok(())
        } else {
            Err(AudioError::OutputLength {
                expected,
                got: out.len(),
            })
        }
    }
}

/// Scales `frames` by a gain ramping linearly from `from` (before the first frame) to
/// `to` (at the last).
fn apply_gain(frames: &mut [[f32; 2]], from: f32, to: f32) {
    if from == to {
        if to != 1.0 {
            for f in frames.iter_mut() {
                f[0] *= to;
                f[1] *= to;
            }
        }
        return;
    }
    let n = frames.len() as f32;
    for (i, f) in frames.iter_mut().enumerate() {
        let g = from + (to - from) * ((i + 1) as f32 / n);
        f[0] *= g;
        f[1] *= g;
    }
}

#[cfg(test)]
mod tests {
    use jstream_media::ChannelLayout;
    use jstream_platform::{AudioPacket, AudioPacketFlags};

    use super::*;

    const RATE: u32 = 48_000;

    fn stereo48() -> AudioFormat {
        AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Interleaved).unwrap()
    }

    fn config(block: usize) -> MixerConfig {
        MixerConfig {
            block_frames: block,
            latency_frames: 480,
            ring_frames: 8192,
            ..MixerConfig::default()
        }
    }

    /// Feeds `secs` of constant `[l, r]` frames in 10 ms packets stamped exactly, with
    /// the first frame at clock time 0.
    fn feed(p: &mut InputProducer, l: f32, r: f32, secs: f64) {
        let frames = 480;
        let samples: Vec<f32> = (0..frames).flat_map(|_| [l, r]).collect();
        let packets = (secs * 100.0) as u64;
        for i in 0..packets {
            p.push_packet(&AudioPacket {
                samples: &samples,
                device_position: i * frames as u64,
                time: Some(MediaTime::new(
                    (i * frames as u64) as i64,
                    Timebase::per_second(RATE).unwrap(),
                )),
                flags: AudioPacketFlags::default(),
            });
        }
    }

    #[test]
    fn config_is_validated() {
        assert!(MixerConfig::default().validate().is_ok());
        let bad = [
            MixerConfig {
                sample_rate: 0,
                ..MixerConfig::default()
            },
            MixerConfig {
                block_frames: 0,
                ..MixerConfig::default()
            },
            MixerConfig {
                ring_frames: 10_000,
                ..MixerConfig::default()
            },
            MixerConfig {
                ring_frames: 4096,
                ..MixerConfig::default()
            },
            MixerConfig {
                latency_frames: 96_000,
                ..MixerConfig::default()
            },
        ];
        for c in bad {
            assert!(
                matches!(Mixer::new(c), Err(AudioError::InvalidConfig { .. })),
                "{c:?}"
            );
        }
    }

    #[test]
    fn unsupported_inputs_are_rejected() {
        let mut m = Mixer::new(MixerConfig::default()).unwrap();
        let planar =
            AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Planar).unwrap();
        assert!(matches!(
            m.add_input(planar),
            Err(AudioError::UnsupportedFormat)
        ));
        for rate in [7_999, 192_001] {
            let f =
                AudioFormat::new(rate, ChannelLayout::Mono, SampleFormat::F32Interleaved).unwrap();
            assert!(
                matches!(m.add_input(f), Err(AudioError::UnsupportedRate { .. })),
                "{rate}"
            );
        }
        for rate in [8_000, 44_100, 192_000] {
            let f =
                AudioFormat::new(rate, ChannelLayout::Mono, SampleFormat::F32Interleaved).unwrap();
            assert!(m.add_input(f).is_ok(), "{rate}");
        }
    }

    /// Whichever of the producer (audio thread) and the mixer (mixer thread) is dropped
    /// last, the input's ring and counters outlive both while a control handle is
    /// held, so the control plane frees them (R-RT-003).
    #[test]
    fn control_handle_holds_the_last_shared_reference() {
        for mixer_first in [true, false] {
            let mut m = Mixer::new(config(480)).unwrap();
            let (p, control) = m.add_input(stereo48()).unwrap();
            let ring = Arc::downgrade(&m.inputs[0].ring);
            let shared = Arc::downgrade(&m.inputs[0].shared);
            if mixer_first {
                drop(m);
                drop(p);
            } else {
                drop(p);
                drop(m);
            }
            assert!(ring.upgrade().is_some(), "ring freed by an RT handle");
            assert!(shared.upgrade().is_some(), "counters freed by an RT handle");
            drop(control);
            assert!(ring.upgrade().is_none());
            assert!(shared.upgrade().is_none());
        }
    }

    /// Two inputs with gains sum on the bus; meters see post-gain levels.
    #[test]
    fn gains_mix_and_meter() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let (mut b, cb) = m.add_input(stereo48()).unwrap();
        feed(&mut a, 0.5, -0.5, 1.0);
        feed(&mut b, 0.25, 0.25, 1.0);
        ca.set_gain(0.5);
        cb.set_gain_db(6.0);
        let mut out = vec![0.0; 960];
        // The 8192-frame rings hold frames 39744..47936 of the 47936 written.
        // The first block ramps from unity to the new gains; mix it and discard it.
        m.mix(40_000, &mut out).unwrap();
        ca.take_meter();
        m.bus().take_meter();
        let blk = m.mix(40_480, &mut out).unwrap();
        assert_eq!(blk.frame, 40_480);
        assert_eq!(
            blk.pts,
            MediaTime::new(40_480, Timebase::per_second(RATE).unwrap())
        );
        let g = 10f32.powf(6.0 / 20.0);
        let want = [0.25 + 0.25 * g, -0.25 + 0.25 * g];
        for f in out.chunks_exact(2) {
            assert!(
                (f[0] - want[0]).abs() < 1e-5 && (f[1] - want[1]).abs() < 1e-5,
                "{f:?}"
            );
        }
        let ma = ca.take_meter();
        assert!((ma.peak[0] - 0.25).abs() < 1e-5 && (ma.rms[1] - 0.25).abs() < 1e-5);
        let bus = m.bus().take_meter();
        assert!((bus.peak[0] - want[0]).abs() < 1e-5);
        assert!((bus.rms[1] - want[1].abs()).abs() < 1e-5);
        // Mute and bus gain.
        ca.set_muted(true);
        m.bus().set_gain(2.0);
        m.mix(40_960, &mut out).unwrap(); // ramp
        m.mix(41_440, &mut out).unwrap();
        assert!((out[0] - 2.0 * 0.25 * g).abs() < 1e-5, "{}", out[0]);
        assert_eq!(ca.stats().underrun_frames, 0);
        assert_eq!(m.bus().stats().blocks, 4);
    }

    #[test]
    fn gain_ramps_across_one_block() {
        let mut frames = [[1.0f32; 2]; 4];
        apply_gain(&mut frames, 0.0, 1.0);
        assert_eq!(frames.map(|f| f[0]), [0.25, 0.5, 0.75, 1.0]);
        let mut frames = [[1.0f32; 2]; 2];
        apply_gain(&mut frames, 0.5, 0.5);
        assert_eq!(frames, [[0.5; 2]; 2]);
    }

    /// Reading where the input has no data is silence, counted as underrun once the
    /// input has started; before it starts nothing is counted.
    #[test]
    fn underrun_is_silence_and_counted() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let mut out = vec![1.0; 960];
        m.mix(0, &mut out).unwrap();
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(ca.stats().underrun_frames, 0);
        feed(&mut a, 0.5, 0.5, 0.1); // frames 0..4800, minus the look-ahead
        m.mix(4_480, &mut out).unwrap();
        let st = ca.stats();
        // 4800 - 64 frames are written; 4480..4736 valid, the rest underrun.
        assert_eq!(st.underrun_frames, 480 - (4_736 - 4_480));
        assert!(out[..2 * 256].iter().all(|&s| (s - 0.5).abs() < 1e-4));
        assert!(out[2 * 256..].iter().all(|&s| s == 0.0));
    }

    /// An input far ahead of the mixer overwrites unread frames: dropped oldest, counted.
    #[test]
    fn overrun_drops_the_oldest_and_counts() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let mut out = vec![0.0; 960];
        m.mix(0, &mut out).unwrap(); // read position 480
        feed(&mut a, 0.5, 0.5, 1.0); // 48000 frames into an 8192-frame ring
        let st = ca.stats();
        let written = 48_000 - 64;
        assert_eq!(st.overrun_frames, written - 8192 - 480);
        assert_eq!(st.late_frames, 480);
    }

    #[test]
    fn poll_produces_blocks_on_the_clock() {
        let mut m = Mixer::new(config(1024)).unwrap();
        let tb = Timebase::per_second(RATE).unwrap();
        let mut out = vec![0.0; 2048];
        // Starts at now - latency; nothing is due until a block has elapsed.
        assert_eq!(m.poll(MediaTime::new(10_000, tb), &mut out).unwrap(), None);
        assert_eq!(m.position(), Some(10_000 - 480));
        assert_eq!(m.poll(MediaTime::new(11_023, tb), &mut out).unwrap(), None);
        let b = m
            .poll(MediaTime::new(11_024, tb), &mut out)
            .unwrap()
            .unwrap();
        assert_eq!(b.frame, 9_520);
        assert_eq!(m.poll(MediaTime::new(11_024, tb), &mut out).unwrap(), None);
        // A clock in another time base works too: 0.3 s in 100 ns ticks is frame 14400.
        let hns = Timebase::new(1, 10_000_000).unwrap();
        let mut n = 0;
        while m
            .poll(MediaTime::new(3_000_000, hns), &mut out)
            .unwrap()
            .is_some()
        {
            n += 1;
        }
        // Blocks at 10544, 11568 and 12592 end at least 480 frames before 14400.
        assert_eq!(n, 3);
        assert_eq!(m.bus().stats().skipped_frames, 0);
        // A stall of 10 s: the mixer jumps to the newest block and counts the skip.
        let pos = m.position().unwrap();
        let b = m
            .poll(MediaTime::new(530_000, tb), &mut out)
            .unwrap()
            .unwrap();
        assert_eq!(b.frame, 530_000 - 480 - 1024);
        assert_eq!(m.bus().stats().skipped_frames, (b.frame - pos) as u64);
        assert!(matches!(
            m.poll(MediaTime::new(0, tb), &mut [0.0; 3]),
            Err(AudioError::OutputLength {
                expected: 2048,
                got: 3
            })
        ));
    }
}
