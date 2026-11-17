//! Audio decoding through symphonia (ADR-0072 (c)): the royalty-free codecs only, PCM,
//! FLAC, Vorbis and MP3. AAC isn't built in: symphonia's `aac` feature stays off until
//! ADR-0072 (g)'s decoder patent check says yes.
//!
//! [`AudioFile`] reads a file symphonia's own readers open (WAV, FLAC, Ogg and MP3) and
//! decodes its first audio track. [`AudioStream`] is the decoding half on its own, for
//! the packets of a container this crate demuxes itself.
//!
//! Decoded audio is interleaved `f32` at the stream's own rate. Mono stays mono, stereo
//! stays stereo, and anything wider is mixed down to stereo ([`Downmix`]). The decoder
//! trims the encoder's priming and padding where the stream says how much there is
//! (symphonia's gapless mode), so a looping file joins up without a gap.

use std::fs::File;
use std::path::{Path, PathBuf};

use jstream_platform::media::{
    AudioBuffer, AudioFormat, ChannelLayout, MediaTime, Rounding, SampleFormat, Timebase,
};
use jstream_platform::{MediaAudioInfo, PlatformError};
use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::{Error, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

/// The `call` an `Os` error names.
const CALL: &str = "symphonia";

/// How many packets in a row may fail to decode before the stream gives up. A damaged
/// packet is skipped; a stream that never decodes again isn't retried forever.
const MAX_BAD_PACKETS: u32 = 64;

/// Maps a symphonia error to the platform's.
pub(crate) fn platform_error(err: &Error) -> PlatformError {
    match err {
        Error::Unsupported(what) => PlatformError::Unsupported {
            what: format!("this media file ({what})"),
        },
        other => PlatformError::Os {
            call: CALL,
            code: None,
            message: other.to_string(),
        },
    }
}

/// Whether `err` is the end of the stream: symphonia reports it as an I/O error.
fn is_end(err: &Error) -> bool {
    matches!(err, Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof)
}

/// A stream's channel mix: the layout it is handed on in, and each output channel's
/// weights over the decoded channels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Downmix {
    layout: ChannelLayout,
    /// `weights[out][in]`, for each output channel. Empty for a pass-through.
    weights: Vec<Vec<f32>>,
    channels: usize,
}

/// The weight of a centre or surround channel folded into left and right: -3 dB.
const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

impl Downmix {
    /// The mix for a stream with `channels`: `None` when it has none.
    pub(crate) fn new(channels: &Channels) -> Option<Self> {
        let count = channels.count();
        match count {
            0 => None,
            1 => Some(Self {
                layout: ChannelLayout::Mono,
                weights: Vec::new(),
                channels: 1,
            }),
            2 => Some(Self {
                layout: ChannelLayout::Stereo,
                weights: Vec::new(),
                channels: 2,
            }),
            _ => {
                let mut left = vec![0.0; count];
                let mut right = vec![0.0; count];
                match channels {
                    Channels::Positioned(positions) => {
                        // A positioned layout stores its channels in the order of their
                        // bits, lowest first.
                        for (index, position) in positions.iter().enumerate() {
                            let (l, r) = position_weights(position);
                            left[index] = l;
                            right[index] = r;
                        }
                    }
                    _ => {
                        // Discrete channels have no positions: the first two are left
                        // and right, and the rest are left out.
                        left[0] = 1.0;
                        right[1] = 1.0;
                    }
                }
                normalise(&mut left);
                normalise(&mut right);
                Some(Self {
                    layout: ChannelLayout::Stereo,
                    weights: vec![left, right],
                    channels: count,
                })
            }
        }
    }

    /// The layout the stream is handed on in.
    pub(crate) fn layout(&self) -> ChannelLayout {
        self.layout
    }

