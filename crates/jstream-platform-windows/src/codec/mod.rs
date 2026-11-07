//! Media Foundation codecs: the [`Codecs`](jstream_platform::Codecs) backend.
//!
//! It has the AAC encoder (`aac`) and the H.264, HEVC and AV1 encoders (`video`), from
//! CPU NV12, RGBA or BGRA frames, or from encode targets on the GPU once the backend is on
//! a GPU context (`Codecs::on_context`). On the CPU path with hardware `Prefer`, a
//! hardware encoder that refuses its first frame falls back to software (`fallback`). HEVC and AV1 run where the GPU (or an installed
//! software MFT) offers them. Video decoders follow; until then they report
//! `Unsupported`.
//!
//! # Behaviour
//!
//! **`MfCodecs`** (`Codecs`), Media Foundation codecs
//! (ADR-0061): the **AAC encoder** and
//! the **video encoders** (H.264, HEVC and AV1), from CPU frames or from encode targets on
//! the GPU. AAC:
//! - **Encoder:** the first AAC encoder MFT Media Foundation ranks for 16-bit PCM in. On
//!   Windows that is Microsoft's software AAC encoder. It takes 44.1 or 48 kHz; mono or
//!   stereo at 96, 128, 160 or 192 kbps, or 5.1 at six times those (576, 768, 960 or
//!   1152 kbps). Any other configuration is `Unsupported`.
//! - **Input:** samples are converted from `f32` to 16-bit PCM, clamped and rounded.
//! - **Output:** one raw AAC-LC access unit (1024 frames, no ADTS) per packet, the framing
//!   `jstream-media` defines. Packet times count from the first buffer's `pts`, in the
//!   input's sample time base, as the `AudioEncoder` trait says. `finish` pads the last
//!   access unit with silence, then drains the MFT. An empty output sample is skipped.
//! - **Threads:** each session keeps Media Foundation started and the process MTA alive
//!   (`CoIncrementMTAUsage`), so the engine's encode thread can drive it without
//!   initialising COM. Sessions are opened from a thread that isn't an STA.
//! - **Teardown:** dropping a session ends streaming and calls `ShutdownObject` on the
//!   activation object, as Microsoft asks. One output sample is reused for every pull.
//! - **Video encoders:** H.264 (High), HEVC (Main) and AV1 (Main), all 8-bit 4:2:0 from
//!   NV12, through one session type (`codec/video.rs`) that differs per codec only in
//!   the MFT's output subtype and profile. H.264 and HEVC packets are Annex B, AV1 packets
//!   one temporal unit of low-overhead OBUs, as `jstream-media` defines; the first
//!   keyframe carries the parameter sets (or sequence header) in-band, which `jstream-mux`'s
//!   enhanced RTMP track builds its decoder configuration from. Windows ships a software
//!   encoder for H.264 only, so HEVC and AV1 open only where the GPU has a hardware
//!   encoder (or a software MFT is installed); elsewhere they are `Unsupported`.
//!   `list` reports every encoder MFT for each of the three.
//! - **Video encoders, CPU path:** `MfCodecs::new()`'s encoders take NV12, RGBA or BGRA
//!   `VideoSurface::Cpu` frames and copy them into Media Foundation memory buffers as
//!   NV12, the MFTs' input. RGBA (the compositor's output, which the engine's pipeline
//!   encodes) and BGRA are converted first, the way display capture converts WGC's BGRA:
//!   BT.709 limited range, left-sited chroma. It is ARCH's fallback path
//!   (ARCH › GPU pipeline, step 6).
//! - **Video encoders, GPU path** (step 5): `Codecs::on_context` on a context `WindowsGpu`
//!   opened (another backend's is `InvalidState`) gives the same backend on that context.
//!   Its video encoders take that context's NV12 encode targets (`VideoSurface::Gpu`) and
//!   nothing else: a CPU frame, or a GPU frame that isn't one of its encode targets, is
//!   `Unsupported`. Audio is unchanged.
//!   - **Choosing an encoder:** only hardware encoder MFTs on the context's adapter
//!     (`MFTEnum2` with the adapter's LUID) that say they are D3D11-aware
//!     (`MF_SA_D3D11_AWARE`). Each gets a DXGI device manager on the context's D3D11
//!     device (`MFT_MESSAGE_SET_D3D_MANAGER`) before its media types, which are the CPU
//!     path's. A configuration that avoids hardware is `Unsupported`, and so is a machine
//!     where no such MFT accepts it; the engine then opens a CPU encoder
//!     (`jstream-encode`'s `VideoSession::open_on`).
//!   - **Input:** each encode target is copied on the GPU into a texture from a sample
//!     allocator on the same device (eight at most, with the encode targets' bind flags),
//!     and that sample is encoded. So the target goes back to its producer as soon as the
//!     copy is queued, however long the MFT holds its input. When the MFT holds all eight,
//!     the frame is refused as `Busy`, which the session drops and counts.
//!   - **Sync:** the copy waits on the context's fence for the target's last staged
//!     write, and D3D11 then signals the context's release fence, which the target's next
//!     staged write waits for (`WindowsGpu`, in `crate::gpu`). Neither waits on the CPU. The encoder
//!     drops the frame once the copy is queued, so a producer writes a target again only
//!     when it holds the frame's only clone.
//! - **Video encoders, both paths:**
//!   - **Choosing an encoder** on the CPU path: hardware encoder MFTs come first, then the
//!     OS software encoder (H.264 only), following `HardwarePreference`. The first one that accepts the
//!     media types wins, and `CodecInfo::hardware` says which kind it was. When
//!     `Prefer` ends on the software MFT this way, `jstream-core` logs a warning naming
//!     the output and why (#443 case 1), so this crate logs nothing for it.
//!   - **First-frame fallback** (#443): with `Prefer`, a hardware encoder on the CPU path
//!     that refuses the first frame at `ProcessInput` is replaced by the software MFT,
//!     which gets that frame (`FirstFrameFallback`, `codec/fallback.rs`). No packet has
//!     come out yet, so the stream sees one decoder configuration. It logs a warning
//!     through `tracing` (the crate's one logging dependency) naming both encoders and
//!     the refusal, and `info()` then reports the software encoder. The encoder doesn't
//!     know its outputs, so `jstream-core` logs one naming each output on the session
//!     (#965). Once the hardware
//!     encoder has taken a frame, a refusal fails the session as before. `Require` and
//!     the GPU path have no software to fall back to. The logic is tested with fake
//!     encoders on every platform.
//!   - **Configuration:** CBR or peak-constrained VBR, the keyframe interval as the GOP
//!     size, no B-frames (so packets come out in presentation order and `dts == pts`),
//!     low latency, quality over speed (`QualityVsSpeed` 70, the band hardware MFTs map to
//!     their quality preset), the codec's profile from its `Format` (H.264 High, HEVC Main or AV1
//!     Main), and BT.709 limited range. `ICodecAPI` settings are
//!     best effort; the media type always carries the bit rate.
//!   - **Timing:** each packet gets back its frame's exact `pts`, even when the frame's
//!     time base doesn't round evenly to the encoder's 100 ns. Keyframes come from
//!     `MFSampleExtension_CleanPoint`.
//!   - **Keyframe on demand:** `request_keyframe` sets `CODECAPI_AVEncVideoForceKeyFrame`,
//!     so the next input becomes an IDR frame. An encoder without `ICodecAPI`, or one
//!     that refuses the property, reports `Unsupported` and keeps its GOP cadence.
//!     Whether the GOP count restarts at a forced keyframe is up to the encoder.
//!   - **Asynchronous MFTs** (most hardware encoders) ask for input and announce output
//!     through events. `encode` and `poll` pump those events, so `can_accept` reflects the
//!     last pump, and the encode thread polls every tick. Each output event gets exactly
//!     one `ProcessOutput` call, as Microsoft's asynchronous-MFT model requires. `finish`
//!     waits up to 5 s for the drain to complete.
//!   - **Stream changes:** when an encoder changes its output type mid-stream, the
//!     configured frame size, frame rate, bit rate and profile are re-applied to the type
//!     it offers. If it refuses them, the offered type is taken as is. The output buffer
//!     size is then read again.
//! - **Not yet:** video decoders, which report `Unsupported` until then.

