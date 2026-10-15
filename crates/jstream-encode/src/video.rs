//! Video sessions: one [`VideoEncoder`] fed with frames from a video mix.

use jstream_platform::media::{MediaKind, Packet};
use jstream_platform::{Codecs, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface};

use crate::session::{Core, Counters, Driver, InputHandle};
use crate::{
    EncodeError, EncoderSettings, PushError, SessionOptions, SessionStats, StreamInfo, Subscription,
};

struct VideoDriver {
    encoder: Box<dyn VideoEncoder>,
}

impl Driver for VideoDriver {
    type Input = VideoSurface;

    fn can_accept(&self) -> bool {
        self.encoder.can_accept()
    }

    fn encode(
        &mut self,
        frame: VideoSurface,
        _after_drop: bool,
        _counters: &Counters,
    ) -> Result<(), PlatformError> {
        // A dropped frame needs no repair: the encoder numbers keyframes over the frames
        // it is given, so the stream stays decodable and frame times keep their gaps.
        self.encoder.encode(frame)
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.encoder.poll(out)
    }

    fn request_keyframe(&mut self) -> bool {
        // A refusal leaves the regular keyframes, which subscribers wait for instead.
        self.encoder.request_keyframe().is_ok()
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.encoder.finish(out)
    }
}

/// One video encoder session: a [`VideoEncoder`] on its own encode thread, fed through a
/// bounded frame queue ([`VideoInput`]), publishing packets to any number of
/// [`Subscription`]s.
///
/// `Send + Sync`, so it can be shared (`Arc`) by every output that uses the same mix and
/// settings ([`SessionRegistry`](crate::SessionRegistry)). Dropping it stops the encode
/// thread without draining the encoder (subscribers see [`StreamEnd::Closed`](crate::StreamEnd::Closed))
/// and joins the thread; call [`finish`](Self::finish) first for a clean end.
pub struct VideoSession {
    core: Core<VideoDriver>,
}

impl VideoSession {
    /// Opens a video encoder for `config` on `codecs` and starts its session.
    ///
    /// # Errors
    /// [`EncodeError::WrongKind`] for an audio codec, [`EncodeError::Open`] when the
    /// platform can't open the encoder, and [`EncodeError::Spawn`] when the encode thread
    /// can't start.
    pub fn open(
        codecs: &dyn Codecs,
        config: VideoEncoderConfig,
        options: SessionOptions,
    ) -> Result<Self, EncodeError> {
        check_kind(&config)?;
        let encoder = codecs.video_encoder(&config).map_err(EncodeError::Open)?;
        Self::start(encoder, config, options)
    }

    /// Starts a session on an encoder that is already open, with the `config` it was
    /// opened with.
    ///
    /// # Errors
    /// [`EncodeError::WrongKind`] for an audio codec, and [`EncodeError::Spawn`] when the
    /// encode thread can't start.
    pub fn start(
        encoder: Box<dyn VideoEncoder>,
        config: VideoEncoderConfig,
        options: SessionOptions,
    ) -> Result<Self, EncodeError> {
        check_kind(&config)?;
        let info = StreamInfo {
            codec: encoder.info().clone(),
            settings: EncoderSettings::Video(config),
        };
        let core = Core::start(
            "jstream-encode-video",
            VideoDriver { encoder },
            info,
            &options,
        )?;
        Ok(Self { core })
    }

    /// A handle for the render side to push frames into.
    pub fn input(&self) -> VideoInput {
        VideoInput {
            inner: self.core.input(),
        }
    }

    /// Subscribes with the session's default queue capacity
    /// ([`SessionOptions::subscriber_capacity`]). The subscription starts at the next
    /// keyframe the encoder produces. Subscribing [requests](Self::request_keyframe) one,
    /// so with an encoder that can force keyframes that is the next frame encoded.
    pub fn subscribe(&self) -> Subscription {
        self.core.subscribe(None)
    }

    /// Subscribes with a queue of `capacity` packets (at least one), requesting a
    /// keyframe as [`subscribe`](Self::subscribe) does.
    pub fn subscribe_with_capacity(&self, capacity: usize) -> Subscription {
        self.core.subscribe(Some(capacity))
    }

    /// Asks for the next frame encoded to be a keyframe, for a consumer that has to
    /// restart decoding, such as an RTMP output that just reconnected. Never blocks.
    /// Requests made before that frame ask for one keyframe. An encoder that can't force
    /// keyframes ignores it, and the next regular keyframe comes as usual.
    /// [`SessionStats::keyframes_requested`] counts the requests the encoder accepted.
    pub fn request_keyframe(&self) {
        self.core.request_keyframe();
    }

    /// The codec and settings of the stream.
    pub fn info(&self) -> &StreamInfo {
        self.core.info()
    }

    /// The session's counters now.
    pub fn stats(&self) -> SessionStats {
        self.core.stats()
    }

    /// Ends the session: refuses further input, encodes the frames still queued, drains
    /// the encoder, publishes the remaining packets, and ends every subscription with
    /// [`StreamEnd::Finished`](crate::StreamEnd::Finished). Blocks until the encode thread
    /// has exited (draining a hardware encoder can take a while). Calling it again
    /// returns the same result.
    ///
    /// # Errors
    /// The error that stopped the session, if it failed before or while finishing.
    pub fn finish(&self) -> Result<SessionStats, EncodeError> {
        self.core.finish()
    }
}

fn check_kind(config: &VideoEncoderConfig) -> Result<(), EncodeError> {
    if config.codec.kind() == MediaKind::Video {
        Ok(())
    } else {
        Err(EncodeError::WrongKind {
            codec: config.codec,
            expected: MediaKind::Video,
        })
    }
}

/// Where the render side pushes frames for one [`VideoSession`]. Cheap to clone; never
/// blocks.
#[derive(Clone)]
pub struct VideoInput {
    inner: InputHandle<VideoSurface>,
}

impl VideoInput {
    /// Queues `frame` for the encoder, or drops it when the queue is full.
    ///
    /// Frames are taken from the queue only when the encoder can accept one, so a slow
    /// encoder fills the queue and the newest frames are dropped here, at the source.
    ///
    /// # Errors
    /// [`PushError::Full`] when the frame was dropped (and counted), and
    /// [`PushError::Closed`] once the session is finishing or has ended.
    pub fn push(&self, frame: VideoSurface) -> Result<(), PushError> {
        self.inner.push(frame)
    }
}

impl std::fmt::Debug for VideoInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoInput").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for VideoSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSession")
            .field("info", self.info())
            .finish_non_exhaustive()
    }
}
