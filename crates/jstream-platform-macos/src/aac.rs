//! The platform-independent half of the AudioToolbox AAC-LC encoder (ADR-0063 §7).
//!
//! - An `AudioConverter` pulls PCM through an input callback and hands back one raw AAC
//!   access unit (1024 frames, no ADTS) per output packet. [`AacFramer`] holds the input
//!   the callback gives it: interleaved `f32`, whatever the engine's buffers were, padded
//!   with silence to a whole access unit at the end, as the `AudioEncoder` trait asks. It
//!   also gives each packet its `pts`: the first buffer's `pts` plus 1024 frames per
//!   packet before it, as the Linux and Windows encoders count them. The encoder's
//!   priming is not subtracted, so the decoded output lags the input by it (2112 frames
//!   on Apple's encoder; 1024 on `jstream-aac`).
//! - The muxers write the `AudioSpecificConfig` from the stream's format
//!   (`jstream-mux`'s `aac_lc`). [`aac_lc_config`] is those bytes, and
//!   [`asc_from_magic_cookie`] reads the config the converter signals out of its magic
//!   cookie, so the encoder can check that the two agree.
//! - [`nearest_bit_rate`] picks the rate to ask for among the ones the encoder offers.

use jstream_platform::PlatformError;
use jstream_platform::media::{AudioBuffer, AudioFormat, MediaTime, Rounding, Timebase};

/// Frames per AAC-LC access unit.
pub const AAC_FRAME: usize = 1024;

/// The sampling frequency indices of ISO/IEC 14496-3 Table 1.18, from 96 kHz down.
const FREQUENCIES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The two-byte `AudioSpecificConfig` of an AAC-LC stream (audio object type 2, a
/// 1024-frame `GASpecificConfig`), the bytes `jstream-mux`'s `aac_lc` writes. `None` for
/// a rate without a frequency index or a channel count outside 1 to 7.
pub const fn aac_lc_config(sample_rate: u32, channels: u8) -> Option<[u8; 2]> {
    let mut index = 0;
    while index < FREQUENCIES.len() {
        if FREQUENCIES[index] == sample_rate {
            break;
        }
        index += 1;
    }
    if index == FREQUENCIES.len() || channels == 0 || channels > 7 {
        return None;
    }
    // In range: index < 13, channels < 8.
    let index = index as u8;
    Some([
        (2 << 3) | (index >> 1),
        ((index & 1) << 7) | (channels << 3),
    ])
}

/// MPEG-4 Systems descriptor tags (ISO/IEC 14496-1 §7.2.2.1).
const ES_DESCRIPTOR: u8 = 0x03;
const DECODER_CONFIG_DESCRIPTOR: u8 = 0x04;
const DECODER_SPECIFIC_INFO: u8 = 0x05;

/// Reads one descriptor's tag and its body, returning them and what follows.
fn descriptor(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, mut rest) = data.split_first()?;
    let mut len = 0usize;
    // The size is up to four bytes of seven bits, each but the last with its top bit set.
    for _ in 0..4 {
        let (&b, tail) = rest.split_first()?;
        rest = tail;
        len = (len << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            let (body, after) = rest.split_at_checked(len)?;
            return Some((tag, body, after));
        }
    }
    None
}

