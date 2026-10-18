//! The AudioToolbox AAC-LC encoder (ADR-0063 §7): an `AudioConverter` from interleaved
//! `f32` PCM to AAC-LC, whose packets are raw access units on the media clock.
//!
//! The converter is opened with [`aac_settings`]: AAC-LC at the input's rate and channel
//! count, constant bit rate (`kAudioCodecBitRateControlMode_Constant`), at the offered
//! rate nearest the configured one ([`nearest_bit_rate`]). Its magic cookie must carry
//! the `AudioSpecificConfig` the muxers write from the format ([`aac_lc_config`]), or
//! opening fails.
//!
//! The converter pulls its input. [`AtAacEncoder::encode`] queues each buffer in an
//! [`AacFramer`] and, while a whole access unit is queued, asks the converter for one
//! packet at a time with `AudioConverterFillComplexBuffer`. The converter calls
//! [`input_proc`] synchronously, on the calling thread, which hands it what is queued.
//! When the queue runs dry it returns [`NEED_INPUT`], which makes the converter stop and
//! keep what it already took; at the end of the stream it returns no data and no error,
//! which makes the converter flush. Packets are counted from the first buffer's `pts`, as
//! on Linux and Windows, so the encoder's priming (2112 frames) is not subtracted.

use std::ffi::c_void;
use std::mem::{size_of, size_of_val};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};

use jstream_platform::media::{AudioBuffer, Codec, Packet};
use jstream_platform::{AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, PlatformError};
use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterGetProperty,
    AudioConverterGetPropertyInfo, AudioConverterNew, AudioConverterRef, AudioConverterSetProperty,
    OpaqueAudioConverter, kAudioCodecBitRateControlMode_Constant,
    kAudioCodecPropertyBitRateControlMode, kAudioConverterApplicableEncodeBitRates,
    kAudioConverterCompressionMagicCookie, kAudioConverterEncodeBitRate,
    kAudioConverterErr_PropertyNotSupported, kAudioConverterPropertyMaximumOutputPacketSize,
};
use objc2_core_audio_types::{
    AudioBuffer as CaBuffer, AudioBufferList, AudioStreamBasicDescription,
    AudioStreamPacketDescription, AudioValueRange, kAudioFormatFlagsNativeFloatPacked,
    kAudioFormatLinearPCM,
};

use crate::aac::{AAC_FRAME, AacFramer, aac_lc_config, asc_from_magic_cookie, nearest_bit_rate};
use crate::codec::{AacSettings, aac_settings};
use crate::fourcc::os_status;

/// The name the encoder lists and opens under.
pub(crate) const AAC_ENCODER_NAME: &str = "AudioToolbox AAC-LC";

/// `noErr`.
const NO_ERR: i32 = 0;

/// What [`input_proc`] returns when nothing is queued yet: the converter stops, keeps
/// what it took, and asks again on the next `AudioConverterFillComplexBuffer`.
const NEED_INPUT: i32 = i32::from_be_bytes(*b"jsIn");

/// What [`input_proc`] returns when it panicked.
const INPUT_PANICKED: i32 = i32::from_be_bytes(*b"jsPn");

/// The largest AAC access unit per channel: 6144 bits (ISO/IEC 14496-3 §4.5.3.2).
const MAX_UNIT_PER_CHANNEL: u32 = 768;

/// At most this many packets past the queued input are drawn at the end of the stream:
/// the priming tail is three, and a converter that never says it is done stops here.
const FLUSH_PACKETS: usize = 8;

fn check(status: i32, call: &'static str, what: &str) -> Result<(), PlatformError> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(os_status(call, status, what))
    }
}

/// The codec list entry for the encoder.
pub(crate) fn aac_encoder_info() -> CodecInfo {
    CodecInfo {
        name: AAC_ENCODER_NAME.into(),
        codec: Codec::Aac,
        role: CodecRole::AudioEncoder,
        hardware: false,
    }
}

