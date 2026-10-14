//! AAC-LC encoding through the Media Foundation AAC encoder MFT.
//!
//! The MFT takes 16-bit PCM and emits one raw AAC access unit (1024 frames) per output
//! sample, without ADTS: the framing `jstream-media` wants. Packet times are counted here
//! from the first buffer's `pts`, as the `AudioEncoder` trait says, rather than read back
//! from the MFT.

use jstream_platform::media::{
    AudioFormat, ChannelLayout, Codec, MediaTime, Rounding, SampleFormat,
};
use jstream_platform::{AudioEncoderConfig, PlatformError};

/// Frames per AAC-LC access unit.
pub(crate) const AAC_FRAME: u64 = 1024;
/// Sample rates the Media Foundation AAC encoder takes.
const RATES: [u32; 2] = [44_100, 48_000];
/// Bit rates the Media Foundation AAC encoder offers for mono and stereo, in kilobits per
/// second (`MF_MT_AUDIO_AVG_BYTES_PER_SECOND` 12000, 16000, 20000 or 24000).
const KBPS_STEREO: [u32; 4] = [96, 128, 160, 192];
/// Bit rates it offers for 5.1: six times the stereo ones (72000 to 144000 bytes/s).
const KBPS_51: [u32; 4] = [576, 768, 960, 1152];

/// Checks a configuration against what the Media Foundation AAC encoder supports, and
/// returns the output's `MF_MT_AUDIO_AVG_BYTES_PER_SECOND`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, a planar input, a sample rate other
/// than 44.1 or 48 kHz, a layout other than mono, stereo or 5.1, or a bit rate the encoder
/// doesn't offer for the layout: 96, 128, 160 or 192 kbps for mono and stereo, and 576,
/// 768, 960 or 1152 kbps for 5.1.
pub(crate) fn check(config: &AudioEncoderConfig) -> Result<u32, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    let f = config.input_format;
    if config.codec != Codec::Aac {
        return unsupported(format!("{:?} from the AAC encoder", config.codec));
    }
    if f.sample_format() != SampleFormat::F32Interleaved {
        return unsupported(format!("AAC input in {:?}", f.sample_format()));
    }
    if !RATES.contains(&f.sample_rate()) {
        return unsupported(format!(
            "AAC at {} Hz (the Media Foundation encoder takes 44100 or 48000)",
            f.sample_rate()
        ));
    }
    let offered = match f.layout() {
        ChannelLayout::Mono | ChannelLayout::Stereo => KBPS_STEREO,
        ChannelLayout::Surround51 => KBPS_51,
        other => return unsupported(format!("AAC in {other:?}")),
    };
    if !offered.contains(&config.kbps) {
        return unsupported(format!(
            "AAC {:?} at {} kbps (the Media Foundation encoder offers {:?} kbps for it)",
            f.layout(),
            config.kbps,
            offered
        ));
    }
    Ok(config.kbps * 1000 / 8)
}

/// Writes `samples` as 16-bit little-endian PCM into `out` (two bytes per sample, as
/// many as both hold), clamping to `-1.0..=1.0` and rounding. NaN becomes silence.
pub(crate) fn write_pcm16(samples: &[f32], out: &mut [u8]) {
    for (o, &s) in out.chunks_exact_mut(2).zip(samples) {
        let v = if s.is_nan() {
            0
        } else {
            // In range after the clamp, so the cast can't saturate.
            (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
        };
        o.copy_from_slice(&v.to_le_bytes());
    }
}

/// Counts frames in and access units out, and gives each packet its `pts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Framing {
    /// The first buffer's `pts`, in the input's sample time base.
    start: Option<MediaTime>,
    /// Frames handed to the encoder so far, padding included.
    frames_in: u64,
    /// Access units already given a `pts`.
    emitted: u64,
}

impl Framing {
    /// Records a buffer of `frames` frames starting at `pts` (only the first buffer's
    /// time is used).
    pub(crate) fn input(
        &mut self,
        pts: MediaTime,
        frames: u64,
        format: AudioFormat,
    ) -> Result<(), PlatformError> {
        if self.start.is_none() {
            self.start = Some(pts.rescale(format.timebase(), Rounding::Nearest)?);
        }
        self.frames_in = self.frames_in.saturating_add(frames);
        Ok(())
    }

