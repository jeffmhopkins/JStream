//! The parts of Core Audio streams that make no system call, so they are tested on every
//! platform: which format a device is recorded or played in, how its buffers map to that
//! format, and how its sample times become the trait's device positions.
//!
//! A HAL device exchanges its samples as `AudioBufferList`s: one buffer per stream (or per
//! channel, for a non-interleaved stream), each holding interleaved `f32` frames of that
//! buffer's channels, in the stream's virtual format. [`plan_stream`] picks the format the
//! handler uses and a [`ChannelMap`] between it and the device's channels; each IOProc
//! cycle then interleaves an input with [`ChannelMap::gather`], or spreads the handler's
//! output over the device's buffers with [`ChannelMap::scatter`], and positions it with
//! [`SamplePositions`].

use jstream_platform::PlatformError;
use jstream_platform::media::{AudioFormat, ChannelLayout, SampleFormat};

/// `kAudioFormatLinearPCM`.
const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
/// `kAudioFormatFlagIsFloat`.
const FLAG_IS_FLOAT: u32 = 1 << 0;
/// `kAudioFormatFlagIsBigEndian`.
const FLAG_IS_BIG_ENDIAN: u32 = 1 << 1;

/// Hundred-nanosecond ticks per second, the crate's [`timebase`](crate::timebase).
const HNS_PER_SEC: u128 = 10_000_000;

/// Whether a stream's virtual format (`AudioStreamBasicDescription`'s format ID, flags
/// and bits per channel) is the native-endian 32-bit float PCM the IOProc can hand on
/// without converting. Core Audio's virtual formats are that on every Mac; anything else
/// is refused rather than misread.
pub(crate) fn is_native_f32(format_id: u32, flags: u32, bits_per_channel: u32) -> bool {
    format_id == LINEAR_PCM
        && flags & FLAG_IS_FLOAT != 0
        && (flags & FLAG_IS_BIG_ENDIAN != 0) == cfg!(target_endian = "big")
        && bits_per_channel == 32
}

/// A device's nominal sample rate (`kAudioDevicePropertyNominalSampleRate`, a `Float64`)
/// as whole frames per second, if it is one.
pub(crate) fn rate_hz(nominal: f64) -> Option<u32> {
    let rounded = nominal.round();
    let whole = nominal.is_finite()
        && rounded >= 1.0
        && rounded <= f64::from(u32::MAX)
        && (nominal - rounded).abs() < 0.01;
    let hz = rounded as u32;
    whole.then_some(hz)
}

/// Where one of the handler's channels comes from: a buffer of the device's buffer list,
/// and a channel within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source {
    buffer: usize,
    channel: usize,
}

/// How a device's input buffers become the handler's interleaved frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelMap {
    /// The channel count of each buffer the device delivers, in order.
    buffers: Vec<usize>,
    /// The source of each of the handler's channels, in order.
    sources: Vec<Source>,
}

impl ChannelMap {
    /// The handler's channel count.
    pub(crate) fn channels(&self) -> usize {
        self.sources.len()
    }

    /// Whether a cycle's buffer list has the layout the map was planned for: the same
    /// number of buffers, with the same channel counts. A device that changed its
    /// configuration while running delivers something else, which the IOProc skips.
    pub(crate) fn matches(&self, mut channels: impl ExactSizeIterator<Item = usize>) -> bool {
        channels.len() == self.buffers.len()
            && self.buffers.iter().all(|&c| channels.next() == Some(c))
    }

    /// The frames a cycle holds: the fewest that any buffer the map reads holds, where
    /// `frames_of(i)` is buffer `i`'s. Buffers it doesn't read (an empty or zero-channel
    /// one included) don't limit it. Zero when it reads none.
    pub(crate) fn frames(&self, frames_of: impl Fn(usize) -> usize) -> usize {
        self.sources
            .iter()
            .map(|s| frames_of(s.buffer))
            .min()
            .unwrap_or(0)
    }