// Its platform-independent parts are tested on every platform.
#[cfg(any(windows, test))]
mod aac;
#[cfg(any(windows, test))]
mod fallback;
#[cfg(windows)]
mod mf;
#[cfg(any(windows, test))]
mod video;

#[cfg(windows)]
pub use backend::MfCodecs;

#[cfg(windows)]
mod backend {
    use std::sync::Arc;

    use jstream_platform::media::Codec;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CaptureDecoder, CaptureEncoding, CodecInfo, CodecRole,
        Codecs, GpuContext, PlatformError, VideoDecoder, VideoDecoderConfig, VideoEncoder,
        VideoEncoderConfig,
    };
    use windows::Win32::Media::MediaFoundation::{
        MFAudioFormat_AAC, MFAudioFormat_PCM, MFMediaType_Audio, MFMediaType_Video,
        MFT_CATEGORY_AUDIO_ENCODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFVideoFormat_NV12,
    };

    use super::aac::MfAacEncoder;
    use super::fallback::FirstFrameFallback;
    use super::mf::{self, MfRuntime};
    use super::video::{Format, MfVideoEncoder};
    use crate::gpu::EncodeGpu;

    /// Codecs through Media Foundation, the OS codecs ADR-0061 chooses.
    ///
    /// It holds no COM objects, so it is `Send + Sync`. Each session starts Media
    /// Foundation for itself and can be driven from any thread. Open sessions from a
    /// thread that is not a single-threaded apartment (not a UI thread): the MFTs are
    /// free-threaded, but one created in an STA would belong to it.
    ///
    /// [`Codecs::on_context`] gives the same backend on a `WindowsGpu` context, whose
    /// video encoders take that context's encode targets instead of CPU frames.
    #[derive(Debug, Default)]
    pub struct MfCodecs {
        /// The context its video encoders read encode targets on; `None` for CPU frames.
        gpu: Option<EncodeGpu>,
    }

    impl MfCodecs {
        /// The backend. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl Codecs for MfCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            let _rt = MfRuntime::start()?;
            let sync = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER;
            let info = |found: mf::Found, codec, role, hardware| CodecInfo {
                name: found.name,
                codec,
                role,
                hardware,
            };
            let mut out = Vec::new();
            let aac = mf::enumerate(
                MFT_CATEGORY_AUDIO_ENCODER,
                sync,
                (MFMediaType_Audio, MFAudioFormat_PCM),
                (MFMediaType_Audio, MFAudioFormat_AAC),
            )?;
            // Only the first is listed: it is the one `audio_encoder` opens.
            out.extend(
                aac.into_iter()
                    .take(1)
                    .map(|f| info(f, Codec::Aac, CodecRole::AudioEncoder, false)),
            );
            for format in Format::ALL {
                let nv12 = (MFMediaType_Video, MFVideoFormat_NV12);
                let coded = (MFMediaType_Video, format.subtype());
                for (flags, hardware) in [
                    (MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, true),
                    (sync, false),
                ] {
                    out.extend(
                        mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, nv12, coded)?
                            .into_iter()
                            .map(|f| info(f, format.codec(), CodecRole::VideoEncoder, hardware)),
                    );
                }
            }
            Ok(out)
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            // The GPU path has hardware encoders only, so nothing to fall back to.
            if let Some(gpu) = &self.gpu {
                return Ok(Box::new(MfVideoEncoder::open_on(config, gpu)?));
            }
            let encoder = MfVideoEncoder::open(config)?;
            if !FirstFrameFallback::applies(config, encoder.info()) {
                return Ok(Box::new(encoder));
            }
            // A hardware MFT can take the media types and still refuse its first frame
            // at `ProcessInput` (#443): then the session moves to the software MFT.
            Ok(Box::new(FirstFrameFallback::new(
                Box::new(encoder),
                *config,
                Box::new(|config: &VideoEncoderConfig| {
                    Ok(Box::new(MfVideoEncoder::open(config)?) as Box<dyn VideoEncoder>)
                }),
            )))
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            Ok(Box::new(MfAacEncoder::open(config)?))
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: format!("{:?} video decoding (not implemented yet)", config.codec),
            })
        }

        /// MJPEG through Media Foundation's MJPEG decoder ([`MfMjpegDecoder`]), into
        /// NV12 CPU frames.
        ///
        /// [`MfMjpegDecoder`]: crate::MfMjpegDecoder
        fn capture_decoder(
            &self,
            encoding: CaptureEncoding,
        ) -> Result<Box<dyn CaptureDecoder>, PlatformError> {
            match encoding {
                CaptureEncoding::Mjpeg => Ok(Box::new(crate::MfMjpegDecoder::new()?)),
                other => Err(PlatformError::Unsupported {
                    what: format!("decoding {other:?} capture frames"),
                }),
            }
        }

        /// The backend on a context `WindowsGpu` opened. Its video encoders take that
        /// context's encode targets; audio is unchanged.
        fn on_context(
            &self,
            context: &GpuContext,
        ) -> Result<Option<Arc<dyn Codecs>>, PlatformError> {
            Ok(Some(Arc::new(Self {
                gpu: Some(EncodeGpu::of(context)?),
            })))
        }
    }
}

