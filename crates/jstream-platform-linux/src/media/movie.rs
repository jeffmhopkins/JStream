//! A movie file (MP4, QuickTime, Matroska or WebM) as a [`MediaDecoder`]: the crate's
//! own demuxers ([`super::mp4`], [`super::mkv`]), H.264, VP8, VP9 and AV1 video on the
//! GPU through VA-API, and the audio through the crate's decoders (MP3, FLAC, Vorbis and
//! AAC-LC, and Opus in Matroska and WebM). A file whose audio doesn't decode plays its
//! video without sound.

use jstream_platform::media::{AudioBuffer, FrameSize, MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::{
    HardwarePreference, MediaDecoder, MediaDecoderConfig, MediaInfo, MediaVideoInfo, PlatformError,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

use super::audio::{AudioStream, cut_before};
use super::demux::{Codec, Demuxed, Track};
use crate::vaapi::{VaapiAv1Decoder, VaapiH264Decoder, VaapiVp8Decoder, VaapiVp9Decoder};

/// The audio half: the track and its decoder.
struct Audio {
    track: Track,
    stream: AudioStream,
    next: usize,
    timebase: Timebase,
    skip_until: Option<MediaTime>,
}

/// A video track's decoder, by codec.
enum VideoDecoder {
    H264(Box<VaapiH264Decoder>),
    Vp8(Box<VaapiVp8Decoder>),
    Vp9(Box<VaapiVp9Decoder>),
    Av1(Box<VaapiAv1Decoder>),
}

impl VideoDecoder {
    fn size(&self) -> FrameSize {
        match self {
            Self::H264(d) => d.size(),
            Self::Vp8(d) => d.size(),
            Self::Vp9(d) => d.size(),
            Self::Av1(d) => d.size(),
        }
    }

    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<(), PlatformError> {
        match self {
            Self::H264(d) => d.decode(sample, pts),
            Self::Vp8(d) => d.decode(sample, pts),
            Self::Vp9(d) => d.decode(sample, pts),
            Self::Av1(d) => d.decode(sample, pts),
        }
    }

    fn flush(&mut self) -> Result<(), PlatformError> {
        match self {
            Self::H264(d) => d.flush(),
            Self::Vp8(d) => {
                d.flush();
                Ok(())
            }
            Self::Vp9(d) => {
                d.flush();
                Ok(())
            }
            Self::Av1(d) => {
                d.flush();
                Ok(())
            }
        }
    }

    fn reset(&mut self) {
        match self {
            Self::H264(d) => d.reset(),
            Self::Vp8(d) => d.reset(),
            Self::Vp9(d) => d.reset(),
            Self::Av1(d) => d.reset(),
        }
    }

    fn pop(&mut self) -> Option<VideoFrame> {
        match self {
            Self::H264(d) => d.pop(),
            Self::Vp8(d) => d.pop(),
            Self::Vp9(d) => d.pop(),
            Self::Av1(d) => d.pop(),
        }
    }
}

/// The video half.
struct Video {
    track: Track,
    decoder: VideoDecoder,
    next: usize,
    flushed: bool,
    timebase: Timebase,
    /// After a seek: frames that end before it are dropped.
    skip_before: Option<i64>,
}

/// An open movie file.
pub(crate) struct Movie {
    file: Demuxed,
    video: Option<Video>,
    audio: Option<Audio>,
    info: MediaInfo,
}

impl Movie {
    /// Opens the decoders for `mp4`'s tracks.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a video codec other than H.264, VP8, VP9 and
    /// AV1, or a file with neither video nor audio this decodes (naming why its audio
    /// doesn't); the video decoder's errors.
    pub(crate) fn open(
        mut mp4: Demuxed,
        config: &MediaDecoderConfig,
    ) -> Result<Self, PlatformError> {
        let video = match mp4.video.take() {
            Some(track) => Some(open_video(&mut mp4, track, config)?),
            None => None,
        };
        let (audio, audio_error) = match mp4.audio.take() {
            Some(track) if config.audio => match open_audio(track) {
                Ok(audio) => (Some(audio), None),
                Err(err) => {
                    tracing::info!(%err, "a movie file's audio doesn't decode");
                    (None, Some(err))
                }
            },
            _ => (None, None),
        };
        let duration = video
            .as_ref()
            .map(|v| MediaTime::new(v.track.duration(), v.timebase))
            .or_else(|| {
                audio
                    .as_ref()
                    .map(|a| MediaTime::new(a.track.duration(), a.timebase))
            });
        let info = MediaInfo {
            video: video.as_ref().map(|v| MediaVideoInfo {
                size: v.decoder.size(),
                frame_period: frame_period(&v.track),
            }),
            audio: audio.as_ref().map(|a| a.stream.info()),
            duration,
        };
        if info.video.is_none() && info.audio.is_none() {
            return Err(audio_error.unwrap_or_else(|| PlatformError::Unsupported {
                what: "a movie file with no video, and no audio Linux decodes (MP3, FLAC, \
                       Vorbis, AAC-LC and Opus play)"
                    .into(),
            }));
        }
        Ok(Self {
            file: mp4,
            video,
            audio,
            info,
        })
    }
}

/// The video decoder for `track`, whose samples `file` reads.
fn open_video(
    file: &mut Demuxed,
    track: Track,
    config: &MediaDecoderConfig,
) -> Result<Video, PlatformError> {
    let unsupported = |name: &str| PlatformError::Unsupported {
        what: format!(
            "this media file's video codec on Linux ({name}): H.264, VP8, VP9 and AV1 play so \
             far"
        ),
    };
    match &track.codec {
        Codec::Avc(_) | Codec::Vp8 | Codec::Vp9 | Codec::Av1(_) => {}
        Codec::Other(name) => return Err(unsupported(name)),
        _ => return Err(unsupported("audio")),
    }
    if config.hardware == HardwarePreference::Avoid {
        return Err(PlatformError::Unsupported {
            what: "software video decoding of media files on Linux".into(),
        });
    }
    let timebase = Timebase::per_second(track.timescale)?;
    let decoder = match &track.codec {
        Codec::Avc(record) => VideoDecoder::H264(Box::new(VaapiH264Decoder::open(
            record,
            timebase,
            config.video_format,
        )?)),
        other => {
            // VP8's, VP9's and AV1's size and colour are in the first key frame.
            let first = track
                .samples
                .iter()
                .find(|s| s.sync)
                .copied()
                .ok_or_else(|| PlatformError::Os {
                    call: "media file",
                    code: None,
                    message: "a video track without a key frame".into(),
                })?;
            let data = file.read(&track, &first)?;
            let format = config.video_format;
            match other {
                Codec::Vp8 => {
                    VideoDecoder::Vp8(Box::new(VaapiVp8Decoder::open(&data, timebase, format)?))
                }
                Codec::Av1(obus) => VideoDecoder::Av1(Box::new(VaapiAv1Decoder::open(
                    obus, &data, timebase, format,
                )?)),
                _ => VideoDecoder::Vp9(Box::new(VaapiVp9Decoder::open(&data, timebase, format)?)),
            }
        }
    };
    let size = decoder.size();
    if (size.width(), size.height()) != track.size {
        tracing::debug!(?size, container = ?track.size, "the stream and its container disagree on the size; the stream wins");
    }
    // An edit list that trims the start leaves the cut frames before 0: decoded, as
    // later frames reference them, but not shown, as after `seek(0)`.
    let skip_before = Some(1 - frame_step(&track));
    Ok(Video {
        track,
        decoder,
        next: 0,
        flushed: false,
        timebase,
        skip_before,
    })
}

/// The audio decoder for `track`, or `None` (logged) for a codec this doesn't decode:
/// the video still plays.
fn open_audio(track: Track) -> Result<Audio, PlatformError> {
    let Some(params) = track.audio_params() else {
        return Err(PlatformError::Unsupported {
            what: format!(
                "this movie file's audio codec on Linux ({}): MP3, FLAC, Vorbis, AAC-LC and \
                 Opus play",
                match &track.codec {
                    Codec::Other(name) => name.as_str(),
                    _ => "video",
                }
            ),
        });
    };
    let timebase = Timebase::per_second(track.timescale)?;
    let stream = AudioStream::new(&params, timebase)?;
    Ok(Audio {
        track,
        stream,
        next: 0,
        timebase,
        // As for the video: what an edit list trims off the start isn't heard.
        skip_until: Some(MediaTime::zero(Timebase::MICROS)),
    })
}

/// The first gap between decode times, in the track's ticks.
fn frame_step(track: &Track) -> i64 {
    match track.samples.as_slice() {
        [a, b, ..] => b.dts.saturating_sub(a.dts).max(0),
        _ => 0,
    }
}

/// The nominal frame duration.
fn frame_period(track: &Track) -> Option<Timebase> {
    let step = u32::try_from(frame_step(track)).ok().filter(|s| *s > 0)?;
    Timebase::new(step, track.timescale).ok()
}

impl MediaDecoder for Movie {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
        let Some(video) = self.video.as_mut() else {
            return Ok(None);
        };
        loop {
            if let Some(frame) = video.decoder.pop() {
                if let Some(before) = video.skip_before {
                    if frame.info().pts.ticks() < before {
                        continue;
                    }
                    video.skip_before = None;
                }
                return Ok(Some(frame));
            }
            if let Some(sample) = video.track.samples.get(video.next).copied() {
                video.next += 1;
                let data = self.file.read(&video.track, &sample)?;
                video.decoder.decode(&data, sample.pts)?;
            } else if !video.flushed {
                video.flushed = true;
                video.decoder.flush()?;
            } else {
                return Ok(None);
            }
        }
    }

    fn next_audio(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
        let Some(audio) = self.audio.as_mut() else {
            return Ok(None);
        };
        while let Some(sample) = audio.track.samples.get(audio.next).copied() {
            audio.next += 1;
            let data = self.file.read(&audio.track, &sample)?;
            let duration = audio
                .track
                .samples
                .get(audio.next)
                .map_or(0, |n| n.dts.saturating_sub(sample.dts).max(0));
            let packet = Packet::new(
                audio.track.id,
                Timestamp::new(sample.pts),
                Duration::new(u64::try_from(duration).unwrap_or(0)),
                data,
            );
            let Some(block) = audio.stream.decode(&packet)? else {
                continue;
            };
            match audio.skip_until {
                Some(until) => {
                    if let Some(block) = cut_before(block, until)? {
                        audio.skip_until = None;
                        return Ok(Some(block));
                    }
                }
                None => return Ok(Some(block)),
            }
        }
        Ok(None)
    }

    fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError> {
        if let Some(video) = self.video.as_mut() {
            let target = to.rescale(video.timebase, Rounding::Down)?.ticks();
            video.next = video.track.seek_index(target);
            video.flushed = false;
            video.decoder.reset();
            // Keep the frame on screen at `to`: drop only those that end before it.
            let step = frame_step(&video.track);
            video.skip_before = (video.next > 0 || target > 0).then_some(target - step + 1);
        }
        if let Some(audio) = self.audio.as_mut() {
            let target = to.rescale(audio.timebase, Rounding::Down)?.ticks();
            // A codec that needs a run-in (Opus) starts that far before the target; what
            // decodes before the target is cut below.
            let start = target.saturating_sub(audio.track.preroll);
            audio.next = audio
                .track
                .samples
                .iter()
                .rposition(|s| s.pts <= start)
                .unwrap_or(0);
            audio.stream.reset();
            let micros = to.rescale(Timebase::MICROS, Rounding::Down)?;
            // At 0 too: what decodes before the start (an edit list's trim, Opus's
            // pre-skip) is cut as it was when the file opened.
            audio.skip_until = Some(micros);
        }
        Ok(())
    }
}