    /// Spreads `frames` interleaved frames of the handler's from `src` over the device's
    /// buffers, from frame `start` of each: `write(buffer, index, sample)` stores one
    /// sample at `index` in buffer `buffer`, and must ignore an index the buffer doesn't
    /// hold. Device channels the map doesn't use are left as they are (the caller clears
    /// them). It doesn't allocate, so it runs on the audio thread.
    pub(crate) fn scatter(
        &self,
        src: &[f32],
        start: usize,
        frames: usize,
        mut write: impl FnMut(usize, usize, f32),
    ) {
        let n = self.sources.len();
        for (f, frame) in src.chunks_exact(n).take(frames).enumerate() {
            for (&v, dst) in frame.iter().zip(&self.sources) {
                let stride = self.buffers[dst.buffer];
                write(dst.buffer, (start + f) * stride + dst.channel, v);
            }
        }
    }

    /// Interleaves `frames` frames, starting at frame `start` of each buffer, into `out`,
    /// which holds `frames * channels()` samples; `buffer(i)` is the device's buffer `i`.
    /// A sample a short or missing buffer doesn't hold is silence. It doesn't allocate,
    /// so it runs on the audio thread.
    pub(crate) fn gather<'a>(
        &self,
        buffer: impl Fn(usize) -> Option<&'a [f32]>,
        start: usize,
        frames: usize,
        out: &mut [f32],
    ) {
        let n = self.sources.len();
        for (f, frame) in out.chunks_exact_mut(n).take(frames).enumerate() {
            for (o, src) in frame.iter_mut().zip(&self.sources) {
                let stride = self.buffers[src.buffer];
                let at = (start + f) * stride + src.channel;
                *o = buffer(src.buffer)
                    .and_then(|b| b.get(at))
                    .copied()
                    .unwrap_or(0.0);
            }
        }
    }
}

/// How a device is recorded or played: the handler's format and which device channels
/// its channels are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamPlan {
    /// Interleaved `f32` at the device's nominal rate.
    pub(crate) format: AudioFormat,
    /// From the device's buffers to the handler's channels.
    pub(crate) map: ChannelMap,
}

/// Chooses how to record or play a device whose buffers in that direction have `buffers`
/// channels each, at `rate` frames per second.
///
/// One channel is mono and two are stereo, in the device's order. Any other count (a
/// multichannel interface, a microphone array, surround speakers) is stereo on the device's
/// preferred stereo pair (`kAudioDevicePropertyPreferredChannelsForStereo`, 1-based
/// channel numbers across all its buffers), or its first two channels when it names no
/// valid pair. The other channels are not mixed in, so this is a selection, not the
/// downmix the `jstream-platform` README proposes for other layouts, and a 5.1 input is
/// recorded or played as its preferred pair too: the pair Core Audio itself uses for
/// stereo. When playing, the other channels get silence. Nothing is resampled (ADR-0014
/// leaves that to the engine).
///
/// # Errors
/// [`PlatformError::Unsupported`] for a device without channels in that direction.
pub(crate) fn plan_stream(
    rate: u32,
    buffers: &[u32],
    stereo_pair: Option<[u32; 2]>,
) -> Result<StreamPlan, PlatformError> {
    let buffers: Vec<usize> = buffers.iter().map(|&c| c as usize).collect();
    let total: usize = buffers.iter().sum();
    // Device channel number (0-based, across buffers) to its buffer and offset.
    let locate = |mut ch: usize| {
        for (buffer, &count) in buffers.iter().enumerate() {
            if ch < count {
                return Some(Source {
                    buffer,
                    channel: ch,
                });
            }
            ch -= count;
        }
        None
    };
    let (layout, picked): (ChannelLayout, Vec<usize>) = match total {
        0 => {
            return Err(PlatformError::Unsupported {
                what: "an audio device without channels in that direction".into(),
            });
        }
        1 => (ChannelLayout::Mono, vec![0]),
        2 => (ChannelLayout::Stereo, vec![0, 1]),
        _ => {
            let valid = |c: u32| (1..=total).contains(&(c as usize));
            let pair = stereo_pair
                .filter(|[l, r]| l != r && valid(*l) && valid(*r))
                .map_or([0, 1], |[l, r]| [l as usize - 1, r as usize - 1]);
            (ChannelLayout::Stereo, pair.to_vec())
        }
    };
    let sources = picked.into_iter().filter_map(locate).collect();
    Ok(StreamPlan {
        format: AudioFormat::new(rate, layout, SampleFormat::F32Interleaved)?,
        map: ChannelMap { buffers, sources },
    })
}