/// The `AudioSpecificConfig` in an AAC encoder's magic cookie
/// (`kAudioConverterCompressionMagicCookie`): an `ES_Descriptor` whose
/// `DecoderConfigDescriptor` carries it as its `DecoderSpecificInfo`, the layout of an MP4
/// `esds` box's payload. `None` when the cookie has another shape or is cut short.
pub fn asc_from_magic_cookie(cookie: &[u8]) -> Option<&[u8]> {
    let (tag, es, _) = descriptor(cookie)?;
    if tag != ES_DESCRIPTOR {
        return None;
    }
    // ES_ID (2 bytes), then the flags byte.
    let (&flags, mut rest) = es.get(2..)?.split_first()?;
    if flags & 0x80 != 0 {
        // streamDependenceFlag: dependsOn_ES_ID.
        rest = rest.get(2..)?;
    }
    if flags & 0x40 != 0 {
        // URL_Flag: a length byte and the URL.
        let (&len, tail) = rest.split_first()?;
        rest = tail.get(usize::from(len)..)?;
    }
    if flags & 0x20 != 0 {
        // OCRstreamFlag: OCR_ES_Id.
        rest = rest.get(2..)?;
    }
    let (tag, config, _) = descriptor(rest)?;
    if tag != DECODER_CONFIG_DESCRIPTOR {
        return None;
    }
    // objectTypeIndication, streamType and buffer size, then the maximum and average
    // bit rates: 13 bytes before the DecoderSpecificInfo.
    let (tag, info, _) = descriptor(config.get(13..)?)?;
    (tag == DECODER_SPECIFIC_INFO && !info.is_empty()).then_some(info)
}

/// The bit rate to ask the encoder for: `target` when one of the `(minimum, maximum)`
/// ranges it offers (`kAudioConverterApplicableEncodeBitRates`) holds it, otherwise the
/// nearest end of one, the lower on a tie. `None` when it offers none.
pub fn nearest_bit_rate(target: u32, ranges: &[(f64, f64)]) -> Option<u32> {
    let t = f64::from(target);
    if ranges.iter().any(|&(lo, hi)| lo <= t && t <= hi) {
        return Some(target);
    }
    let best = ranges
        .iter()
        .flat_map(|&(lo, hi)| [lo, hi])
        .filter(|r| r.is_finite() && *r > 0.0)
        .min_by(|a, b| {
            let (da, db) = ((a - t).abs(), (b - t).abs());
            da.total_cmp(&db).then(a.total_cmp(b))
        })?;
    // Positive and finite, and AAC rates are far below u32::MAX.
    Some(best.round().min(f64::from(u32::MAX)) as u32)
}

/// The input an AAC `AudioConverter` pulls, and the timing of what it emits.
///
/// Buffers go in with [`push`](Self::push), as interleaved `f32`. The converter's input
/// callback takes them out with [`supply`](Self::supply), which copies them into a staging
/// buffer that stays put until the next call, as the callback's contract asks.
/// [`finish`](Self::finish) pads the input to a whole access unit and marks its end.
#[derive(Debug)]
pub struct AacFramer {
    format: AudioFormat,
    /// Interleaved samples not yet supplied start at `read`.
    queue: Vec<f32>,
    read: usize,
    staging: Vec<f32>,
    /// The first buffer's `pts`, in the input's sample time base.
    start: Option<MediaTime>,
    /// Frames pushed so far, padding included.
    frames_in: u64,
    /// Packets given a `pts` so far.
    emitted: u64,
    finished: bool,
}

impl AacFramer {
    /// An empty framer for buffers in `format`.
    pub fn new(format: AudioFormat) -> Self {
        Self {
            format,
            queue: Vec::new(),
            read: 0,
            staging: Vec::new(),
            start: None,
            frames_in: 0,
            emitted: 0,
            finished: false,
        }
    }

