//! What the movie demuxers ([`super::mp4`], [`super::mkv`]) hand the movie decoder: a
//! video track and an audio track, each a list of samples by position and time, and
//! the file to read them from.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use jstream_platform::PlatformError;
use symphonia::core::audio::Channels;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_VORBIS,
};

/// The most samples a track may list: 20 million, and never more than the file has
/// bytes. Every sample worth decoding takes at least a byte, so a crafted table of
/// empty samples can't make a small file cost gigabytes of sample list (#1533).
pub(crate) fn max_samples(file_len: u64) -> usize {
    const MAX: u64 = 20_000_000;
    usize::try_from(MAX.min(file_len)).unwrap_or(0)
}

/// One sample: where it is and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    pub(crate) offset: u64,
    pub(crate) size: u32,
    /// The decode time, in the track's time scale.
    pub(crate) dts: i64,
    /// The presentation time on the file's timeline (edit list applied), in the
    /// track's time scale.
    pub(crate) pts: i64,
    pub(crate) sync: bool,
}

/// A track's codec, as far as this crate decodes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    /// H.264, with its `avcC` record.
    Avc(Vec<u8>),
    /// VP8: each frame carries what decoding needs.
    Vp8,
    /// VP9: each frame carries what decoding needs.
    Vp9,
    /// AV1, with the configuration OBUs of its `av1C` record (its sequence header),
    /// which may be empty: key frames carry it too.
    Av1(Vec<u8>),
    /// MPEG-1/2 Layer III audio.
    Mp3,
    /// Vorbis, with its identification and setup headers (Matroska's `CodecPrivate`,
    /// Xiph-laced).
    Vorbis(Vec<u8>),
    /// FLAC, with its `STREAMINFO` block.
    Flac(Vec<u8>),
    /// Opus, with its identification header (`OpusHead`, RFC 7845 §5.1).
    Opus(Vec<u8>),
    /// Anything else, by its sample entry's four-character code or, for `mp4a`, its
    /// object type.
    Other(String),
}

/// One track.
#[derive(Clone, Debug)]
pub(crate) struct Track {
    pub(crate) id: u32,
    pub(crate) video: bool,
    pub(crate) codec: Codec,
    /// Ticks per second of the sample times.
    pub(crate) timescale: u32,
    /// Video: the sample entry's width and height.
    pub(crate) size: (u32, u32),
    /// Audio: channels and sample rate.
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    /// In decoding order.
    pub(crate) samples: Vec<Sample>,
    /// Bytes every sample starts with that the file leaves out (Matroska's header
    /// stripping); empty for most files.
    pub(crate) prefix: Vec<u8>,
    /// Audio: how far before a seek's target decoding starts, in the track's time scale,
    /// so a decoder that needs a run-in (Opus's 80 ms) has settled by then. 0 for most.
    pub(crate) preroll: i64,
}

impl Track {
    /// symphonia's parameters for this audio track, or `None` for a codec this crate
    /// doesn't decode.
    pub(crate) fn audio_params(&self) -> Option<AudioCodecParameters> {
        let mut params = AudioCodecParameters::new();
        match &self.codec {
            Codec::Mp3 => {
                params.for_codec(CODEC_ID_MP3);
            }
            Codec::Flac(info) => {
                params
                    .for_codec(CODEC_ID_FLAC)
                    .with_extra_data(info.clone().into_boxed_slice());
            }
            Codec::Vorbis(headers) => {
                params
                    .for_codec(CODEC_ID_VORBIS)
                    .with_extra_data(headers.clone().into_boxed_slice());
            }
            Codec::Opus(head) => {
                // Opus always decodes at 48 kHz, whatever the container says.
                params
                    .for_codec(CODEC_ID_OPUS)
                    .with_extra_data(head.clone().into_boxed_slice())
                    .with_sample_rate(48_000)
                    .with_channels(Channels::Discrete(self.channels));
                return Some(params);
            }
            Codec::Avc(_) | Codec::Vp8 | Codec::Vp9 | Codec::Av1(_) | Codec::Other(_) => {
                return None;
            }
        }
        params
            .with_sample_rate(self.sample_rate)
            .with_channels(Channels::Discrete(self.channels));
        Some(params)
    }

    /// How long the track plays, in its time scale.
    pub(crate) fn duration(&self) -> i64 {
        self.samples
            .iter()
            .map(|s| s.pts)
            .max()
            .map_or(0, |last| last.saturating_add(self.frame_step()))
    }

    /// A typical gap between samples, for the last one's duration.
    fn frame_step(&self) -> i64 {
        match self.samples.as_slice() {
            [.., a, b] => b.dts.saturating_sub(a.dts).max(0),
            _ => 0,
        }
    }

    /// The index to start decoding from to show `pts`: the last sync sample at or
    /// before it in decoding order, or the first sample. For a track without sync
    /// samples every sample is one.
    pub(crate) fn seek_index(&self, pts: i64) -> usize {
        let mut best = 0;
        for (i, s) in self.samples.iter().enumerate() {
            if s.sync {
                // Sync samples are shown in decoding order: the first one after `pts`
                // ends the search.
                if s.pts > pts {
                    break;
                }
                best = i;
            }
        }
        best
    }
}

/// An opened movie file: its tracks, and the file to read samples from.
pub(crate) struct Demuxed {
    file: File,
    pub(crate) video: Option<Track>,
    pub(crate) audio: Option<Track>,
}

impl Demuxed {
    pub(crate) fn new(file: File, video: Option<Track>, audio: Option<Track>) -> Self {
        Self { file, video, audio }
    }

    /// Reads one of `track`'s samples, its stripped prefix put back.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the file doesn't read.
    pub(crate) fn read(
        &mut self,
        track: &Track,
        sample: &Sample,
    ) -> Result<Vec<u8>, PlatformError> {
        let io = |err: std::io::Error| PlatformError::Os {
            call: "read",
            code: err.raw_os_error().and_then(|c| u32::try_from(c).ok()),
            message: err.to_string(),
        };
        self.file.seek(SeekFrom::Start(sample.offset)).map_err(io)?;
        let mut data = Vec::with_capacity(track.prefix.len() + sample.size as usize);
        data.extend_from_slice(&track.prefix);
        let start = data.len();
        data.resize(start + usize::try_from(sample.size).unwrap_or(0), 0);
        self.file.read_exact(&mut data[start..]).map_err(io)?;
        Ok(data)
    }
}
