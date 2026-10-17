//! H.264 encoding through Media Foundation encoder MFTs, from CPU NV12, RGBA or BGRA
//! frames or from NV12 encode targets on the GPU.
//!
//! On the CPU path, frames are copied into a Media Foundation memory buffer as NV12, the
//! encoder's input. RGBA and BGRA frames (the compositor's output) are converted to NV12
//! on the way ([`Nv12Packer`]). On the GPU path (ARCH › GPU pipeline, step 5), opened
//! through `MfCodecs`' `Codecs::on_context`, the MFT runs on the context's D3D11 device
//! through a DXGI device manager, and each encode target is copied on the GPU into a
//! texture from the MFT's own sample allocator (`EncodeGpu::copy`), so no frame touches
//! the CPU. Only D3D11-aware hardware MFTs on the context's adapter take that path; the
//! engine falls back to the CPU path without one (step 6).
//!
//! Hardware encoders are tried first and the OS software encoder after them, as
//! [`HardwarePreference`](jstream_platform::HardwarePreference) says (ADR-0061).
//! Hardware MFTs are usually asynchronous: they ask for input and announce output
//! through events, which `poll` pumps.

use std::collections::VecDeque;

use jstream_platform::media::{
    Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Rounding, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{PlatformError, RateControl, VideoEncoderConfig};

use crate::capture::convert::{RgbOrder, rgb_to_nv12};

/// What the MFT is configured with, derived from a [`VideoEncoderConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// `MF_MT_FRAME_RATE`: frames per second as numerator and denominator.
    pub(crate) fps: (u32, u32),
    /// The duration of one frame in 100 ns units, rounded to the nearest.
    pub(crate) frame_hns: i64,
    /// The mean bit rate, in bits per second.
    pub(crate) bitrate: u32,
    /// The peak bit rate for VBR, in bits per second; `None` for CBR.
    pub(crate) max_bitrate: Option<u32>,
}

/// Checks a configuration and derives the MFT settings.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, an input other than NV12, RGBA or
/// BGRA, odd dimensions (NV12 halves them for chroma), a zero keyframe interval, a zero
/// bit rate or a VBR peak below its mean.
pub(crate) fn check(config: &VideoEncoderConfig) -> Result<Settings, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::H264 {
        return unsupported(format!("{:?} from the H.264 encoder", config.codec));
    }
    if !matches!(
        config.input_format,
        PixelFormat::Nv12 | PixelFormat::Rgba8 | PixelFormat::Bgra8
    ) {
        return unsupported(format!(
            "H.264 from {:?} (the encoder takes NV12, RGBA or BGRA)",
            config.input_format
        ));
    }
    let (w, h) = (config.size.width(), config.size.height());
    if w % 2 != 0 || h % 2 != 0 {
        return unsupported(format!("H.264 at an odd size, {w}x{h}"));
    }
    if config.keyframe_interval == 0 {
        return unsupported("a keyframe interval of 0".into());
    }
    let (kbps, max) = match config.rate_control {
        RateControl::Cbr { kbps } => (kbps, None),
        RateControl::Vbr { kbps, max_kbps } => (kbps, Some(max_kbps)),
        other => return unsupported(format!("rate control {other:?}")),
    };
    if kbps == 0 || max.is_some_and(|m| m < kbps) {
        return unsupported(format!("rate control {:?}", config.rate_control));
    }
    let bps = |k: u32| k.saturating_mul(1000);
    // A frame period of num/den seconds is den/num frames per second.
    let p = config.frame_period;
    let (num, den) = (u64::from(p.num()), u64::from(p.den()));
    let frame_hns = i64::try_from((num * 10_000_000 + den / 2) / den).unwrap_or(i64::MAX);
    Ok(Settings {
        fps: (p.den(), p.num()),
        frame_hns,
        bitrate: bps(kbps),
        max_bitrate: max.map(bps),
    })
}

/// Copies an NV12 frame into `out` as Media Foundation's contiguous NV12: the Y plane's
/// rows, then the interleaved UV plane's rows, each row `width` bytes with no padding.
/// `out` must hold `width * height * 3 / 2` bytes.
///
/// # Errors
/// [`PlatformError::Unsupported`] when the frame isn't NV12, and
/// [`PlatformError::InvalidState`] when `out` is the wrong size.
pub(crate) fn pack_nv12(frame: &VideoFrame, out: &mut [u8]) -> Result<(), PlatformError> {
    let info = frame.info();
    if info.format != PixelFormat::Nv12 {
        return Err(PlatformError::Unsupported {
            what: format!("packing {:?} as NV12", info.format),
        });
    }
    let w = info.size.width() as usize;
    let h = info.size.height();
    if out.len() != w * h as usize * 3 / 2 {
        return Err(PlatformError::InvalidState {
            what: "NV12 packing",
            state: "the output buffer doesn't fit the frame",
        });
    }
    let mut rows = out.chunks_exact_mut(w);
    for (plane, height) in [(0, h), (1, h / 2)] {
        for y in 0..height {
            // Both exist: NV12 has two planes, and `VideoFrame` checks their sizes.
            if let (Some(src), Some(dst)) = (frame.row(plane, y), rows.next()) {
                dst.copy_from_slice(&src[..w]);
            }
        }
    }
    Ok(())
}

