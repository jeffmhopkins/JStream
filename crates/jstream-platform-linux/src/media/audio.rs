//! Audio decoding (ADR-0072 (c)): PCM, FLAC, Vorbis and MP3 through symphonia, Opus
//! through `rusty-opus` ([`super::opus`], ADR-0159), and AAC-LC through symphonia within
//! ADR-0160's limits: never SBR or parametric stereo ([`super::aac`] refuses a stream
//! that signals them).
//!
//! [`AudioFile`] reads a file symphonia's own readers open (WAV, FLAC, Ogg, MP3 and ADTS
//! AAC) and
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

use super::opus::{self, OpusStream};
use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_OPUS};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::{Error, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

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
        match channels {
            // A positioned layout stores its channels in the order of their bits,
            // lowest first.
            Channels::Positioned(positions) => Self::ordered(
                channels.count(),
                Some(&positions.iter().collect::<Vec<_>>()),
            ),
            // Discrete channels have no positions.
            _ => Self::ordered(channels.count(), None),
        }
    }

    /// The mix for `count` channels in the order `positions` lists them, when they have
    /// positions: `None` when there are none.
    pub(crate) fn ordered(count: usize, positions: Option<&[Position]>) -> Option<Self> {
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
                match positions.filter(|p| p.len() == count) {
                    Some(positions) => {
                        for (index, position) in positions.iter().enumerate() {
                            let (l, r) = position_weights(*position);
                            left[index] = l;
                            right[index] = r;
                        }
                    }
                    None => {
                        // Without positions, the first two are left and right, and the
                        // rest are left out.
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

/// The decoder behind an [`AudioStream`].
enum Decoder {
    /// symphonia's, for PCM, FLAC, Vorbis and MP3.
    Symphonia(Box<dyn AudioDecoder>),
    /// Opus, through `rusty-opus` (ADR-0159).
    Opus(Box<OpusStream>),
}

/// What decoding one packet gave.
enum Decoded {
    /// Interleaved samples of the stream's channels, and how many frames the block
    /// starts after the packet.
    Samples(Vec<f32>, u64),
    /// A damaged packet, and why.
    Bad(String),
}

/// Decodes one audio stream's packets: the codec's decoder, and the mix to the output
/// layout.
pub(crate) struct AudioStream {
    decoder: Decoder,
    downmix: Downmix,
    format: AudioFormat,
    /// The time base of the packets' timestamps.
    timebase: Timebase,
    bad_packets: u32,
    /// Where the next block starts if it follows on from the last, at the stream's
    /// rate: `None` after a reset.
    expected: Option<MediaTime>,
}

impl AudioStream {
    /// A decoder for `params`, whose packets are timed in `timebase`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a codec that isn't built in, AAC beyond
    /// AAC-LC, or a stream that doesn't say its rate or channels.
    pub(crate) fn new(
        params: &AudioCodecParameters,
        timebase: Timebase,
    ) -> Result<Self, PlatformError> {
        if params.codec == CODEC_ID_OPUS {
            return Self::opus(params, timebase);
        }
        if params.codec == CODEC_ID_AAC {
            super::aac::check(params.extra_data.as_deref().unwrap_or_default())?;
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|err| platform_error(&err))?;
        // The decoder's own rate: an AAC stream's config can say another than its
        // container (implicit HE-AAC's LC core runs at half the rate).
        let rate = decoder
            .codec_params()
            .sample_rate
            .or(params.sample_rate)
            .ok_or_else(|| PlatformError::Unsupported {
                what: "an audio stream without a sample rate".into(),
            })?;
        let downmix = decoder
            .codec_params()
            .channels
            .as_ref()
            .or(params.channels.as_ref())
            .and_then(Downmix::new)
            .ok_or_else(|| PlatformError::Unsupported {
                what: "an audio stream without channels".into(),
            })?;
        Self::with(Decoder::Symphonia(decoder), downmix, rate, timebase)
    }

    /// An Opus stream: its identification header is `params`' extra data.
    fn opus(params: &AudioCodecParameters, timebase: Timebase) -> Result<Self, PlatformError> {
        let head = params
            .extra_data
            .as_deref()
            .and_then(opus::Head::parse)
            .ok_or_else(|| PlatformError::Unsupported {
                what: "an Opus stream without a header this decodes".into(),
            })?;
        let stream = OpusStream::new(&head).map_err(|err| PlatformError::Unsupported {
            what: format!("this Opus stream's layout ({err})"),
        })?;
        let downmix =
            Downmix::ordered(stream.channels(), head.positions().as_deref()).ok_or_else(|| {
                PlatformError::Unsupported {
                    what: "an audio stream without channels".into(),
                }
            })?;
        Self::with(
            Decoder::Opus(Box::new(stream)),
            downmix,
            opus::RATE,
            timebase,
        )
    }

    fn with(
        decoder: Decoder,
        downmix: Downmix,
        rate: u32,
        timebase: Timebase,
    ) -> Result<Self, PlatformError> {
        let format = AudioFormat::new(rate, downmix.layout(), SampleFormat::F32Interleaved)?;
        Ok(Self {
            decoder,
            downmix,
            format,
            timebase,
            bad_packets: 0,
            expected: None,
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
        let decoded = match &mut self.decoder {
            Decoder::Symphonia(decoder) => match decoder.decode(packet) {
                Ok(decoded) => {
                    let mut samples = Vec::new();
                    decoded.copy_to_vec_interleaved::<f32>(&mut samples);
                    // Gapless decoding drops the packet's first `trim_start` frames (an
                    // MP3's encoder delay), so the block starts that much after the
                    // packet.
                    Decoded::Samples(samples, packet.trim_start.get())
                }
                Err(Error::DecodeError(why)) => Decoded::Bad(why.to_string()),
                Err(err) => return Err(platform_error(&err)),
            },
            Decoder::Opus(stream) => match stream.decode(&packet.data) {
                Ok(samples) => {
                    // Opus trims here what a packet says to (Ogg's end padding):
                    // symphonia's decoders do it themselves. The pre-skip is cut by
                    // whoever reads the container ([`AudioFile`], or Matroska's
                    // `CodecDelay`), which times the stream from after it.
                    let channels = stream.channels();
                    let frames = samples.len() / channels;
                    let start = usize::try_from(packet.trim_start.get())
                        .unwrap_or(usize::MAX)
                        .min(frames);
                    let end = usize::try_from(packet.trim_end.get())
                        .unwrap_or(usize::MAX)
                        .min(frames - start);
                    let kept = samples[start * channels..(frames - end) * channels].to_vec();
                    Decoded::Samples(kept, packet.trim_start.get())
                }
                Err(err) => Decoded::Bad(err.to_string()),
            },
        };
        let (samples, trim) = match decoded {
            Decoded::Samples(samples, trim) => (samples, trim),
            Decoded::Bad(why) => {
                self.bad_packets += 1;
                if self.bad_packets > MAX_BAD_PACKETS {
                    return Err(PlatformError::Os {
                        call: CALL,
                        code: None,
                        message: format!("the audio stops decoding: {why}"),
                    });
                }
                tracing::debug!(why, "skipping an audio packet that didn't decode");
                return Ok(None);
            }
        };
        self.bad_packets = 0;
        if samples.is_empty() {
            return Ok(None);
        }
        let samples = self.downmix.apply(samples);
        let trim = i64::try_from(trim).unwrap_or(0);
        let pts = MediaTime::new(packet.pts.get().saturating_add(trim), self.timebase);
        let pts = self.follow_on(pts, samples.len() / self.format.channels())?;
        Ok(Some(AudioBuffer::new(self.format, pts, samples)?))
    }

    /// The time a block of `frames` decoded from a packet at `pts` starts at. A
    /// container that times packets coarsely (Matroska in milliseconds) puts a block
    /// up to a tick off from where the last one ended; within a tick, the block
    /// follows on exactly, so the audio has no gaps or overlaps.
    fn follow_on(&mut self, pts: MediaTime, frames: usize) -> Result<MediaTime, PlatformError> {
        let rate = self.format.sample_rate();
        let samples = Timebase::per_second(rate)?;
        let at = pts.rescale(samples, Rounding::Nearest)?;
        // One packet tick, in samples, rounded up.
        let tick = (u64::from(rate) * u64::from(self.timebase.num()))
            .div_ceil(u64::from(self.timebase.den()));
        let tick = i64::try_from(tick).unwrap_or(i64::MAX);
        let start = match self.expected {
            Some(expected) if at.ticks().abs_diff(expected.ticks()) <= tick.unsigned_abs() => {
                expected
            }
            _ => at,
        };
        let frames = i64::try_from(frames).unwrap_or(i64::MAX);
        self.expected = Some(MediaTime::new(
            start.ticks().saturating_add(frames),
            samples,
        ));
        Ok(start)
    }

    /// Forgets what the decoder holds, after a seek.
    pub(crate) fn reset(&mut self) {
        self.expected = None;
        match &mut self.decoder {
            Decoder::Symphonia(decoder) => decoder.reset(),
            Decoder::Opus(stream) => stream.reset(),
        }
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
    /// How far the reader's packet times run ahead of the file's timeline, in the
    /// track's time base: an Ogg Opus stream's pre-skip, which symphonia leaves in its
    /// granule clock (zero otherwise).
    delay: MediaTime,
    /// How far before its target a seek starts decoding, so the decoder has settled by
    /// then: Opus's 80 ms (RFC 7845 §4.6), nothing for the others.
    preroll: MediaTime,
}

/// How far before a seek's target an Opus stream starts decoding (RFC 7845 §4.6).
const OPUS_PREROLL_MICROS: i64 = 80_000;

/// The first 64 KiB of `path`, where an ADTS stream's first frame header is (after any
/// ID3 tag); `None` when it can't be read.
fn adts_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    Some(head)
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
        let opus = params.codec == CODEC_ID_OPUS;
        // symphonia's Ogg reader times Opus packets on the granule clock, which counts
        // the pre-skip, and trims none of it per packet: the file's timeline starts
        // after it.
        let delay = MediaTime::new(
            if opus {
                track.delay.map_or(0, i64::from)
            } else {
                0
            },
            tb,
        );
        let preroll = MediaTime::new(if opus { OPUS_PREROLL_MICROS } else { 0 }, Timebase::MICROS);
        // An ADTS stream carries no AudioSpecificConfig: its first frame header says the
        // profile and channels, which symphonia's decoder doesn't check.
        if audio
            && params.codec == CODEC_ID_AAC
            && params.extra_data.is_none()
            && let Some(asc) = adts_head(path).as_deref().and_then(super::aac::adts_config)
        {
            super::aac::check(&asc)?;
        }
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
            skip_until: (delay.ticks() > 0).then_some(MediaTime::new(0, tb)),
            delay,
            preroll,
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
            let mut packet = match self.reader.next_packet() {
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
            packet.pts = Timestamp::new(packet.pts.get().saturating_sub(self.delay.ticks()));
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
        let micros = to.rescale(Timebase::MICROS, Rounding::Down)?.ticks().max(0);
        // What decodes before the target is cut, from the start of the file too (an Ogg
        // Opus stream's pre-skip).
        self.skip_until = Some(MediaTime::new(micros, Timebase::MICROS));
        // The reader's own time for the target, less the pre-roll.
        let delay = self.delay.rescale(Timebase::MICROS, Rounding::Down)?;
        let from = micros
            .saturating_add(delay.ticks())
            .saturating_sub(self.preroll.ticks());
        if micros == 0 || from <= 0 {
            return Ok(());
        }
        let seek = SeekTo::Time {
            time: Time::from_micros(from),
            track_id: Some(self.track),
        };
        match self.reader.seek(SeekMode::Accurate, seek) {
            Ok(_) => {}
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
