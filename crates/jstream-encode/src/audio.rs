//! Audio sessions: one [`AudioEncoder`] fed with sample blocks from an audio bus.

use std::sync::atomic::Ordering;

use jstream_platform::media::{AudioBuffer, AudioFormat, MediaKind, MediaTime, Packet, Rounding};
use jstream_platform::{AudioEncoder, AudioEncoderConfig, Codecs, PlatformError};

use crate::session::{Core, Counters, Driver, InputHandle};
use crate::{
    EncodeError, EncoderSettings, PushError, SessionOptions, SessionStats, StreamInfo, Subscription,
};

struct AudioDriver {
    encoder: Box<dyn AudioEncoder>,
    format: AudioFormat,
    /// Where the last block encoded ended, in sample frames (the format's time base).
    next: Option<i64>,
    /// The longest gap filled with silence, in sample frames.
    max_gap: i64,
    /// The encoder refused the last block, or the silence before it, as busy, so the
    /// next block follows a gap.
    gap: bool,
}

impl AudioDriver {
    /// Encodes silence over the gap between the last block and one starting at `start`,
    /// when it is positive and no longer than `max_gap`.
    fn fill_gap(&mut self, start: i64, counters: &Counters) -> Result<(), PlatformError> {
        let Some(next) = self.next else {
            return Ok(());
        };
        let missing = start.saturating_sub(next);
        if missing <= 0 || missing > self.max_gap {
            return Ok(());
        }
        let frames = usize::try_from(missing).unwrap_or(0);
        let pts = MediaTime::new(next, self.format.timebase());
        let silence = AudioBuffer::silence(self.format, pts, frames)?;
        self.encoder.encode(&silence)?;
        counters
            .silence_frames
            .fetch_add(missing.unsigned_abs(), Ordering::Relaxed);
        Ok(())
    }
}

impl Driver for AudioDriver {
    type Input = AudioBuffer;

    fn can_accept(&self) -> bool {
        // The audio trait buffers internally and has no input back-pressure.
        true
    }

    fn encode(
        &mut self,
        buffer: AudioBuffer,
        after_drop: bool,
        counters: &Counters,
    ) -> Result<(), PlatformError> {
        let start = buffer
            .pts()
            .rescale(self.format.timebase(), Rounding::Nearest)?
            .ticks();
        // Audio encoders time packets by counting samples from the first block, so a
        // dropped block would shift everything after it earlier. Fill the hole instead.
        // A refusal as busy, of the silence or of the block itself, leaves a hole too,
        // so the next block fills from `next` over both.
        let result = if after_drop || self.gap {
            self.fill_gap(start, counters)
        } else {
            Ok(())
        }
        .and_then(|()| self.encoder.encode(&buffer));
        if let Err(e) = result {
            if matches!(e, PlatformError::Busy { .. }) {
                self.gap = true;
            }
            return Err(e);
        }
        self.gap = false;
        let frames = i64::try_from(buffer.frames()).unwrap_or(i64::MAX);
        self.next = Some(start.saturating_add(frames));
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.encoder.poll(out)
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.encoder.finish(out)
    }
}

/// One audio encoder session: an [`AudioEncoder`] on its own encode thread, fed through
/// a bounded queue of sample blocks ([`AudioInput`]), publishing packets to any number
/// of [`Subscription`]s. Shared, dropped and finished like a
/// [`VideoSession`](crate::VideoSession).
pub struct AudioSession {
    core: Core<AudioDriver>,
}

impl AudioSession {
    /// Opens an audio encoder for `config` on `codecs` and starts its session.
    ///
    /// # Errors
    /// [`EncodeError::WrongKind`] for a video codec, [`EncodeError::Open`] when the
    /// platform can't open the encoder, and [`EncodeError::Spawn`] when the encode thread
    /// can't start.
    pub fn open(
        codecs: &dyn Codecs,
        config: AudioEncoderConfig,
        options: SessionOptions,
    ) -> Result<Self, EncodeError> {
        check_kind(&config)?;
        let encoder = codecs.audio_encoder(&config).map_err(EncodeError::Open)?;
        Self::start(encoder, config, options)
    }

    /// Starts a session on an encoder that is already open, with the `config` it was
    /// opened with.
    ///
    /// # Errors
    /// [`EncodeError::WrongKind`] for a video codec, and [`EncodeError::Spawn`] when the
    /// encode thread can't start.
    pub fn start(
        encoder: Box<dyn AudioEncoder>,
        config: AudioEncoderConfig,
        options: SessionOptions,
    ) -> Result<Self, EncodeError> {
        check_kind(&config)?;
        let info = StreamInfo {
            codec: encoder.info().clone(),
            settings: EncoderSettings::Audio(config),
        };
        let format = config.input_format;
        let max_gap =
            u128::from(format.sample_rate()) * options.max_audio_gap.as_nanos() / 1_000_000_000;
        let driver = AudioDriver {
            encoder,
            format,
            next: None,
            max_gap: i64::try_from(max_gap).unwrap_or(i64::MAX),
            gap: false,
        };
        let core = Core::start("jstream-encode-audio", driver, info, &options)?;
        Ok(Self { core })
    }

    /// A handle for the mixer side to push sample blocks into.
    pub fn input(&self) -> AudioInput {
        AudioInput {
            inner: self.core.input(),
        }
    }

    /// Subscribes with the session's default queue capacity. Every AAC packet is a
    /// keyframe, so the subscription starts with the next packet.
    pub fn subscribe(&self) -> Subscription {
        self.core.subscribe(None)
    }

    /// Subscribes with a queue of `capacity` packets (at least one).
    pub fn subscribe_with_capacity(&self, capacity: usize) -> Subscription {
        self.core.subscribe(Some(capacity))
    }

    /// The codec and settings of the stream.
    pub fn info(&self) -> &StreamInfo {
        self.core.info()
    }

    /// The session's counters now.
    pub fn stats(&self) -> SessionStats {
        self.core.stats()
    }

    /// Ends the session as [`VideoSession::finish`](crate::VideoSession::finish) does;
    /// the encoder pads its last access unit with silence.
    ///
    /// # Errors
    /// The error that stopped the session, if it failed before or while finishing.
    pub fn finish(&self) -> Result<SessionStats, EncodeError> {
        self.core.finish()
    }
}

fn check_kind(config: &AudioEncoderConfig) -> Result<(), EncodeError> {
    if config.codec.kind() == MediaKind::Audio {
        Ok(())
    } else {
        Err(EncodeError::WrongKind {
            codec: config.codec,
            expected: MediaKind::Audio,
        })
    }
}

/// Where the mixer side pushes sample blocks for one [`AudioSession`]. Cheap to clone;
/// never blocks.
#[derive(Clone)]
pub struct AudioInput {
    inner: InputHandle<AudioBuffer>,
}

impl AudioInput {
    /// Queues `buffer` for the encoder, or drops it when the queue is full. The block
    /// after a drop is preceded by silence over the gap (up to
    /// [`SessionOptions::max_audio_gap`]), so later packets keep their times.
    ///
    /// # Errors
    /// [`PushError::Full`] when the block was dropped (and counted), and
    /// [`PushError::Closed`] once the session is finishing or has ended.
    pub fn push(&self, buffer: AudioBuffer) -> Result<(), PushError> {
        self.inner.push(buffer)
    }
}

impl std::fmt::Debug for AudioInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioInput").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AudioSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSession")
            .field("info", self.info())
            .finish_non_exhaustive()
    }
}
