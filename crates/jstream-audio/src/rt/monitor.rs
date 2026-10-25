//! A bus monitor's playback (ADR-0028, D22): what a monitor hears goes from the mixer
//! thread to an output device's audio thread.
//!
//! The mixer thread hands each block it monitors ([`Mixer::monitor_block`]) to a
//! [`MonitorFeed`], which writes it into a [`FrameRing`] under its engine frames, as an
//! input's producer does. The device's audio thread runs the [`MonitorPlayer`]
//! (`jstream_platform::AudioOutputHandler`): it reads the ring a fixed distance behind
//! the newest frame written, resamples from the engine rate to the device's, and maps
//! stereo onto the device's channels.
//!
//! **Clock.** The device plays on its own clock, which drifts against the media clock
//! the mixer runs on. The player steers its resampler so that the distance it reads
//! behind the feed stays at [`MonitorPlayer::target_frames`], with the controller every
//! input uses ([`control`](super::control)): the error is that distance less the
//! target, in seconds, smoothed as an input's timing error is. A distance more than
//! [`REANCHOR_ERROR_S`] above the target (the device stalled, or the feed came back
//! after a gap) moves the read position forward onto the target instead, counted in
//! [`MonitorStats::reanchors`]. A distance that far below it (the feed stalled or
//! stopped) leaves the player reading forward into silence, counted as underrun, and
//! the controller untouched: moving back would replay what was already heard.
//!
//! **Latency.** The target is 1.5 blocks plus 20 ms: the feed writes a block at a time,
//! so the distance swings by a block, and the rest covers a device period and the
//! resampler's look-ahead. What the device plays is that far behind the mixer, which
//! itself mixes [`MixerConfig::latency_frames`] behind the clock.
//!
//! Neither side allocates, locks, blocks or logs after construction (R-RT-001), and
//! each hands its references to the ring and the counters to the mixer's collector when
//! it is dropped (R-RT-003).
//!
//! [`Mixer::monitor_block`]: super::mixer::Mixer::monitor_block
//! [`MixerConfig::latency_frames`]: super::mixer::MixerConfig::latency_frames

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
use jstream_platform::{AudioOutputHandler, AudioRenderBuffer, PlatformError};

use super::control::{Ema, Pi};
use super::input::{ERROR_SMOOTHING_S, LOOP_BANDWIDTH_HZ, LOOP_DAMPING, MAX_CORRECTION};
use super::retire::Retirer;
use super::ring::{FrameRing, RingWriter};
use super::shared::Counter;
use super::sinc::{MAX_DOWN_RATIO, MAX_UP_RATIO, Output, Resampler};
use crate::error::AudioError;

/// How far the read distance may drift from its target before the player re-anchors.
pub(crate) const REANCHOR_ERROR_S: f64 = 0.050;
/// Engine frames the player reads from the ring at a time.
const STAGE_FRAMES: usize = 64;
/// Output frames one input frame can complete beyond a render's end: the most an
/// upsampling step emits (rate_out / rate_in, at most [`MAX_UP_RATIO`]), with margin.
const CARRY_FRAMES: usize = 2 * MAX_UP_RATIO as usize;

/// Counters both sides of a monitor share with its [`MonitorControl`].
#[derive(Debug, Default)]
pub(crate) struct MonitorShared {
    blocks: Counter,
    renders: Counter,
    underrun_frames: Counter,
    reanchors: Counter,
    failed: AtomicBool,
}

/// What a monitor did, as [`MonitorControl::stats`] reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorStats {
    /// Blocks the feed wrote.
    pub blocks: u64,
    /// Times the device asked the player for samples.
    pub renders: u64,
    /// Engine frames the player read before the feed had written them, played as
    /// silence.
    pub underrun_frames: u64,
    /// Times the player moved its read position forward onto the target distance.
    pub reanchors: u64,
    /// Whether the device stream failed and plays nothing more.
    pub failed: bool,
}

/// A monitor's three ends, from [`MixerControl::add_monitor`](super::mixer::MixerControl::add_monitor).
#[derive(Debug)]
pub struct Monitor {
    /// For the mixer thread, which pushes every block the monitor hears.
    pub feed: MonitorFeed,
    /// For `AudioIo::open_output`, as the device stream's handler.
    pub player: MonitorPlayer,
    /// For the control plane, which reads the counters.
    pub control: MonitorControl,
}

