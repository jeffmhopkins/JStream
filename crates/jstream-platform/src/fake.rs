//! Deterministic in-memory backends, for engine and scenario tests (feature `fake`).
//!
//! Nothing here touches a device, a GPU or the OS clock (R-TEST-004), and everything is
//! driven by the test: time moves only when [`FakeClock::advance`] is called, capture
//! delivers a frame only when time has moved, and audio packets arrive only when
//! [`FakeAudioIo::deliver`] is called. The fake codecs produce packets with the right
//! timing and keyframe pattern, but their payloads are placeholders, not decodable
//! bitstreams.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jstream_media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaTime, Packet,
    PixelFormat, Rounding, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};

use crate::{
    AudioDeviceInfo, AudioDirection, AudioEncoder, AudioEncoderConfig, AudioInputHandler,
    AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget, AudioPacket,
    AudioPacketFlags, AudioRenderBuffer, AudioStream, Backends, Capture, CaptureDecoder,
    CaptureEncoding, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, CodecInfo,
    CodecRole, Codecs, CompressedFrame, DeviceId, GpuContext, GpuFrame, GpuInterop,
    HardwarePreference, MediaClock, PlatformError, Secret, StreamKeyStore, VcamFormat, VcamSink,
    VideoDecoder, VideoDecoderConfig, VideoDeviceFormat, VideoDeviceInfo, VideoDevices,
    VideoEncoder, VideoEncoderConfig, VideoSurface,
};

/// A full set of fake backends on `clock`: a 320x180 display, a stereo 48 kHz
/// microphone and speakers, the fake codecs, no GPU interop ([`FakeGpuInterop`]) and a
/// fake virtual camera.
///
/// # Errors
/// None in practice; the `Result` carries the media-type constructors' errors.
pub fn backends(clock: &FakeClock) -> Result<Backends, PlatformError> {
    let size = FrameSize::new(320, 180)?;
    Ok(Backends {
        clock: Arc::new(clock.clone()),
        capture: Arc::new(FakeCapture::new(clock.clone(), size)),
        audio: Arc::new(FakeAudioIo::new(
            clock.clone(),
            48_000,
            ChannelLayout::Stereo,
        )?),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        vcam: Some(Box::new(FakeVcamSink::new())),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic in another test thread must not cascade; the fake's state stays usable.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------
// GPU

/// GPU interop that has none: CI machines have no GPU (R-TEST-004), so the engine under
/// test takes its fallback, its own device and the CPU frame paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeGpuInterop;

impl GpuInterop for FakeGpuInterop {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        Err(PlatformError::Unsupported {
            what: "GPU interop (the fake backends have none)".into(),
        })
    }

    fn encode_target(
        &self,
        _context: &GpuContext,
        _size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        Err(PlatformError::InvalidState {
            what: "encode_target",
            state: "the fake backends open no GPU contexts",
        })
    }
}

// ---------------------------------------------------------------------------------------
// Clock

/// A media clock that moves only when told to. Clones share one time line.
#[derive(Clone, Debug)]
pub struct FakeClock {
    timebase: Timebase,
    ticks: Arc<AtomicI64>,
}

impl FakeClock {
    /// A clock at zero, counting in `timebase`.
    pub fn new(timebase: Timebase) -> Self {
        Self {
            timebase,
            ticks: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Moves time forward by `ticks` (saturating at the largest value).
    pub fn advance(&self, ticks: u64) {
        let by = i64::try_from(ticks).unwrap_or(i64::MAX);
        // The closure always returns `Some`, so the update cannot fail.
        let _ = self
            .ticks
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
                Some(t.saturating_add(by))
            });
    }
}

impl MediaClock for FakeClock {
    fn timebase(&self) -> Timebase {
        self.timebase
    }

    fn now(&self) -> MediaTime {
        MediaTime::new(self.ticks.load(Ordering::SeqCst), self.timebase)
    }
}

// ---------------------------------------------------------------------------------------
// Capture

/// The ID of the one display [`FakeCapture`] offers.
pub const FAKE_DISPLAY: &str = "fake-display-0";

/// Video capture of one fake display. A started stream delivers an all-zero BGRA8 frame,
/// stamped with the clock, whenever the clock has moved since its last frame.
#[derive(Clone, Debug)]
pub struct FakeCapture {
    clock: FakeClock,
    size: FrameSize,
}

impl FakeCapture {
    /// One display ([`FAKE_DISPLAY`]) of `size`, stamped by `clock`.
    pub fn new(clock: FakeClock, size: FrameSize) -> Self {
        Self { clock, size }
    }
}

impl Capture for FakeCapture {
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        Ok(vec![CaptureTargetInfo {
            target: CaptureTarget::Display(DeviceId::new(FAKE_DISPLAY)),
            name: "Fake display".into(),
            size: Some(self.size),
            primary: true,
        }])
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        match target {
            CaptureTarget::Display(id) if id.as_str() == FAKE_DISPLAY => {
                Ok(Box::new(FakeCaptureStream {
                    clock: self.clock.clone(),
                    size: self.size,
                    last: None,
                    stats: CaptureStats::default(),
                }))
            }
            CaptureTarget::Display(id) => Err(PlatformError::NotFound {
                kind: "display",
                id: id.to_string(),
            }),
            CaptureTarget::Window(id) => Err(PlatformError::NotFound {
                kind: "window",
                id: id.to_string(),
            }),
        }
    }
}

struct FakeCaptureStream {
    clock: FakeClock,
    size: FrameSize,
    last: Option<MediaTime>,
    stats: CaptureStats,
}

impl CaptureStream for FakeCaptureStream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        let now = self.clock.now();
        if self.last == Some(now) {
            return Ok(None);
        }
        let frame = VideoFrame::zeroed(VideoFrameInfo {
            size: self.size,
            format: PixelFormat::Bgra8,
            color: ColorInfo::default(),
            pts: now,
        })?;
        self.last = Some(now);
        self.stats.frames_captured += 1;
        Ok(Some(frame.into()))
    }

    fn stats(&self) -> CaptureStats {
        self.stats
    }
}

