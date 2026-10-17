//! Encoders and decoders. This crate is the single owner of these traits
//! (docs/ARCHITECTURE.md#crate-map): `jstream-encode` drives encoders through them, and
//! sources drive decoders, whatever platform provides them.
//!
//! Every implementation wraps an OS or GPU-vendor codec, never a bundled C codec library
//! (docs/adr/0061-source-types-and-decoding-scope-v2.md). The call pattern is the one spike S1 drives
//! a Media Foundation encoder MFT with: check [`can_accept`](VideoEncoder::can_accept),
//! submit, then [`poll`](VideoEncoder::poll) without blocking for whatever output is
//! ready, and [`finish`](VideoEncoder::finish) to drain at the end of the stream. A
//! synchronous codec fits the same pattern. [`request_keyframe`](VideoEncoder::request_keyframe)
//! forces the next video frame to be a keyframe.

use std::sync::Arc;

use jstream_media::{AudioBuffer, AudioFormat, Codec, FrameSize, Packet, PixelFormat, Timebase};

use crate::{GpuContext, PlatformError, VideoSurface};

/// What a codec does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodecRole {
    /// Encodes video frames into packets.
    VideoEncoder,
    /// Encodes audio buffers into packets.
    AudioEncoder,
    /// Decodes video packets into frames.
    VideoDecoder,
}

/// One codec implementation the platform offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecInfo {
    /// A human-readable name, such as the encoder MFT's friendly name.
    pub name: String,
    /// The compressed format.
    pub codec: Codec,
    /// What it does.
    pub role: CodecRole,
    /// Whether it runs on dedicated hardware (a GPU encode or decode engine).
    pub hardware: bool,
}

/// Whether to use a hardware codec. ADR-0061: hardware first, with the OS software codec
/// as the fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HardwarePreference {
    /// Hardware if there is one, otherwise software.
    #[default]
    Prefer,
    /// Hardware only; fail with [`PlatformError::Unsupported`] if there is none.
    Require,
    /// Software only.
    Avoid,
}

/// How an encoder spends bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RateControl {
    /// Constant bit rate, as RTMP ingest expects.
    Cbr {
        /// The target, in kilobits per second.
        kbps: u32,
    },
    /// Variable bit rate with a peak.
    Vbr {
        /// The average target, in kilobits per second.
        kbps: u32,
        /// The peak, in kilobits per second.
        max_kbps: u32,
    },
}

/// What to open a video encoder for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoEncoderConfig {
    /// The output codec; must be a video codec.
    pub codec: Codec,
    /// The size of every input frame.
    pub size: FrameSize,
    /// The pixel format of every input frame (NV12 on the Windows path).
    pub input_format: PixelFormat,
    /// The duration of one frame, as a time base: `1/60` for 60 fps, `1001/30000` for
    /// 29.97 fps.
    pub frame_period: Timebase,
    /// Bit-rate control.
    pub rate_control: RateControl,
    /// Frames from one keyframe to the next (spike S1 uses two seconds' worth).
    pub keyframe_interval: u32,
    /// Hardware or software.
    pub hardware: HardwarePreference,
}

/// What to open an audio encoder for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioEncoderConfig {
    /// The output codec; must be an audio codec.
    pub codec: Codec,
    /// The format of every input buffer.
    pub input_format: AudioFormat,
    /// The target bit rate, in kilobits per second.
    pub kbps: u32,
}

/// What to open a video decoder for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoDecoderConfig {
    /// The input codec; must be a video codec.
    pub codec: Codec,
    /// The pixel format to decode to.
    pub output_format: PixelFormat,
    /// Hardware or software.
    pub hardware: HardwarePreference,
}

/// The codec backend: lists the platform's codecs and opens sessions on them.
///
/// Shared (`Arc<dyn Codecs>`) and called from the control thread, so it is
/// `Send + Sync`.
pub trait Codecs: Send + Sync {
    /// Every codec the platform offers.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS refuses to enumerate them.
    fn list(&self) -> Result<Vec<CodecInfo>, PlatformError>;

    /// Opens a video encoder session.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when no encoder matches the codec, size, input
    /// format or hardware preference, and [`PlatformError::Os`] when the OS refuses.
    fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError>;

    /// Opens an audio encoder session.
    ///
    /// # Errors
    /// As for [`video_encoder`](Codecs::video_encoder).
    fn audio_encoder(
        &self,
        config: &AudioEncoderConfig,
    ) -> Result<Box<dyn AudioEncoder>, PlatformError>;

    /// Opens a video decoder session.
    ///
    /// # Errors
    /// As for [`video_encoder`](Codecs::video_encoder).
    fn video_decoder(
        &self,
        config: &VideoDecoderConfig,
    ) -> Result<Box<dyn VideoDecoder>, PlatformError>;