/// Reads a monitor's counters. Cheap to clone.
#[derive(Clone, Debug)]
pub struct MonitorControl {
    shared: Arc<MonitorShared>,
}

impl MonitorControl {
    /// The counters so far.
    pub fn stats(&self) -> MonitorStats {
        let s = &self.shared;
        MonitorStats {
            blocks: s.blocks.get(),
            renders: s.renders.get(),
            underrun_frames: s.underrun_frames.get(),
            reanchors: s.reanchors.get(),
            failed: s.failed.load(Ordering::Relaxed),
        }
    }
}

/// The mixer thread's end of a monitor.
#[derive(Debug)]
pub struct MonitorFeed {
    writer: Option<RingWriter>,
    shared: Option<Arc<MonitorShared>>,
    retirer: Retirer,
}

impl MonitorFeed {
    /// Writes `block` (interleaved stereo, as [`Mixer::monitor_block`] returns it)
    /// under engine frames `[frame, frame + block.len() / 2)`. A block that starts
    /// after the previous one's end leaves silence in between; one that starts before
    /// it is ignored, since the ring only appends.
    ///
    /// [`Mixer::monitor_block`]: super::mixer::Mixer::monitor_block
    pub fn push(&mut self, frame: i64, block: &[f32]) {
        let (Some(writer), Some(shared)) = (&mut self.writer, &self.shared) else {
            return;
        };
        if writer.position().is_some_and(|pos| frame < pos) {
            return;
        }
        writer.seek(frame);
        writer.write(block.as_chunks::<2>().0);
        shared.blocks.add(1);
    }
}

impl Drop for MonitorFeed {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            self.retirer.ring(writer.into_ring());
        }
        if let Some(shared) = self.shared.take() {
            self.retirer.monitor(shared);
        }
    }
}

/// The output device's end of a monitor: an `AudioOutputHandler` for the device
/// stream.
#[derive(Debug)]
pub struct MonitorPlayer {
    ring: Option<Arc<FrameRing>>,
    shared: Option<Arc<MonitorShared>>,
    retirer: Retirer,
    capacity: i64,
    resampler: Resampler,
    layout: ChannelLayout,
    engine_rate: f64,
    device_rate: f64,
    target: i64,
    /// The next engine frame read from the ring, once anchored.
    pos: Option<i64>,
    stage: Box<[[f32; 2]]>,
    /// The next staged frame to push into the resampler.
    staged_at: usize,
    /// Output frames the resampler completed past the previous render's end.
    carry: [[f32; 2]; CARRY_FRAMES],
    carried: usize,
    pi: Pi,
    smooth: Ema,
}

impl MonitorPlayer {
    /// How far behind the newest frame the feed wrote the player reads, in engine
    /// frames.
    pub fn target_frames(&self) -> i64 {
        self.target
    }

    /// The engine frame the resampler has been fed up to.
    fn pushed(&self, pos: i64) -> i64 {
        pos - (STAGE_FRAMES - self.staged_at) as i64
    }

    /// Restarts reading `target` frames behind `written`, from silence.
    fn anchor(&mut self, written: i64) {
        self.pos = Some(written - self.target);
        self.staged_at = STAGE_FRAMES;
        self.carried = 0;
        self.resampler.reset(0.0);
        self.smooth.reset();
    }

    /// Steers the resampler from the read distance, or re-anchors.
    fn steer(&mut self, ring: &FrameRing, shared: &MonitorShared, frames: usize) {
        let Some(written) = ring.written() else {
            return;
        };
        let Some(pos) = self.pos else {
            self.anchor(written);
            return;
        };
        let error = (written - self.pushed(pos) - self.target) as f64 / self.engine_rate;
        if error > REANCHOR_ERROR_S || written - pos > self.capacity / 2 {
            // Too far behind the feed (the device stalled, or the feed jumped ahead):
            // move forward onto the target.
            shared.reanchors.add(1);
            self.anchor(written);
            return;
        }
        if error < -REANCHOR_ERROR_S {
            // Ahead of the feed: it stalled or stopped. Moving back would replay what
            // was already heard, so keep reading forward into silence, and leave the
            // controller alone until the feed is back.
            return;
        }
        let dt = frames as f64 / self.device_rate;
        if let Some(e) = self.smooth.update(error, dt) {
            self.resampler.set_correction(self.pi.update(e, dt));
        }
    }
}

