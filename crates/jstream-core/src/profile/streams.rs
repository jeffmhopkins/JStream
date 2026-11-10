//! What `profile.get` reports past the drawers (protocol 0.92, #1220): each encoded
//! stream's frames from its encoder to where it sends them, and the audio mixer's blocks.
//!
//! An output and an H.264 preview stream each register a [`StreamMeter`] while they run,
//! whose `read` loads its encoder's and sender's running totals: atomics, or locks no
//! real-time thread waits on. The mixer thread counts its blocks into a [`MixerMeter`]
//! with relaxed atomics only (R-RT-001), and the program stage registers a reader of it
//! with its master bus's skips. The [`Profiler`](super::Profiler) reads all of them into
//! each sample, and a window is the difference between two samples.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use jstream_protocol::{MixerProfile, ProfileStream, StreamProfile};

use crate::lock;

use super::{finite, ms};

/// One encoded stream's running totals, since its meter was registered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamTotals {
    /// Video frames handed to its encoder, dropped ones included.
    pub(crate) frames_in: u64,
    /// Video frames its encoder took.
    pub(crate) encoded: u64,
    /// Video frames it sent on.
    pub(crate) frames_out: u64,
    /// Video frames lost between its encoder's input and where it sends them.
    pub(crate) dropped: u64,
    /// Frames its encoder took more than a frame period after they were handed over.
    pub(crate) late: u64,
    /// The encoder's time over every frame, in nanoseconds.
    pub(crate) encode_nanos: u64,
    /// The bytes it sent on.
    pub(crate) bytes: u64,
}

/// The frame rate and bit rate a stream's encoders were opened at: its video
/// session's `video`, plus its audio session's `audio` for an output.
pub(crate) fn encoder_target(
    video: &jstream_encode::StreamInfo,
    audio: Option<&jstream_encode::StreamInfo>,
) -> (f32, u32) {
    use jstream_encode::EncoderSettings;
    use jstream_platform::RateControl;
    let (fps, video_kbps) = match &video.settings {
        EncoderSettings::Video(config) => {
            let period = config.frame_period;
            let fps = if period.num() == 0 {
                0.0
            } else {
                period.den() as f32 / period.num() as f32
            };
            let kbps = match config.rate_control {
                RateControl::Cbr { kbps } | RateControl::Vbr { kbps, .. } => kbps,
                _ => 0,
            };
            (fps, kbps)
        }
        EncoderSettings::Audio(_) => (0.0, 0),
    };
    let audio_kbps = match audio.map(|audio| &audio.settings) {
        Some(EncoderSettings::Audio(config)) => config.kbps,
        _ => 0,
    };
    (fps, video_kbps.saturating_add(audio_kbps))
}

/// Reads one stream's running totals.
pub(crate) type ReadStream = Box<dyn Fn() -> StreamTotals + Send + Sync>;

/// What a stream is, for its profile.
pub(crate) struct StreamMeter {
    /// Which stream.
    pub(crate) stream: ProfileStream,
    /// The frame rate it is encoded at.
    pub(crate) fps: f32,
    /// The bit rate its encoders are set to.
    pub(crate) target_kbps: u32,
    /// Reads its totals.
    pub(crate) read: ReadStream,
}

/// The streams registered now, by a number of their own: a stream registered again (an
/// output's next run) counts from zero.
#[derive(Default)]
pub(crate) struct StreamMeters {
    next: u64,
    meters: BTreeMap<u64, StreamMeter>,
}

impl StreamMeters {
    fn add(&mut self, meter: StreamMeter) -> u64 {
        self.next = self.next.saturating_add(1);
        self.meters.insert(self.next, meter);
        self.next
    }

    /// Every stream's totals now.
    pub(super) fn read(&self) -> BTreeMap<u64, StreamReading> {
        self.meters
            .iter()
            .map(|(id, meter)| {
                (
                    *id,
                    StreamReading {
                        stream: meter.stream.clone(),
                        fps: meter.fps,
                        target_kbps: meter.target_kbps,
                        totals: (meter.read)(),
                    },
                )
            })
            .collect()
    }
}