// ---------------------------------------------------------------------------------------
// Video capture devices

/// The ID of [`FakeVideoDevices`]'s webcam, paired with [`FAKE_MIC`].
pub const FAKE_WEBCAM: &str = "fake-webcam-0";
/// The ID of [`FakeVideoDevices`]'s capture card, which has no paired audio.
pub const FAKE_CAPTURE_CARD: &str = "fake-capture-card-0";

/// Two video capture devices: a webcam ([`FAKE_WEBCAM`], paired with [`FAKE_MIC`]) that
/// offers 640x360 NV12 and YUY2 and 1280x720 MJPEG at 30 fps, and a capture card
/// ([`FAKE_CAPTURE_CARD`], no audio) that offers 1280x720 NV12 at 60 fps.
///
/// A stream delivers a frame, stamped with the clock, whenever the clock has moved since
/// its last one, as [`FakeCapture`]'s does. Raw and YUY2 frames are all-zero NV12. An
/// MJPEG frame is a placeholder (a JPEG start marker, the frame number, an end marker)
/// handed to the stream's [`CaptureDecoder`]; a frame the decoder rejects is dropped.
/// Clones share the devices, so a test keeps a clone to [`unplug`](Self::unplug) one.
#[derive(Clone)]
pub struct FakeVideoDevices {
    clock: FakeClock,
    state: Arc<Mutex<VideoDeviceState>>,
}

#[derive(Default)]
struct VideoDeviceState {
    next_id: u64,
    /// Open streams, by stream ID, with the device each one captures.
    streams: BTreeMap<u64, DeviceId>,
    unplugged: Vec<DeviceId>,
}

impl FakeVideoDevices {
    /// Both devices, stamped by `clock`.
    pub fn new(clock: FakeClock) -> Self {
        Self {
            clock,
            state: Arc::default(),
        }
    }

    /// Removes the device `id`: it is no longer listed or opened, and its open streams
    /// report [`PlatformError::DeviceLost`].
    pub fn unplug(&self, id: &DeviceId) {
        let mut state = lock(&self.state);
        if !state.unplugged.contains(id) {
            state.unplugged.push(id.clone());
        }
    }

    /// How many streams are open.
    pub fn open_streams(&self) -> usize {
        lock(&self.state).streams.len()
    }

    fn all() -> Vec<VideoDeviceInfo> {
        vec![
            VideoDeviceInfo {
                id: DeviceId::new(FAKE_WEBCAM),
                name: "Fake webcam".into(),
                audio: Some(DeviceId::new(FAKE_MIC)),
            },
            VideoDeviceInfo {
                id: DeviceId::new(FAKE_CAPTURE_CARD),
                name: "Fake capture card".into(),
                audio: None,
            },
        ]
    }