/// An `AudioConverter`, disposed of when dropped.
struct Converter(NonNull<OpaqueAudioConverter>);

// SAFETY: an AudioConverter has no thread affinity: it needs no run loop and calls its
// input proc only on the thread inside `AudioConverterFillComplexBuffer`. `Converter` is
// not `Sync`, so only the thread that owns the encoder calls it.
unsafe impl Send for Converter {}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: the converter is live and no call into it is in progress (`&mut self`).
        let _ = unsafe { AudioConverterDispose(self.raw()) };
    }
}

impl Converter {
    fn raw(&self) -> AudioConverterRef {
        self.0.as_ptr()
    }

    /// Sets a `UInt32` property.
    fn set_u32(&self, id: u32, value: u32) -> i32 {
        // SAFETY: the converter is live, and `value` is the four bytes the size says, for
        // a property AudioConverter.h or AudioCodec.h documents as a UInt32.
        unsafe {
            AudioConverterSetProperty(
                self.raw(),
                id,
                size_of::<u32>() as u32,
                NonNull::from(&value).cast(),
            )
        }
    }

    /// Reads a `UInt32` property.
    fn get_u32(&self, id: u32) -> Option<u32> {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: the converter is live, and `size` and `value` are valid out pointers,
        // `value` as large as `size` says, for a property documented as a UInt32.
        let status = unsafe {
            AudioConverterGetProperty(
                self.raw(),
                id,
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        (status == NO_ERR && size as usize == size_of::<u32>()).then_some(value)
    }

    /// The size of a property, in bytes, when the converter has it.
    fn property_size(&self, id: u32) -> Option<usize> {
        let mut size = 0u32;
        // SAFETY: the converter is live, `size` is a valid out pointer, and the writable
        // flag may be null.
        let status =
            unsafe { AudioConverterGetPropertyInfo(self.raw(), id, &mut size, ptr::null_mut()) };
        (status == NO_ERR).then_some(size as usize)
    }

    /// Reads a property of `T`s into a vector, `None` when the converter lacks it.
    fn get_array<T: Copy>(&self, id: u32, empty: T) -> Option<Vec<T>> {
        let bytes = self.property_size(id)?;
        let count = bytes / size_of::<T>();
        if count == 0 {
            return Some(Vec::new());
        }
        let mut values = vec![empty; count];
        let mut size = u32::try_from(count * size_of::<T>()).ok()?;
        // SAFETY: the converter is live; `values` is `size` writable bytes of the element
        // type the property is documented as (bytes for the cookie, AudioValueRange for
        // the bit rates), and `size` is a valid in/out pointer.
        let status = unsafe {
            AudioConverterGetProperty(
                self.raw(),
                id,
                NonNull::from(&mut size),
                NonNull::from(values.as_mut_slice()).cast(),
            )
        };
        if status != NO_ERR {
            return None;
        }
        values.truncate(size as usize / size_of::<T>());
        Some(values)
    }

    /// The `AudioSpecificConfig` in the converter's magic cookie, when it has one this
    /// crate can read.
    fn signalled_config(&self) -> Option<Vec<u8>> {
        let cookie = self.get_array(kAudioConverterCompressionMagicCookie, 0u8)?;
        asc_from_magic_cookie(&cookie).map(<[u8]>::to_vec)
    }
}

/// The interleaved `f32` PCM the converter takes.
fn pcm_format(settings: &AacSettings) -> AudioStreamBasicDescription {
    let bytes_per_frame = 4 * settings.channels;
    AudioStreamBasicDescription {
        mSampleRate: f64::from(settings.sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: settings.channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

/// The AAC-LC it emits: 1024 frames a packet, of variable size.
fn aac_format(settings: &AacSettings) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(settings.sample_rate),
        mFormatID: settings.format_id,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: AAC_FRAME as u32,
        mBytesPerFrame: 0,
        mChannelsPerFrame: settings.channels,
        mBitsPerChannel: 0,
        mReserved: 0,
    }
}

/// What [`input_proc`] reaches through its user data, for one
/// `AudioConverterFillComplexBuffer` call.
struct Input<'a> {
    framer: &'a mut AacFramer,
}

/// What [`input_proc`] does with a request for `requested` frames: points `list` at the
/// queued input and returns the frames given and the status.
fn supply(input: &mut Input<'_>, requested: u32, list: &mut AudioBufferList) -> (u32, i32) {
    let channels = input.framer.format().channels();
    let finished = input.framer.is_finished();
    list.mNumberBuffers = 1;
    list.mBuffers[0] = CaBuffer {
        mNumberChannels: u32::try_from(channels).unwrap_or(0),
        mDataByteSize: 0,
        mData: ptr::null_mut(),
    };
    if requested == 0 {
        return (0, NEED_INPUT);
    }
    let Some(samples) = input.framer.supply(requested as usize) else {
        // No data and no error is the end of the stream; an error with no data is a
        // pause until the next call.
        return (0, if finished { NO_ERR } else { NEED_INPUT });
    };
    let frames = samples.len() / channels;
    let (Ok(frames), Ok(bytes)) = (u32::try_from(frames), u32::try_from(size_of_val(samples)))
    else {
        return (0, INPUT_PANICKED);
    };
    list.mBuffers[0].mDataByteSize = bytes;
    // The converter only reads the input. The staging buffer stays unchanged until the
    // next `supply`, as the input proc's contract asks.
    list.mBuffers[0].mData = samples.as_ptr().cast_mut().cast();
    (frames, NO_ERR)
}

/// The converter's input proc, called on this thread from inside
/// `AudioConverterFillComplexBuffer`. No panic leaves it (R-UNSAFE-005).
unsafe extern "C-unwind" fn input_proc(
    _converter: AudioConverterRef,
    packets: NonNull<u32>,
    data: NonNull<AudioBufferList>,
    _descriptions: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: AudioToolbox passes a valid packet count for this call. It is set to zero
    // first so that an error, a panic included, gives no data, as the contract asks.
    let requested = unsafe {
        let requested = packets.read();
        packets.write(0);
        requested
    };
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `user` is the `Input` that `AtAacEncoder::pull` passed, live for the
        // whole call and not touched by it until the call returns. `data` is the buffer
        // list AudioToolbox owns for this call; interleaved PCM uses its one buffer.
        let (input, list) = unsafe { (user.cast::<Input<'_>>().as_mut(), data.as_ptr()) };
        let Some(input) = input else {
            return INPUT_PANICKED;
        };
        // SAFETY: as above; nothing else refers to the list during the call.
        let list = unsafe { &mut *list };
        let (frames, status) = supply(input, requested, list);
        // SAFETY: as above.
        unsafe { packets.write(frames) };
        status
    }))
    .unwrap_or(INPUT_PANICKED)
}

/// One AudioToolbox AAC-LC encoder session.
pub(crate) struct AtAacEncoder {
    converter: Converter,
    framer: AacFramer,
    info: CodecInfo,
    channels: u32,
    /// One packet's output buffer, the largest packet the converter can emit.
    unit: Vec<u8>,
    pending: Vec<Packet>,
}

impl AtAacEncoder {
    /// A converter for `config`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a configuration [`aac_settings`] refuses, or
    /// when the converter would signal another `AudioSpecificConfig` than the muxers
    /// write, and [`PlatformError::Os`] when AudioToolbox refuses.
    pub(crate) fn open(config: &AudioEncoderConfig) -> Result<Self, PlatformError> {
        let settings = aac_settings(config)?;
        let input = pcm_format(&settings);
        let output = aac_format(&settings);
        let mut raw: AudioConverterRef = ptr::null_mut();
        // SAFETY: both descriptions are valid for the call, and `raw` is a valid out
        // pointer for the new converter.
        let status = unsafe {
            AudioConverterNew(
                NonNull::from(&input),
                NonNull::from(&output),
                NonNull::from(&mut raw),
            )
        };
        check(status, "AudioConverterNew", "opening the AAC encoder")?;
        let converter = Converter(NonNull::new(raw).ok_or_else(|| PlatformError::Os {
            call: "AudioConverterNew",
            code: None,
            message: "returned no converter".into(),
        })?);
        // The bit-rate mode decides which rates are offered, so it goes first.
        let status = converter.set_u32(
            kAudioCodecPropertyBitRateControlMode,
            kAudioCodecBitRateControlMode_Constant,
        );
        if status != kAudioConverterErr_PropertyNotSupported {
            check(status, "AudioConverterSetProperty", "constant bit rate")?;
        }
        let empty = AudioValueRange {
            mMinimum: 0.0,
            mMaximum: 0.0,
        };
        let offered: Vec<(f64, f64)> = converter
            .get_array(kAudioConverterApplicableEncodeBitRates, empty)
            .unwrap_or_default()
            .iter()
            .map(|r| (r.mMinimum, r.mMaximum))
            .collect();
        let bit_rate = nearest_bit_rate(settings.bit_rate, &offered).unwrap_or(settings.bit_rate);
        check(
            converter.set_u32(kAudioConverterEncodeBitRate, bit_rate),
            "AudioConverterSetProperty",
            "the AAC bit rate",
        )?;
        let channels = settings.channels;
        let expected = u8::try_from(channels)
            .ok()
            .and_then(|c| aac_lc_config(settings.sample_rate, c));
        if let (Some(signalled), Some(expected)) = (converter.signalled_config(), expected)
            && signalled.get(..2) != Some(&expected[..])
        {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "AudioToolbox's AAC encoder signals AudioSpecificConfig {signalled:02x?}, \
                     not the AAC-LC {expected:02x?} the muxers write"
                ),
            });
        }
        let max_packet = converter
            .get_u32(kAudioConverterPropertyMaximumOutputPacketSize)
            .filter(|&n| n > 0)
            .unwrap_or(MAX_UNIT_PER_CHANNEL * channels);
        Ok(Self {
            converter,
            framer: AacFramer::new(config.input_format),
            info: aac_encoder_info(),
            channels,
            unit: vec![0; max_packet as usize],
            pending: Vec::new(),
        })
    }

    /// Asks the converter for one packet. `Ok(true)` when it produced one and can be
    /// asked again; `Ok(false)` when it needs more input or has reached the end.
    fn pull(&mut self) -> Result<bool, PlatformError> {
        let capacity = u32::try_from(self.unit.len()).unwrap_or(u32::MAX);
        let mut packets: u32 = 1;
        let mut description = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        };
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [CaBuffer {
                mNumberChannels: self.channels,
                mDataByteSize: capacity,
                mData: self.unit.as_mut_ptr().cast(),
            }],
        };
        let mut input = Input {
            framer: &mut self.framer,
        };
        // SAFETY: the converter is live. `input_proc` has the
        // AudioConverterComplexInputDataProc signature and reads its user data only as
        // `&mut Input`, which outlives this synchronous call. `packets`, `list` and
        // `description` are valid for the call; the list's one buffer is `capacity`
        // writable bytes of `unit`, room for one packet of the converter's maximum size,
        // and `description` holds the one packet description that asks for.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.raw(),
                Some(input_proc),
                ptr::from_mut(&mut input).cast(),
                NonNull::from(&mut packets),
                NonNull::from(&mut list),
                &mut description,
            )
        };
        match status {
            NO_ERR | NEED_INPUT => {}
            INPUT_PANICKED => {
                return Err(PlatformError::Os {
                    call: "AudioConverterComplexInputDataProc",
                    code: None,
                    message: "supplying AAC input panicked".into(),
                });
            }
            other => {
                return Err(os_status(
                    "AudioConverterFillComplexBuffer",
                    other,
                    "encoding AAC",
                ));
            }
        }
        if packets == 0 {
            return Ok(false);
        }
        let len = (list.mBuffers[0].mDataByteSize as usize).min(self.unit.len());
        if len == 0 {
            return Err(PlatformError::Os {
                call: "AudioConverterFillComplexBuffer",
                code: None,
                message: "an empty AAC packet".into(),
            });
        }
        let pts = self.framer.next_pts()?;
        self.pending
            .push(Packet::new(Codec::Aac, true, pts, pts, &self.unit[..len])?);
        Ok(status == NO_ERR)
    }

    /// Draws packets while a whole access unit is queued, or, once finished, until the
    /// converter has flushed.
    fn drain(&mut self) -> Result<(), PlatformError> {
        let finishing = self.framer.is_finished();
        if self.framer.frames_in() == 0 {
            // Nothing went in, so nothing comes out, and no packet has a time to take.
            return Ok(());
        }
        let mut budget = self.framer.available() / AAC_FRAME + FLUSH_PACKETS;
        while budget > 0 && (finishing || self.framer.available() >= AAC_FRAME) {
            budget -= 1;
            if !self.pull()? {
                break;
            }
        }
        Ok(())
    }

    /// The `AudioSpecificConfig` the converter signals in its magic cookie.
    #[cfg(test)]
    fn audio_specific_config(&self) -> Option<Vec<u8>> {
        self.converter.signalled_config()
    }
}