/// Where the threads that don't hold the engine register their streams: the preview
/// hub's. Cheap to clone.
#[derive(Clone, Default)]
pub(crate) struct StreamRegistry(pub(super) Arc<Mutex<StreamMeters>>);

impl StreamRegistry {
    /// Meters `meter` until the returned guard drops.
    pub(crate) fn add(&self, meter: StreamMeter) -> StreamGuard {
        add_stream(&self.0, meter)
    }
}

/// Registers `meter` with `meters` until the returned guard drops.
pub(crate) fn add_stream(meters: &Arc<Mutex<StreamMeters>>, meter: StreamMeter) -> StreamGuard {
    let id = lock(meters).add(meter);
    StreamGuard {
        meters: Arc::downgrade(meters),
        id,
    }
}

/// Removes a stream's meter when dropped.
pub(crate) struct StreamGuard {
    meters: Weak<Mutex<StreamMeters>>,
    id: u64,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Some(meters) = self.meters.upgrade() {
            lock(&meters).meters.remove(&self.id);
        }
    }
}

/// One stream as one sample read it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StreamReading {
    stream: ProfileStream,
    fps: f32,
    target_kbps: u32,
    totals: StreamTotals,
}

/// The streams' profiles between `base` and `now`, most frames lost first, then most
/// late; a stream that sent and lost nothing in the window is left out.
pub(super) fn stream_profiles(
    now: &BTreeMap<u64, StreamReading>,
    base: Option<&BTreeMap<u64, StreamReading>>,
    span: Duration,
    top: usize,
) -> Vec<StreamProfile> {
    let mut streams: Vec<StreamProfile> = now
        .iter()
        .filter_map(|(id, reading)| {
            let before = base
                .and_then(|base| base.get(id))
                .map(|before| before.totals)
                .unwrap_or_default();
            let now = reading.totals;
            let frames_in = now.frames_in.saturating_sub(before.frames_in);
            let frames_out = now.frames_out.saturating_sub(before.frames_out);
            let dropped = now.dropped.saturating_sub(before.dropped);
            if frames_in == 0 && frames_out == 0 && dropped == 0 {
                return None;
            }
            let encoded = now.encoded.saturating_sub(before.encoded);
            let encode = now.encode_nanos.saturating_sub(before.encode_nanos);
            let bytes = now.bytes.saturating_sub(before.bytes);
            Some(StreamProfile {
                stream: reading.stream.clone(),
                fps: finite(reading.fps),
                frames_in,
                encoded,
                frames_out,
                dropped,
                late: now.late.saturating_sub(before.late),
                encode_ms: ms(encode.checked_div(encoded).unwrap_or(0)),
                kbps: kbps(bytes, span),
                target_kbps: reading.target_kbps,
            })
        })
        .collect();
    streams.sort_by_key(|stream| std::cmp::Reverse((stream.dropped, stream.late)));
    streams.truncate(top);
    streams
}

/// `bytes` over `span`, in kilobits a second.
fn kbps(bytes: u64, span: Duration) -> u32 {
    let millis = span.as_millis();
    if millis == 0 {
        return 0;
    }
    // Bits per millisecond is kilobits per second.
    u32::try_from(u128::from(bytes).saturating_mul(8) / millis).unwrap_or(u32::MAX)
}

/// What the mixer thread counts as it mixes: relaxed atomics only, so counting never
/// waits (R-RT-001).
#[derive(Debug, Default)]
pub(crate) struct MixerMeter {
    blocks: AtomicU64,
    late_blocks: AtomicU64,
    /// The longest mix since the profiler last read it, in nanoseconds.
    max_nanos: AtomicU64,
    /// A block's length, in nanoseconds.
    block_nanos: AtomicU64,
}

impl MixerMeter {
    /// A meter for blocks `block` long.
    pub(crate) fn new(block: Duration) -> Self {
        let meter = Self::default();
        meter.block_nanos.store(nanos(block), Ordering::Relaxed);
        meter
    }