    /// Records `frames` frames of silence added to complete the last access unit.
    pub(crate) fn pad(&mut self, frames: u64) {
        self.frames_in = self.frames_in.saturating_add(frames);
    }

    /// Frames handed to the encoder so far, padding included.
    pub(crate) fn frames_in(&self) -> u64 {
        self.frames_in
    }

    /// The silence needed to complete the last access unit: 0 when the input so far is a
    /// whole number of access units, or when nothing has been encoded.
    pub(crate) fn padding(&self) -> u64 {
        if self.start.is_none() {
            return 0;
        }
        (AAC_FRAME - self.frames_in % AAC_FRAME) % AAC_FRAME
    }

    /// The `pts` of the next access unit: the start plus the frames before it.
    pub(crate) fn next_pts(&mut self) -> Result<MediaTime, PlatformError> {
        let start = self.start.ok_or(PlatformError::InvalidState {
            what: "AAC packet timing",
            state: "the encoder produced output before any input",
        })?;
        let offset =
            i64::try_from(self.emitted * AAC_FRAME).map_err(|_| PlatformError::InvalidState {
                what: "AAC packet timing",
                state: "the stream is too long",
            })?;
        self.emitted += 1;
        Ok(start.checked_add(MediaTime::new(offset, start.timebase()))?)
    }
}

#[cfg(windows)]
pub(crate) use encoder::MfAacEncoder;

#[cfg(windows)]
mod encoder {
    use jstream_platform::media::{AudioBuffer, Packet};
    use jstream_platform::{AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, PlatformError};
    use windows::Win32::Media::MediaFoundation::{
        MF_E_NOTACCEPTING, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
        MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
        MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MFAudioFormat_AAC,
        MFAudioFormat_PCM, MFCreateMediaType, MFMediaType_Audio, MFT_CATEGORY_AUDIO_ENCODER,
        MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    };

    use super::{AAC_FRAME, Framing, check, write_pcm16};
    use crate::codec::mf::{self, MfRuntime, Mft, Output, PullMode};
    use crate::com::OsResult;

    /// One AAC encoder session on the Media Foundation AAC encoder MFT.
    pub(crate) struct MfAacEncoder {
        // Fields drop in order: the transform before the runtime that keeps MF started.
        mft: Mft,
        output: Output,
        info: CodecInfo,
        config: AudioEncoderConfig,
        framing: Framing,
        /// Staging for the 16-bit PCM of one buffer; grows to the largest buffer seen.
        pcm: Vec<u8>,
        pending: Vec<Packet>,
        finished: bool,
        _mf: MfRuntime,
    }

    // SAFETY: the encoder session is owned and driven by one thread at a time (`&mut self`
    // for every call; the trait asks only for `Send`). The AAC encoder MFT is an in-proc,
    // free-threaded ("Both") object, and `MfRuntime` keeps the process MTA alive, so a
    // thread that never initialised COM may call it as an implicit MTA member.
    unsafe impl Send for MfAacEncoder {}

