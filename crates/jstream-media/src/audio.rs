//! Audio formats and sample buffers.
//!
//! Samples are `f32`, nominally in `-1.0..=1.0`. A *frame* is one sample for every
//! channel at one instant.

use crate::{MediaError, MediaTime, Timebase};

/// How samples are laid out in a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// `f32` frames one after another: `L0 R0 L1 R1 …`.
    F32Interleaved,
    /// `f32` channels one after another: `L0 L1 … R0 R1 …`.
    F32Planar,
}

impl SampleFormat {
    /// Bytes per sample: 4 for both `f32` layouts.
    pub fn bytes_per_sample(self) -> usize {
        4
    }

    /// Whether each channel is stored contiguously.
    pub fn is_planar(self) -> bool {
        matches!(self, Self::F32Planar)
    }
}

/// The channels in a stream, in storage order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChannelLayout {
    /// One channel.
    Mono,
    /// Front left, front right.
    Stereo,
    /// 5.1: front left, front right, center, LFE, back left, back right (the
    /// WAVEFORMATEXTENSIBLE order).
    Surround51,
}

impl ChannelLayout {
    /// The number of channels; always at least 1.
    pub fn channel_count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
        }
    }
}

/// Sample rate, channel layout and sample format of an audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    sample_rate: u32,
    layout: ChannelLayout,
    sample_format: SampleFormat,
}

impl AudioFormat {
    /// Creates a format. `sample_rate` is in frames per second.
    ///
    /// # Errors
    /// [`MediaError::ZeroSampleRate`] when `sample_rate` is zero.
    pub fn new(
        sample_rate: u32,
        layout: ChannelLayout,
        sample_format: SampleFormat,
    ) -> Result<Self, MediaError> {
        if sample_rate == 0 {
            return Err(MediaError::ZeroSampleRate);
        }
        Ok(Self {
            sample_rate,
            layout,
            sample_format,
        })
    }

    /// Frames per second; always greater than zero.
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    /// The channel layout.
    pub fn layout(self) -> ChannelLayout {
        self.layout
    }

    /// The sample layout.
    pub fn sample_format(self) -> SampleFormat {
        self.sample_format
    }

    /// The number of channels.
    pub fn channels(self) -> usize {
        self.layout.channel_count()
    }

    /// One tick per frame: `1 / sample_rate` seconds.
    pub fn timebase(self) -> Timebase {
        Timebase::per_second(self.sample_rate).expect("invariant: sample_rate is non-zero")
    }
}

/// A block of audio frames with a start timestamp.
///
/// Invariant: the sample count is `frames * channels`, so every frame is complete. In
/// planar buffers channel `c` occupies `samples[c * frames .. (c + 1) * frames]`.
/// Construction allocates; the real-time mixer reuses buffers instead of building new
/// ones (docs/RULES.md#R-RT-001).
///
/// ```
/// use jstream_media::{AudioBuffer, AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
///
/// let fmt = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)?;
/// let buf = AudioBuffer::new(fmt, MediaTime::zero(Timebase::NANOS), vec![0.1, -0.1, 0.2, -0.2])?;
/// assert_eq!(buf.frames(), 2);
/// assert_eq!(buf.sample(1, 1), Some(-0.2));
/// assert_eq!(buf.duration()?.ticks(), 2); // in 1/48000 s
/// # Ok::<(), jstream_media::MediaError>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBuffer {
    format: AudioFormat,
    pts: MediaTime,
    frames: usize,
    samples: Vec<f32>,
}

impl AudioBuffer {
    /// A buffer of `samples`, laid out as `format` says, starting at `pts`.
    ///
    /// # Errors
    /// [`MediaError::SampleCountMismatch`] when `samples` isn't a whole number of frames.
    pub fn new(format: AudioFormat, pts: MediaTime, samples: Vec<f32>) -> Result<Self, MediaError> {
        let channels = format.channels();
        if !samples.len().is_multiple_of(channels) {
            return Err(MediaError::SampleCountMismatch {
                samples: samples.len(),
                channels,
            });
        }
        Ok(Self {
            format,
            pts,
            frames: samples.len() / channels,
            samples,
        })
    }

    /// `frames` frames of silence starting at `pts`.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the sample count doesn't fit in `usize`.
    pub fn silence(format: AudioFormat, pts: MediaTime, frames: usize) -> Result<Self, MediaError> {
        let len = frames
            .checked_mul(format.channels())
            .ok_or(MediaError::Overflow {
                what: "sample count",
            })?;
        Self::new(format, pts, vec![0.0; len])
    }

    /// The buffer's format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The timestamp of the first frame.
    pub fn pts(&self) -> MediaTime {
        self.pts
    }

    /// Sets the timestamp of the first frame.
    pub fn set_pts(&mut self, pts: MediaTime) {
        self.pts = pts;
    }

    /// The number of frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// All samples in storage order.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// All samples in storage order, mutably. The length can't change, so the frame
    /// invariant holds.
    pub fn samples_mut(&mut self) -> &mut [f32] {
        &mut self.samples
    }

    /// The sample for `channel` in `frame`, or `None` when either is out of range.
    pub fn sample(&self, frame: usize, channel: usize) -> Option<f32> {
        self.index(frame, channel)
            .and_then(|i| self.samples.get(i).copied())
    }

    /// Channel `channel` of a planar buffer, or `None` for an interleaved buffer or an
    /// out-of-range channel.
    pub fn channel(&self, channel: usize) -> Option<&[f32]> {
        if !self.format.sample_format.is_planar() || channel >= self.format.channels() {
            return None;
        }
        let start = channel.checked_mul(self.frames)?;
        self.samples.get(start..start.checked_add(self.frames)?)
    }

