//! A movie file (MP4, QuickTime, Matroska or WebM) as a [`MediaDecoder`]: the crate's
//! own demuxers ([`super::mp4`], [`super::mkv`]), H.264 video on the GPU through VA-API,
//! and the audio through symphonia's decoders (MP3, FLAC and Vorbis; AAC waits for
//! ADR-0072 (g), and Opus for its FFmpeg path, so a file with that audio plays its video
//! without sound).

use jstream_platform::media::{AudioBuffer, MediaTime, Rounding, Timebase, VideoFrame};
use jstream_platform::{
    HardwarePreference, MediaDecoder, MediaDecoderConfig, MediaInfo, MediaVideoInfo, PlatformError,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

use super::audio::{AudioStream, cut_before};
use super::demux::{Codec, Demuxed, Track};
use crate::vaapi::VaapiH264Decoder;

/// The audio half: the track and its decoder.
struct Audio {
    track: Track,
    stream: AudioStream,
    next: usize,
    timebase: Timebase,
    skip_until: Option<MediaTime>,
}

/// The video half.
struct Video {
    track: Track,
    decoder: VaapiH264Decoder,
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
    /// [`PlatformError::Unsupported`] for a video codec other than H.264, or a file with
    /// neither video nor audio this decodes; [`VaapiH264Decoder::open`]'s errors.
    pub(crate) fn open(
        mut mp4: Demuxed,
        config: &MediaDecoderConfig,
    ) -> Result<Self, PlatformError> {
        let video = match mp4.video.take() {
            Some(track) => Some(open_video(track, config)?),
            None => None,
        };
        let audio = match mp4.audio.take() {
            Some(track) if config.audio => open_audio(track),
            _ => None,
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
            return Err(PlatformError::Unsupported {
                what: "a movie file with no video, and no audio Linux decodes (MP3, FLAC \
                       and Vorbis play; AAC waits for ADR-0072's decoder patent check)"
                    .into(),
            });
        }
        Ok(Self {
            file: mp4,
            video,
            audio,
            info,
        })
    }
}

/// The video decoder for `track`.
fn open_video(track: Track, config: &MediaDecoderConfig) -> Result<Video, PlatformError> {
    let Codec::Avc(record) = &track.codec else {
        return Err(PlatformError::Unsupported {
            what: format!(
                "this media file's video codec on Linux ({}): H.264 plays so far",
                match &track.codec {
                    Codec::Other(name) => name.as_str(),
                    _ => "audio",
                }
            ),
        });
    };
    if config.hardware == HardwarePreference::Avoid {
        return Err(PlatformError::Unsupported {
            what: "software video decoding of media files on Linux".into(),
        });
    }
    let timebase = Timebase::per_second(track.timescale)?;
    let decoder = VaapiH264Decoder::open(record, timebase, config.video_format)?;
    let size = decoder.size();
    if (size.width(), size.height()) != track.size {
        tracing::debug!(?size, sample_entry = ?track.size, "the SPS and the sample entry disagree on the size; the SPS wins");
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
fn open_audio(track: Track) -> Option<Audio> {
    let Some(params) = track.audio_params() else {
        tracing::info!(codec = ?track.codec, "a movie file's audio isn't decoded on Linux");
        return None;
    };
    let timebase = Timebase::per_second(track.timescale).ok()?;
    match AudioStream::new(&params, timebase) {
        Ok(stream) => Some(Audio {
            track,
            stream,
            next: 0,
            timebase,
            // As for the video: what an edit list trims off the start isn't heard.
            skip_until: Some(MediaTime::zero(Timebase::MICROS)),
        }),
        Err(err) => {
            tracing::info!(%err, "a movie file's audio doesn't decode");
            None
        }
    }
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
                .map_or(0, |n| (n.dts - sample.dts).max(0));
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
            audio.next = audio
                .track
                .samples
                .iter()
                .rposition(|s| s.pts <= target)
                .unwrap_or(0);
            audio.stream.reset();
            let micros = to.rescale(Timebase::MICROS, Rounding::Down)?;
            audio.skip_until = (micros.ticks() > 0).then_some(micros);
        }
        Ok(())
    }
}