    impl MfAacEncoder {
        /// Opens the first AAC encoder MFT that takes 16-bit PCM.
        pub(crate) fn open(config: &AudioEncoderConfig) -> Result<Self, PlatformError> {
            let avg_bytes = check(config)?;
            let rt = MfRuntime::start()?;
            let found = mf::enumerate(
                MFT_CATEGORY_AUDIO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                (MFMediaType_Audio, MFAudioFormat_PCM),
                (MFMediaType_Audio, MFAudioFormat_AAC),
            )?;
            let Some(mft) = found.into_iter().next() else {
                return Err(PlatformError::Unsupported {
                    what: "AAC encoding: no Media Foundation AAC encoder is installed".into(),
                });
            };
            let session = mft.activate()?;
            let transform = &session.transform;
            let f = config.input_format;
            let channels = f.channels() as u32;
            let rate = f.sample_rate();
            // SAFETY: plain COM calls on live objects created here.
            unsafe {
                let out = MFCreateMediaType().os("MFCreateMediaType")?;
                out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .and_then(|()| out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC))
                    .and_then(|()| out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))
                    .and_then(|()| out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate))
                    .and_then(|()| out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))
                    .and_then(|()| out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg_bytes))
                    // 0: raw access units, no ADTS.
                    .and_then(|()| out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0))
                    .os("IMFMediaType::Set* (AAC output)")?;
                transform
                    .SetOutputType(0, &out, 0)
                    .os("IMFTransform::SetOutputType (AAC)")?;
                let input = MFCreateMediaType().os("MFCreateMediaType")?;
                input
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM))
                    .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))
                    .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate))
                    .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))
                    .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * channels))
                    .and_then(|()| {
                        input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 2 * channels * rate)
                    })
                    .os("IMFMediaType::Set* (PCM input)")?;
                transform
                    .SetInputType(0, &input, 0)
                    .os("IMFTransform::SetInputType (PCM)")?;
            }
            // One AAC access unit is at most 768 bytes per channel.
            let output = Output::new(transform, 768 * channels)?;
            // SAFETY: plain COM calls on a configured transform.
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .and_then(|()| transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                    .os("IMFTransform::ProcessMessage (start)")?;
            }
            Ok(Self {
                mft: session,
                output,
                info: CodecInfo {
                    name: mft.name,
                    codec: config.codec,
                    role: CodecRole::AudioEncoder,
                    hardware: false,
                },
                config: *config,
                framing: Framing::default(),
                pcm: Vec::new(),
                pending: Vec::new(),
                finished: false,
                _mf: rt,
            })
        }

        /// Hands `frames` frames of interleaved samples (silence when `samples` is
        /// `None`) to the MFT as 16-bit PCM, then collects whatever it produced.
        fn feed(&mut self, samples: Option<&[f32]>, frames: u64) -> Result<(), PlatformError> {
            let channels = self.config.input_format.channels();
            let bytes = frames as usize * channels * 2;
            if self.pcm.len() < bytes {
                self.pcm.resize(bytes, 0);
            }
            let pcm = &mut self.pcm[..bytes];
            match samples {
                Some(s) => write_pcm16(s, pcm),
                None => pcm.fill(0),
            }
            let rate = u64::from(self.config.input_format.sample_rate());
            let hns = |f: u64| i64::try_from(f * 10_000_000 / rate).unwrap_or(i64::MAX);
            let start = self.framing.frames_in() - frames;
            let sample = mf::sample_from(pcm, hns(start), hns(start + frames) - hns(start))?;
            // SAFETY: plain COM call; the sample is a live object built above.
            match unsafe { self.mft.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    // A synchronous MFT only refuses input while it holds output: collect
                    // it, then the input fits.
                    self.collect()?;
                    // SAFETY: as above.
                    unsafe { self.mft.transform.ProcessInput(0, &sample, 0) }
                        .os("IMFTransform::ProcessInput (AAC)")?;
                }
                Err(e) => return Err(crate::com::os_error("IMFTransform::ProcessInput (AAC)", &e)),
            }
            self.collect()
        }

        /// Moves every access unit the MFT has ready into `pending`.
        fn collect(&mut self) -> Result<(), PlatformError> {
            while let Some(sample) = self.output.pull(&self.mft.transform, PullMode::Drain)? {
                let pts = self.framing.next_pts()?;
                self.pending
                    .push(Packet::new(self.info.codec, true, pts, pts, sample.data)?);
            }
            Ok(())
        }
    }

    impl AudioEncoder for MfAacEncoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "AAC encode",
                    state: "the encoder is finished",
                });
            }
            if buffer.format() != self.config.input_format {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "audio in {:?} (configured for {:?})",
                        buffer.format(),
                        self.config.input_format
                    ),
                });
            }
            if buffer.frames() == 0 {
                return Ok(());
            }
            let frames = buffer.frames() as u64;
            self.framing
                .input(buffer.pts(), frames, self.config.input_format)?;
            self.feed(Some(buffer.samples()), frames)
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            out.append(&mut self.pending);
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished {
                self.finished = true;
                let pad = self.framing.padding();
                if pad > 0 {
                    self.framing.pad(pad);
                    self.feed(None, pad)?;
                }
                debug_assert_eq!(self.framing.frames_in() % AAC_FRAME, 0);
                // SAFETY: plain COM calls on a streaming transform.
                unsafe {
                    self.mft
                        .transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                        .and_then(|()| {
                            self.mft
                                .transform
                                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                        })
                        .os("IMFTransform::ProcessMessage (drain)")?;
                }
                self.collect()?;
            }
            self.poll(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        AudioFormat, ChannelLayout, Codec, MediaTime, SampleFormat, Timebase,
    };
    use jstream_platform::{AudioEncoderConfig, PlatformError};

    use super::{AAC_FRAME, Framing, check, write_pcm16};

    fn config(rate: u32, layout: ChannelLayout, kbps: u32) -> AudioEncoderConfig {
        AudioEncoderConfig {
            codec: Codec::Aac,
            input_format: AudioFormat::new(rate, layout, SampleFormat::F32Interleaved)
                .expect("valid format"),
            kbps,
        }
    }

    #[test]
    fn accepts_what_the_mf_encoder_supports() {
        assert_eq!(
            check(&config(48_000, ChannelLayout::Stereo, 128)),
            Ok(16_000)
        );
        assert_eq!(check(&config(44_100, ChannelLayout::Mono, 96)), Ok(12_000));
        assert_eq!(
            check(&config(48_000, ChannelLayout::Surround51, 576)),
            Ok(72_000)
        );
        assert_eq!(
            check(&config(48_000, ChannelLayout::Surround51, 1152)),
            Ok(144_000)
        );
    }

    #[test]
    fn refuses_the_rest() {
        let unsupported =
            |c: AudioEncoderConfig| matches!(check(&c), Err(PlatformError::Unsupported { .. }));
        assert!(unsupported(config(32_000, ChannelLayout::Stereo, 128)));
        assert!(unsupported(config(48_000, ChannelLayout::Stereo, 320)));
        // Each layout has its own bit rates: stereo ones don't apply to 5.1, or back.
        assert!(unsupported(config(48_000, ChannelLayout::Surround51, 192)));
        assert!(unsupported(config(48_000, ChannelLayout::Stereo, 576)));
        let mut c = config(48_000, ChannelLayout::Stereo, 128);
        c.codec = Codec::H264;
        assert!(unsupported(c));
        let mut c = config(48_000, ChannelLayout::Stereo, 128);
        c.input_format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Planar)
            .expect("valid");
        assert!(unsupported(c));
    }

    #[test]
    fn pcm16_clamps_rounds_and_silences_nan() {
        let mut out = [0u8; 12];
        write_pcm16(&[0.0, 1.0, -1.0, 2.0, 0.5, f32::NAN], &mut out);
        let v: Vec<i16> = out
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(v, [0, 32_767, -32_767, 32_767, 16_384, 0]);
    }

    #[test]
    fn packets_count_from_the_first_buffer() {
        let fmt = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("valid");
        let mut f = Framing::default();
        assert_eq!(f.padding(), 0);
        // The first buffer starts at 1 s on a nanosecond clock; later pts are ignored.
        f.input(MediaTime::new(1_000_000_000, Timebase::NANOS), 1500, fmt)
            .expect("input");
        f.input(MediaTime::new(9, Timebase::NANOS), 600, fmt)
            .expect("input");
        assert_eq!(f.frames_in(), 2100);
        assert_eq!(f.padding(), 3 * AAC_FRAME - 2100);
        let first = f.next_pts().expect("pts");
        assert_eq!((first.ticks(), first.timebase()), (48_000, fmt.timebase()));
        assert_eq!(f.next_pts().expect("pts").ticks(), 48_000 + 1024);
        f.pad(f.padding());
        assert_eq!(f.padding(), 0);
        assert_eq!(f.frames_in(), 3 * AAC_FRAME);
    }

    #[test]
    fn output_before_input_is_an_error() {
        assert!(matches!(
            Framing::default().next_pts(),
            Err(PlatformError::InvalidState { .. })
        ));
    }
}