    /// The buffer's length as a time span in `1 / sample_rate` ticks.
    ///
    /// # Errors
    /// [`MediaError::Overflow`] when the frame count doesn't fit in `i64`.
    pub fn duration(&self) -> Result<MediaTime, MediaError> {
        let ticks = i64::try_from(self.frames).map_err(|_| MediaError::Overflow {
            what: "audio duration",
        })?;
        Ok(MediaTime::new(ticks, self.format.timebase()))
    }

    /// The samples, giving up the buffer.
    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }

    fn index(&self, frame: usize, channel: usize) -> Option<usize> {
        let channels = self.format.channels();
        if frame >= self.frames || channel >= channels {
            return None;
        }
        if self.format.sample_format.is_planar() {
            channel.checked_mul(self.frames)?.checked_add(frame)
        } else {
            frame.checked_mul(channels)?.checked_add(channel)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioBuffer, AudioFormat, ChannelLayout, SampleFormat};
    use crate::{MediaError, MediaTime, Rounding, Timebase};

    fn fmt(layout: ChannelLayout, sf: SampleFormat) -> AudioFormat {
        AudioFormat::new(48_000, layout, sf).unwrap()
    }

    fn t0() -> MediaTime {
        MediaTime::zero(Timebase::NANOS)
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        assert_eq!(
            AudioFormat::new(0, ChannelLayout::Stereo, SampleFormat::F32Planar),
            Err(MediaError::ZeroSampleRate)
        );
    }

    #[test]
    fn channel_counts() {
        assert_eq!(ChannelLayout::Mono.channel_count(), 1);
        assert_eq!(ChannelLayout::Stereo.channel_count(), 2);
        assert_eq!(ChannelLayout::Surround51.channel_count(), 6);
        assert_eq!(SampleFormat::F32Planar.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::F32Interleaved.bytes_per_sample(), 4);
    }

    #[test]
    fn partial_frames_are_rejected() {
        let f = fmt(ChannelLayout::Surround51, SampleFormat::F32Interleaved);
        assert_eq!(
            AudioBuffer::new(f, t0(), vec![0.0; 13]),
            Err(MediaError::SampleCountMismatch {
                samples: 13,
                channels: 6
            })
        );
        assert_eq!(
            AudioBuffer::new(f, t0(), vec![0.0; 12]).unwrap().frames(),
            2
        );
    }

    #[test]
    fn empty_buffer_is_valid() {
        let f = fmt(ChannelLayout::Stereo, SampleFormat::F32Planar);
        let b = AudioBuffer::new(f, t0(), Vec::new()).unwrap();
        assert_eq!(b.frames(), 0);
        assert_eq!(b.sample(0, 0), None);
        assert_eq!(b.channel(1), Some(&[][..]));
    }

    #[test]
    fn interleaved_indexing() {
        let f = fmt(ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        let b = AudioBuffer::new(f, t0(), vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        assert_eq!(b.frames(), 3);
        assert_eq!(b.sample(0, 1), Some(2.0));
        assert_eq!(b.sample(2, 0), Some(5.0));
        assert_eq!(b.sample(3, 0), None);
        assert_eq!(b.sample(0, 2), None);
        assert_eq!(
            b.channel(0),
            None,
            "interleaved buffers have no channel slices"
        );
    }

    #[test]
    fn planar_indexing() {
        let f = fmt(ChannelLayout::Stereo, SampleFormat::F32Planar);
        let b = AudioBuffer::new(f, t0(), vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        assert_eq!(b.frames(), 3);
        assert_eq!(b.channel(0), Some(&[1.0, 2.0, 3.0][..]));
        assert_eq!(b.channel(1), Some(&[4.0, 5.0, 6.0][..]));
        assert_eq!(b.channel(2), None);
        assert_eq!(b.sample(2, 1), Some(6.0));
        assert_eq!(b.sample(0, 1), Some(4.0));
    }

    #[test]
    fn silence_has_the_requested_frames() {
        let f = fmt(ChannelLayout::Surround51, SampleFormat::F32Planar);
        let b = AudioBuffer::silence(f, t0(), 480).unwrap();
        assert_eq!(b.frames(), 480);
        assert_eq!(b.samples().len(), 480 * 6);
        assert!(b.samples().iter().all(|s| *s == 0.0));
        assert_eq!(
            AudioBuffer::silence(f, t0(), usize::MAX),
            Err(MediaError::Overflow {
                what: "sample count"
            })
        );
    }

    #[test]
    fn duration_is_in_sample_ticks() {
        let f = fmt(ChannelLayout::Mono, SampleFormat::F32Interleaved);
        let b = AudioBuffer::silence(f, t0(), 480).unwrap();
        let d = b.duration().unwrap();
        assert_eq!(d.timebase(), Timebase::per_second(48_000).unwrap());
        assert_eq!(
            d.rescale(Timebase::MILLIS, Rounding::Down).unwrap().ticks(),
            10
        );
    }

    #[test]
    fn mutation_keeps_shape() {
        let f = fmt(ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        let mut b = AudioBuffer::silence(f, t0(), 2).unwrap();
        b.samples_mut()[3] = 0.5;
        b.set_pts(MediaTime::new(5, Timebase::MILLIS));
        assert_eq!(b.sample(1, 1), Some(0.5));
        assert_eq!(b.pts().ticks(), 5);
        assert_eq!(b.format(), f);
        assert_eq!(b.into_samples(), vec![0.0, 0.0, 0.0, 0.5]);
    }
}