    /// Counts one block whose mix took `took`.
    pub(crate) fn mixed(&self, took: Duration) {
        let took = nanos(took);
        self.blocks.fetch_add(1, Ordering::Relaxed);
        if took > self.block_nanos.load(Ordering::Relaxed) {
            self.late_blocks.fetch_add(1, Ordering::Relaxed);
        }
        self.max_nanos.fetch_max(took, Ordering::Relaxed);
    }

    /// The totals, with the longest mix since the last read; starts a new longest.
    fn take(&self) -> (u64, u64, u64, u64) {
        (
            self.blocks.load(Ordering::Relaxed),
            self.late_blocks.load(Ordering::Relaxed),
            self.max_nanos.swap(0, Ordering::Relaxed),
            self.block_nanos.load(Ordering::Relaxed),
        )
    }
}

fn nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}

/// The running stage's mixer, and how to read its master bus's skipped frames.
pub(crate) struct MixerSource {
    pub(crate) meter: Arc<MixerMeter>,
    pub(crate) skipped: Box<dyn Fn() -> u64 + Send + Sync>,
}

/// The mixer source of the running stage, if any, numbered so a later stage's totals
/// count from zero.
#[derive(Default)]
pub(crate) struct MixerSlot {
    run: u64,
    source: Option<MixerSource>,
}

/// Puts `source` in `slot` until the returned guard drops.
pub(crate) fn add_mixer(slot: &Arc<Mutex<MixerSlot>>, source: MixerSource) -> MixerGuard {
    let mut held = lock(slot);
    held.run = held.run.saturating_add(1);
    held.source = Some(source);
    MixerGuard {
        slot: Arc::downgrade(slot),
        run: held.run,
    }
}

/// Clears the mixer source when dropped, unless a later stage replaced it.
pub(crate) struct MixerGuard {
    slot: Weak<Mutex<MixerSlot>>,
    run: u64,
}

impl Drop for MixerGuard {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.upgrade() {
            let mut slot = lock(&slot);
            if slot.run == self.run {
                slot.source = None;
            }
        }
    }
}

/// The audio inputs' running totals: frames played as silence for want of audio, and
/// frames dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputTotals {
    pub(crate) underrun_frames: u64,
    pub(crate) dropped_frames: u64,
}

/// The mixer and the inputs as one sample read them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MixerReading {
    /// The stage it was read from; `None` with no stage running.
    run: Option<u64>,
    blocks: u64,
    late_blocks: u64,
    /// The longest mix since the sample before.
    pub(super) max_nanos: u64,
    block_nanos: u64,
    skipped: u64,
    inputs: InputTotals,
}

impl MixerSlot {
    /// Reads the mixer and the inputs' totals `inputs`.
    pub(super) fn read(&self, inputs: InputTotals) -> MixerReading {
        match &self.source {
            Some(source) => {
                let (blocks, late_blocks, max_nanos, block_nanos) = source.meter.take();
                MixerReading {
                    run: Some(self.run),
                    blocks,
                    late_blocks,
                    max_nanos,
                    block_nanos,
                    skipped: (source.skipped)(),
                    inputs,
                }
            }
            None => MixerReading {
                inputs,
                ..MixerReading::default()
            },
        }
    }
}