impl AudioEncoder for AtAacEncoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
        self.framer.push(buffer)?;
        self.drain()
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.append(&mut self.pending);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        if !self.framer.is_finished() {
            self.framer.finish();
            self.drain()?;
        }
        self.poll(out)
    }
}

/// These run on any Mac, the CI runner included: Apple's AAC encoder is software and
/// needs no permission.
#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        AudioBuffer, AudioFormat, ChannelLayout, Codec, MediaTime, Rounding, SampleFormat, Timebase,
    };
    use jstream_platform::{AudioEncoder, AudioEncoderConfig, Codecs, PlatformError};

    use super::{AAC_ENCODER_NAME, AtAacEncoder};
    use crate::{VideoToolboxCodecs, aac_lc_config};

    fn config(rate: u32, layout: ChannelLayout, format: SampleFormat) -> AudioEncoderConfig {
        AudioEncoderConfig {
            codec: Codec::Aac,
            input_format: AudioFormat::new(rate, layout, format).expect("valid format"),
            kbps: 160,
        }
    }

    /// `ticks` 20 ms buffers of a tone, from 1 s on a nanosecond clock.
    fn buffers(format: AudioFormat, ticks: u32) -> Vec<AudioBuffer> {
        let frames = format.sample_rate() as usize / 50;
        let channels = format.channels();
        (0..ticks)
            .map(|tick| {
                let pts = MediaTime::new(
                    1_000_000_000 + i64::from(tick) * 20_000_000,
                    Timebase::NANOS,
                );
                let samples = (0..frames * channels)
                    .map(|i| ((i + tick as usize * frames) as f32 * 0.05).sin() * 0.3)
                    .collect();
                AudioBuffer::new(format, pts, samples).expect("buffer")
            })
            .collect()
    }

    fn encode(
        encoder: &mut dyn AudioEncoder,
        buffers: &[AudioBuffer],
    ) -> Vec<jstream_platform::media::Packet> {
        let mut out = Vec::new();
        for b in buffers {
            encoder.encode(b).expect("encode");
            encoder.poll(&mut out).expect("poll");
        }
        encoder.finish(&mut out).expect("finish");
        out
    }

    #[test]
    fn the_aac_encoder_is_listed() {
        let list = VideoToolboxCodecs::new().list().expect("list");
        assert!(
            list.iter()
                .any(|c| c.codec == Codec::Aac && c.name == AAC_ENCODER_NAME),
            "{list:?}"
        );
    }

    #[test]
    fn stereo_encodes_to_raw_access_units_timed_from_the_first_buffer() {
        let config = config(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        let mut encoder = VideoToolboxCodecs::new()
            .audio_encoder(&config)
            .expect("an AAC converter");
        assert_eq!(encoder.info().codec, Codec::Aac);
        let input = buffers(config.input_format, 10);
        let packets = encode(encoder.as_mut(), &input);
        // 9600 frames, padded to ten units, and the encoder's priming tail after them.
        assert!(
            (10..=14).contains(&packets.len()),
            "{} packets",
            packets.len()
        );
        let tb = config.input_format.timebase();
        let first = input[0]
            .pts()
            .rescale(tb, Rounding::Nearest)
            .expect("rescale");
        for (n, p) in packets.iter().enumerate() {
            assert_eq!(p.codec(), Codec::Aac);
            assert!(p.is_keyframe());
            assert_eq!(p.pts().timebase(), tb);
            assert_eq!(
                p.pts().ticks(),
                first.ticks() + n as i64 * 1024,
                "packet {n}"
            );
            assert_eq!(p.dts(), p.pts());
            let data = p.data();
            assert!(!data.is_empty() && data.len() <= 768 * 2, "{}", data.len());
            // Raw, not ADTS (a 0xFFF syncword).
            assert!(!(data.len() > 1 && data[0] == 0xFF && data[1] & 0xF0 == 0xF0));
        }
        let late = AudioBuffer::silence(config.input_format, first, 10).expect("buffer");
        assert!(matches!(
            encoder.encode(&late),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    #[test]
    fn the_signalled_config_is_the_one_the_muxers_write() {
        for (rate, layout, format, channels) in [
            (
                48_000,
                ChannelLayout::Stereo,
                SampleFormat::F32Interleaved,
                2,
            ),
            (44_100, ChannelLayout::Mono, SampleFormat::F32Planar, 1),
        ] {
            let config = config(rate, layout, format);
            let encoder = AtAacEncoder::open(&config).expect("an AAC converter");
            let signalled = encoder.audio_specific_config().expect("a magic cookie");
            let expected = aac_lc_config(rate, channels).expect("a config");
            assert_eq!(&signalled[..2], &expected[..], "{config:?}");
        }
    }

    #[test]
    fn planar_mono_encodes() {
        let config = config(44_100, ChannelLayout::Mono, SampleFormat::F32Planar);
        let mut encoder = AtAacEncoder::open(&config).expect("an AAC converter");
        let packets = encode(&mut encoder, &buffers(config.input_format, 5));
        // 4410 frames are five units.
        assert!((5..=9).contains(&packets.len()), "{}", packets.len());
        assert!(packets.iter().all(|p| p.data().len() <= 768));
    }

    #[test]
    fn mismatched_buffers_and_empty_streams() {
        let config = config(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        let mut encoder = AtAacEncoder::open(&config).expect("an AAC converter");
        let other = AudioFormat::new(44_100, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let wrong =
            AudioBuffer::silence(other, MediaTime::zero(Timebase::NANOS), 100).expect("buffer");
        assert!(matches!(
            encoder.encode(&wrong),
            Err(PlatformError::Unsupported { .. })
        ));
        let mut out = Vec::new();
        encoder.finish(&mut out).expect("finish");
        assert!(out.is_empty(), "{} packets from no input", out.len());
    }

    #[test]
    fn configurations_audiotoolbox_cant_take_are_refused_before_opening() {
        let codecs = VideoToolboxCodecs::new();
        let mut not_aac = config(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved);
        not_aac.codec = Codec::H264;
        for c in [
            config(96_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved),
            config(
                48_000,
                ChannelLayout::Surround51,
                SampleFormat::F32Interleaved,
            ),
            not_aac,
        ] {
            assert!(
                matches!(
                    codecs.audio_encoder(&c),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{c:?}"
            );
        }
    }
}