    /// This backend on `context`'s device: its video encoders take `VideoSurface::Gpu`
    /// frames, the platform's encode targets on that context
    /// ([`GpuInterop::encode_target`](crate::GpuInterop::encode_target)), and read them
    /// without a CPU copy (README › GPU frames). `Ok(None)`, the default, when the
    /// backend has no GPU path; the engine then keeps this backend and CPU frames.
    ///
    /// The encoders it opens take GPU frames only, and drop each frame once they have
    /// read it, so the producer can write the target again when it holds the frame's
    /// only clone. Opening one fails with [`PlatformError::Unsupported`] when no encoder
    /// can read the context's textures; the engine then opens a CPU encoder instead.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] for a context another platform opened.
    fn on_context(&self, context: &GpuContext) -> Result<Option<Arc<dyn Codecs>>, PlatformError> {
        let _ = context;
        Ok(None)
    }
}

/// One video encoder session, driven by one encode thread (so `Send`, not `Sync`).
///
/// Packets keep the timing of the frames they encode: a packet's `pts` is the same
/// instant as its frame's `pts` (on the media clock), and `dts <= pts`. Video packets
/// follow the engine's framing conventions (H.264 as Annex B, per `jstream-media`).
pub trait VideoEncoder: Send {
    /// Which codec implementation was opened.
    fn info(&self) -> &CodecInfo;

    /// Whether [`encode`](VideoEncoder::encode) would take a frame now. An asynchronous
    /// encoder says no until it asks for more input.
    fn can_accept(&self) -> bool;

    /// Queues one frame. Must not block for long: the encode thread calls it every tick
    /// (R-RT-002).
    ///
    /// # Errors
    /// [`PlatformError::Busy`] when [`can_accept`](VideoEncoder::can_accept) is false
    /// (the caller drops the frame and counts it, R-RT-004),
    /// [`PlatformError::Unsupported`] when the frame's size, format or surface kind
    /// doesn't match the configuration, [`PlatformError::InvalidState`] after
    /// [`finish`](VideoEncoder::finish), and [`PlatformError::Os`] when the codec fails.
    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError>;

    /// Appends every packet that is ready to `out`, in decode order, without blocking.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails.
    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;

    /// Asks for the next frame given to [`encode`](VideoEncoder::encode) to be encoded
    /// as a keyframe, so a viewer that joins mid-stream can start decoding without
    /// waiting out the keyframe interval. Several requests before that frame ask for one
    /// keyframe. Whether the regular keyframe cadence restarts from it is up to the
    /// codec. Must not block for long: the encode thread calls it (R-RT-002).
    ///
    /// The default refuses: an encoder that can't force a keyframe still produces one
    /// every `keyframe_interval` frames.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the encoder can't force a keyframe,
    /// [`PlatformError::InvalidState`] after [`finish`](VideoEncoder::finish), and
    /// [`PlatformError::Os`] when the codec refuses the request.
    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported {
            what: format!("forcing a keyframe on {}", self.info().name),
        })
    }

    /// Ends the stream: drains the encoder and appends the remaining packets to `out`.
    /// May block until the codec has drained. Afterwards the session takes no more input.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails or doesn't drain in time.
    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;
}

/// One audio encoder session, driven by one encode thread.
///
/// Packet timing follows the input: the first packet starts at the first buffer's `pts`,
/// and each packet's `pts` is that start plus the samples before it. AAC packets are raw
/// access units, without ADTS (per `jstream-media`).
pub trait AudioEncoder: Send {
    /// Which codec implementation was opened.
    fn info(&self) -> &CodecInfo;

    /// Queues one buffer of samples. The encoder copies what it needs, and buffers any
    /// remainder that doesn't fill a whole codec frame.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the buffer's format doesn't match the
    /// configuration, [`PlatformError::InvalidState`] after
    /// [`finish`](AudioEncoder::finish), and [`PlatformError::Os`] when the codec fails.
    fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError>;

    /// Appends every packet that is ready to `out`, without blocking.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails.
    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;

    /// Ends the stream: encodes any buffered remainder (padded with silence) and appends
    /// the remaining packets to `out`. Afterwards the session takes no more input.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails.
    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError>;
}

/// One video decoder session, driven by one thread.
///
/// A decoded frame's `pts` is its packet's `pts`. Frames come out in presentation order.
pub trait VideoDecoder: Send {
    /// Which codec implementation was opened.
    fn info(&self) -> &CodecInfo;

    /// Whether [`decode`](VideoDecoder::decode) would take a packet now.
    fn can_accept(&self) -> bool;

    /// Queues one packet.
    ///
    /// # Errors
    /// [`PlatformError::Busy`] when [`can_accept`](VideoDecoder::can_accept) is false,
    /// [`PlatformError::Unsupported`] for a packet of another codec,
    /// [`PlatformError::InvalidState`] after [`finish`](VideoDecoder::finish), and
    /// [`PlatformError::Os`] when the codec fails (including on a corrupt packet).
    fn decode(&mut self, packet: &Packet) -> Result<(), PlatformError>;

    /// Appends every frame that is ready to `out`, without blocking.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails.
    fn poll(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError>;

    /// Ends the stream: drains the decoder and appends the remaining frames to `out`.
    /// Afterwards the session takes no more input.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the codec fails.
    fn finish(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError>;
}