/// The duration of `frames` frames at `rate` in 100 ns ticks, rounding toward zero: how
/// far into an IOProc cycle a later chunk of it starts.
pub(crate) fn frames_hns(frames: u64, rate: u32) -> i64 {
    if rate == 0 {
        return 0;
    }
    i64::try_from(u128::from(frames) * HNS_PER_SEC / u128::from(rate)).unwrap_or(i64::MAX)
}

/// The trait's device positions from Core Audio's sample times: frames since the stream
/// started, counting frames lost in a glitch, and carried across a move to a new device
/// (whose sample times have nothing to do with the old one's).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SamplePositions {
    /// The current device's sample time at stream position `base`.
    origin: Option<f64>,
    /// The stream position the current device started at.
    base: u64,
    /// One past the last frame reported.
    end: u64,
    /// The next packet is the first on a new device.
    moved: bool,
}

impl SamplePositions {
    /// The position of a packet of `frames` frames whose first frame has the device's
    /// sample time `sample_time` (`None` when the time stamp doesn't carry a valid one),
    /// and whether it is discontinuous with the packet before it.
    ///
    /// A packet later than expected moves the position forward (frames were lost). A
    /// packet earlier than expected, which a device restarting its clock reports, is
    /// placed right after the last one and flagged.
    pub(crate) fn packet(&mut self, sample_time: Option<f64>, frames: u64) -> (u64, bool) {
        let mut gap = std::mem::take(&mut self.moved);
        let pos = match sample_time.filter(|t| t.is_finite()) {
            None => self.end,
            Some(t) => {
                let origin = *self.origin.get_or_insert(t);
                let ahead = (t - origin).round();
                let pos = self.base.saturating_add(ahead.max(0.0) as u64);
                if pos < self.end || ahead < 0.0 {
                    // The device's clock went back: re-anchor so this packet follows on.
                    let since = (self.end - self.base) as f64;
                    self.origin = Some(t - since);
                    gap = true;
                    self.end
                } else {
                    gap |= pos != self.end;
                    pos
                }
            }
        };
        self.end = pos.saturating_add(frames);
        (pos, gap)
    }