    fn present(&self, id: &DeviceId) -> Result<(), PlatformError> {
        let known = Self::all().iter().any(|d| &d.id == id);
        if !known || lock(&self.state).unplugged.contains(id) {
            return Err(PlatformError::NotFound {
                kind: "video capture device",
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

impl VideoDevices for FakeVideoDevices {
    fn devices(&self) -> Result<Vec<VideoDeviceInfo>, PlatformError> {
        let state = lock(&self.state);
        Ok(Self::all()
            .into_iter()
            .filter(|d| !state.unplugged.contains(&d.id))
            .collect())
    }

    fn formats(&self, device: &DeviceId) -> Result<Vec<VideoDeviceFormat>, PlatformError> {
        self.present(device)?;
        let format = |w, h, fps, encoding| -> Result<VideoDeviceFormat, PlatformError> {
            Ok(VideoDeviceFormat {
                size: FrameSize::new(w, h)?,
                frame_period: Timebase::new(1, fps)?,
                encoding,
            })
        };
        let nv12 = CaptureEncoding::Raw(PixelFormat::Nv12);
        if device.as_str() == FAKE_WEBCAM {
            Ok(vec![
                format(640, 360, 30, nv12)?,
                format(640, 360, 30, CaptureEncoding::Yuy2)?,
                format(1280, 720, 30, CaptureEncoding::Mjpeg)?,
            ])
        } else {
            Ok(vec![format(1280, 720, 60, nv12)?])
        }
    }

    fn open(
        &self,
        device: &DeviceId,
        format: &VideoDeviceFormat,
        decoder: Option<Box<dyn CaptureDecoder>>,
    ) -> Result<Box<dyn CaptureStream>, PlatformError> {
        if !self.formats(device)?.contains(format) {
            return Err(PlatformError::Unsupported {
                what: format!("{format:?} on {device}"),
            });
        }
        let decoder = match (format.encoding.is_compressed(), decoder) {
            (true, None) => {
                return Err(PlatformError::Unsupported {
                    what: format!("{:?} without a decoder", format.encoding),
                });
            }
            (true, decoder) => decoder,
            (false, _) => None,
        };
        let mut state = lock(&self.state);
        let id = state.next_id;
        state.next_id += 1;
        state.streams.insert(id, device.clone());
        Ok(Box::new(FakeVideoDeviceStream {
            id,
            device: device.clone(),
            clock: self.clock.clone(),
            format: *format,
            decoder,
            state: Arc::clone(&self.state),
            last: None,
            stats: CaptureStats::default(),
        }))
    }
}

struct FakeVideoDeviceStream {
    id: u64,
    device: DeviceId,
    clock: FakeClock,
    format: VideoDeviceFormat,
    decoder: Option<Box<dyn CaptureDecoder>>,
    state: Arc<Mutex<VideoDeviceState>>,
    last: Option<MediaTime>,
    stats: CaptureStats,
}

impl CaptureStream for FakeVideoDeviceStream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if lock(&self.state).unplugged.contains(&self.device) {
            return Err(PlatformError::DeviceLost {
                what: self.device.to_string(),
                reason: "unplugged".into(),
            });
        }
        let now = self.clock.now();
        if self.last == Some(now) {
            return Ok(None);
        }
        self.last = Some(now);
        self.stats.frames_captured += 1;
        let Some(decoder) = self.decoder.as_mut() else {
            let frame = VideoFrame::zeroed(VideoFrameInfo {
                size: self.format.size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: now,
            })?;
            return Ok(Some(frame.into()));
        };
        // A JPEG start-of-image marker, the frame number, an end-of-image marker.
        let mut data = vec![0xFF, 0xD8];
        data.extend_from_slice(&self.stats.frames_captured.to_be_bytes());
        data.extend_from_slice(&[0xFF, 0xD9]);
        let frame = CompressedFrame {
            encoding: self.format.encoding,
            data: &data,
            size: self.format.size,
            pts: now,
        };
        // As the trait says, a frame the decoder rejects is dropped and the stream goes on.
        Ok(decoder.decode(&frame).ok())
    }

    fn stats(&self) -> CaptureStats {
        self.stats
    }
}

impl Drop for FakeVideoDeviceStream {
    fn drop(&mut self) {
        lock(&self.state).streams.remove(&self.id);
    }
}

/// A [`CaptureDecoder`] that turns every compressed frame into an all-zero frame of the
/// frame's size, in one pixel format, keeping its `pts`. It never looks at the bytes.
#[derive(Clone, Copy, Debug)]
pub struct FakeCaptureDecoder {
    format: PixelFormat,
}

impl FakeCaptureDecoder {
    /// A decoder that outputs `format`.
    pub fn new(format: PixelFormat) -> Self {
        Self { format }
    }
}

impl CaptureDecoder for FakeCaptureDecoder {
    fn decode(&mut self, frame: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError> {
        let frame = VideoFrame::zeroed(VideoFrameInfo {
            size: frame.size,
            format: self.format,
            color: ColorInfo::default(),
            pts: frame.pts,
        })?;
        Ok(frame.into())
    }
}

// ---------------------------------------------------------------------------------------
// Audio

/// The ID of [`FakeAudioIo`]'s microphone.
pub const FAKE_MIC: &str = "fake-mic";
/// The ID of [`FakeAudioIo`]'s speakers.
pub const FAKE_SPEAKERS: &str = "fake-speakers";

/// Audio I/O with one microphone ([`FAKE_MIC`]) and one output ([`FAKE_SPEAKERS`]), both
/// in one format. Input streams receive packets only when [`deliver`](Self::deliver) is
/// called, and output streams are asked for samples only when [`render`](Self::render)
/// is. Clones share the open streams, so a test keeps a clone after handing one to the
/// engine. Process loopback is unsupported.
#[derive(Clone)]
pub struct FakeAudioIo {
    clock: FakeClock,
    format: AudioFormat,
    state: Arc<Mutex<AudioState>>,
}

#[derive(Default)]
struct AudioState {
    next_id: u64,
    inputs: BTreeMap<u64, (Box<dyn AudioInputHandler>, u64)>,
    outputs: BTreeMap<u64, (Box<dyn AudioOutputHandler>, u64)>,
}

impl FakeAudioIo {
    /// Devices at `sample_rate` Hz with `layout`, timestamped by `clock`.
    ///
    /// # Errors
    /// [`PlatformError::Media`] when `sample_rate` is zero.
    pub fn new(
        clock: FakeClock,
        sample_rate: u32,
        layout: ChannelLayout,
    ) -> Result<Self, PlatformError> {
        Ok(Self {
            clock,
            format: AudioFormat::new(sample_rate, layout, SampleFormat::F32Interleaved)?,
            state: Arc::default(),
        })
    }

    /// Gives every open input stream one packet of `frames` frames, every sample equal
    /// to `value`, stamped with the clock's current time. Device positions advance per
    /// stream.
    pub fn deliver(&self, frames: usize, value: f32) {
        let samples = vec![value; frames.saturating_mul(self.format.channels())];
        let time = Some(self.clock.now());
        let mut state = lock(&self.state);
        for (handler, position) in state.inputs.values_mut() {
            handler.on_packet(&AudioPacket {
                samples: &samples,
                device_position: *position,
                time,
                flags: AudioPacketFlags::default(),
            });
            *position = position.saturating_add(frames as u64);
        }
    }

    /// Asks every open output stream for `frames` frames and returns what each wrote,
    /// in the order the streams were opened.
    pub fn render(&self, frames: usize) -> Vec<Vec<f32>> {
        let time = Some(self.clock.now());
        let mut state = lock(&self.state);
        let mut rendered = Vec::with_capacity(state.outputs.len());
        for (handler, position) in state.outputs.values_mut() {
            let mut samples = vec![0.0; frames.saturating_mul(self.format.channels())];
            handler.render(&mut AudioRenderBuffer {
                samples: &mut samples,
                device_position: *position,
                time,
            });
            *position = position.saturating_add(frames as u64);
            rendered.push(samples);
        }
        rendered
    }

    /// Calls `on_error` with [`PlatformError::DeviceLost`] on every open stream and
    /// closes them, as if the devices were unplugged.
    pub fn unplug(&self) {
        let lost = PlatformError::DeviceLost {
            what: "fake audio device".into(),
            reason: "unplugged".into(),
        };
        let mut state = lock(&self.state);
        for (mut handler, _) in std::mem::take(&mut state.inputs).into_values() {
            handler.on_error(&lost);
        }
        for (mut handler, _) in std::mem::take(&mut state.outputs).into_values() {
            handler.on_error(&lost);
        }
    }

    /// How many input and output streams are open.
    pub fn open_streams(&self) -> usize {
        let state = lock(&self.state);
        state.inputs.len() + state.outputs.len()
    }

    fn stream(&self, id: u64) -> Box<dyn AudioStream> {
        Box::new(FakeAudioStream {
            id,
            format: self.format,
            state: Arc::clone(&self.state),
        })
    }
}

impl AudioIo for FakeAudioIo {
    fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
        Ok(vec![
            AudioDeviceInfo {
                id: DeviceId::new(FAKE_MIC),
                name: "Fake microphone".into(),
                direction: AudioDirection::Input,
                is_default: true,
            },
            AudioDeviceInfo {
                id: DeviceId::new(FAKE_SPEAKERS),
                name: "Fake speakers".into(),
                direction: AudioDirection::Output,
                is_default: true,
            },
        ])
    }

    fn open_input(
        &self,
        target: &AudioInputTarget,
        handler: Box<dyn AudioInputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError> {
        match target {
            AudioInputTarget::DefaultInput | AudioInputTarget::DefaultOutputLoopback => {}
            AudioInputTarget::Input(id) if id.as_str() == FAKE_MIC => {}
            AudioInputTarget::OutputLoopback(id) if id.as_str() == FAKE_SPEAKERS => {}
            AudioInputTarget::Input(id) | AudioInputTarget::OutputLoopback(id) => {
                return Err(PlatformError::NotFound {
                    kind: "audio device",
                    id: id.to_string(),
                });
            }
            AudioInputTarget::ProcessLoopback { .. } => {
                return Err(PlatformError::Unsupported {
                    what: "process loopback".into(),
                });
            }
        }
        let mut state = lock(&self.state);
        let id = state.next_id;
        state.next_id += 1;
        state.inputs.insert(id, (handler, 0));
        Ok(self.stream(id))
    }

    fn open_output(
        &self,
        target: &AudioOutputTarget,
        handler: Box<dyn AudioOutputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError> {
        match target {
            AudioOutputTarget::DefaultOutput => {}
            AudioOutputTarget::Output(id) if id.as_str() == FAKE_SPEAKERS => {}
            AudioOutputTarget::Output(id) => {
                return Err(PlatformError::NotFound {
                    kind: "audio device",
                    id: id.to_string(),
                });
            }
        }
        let mut state = lock(&self.state);
        let id = state.next_id;
        state.next_id += 1;
        state.outputs.insert(id, (handler, 0));
        Ok(self.stream(id))
    }
}

struct FakeAudioStream {
    id: u64,
    format: AudioFormat,
    state: Arc<Mutex<AudioState>>,
}

impl AudioStream for FakeAudioStream {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

impl Drop for FakeAudioStream {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        state.inputs.remove(&self.id);
        state.outputs.remove(&self.id);
    }
}

// ---------------------------------------------------------------------------------------
// Codecs

/// How many inputs a fake codec holds before it reports [`PlatformError::Busy`].
pub const FAKE_CODEC_QUEUE: usize = 2;
/// Sample frames per packet from the fake AAC encoder, as in AAC-LC.
pub const FAKE_AAC_FRAME: usize = 1024;
/// The size of every frame the fake decoder returns.
pub const FAKE_DECODE_SIZE: (u32, u32) = (16, 16);

/// Software-only fake codecs: an H.264 encoder, an AAC encoder and an H.264 decoder.
///
/// The video encoder turns each frame into one packet with the frame's timestamp,
/// keyframes every `keyframe_interval` frames, and no B-frames. The audio encoder emits
/// one packet per [`FAKE_AAC_FRAME`] sample frames, timed from the first buffer and
/// ignoring gaps in later timestamps. The decoder turns each packet into one all-zero frame
/// of [`FAKE_DECODE_SIZE`] and returns the frames in presentation order: it holds each
/// frame until a packet whose `dts` reaches the frame's `pts` has been decoded (no later
/// packet can then present earlier, since decode timestamps never decrease and
/// `dts <= pts`), and [`finish`](VideoDecoder::finish) releases the rest. Packets from the
/// fake encoder have `dts == pts`, so their frames are released at once; packets with
/// B-frame reordering are held only as deep as their `dts` lag needs. The video encoder
/// and decoder hold up to [`FAKE_CODEC_QUEUE`] unpolled outputs (frames still held for
/// reordering don't count); the audio encoder has no limit.
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeCodecs;

fn fake_info(codec: Codec, role: CodecRole) -> CodecInfo {
    let name = match role {
        CodecRole::VideoEncoder => "Fake H.264 encoder",
        CodecRole::AudioEncoder => "Fake AAC encoder",
        CodecRole::VideoDecoder => "Fake H.264 decoder",
    };
    CodecInfo {
        name: name.into(),
        codec,
        role,
        hardware: false,
    }
}

fn check_codec(
    codec: Codec,
    wanted: Codec,
    hardware: HardwarePreference,
) -> Result<(), PlatformError> {
    if codec != wanted {
        return Err(PlatformError::Unsupported {
            what: format!("{codec:?} in this role"),
        });
    }
    if hardware == HardwarePreference::Require {
        return Err(PlatformError::Unsupported {
            what: "hardware codec".into(),
        });
    }
    Ok(())
}

impl Codecs for FakeCodecs {
    fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
        Ok(vec![
            fake_info(Codec::H264, CodecRole::VideoEncoder),
            fake_info(Codec::Aac, CodecRole::AudioEncoder),
            fake_info(Codec::H264, CodecRole::VideoDecoder),
        ])
    }

    fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
        check_codec(config.codec, Codec::H264, config.hardware)?;
        if config.keyframe_interval == 0 {
            return Err(PlatformError::Unsupported {
                what: "a keyframe interval of 0".into(),
            });
        }
        Ok(Box::new(FakeVideoEncoder {
            info: fake_info(Codec::H264, CodecRole::VideoEncoder),
            config: *config,
            pending: VecDeque::new(),
            frames: 0,
            since_keyframe: 0,
            force_keyframe: false,
            finished: false,
        }))
    }

    fn audio_encoder(
        &self,
        config: &AudioEncoderConfig,
    ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
        check_codec(config.codec, Codec::Aac, HardwarePreference::Avoid)?;
        Ok(Box::new(FakeAudioEncoder {
            info: fake_info(Codec::Aac, CodecRole::AudioEncoder),
            config: *config,
            start: None,
            buffered: 0,
            emitted: 0,
            pending: VecDeque::new(),
            finished: false,
        }))
    }

    fn video_decoder(
        &self,
        config: &VideoDecoderConfig,
    ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
        check_codec(config.codec, Codec::H264, config.hardware)?;
        let (w, h) = FAKE_DECODE_SIZE;
        Ok(Box::new(FakeVideoDecoder {
            info: fake_info(Codec::H264, CodecRole::VideoDecoder),
            config: *config,
            size: FrameSize::new(w, h)?,
            held: Vec::new(),
            pending: VecDeque::new(),
            finished: false,
        }))
    }
}

const FINISHED: PlatformError = PlatformError::InvalidState {
    what: "codec input",
    state: "finished",
};

struct FakeVideoEncoder {
    info: CodecInfo,
    config: VideoEncoderConfig,
    pending: VecDeque<Packet>,
    frames: u64,
    /// Frames encoded since the last keyframe; the cadence restarts at a forced one.
    since_keyframe: u64,
    /// [`request_keyframe`](VideoEncoder::request_keyframe) was called since the last
    /// frame was encoded.
    force_keyframe: bool,
    finished: bool,
}

impl VideoEncoder for FakeVideoEncoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished && self.pending.len() < FAKE_CODEC_QUEUE
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        if self.finished {
            return Err(FINISHED);
        }
        if !self.can_accept() {
            return Err(PlatformError::Busy {
                what: "video encoder",
            });
        }
        let info = frame.info();
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
        let keyframe = self.force_keyframe
            || self
                .since_keyframe
                .is_multiple_of(u64::from(self.config.keyframe_interval));
        if keyframe {
            self.since_keyframe = 0;
        }
        // An Annex B start code and an IDR or non-IDR NAL header, then the frame number.
        let mut data = vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }];
        data.extend_from_slice(&self.frames.to_be_bytes());
        let packet = Packet::new(self.info.codec, keyframe, info.pts, info.pts, data)?;
        self.pending.push_back(packet);
        self.frames += 1;
        self.since_keyframe += 1;
        self.force_keyframe = false;
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.extend(self.pending.drain(..));
        Ok(())
    }

    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        if self.finished {
            return Err(FINISHED);
        }
        self.force_keyframe = true;
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.finished = true;
        self.poll(out)
    }
}