/// The mixer's profile between `base` and `now`, whose longest mix is `max_nanos`;
/// `None` when it mixed no block in the window.
pub(super) fn mixer_profile(
    now: &MixerReading,
    base: Option<&MixerReading>,
    max_nanos: u64,
) -> Option<MixerProfile> {
    now.run?;
    // A base from another stage, or none, counts the stage's blocks from zero.
    let stage = base.filter(|base| base.run == now.run);
    let blocks = now.blocks.saturating_sub(stage.map_or(0, |b| b.blocks));
    if blocks == 0 {
        return None;
    }
    // The inputs outlive stages, so their counts are against any base.
    let inputs = base.map(|base| base.inputs).unwrap_or_default();
    Some(MixerProfile {
        blocks,
        late_blocks: now
            .late_blocks
            .saturating_sub(stage.map_or(0, |b| b.late_blocks)),
        block_max_ms: ms(max_nanos),
        block_ms: ms(now.block_nanos),
        skipped_frames: now.skipped.saturating_sub(stage.map_or(0, |b| b.skipped)),
        underrun_frames: now
            .inputs
            .underrun_frames
            .saturating_sub(inputs.underrun_frames),
        dropped_frames: now
            .inputs
            .dropped_frames
            .saturating_sub(inputs.dropped_frames),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Finite32, OutputId};

    fn reading(totals: StreamTotals) -> StreamReading {
        StreamReading {
            stream: ProfileStream::Output {
                output_id: OutputId::new("rec"),
            },
            fps: 60.0,
            target_kbps: 6000,
            totals,
        }
    }

    #[test]
    fn a_stream_is_its_totals_since_the_base_and_idle_ones_are_left_out() {
        let base = BTreeMap::from([(
            1,
            reading(StreamTotals {
                frames_in: 100,
                encoded: 100,
                frames_out: 100,
                encode_nanos: 100_000_000,
                bytes: 1_000,
                ..StreamTotals::default()
            }),
        )]);
        let now = BTreeMap::from([
            (
                1,
                reading(StreamTotals {
                    frames_in: 160,
                    encoded: 158,
                    frames_out: 157,
                    dropped: 3,
                    late: 4,
                    encode_nanos: 100_000_000 + 58 * 2_000_000,
                    bytes: 1_000 + 750_000,
                }),
            ),
            // Registered after the base, and idle.
            (2, reading(StreamTotals::default())),
        ]);
        let streams = stream_profiles(&now, Some(&base), Duration::from_secs(1), 20);
        assert_eq!(streams.len(), 1);
        let stream = &streams[0];
        assert_eq!(
            (
                stream.frames_in,
                stream.encoded,
                stream.frames_out,
                stream.dropped,
                stream.late
            ),
            (60, 58, 57, 3, 4)
        );
        assert_eq!(stream.encode_ms, Finite32::new(2.0).expect("finite"));
        assert_eq!(stream.kbps, 6000);
        assert_eq!(stream.target_kbps, 6000);
    }

    #[test]
    fn the_mixer_counts_late_blocks_and_its_longest_and_a_new_stage_starts_over() {
        let slot = Arc::new(Mutex::new(MixerSlot::default()));
        let meter = Arc::new(MixerMeter::new(Duration::from_millis(10)));
        let guard = add_mixer(
            &slot,
            MixerSource {
                meter: Arc::clone(&meter),
                skipped: Box::new(|| 7),
            },
        );
        let inputs = InputTotals {
            underrun_frames: 10,
            dropped_frames: 1,
        };
        let base = lock(&slot).read(inputs);
        meter.mixed(Duration::from_millis(2));
        meter.mixed(Duration::from_millis(12));
        let now = lock(&slot).read(InputTotals {
            underrun_frames: 490,
            dropped_frames: 1,
        });
        let profile = mixer_profile(&now, Some(&base), now.max_nanos).expect("it mixed");
        assert_eq!((profile.blocks, profile.late_blocks), (2, 1));
        assert_eq!(profile.block_max_ms, Finite32::new(12.0).expect("finite"));
        assert_eq!(profile.block_ms, Finite32::new(10.0).expect("finite"));
        assert_eq!((profile.underrun_frames, profile.dropped_frames), (480, 0));
        assert_eq!(profile.skipped_frames, 0);
        // The longest is taken by the read.
        assert_eq!(lock(&slot).read(inputs).max_nanos, 0);

        drop(guard);
        assert_eq!(
            mixer_profile(&lock(&slot).read(inputs), Some(&base), 0),
            None
        );
        // A new stage's blocks count from zero, though its base came from the old one.
        let meter = Arc::new(MixerMeter::new(Duration::from_millis(10)));
        let _guard = add_mixer(
            &slot,
            MixerSource {
                meter: Arc::clone(&meter),
                skipped: Box::new(|| 0),
            },
        );
        meter.mixed(Duration::from_millis(1));
        let profile = mixer_profile(&lock(&slot).read(inputs), Some(&now), 0).expect("it mixed");
        assert_eq!(profile.blocks, 1);
    }
}