impl MonitorPlayer {
    /// Fills `samples` (already silent) from the ring.
    fn play(&mut self, ring: &FrameRing, shared: &MonitorShared, samples: &mut [f32]) {
        let channels = self.layout.channel_count();
        let frames = samples.len() / channels;
        shared.renders.add(1);
        self.steer(ring, shared, frames);
        let Some(mut pos) = self.pos else {
            return;
        };
        let layout = self.layout;
        let mut out = samples.chunks_exact_mut(channels);
        let mut written = 0;
        // First what the previous render's last input frame completed past its end.
        let carried = std::mem::take(&mut self.carried);
        for k in 0..carried {
            let Some(slot) = out.next() else {
                // A render shorter than the carry keeps the rest for the next one.
                self.carry.copy_within(k..carried, 0);
                self.carried = carried - k;
                return;
            };
            put(slot, layout, self.carry[k]);
            written += 1;
        }
        let Self {
            resampler,
            stage,
            staged_at,
            carry,
            carried,
            ..
        } = self;
        while written < frames {
            if *staged_at == STAGE_FRAMES {
                let report = ring.read(pos, stage);
                ring.set_read_pos(pos + STAGE_FRAMES as i64);
                shared
                    .underrun_frames
                    .add((STAGE_FRAMES - report.valid.min(STAGE_FRAMES)) as u64);
                pos += STAGE_FRAMES as i64;
                *staged_at = 0;
            }
            let frame = stage[*staged_at];
            *staged_at += 1;
            resampler.push(frame, &mut |output| {
                // `push` emits frames only; silence runs come from `push_silence`.
                let Output::Frame(f) = output else { return };
                if let Some(slot) = out.next() {
                    put(slot, layout, f);
                    written += 1;
                } else if let Some(c) = carry.get_mut(*carried) {
                    *c = f;
                    *carried += 1;
                }
            });
        }
        self.pos = Some(pos);
    }
}

/// Writes stereo `frame` into one device frame of `layout`: both channels averaged
/// into mono, left and right into the front pair otherwise, the rest silent.
fn put(out: &mut [f32], layout: ChannelLayout, frame: [f32; 2]) {
    if layout == ChannelLayout::Mono {
        out.fill(0.5 * (frame[0] + frame[1]));
    } else {
        // Already silent; `zip` stops at the device's channel count.
        for (sample, v) in out.iter_mut().zip(frame) {
            *sample = v;
        }
    }
}