struct FakeAudioEncoder {
    info: CodecInfo,
    config: AudioEncoderConfig,
    /// The first buffer's pts, in the input's sample time base.
    start: Option<MediaTime>,
    /// Sample frames received but not yet in a packet.
    buffered: usize,
    /// Sample frames already in packets.
    emitted: i64,
    pending: VecDeque<Packet>,
    finished: bool,
}

impl FakeAudioEncoder {
    fn emit(&mut self, start: MediaTime) -> Result<(), PlatformError> {
        let pts = start.checked_add(MediaTime::new(self.emitted, start.timebase()))?;
        let mut data = vec![0x21];
        data.extend_from_slice(&self.emitted.to_be_bytes());
        self.pending
            .push_back(Packet::new(self.info.codec, true, pts, pts, data)?);
        self.emitted += FAKE_AAC_FRAME as i64;
        Ok(())
    }
}

impl AudioEncoder for FakeAudioEncoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
        if self.finished {
            return Err(FINISHED);
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
        let start = match self.start {
            Some(start) => start,
            None => {
                let tb = self.config.input_format.timebase();
                let start = buffer.pts().rescale(tb, Rounding::Nearest)?;
                self.start = Some(start);
                start
            }
        };
        self.buffered = self.buffered.saturating_add(buffer.frames());
        while self.buffered >= FAKE_AAC_FRAME {
            self.emit(start)?;
            self.buffered -= FAKE_AAC_FRAME;
        }
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.extend(self.pending.drain(..));
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        if let (false, Some(start)) = (self.finished, self.start)
            && self.buffered > 0
        {
            self.emit(start)?;
            self.buffered = 0;
        }
        self.finished = true;
        self.poll(out)
    }
}