    /// Mixes interleaved `samples` of the stream's channels into the output layout.
    fn apply(&self, samples: Vec<f32>) -> Vec<f32> {
        if self.weights.is_empty() {
            return samples;
        }
        let mut out = Vec::with_capacity(samples.len() / self.channels * self.weights.len());
        for frame in samples.chunks_exact(self.channels) {
            for weights in &self.weights {
                out.push(frame.iter().zip(weights).map(|(s, w)| s * w).sum());
            }
        }
        out
    }
}

/// How much of a channel at `position` goes to left and to right.
fn position_weights(position: Position) -> (f32, f32) {
    let left = Position::FRONT_LEFT
        | Position::FRONT_LEFT_CENTER
        | Position::FRONT_LEFT_WIDE
        | Position::SIDE_LEFT
        | Position::REAR_LEFT
        | Position::TOP_FRONT_LEFT
        | Position::TOP_SIDE_LEFT
        | Position::TOP_REAR_LEFT
        | Position::BOTTOM_FRONT_LEFT;
    let right = Position::FRONT_RIGHT
        | Position::FRONT_RIGHT_CENTER
        | Position::FRONT_RIGHT_WIDE
        | Position::SIDE_RIGHT
        | Position::REAR_RIGHT
        | Position::TOP_FRONT_RIGHT
        | Position::TOP_SIDE_RIGHT
        | Position::TOP_REAR_RIGHT
        | Position::BOTTOM_FRONT_RIGHT;
    let centre = Position::FRONT_CENTER
        | Position::REAR_CENTER
        | Position::TOP_CENTER
        | Position::TOP_FRONT_CENTER
        | Position::TOP_REAR_CENTER
        | Position::BOTTOM_FRONT_CENTER;
    let front = Position::FRONT_LEFT | Position::FRONT_RIGHT;
    let full = if front.contains(position) {
        1.0
    } else {
        MINUS_3DB
    };
    if left.contains(position) {
        (full, 0.0)
    } else if right.contains(position) {
        (0.0, full)
    } else if centre.contains(position) {
        (MINUS_3DB, MINUS_3DB)
    } else {
        // The LFE channels, and positions with no side.
        (0.0, 0.0)
    }
}

/// Scales `weights` so they sum to at most 1, so the mix can't clip what didn't.
fn normalise(weights: &mut [f32]) {
    let sum: f32 = weights.iter().sum();
    if sum > 1.0 {
        for w in weights {
            *w /= sum;
        }
    }
}

/// The engine's time base for a symphonia one.
fn timebase(tb: TimeBase) -> Result<Timebase, PlatformError> {
    Ok(Timebase::new(tb.numer.get(), tb.denom.get())?)
}

/// Decodes one audio stream's packets: symphonia's decoder for the codec, and the mix
/// to the output layout.
pub(crate) struct AudioStream {
    decoder: Box<dyn AudioDecoder>,
    downmix: Downmix,
    format: AudioFormat,
    /// The time base of the packets' timestamps.
    timebase: Timebase,
    bad_packets: u32,
}