impl AudioOutputHandler for MonitorPlayer {
    fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
        buffer.samples.fill(0.0);
        // Moved out and back so `steer` can borrow `self`; no reference count changes.
        let (Some(ring), Some(shared)) = (self.ring.take(), self.shared.take()) else {
            return;
        };
        self.play(&ring, &shared, buffer.samples);
        self.ring = Some(ring);
        self.shared = Some(shared);
    }

    fn on_error(&mut self, _error: &PlatformError) {
        if let Some(shared) = &self.shared {
            shared.failed.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for MonitorPlayer {
    fn drop(&mut self) {
        if let Some(ring) = self.ring.take() {
            self.retirer.ring(ring);
        }
        if let Some(shared) = self.shared.take() {
            self.retirer.monitor(shared);
        }
    }
}

/// An output handler to open a device stream with before its format is known: it plays
/// silence until a [`MonitorPlayer`] built for the stream's format is plugged in through
/// its [`MonitorPlug`] ([`monitor_socket`]). `AudioIo::open_output` reports the format
/// only on the stream it returns, and a player has to be built for it, off the audio
/// thread. The socket takes the player with one lock-free `try_recv` per render until it
/// has it; a stream failure reported before then is passed on to it.
#[derive(Debug)]
pub struct MonitorSocket {
    rx: Receiver<MonitorPlayer>,
    player: Option<MonitorPlayer>,
    failed: bool,
}

/// Plugs a player into its [`MonitorSocket`].
#[derive(Debug)]
pub struct MonitorPlug {
    tx: Sender<MonitorPlayer>,
}

impl MonitorPlug {
    /// Hands `player` to the socket's next render. Returns `false`, dropping `player`
    /// on this thread, when the socket is gone (its stream was dropped).
    pub fn plug(self, player: MonitorPlayer) -> bool {
        self.tx.try_send(player).is_ok()
    }
}

/// A [`MonitorSocket`] for `AudioIo::open_output` and the [`MonitorPlug`] that plugs the
/// player in once the stream's format is known.
pub fn monitor_socket() -> (MonitorSocket, MonitorPlug) {
    let (tx, rx) = crossbeam_channel::bounded(1);
    (
        MonitorSocket {
            rx,
            player: None,
            failed: false,
        },
        MonitorPlug { tx },
    )
}

impl AudioOutputHandler for MonitorSocket {
    fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
        if self.player.is_none()
            && let Ok(player) = self.rx.try_recv()
        {
            if self.failed
                && let Some(shared) = &player.shared
            {
                shared.failed.store(true, Ordering::Relaxed);
            }
            self.player = Some(player);
        }
        match &mut self.player {
            Some(player) => player.render(buffer),
            None => buffer.samples.fill(0.0),
        }
    }

    fn on_error(&mut self, error: &PlatformError) {
        match &mut self.player {
            Some(player) => player.on_error(error),
            None => self.failed = true,
        }
    }
}

/// The monitor's own ring size, independent of the inputs' rings: room for twice the
/// farthest the player keeps reading behind the newest frame (the target, a block's
/// swing and the re-anchor threshold), so that only a real stall reaches the ring's
/// half-capacity guard in `steer`.
fn monitor_ring_frames(target: i64, block_frames: usize, engine_rate: u32) -> usize {
    let threshold = (f64::from(engine_rate) * REANCHOR_ERROR_S).ceil() as usize;
    let reach = usize::try_from(target).unwrap_or(0) + block_frames + threshold;
    (2 * reach).next_power_of_two()
}

/// Builds a monitor playing on a device of `device`'s format, for a mixer at
/// `engine_rate` Hz with `block_frames`-frame blocks.
pub(crate) fn monitor(
    device: AudioFormat,
    engine_rate: u32,
    block_frames: usize,
    retirer: &Retirer,
) -> Result<Monitor, AudioError> {
    if device.sample_format() != SampleFormat::F32Interleaved {
        return Err(AudioError::UnsupportedFormat);
    }
    let rate = device.sample_rate();
    // The resampler runs from the engine rate to the device's.
    if u64::from(rate) * u64::from(MAX_DOWN_RATIO) < u64::from(engine_rate)
        || u64::from(rate) > u64::from(engine_rate) * u64::from(MAX_UP_RATIO)
    {
        return Err(AudioError::UnsupportedRate {
            rate,
            engine: engine_rate,
        });
    }
    let target = (block_frames + block_frames / 2) as i64 + i64::from(engine_rate / 50);
    let ring_frames = monitor_ring_frames(target, block_frames, engine_rate);
    let ring = Arc::new(FrameRing::new(ring_frames));
    let shared = Arc::new(MonitorShared::default());
    let feed = MonitorFeed {
        writer: Some(RingWriter::new(Arc::clone(&ring))),
        shared: Some(Arc::clone(&shared)),
        retirer: retirer.clone(),
    };
    let player = MonitorPlayer {
        ring: Some(ring),
        shared: Some(Arc::clone(&shared)),
        retirer: retirer.clone(),
        capacity: i64::try_from(ring_frames).unwrap_or(i64::MAX),
        resampler: Resampler::new(engine_rate, rate),
        layout: device.layout(),
        engine_rate: f64::from(engine_rate),
        device_rate: f64::from(rate),
        target,
        pos: None,
        stage: vec![[0.0; 2]; STAGE_FRAMES].into_boxed_slice(),
        staged_at: STAGE_FRAMES,
        carry: [[0.0; 2]; CARRY_FRAMES],
        carried: 0,
        pi: Pi::new(LOOP_BANDWIDTH_HZ, LOOP_DAMPING, MAX_CORRECTION),
        smooth: Ema::new(ERROR_SMOOTHING_S),
    };
    Ok(Monitor {
        feed,
        player,
        control: MonitorControl { shared },
    })
}

#[cfg(test)]
mod tests {
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_platform::{AudioOutputHandler, AudioRenderBuffer};

    use super::Monitor;
    use crate::{AudioError, Mixer, MixerConfig};

    const ENGINE: u32 = 48_000;
    const BLOCK: usize = 480;

    fn mixer() -> Mixer {
        Mixer::new(MixerConfig {
            block_frames: BLOCK,
            latency_frames: 480,
            ring_frames: 8192,
            ..MixerConfig::default()
        })
        .unwrap()
    }

    fn device(rate: u32, layout: ChannelLayout) -> AudioFormat {
        AudioFormat::new(rate, layout, SampleFormat::F32Interleaved).unwrap()
    }

    /// What a run played: every device frame's samples, in order.
    struct Run {
        played: Vec<f32>,
    }

    /// Runs `secs` in 1 ms steps: the feed gets a block of `[l, r]` whenever the engine
    /// clock passes one's end (none while `fed(t)` is false), and the device renders a
    /// 10 ms period whenever its clock, `1 + ppm` times the engine's, has one due.
    fn run(
        monitor: &mut Monitor,
        format: AudioFormat,
        secs: f64,
        ppm: f64,
        frame: [f32; 2],
        fed: impl Fn(f64) -> bool,
    ) -> Run {
        let rate = f64::from(format.sample_rate());
        let channels = format.channels();
        let period = (rate / 100.0) as usize;
        let block = vec![frame; BLOCK].concat();
        let mut next_block = 0i64;
        let mut rendered = 0usize;
        let mut buf = vec![0.0; period * channels];
        let mut played = Vec::new();
        for ms in 1..=(secs * 1000.0) as u64 {
            let t = ms as f64 / 1000.0;
            let engine_now = (t * f64::from(ENGINE)) as i64;
            while next_block + BLOCK as i64 <= engine_now {
                if fed(t) {
                    monitor.feed.push(next_block, &block);
                }
                next_block += BLOCK as i64;
            }
            let device_now = (t * rate * (1.0 + ppm * 1e-6)) as usize;
            while rendered + period <= device_now {
                monitor.player.render(&mut AudioRenderBuffer {
                    samples: &mut buf,
                    device_position: rendered as u64,
                    time: None,
                });
                played.extend_from_slice(&buf);
                rendered += period;
            }
        }
        Run { played }
    }

    fn assert_level(samples: &[f32], want: f32, what: &str) {
        for (i, s) in samples.iter().enumerate() {
            assert!(
                (s - want).abs() < 1e-3,
                "{what}: sample {i} is {s}, want {want}"
            );
        }
    }

    /// A mixer at the smallest ring its config allows, with a short latency, still
    /// gives the monitor a ring that holds the target: the player settles and never
    /// re-anchors.
    #[test]
    fn the_monitor_ring_holds_the_target_at_the_smallest_mixer_ring() {
        let m = Mixer::new(MixerConfig {
            block_frames: BLOCK,
            latency_frames: 480,
            ring_frames: 2048,
            ..MixerConfig::default()
        })
        .unwrap();
        let format = device(ENGINE, ChannelLayout::Stereo);
        let mut monitor = m.control().add_monitor(format).unwrap();
        // Target 1680; 2 * (1680 + 480 + 2400) = 9120, so 16384.
        assert_eq!(super::monitor_ring_frames(1680, BLOCK, ENGINE), 16_384);
        let run = run(&mut monitor, format, 5.0, 0.0, [0.5, -0.25], |_| true);
        let settled = &run.played[2 * ENGINE as usize..];
        for f in settled.chunks_exact(2) {
            assert!(
                (f[0] - 0.5).abs() < 1e-3 && (f[1] + 0.25).abs() < 1e-3,
                "{f:?}"
            );
        }
        let stats = monitor.control.stats();
        assert_eq!(stats.reanchors, 0, "{stats:?}");
    }

    /// A 48 kHz stereo device plays the feed after the target delay, at its level, and
    /// never underruns once playing.
    #[test]
    fn a_device_at_the_engine_rate_plays_the_bus() {
        let m = mixer();
        let format = device(ENGINE, ChannelLayout::Stereo);
        let mut monitor = m.control().add_monitor(format).unwrap();
        assert_eq!(monitor.player.target_frames(), 720 + 960);
        let run = run(&mut monitor, format, 5.0, 0.0, [0.5, -0.25], |_| true);
        let settled = &run.played[2 * ENGINE as usize..];
        for f in settled.chunks_exact(2) {
            assert!(
                (f[0] - 0.5).abs() < 1e-3 && (f[1] + 0.25).abs() < 1e-3,
                "{f:?}"
            );
        }
        let stats = monitor.control.stats();
        assert_eq!(stats.reanchors, 0, "{stats:?}");
        assert_eq!(stats.blocks, 5 * 100);
        assert_eq!(stats.renders, 5 * 100);
        assert!(!stats.failed);
        // Only the resampler's first stage after anchoring can find unwritten frames.
        let before = stats.underrun_frames;
        let _ = run_more(&mut monitor, format);
        assert_eq!(monitor.control.stats().underrun_frames, before);
    }

    fn run_more(monitor: &mut Monitor, format: AudioFormat) -> Run {
        // Continue on the same clocks: the blocks carry on from where the run left them.
        let rate = format.sample_rate() as usize;
        let mut buf = vec![0.0; rate / 100 * format.channels()];
        let block = vec![[0.5f32, -0.25]; BLOCK].concat();
        let mut played = Vec::new();
        let mut next = 5 * i64::from(ENGINE);
        for _ in 0..100 {
            // One 10 ms period each: the block for it, then the render.
            monitor.feed.push(next - BLOCK as i64, &block);
            next += BLOCK as i64;
            monitor.player.render(&mut AudioRenderBuffer {
                samples: &mut buf,
                device_position: 0,
                time: None,
            });
            played.extend_from_slice(&buf);
        }
        Run { played }
    }

    /// The player resamples to the device's rate and maps stereo onto its channels:
    /// averaged into mono, the front pair of 5.1.
    #[test]
    fn other_rates_and_layouts() {
        let m = mixer();
        for (rate, layout, want) in [
            (44_100, ChannelLayout::Mono, vec![0.25]),
            (
                96_000,
                ChannelLayout::Surround51,
                vec![0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            (16_000, ChannelLayout::Stereo, vec![0.5, 0.0]),
        ] {
            let format = device(rate, layout);
            let mut monitor = m.control().add_monitor(format).unwrap();
            let run = run(&mut monitor, format, 3.0, 0.0, [0.5, 0.0], |_| true);
            let channels = format.channels();
            let settled = &run.played[rate as usize * channels..];
            for (c, want) in want.iter().enumerate() {
                let channel: Vec<f32> = settled.iter().skip(c).step_by(channels).copied().collect();
                assert_level(&channel, *want, &format!("{rate} Hz channel {c}"));
            }
            assert_eq!(monitor.control.stats().reanchors, 0);
        }
    }

    /// A device clock 1000 ppm off the media clock, which would drift past the
    /// re-anchoring threshold within a minute: the controller takes it up, so the
    /// player never re-anchors and plays the bus throughout.
    #[test]
    fn device_drift_is_steered_out() {
        let m = mixer();
        let format = device(ENGINE, ChannelLayout::Stereo);
        for ppm in [1000.0, -1000.0] {
            let mut monitor = m.control().add_monitor(format).unwrap();
            let run = run(&mut monitor, format, 120.0, ppm, [0.5, 0.5], |_| true);
            let stats = monitor.control.stats();
            assert_eq!(stats.reanchors, 0, "{ppm} ppm: {stats:?}");
            let settled = &run.played[2 * 10 * ENGINE as usize..];
            assert_level(settled, 0.5, &format!("{ppm} ppm"));
        }
    }

    /// A feed that stops leaves the player reading on into silence, and when it comes
    /// back the player picks it up where it stands, without moving.
    #[test]
    fn a_feed_that_stops_plays_silence_and_picks_up() {
        let m = mixer();
        let format = device(ENGINE, ChannelLayout::Stereo);
        let mut monitor = m.control().add_monitor(format).unwrap();
        let run = run(&mut monitor, format, 6.0, 0.0, [0.5, 0.5], |t| {
            !(2.0..3.0).contains(&t)
        });
        let stats = monitor.control.stats();
        assert_eq!(stats.reanchors, 0, "{stats:?}");
        assert!(stats.underrun_frames > 40_000, "{stats:?}");
        let rate = ENGINE as usize;
        assert_level(
            &run.played[2 * (2 * rate + rate / 10)..2 * (3 * rate)],
            0.0,
            "stopped",
        );
        assert_level(&run.played[2 * 4 * rate..], 0.5, "back");
    }

    /// A device that stalls while the feed carries on comes back too far behind it,
    /// and the player moves forward onto the target.
    #[test]
    fn a_stalled_device_reanchors() {
        let m = mixer();
        let format = device(ENGINE, ChannelLayout::Stereo);
        let mut monitor = m.control().add_monitor(format).unwrap();
        let _ = run(&mut monitor, format, 5.0, 0.0, [0.5, 0.5], |_| true);
        let block = vec![[0.5f32, 0.5]; BLOCK].concat();
        // Half a second of blocks the device doesn't ask for.
        for k in 0..50 {
            monitor
                .feed
                .push(5 * i64::from(ENGINE) + k * BLOCK as i64, &block);
        }
        let mut buf = vec![0.0; 960];
        monitor.player.render(&mut AudioRenderBuffer {
            samples: &mut buf,
            device_position: 0,
            time: None,
        });
        assert_eq!(monitor.control.stats().reanchors, 1);
    }

    /// Nothing plays before the feed writes, and a stream failure is reported.
    #[test]
    fn silence_before_the_first_block_and_failure() {
        let m = mixer();
        let mut monitor = m
            .control()
            .add_monitor(device(ENGINE, ChannelLayout::Stereo))
            .unwrap();
        let mut buf = vec![1.0; 960];
        monitor.player.render(&mut AudioRenderBuffer {
            samples: &mut buf,
            device_position: 0,
            time: None,
        });
        assert_level(&buf, 0.0, "before the feed");
        assert!(!monitor.control.stats().failed);
        monitor
            .player
            .on_error(&jstream_platform::PlatformError::DeviceLost {
                what: "speakers".into(),
                reason: "unplugged".into(),
            });
        assert!(monitor.control.stats().failed);
    }

    #[test]
    fn unsupported_devices_are_refused() {
        let m = mixer();
        for rate in [8_000, 384_000] {
            assert!(matches!(
                m.control().add_monitor(device(rate, ChannelLayout::Stereo)),
                Err(AudioError::UnsupportedRate { .. })
            ));
        }
        let planar = AudioFormat::new(ENGINE, ChannelLayout::Stereo, SampleFormat::F32Planar);
        if let Ok(planar) = planar {
            assert!(matches!(
                m.control().add_monitor(planar),
                Err(AudioError::UnsupportedFormat)
            ));
        }
    }

    /// A socket plays silence until its player is plugged in, then the player; a
    /// failure before that reaches the player's stats; a plug whose socket is gone says
    /// so.
    #[test]
    fn a_socket_plays_its_player_once_plugged() {
        let m = mixer();
        let format = device(ENGINE, ChannelLayout::Stereo);
        let (mut socket, plug) = super::monitor_socket();
        let mut buf = vec![1.0; 960];
        let render = |socket: &mut super::MonitorSocket, buf: &mut Vec<f32>| {
            socket.render(&mut AudioRenderBuffer {
                samples: buf,
                device_position: 0,
                time: None,
            });
        };
        render(&mut socket, &mut buf);
        assert_level(&buf, 0.0, "unplugged");
        socket.on_error(&jstream_platform::PlatformError::DeviceLost {
            what: "speakers".into(),
            reason: "unplugged".into(),
        });
        let mut monitor = m.control().add_monitor(format).unwrap();
        assert!(!monitor.control.stats().failed);
        let block = vec![[0.5f32, 0.5]; BLOCK].concat();
        let mut frame = 0;
        for _ in 0..10 {
            monitor.feed.push(frame, &block);
            frame += BLOCK as i64;
        }
        assert!(plug.plug(monitor.player));
        let renders = monitor.control.stats().renders;
        render(&mut socket, &mut buf);
        assert_eq!(
            monitor.control.stats().renders,
            renders + 1,
            "the player renders"
        );
        assert!(
            monitor.control.stats().failed,
            "the earlier failure is passed on"
        );

        let (socket, plug) = super::monitor_socket();
        drop(socket);
        let other = m.control().add_monitor(format).unwrap();
        assert!(!plug.plug(other.player));
    }

    /// Both ends hand the ring to the collector: neither the mixer thread nor the audio
    /// thread frees it (R-RT-003).
    #[test]
    fn the_ring_is_freed_on_the_collector() {
        let m = mixer();
        let monitor = m
            .control()
            .add_monitor(device(ENGINE, ChannelLayout::Stereo))
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        monitor.player.ring.as_ref().unwrap().on_drop(tx);
        let collector = monitor.player.retirer.thread();
        let Monitor {
            feed,
            player,
            control,
        } = monitor;
        drop(feed);
        drop(player);
        drop(control);
        let freed_on = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(freed_on, collector);
    }
}