struct FakeVideoDecoder {
    info: CodecInfo,
    config: VideoDecoderConfig,
    size: FrameSize,
    /// Decoded frames waiting for reordering, in decode order.
    held: Vec<VideoSurface>,
    /// Frames ready for [`poll`](VideoDecoder::poll), in presentation order.
    pending: VecDeque<VideoSurface>,
    finished: bool,
}

impl FakeVideoDecoder {
    /// Moves every held frame presented at or before `until` (every frame when `None`) to
    /// `pending`, earliest first.
    fn release(&mut self, until: Option<MediaTime>) {
        // Stable, so frames with equal pts keep their decode order.
        self.held.sort_by_key(|f| f.info().pts);
        let n = match until {
            Some(until) => self.held.partition_point(|f| f.info().pts <= until),
            None => self.held.len(),
        };
        self.pending.extend(self.held.drain(..n));
    }
}

impl VideoDecoder for FakeVideoDecoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished && self.pending.len() < FAKE_CODEC_QUEUE
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), PlatformError> {
        if self.finished {
            return Err(FINISHED);
        }
        if packet.codec() != self.info.codec {
            return Err(PlatformError::Unsupported {
                what: format!("a {:?} packet in an H.264 decoder", packet.codec()),
            });
        }
        if !self.can_accept() {
            return Err(PlatformError::Busy {
                what: "video decoder",
            });
        }
        let frame = VideoFrame::zeroed(VideoFrameInfo {
            size: self.size,
            format: self.config.output_format,
            color: ColorInfo::default(),
            pts: packet.pts(),
        })?;
        self.held.push(frame.into());
        // Every later packet has `dts >= packet.dts()`, so its frame's pts is too.
        self.release(Some(packet.dts()));
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        out.extend(self.pending.drain(..));
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        self.finished = true;
        self.release(None);
        self.poll(out)
    }
}