/// Turns the encoder's input frames into the contiguous NV12 its MFT takes, reusing its
/// buffers. NV12 is copied ([`pack_nv12`]). RGBA (what the engine's compositor produces)
/// and BGRA are converted first, BT.709 limited range with left-sited chroma, the way
/// display capture converts WGC's BGRA.
#[derive(Debug)]
pub(crate) struct Nv12Packer {
    out: Vec<u8>,
    /// The converted frame for RGB input; made at the first one.
    converted: Option<VideoFrame>,
}

impl Nv12Packer {
    /// A packer for frames of `size`, which must be even (as [`check`] requires).
    pub(crate) fn new(size: FrameSize) -> Self {
        Self {
            out: vec![0; size.width() as usize * size.height() as usize * 3 / 2],
            converted: None,
        }
    }

    /// Packs `frame` and returns the NV12 bytes.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a format other than NV12, RGBA or BGRA, and
    /// [`PlatformError::InvalidState`] when the frame's size isn't the packer's.
    pub(crate) fn pack(&mut self, frame: &VideoFrame) -> Result<&[u8], PlatformError> {
        let info = frame.info();
        let order = match info.format {
            PixelFormat::Nv12 => {
                pack_nv12(frame, &mut self.out)?;
                return Ok(&self.out);
            }
            PixelFormat::Rgba8 => RgbOrder::Rgba,
            PixelFormat::Bgra8 => RgbOrder::Bgra,
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("packing {other:?} as NV12"),
                });
            }
        };
        let converted = match &mut self.converted {
            Some(c) if c.info().size == info.size => c,
            slot => slot.insert(
                VideoFrame::zeroed(VideoFrameInfo {
                    size: info.size,
                    format: PixelFormat::Nv12,
                    color: ColorInfo::default(),
                    pts: info.pts,
                })
                .map_err(|_| PlatformError::Unsupported {
                    what: format!("NV12 at {}x{}", info.size.width(), info.size.height()),
                })?,
            ),
        };
        // One plane: `VideoFrame` checks the plane count of a packed format.
        let Some(plane) = frame.plane(0) else {
            return Err(PlatformError::InvalidState {
                what: "NV12 packing",
                state: "the RGB frame has no plane",
            });
        };
        rgb_to_nv12(plane.data(), plane.stride(), order, converted)?;
        pack_nv12(converted, &mut self.out)?;
        Ok(&self.out)
    }
}

/// The frames in flight: the time each was sent to the encoder with (100 ns units) and
/// the `pts` it came with, so each packet gets back its frame's exact `pts` however the
/// frame's time base rounds to 100 ns.
#[derive(Debug, Default)]
pub(crate) struct InFlight {
    frames: VecDeque<(i64, MediaTime)>,
}

/// 100 ns ticks, the unit of Media Foundation sample times.
pub(crate) fn hns() -> Timebase {
    crate::clock::timebase()
}

impl InFlight {
    /// Records a frame and returns its sample time in 100 ns units.
    pub(crate) fn send(&mut self, pts: MediaTime) -> Result<i64, PlatformError> {
        let t = pts.rescale(hns(), Rounding::Nearest)?.ticks();
        self.frames.push_back((t, pts));
        Ok(t)
    }

    /// The `pts` of the frame an output sample with this time encodes. Frames before it
    /// that the encoder dropped are forgotten. A time the map doesn't know (or none) is
    /// returned as is, in 100 ns units.
    pub(crate) fn receive(&mut self, time: Option<i64>) -> MediaTime {
        let Some(t) = time else {
            return match self.frames.pop_front() {
                Some((_, pts)) => pts,
                None => MediaTime::zero(hns()),
            };
        };
        while let Some(&(sent, pts)) = self.frames.front() {
            if sent > t {
                break;
            }
            self.frames.pop_front();
            if sent == t {
                return pts;
            }
        }
        MediaTime::new(t, hns())
    }

    /// Frames sent and not yet received.
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(windows)]
pub(crate) use encoder::MfH264Encoder;

#[cfg(windows)]
mod encoder {
    use std::time::{Duration, Instant};