    /// The stream moved to a new device: its positions continue after the last frame,
    /// and its first packet is flagged as discontinuous.
    pub(crate) fn new_device(&mut self) {
        self.base = self.end;
        self.origin = None;
        self.moved = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The buffer lookup `gather` takes, over a list of slices.
    fn at<'a>(buffers: &'a [&'a [f32]]) -> impl Fn(usize) -> Option<&'a [f32]> {
        move |i| buffers.get(i).copied()
    }

    #[test]
    fn only_native_f32_pcm_is_taken() {
        let native_big = cfg!(target_endian = "big");
        let endian = if native_big { FLAG_IS_BIG_ENDIAN } else { 0 };
        // kAudioFormatFlagsNativeFloatPacked: float, packed, native endian.
        assert!(is_native_f32(
            LINEAR_PCM,
            FLAG_IS_FLOAT | 1 << 3 | endian,
            32
        ));
        assert!(!is_native_f32(LINEAR_PCM, FLAG_IS_FLOAT | endian, 64));
        assert!(
            !is_native_f32(LINEAR_PCM, 1 << 2 | endian, 32),
            "signed integer"
        );
        assert!(!is_native_f32(
            LINEAR_PCM,
            FLAG_IS_FLOAT | (FLAG_IS_BIG_ENDIAN ^ endian),
            32
        ));
        assert!(!is_native_f32(
            u32::from_be_bytes(*b"aac "),
            FLAG_IS_FLOAT,
            32
        ));
    }

    #[test]
    fn nominal_rates_round_to_whole_hertz() {
        assert_eq!(rate_hz(48_000.0), Some(48_000));
        assert_eq!(rate_hz(44_100.000_001), Some(44_100));
        assert_eq!(rate_hz(44_099.5), None);
        assert_eq!(rate_hz(0.0), None);
        assert_eq!(rate_hz(-48_000.0), None);
        assert_eq!(rate_hz(f64::NAN), None);
        assert_eq!(rate_hz(1e12), None);
    }

    #[test]
    fn mono_and_stereo_keep_the_device_order() {
        let mono = plan_stream(48_000, &[1], None).expect("plannable");
        assert_eq!(mono.format.layout(), ChannelLayout::Mono);
        assert_eq!(mono.format.sample_rate(), 48_000);
        assert_eq!(mono.format.sample_format(), SampleFormat::F32Interleaved);
        let stereo = plan_stream(44_100, &[2], Some([2, 1])).expect("plannable");
        assert_eq!(stereo.format.layout(), ChannelLayout::Stereo);
        let mut out = [0.0; 4];
        stereo
            .map
            .gather(at(&[&[1.0, 2.0, 3.0, 4.0]]), 0, 2, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0], "a stereo device is taken as is");
    }

    #[test]
    fn non_interleaved_stereo_is_interleaved() {
        let p = plan_stream(48_000, &[1, 1], None).expect("plannable");
        assert_eq!(p.map.channels(), 2);
        assert!(p.map.matches([1, 1].into_iter()));
        assert!(!p.map.matches([2].into_iter()));
        assert!(!p.map.matches([1, 1, 1].into_iter()));
        let mut out = [0.0; 6];
        p.map
            .gather(at(&[&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]]), 0, 3, &mut out);
        assert_eq!(out, [1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn multichannel_devices_record_their_preferred_pair() {
        // An interface with an 8-channel stream and a 2-channel one, preferring 9 and 10.
        let p = plan_stream(96_000, &[8, 2], Some([9, 10])).expect("plannable");
        assert_eq!(p.format.layout(), ChannelLayout::Stereo);
        let a: Vec<f32> = (0..16u8).map(f32::from).collect(); // 2 frames of 8
        let b = [100.0, 101.0, 102.0, 103.0]; // 2 frames of 2
        let mut out = [0.0; 4];
        p.map.gather(at(&[&a, &b]), 0, 2, &mut out);
        assert_eq!(out, [100.0, 101.0, 102.0, 103.0]);
        // Starting one frame in.
        let mut one = [0.0; 2];
        p.map.gather(at(&[&a, &b]), 1, 1, &mut one);
        assert_eq!(one, [102.0, 103.0]);
        // A pair across buffers, and swapped.
        let across = plan_stream(96_000, &[8, 2], Some([10, 3])).expect("plannable");
        across.map.gather(at(&[&a, &b]), 0, 1, &mut one);
        assert_eq!(one, [101.0, 2.0]);
    }

    #[test]
    fn an_invalid_preferred_pair_falls_back_to_the_first_two() {
        for pair in [None, Some([0, 1]), Some([3, 3]), Some([1, 5])] {
            let p = plan_stream(48_000, &[4], pair).expect("plannable");
            let mut out = [0.0; 2];
            p.map.gather(at(&[&[1.0, 2.0, 3.0, 4.0]]), 0, 1, &mut out);
            assert_eq!(out, [1.0, 2.0], "{pair:?}");
        }
    }

    #[test]
    fn a_device_without_inputs_is_refused() {
        assert!(matches!(
            plan_stream(48_000, &[], None),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(matches!(
            plan_stream(48_000, &[0, 0], None),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(plan_stream(0, &[2], None).is_err(), "a zero rate");
    }

    #[test]
    fn unread_buffers_do_not_limit_the_frames() {
        // An empty, zero-channel buffer in front of the stereo one.
        let p = plan_stream(48_000, &[0, 2], None).expect("plannable");
        let sizes = [0, 512];
        assert_eq!(p.map.frames(|i| sizes[i]), 512);
        // Among the buffers it reads, the shortest wins.
        let q = plan_stream(48_000, &[1, 1], None).expect("plannable");
        let sizes = [512, 256];
        assert_eq!(q.map.frames(|i| sizes[i]), 256);
    }

    #[test]
    fn output_is_spread_over_the_preferred_pair() {
        // A 4-channel output in two stereo buffers, playing stereo on channels 3 and 4.
        let p = plan_stream(48_000, &[2, 2], Some([3, 4])).expect("plannable");
        let mut bufs = [[0.0f32; 4], [0.0f32; 4]];
        let src = [1.0, 2.0, 3.0, 4.0]; // 2 frames of stereo
        p.map.scatter(&src, 0, 2, |b, i, v| {
            if let Some(s) = bufs[b].get_mut(i) {
                *s = v;
            }
        });
        assert_eq!(bufs[0], [0.0; 4], "unused channels untouched");
        assert_eq!(bufs[1], [1.0, 2.0, 3.0, 4.0]);
        // From one frame in, and an index past the buffer is ignored by `write`.
        let mut mono = [0.0f32; 2];
        let m = plan_stream(48_000, &[1], None).expect("plannable");
        m.map.scatter(&[5.0, 6.0], 1, 2, |_, i, v| {
            if let Some(s) = mono.get_mut(i) {
                *s = v;
            }
        });
        assert_eq!(mono, [0.0, 5.0]);
    }

    #[test]
    fn short_buffers_read_as_silence() {
        let p = plan_stream(48_000, &[2], None).expect("plannable");
        let mut out = [9.0; 4];
        p.map.gather(at(&[&[1.0, 2.0, 3.0]]), 0, 2, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 0.0]);
        p.map.gather(at(&[]), 0, 2, &mut out);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn chunk_offsets_are_in_100ns_ticks() {
        assert_eq!(frames_hns(48_000, 48_000), 10_000_000);
        assert_eq!(frames_hns(512, 48_000), 106_666);
        assert_eq!(frames_hns(1, 0), 0);
    }

    #[test]
    fn positions_count_from_the_first_sample_time() {
        let mut p = SamplePositions::default();
        assert_eq!(p.packet(Some(123_456.0), 512), (0, false));
        assert_eq!(p.packet(Some(123_968.0), 512), (512, false));
        // 256 frames lost.
        assert_eq!(p.packet(Some(124_736.0), 512), (1280, true));
        // No valid sample time: assumed continuous.
        assert_eq!(p.packet(None, 512), (1792, false));
    }

    #[test]
    fn a_clock_going_back_follows_on_and_is_flagged() {
        let mut p = SamplePositions::default();
        assert_eq!(p.packet(Some(10_000.0), 100), (0, false));
        assert_eq!(p.packet(Some(5.0), 100), (100, true));
        // Re-anchored on the new times.
        assert_eq!(p.packet(Some(105.0), 100), (200, false));
    }

    #[test]
    fn a_new_device_continues_the_positions() {
        let mut p = SamplePositions::default();
        p.packet(Some(1_000.0), 480);
        p.packet(Some(1_480.0), 480);
        p.new_device();
        assert_eq!(p.packet(Some(77.0), 480), (960, true));
        assert_eq!(p.packet(Some(557.0), 480), (1440, false));
    }
}