// ---------------------------------------------------------------------------------------
// Virtual camera

/// A virtual-camera sink that records what it was given.
///
/// It is the engine's handle: dropping it stops the camera, as [`VcamSink`] requires. A
/// test keeps a [`probe`](Self::probe) to watch the sink after handing it to the engine,
/// including after the engine drops it.
#[derive(Debug, Default)]
pub struct FakeVcamSink {
    state: Arc<Mutex<VcamState>>,
}

/// A test's view of a [`FakeVcamSink`]: reads what the sink recorded, and outlives it.
/// Clones share the record.
#[derive(Clone, Debug)]
pub struct FakeVcamProbe {
    state: Arc<Mutex<VcamState>>,
}

impl FakeVcamProbe {
    /// Frames published since the sink was created.
    pub fn published(&self) -> u64 {
        lock(&self.state).published
    }

    /// The pts of the last frame published.
    pub fn last_pts(&self) -> Option<MediaTime> {
        lock(&self.state).last_pts
    }

    /// The format the sink is started with, or `None` when it is stopped or dropped.
    pub fn format(&self) -> Option<VcamFormat> {
        lock(&self.state).format
    }
}

#[derive(Debug, Default)]
struct VcamState {
    format: Option<VcamFormat>,
    published: u64,
    last_pts: Option<MediaTime>,
}