impl AudioStream {
    /// A decoder for `params`, whose packets are timed in `timebase`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a codec that isn't built in (AAC among them)
    /// or a stream that doesn't say its rate or channels.
    pub(crate) fn new(
        params: &AudioCodecParameters,
        timebase: Timebase,
    ) -> Result<Self, PlatformError> {
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|err| platform_error(&err))?;
        let rate = params
            .sample_rate
            .ok_or_else(|| PlatformError::Unsupported {
                what: "an audio stream without a sample rate".into(),
            })?;
        let downmix = params
            .channels
            .as_ref()
            .and_then(Downmix::new)
            .ok_or_else(|| PlatformError::Unsupported {
                what: "an audio stream without channels".into(),
            })?;
        let format = AudioFormat::new(rate, downmix.layout(), SampleFormat::F32Interleaved)?;
        Ok(Self {
            decoder,
            downmix,
            format,
            timebase,
            bad_packets: 0,
        })
    }

    /// The format every decoded block is in.
    pub(crate) fn info(&self) -> MediaAudioInfo {
        MediaAudioInfo {
            format: self.format,
        }
    }

    /// Decodes `packet`. `Ok(None)` when the packet had nothing to hand on: no frames,
    /// or a damaged packet that was skipped.
    ///
    /// # Errors
    /// [`PlatformError::Os`] once too many packets in a row fail to decode.
    pub(crate) fn decode(
        &mut self,
        packet: &symphonia::core::packet::Packet,
    ) -> Result<Option<AudioBuffer>, PlatformError> {
        let decoded = match self.decoder.decode(packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(why)) => {
                self.bad_packets += 1;
                if self.bad_packets > MAX_BAD_PACKETS {
                    return Err(platform_error(&Error::DecodeError(why)));
                }
                tracing::debug!(why, "skipping an audio packet that didn't decode");
                return Ok(None);
            }
            Err(err) => return Err(platform_error(&err)),
        };
        self.bad_packets = 0;
        if decoded.frames() == 0 {
            return Ok(None);
        }
        let mut samples = Vec::new();
        decoded.copy_to_vec_interleaved::<f32>(&mut samples);
        let samples = self.downmix.apply(samples);
        // Gapless decoding drops the packet's first `trim_start` frames (an MP3's encoder
        // delay), so the block starts that much after the packet.
        let trim = i64::try_from(packet.trim_start.get()).unwrap_or(0);
        let pts = MediaTime::new(packet.pts.get().saturating_add(trim), self.timebase);
        Ok(Some(AudioBuffer::new(self.format, pts, samples)?))
    }

    /// Forgets what the decoder holds, after a seek.
    pub(crate) fn reset(&mut self) {
        self.decoder.reset();
    }
}

/// An audio file symphonia reads: WAV, FLAC, Ogg (Vorbis) or MP3.
pub(crate) struct AudioFile {
    /// The file, opened again for each seek.
    path: PathBuf,
    reader: Box<dyn FormatReader>,
    track: u32,
    /// `None` when the file was opened without audio.
    stream: Option<AudioStream>,
    duration: Option<MediaTime>,
    /// Set by a seek past the end, until the next seek.
    at_end: bool,
    /// After a seek, the time the next block starts at: what decodes before it is cut.
    skip_until: Option<MediaTime>,
}

/// Opens a reader on `file`, or `None` when symphonia can't read its container.
fn probe(path: &Path, file: File) -> Result<Option<Box<dyn FormatReader>>, PlatformError> {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    match symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        Ok(reader) => Ok(Some(reader)),
        Err(Error::Unsupported(_)) => Ok(None),
        Err(err) if is_end(&err) => Ok(None),
        Err(err) => Err(platform_error(&err)),
    }
}