    /// The input format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Queues `buffer`, interleaving a planar one. The first non-empty buffer's `pts`
    /// starts the packet times.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a buffer in another format,
    /// [`PlatformError::InvalidState`] after [`finish`](Self::finish), and an error when
    /// the `pts` can't be put on the sample time base.
    pub fn push(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "AAC encode",
                state: "the encoder is finished",
            });
        }
        if buffer.format() != self.format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "audio in {:?} (configured for {:?})",
                    buffer.format(),
                    self.format
                ),
            });
        }
        let frames = buffer.frames();
        if frames == 0 {
            return Ok(());
        }
        if self.start.is_none() {
            self.start = Some(
                buffer
                    .pts()
                    .rescale(self.format.timebase(), Rounding::Nearest)?,
            );
        }
        self.compact();
        let samples = buffer.samples();
        if self.format.sample_format().is_planar() {
            let channels = self.format.channels();
            self.queue.reserve(samples.len());
            for frame in 0..frames {
                for channel in 0..channels {
                    self.queue.push(samples[channel * frames + frame]);
                }
            }
        } else {
            self.queue.extend_from_slice(samples);
        }
        self.frames_in = self.frames_in.saturating_add(frames as u64);
        Ok(())
    }

    /// Drops the samples already supplied from the front of the queue.
    fn compact(&mut self) {
        if self.read > 0 {
            self.queue.drain(..self.read);
            self.read = 0;
        }
    }

    /// Frames queued and not yet supplied.
    pub fn available(&self) -> usize {
        (self.queue.len() - self.read) / self.format.channels()
    }

    /// Pads the input with silence to a whole access unit and marks its end. Nothing is
    /// added when nothing was pushed. Calling it again does nothing.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.start.is_none() {
            return;
        }
        let frame = AAC_FRAME as u64;
        let pad = (frame - self.frames_in % frame) % frame;
        self.compact();
        // At most 1023 frames.
        let pad_samples = pad as usize * self.format.channels();
        self.queue.resize(self.queue.len() + pad_samples, 0.0);
        self.frames_in += pad;
    }

    /// Whether [`finish`](Self::finish) was called.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Frames pushed so far, padding included.
    pub fn frames_in(&self) -> u64 {
        self.frames_in
    }

    /// Up to `max_frames` frames of queued input, interleaved, in a buffer that stays
    /// unchanged until the next call. `None` when nothing is queued: the converter must
    /// wait for more, or, after [`finish`](Self::finish), has reached the end.
    pub fn supply(&mut self, max_frames: usize) -> Option<&[f32]> {
        let frames = self.available().min(max_frames);
        if frames == 0 {
            return None;
        }
        let len = frames * self.format.channels();
        self.staging.clear();
        self.staging
            .extend_from_slice(&self.queue[self.read..self.read + len]);
        self.read += len;
        Some(&self.staging)
    }

    /// The `pts` of the next packet: the first buffer's `pts` plus 1024 frames for every
    /// packet before it.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] before any input, or when the stream outgrows the
    /// time base.
    pub fn next_pts(&mut self) -> Result<MediaTime, PlatformError> {
        let start = self.start.ok_or(PlatformError::InvalidState {
            what: "AAC packet timing",
            state: "the encoder produced output before any input",
        })?;
        let offset = self
            .emitted
            .checked_mul(AAC_FRAME as u64)
            .and_then(|o| i64::try_from(o).ok())
            .ok_or(PlatformError::InvalidState {
                what: "AAC packet timing",
                state: "the stream is too long",
            })?;
        let pts = start.checked_add(MediaTime::new(offset, start.timebase()))?;
        self.emitted += 1;
        Ok(pts)
    }

    /// The input's sample time base, which packet times are given in.
    pub fn timebase(&self) -> Timebase {
        self.format.timebase()
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;
    use jstream_platform::media::{
        AudioBuffer, AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase,
    };

    use super::{AAC_FRAME, AacFramer, aac_lc_config, asc_from_magic_cookie, nearest_bit_rate};

    fn format(layout: ChannelLayout, samples: SampleFormat) -> AudioFormat {
        AudioFormat::new(48_000, layout, samples).expect("valid format")
    }

    #[test]
    fn aac_lc_config_matches_what_the_muxers_write() {
        // jstream-mux's AudioSpecificConfig::aac_lc and jstream-aac's encoder give these.
        assert_eq!(aac_lc_config(48_000, 2), Some([0x11, 0x90]));
        assert_eq!(aac_lc_config(44_100, 2), Some([0x12, 0x10]));
        assert_eq!(aac_lc_config(44_100, 1), Some([0x12, 0x08]));
        assert_eq!(aac_lc_config(48_000, 1), Some([0x11, 0x88]));
        assert_eq!(aac_lc_config(47_000, 2), None);
        assert_eq!(aac_lc_config(48_000, 0), None);
        assert_eq!(aac_lc_config(48_000, 8), None);
    }

    /// An `esds` payload as Apple's AAC encoder writes it: ES_Descriptor, then
    /// DecoderConfigDescriptor, then DecoderSpecificInfo, with `long` four-byte sizes.
    fn cookie(asc: &[u8], flags: u8, long: bool) -> Vec<u8> {
        let size = |n: usize| -> Vec<u8> {
            let n = u8::try_from(n).expect("small");
            if long {
                vec![0x80, 0x80, 0x80, n]
            } else {
                vec![n]
            }
        };
        let mut info = vec![0x05];
        info.extend(size(asc.len()));
        info.extend_from_slice(asc);
        let mut config = vec![0x04];
        let mut body = vec![0x40, 0x15, 0, 0x18, 0, 0, 2, 0x71, 0, 0, 2, 0x71, 0];
        body.extend(&info);
        config.extend(size(body.len()));
        config.extend(body);
        let mut es = vec![0, 0, flags];
        if flags & 0x80 != 0 {
            es.extend([0, 7]);
        }
        if flags & 0x40 != 0 {
            es.extend([3, b'a', b'b', b'c']);
        }
        if flags & 0x20 != 0 {
            es.extend([0, 9]);
        }
        es.extend(config);
        es.extend([0x06, 0x01, 0x02]); // SLConfigDescriptor
        let mut out = vec![0x03];
        out.extend(size(es.len()));
        out.extend(es);
        out
    }

    #[test]
    fn the_config_is_read_from_the_magic_cookie() {
        for long in [false, true] {
            for flags in [0x00, 0x80, 0x40, 0x20, 0xE0] {
                let c = cookie(&[0x11, 0x90], flags, long);
                assert_eq!(
                    asc_from_magic_cookie(&c),
                    Some(&[0x11, 0x90][..]),
                    "{flags:#x} {long}"
                );
            }
        }
        let good = cookie(&[0x12, 0x10, 0x56, 0xE5, 0x00], 0, true);
        assert_eq!(
            asc_from_magic_cookie(&good),
            Some(&[0x12, 0x10, 0x56, 0xE5, 0x00][..])
        );
        for cut in 0..good.len() {
            assert_eq!(asc_from_magic_cookie(&good[..cut]), None, "{cut}");
        }
        // A bare AudioSpecificConfig, or another descriptor, isn't a cookie.
        assert_eq!(asc_from_magic_cookie(&[0x11, 0x90]), None);
        let mut other = cookie(&[0x11, 0x90], 0, false);
        other[0] = 0x04;
        assert_eq!(asc_from_magic_cookie(&other), None);
        // An empty DecoderSpecificInfo carries no config.
        assert_eq!(asc_from_magic_cookie(&cookie(&[], 0, false)), None);
    }

    #[test]
    fn the_nearest_offered_bit_rate_is_asked_for() {
        let discrete: Vec<(f64, f64)> = [64_000.0, 96_000.0, 128_000.0, 160_000.0, 192_000.0]
            .iter()
            .map(|&r| (r, r))
            .collect();
        assert_eq!(nearest_bit_rate(128_000, &discrete), Some(128_000));
        assert_eq!(nearest_bit_rate(140_000, &discrete), Some(128_000));
        assert_eq!(nearest_bit_rate(150_000, &discrete), Some(160_000));
        // A tie goes to the lower rate.
        assert_eq!(nearest_bit_rate(144_000, &discrete), Some(128_000));
        assert_eq!(nearest_bit_rate(320_000, &discrete), Some(192_000));
        assert_eq!(nearest_bit_rate(32_000, &discrete), Some(64_000));
        assert_eq!(
            nearest_bit_rate(100_000, &[(32_000.0, 256_000.0)]),
            Some(100_000)
        );
        assert_eq!(nearest_bit_rate(128_000, &[]), None);
        assert_eq!(nearest_bit_rate(128_000, &[(f64::NAN, f64::NAN)]), None);
    }

    #[test]
    fn packets_are_timed_from_the_first_buffer_and_padded_to_a_whole_unit() {
        let f = format(ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        let mut framer = AacFramer::new(f);
        assert!(matches!(
            framer.next_pts(),
            Err(PlatformError::InvalidState { .. })
        ));
        // 960-frame buffers from a 20 ms mixer tick, on a nanosecond clock from 1 s.
        for tick in 0..3i64 {
            let pts = MediaTime::new(1_000_000_000 + tick * 20_000_000, Timebase::NANOS);
            let buffer = AudioBuffer::silence(f, pts, 960).expect("buffer");
            framer.push(&buffer).expect("push");
        }
        assert_eq!(framer.available(), 2880);
        assert_eq!(framer.frames_in(), 2880);
        framer.finish();
        assert!(framer.is_finished());
        assert_eq!(framer.frames_in(), 3 * AAC_FRAME as u64);
        assert_eq!(framer.available(), 3 * AAC_FRAME);
        for n in 0..4 {
            let pts = framer.next_pts().expect("pts");
            assert_eq!(pts.timebase(), f.timebase());
            assert_eq!(pts.ticks(), 48_000 + n * 1024);
        }
        let late = AudioBuffer::silence(f, MediaTime::zero(Timebase::NANOS), 10).expect("buf");
        assert!(matches!(
            framer.push(&late),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    #[test]
    fn supply_hands_out_interleaved_input_in_order() {
        let f = format(ChannelLayout::Stereo, SampleFormat::F32Planar);
        let mut framer = AacFramer::new(f);
        let zero = MediaTime::zero(Timebase::NANOS);
        // Planar: L0 L1 L2 then R0 R1 R2.
        let planar =
            AudioBuffer::new(f, zero, vec![1.0, 2.0, 3.0, -1.0, -2.0, -3.0]).expect("buffer");
        framer.push(&planar).expect("push");
        assert_eq!(framer.supply(2), Some(&[1.0, -1.0, 2.0, -2.0][..]));
        let more = AudioBuffer::new(f, zero, vec![4.0, -4.0]).expect("buffer");
        framer.push(&more).expect("push");
        assert_eq!(framer.available(), 2);
        assert_eq!(framer.supply(10), Some(&[3.0, -3.0, 4.0, -4.0][..]));
        assert_eq!(framer.supply(10), None);
        framer.finish();
        // Four frames in, so 1020 frames of silence complete the unit.
        assert_eq!(framer.available(), AAC_FRAME - 4);
        let tail = framer.supply(AAC_FRAME).expect("padding");
        assert_eq!(tail.len(), (AAC_FRAME - 4) * 2);
        assert!(tail.iter().all(|&s| s == 0.0));
        assert_eq!(framer.supply(AAC_FRAME), None);
    }

    #[test]
    fn empty_streams_and_mismatched_buffers() {
        let f = format(ChannelLayout::Mono, SampleFormat::F32Interleaved);
        let mut framer = AacFramer::new(f);
        let zero = MediaTime::zero(Timebase::NANOS);
        framer
            .push(&AudioBuffer::silence(f, zero, 0).expect("buffer"))
            .expect("an empty buffer is fine");
        let stereo = format(ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        assert!(matches!(
            framer.push(&AudioBuffer::silence(stereo, zero, 4).expect("buffer")),
            Err(PlatformError::Unsupported { .. })
        ));
        // Nothing went in, so finishing adds no padding.
        framer.finish();
        framer.finish();
        assert_eq!(framer.available(), 0);
        assert_eq!(framer.supply(AAC_FRAME), None);
    }
}