impl FakeVcamSink {
    /// A stopped sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Frames published since the sink was created.
    pub fn published(&self) -> u64 {
        lock(&self.state).published
    }

    /// The pts of the last frame published.
    pub fn last_pts(&self) -> Option<MediaTime> {
        lock(&self.state).last_pts
    }

    /// A probe on this sink's record, for the test to keep.
    pub fn probe(&self) -> FakeVcamProbe {
        FakeVcamProbe {
            state: Arc::clone(&self.state),
        }
    }
}

impl Drop for FakeVcamSink {
    fn drop(&mut self) {
        // Dropping a started sink stops it (`VcamSink`).
        lock(&self.state).format = None;
    }
}

impl VcamSink for FakeVcamSink {
    fn start(&mut self, format: VcamFormat) -> Result<(), PlatformError> {
        let mut state = lock(&self.state);
        if state.format.is_some() {
            return Err(PlatformError::InvalidState {
                what: "vcam start",
                state: "already started",
            });
        }
        state.format = Some(format);
        Ok(())
    }

    fn publish(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let mut state = lock(&self.state);
        let Some(format) = state.format else {
            return Err(PlatformError::InvalidState {
                what: "vcam publish",
                state: "not started",
            });
        };
        let info = frame.info();
        if info.size != format.size || info.format != format.pixel_format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {:?} {}x{} frame on a {:?} {}x{} camera",
                    info.format,
                    info.size.width(),
                    info.size.height(),
                    format.pixel_format,
                    format.size.width(),
                    format.size.height()
                ),
            });
        }
        state.published += 1;
        state.last_pts = Some(info.pts);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlatformError> {
        lock(&self.state).format = None;
        Ok(())
    }

    fn format(&self) -> Option<VcamFormat> {
        lock(&self.state).format
    }
}