/// Platform tests against the OS encoders (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because a CI runner may lack Media Foundation or a GPU encoder. Run them on a Windows
/// machine: `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
#[cfg(all(test, windows, feature = "hw-tests"))]
mod device_tests {
    use jstream_platform::media::{
        AudioBuffer, AudioFormat, ChannelLayout, Codec, MediaTime, SampleFormat, Timebase,
    };
    use jstream_platform::{AudioEncoderConfig, Codecs};

    use super::MfCodecs;

    #[test]
    #[ignore = "needs Media Foundation's AAC encoder"]
    fn encodes_a_second_of_tone_to_aac_access_units() {
        let codecs = MfCodecs::new();
        assert!(!codecs.list().expect("list").is_empty());
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let mut enc = codecs
            .audio_encoder(&AudioEncoderConfig {
                codec: Codec::Aac,
                input_format: format,
                kbps: 128,
            })
            .expect("open AAC encoder");
        // One second as 100 buffers of 480 frames, starting at 2 s on a 100 ns clock.
        let mut out = Vec::new();
        let mut phase = 0.0f32;
        for i in 0..100i64 {
            let mut samples = Vec::with_capacity(960);
            for _ in 0..480 {
                let v = 0.25 * (phase * std::f32::consts::TAU).sin();
                samples.extend([v, v]);
                phase = (phase + 440.0 / 48_000.0).fract();
            }
            let pts = MediaTime::new(
                20_000_000 + i * 100_000,
                Timebase::new(1, 10_000_000).expect("tb"),
            );
            let buf = AudioBuffer::new(format, pts, samples).expect("buffer");
            enc.encode(&buf).expect("encode");
            enc.poll(&mut out).expect("poll");
        }
        enc.finish(&mut out).expect("finish");
        // 48000 frames is 46.875 access units, so 47 once padded; the MFT may hold back
        // or add a priming unit, so allow one either way.
        assert!((46..=48).contains(&out.len()), "{} packets", out.len());
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.codec(), Codec::Aac);
            assert!(!p.data().is_empty());
            assert_eq!(p.pts().ticks(), 96_000 + 1024 * i as i64, "packet {i}");
            assert_eq!(p.pts().timebase(), format.timebase());
        }
        assert!(
            enc.encode(
                &AudioBuffer::silence(format, MediaTime::zero(format.timebase()), 10).expect("buf")
            )
            .is_err()
        );
    }

    /// The NAL unit types after each start code of an Annex B payload, with `kind`
    /// reading the type from the first header byte.
    fn nal_types(data: &[u8], kind: fn(u8) -> u8) -> Vec<u8> {
        data.windows(4)
            .filter(|w| w[..3] == [0, 0, 1])
            .map(|w| kind(w[3]))
            .collect()
    }

    /// The OBU types of a low-overhead AV1 payload, or `None` when it doesn't parse.
    fn obu_types(mut data: &[u8]) -> Option<Vec<u8>> {
        let mut types = Vec::new();
        while let Some(&header) = data.first() {
            if header & 0x80 != 0 {
                return None;
            }
            let mut at = 1 + usize::from(header & 0x04 != 0);
            let size = if header & 0x02 != 0 {
                let mut size = 0usize;
                for i in 0..8 {
                    let byte = *data.get(at)?;
                    at += 1;
                    size |= usize::from(byte & 0x7F) << (7 * i);
                    if byte & 0x80 == 0 {
                        break;
                    }
                }
                size
            } else {
                data.len().checked_sub(at)?
            };
            types.push((header >> 3) & 0x0F);
            data = data.get(at + size..)?;
        }
        Some(types)
    }

    /// Checks that every packet is framed as `jstream-media` says for `codec`, and that
    /// the first keyframe carries the decoder configuration in-band, which the FLV
    /// muxer's enhanced RTMP track builds its sequence header from.
    fn assert_framing(codec: Codec, out: &[jstream_platform::media::Packet]) {
        for p in out {
            assert_eq!(p.codec(), codec);
            assert_eq!(p.pts(), p.dts());
            let data = p.data();
            match codec {
                Codec::Av1 => assert!(obu_types(data).is_some(), "low-overhead OBUs"),
                _ => assert!(
                    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1]),
                    "Annex B: a start code first"
                ),
            }
        }
        let key = out[0].data();
        match codec {
            Codec::H264 => assert!(nal_types(key, |b| b & 0x1F).contains(&7), "an SPS"),
            Codec::Hevc => {
                let types = nal_types(key, |b| (b >> 1) & 0x3F);
                assert!(
                    [32, 33, 34].iter().all(|t| types.contains(t)),
                    "VPS, SPS and PPS: {types:?}"
                );
            }
            _ => {
                let types = obu_types(key).expect("OBUs");
                assert!(types.contains(&1), "a sequence header: {types:?}");
            }
        }
    }

    /// Encodes 60 frames of a moving gradient in `codec` from `format` frames (NV12, or
    /// the compositor's RGBA that the engine's pipeline hands the encoder).
    fn encode_two_seconds(
        codec: Codec,
        hardware: jstream_platform::HardwarePreference,
        format: jstream_platform::media::PixelFormat,
    ) {
        use jstream_platform::media::{
            ColorInfo, FrameSize, PixelFormat, VideoFrame, VideoFrameInfo,
        };
        use jstream_platform::{RateControl, VideoEncoderConfig, VideoSurface};

        let codecs = MfCodecs::new();
        let size = FrameSize::new(1280, 720).expect("size");
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec,
                size,
                input_format: format,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control: RateControl::Cbr { kbps: 3000 },
                keyframe_interval: 30,
                hardware,
            })
            .unwrap_or_else(|e| panic!("open a {codec:?} encoder: {e}"));
        assert_eq!(
            enc.info().hardware,
            hardware == jstream_platform::HardwarePreference::Require
        );
        let tb = Timebase::MPEG_90K;
        let mut out = Vec::new();
        let mut sent = 0;
        for i in 0..60i64 {
            let info = VideoFrameInfo {
                size,
                format,
                color: ColorInfo::default(),
                pts: MediaTime::new(i * 3000, tb),
            };
            let mut frame = VideoFrame::zeroed(info).expect("frame");
            // A moving gradient, so the encoder has something to code: grey in each
            // byte of an RGBA pixel, or the luma of NV12 with neutral chroma.
            let bytes_per_px = if format == PixelFormat::Nv12 { 1 } else { 4 };
            for y in 0..720u32 {
                if let Some(row) = frame.row_mut(0, y) {
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = ((x / bytes_per_px) as i64 + y as i64 + i * 4) as u8;
                    }
                }
            }
            for y in 0..360u32 {
                if let Some(row) = frame.row_mut(1, y) {
                    row.fill(128);
                }
            }
            // An asynchronous encoder may not want input yet: poll until it does.
            for _ in 0..1000 {
                enc.poll(&mut out).expect("poll");
                if enc.can_accept() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            enc.encode(VideoSurface::Cpu(frame)).expect("encode");
            sent += 1;
            enc.poll(&mut out).expect("poll");
        }
        enc.finish(&mut out).expect("finish");
        assert!(
            out.len() >= sent - 2,
            "{} packets for {sent} frames",
            out.len()
        );
        assert!(out[0].is_keyframe(), "the first packet is a keyframe");
        assert_eq!(out[0].pts(), MediaTime::new(0, tb));
        for w in out.windows(2) {
            assert!(w[0].pts().ticks() < w[1].pts().ticks(), "pts increase");
        }
        assert_framing(codec, &out);
        assert!(
            out.iter().filter(|p| p.is_keyframe()).count() >= 2,
            "keyframe every 30"
        );
    }

    #[test]
    #[ignore = "needs Media Foundation's software H.264 encoder"]
    fn encodes_h264_in_software() {
        use jstream_platform::media::PixelFormat;
        encode_two_seconds(
            Codec::H264,
            jstream_platform::HardwarePreference::Avoid,
            PixelFormat::Nv12,
        );
    }

    /// #424: the engine's pipeline encodes the compositor's RGBA frames.
    #[test]
    #[ignore = "needs Media Foundation's software H.264 encoder"]
    fn encodes_h264_from_rgba() {
        use jstream_platform::media::PixelFormat;
        encode_two_seconds(
            Codec::H264,
            jstream_platform::HardwarePreference::Avoid,
            PixelFormat::Rgba8,
        );
    }

    #[test]
    #[ignore = "needs a GPU with a hardware H.264 encoder"]
    fn encodes_h264_in_hardware() {
        use jstream_platform::media::PixelFormat;
        encode_two_seconds(
            Codec::H264,
            jstream_platform::HardwarePreference::Require,
            PixelFormat::Rgba8,
        );
    }

    #[test]
    #[ignore = "needs a GPU with a hardware HEVC encoder"]
    fn encodes_hevc_in_hardware() {
        use jstream_platform::media::PixelFormat;
        encode_two_seconds(
            Codec::Hevc,
            jstream_platform::HardwarePreference::Require,
            PixelFormat::Rgba8,
        );
    }

    #[test]
    #[ignore = "needs a GPU with a hardware AV1 encoder (NVIDIA RTX 40, AMD RX 7000, Intel Arc or newer)"]
    fn encodes_av1_in_hardware() {
        use jstream_platform::media::PixelFormat;
        encode_two_seconds(
            Codec::Av1,
            jstream_platform::HardwarePreference::Require,
            PixelFormat::Rgba8,
        );
    }

    /// Clears an NV12 encode target's planes to `luma` and neutral chroma, hands it back
    /// to D3D11 (`PRESENT`), and submits with its fences staged, as render will.
    fn draw_target(
        context: &jstream_platform::GpuContext,
        frame: &jstream_platform::GpuFrame,
        luma: f64,
    ) {
        let plane = |aspect, format| {
            frame.texture().create_view(&wgpu::TextureViewDescriptor {
                label: Some("nv12 plane"),
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let planes = [
            (
                plane(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm),
                luma,
            ),
            (
                plane(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm),
                0.5,
            ),
        ];
        let mut enc = context.device().create_command_encoder(&Default::default());
        for (view, value) in &planes {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear nv12 plane"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: *value,
                            g: 0.5,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        enc.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: frame.texture(),
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        frame.stage(context.queue()).expect("stage");
        context.queue().submit([enc.finish()]);
    }

    /// The GPU path end to end: encode targets written by wgpu on the platform's DX12
    /// device, copied on D3D11 into the encoder's own textures, and encoded by a hardware
    /// MFT on the same adapter, with no CPU copy. A ring of three targets is reused, as
    /// render will, each written again only once the encoder has let go of it.
    #[test]
    #[ignore = "needs a DX12 GPU with NV12 textures and a D3D11-aware hardware H.264 encoder"]
    fn encodes_h264_from_encode_targets() {
        encode_from_targets(Codec::H264);
    }

    #[test]
    #[ignore = "needs a DX12 GPU with NV12 textures and a D3D11-aware hardware HEVC encoder"]
    fn encodes_hevc_from_encode_targets() {
        encode_from_targets(Codec::Hevc);
    }

    #[test]
    #[ignore = "needs a DX12 GPU with NV12 textures and a D3D11-aware hardware AV1 encoder"]
    fn encodes_av1_from_encode_targets() {
        encode_from_targets(Codec::Av1);
    }

    fn encode_from_targets(codec: Codec) {
        use std::sync::Arc;

        use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase};
        use jstream_platform::{
            GpuInterop, HardwarePreference, PlatformError, RateControl, VideoEncoderConfig,
            VideoSurface,
        };

        use crate::WindowsGpu;

        let gpu = WindowsGpu::new();
        let context = gpu.open_device().expect("open the media adapter's device");
        let codecs = MfCodecs::new()
            .on_context(&context)
            .expect("our context")
            .expect("MF has a GPU path");
        let size = FrameSize::new(1280, 720).expect("size");
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec,
                size,
                input_format: PixelFormat::Nv12,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control: RateControl::Cbr { kbps: 3000 },
                keyframe_interval: 30,
                hardware: HardwarePreference::Require,
            })
            .unwrap_or_else(|e| {
                panic!("a D3D11-aware hardware {codec:?} encoder on the context's adapter: {e}")
            });
        assert!(enc.info().hardware);
        let ring: Vec<_> = (0..3)
            .map(|_| gpu.encode_target(&context, size).expect("encode target"))
            .collect();
        let tb = Timebase::MPEG_90K;
        let mut out = Vec::new();
        let mut sent = 0;
        for i in 0..60i64 {
            let target = &ring[i as usize % ring.len()];
            // The encoder dropped its clone after copying, so the ring holds the only one.
            assert_eq!(
                Arc::strong_count(target.sync()),
                1,
                "frame {i}: the target is still held"
            );
            // A moving shade, so the encoder has something to code.
            draw_target(&context, target, (i % 30) as f64 / 30.0);
            for _ in 0..1000 {
                enc.poll(&mut out).expect("poll");
                if enc.can_accept() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let frame = target.clone().with_pts(MediaTime::new(i * 3000, tb));
            enc.encode(VideoSurface::Gpu(frame)).expect("encode");
            sent += 1;
            enc.poll(&mut out).expect("poll");
        }
        // A CPU frame is refused on the GPU path, without ending the stream.
        let cpu =
            jstream_platform::media::VideoFrame::zeroed(jstream_platform::media::VideoFrameInfo {
                size,
                format: PixelFormat::Nv12,
                color: jstream_platform::media::ColorInfo::default(),
                pts: MediaTime::new(60 * 3000, tb),
            })
            .expect("frame");
        for _ in 0..1000 {
            enc.poll(&mut out).expect("poll");
            if enc.can_accept() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(matches!(
            enc.encode(VideoSurface::Cpu(cpu)),
            Err(PlatformError::Unsupported { .. })
        ));
        enc.finish(&mut out).expect("finish");
        assert!(
            out.len() >= sent - 2,
            "{} packets for {sent} frames",
            out.len()
        );
        assert!(out[0].is_keyframe(), "the first packet is a keyframe");
        assert_eq!(out[0].pts(), MediaTime::new(0, tb));
        for w in out.windows(2) {
            assert!(w[0].pts().ticks() < w[1].pts().ticks(), "pts increase");
        }
        assert_framing(codec, &out);
        // Release the encoder's textures before the context's device.
        drop(enc);
        drop(codecs);
        drop(ring);
        drop(context);
    }
}