impl AudioFile {
    /// Opens `path` if symphonia can read its container. `Ok(None)` when it can't, so the
    /// caller tries its own demuxers.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a file with no audio track or one whose codec
    /// isn't built in, and [`PlatformError::Os`] when reading fails.
    pub(crate) fn open(
        path: &Path,
        file: File,
        audio: bool,
    ) -> Result<Option<Self>, PlatformError> {
        let Some(reader) = probe(path, file)? else {
            return Ok(None);
        };
        let track = reader
            .default_track(TrackType::Audio)
            .or_else(|| reader.first_track(TrackType::Audio))
            .ok_or_else(|| PlatformError::Unsupported {
                what: "a media file with no audio or video".into(),
            })?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            return Err(PlatformError::Unsupported {
                what: "an audio track symphonia can't describe".into(),
            });
        };
        let tb = timebase(track.time_base.ok_or_else(|| PlatformError::Unsupported {
            what: "an audio track without a time base".into(),
        })?)?;
        let duration = track.duration.and_then(|d| {
            let ticks = i64::try_from(d.get()).ok()?;
            Some(MediaTime::new(ticks, tb))
        });
        let id = track.id;
        let stream = if audio {
            Some(AudioStream::new(params, tb)?)
        } else {
            None
        };
        Ok(Some(Self {
            path: path.to_path_buf(),
            reader,
            track: id,
            stream,
            duration,
            at_end: false,
            skip_until: None,
        }))
    }

    /// The decoded stream's format, or `None` when the file was opened without audio.
    pub(crate) fn info(&self) -> Option<MediaAudioInfo> {
        self.stream.as_ref().map(AudioStream::info)
    }

    /// How long the file plays, when it says.
    pub(crate) fn duration(&self) -> Option<MediaTime> {
        self.duration
    }

    /// The next decoded block, or `Ok(None)` at the end.
    pub(crate) fn next(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        if self.at_end {
            return Ok(None);
        }
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(err) if is_end(&err) => return Ok(None),
                Err(Error::ResetRequired) => {
                    stream.reset();
                    continue;
                }
                Err(err) => return Err(platform_error(&err)),
            };
            if packet.track_id != self.track {
                continue;
            }
            let Some(block) = stream.decode(&packet)? else {
                continue;
            };
            match self.skip_until {
                Some(until) => {
                    if let Some(block) = cut_before(block, until)? {
                        self.skip_until = None;
                        return Ok(Some(block));
                    }
                }
                None => return Ok(Some(block)),
            }
        }
    }

    /// Moves to `to` on the file's timeline; a time past the end gives the end.
    ///
    /// The file is opened again first: some of symphonia's readers don't read on after
    /// seeking from the end of the file, which is where a looping file seeks from.
    pub(crate) fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError> {
        let file = File::open(&self.path).map_err(|err| PlatformError::Os {
            call: "open",
            code: err.raw_os_error().and_then(|c| u32::try_from(c).ok()),
            message: err.to_string(),
        })?;
        self.reader = probe(&self.path, file)?.ok_or_else(|| PlatformError::Os {
            call: CALL,
            code: None,
            message: "the file no longer opens".into(),
        })?;
        if let Some(stream) = self.stream.as_mut() {
            stream.reset();
        }
        self.at_end = false;
        self.skip_until = None;
        let micros = to.rescale(Timebase::MICROS, Rounding::Down)?.ticks().max(0);
        if micros == 0 {
            return Ok(());
        }
        let seek = SeekTo::Time {
            time: Time::from_micros(micros),
            track_id: Some(self.track),
        };
        match self.reader.seek(SeekMode::Accurate, seek) {
            Ok(_) => self.skip_until = Some(MediaTime::new(micros, Timebase::MICROS)),
            // Past the end: the next read gives the end.
            Err(Error::SeekError(SeekErrorKind::OutOfRange)) => self.at_end = true,
            Err(err) if is_end(&err) => self.at_end = true,
            Err(err) => return Err(platform_error(&err)),
        }
        Ok(())
    }
}

/// `block` without the frames before `until`: `None` when it ends before it.
pub(crate) fn cut_before(
    block: AudioBuffer,
    until: MediaTime,
) -> Result<Option<AudioBuffer>, PlatformError> {
    let format = block.format();
    let rate = i64::from(format.sample_rate());
    let start = block
        .pts()
        .rescale(Timebase::MICROS, Rounding::Down)?
        .ticks();
    let until = until.ticks();
    if start >= until {
        return Ok(Some(block));
    }
    // Whole frames before `until`, rounded down so the cut never overshoots it.
    let skip = usize::try_from((until - start) * rate / 1_000_000).unwrap_or(usize::MAX);
    if skip >= block.frames() {
        return Ok(None);
    }
    let channels = format.channels();
    let skipped = i64::try_from(skip).unwrap_or(i64::MAX);
    let pts = MediaTime::new(start + skipped * 1_000_000 / rate, Timebase::MICROS);
    let samples = block.into_samples().split_off(skip * channels);
    Ok(Some(AudioBuffer::new(format, pts, samples)?))
}

#[cfg(test)]
mod tests;