// ---------------------------------------------------------------------------------------
// Stream keys

/// An in-memory [`StreamKeyStore`]: what the OS credential store is to the engine, for
/// tests. Clones share the same keys, so a test can keep one and check what the engine
/// stored. [`set_failing`](Self::set_failing) makes every call fail, to test the error
/// paths; the error names the call, never a key.
#[derive(Clone, Debug, Default)]
pub struct MemoryKeyStore {
    state: Arc<Mutex<KeyState>>,
}

#[derive(Debug, Default)]
struct KeyState {
    keys: BTreeMap<String, Secret>,
    failing: bool,
}

impl MemoryKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call fail (`true`) or work again (`false`).
    pub fn set_failing(&self, failing: bool) {
        lock(&self.state).failing = failing;
    }

    /// How many keys are stored.
    pub fn len(&self) -> usize {
        lock(&self.state).keys.len()
    }

    /// Whether no key is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check(state: &KeyState, call: &'static str) -> Result<(), PlatformError> {
        if state.failing {
            return Err(PlatformError::Os {
                call,
                code: None,
                message: "the fake credential store is set to fail".into(),
            });
        }
        Ok(())
    }
}

impl StreamKeyStore for MemoryKeyStore {
    fn set(&self, output_id: &str, key: &Secret) -> Result<(), PlatformError> {
        let mut state = lock(&self.state);
        Self::check(&state, "MemoryKeyStore::set")?;
        state
            .keys
            .insert(output_id.to_owned(), Secret::new(key.expose()));
        Ok(())
    }

    fn get(&self, output_id: &str) -> Result<Option<Secret>, PlatformError> {
        let state = lock(&self.state);
        Self::check(&state, "MemoryKeyStore::get")?;
        Ok(state
            .keys
            .get(output_id)
            .map(|key| Secret::new(key.expose())))
    }

    fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
        let mut state = lock(&self.state);
        Self::check(&state, "MemoryKeyStore::clear")?;
        state.keys.remove(output_id);
        Ok(())
    }
}