    use jstream_platform::media::{Codec, Packet};
    use jstream_platform::{
        CodecInfo, CodecRole, GpuFrame, HardwarePreference, PlatformError, VideoEncoder,
        VideoEncoderConfig, VideoSurface,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, ID3D11Texture2D,
    };
    use windows::Win32::Media::MediaFoundation::{
        CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
        CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
        CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
        ICodecAPI, IMF2DBuffer, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager,
        IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, IMFVideoSampleAllocatorEx,
        METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
        MF_E_NO_EVENTS_AVAILABLE, MF_E_NOTACCEPTING, MF_E_SAMPLEALLOCATOR_EMPTY,
        MF_EVENT_FLAG_NO_WAIT, MF_EVENT_TYPE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
        MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
        MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX,
        MF_SA_D3D11_AWARE, MF_SA_D3D11_BINDFLAGS, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
        MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateMediaType,
        MFCreateVideoSampleAllocatorEx, MFMediaType_Video, MFNominalRange_16_235,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_LOCALMFT,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFVideoFormat_H264,
        MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709,
        eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
        eAVEncH264VProfile_High,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{GUID, Interface};

    use super::{InFlight, Nv12Packer, Settings, check};
    use crate::codec::mf::{self, Found, MfRuntime, Mft, Output, PullMode, RetypeArgs};
    use crate::com::{OsResult, os_error};
    use crate::gpu::EncodeGpu;

    /// How long `finish` waits for an asynchronous encoder to drain.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
    /// Input textures on the GPU path: as many frames as the session queues ahead of
    /// the encoder. When the MFT holds them all, a frame is refused as busy (and the
    /// session drops and counts it).
    const GPU_SAMPLES: u32 = 8;

    /// What the encoder's frames are copied into.
    enum Input {
        /// Packs (and converts, for RGB input) each CPU frame for the MFT.
        Cpu(Nv12Packer),
        /// The MFT's input textures, for encode targets.
        Gpu(GpuInput),
    }

    /// The GPU path's input: textures from a sample allocator on the context's D3D11
    /// device, which encode targets are copied into.
    struct GpuInput {
        // COM objects drop before `gpu`, which keeps the wgpu device (and its driver)
        // loaded until they are released.
        allocator: IMFVideoSampleAllocatorEx,
        _manager: IMFDXGIDeviceManager,
        gpu: EncodeGpu,
    }

    impl GpuInput {
        /// A sample holding a copy of `frame`, an encode target, queued on the GPU.
        ///
        /// # Errors
        /// [`PlatformError::Busy`] when the MFT holds every input texture,
        /// [`PlatformError::Unsupported`] for a frame that is not an encode target of the
        /// encoder's context, and [`PlatformError::Os`] when MF or the driver fails.
        fn sample(&self, frame: &GpuFrame) -> Result<IMFSample, PlatformError> {
            // SAFETY: plain COM call on an initialised allocator.
            let sample = match unsafe { self.allocator.AllocateSample() } {
                Ok(sample) => sample,
                Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {
                    return Err(PlatformError::Busy {
                        what: "H.264 encoder input textures",
                    });
                }
                Err(e) => return Err(os_error("IMFVideoSampleAllocator::AllocateSample", &e)),
            };
            // SAFETY: plain COM calls on the allocator's sample, whose one buffer is a
            // DXGI surface buffer on the context's D3D11 device. `GetResource` fills
            // `raw` with an owned reference, which `from_raw` takes over.
            let (buffer, texture, subresource) = unsafe {
                let buffer = sample
                    .GetBufferByIndex(0)
                    .os("IMFSample::GetBufferByIndex")?;
                let dxgi: IMFDXGIBuffer = buffer.cast().os("IMFDXGIBuffer")?;
                let mut raw = std::ptr::null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                    .os("IMFDXGIBuffer::GetResource")?;
                let texture = ID3D11Texture2D::from_raw(raw);
                let subresource = dxgi
                    .GetSubresourceIndex()
                    .os("IMFDXGIBuffer::GetSubresourceIndex")?;
                (buffer, texture, subresource)
            };
            self.gpu.copy(frame, &texture, subresource)?;
            // Some encoders check the buffer's length. Best effort: one without a 2D
            // view keeps the length the allocator gave it.
            if let Ok(view) = buffer.cast::<IMF2DBuffer>() {
                // SAFETY: plain COM calls on the live buffer.
                unsafe {
                    if let Ok(len) = view.GetContiguousLength() {
                        let _ = buffer.SetCurrentLength(len);
                    }
                }
            }
            Ok(sample)
        }
    }

    /// The DXGI device manager on the context's D3D11 device.
    fn device_manager(gpu: &EncodeGpu) -> Result<IMFDXGIDeviceManager, PlatformError> {
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: valid out-pointers to locals.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
            .os("MFCreateDXGIDeviceManager")?;
        let manager = manager.ok_or(PlatformError::Os {
            call: "MFCreateDXGIDeviceManager",
            code: None,
            message: "returned no manager".into(),
        })?;
        // SAFETY: plain COM call; the device is live, multithread-protected and has
        // video support, as the manager requires.
        unsafe { manager.ResetDevice(gpu.device(), token) }
            .os("IMFDXGIDeviceManager::ResetDevice")?;
        Ok(manager)
    }

    /// A sample allocator for the MFT's current NV12 input type, on `manager`'s device.
    /// Its textures have the encode targets' bind flags, which the adapter is known to
    /// allocate in NV12.
    fn allocator(
        transform: &IMFTransform,
        manager: &IMFDXGIDeviceManager,
    ) -> Result<IMFVideoSampleAllocatorEx, PlatformError> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: valid out-pointer; on success it holds an owned reference of the
        // requested interface, which `from_raw` takes over. Then plain COM calls on live
        // objects.
        unsafe {
            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)
                .os("MFCreateVideoSampleAllocatorEx")?;
            let allocator = IMFVideoSampleAllocatorEx::from_raw(raw);
            allocator
                .SetDirectXManager(manager)
                .os("IMFVideoSampleAllocator::SetDirectXManager")?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1).os("MFCreateAttributes")?;
            let attributes = attributes.ok_or(PlatformError::Os {
                call: "MFCreateAttributes",
                code: None,
                message: "returned no attribute store".into(),
            })?;
            attributes
                .SetUINT32(
                    &MF_SA_D3D11_BINDFLAGS,
                    (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                )
                .os("IMFAttributes::SetUINT32 (MF_SA_D3D11_BINDFLAGS)")?;
            let input = transform
                .GetInputCurrentType(0)
                .os("IMFTransform::GetInputCurrentType")?;
            allocator
                .InitializeSampleAllocatorEx(2, GPU_SAMPLES, &attributes, &input)
                .os("IMFVideoSampleAllocatorEx::InitializeSampleAllocatorEx")?;
            Ok(allocator)
        }
    }

    /// One H.264 encoder session on a Media Foundation encoder MFT.
    pub(crate) struct MfH264Encoder {
        // Fields drop in order: COM objects before the runtime that keeps MF started.
        mft: Mft,
        /// The event queue of an asynchronous MFT; `None` for a synchronous one.
        events: Option<IMFMediaEventGenerator>,
        output: Output,
        info: CodecInfo,
        config: VideoEncoderConfig,
        settings: Settings,
        /// Inputs an asynchronous MFT has asked for and not yet received.
        need_input: u32,
        in_flight: InFlight,
        input: Input,
        pending: Vec<Packet>,
        drained: bool,
        finished: bool,
        _mf: MfRuntime,
    }

    // SAFETY: the session is owned and driven by one thread at a time (`&mut self` for
    // every call except `can_accept`, which reads a plain field). Media Foundation
    // encoder MFTs are in-proc, free-threaded objects, as are the GPU path's DXGI device
    // manager and sample allocator, and `MfRuntime` keeps the process MTA alive, so a
    // thread that never initialised COM may call them as an implicit MTA member. The
    // GPU path's `EncodeGpu` is `Send + Sync`; its D3D11 device is multithread-protected.
    unsafe impl Send for MfH264Encoder {}

    impl MfH264Encoder {
        /// Opens an H.264 encoder that takes CPU frames (NV12, or RGBA or BGRA converted
        /// by [`Nv12Packer`]): a hardware MFT first (unless the preference avoids
        /// hardware), then the software one (unless it requires hardware). The first
        /// that accepts the configuration wins.
        pub(crate) fn open(config: &VideoEncoderConfig) -> Result<Self, PlatformError> {
            Self::open_with(config, None)
        }

        /// Opens an H.264 encoder that takes encode targets on `gpu`'s context: the
        /// first D3D11-aware hardware MFT on its adapter that accepts the configuration
        /// and the context's device.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] when the configuration avoids hardware or no
        /// such MFT accepts it, and [`PlatformError::Os`] when MF fails.
        pub(crate) fn open_on(
            config: &VideoEncoderConfig,
            gpu: &EncodeGpu,
        ) -> Result<Self, PlatformError> {
            if config.hardware == HardwarePreference::Avoid {
                return Err(PlatformError::Unsupported {
                    what: "H.264 from GPU frames with hardware avoided (only hardware \
                           encoders read D3D11 textures)"
                        .into(),
                });
            }
            Self::open_with(config, Some(gpu))
        }

        fn open_with(
            config: &VideoEncoderConfig,
            gpu: Option<&EncodeGpu>,
        ) -> Result<Self, PlatformError> {
            let settings = check(config)?;
            let rt = MfRuntime::start()?;
            let manager = gpu.map(device_manager).transpose()?;
            let io = (
                (MFMediaType_Video, MFVideoFormat_NV12),
                (MFMediaType_Video, MFVideoFormat_H264),
            );
            let mut candidates = Vec::new();
            if let Some(gpu) = gpu {
                let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
                let on =
                    mf::enumerate_on(MFT_CATEGORY_VIDEO_ENCODER, flags, io.0, io.1, gpu.luid())?;
                candidates.extend(on.into_iter().map(|found| (found, true)));
            } else if config.hardware != HardwarePreference::Avoid {
                let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
                for found in mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, io.0, io.1)? {
                    candidates.push((found, true));
                }
            }
            if gpu.is_none() && config.hardware != HardwarePreference::Require {
                let flags =
                    MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER;
                for found in mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, io.0, io.1)? {
                    candidates.push((found, false));
                }
            }
            let mut refusals = Vec::new();
            for (found, hardware) in candidates {
                let configured = Self::configure(&found, config, settings, manager.as_ref())
                    .and_then(|(mft, events, output)| {
                        let input = match (gpu, &manager) {
                            (Some(gpu), Some(manager)) => Input::Gpu(GpuInput {
                                allocator: allocator(&mft.transform, manager)?,
                                _manager: manager.clone(),
                                gpu: gpu.clone(),
                            }),
                            _ => Input::Cpu(Nv12Packer::new(config.size)),
                        };
                        Ok((mft, events, output, input))
                    });
                match configured {
                    Ok((mft, events, output, input)) => {
                        return Ok(Self {
                            mft,
                            events,
                            output,
                            info: CodecInfo {
                                name: found.name,
                                codec: Codec::H264,
                                role: CodecRole::VideoEncoder,
                                hardware,
                            },
                            config: *config,
                            settings,
                            need_input: 0,
                            in_flight: InFlight::default(),
                            input,
                            pending: Vec::new(),
                            drained: false,
                            finished: false,
                            _mf: rt,
                        });
                    }
                    Err(e) => refusals.push(format!("{}: {e}", found.name)),
                }
            }
            Err(PlatformError::Unsupported {
                what: if refusals.is_empty() {
                    format!(
                        "H.264 encoding with {:?}{}: no matching encoder MFT is installed",
                        config.hardware,
                        if gpu.is_some() {
                            " from D3D11 textures on the context's adapter"
                        } else {
                            ""
                        }
                    )
                } else {
                    format!(
                        "H.264 encoding at {}x{}: every encoder refused ({})",
                        config.size.width(),
                        config.size.height(),
                        refusals.join("; ")
                    )
                },
            })
        }

        /// Activates and configures one candidate, on `manager`'s D3D11 device when
        /// there is one, and starts it streaming.
        fn configure(
            found: &Found,
            config: &VideoEncoderConfig,
            s: Settings,
            manager: Option<&IMFDXGIDeviceManager>,
        ) -> Result<(Mft, Option<IMFMediaEventGenerator>, Output), PlatformError> {
            let mft = found.activate()?;
            let transform = &mft.transform;
            // SAFETY: plain COM calls on a live transform.
            let attrs = unsafe { transform.GetAttributes() }.os("IMFTransform::GetAttributes")?;
            // SAFETY: as above.
            let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
            if is_async {
                // SAFETY: as above; an async MFT refuses every call until unlocked.
                unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                    .os("MF_TRANSFORM_ASYNC_UNLOCK")?;
            }
            // The device goes in before the media types, as MF asks.
            if let Some(manager) = manager {
                // SAFETY: plain COM call on the transform's attributes.
                if unsafe { attrs.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) == 0 {
                    return Err(PlatformError::Unsupported {
                        what: "D3D11 textures (the MFT is not D3D11-aware)".into(),
                    });
                }
                // SAFETY: plain COM call; the message's parameter is the manager's
                // interface pointer, which the MFT takes its own reference to.
                unsafe {
                    transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                }
                .os("IMFTransform::ProcessMessage (MFT_MESSAGE_SET_D3D_MANAGER)")?;
            }
            // Best effort: an encoder without ICodecAPI, or one that refuses a setting,
            // still encodes with its defaults and the media type's bit rate.
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                let mode = match s.max_bitrate {
                    None => eAVEncCommonRateControlMode_CBR.0,
                    Some(_) => eAVEncCommonRateControlMode_PeakConstrainedVBR.0,
                };
                let _ = set_u32(&api, &CODECAPI_AVEncCommonRateControlMode, mode as u32);
                let _ = set_u32(&api, &CODECAPI_AVEncCommonMeanBitRate, s.bitrate);
                if let Some(max) = s.max_bitrate {
                    let _ = set_u32(&api, &CODECAPI_AVEncCommonMaxBitRate, max);
                }
                let _ = set_u32(&api, &CODECAPI_AVEncMPVGOPSize, config.keyframe_interval);
                // No B-frames, so packets come out in presentation order and dts == pts.
                let _ = set_u32(&api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
                let _ = set_bool(&api, &CODECAPI_AVLowLatencyMode, true);
            }
            let (w, h) = (config.size.width(), config.size.height());
            // Encoders take the output type first.
            // SAFETY: plain COM calls on live objects created here.
            unsafe {
                let out = MFCreateMediaType().os("MFCreateMediaType")?;
                video_type(&out, &MFVideoFormat_H264, w, h, s.fps)?;
                out.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate)
                    .and_then(|()| {
                        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
                    })
                    .os("IMFMediaType::Set* (H.264 output)")?;
                transform
                    .SetOutputType(0, &out, 0)
                    .os("IMFTransform::SetOutputType (H.264)")?;
                let input = nv12_input_type(transform)?;
                video_type(&input, &MFVideoFormat_NV12, w, h, s.fps)?;
                // BT.709 limited range, the colour ARCH's pipeline encodes in (S1).
                input
                    .SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
                    .and_then(|()| {
                        input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)
                    })
                    .os("IMFMediaType::Set* (NV12 input)")?;
                transform
                    .SetInputType(0, &input, 0)
                    .os("IMFTransform::SetInputType (NV12)")?;
            }
            // An access unit never exceeds an uncompressed frame.
            let output = Output::new(transform, w * h * 3 / 2)?.with_retype(
                retype,
                RetypeArgs {
                    width: w,
                    height: h,
                    fps: s.fps,
                    bitrate: s.bitrate,
                    profile: eAVEncH264VProfile_High.0 as u32,
                },
            );
            let events = if is_async {
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .os("async MFT as IMFMediaEventGenerator")?,
                )
            } else {
                None
            };
            // SAFETY: plain COM calls on a configured transform.
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .and_then(|()| transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                    .os("IMFTransform::ProcessMessage (start)")?;
            }
            Ok((mft, events, output))
        }

        /// Handles every queued event of an asynchronous MFT without blocking, or pulls
        /// every ready output of a synchronous one.
        fn pump(&mut self) -> Result<(), PlatformError> {
            let Some(events) = self.events.clone() else {
                return self.collect_all();
            };
            loop {
                // SAFETY: plain COM call on a live event generator.
                let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(e) => e,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                    Err(e) => return Err(os_error("IMFMediaEventGenerator::GetEvent", &e)),
                };
                // SAFETY: plain COM call on a live event.
                let kind =
                    MF_EVENT_TYPE(unsafe { event.GetType() }.os("IMFMediaEvent::GetType")? as i32);
                if kind == METransformNeedInput {
                    self.need_input = self.need_input.saturating_add(1);
                } else if kind == METransformHaveOutput {
                    // One ProcessOutput per event: calling again without one is
                    // E_UNEXPECTED on an asynchronous MFT.
                    self.collect_one(PullMode::Once)?;
                } else if kind == METransformDrainComplete {
                    self.drained = true;
                }
            }
        }

        /// Pulls output until the MFT needs more input (synchronous MFTs).
        fn collect_all(&mut self) -> Result<(), PlatformError> {
            while self.collect_one(PullMode::Drain)? {}
            Ok(())
        }

        /// Pulls one output sample into `pending`; false when there was none.
        fn collect_one(&mut self, mode: PullMode) -> Result<bool, PlatformError> {
            let Some(sample) = self.output.pull(&self.mft.transform, mode)? else {
                return Ok(false);
            };
            let pts = self.in_flight.receive(sample.time);
            self.pending.push(Packet::new(
                Codec::H264,
                sample.keyframe,
                pts,
                pts,
                sample.data,
            )?);
            Ok(true)
        }
    }

    impl VideoEncoder for MfH264Encoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        /// For an asynchronous MFT this reflects the input requests seen by the last
        /// `encode` or `poll`, so the encode thread calls `poll` every tick.
        fn can_accept(&self) -> bool {
            !self.finished && (self.events.is_none() || self.need_input > 0)
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "H.264 encode",
                    state: "the encoder is finished",
                });
            }
            if self.events.is_some() {
                self.pump()?;
            }
            if !self.can_accept() {
                return Err(PlatformError::Busy {
                    what: "H.264 encoder",
                });
            }
            let info = *frame.info();
            if info.size != self.config.size || info.format != self.config.input_format {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "a {:?} {}x{} frame (configured for {:?} {}x{})",
                        info.format,
                        info.size.width(),
                        info.size.height(),
                        self.config.input_format,
                        self.config.size.width(),
                        self.config.size.height()
                    ),
                });
            }
            let sample = match (&mut self.input, frame) {
                (Input::Cpu(packer), VideoSurface::Cpu(frame)) => {
                    let nv12 = packer.pack(&frame)?;
                    let time = self.in_flight.send(info.pts)?;
                    mf::sample_from(nv12, time, self.settings.frame_hns)?
                }
                (Input::Gpu(gpu), VideoSurface::Gpu(frame)) => {
                    // Copied before the time is recorded: a busy encoder records nothing.
                    let sample = gpu.sample(&frame)?;
                    // The encode target goes back to its producer now; the copy is queued.
                    drop(frame);
                    let time = self.in_flight.send(info.pts)?;
                    // SAFETY: plain COM calls on the live sample.
                    unsafe {
                        sample
                            .SetSampleTime(time)
                            .and_then(|()| sample.SetSampleDuration(self.settings.frame_hns))
                    }
                    .os("IMFSample::SetSampleTime")?;
                    sample
                }
                (Input::Cpu(_), _) => {
                    return Err(PlatformError::Unsupported {
                        what: "a GPU frame on the CPU H.264 path".into(),
                    });
                }
                (Input::Gpu(_), _) => {
                    return Err(PlatformError::Unsupported {
                        what: "a CPU frame on the GPU H.264 path".into(),
                    });
                }
            };
            // SAFETY: plain COM call; the sample is a live object built above.
            match unsafe { self.mft.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING && self.events.is_none() => {
                    // A synchronous MFT only refuses input while it holds output.
                    self.collect_all()?;
                    // SAFETY: as above.
                    unsafe { self.mft.transform.ProcessInput(0, &sample, 0) }
                        .os("IMFTransform::ProcessInput (H.264)")?;
                }
                Err(e) => return Err(os_error("IMFTransform::ProcessInput (H.264)", &e)),
            }
            self.need_input = self.need_input.saturating_sub(1);
            self.pump()
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished || !self.drained {
                self.pump()?;
            }
            out.append(&mut self.pending);
            Ok(())
        }

        /// `CODECAPI_AVEncVideoForceKeyFrame`: the encoder makes the next input it
        /// processes an IDR frame. An encoder without `ICodecAPI`, or one that refuses
        /// the property, reports `Unsupported` and keeps its GOP cadence.
        fn request_keyframe(&mut self) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "H.264 keyframe request",
                    state: "the encoder is finished",
                });
            }
            let refused = |why: String| PlatformError::Unsupported {
                what: format!("forcing a keyframe on {} ({why})", self.info.name),
            };
            let api = self
                .mft
                .transform
                .cast::<ICodecAPI>()
                .map_err(|e| refused(format!("no ICodecAPI: {e}")))?;
            set_u32(&api, &CODECAPI_AVEncVideoForceKeyFrame, 1).map_err(|e| refused(e.to_string()))
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished {
                self.finished = true;
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
                if self.events.is_none() {
                    self.collect_all()?;
                    self.drained = true;
                } else {
                    let deadline = Instant::now() + DRAIN_TIMEOUT;
                    while !self.drained {
                        self.pump()?;
                        if Instant::now() > deadline {
                            out.append(&mut self.pending);
                            return Err(PlatformError::Os {
                                call: "MFT_MESSAGE_COMMAND_DRAIN",
                                code: None,
                                message: format!(
                                    "the encoder didn't drain within {} s ({} frames still in flight)",
                                    DRAIN_TIMEOUT.as_secs(),
                                    self.in_flight.len()
                                ),
                            });
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            out.append(&mut self.pending);
            Ok(())
        }
    }

    /// Re-applies the configured output attributes to the type an encoder offers after a
    /// stream change, which may not carry them.
    fn retype(t: &IMFMediaType, a: &RetypeArgs) -> windows::core::Result<()> {
        let pack = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
        // SAFETY: plain COM calls on a live media type.
        unsafe {
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(a.width, a.height))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(a.fps.0, a.fps.1))?;
            t.SetUINT32(&MF_MT_AVG_BITRATE, a.bitrate)?;
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, a.profile)
        }
    }

    /// The MFT's own NV12 input type, which carries any attributes it needs; a fresh
    /// one if it lists none.
    fn nv12_input_type(transform: &IMFTransform) -> Result<IMFMediaType, PlatformError> {
        for i in 0.. {
            // SAFETY: plain COM call; the index only selects an entry.
            let Ok(t) = (unsafe { transform.GetInputAvailableType(0, i) }) else {
                break;
            };
            // SAFETY: plain COM call on a live media type.
            if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
                return Ok(t);
            }
        }
        // SAFETY: plain constructor call.
        unsafe { MFCreateMediaType() }.os("MFCreateMediaType")
    }

    /// Sets the attributes every video media type here carries.
    ///
    /// # Safety
    /// Plain COM calls on a live media type; marked `unsafe` only because the callers'
    /// blocks already are.
    unsafe fn video_type(
        t: &IMFMediaType,
        subtype: &GUID,
        w: u32,
        h: u32,
        fps: (u32, u32),
    ) -> Result<(), PlatformError> {
        let pack = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
        // SAFETY: plain COM calls on a live media type.
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| t.SetGUID(&MF_MT_SUBTYPE, subtype))
                .and_then(|()| t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)))
                .and_then(|()| t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.0, fps.1)))
                .and_then(|()| t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)))
                .and_then(|()| {
                    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
        }
        .os("IMFMediaType::Set* (video)")
    }

    /// Sets an `ICodecAPI` value as a `VT_UI4` VARIANT.
    fn set_u32(api: &ICodecAPI, key: &GUID, value: u32) -> windows::core::Result<()> {
        // SAFETY: plain COM call; the VARIANT outlives it.
        unsafe { api.SetValue(key, &VARIANT::from(value)) }
    }

    /// Sets an `ICodecAPI` value as a `VT_BOOL` VARIANT.
    fn set_bool(api: &ICodecAPI, key: &GUID, value: bool) -> windows::core::Result<()> {
        // SAFETY: plain COM call; the VARIANT outlives it.
        unsafe { api.SetValue(key, &VARIANT::from(value)) }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame,
        VideoFrameInfo,
    };
    use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

    use super::{InFlight, Nv12Packer, check, hns, pack_nv12};

    fn config() -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::H264,
            size: FrameSize::new(1920, 1080).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1001, 30_000).expect("29.97"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 60,
            hardware: HardwarePreference::Prefer,
        }
    }

    #[test]
    fn derives_mft_settings() {
        let s = check(&config()).expect("valid");
        assert_eq!(s.fps, (30_000, 1001));
        // 333 666.67 rounds up.
        assert_eq!(s.frame_hns, 333_667);
        assert_eq!(s.bitrate, 6_000_000);
        assert_eq!(s.max_bitrate, None);
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 8000,
        };
        c.frame_period = Timebase::new(1, 60).expect("60");
        let s = check(&c).expect("valid");
        assert_eq!((s.fps, s.frame_hns), ((60, 1), 166_667));
        assert_eq!(s.max_bitrate, Some(8_000_000));
    }

    /// #424: the engine's pipeline hands the encoder the compositor's RGBA frames, and
    /// every recording and stream on Windows failed to start when this was refused.
    #[test]
    fn takes_the_compositors_rgb_output() {
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
            let mut c = config();
            c.input_format = format;
            assert_eq!(
                check(&c).expect("accepted"),
                check(&config()).expect("valid")
            );
        }
    }

    #[test]
    fn refuses_what_the_path_cant_encode() {
        let refused =
            |c: VideoEncoderConfig| matches!(check(&c), Err(PlatformError::Unsupported { .. }));
        let mut c = config();
        c.codec = Codec::Aac;
        assert!(refused(c));
        let mut c = config();
        c.input_format = PixelFormat::I420;
        assert!(refused(c));
        let mut c = config();
        c.input_format = PixelFormat::P010;
        assert!(refused(c));
        let mut c = config();
        c.size = FrameSize::new(1921, 1080).expect("size");
        assert!(refused(c));
        let mut c = config();
        c.keyframe_interval = 0;
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 3000,
        };
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Cbr { kbps: 0 };
        assert!(refused(c));
    }

    fn nv12(w: u32, h: u32, stride: usize) -> VideoFrame {
        let info = VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        };
        // Row `y` of plane `p` is filled with `p * 100 + y`, padding with 0xEE.
        let plane = |p: u8, rows: u32| {
            let mut data = vec![0xEE; stride * rows as usize];
            for y in 0..rows as usize {
                data[y * stride..y * stride + w as usize].fill(p * 100 + y as u8);
            }
            Plane::new(data, stride)
        };
        VideoFrame::new(info, vec![plane(0, h), plane(1, h / 2)]).expect("frame")
    }

    #[test]
    fn packs_nv12_without_row_padding() {
        let frame = nv12(4, 4, 8);
        let mut out = vec![0; 4 * 4 * 3 / 2];
        pack_nv12(&frame, &mut out).expect("pack");
        assert_eq!(
            out,
            [
                0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, // Y
                100, 100, 100, 100, 101, 101, 101, 101, // UV
            ]
        );
        let mut wrong = vec![0; 5];
        assert!(pack_nv12(&frame, &mut wrong).is_err());
    }

    /// A `w`x`h` packed RGB frame of `format` whose pixel at (x, y) is `px(x, y)` as
    /// `[r, g, b]`, rows padded to `stride` bytes with 0xEE.
    fn rgb(
        format: PixelFormat,
        w: u32,
        h: u32,
        stride: usize,
        px: impl Fn(u32, u32) -> [u8; 3],
    ) -> VideoFrame {
        let info = VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        };
        let mut data = vec![0xEE; stride * h as usize];
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = px(x, y);
                let o = y as usize * stride + x as usize * 4;
                let bytes = match format {
                    PixelFormat::Bgra8 => [b, g, r, 255],
                    _ => [r, g, b, 255],
                };
                data[o..o + 4].copy_from_slice(&bytes);
            }
        }
        VideoFrame::new(info, vec![Plane::new(data, stride)]).expect("frame")
    }

    fn packed(frame: &VideoFrame) -> Vec<u8> {
        Nv12Packer::new(frame.info().size)
            .pack(frame)
            .expect("pack")
            .to_vec()
    }

    /// #424: the compositor's RGBA reaches the MFT as BT.709 limited NV12.
    #[test]
    fn packs_rgba_and_bgra_as_nv12() {
        // Black, white and mid grey: luma at the range's ends and middle, neutral chroma.
        for (value, luma) in [(0, 16), (255, 235), (128, 126)] {
            let out = packed(&rgb(PixelFormat::Rgba8, 4, 2, 16, |_, _| [value; 3]));
            assert_eq!(&out[..8], &[luma; 8], "luma of {value}");
            assert_eq!(&out[8..], &[128; 4], "chroma of {value}");
        }
        // Pure red, BT.709 limited range's reference value.
        let out = packed(&rgb(PixelFormat::Rgba8, 2, 2, 8, |_, _| [255, 0, 0]));
        assert_eq!(out, [63, 63, 63, 63, 102, 240]);
        // BGRA of the same pixels, and padded rows, pack the same.
        let px = |x: u32, y: u32| [(x * 40) as u8, (y * 60) as u8, (x * y * 10) as u8];
        let tight = packed(&rgb(PixelFormat::Rgba8, 6, 4, 24, px));
        assert_eq!(packed(&rgb(PixelFormat::Rgba8, 6, 4, 40, px)), tight);
        assert_eq!(packed(&rgb(PixelFormat::Bgra8, 6, 4, 32, px)), tight);
    }

    #[test]
    fn packer_refuses_what_it_cant_pack() {
        let mut packer = Nv12Packer::new(FrameSize::new(4, 4).expect("size"));
        // Another size than the packer's.
        let frame = rgb(PixelFormat::Rgba8, 4, 2, 16, |_, _| [0; 3]);
        assert!(packer.pack(&frame).is_err());
        let i420 = VideoFrame::zeroed(VideoFrameInfo {
            size: FrameSize::new(4, 4).expect("size"),
            format: PixelFormat::I420,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        })
        .expect("frame");
        assert!(matches!(
            packer.pack(&i420),
            Err(PlatformError::Unsupported { .. })
        ));
        // NV12 still packs.
        assert_eq!(packer.pack(&nv12(4, 4, 8)).expect("pack").len(), 24);
    }

    #[test]
    fn packets_get_their_frames_exact_pts_back() {
        let mut f = InFlight::default();
        let tb = Timebase::MPEG_90K;
        // 1/90000 s doesn't divide 100 ns evenly for every value; the map keeps the
        // original.
        let a = MediaTime::new(3_003, tb);
        let b = MediaTime::new(6_006, tb);
        let c = MediaTime::new(9_009, tb);
        let ta = f.send(a).expect("send");
        let tb_ = f.send(b).expect("send");
        let tc = f.send(c).expect("send");
        assert_eq!(f.len(), 3);
        assert_eq!(f.receive(Some(ta)), a);
        // The encoder dropped `b`: receiving `c` forgets it.
        assert_eq!(f.receive(Some(tc)), c);
        assert_eq!(f.len(), 0);
        let _ = tb_;
        // An unknown time comes back as 100 ns ticks.
        let odd = f.receive(Some(42));
        assert_eq!((odd.ticks(), odd.timebase()), (42, hns()));
        // No time at all takes the oldest frame in flight.
        let d = MediaTime::new(1, tb);
        f.send(d).expect("send");
        assert_eq!(f.receive(None), d);
    }
}
