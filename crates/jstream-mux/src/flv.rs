//! FLV: the container RTMP carries. [`FlvTagger`] turns packets into tags with no I/O,
//! for the RTMP client to send as messages; [`FlvWriter`] adds the file header and tag
//! framing and writes to any [`std::io::Write`].
//!
//! H.264 and AAC use the legacy FLV tags (`CodecID` 7, `SoundFormat` 10). HEVC and AV1
//! use enhanced RTMP's `ExVideoTagHeader`, which names the codec by FourCC
//! ([`VideoFourCc`]); [`FlvTagger::with_enhanced_video`] selects it.

use std::io::Write;

use jstream_media::{Codec, MediaError, MediaKind, MediaTime, Packet, Rounding, Timebase};

use crate::MuxError;
use crate::aac::AudioSpecificConfig;
use crate::amf0::Amf0Value;
use crate::av1::{self, Av1CodecConfig};
use crate::h264::{AvcDecoderConfig, annexb_to_avcc};
use crate::hevc::HevcDecoderConfig;

/// FLV `CodecID` of AVC (H.264).
const VIDEO_CODEC_AVC: u8 = 7;
/// The `IsExHeader` bit of an enhanced RTMP video tag's first byte.
const EX_HEADER: u8 = 0x80;
/// FLV `SoundFormat` of AAC.
const SOUND_FORMAT_AAC: u8 = 10;
/// The largest composition time offset an `SI24` field holds.
const MAX_CTS: i64 = 0x7F_FFFF;
/// The largest tag body a 24-bit `DataSize` holds.
const MAX_TAG_BODY: usize = 0xFF_FFFF;

/// The kind of an FLV tag. The values are also the RTMP message type IDs that carry the
/// same bodies (8 audio, 9 video, 18 AMF0 data).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagType {
    /// An audio tag (8).
    Audio,
    /// A video tag (9).
    Video,
    /// A script data tag (18), holding AMF0 values.
    Script,
}

impl TagType {
    /// The FLV `TagType` byte, equal to the RTMP message type ID.
    pub fn id(self) -> u8 {
        match self {
            Self::Audio => 8,
            Self::Video => 9,
            Self::Script => 18,
        }
    }
}

/// The `AVCPacketType` byte of an AVC video tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AvcPacketType {
    /// An `AVCDecoderConfigurationRecord` (0).
    SequenceHeader,
    /// One access unit of AVCC NAL units (1).
    Nalu,
    /// End of sequence (2), with an empty payload.
    EndOfSequence,
}

/// A video codec enhanced RTMP carries, named by its FourCC in the `ExVideoTagHeader` and,
/// as a number, in `onMetaData`'s `videocodecid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoFourCc {
    /// HEVC, `hvc1`.
    Hevc,
    /// AV1, `av01`.
    Av1,
}

impl VideoFourCc {
    /// The four ASCII bytes, as written in the tag.
    pub fn bytes(self) -> [u8; 4] {
        match self {
            Self::Hevc => *b"hvc1",
            Self::Av1 => *b"av01",
        }
    }

    /// The packet codec this FourCC carries.
    pub fn codec(self) -> Codec {
        match self {
            Self::Hevc => Codec::Hevc,
            Self::Av1 => Codec::Av1,
        }
    }

    /// The `videocodecid` number: the FourCC read as a big-endian integer.
    pub fn metadata_id(self) -> u32 {
        u32::from_be_bytes(self.bytes())
    }
}

/// The `VideoPacketType` of an enhanced RTMP video tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExVideoPacketType {
    /// The decoder configuration record (0).
    SequenceStart,
    /// One coded frame (1). For HEVC, a 24-bit composition time comes first.
    CodedFrames,
    /// End of sequence (2), with an empty payload.
    SequenceEnd,
    /// One coded frame with a composition time of zero, which is not written (3).
    CodedFramesX,
}

/// The `AACPacketType` byte of an AAC audio tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AacPacketType {
    /// An `AudioSpecificConfig` (0).
    SequenceHeader,
    /// One raw AAC access unit (1).
    Raw,
}

/// One FLV tag: what an RTMP client sends as a message, and what [`FlvWriter`] frames
/// into a file.
///
/// `body` is the tag's data exactly as FLV defines it: for video, the frame-type and
/// codec byte, the `AVCPacketType`, the 24-bit composition time and the payload; for
/// audio, the sound-format byte, the `AACPacketType` and the payload; for script data,
/// the AMF0 values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlvTag {
    /// What the tag carries.
    pub tag_type: TagType,
    /// The decode timestamp in milliseconds from the stream origin, wrapped to 32 bits
    /// as FLV and RTMP do.
    pub timestamp: u32,
    /// The tag data.
    pub body: Vec<u8>,
}

impl FlvTag {
    /// The 11-byte FLV tag header: type, 24-bit data size, the low 24 bits of the
    /// timestamp, the extended (high) timestamp byte, and a zero stream ID.
    ///
    /// # Errors
    /// [`MuxError::TooLong`] when the body exceeds the 24-bit data size.
    pub fn header(&self) -> Result<[u8; 11], MuxError> {
        let size = self.body_size()?.to_be_bytes();
        let ts = self.timestamp.to_be_bytes();
        Ok([
            self.tag_type.id(),
            size[1],
            size[2],
            size[3],
            ts[1],
            ts[2],
            ts[3],
            ts[0],
            0,
            0,
            0,
        ])
    }

    /// Writes the tag header, the body and the `PreviousTagSize` that follows every tag.
    ///
    /// # Errors
    /// [`MuxError::TooLong`] as for [`FlvTag::header`], and [`MuxError::Io`].
    pub fn write_to<W: Write>(&self, out: &mut W) -> Result<(), MuxError> {
        let header = self.header()?;
        let previous_tag_size = self.body_size()? + 11;
        out.write_all(&header)?;
        out.write_all(&self.body)?;
        out.write_all(&previous_tag_size.to_be_bytes())?;
        Ok(())
    }

    fn body_size(&self) -> Result<u32, MuxError> {
        if self.body.len() > MAX_TAG_BODY {
            return Err(MuxError::TooLong {
                what: "FLV tag body",
                len: self.body.len(),
                max: MAX_TAG_BODY,
            });
        }
        // At most 2^24 - 1, checked above.
        Ok(self.body.len() as u32)
    }
}

/// The 9-byte FLV file header followed by `PreviousTagSize0` (always zero).
pub fn file_header(has_audio: bool, has_video: bool) -> [u8; 13] {
    let flags = (u8::from(has_audio) << 2) | u8::from(has_video);
    [b'F', b'L', b'V', 1, flags, 0, 0, 0, 9, 0, 0, 0, 0]
}

/// The body of an AVC video tag.
///
/// `cts_ms` is the composition time offset, pts − dts in milliseconds, stored as a signed
/// 24-bit big-endian integer.
///
/// # Errors
/// [`MuxError::CompositionOffsetOutOfRange`] when `cts_ms` doesn't fit in 24 bits.
pub fn video_tag_body(
    keyframe: bool,
    packet_type: AvcPacketType,
    cts_ms: i32,
    payload: &[u8],
) -> Result<Vec<u8>, MuxError> {
    if i64::from(cts_ms).abs() > MAX_CTS + i64::from(cts_ms < 0) {
        return Err(MuxError::CompositionOffsetOutOfRange {
            ms: i64::from(cts_ms),
        });
    }
    let frame_type: u8 = if keyframe { 1 } else { 2 };
    let packet_type: u8 = match packet_type {
        AvcPacketType::SequenceHeader => 0,
        AvcPacketType::Nalu => 1,
        AvcPacketType::EndOfSequence => 2,
    };
    let cts = cts_ms.to_be_bytes();
    let mut body = Vec::with_capacity(5 + payload.len());
    body.extend_from_slice(&[
        (frame_type << 4) | VIDEO_CODEC_AVC,
        packet_type,
        cts[1],
        cts[2],
        cts[3],
    ]);
    body.extend_from_slice(payload);
    Ok(body)
}

/// The body of an enhanced RTMP video tag: the `IsExHeader` bit, the frame type and the
/// `VideoPacketType` in one byte, the FourCC, then the payload. For HEVC
/// [`ExVideoPacketType::CodedFrames`], `cts_ms` comes before the payload as a signed
/// 24-bit big-endian integer; every other tag has no composition time, so `cts_ms` must
/// be zero.
///
/// # Errors
/// [`MuxError::CompositionOffsetOutOfRange`] when `cts_ms` doesn't fit in 24 bits, and
/// [`MuxError::UnexpectedCompositionOffset`] when it is non-zero for a tag without one.
pub fn ex_video_tag_body(
    keyframe: bool,
    packet_type: ExVideoPacketType,
    fourcc: VideoFourCc,
    cts_ms: i32,
    payload: &[u8],
) -> Result<Vec<u8>, MuxError> {
    let has_cts = packet_type == ExVideoPacketType::CodedFrames && fourcc == VideoFourCc::Hevc;
    if !has_cts && cts_ms != 0 {
        return Err(MuxError::UnexpectedCompositionOffset {
            codec: fourcc.codec(),
            ms: i64::from(cts_ms),
        });
    }
    if i64::from(cts_ms).abs() > MAX_CTS + i64::from(cts_ms < 0) {
        return Err(MuxError::CompositionOffsetOutOfRange {
            ms: i64::from(cts_ms),
        });
    }
    Ok(ex_tag(
        keyframe,
        packet_type,
        fourcc,
        has_cts.then_some(cts_ms),
        payload,
    ))
}

/// [`ex_video_tag_body`] once `cts_ms` is checked: written only when `Some`.
fn ex_tag(
    keyframe: bool,
    packet_type: ExVideoPacketType,
    fourcc: VideoFourCc,
    cts_ms: Option<i32>,
    payload: &[u8],
) -> Vec<u8> {
    let frame_type: u8 = if keyframe { 1 } else { 2 };
    let packet_type: u8 = match packet_type {
        ExVideoPacketType::SequenceStart => 0,
        ExVideoPacketType::CodedFrames => 1,
        ExVideoPacketType::SequenceEnd => 2,
        ExVideoPacketType::CodedFramesX => 3,
    };
    let mut body = Vec::with_capacity(8 + payload.len());
    body.push(EX_HEADER | (frame_type << 4) | packet_type);
    body.extend_from_slice(&fourcc.bytes());
    if let Some(cts) = cts_ms {
        body.extend_from_slice(&cts.to_be_bytes()[1..]);
    }
    body.extend_from_slice(payload);
    body
}

/// The body of an AAC audio tag. The first byte is always `0xAF`: AAC, and the flags FLV
/// requires for AAC (44 kHz, 16-bit, stereo) whatever the real format, which the
/// `AudioSpecificConfig` carries.
pub fn audio_tag_body(packet_type: AacPacketType, payload: &[u8]) -> Vec<u8> {
    let packet_type: u8 = match packet_type {
        AacPacketType::SequenceHeader => 0,
        AacPacketType::Raw => 1,
    };
    let mut body = Vec::with_capacity(2 + payload.len());
    body.extend_from_slice(&[
        (SOUND_FORMAT_AAC << 4) | (3 << 2) | (1 << 1) | 1,
        packet_type,
    ]);
    body.extend_from_slice(payload);
    body
}

/// A millisecond timestamp as FLV and RTMP store it: modulo 2^32, so it wraps after about
/// 49.7 days.
pub fn wrap_timestamp(ms: i64) -> u32 {
    // rem_euclid keeps the value in 0..2^32, so the cast is exact.
    ms.rem_euclid(1 << 32) as u32
}

/// The video track: H.264, or with [`FlvTagger::with_enhanced_video`], HEVC or AV1.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoTrack {
    /// Coded width in pixels, for `onMetaData`.
    pub width: u32,
    /// Coded height in pixels, for `onMetaData`.
    pub height: u32,
    /// Frames per second, for `onMetaData`.
    pub frame_rate: Option<f64>,
    /// Target bitrate in kbit/s, for `onMetaData`.
    pub bitrate_kbps: Option<u32>,
    /// The H.264 decoder configuration. When `None`, it is built from the SPS and PPS of
    /// the first video packet, which must then be a keyframe carrying them in-band. An
    /// HEVC or AV1 track leaves it `None` and takes its configuration from
    /// [`ExVideoConfig`].
    pub avc: Option<AvcDecoderConfig>,
}

/// The codec and decoder configuration of an enhanced RTMP video track. A `None`
/// configuration is built from the first video packet, which must then be a keyframe
/// carrying it in-band: VPS, SPS and PPS for HEVC, a sequence header OBU for AV1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExVideoConfig {
    /// HEVC, with Annex B packets.
    Hevc(Option<HevcDecoderConfig>),
    /// AV1, with packets of low-overhead OBUs.
    Av1(Option<Av1CodecConfig>),
}

impl ExVideoConfig {
    /// The codec's FourCC.
    pub fn fourcc(&self) -> VideoFourCc {
        match self {
            Self::Hevc(_) => VideoFourCc::Hevc,
            Self::Av1(_) => VideoFourCc::Av1,
        }
    }

    /// The serialized decoder configuration record, if the configuration is known.
    pub fn record(&self) -> Option<Vec<u8>> {
        match self {
            Self::Hevc(c) => c.as_ref().map(HevcDecoderConfig::to_bytes),
            Self::Av1(c) => c.as_ref().map(Av1CodecConfig::to_bytes),
        }
    }

    fn is_known(&self) -> bool {
        matches!(self, Self::Hevc(Some(_)) | Self::Av1(Some(_)))
    }

    /// The same codec with the configuration taken from a keyframe.
    fn learn_from(&self, packet: &Packet) -> Result<Self, MuxError> {
        let codec = packet.codec();
        if !packet.is_keyframe() {
            return Err(MuxError::MissingDecoderConfig { codec });
        }
        // A keyframe without the parameter sets keeps the error that says which ones
        // were missing.
        match self {
            Self::Hevc(_) => {
                HevcDecoderConfig::from_annexb(packet.data()).map(|c| Self::Hevc(Some(c)))
            }
            Self::Av1(_) => Av1CodecConfig::from_obus(packet.data()).map(|c| Self::Av1(Some(c))),
        }
    }
}

/// The AAC track.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    /// The decoder configuration.
    pub config: AudioSpecificConfig,
    /// Target bitrate in kbit/s, for `onMetaData`.
    pub bitrate_kbps: Option<u32>,
}

/// How the `onMetaData` script tag is wrapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MetadataStyle {
    /// `"onMetaData", {…}`, as stored in FLV files.
    #[default]
    File,
    /// `"@setDataFrame", "onMetaData", {…}`, the data message an RTMP publisher sends;
    /// the server strips `@setDataFrame` and stores the rest. Use it only with a client
    /// that sends the tag body unchanged. `jstream_outputs`' `RtmpPublisher::send_metadata`
    /// adds `@setDataFrame` itself, so it takes [`MetadataStyle::File`].
    SetDataFrame,
}

/// What an FLV stream contains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlvConfig {
    /// The video track, if any.
    pub video: Option<VideoTrack>,
    /// The audio track, if any.
    pub audio: Option<AudioTrack>,
    /// The instant written as timestamp 0. `None` means zero: packet times are used as
    /// they are. Packets before the origin are rejected.
    pub origin: Option<MediaTime>,
    /// How `onMetaData` is wrapped.
    pub metadata_style: MetadataStyle,
    /// The `encoder` string in `onMetaData`, if any.
    pub encoder: Option<String>,
}

impl FlvConfig {
    /// The `onMetaData` ECMA array: video size, frame rate, bitrates, codec IDs and the
    /// audio rate, size and channel flag. Optional values are written only when set. The
    /// video codec ID is H.264's; [`FlvTagger`] writes the FourCC instead for an enhanced
    /// RTMP track.
    pub fn metadata(&self) -> Amf0Value {
        self.metadata_with(f64::from(VIDEO_CODEC_AVC))
    }

    fn metadata_with(&self, video_codec_id: f64) -> Amf0Value {
        let num = |key: &str, v: f64| (key.to_string(), Amf0Value::Number(v));
        let mut props = Vec::new();
        if let Some(v) = &self.video {
            props.push(num("width", f64::from(v.width)));
            props.push(num("height", f64::from(v.height)));
            if let Some(kbps) = v.bitrate_kbps {
                props.push(num("videodatarate", f64::from(kbps)));
            }
            if let Some(fps) = v.frame_rate {
                props.push(num("framerate", fps));
            }
            props.push(num("videocodecid", video_codec_id));
        }
        if let Some(a) = &self.audio {
            if let Some(kbps) = a.bitrate_kbps {
                props.push(num("audiodatarate", f64::from(kbps)));
            }
            props.push(num("audiosamplerate", f64::from(a.config.sample_rate())));
            props.push(num("audiosamplesize", 16.0));
            props.push((
                "stereo".to_string(),
                Amf0Value::Boolean(a.config.channels() == 2),
            ));
            props.push(num("audiocodecid", f64::from(SOUND_FORMAT_AAC)));
        }
        if let Some(encoder) = &self.encoder {
            props.push(("encoder".to_string(), Amf0Value::String(encoder.clone())));
        }
        Amf0Value::EcmaArray(props)
    }
}

/// Turns packets into FLV tags, with no I/O.
///
/// The first call to [`FlvTagger::start`] or [`FlvTagger::push`] yields the `onMetaData`
/// tag and the sequence headers; every packet then yields one tag. Timestamps are each
/// packet's dts, rescaled to milliseconds (rounded to nearest) from the origin; the
/// composition offset is the rounded pts minus the rounded dts.
///
/// H.264 and HEVC payloads are Annex B and are converted to 4-byte length prefixes; AV1
/// payloads lose their temporal delimiters and padding ([`av1::to_sample`]); audio
/// payloads are raw AAC.
#[derive(Debug)]
pub struct FlvTagger {
    config: FlvConfig,
    ex_video: Option<ExVideoConfig>,
    origin_ms: i64,
    started: bool,
    video_config_sent: bool,
    last_video_ms: Option<i64>,
    last_audio_ms: Option<i64>,
}

impl FlvTagger {
    /// A tagger for `config`, with an H.264 video track if it has one.
    ///
    /// # Errors
    /// [`MuxError::Media`] when the origin can't be expressed in milliseconds.
    pub fn new(config: FlvConfig) -> Result<Self, MuxError> {
        let origin_ms = match config.origin {
            Some(t) => to_ms(t)?,
            None => 0,
        };
        Ok(Self {
            config,
            ex_video: None,
            origin_ms,
            started: false,
            video_config_sent: false,
            last_video_ms: None,
            last_audio_ms: None,
        })
    }

    /// A tagger for `config` whose video track carries HEVC or AV1 through enhanced RTMP
    /// tags. The track's size, frame rate and bitrate come from `config.video`, whose
    /// `avc` must be `None`; the decoder configuration comes from `video`.
    ///
    /// # Errors
    /// [`MuxError::InvalidConfig`] when `config` has no video track or sets `avc`, and as
    /// [`FlvTagger::new`].
    pub fn with_enhanced_video(config: FlvConfig, video: ExVideoConfig) -> Result<Self, MuxError> {
        match &config.video {
            None => Err(MuxError::InvalidConfig {
                reason: "an enhanced video codec needs a video track",
            }),
            Some(track) if track.avc.is_some() => Err(MuxError::InvalidConfig {
                reason: "an HEVC or AV1 track can't have an AVC decoder configuration",
            }),
            Some(_) => Ok(Self {
                ex_video: Some(video),
                ..Self::new(config)?
            }),
        }
    }

    /// The configuration, including an AVC configuration taken from the stream.
    pub fn config(&self) -> &FlvConfig {
        &self.config
    }

    /// The enhanced RTMP video codec, including a decoder configuration taken from the
    /// stream; `None` for H.264 or no video.
    pub fn enhanced_video(&self) -> Option<&ExVideoConfig> {
        self.ex_video.as_ref()
    }

    /// The codec of the video track, if there is one.
    fn video_codec(&self) -> Option<Codec> {
        self.config.video.as_ref()?;
        Some(
            self.ex_video
                .as_ref()
                .map_or(Codec::H264, |v| v.fourcc().codec()),
        )
    }

    /// The `onMetaData` tag and every sequence header known so far, all at timestamp 0.
    /// Yields nothing after the first call (or the first [`FlvTagger::push`]).
    ///
    /// # Errors
    /// [`MuxError::TooLong`] if the metadata can't be encoded.
    pub fn start(&mut self) -> Result<Vec<FlvTag>, MuxError> {
        if self.started {
            return Ok(Vec::new());
        }
        let mut values = Vec::with_capacity(3);
        if self.config.metadata_style == MetadataStyle::SetDataFrame {
            values.push(Amf0Value::String("@setDataFrame".into()));
        }
        values.push(Amf0Value::String("onMetaData".into()));
        values.push(match &self.ex_video {
            Some(v) => self
                .config
                .metadata_with(f64::from(v.fourcc().metadata_id())),
            None => self.config.metadata(),
        });
        let mut tags = vec![FlvTag {
            tag_type: TagType::Script,
            timestamp: 0,
            body: Amf0Value::encode_all(&values)?,
        }];
        if let Some(tag) = self.video_sequence_header(0) {
            tags.push(tag);
            self.video_config_sent = true;
        }
        self.started = true;
        if let Some(audio) = &self.config.audio {
            tags.push(FlvTag {
                tag_type: TagType::Audio,
                timestamp: 0,
                body: audio_tag_body(AacPacketType::SequenceHeader, audio.config.as_bytes()),
            });
        }
        Ok(tags)
    }

    /// The tags for one packet: the start tags first if not yet yielded, then the video
    /// sequence header if it comes from this packet, then the packet's own tag.
    ///
    /// # Errors
    /// [`MuxError::NoTrack`] for a codec with no configured track,
    /// [`MuxError::NegativeTimestamp`] for a dts before the origin,
    /// [`MuxError::NonMonotonicDts`] when a track's dts goes backwards,
    /// [`MuxError::CompositionOffsetOutOfRange`], [`MuxError::MissingAvcConfig`] or
    /// [`MuxError::MissingDecoderConfig`] for a first video packet that is not a keyframe
    /// or lacks its decoder configuration ([`MuxError::MissingHevcParameterSets`] and
    /// [`MuxError::MissingAv1SequenceHeader`] for an HEVC or AV1 keyframe),
    /// [`MuxError::UnexpectedCompositionOffset`] for an AV1 packet whose
    /// pts and dts differ, the framing errors of [`MuxError::NotAnnexB`],
    /// [`MuxError::InvalidObu`], [`MuxError::InvalidHevcSps`],
    /// [`MuxError::InvalidAv1SequenceHeader`], and [`MuxError::Media`].
    pub fn push(&mut self, packet: &Packet) -> Result<Vec<FlvTag>, MuxError> {
        let codec = packet.codec();
        let video = codec.kind() == MediaKind::Video;
        let has_track = if video {
            self.video_codec() == Some(codec)
        } else {
            codec == Codec::Aac && self.config.audio.is_some()
        };
        if !has_track {
            return Err(MuxError::NoTrack { codec });
        }
        let dts = to_ms(packet.dts())?.saturating_sub(self.origin_ms);
        let pts = to_ms(packet.pts())?.saturating_sub(self.origin_ms);
        if dts < 0 {
            return Err(MuxError::NegativeTimestamp { ms: dts });
        }
        let last = if video {
            self.last_video_ms
        } else {
            self.last_audio_ms
        };
        if let Some(previous) = last.filter(|&p| dts < p) {
            return Err(MuxError::NonMonotonicDts {
                codec,
                previous,
                current: dts,
            });
        }
        // Rounding is monotonic and dts <= pts, so this is never negative.
        let cts = pts - dts;
        let cts = i32::try_from(cts)
            .ok()
            .filter(|&c| i64::from(c) <= MAX_CTS)
            .ok_or(MuxError::CompositionOffsetOutOfRange { ms: cts })?;
        let timestamp = wrap_timestamp(dts);

        // Everything fallible happens before any state changes, so a rejected packet
        // leaves the tagger as it was.
        let mut learned = Learned::Nothing;
        let tag = if video {
            if !self.video_config_sent {
                learned = self.config_from_packet(packet)?;
            }
            FlvTag {
                tag_type: TagType::Video,
                timestamp,
                body: self.video_body(packet, cts)?,
            }
        } else {
            FlvTag {
                tag_type: TagType::Audio,
                timestamp,
                body: audio_tag_body(AacPacketType::Raw, packet.data()),
            }
        };

        let mut tags = self.start()?;
        match learned {
            Learned::Nothing => {}
            Learned::Avc(avc) => {
                if let Some(video) = &mut self.config.video {
                    video.avc = Some(avc);
                }
            }
            Learned::Ex(ex) => self.ex_video = Some(ex),
        }
        if !self.video_config_sent && video {
            // The configuration was just learned, so a sequence header exists.
            if let Some(header) = self.video_sequence_header(timestamp) {
                tags.push(header);
                self.video_config_sent = true;
            }
        }
        if video {
            self.last_video_ms = Some(dts);
        } else {
            self.last_audio_ms = Some(dts);
        }
        tags.push(tag);
        Ok(tags)
    }

    /// The end-of-sequence tag, at the last video timestamp, if any video was written;
    /// otherwise nothing.
    pub fn finish(&mut self) -> Vec<FlvTag> {
        let Some(ms) = self.last_video_ms.filter(|_| self.video_config_sent) else {
            return Vec::new();
        };
        let body = match &self.ex_video {
            None => vec![(1 << 4) | VIDEO_CODEC_AVC, 2, 0, 0, 0],
            Some(v) => ex_tag(true, ExVideoPacketType::SequenceEnd, v.fourcc(), None, &[]),
        };
        vec![FlvTag {
            tag_type: TagType::Video,
            timestamp: wrap_timestamp(ms),
            body,
        }]
    }

    /// The decoder configuration a first video packet carries, when none is configured.
    fn config_from_packet(&self, packet: &Packet) -> Result<Learned, MuxError> {
        match &self.ex_video {
            Some(ex) if ex.is_known() => Ok(Learned::Nothing),
            Some(ex) => ex.learn_from(packet).map(Learned::Ex),
            None if self.config.video.as_ref().is_some_and(|v| v.avc.is_some()) => {
                Ok(Learned::Nothing)
            }
            None => Self::avc_from_packet(packet).map(Learned::Avc),
        }
    }

    /// The body of a video packet's tag.
    fn video_body(&self, packet: &Packet, cts: i32) -> Result<Vec<u8>, MuxError> {
        let key = packet.is_keyframe();
        match self.ex_video.as_ref().map(ExVideoConfig::fourcc) {
            None => video_tag_body(
                key,
                AvcPacketType::Nalu,
                cts,
                &annexb_to_avcc(packet.data())?,
            ),
            Some(fourcc @ VideoFourCc::Hevc) => {
                let packet_type = if cts == 0 {
                    ExVideoPacketType::CodedFramesX
                } else {
                    ExVideoPacketType::CodedFrames
                };
                let data = annexb_to_avcc(packet.data())?;
                ex_video_tag_body(key, packet_type, fourcc, cts, &data)
            }
            Some(fourcc @ VideoFourCc::Av1) => {
                let data = av1::to_sample(packet.data())?;
                ex_video_tag_body(key, ExVideoPacketType::CodedFrames, fourcc, cts, &data)
            }
        }
    }

    /// The video sequence header tag, if the decoder configuration is known.
    fn video_sequence_header(&self, timestamp: u32) -> Option<FlvTag> {
        let body = match &self.ex_video {
            None => avc_sequence_header(self.config.video.as_ref()?.avc.as_ref()?),
            Some(ex) => ex_tag(
                true,
                ExVideoPacketType::SequenceStart,
                ex.fourcc(),
                None,
                &ex.record()?,
            ),
        };
        Some(FlvTag {
            tag_type: TagType::Video,
            timestamp,
            body,
        })
    }

    fn avc_from_packet(packet: &Packet) -> Result<AvcDecoderConfig, MuxError> {
        if !packet.is_keyframe() {
            return Err(MuxError::MissingAvcConfig);
        }
        match AvcDecoderConfig::from_annexb(packet.data()) {
            Err(MuxError::MissingParameterSets { .. }) => Err(MuxError::MissingAvcConfig),
            other => other,
        }
    }
}

/// A decoder configuration [`FlvTagger::push`] took from a packet, applied once nothing
/// else can fail.
enum Learned {
    Nothing,
    Avc(AvcDecoderConfig),
    Ex(ExVideoConfig),
}

fn avc_sequence_header(avc: &AvcDecoderConfig) -> Vec<u8> {
    let record = avc.to_bytes();
    let mut body = Vec::with_capacity(5 + record.len());
    body.extend_from_slice(&[(1 << 4) | VIDEO_CODEC_AVC, 0, 0, 0, 0]);
    body.extend_from_slice(&record);
    body
}

/// A media time in whole milliseconds, rounded to the nearest. This is the one dts-to-ms
/// conversion: [`FlvTagger`] uses it for every timestamp, and the RTMP client's
/// `timestamp_ms` in `jstream-outputs` builds on it. The result may be negative; wrap it
/// with [`wrap_timestamp`] for an FLV or RTMP timestamp.
///
/// # Errors
/// [`MediaError`] when the time can't be expressed in `i64` milliseconds.
pub fn to_ms(t: MediaTime) -> Result<i64, MediaError> {
    Ok(t.rescale(Timebase::MILLIS, Rounding::Nearest)?.ticks())
}

/// Writes an FLV file (or stream) to any [`Write`]: the file header and start tags on
/// creation, a tag per packet, and the end-of-sequence tag on [`FlvWriter::finish`].
///
/// The header flags are set from the configured tracks. `duration` and `filesize` are
/// not written, because the output may be a live stream.
#[derive(Debug)]
pub struct FlvWriter<W: Write> {
    out: W,
    tagger: FlvTagger,
}

impl<W: Write> FlvWriter<W> {
    /// Writes the file header, `onMetaData` and the known sequence headers to `out`.
    /// An invalid configuration is rejected before anything is written.
    ///
    /// # Errors
    /// As [`FlvTagger::new`] and [`FlvTagger::start`], and [`MuxError::Io`].
    pub fn new(out: W, config: FlvConfig) -> Result<Self, MuxError> {
        Self::with_tagger(out, FlvTagger::new(config)?)
    }

    /// As [`FlvWriter::new`], with an HEVC or AV1 video track
    /// ([`FlvTagger::with_enhanced_video`]).
    ///
    /// # Errors
    /// As [`FlvTagger::with_enhanced_video`] and [`FlvTagger::start`], and
    /// [`MuxError::Io`].
    pub fn with_enhanced_video(
        out: W,
        config: FlvConfig,
        video: ExVideoConfig,
    ) -> Result<Self, MuxError> {
        Self::with_tagger(out, FlvTagger::with_enhanced_video(config, video)?)
    }

    fn with_tagger(mut out: W, mut tagger: FlvTagger) -> Result<Self, MuxError> {
        let config = tagger.config();
        let header = file_header(config.audio.is_some(), config.video.is_some());
        // Every fallible step that doesn't touch `out` comes first.
        let start = tagger.start()?;
        out.write_all(&header)?;
        for tag in start {
            tag.write_to(&mut out)?;
        }
        Ok(Self { out, tagger })
    }

    /// Writes the tags for one packet.
    ///
    /// # Errors
    /// As [`FlvTagger::push`], and [`MuxError::Io`].
    pub fn write_packet(&mut self, packet: &Packet) -> Result<(), MuxError> {
        for tag in self.tagger.push(packet)? {
            tag.write_to(&mut self.out)?;
        }
        Ok(())
    }

    /// The output.
    pub fn get_ref(&self) -> &W {
        &self.out
    }

    /// Writes the end-of-sequence tag, flushes, and returns the output.
    ///
    /// # Errors
    /// [`MuxError::Io`].
    pub fn finish(mut self) -> Result<W, MuxError> {
        for tag in self.tagger.finish() {
            tag.write_to(&mut self.out)?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use jstream_media::{Codec, MediaTime, Packet, Timebase};

    use super::*;
    use crate::av1::tests::{
        SEQUENCE_HEADER_444 as AV1_SEQUENCE_HEADER, keyframe_tu as av1_keyframe,
    };
    use crate::h264::tests::{PPS, SPS};
    use crate::hevc::tests::{PPS as HEVC_PPS, VPS as HEVC_VPS, idr_annexb as hevc_idr};

    fn ms(t: i64) -> MediaTime {
        MediaTime::new(t, Timebase::MILLIS)
    }

    fn k90(t: i64) -> MediaTime {
        MediaTime::new(t, Timebase::MPEG_90K)
    }

    /// An IDR access unit carrying SPS and PPS in-band, as Media Foundation emits it.
    fn idr_annexb() -> Vec<u8> {
        let mut au = Vec::new();
        for nalu in [
            &SPS[..],
            &PPS[..],
            &[0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01],
        ] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        au
    }

    fn video_track(avc: Option<AvcDecoderConfig>) -> VideoTrack {
        VideoTrack {
            width: 1280,
            height: 720,
            frame_rate: None,
            bitrate_kbps: None,
            avc,
        }
    }

    fn audio_track() -> AudioTrack {
        AudioTrack {
            config: AudioSpecificConfig::aac_lc(48_000, 2).unwrap(),
            bitrate_kbps: None,
        }
    }

    #[test]
    fn file_header_is_byte_exact() {
        assert_eq!(
            file_header(true, true),
            [0x46, 0x4C, 0x56, 0x01, 0x05, 0, 0, 0, 0x09, 0, 0, 0, 0]
        );
        assert_eq!(file_header(false, true)[4], 0x01);
        assert_eq!(file_header(true, false)[4], 0x04);
    }

    #[test]
    fn tag_framing_is_byte_exact() {
        let tag = FlvTag {
            tag_type: TagType::Audio,
            timestamp: 0,
            body: audio_tag_body(AacPacketType::SequenceHeader, &[0x11, 0x90]),
        };
        let mut out = Vec::new();
        tag.write_to(&mut out).unwrap();
        assert_eq!(
            out,
            [
                0x08, 0x00, 0x00, 0x04, // audio, size 4
                0x00, 0x00, 0x00, 0x00, // timestamp, extended
                0x00, 0x00, 0x00, // stream id
                0xAF, 0x00, 0x11, 0x90, // AAC sequence header
                0x00, 0x00, 0x00, 0x0F, // PreviousTagSize = 11 + 4
            ]
        );
    }

    #[test]
    fn extended_timestamp_byte_holds_the_high_bits() {
        let tag = FlvTag {
            tag_type: TagType::Video,
            timestamp: 0x0123_4567,
            body: vec![0x27, 0x01, 0, 0, 0, 0xAA],
        };
        assert_eq!(
            tag.header().unwrap(),
            [0x09, 0x00, 0x00, 0x06, 0x23, 0x45, 0x67, 0x01, 0, 0, 0]
        );
        // 2^24 ms (about 4.66 hours) is the first time the extended byte is used
        let tag = FlvTag {
            timestamp: 0x0100_0000,
            ..tag
        };
        assert_eq!(&tag.header().unwrap()[4..8], &[0x00, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn timestamps_wrap_at_32_bits() {
        assert_eq!(wrap_timestamp(0), 0);
        assert_eq!(wrap_timestamp(0xFFFF_FFFF), 0xFFFF_FFFF);
        assert_eq!(wrap_timestamp(1 << 32), 0);
        assert_eq!(wrap_timestamp((1 << 32) + 5), 5);

        // Through the tagger: an audio packet 2^32 + 5 ms into the stream
        let mut tagger = FlvTagger::new(FlvConfig {
            audio: Some(audio_track()),
            ..FlvConfig::default()
        })
        .unwrap();
        let at = (1 << 32) + 5;
        let p = Packet::new(Codec::Aac, true, ms(at), ms(at), vec![0x21]).unwrap();
        let tags = tagger.push(&p).unwrap();
        let last = tags.last().unwrap();
        assert_eq!(last.timestamp, 5);
        assert_eq!(&last.header().unwrap()[4..8], &[0, 0, 5, 0]);
    }

    #[test]
    fn video_tag_body_is_byte_exact() {
        assert_eq!(
            video_tag_body(true, AvcPacketType::Nalu, 0x0102, &[0xAB]).unwrap(),
            [0x17, 0x01, 0x00, 0x01, 0x02, 0xAB]
        );
        assert_eq!(
            video_tag_body(false, AvcPacketType::Nalu, -1, &[]).unwrap(),
            [0x27, 0x01, 0xFF, 0xFF, 0xFF]
        );
        assert!(video_tag_body(false, AvcPacketType::Nalu, 0x7F_FFFF, &[]).is_ok());
        assert!(video_tag_body(false, AvcPacketType::Nalu, -0x80_0000, &[]).is_ok());
        assert!(matches!(
            video_tag_body(false, AvcPacketType::Nalu, 0x80_0000, &[]),
            Err(MuxError::CompositionOffsetOutOfRange { ms: 0x80_0000 })
        ));
        assert!(video_tag_body(false, AvcPacketType::Nalu, -0x80_0001, &[]).is_err());
    }

    #[test]
    fn audio_tag_body_is_byte_exact() {
        assert_eq!(
            audio_tag_body(AacPacketType::Raw, &[0x21, 0x10]),
            [0xAF, 0x01, 0x21, 0x10]
        );
    }

    #[test]
    fn metadata_is_byte_exact() {
        let config = FlvConfig {
            video: Some(video_track(None)),
            ..FlvConfig::default()
        };
        let mut tagger = FlvTagger::new(config).unwrap();
        let tags = tagger.start().unwrap();
        assert_eq!(tags.len(), 1, "no sequence header without an AVC config");
        let mut expected = vec![0x02, 0x00, 0x0A];
        expected.extend_from_slice(b"onMetaData");
        expected.extend_from_slice(&[0x08, 0x00, 0x00, 0x00, 0x03]);
        expected.extend_from_slice(&[0x00, 0x05]);
        expected.extend_from_slice(b"width");
        expected.extend_from_slice(&[0x00, 0x40, 0x94, 0, 0, 0, 0, 0, 0]);
        expected.extend_from_slice(&[0x00, 0x06]);
        expected.extend_from_slice(b"height");
        expected.extend_from_slice(&[0x00, 0x40, 0x86, 0x80, 0, 0, 0, 0, 0]);
        expected.extend_from_slice(&[0x00, 0x0C]);
        expected.extend_from_slice(b"videocodecid");
        expected.extend_from_slice(&[0x00, 0x40, 0x1C, 0, 0, 0, 0, 0, 0]);
        expected.extend_from_slice(&[0x00, 0x00, 0x09]);
        assert_eq!(expected.len(), 77);
        assert_eq!(tags[0].tag_type, TagType::Script);
        assert_eq!(tags[0].body, expected);
        assert_eq!(
            tags[0].header().unwrap(),
            [0x12, 0x00, 0x00, 0x4D, 0, 0, 0, 0, 0, 0, 0]
        );
        assert!(tagger.start().unwrap().is_empty(), "start yields once");
    }

    #[test]
    fn full_metadata_and_set_data_frame() {
        let config = FlvConfig {
            video: Some(VideoTrack {
                frame_rate: Some(30.0),
                bitrate_kbps: Some(2500),
                ..video_track(None)
            }),
            audio: Some(AudioTrack {
                bitrate_kbps: Some(128),
                ..audio_track()
            }),
            metadata_style: MetadataStyle::SetDataFrame,
            encoder: Some("JStream".into()),
            ..FlvConfig::default()
        };
        let meta = config.metadata();
        let keys: Vec<_> = match &meta {
            Amf0Value::EcmaArray(props) => props.iter().map(|(k, _)| k.as_str()).collect(),
            other => panic!("not an ECMA array: {other:?}"),
        };
        assert_eq!(
            keys,
            [
                "width",
                "height",
                "videodatarate",
                "framerate",
                "videocodecid",
                "audiodatarate",
                "audiosamplerate",
                "audiosamplesize",
                "stereo",
                "audiocodecid",
                "encoder"
            ]
        );
        assert_eq!(
            meta.property("audiosamplerate"),
            Some(&Amf0Value::Number(48_000.0))
        );
        assert_eq!(
            meta.property("audiocodecid"),
            Some(&Amf0Value::Number(10.0))
        );
        assert_eq!(meta.property("stereo"), Some(&Amf0Value::Boolean(true)));

        let tags = FlvTagger::new(config).unwrap().start().unwrap();
        let values = Amf0Value::decode_all(&tags[0].body).unwrap();
        assert_eq!(
            &values[..2],
            &[
                Amf0Value::String("@setDataFrame".into()),
                Amf0Value::String("onMetaData".into())
            ]
        );
        assert_eq!(values[2], meta);
        // audio sequence header follows the metadata
        assert_eq!(tags[1].body, [0xAF, 0x00, 0x11, 0x90]);
    }

    #[test]
    fn sequence_headers_from_config() {
        let avc = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap();
        let config = FlvConfig {
            video: Some(video_track(Some(avc.clone()))),
            audio: Some(audio_track()),
            ..FlvConfig::default()
        };
        let tags = FlvTagger::new(config).unwrap().start().unwrap();
        assert_eq!(tags.len(), 3);
        let mut seq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
        seq.extend_from_slice(&avc.to_bytes());
        assert_eq!(tags[1].tag_type, TagType::Video);
        assert_eq!(tags[1].body, seq);
        assert_eq!(tags[2].tag_type, TagType::Audio);
    }

    #[test]
    fn avc_config_is_taken_from_the_first_keyframe() {
        let mut tagger = FlvTagger::new(FlvConfig {
            video: Some(video_track(None)),
            ..FlvConfig::default()
        })
        .unwrap();
        let p = Packet::new(Codec::H264, true, ms(40), ms(40), idr_annexb()).unwrap();
        let tags = tagger.push(&p).unwrap();
        assert_eq!(tags.len(), 3, "metadata, sequence header, frame");
        let avc = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap();
        assert_eq!(&tags[1].body[5..], &avc.to_bytes()[..]);
        assert_eq!(tags[1].timestamp, 40);
        assert_eq!(tagger.config().video.as_ref().unwrap().avc, Some(avc));

        let mut frame = vec![0x17, 0x01, 0, 0, 0];
        frame.extend_from_slice(&annexb_to_avcc(&idr_annexb()).unwrap());
        assert_eq!(tags[2].body, frame);
        assert_eq!(tags[2].timestamp, 40);

        let p = Packet::new(
            Codec::H264,
            false,
            ms(80),
            ms(80),
            vec![0, 0, 1, 0x41, 0x9A],
        )
        .unwrap();
        let tags = tagger.push(&p).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].body, [0x27, 0x01, 0, 0, 0, 0, 0, 0, 2, 0x41, 0x9A]);

        let end = tagger.finish();
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].body, [0x17, 0x02, 0, 0, 0]);
        assert_eq!(end[0].timestamp, 80);
    }

    #[test]
    fn video_without_parameter_sets_is_an_error() {
        let mut tagger = FlvTagger::new(FlvConfig {
            video: Some(video_track(None)),
            ..FlvConfig::default()
        })
        .unwrap();
        let inter = Packet::new(Codec::H264, false, ms(0), ms(0), vec![0, 0, 1, 0x41]).unwrap();
        assert!(matches!(
            tagger.push(&inter),
            Err(MuxError::MissingAvcConfig)
        ));
        let bare_idr = Packet::new(Codec::H264, true, ms(0), ms(0), vec![0, 0, 1, 0x65]).unwrap();
        assert!(matches!(
            tagger.push(&bare_idr),
            Err(MuxError::MissingAvcConfig)
        ));
        // A rejected packet changes nothing: the start tags still come with the first
        // packet that is accepted.
        let idr = Packet::new(Codec::H264, true, ms(0), ms(0), idr_annexb()).unwrap();
        assert_eq!(tagger.push(&idr).unwrap().len(), 3);
    }

    #[test]
    fn b_frame_composition_offset() {
        // Decode order I P B at 30000/1001 fps in a 90 kHz time base, with a one-frame
        // reorder delay: dts = n * 3003 - 3003, pts by display position.
        let avc = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap();
        let mut tagger = FlvTagger::new(FlvConfig {
            video: Some(video_track(Some(avc))),
            origin: Some(k90(-3003)),
            ..FlvConfig::default()
        })
        .unwrap();
        tagger.start().unwrap();
        let frames = [
            // (keyframe, pts, dts) in 90 kHz ticks
            (true, 0, -3003),
            (false, 9009, 0),
            (false, 3003, 3003),
        ];
        let mut out = Vec::new();
        for (key, pts, dts) in frames {
            let p = Packet::new(Codec::H264, key, k90(pts), k90(dts), vec![0, 0, 1, 0x41]).unwrap();
            let tags = tagger.push(&p).unwrap();
            out.push((tags[0].timestamp, tags[0].body[..5].to_vec()));
        }
        // origin -33.37 ms rounds to -33; dts -33 -> 0, 0 -> 33, 33.37 -> 33 + 33 = 66
        // I: pts 0 - dts -33 = 33 ms (0x21); P: 100.1 -> 100 - 0 = 100 (0x64); B: 0
        assert_eq!(
            out,
            [
                (0, vec![0x17, 0x01, 0x00, 0x00, 0x21]),
                (33, vec![0x27, 0x01, 0x00, 0x00, 0x64]),
                (66, vec![0x27, 0x01, 0x00, 0x00, 0x00]),
            ]
        );
    }

    #[test]
    fn writer_rejects_a_bad_origin_before_writing() {
        // i64::MAX seconds overflows i64 milliseconds, so the origin is rejected.
        let seconds = Timebase::new(1, 1).unwrap();
        let mut out = Vec::new();
        let result = FlvWriter::new(
            &mut out,
            FlvConfig {
                audio: Some(audio_track()),
                origin: Some(MediaTime::new(i64::MAX, seconds)),
                ..FlvConfig::default()
            },
        );
        assert!(matches!(result, Err(MuxError::Media(_))), "{result:?}");
        assert!(out.is_empty());
    }

    #[test]
    fn timestamp_errors() {
        let mut tagger = FlvTagger::new(FlvConfig {
            audio: Some(audio_track()),
            origin: Some(ms(1_000)),
            ..FlvConfig::default()
        })
        .unwrap();
        let aac = |t| Packet::new(Codec::Aac, true, ms(t), ms(t), vec![0x21]).unwrap();
        assert!(matches!(
            tagger.push(&aac(999)),
            Err(MuxError::NegativeTimestamp { ms: -1 })
        ));
        assert_eq!(
            tagger.push(&aac(1_021)).unwrap().last().unwrap().timestamp,
            21
        );
        assert!(matches!(
            tagger.push(&aac(1_020)),
            Err(MuxError::NonMonotonicDts {
                codec: Codec::Aac,
                previous: 21,
                current: 20
            })
        ));
        assert!(tagger.push(&aac(1_021)).is_ok(), "equal dts is allowed");

        let p = Packet::new(Codec::H264, true, ms(2_000), ms(2_000), vec![1]).unwrap();
        assert!(matches!(
            tagger.push(&p),
            Err(MuxError::NoTrack { codec: Codec::H264 })
        ));
        assert!(tagger.finish().is_empty(), "no video, no end of sequence");

        // pts - dts beyond the SI24 range
        let mut tagger = FlvTagger::new(FlvConfig {
            video: Some(video_track(None)),
            ..FlvConfig::default()
        })
        .unwrap();
        let p = Packet::new(Codec::H264, true, ms(0x80_0000), ms(0), idr_annexb()).unwrap();
        assert!(matches!(
            tagger.push(&p),
            Err(MuxError::CompositionOffsetOutOfRange { ms: 0x80_0000 })
        ));
    }

    /// Writes a tiny FLV (one IDR frame, one AAC frame) and walks its tag structure.
    ///
    /// To inspect it with real tools, write `bytes` to a file (e.g. `std::fs::write`) and
    /// run `ffprobe -show_streams -show_packets tiny.flv` locally: ffprobe should report
    /// an h264 stream (1280x720, High) and an aac stream (48000 Hz, stereo). The payloads
    /// here are not decodable pictures or audio, so only the container is meaningful. CI
    /// does not depend on ffmpeg.
    #[test]
    fn writes_a_tiny_flv() {
        let config = FlvConfig {
            video: Some(video_track(None)),
            audio: Some(audio_track()),
            ..FlvConfig::default()
        };
        let mut w = FlvWriter::new(Vec::new(), config).unwrap();
        w.write_packet(&Packet::new(Codec::H264, true, ms(0), ms(0), idr_annexb()).unwrap())
            .unwrap();
        w.write_packet(
            &Packet::new(Codec::Aac, true, ms(0), ms(0), vec![0x21, 0x10, 0x04]).unwrap(),
        )
        .unwrap();
        let bytes = w.finish().unwrap();

        assert_eq!(&bytes[..13], &file_header(true, true));
        let mut pos = 13;
        let mut kinds = Vec::new();
        while pos < bytes.len() {
            let size = usize::from(bytes[pos + 1]) << 16
                | usize::from(bytes[pos + 2]) << 8
                | usize::from(bytes[pos + 3]);
            let end = pos + 11 + size;
            let prev = u32::from_be_bytes(bytes[end..end + 4].try_into().unwrap());
            assert_eq!(prev as usize, 11 + size);
            kinds.push((bytes[pos], bytes[pos + 11], bytes[pos + 12]));
            pos = end + 4;
        }
        assert_eq!(pos, bytes.len());
        assert_eq!(
            kinds,
            [
                (18, 0x02, 0x00), // onMetaData (AMF0 string marker, length high byte)
                (8, 0xAF, 0x00),  // AAC sequence header
                (9, 0x17, 0x00),  // AVC sequence header, taken from the keyframe
                (9, 0x17, 0x01),  // IDR frame
                (8, 0xAF, 0x01),  // AAC frame
                (9, 0x17, 0x02),  // end of sequence
            ]
        );
    }

    #[test]
    fn fourcc_ids() {
        assert_eq!(VideoFourCc::Hevc.bytes(), *b"hvc1");
        assert_eq!(VideoFourCc::Av1.bytes(), *b"av01");
        // The spec's example: "av01" == 0x61763031 == 1635135537.0
        assert_eq!(VideoFourCc::Av1.metadata_id(), 0x6176_3031);
        assert_eq!(VideoFourCc::Hevc.metadata_id(), 0x6876_6331);
        assert_eq!(VideoFourCc::Hevc.codec(), Codec::Hevc);
        assert_eq!(VideoFourCc::Av1.codec(), Codec::Av1);
    }

    #[test]
    fn ex_video_tag_body_is_byte_exact() {
        use ExVideoPacketType::{CodedFrames, CodedFramesX, SequenceEnd, SequenceStart};
        let hevc = VideoFourCc::Hevc;
        let av1 = VideoFourCc::Av1;
        assert_eq!(
            ex_video_tag_body(true, SequenceStart, hevc, 0, &[0x01]).unwrap(),
            [0x90, b'h', b'v', b'c', b'1', 0x01]
        );
        assert_eq!(
            ex_video_tag_body(true, CodedFrames, hevc, 0x0102, &[0xAB]).unwrap(),
            [0x91, b'h', b'v', b'c', b'1', 0x00, 0x01, 0x02, 0xAB]
        );
        assert_eq!(
            ex_video_tag_body(false, CodedFrames, hevc, -1, &[]).unwrap(),
            [0xA1, b'h', b'v', b'c', b'1', 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(
            ex_video_tag_body(false, CodedFramesX, hevc, 0, &[0xAB]).unwrap(),
            [0xA3, b'h', b'v', b'c', b'1', 0xAB]
        );
        assert_eq!(
            ex_video_tag_body(true, SequenceEnd, av1, 0, &[]).unwrap(),
            [0x92, b'a', b'v', b'0', b'1']
        );
        assert_eq!(
            ex_video_tag_body(false, CodedFrames, av1, 0, &[0x32, 0x00]).unwrap(),
            [0xA1, b'a', b'v', b'0', b'1', 0x32, 0x00],
            "AV1 has no composition time"
        );
        assert!(matches!(
            ex_video_tag_body(false, CodedFrames, av1, 33, &[]),
            Err(MuxError::UnexpectedCompositionOffset {
                codec: Codec::Av1,
                ms: 33
            })
        ));
        assert!(matches!(
            ex_video_tag_body(false, CodedFramesX, hevc, 1, &[]),
            Err(MuxError::UnexpectedCompositionOffset { .. })
        ));
        assert!(matches!(
            ex_video_tag_body(false, CodedFrames, hevc, 0x80_0000, &[]),
            Err(MuxError::CompositionOffsetOutOfRange { ms: 0x80_0000 })
        ));
        assert!(ex_video_tag_body(false, CodedFrames, hevc, -0x80_0000, &[]).is_ok());
    }

    fn hevc_tagger(config: Option<HevcDecoderConfig>) -> FlvTagger {
        FlvTagger::with_enhanced_video(
            FlvConfig {
                video: Some(video_track(None)),
                audio: Some(audio_track()),
                ..FlvConfig::default()
            },
            ExVideoConfig::Hevc(config),
        )
        .unwrap()
    }

    #[test]
    fn hevc_config_is_taken_from_the_first_keyframe() {
        let mut tagger = hevc_tagger(None);
        let start = tagger.start().unwrap();
        assert_eq!(
            start.len(),
            2,
            "metadata and AAC header; HEVC waits for a keyframe"
        );
        let meta = Amf0Value::decode_all(&start[0].body).unwrap();
        assert_eq!(
            meta[1].property("videocodecid"),
            Some(&Amf0Value::Number(f64::from(0x6876_6331u32)))
        );

        let p = Packet::new(Codec::Hevc, true, ms(40), ms(40), hevc_idr()).unwrap();
        let tags = tagger.push(&p).unwrap();
        assert_eq!(tags.len(), 2, "sequence start, frame");
        let config = HevcDecoderConfig::from_annexb(&hevc_idr()).unwrap();
        let mut seq = vec![0x90, b'h', b'v', b'c', b'1'];
        seq.extend_from_slice(&config.to_bytes());
        assert_eq!((tags[0].tag_type, tags[0].timestamp), (TagType::Video, 40));
        assert_eq!(tags[0].body, seq);
        assert_eq!(
            tagger.enhanced_video(),
            Some(&ExVideoConfig::Hevc(Some(config)))
        );

        // pts == dts: CodedFramesX, no composition time
        let mut frame = vec![0x93, b'h', b'v', b'c', b'1'];
        frame.extend_from_slice(&annexb_to_avcc(&hevc_idr()).unwrap());
        assert_eq!(tags[1].body, frame);

        // A B-frame reference: pts 33 ms after dts, so CodedFrames with the offset
        let p = Packet::new(
            Codec::Hevc,
            false,
            ms(113),
            ms(80),
            vec![0, 0, 1, 0x02, 0x01, 0xD0],
        )
        .unwrap();
        let tags = tagger.push(&p).unwrap();
        assert_eq!(
            tags[0].body,
            [
                0xA1, b'h', b'v', b'c', b'1', 0, 0, 33, 0, 0, 0, 3, 0x02, 0x01, 0xD0
            ]
        );

        let end = tagger.finish();
        assert_eq!(end[0].body, [0x92, b'h', b'v', b'c', b'1']);
        assert_eq!(end[0].timestamp, 80);
    }

    #[test]
    fn configured_hevc_header_comes_first() {
        let config = HevcDecoderConfig::from_annexb(&hevc_idr()).unwrap();
        let mut tagger = hevc_tagger(Some(config.clone()));
        let tags = tagger.start().unwrap();
        assert_eq!(tags.len(), 3);
        assert_eq!(&tags[1].body[..5], &[0x90, b'h', b'v', b'c', b'1']);
        assert_eq!(&tags[1].body[5..], &config.to_bytes()[..]);
        assert_eq!(tags[2].tag_type, TagType::Audio);

        // A keyframe without in-band parameter sets is fine once configured.
        let p = Packet::new(Codec::Hevc, true, ms(0), ms(0), vec![0, 0, 1, 0x28, 0x01]).unwrap();
        assert_eq!(tagger.push(&p).unwrap().len(), 1);
    }

    #[test]
    fn enhanced_video_errors() {
        let no_video =
            FlvTagger::with_enhanced_video(FlvConfig::default(), ExVideoConfig::Av1(None));
        assert!(matches!(no_video, Err(MuxError::InvalidConfig { .. })));
        let avc = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap();
        let both = FlvTagger::with_enhanced_video(
            FlvConfig {
                video: Some(video_track(Some(avc))),
                ..FlvConfig::default()
            },
            ExVideoConfig::Hevc(None),
        );
        assert!(matches!(both, Err(MuxError::InvalidConfig { .. })));

        let mut tagger = hevc_tagger(None);
        let h264 = Packet::new(Codec::H264, true, ms(0), ms(0), idr_annexb()).unwrap();
        assert!(matches!(
            tagger.push(&h264),
            Err(MuxError::NoTrack { codec: Codec::H264 })
        ));
        let av1 = Packet::new(Codec::Av1, true, ms(0), ms(0), av1_keyframe()).unwrap();
        assert!(matches!(
            tagger.push(&av1),
            Err(MuxError::NoTrack { codec: Codec::Av1 })
        ));
        let inter =
            Packet::new(Codec::Hevc, false, ms(0), ms(0), vec![0, 0, 1, 0x02, 0x01]).unwrap();
        assert!(matches!(
            tagger.push(&inter),
            Err(MuxError::MissingDecoderConfig { codec: Codec::Hevc })
        ));
        let bare = Packet::new(Codec::Hevc, true, ms(0), ms(0), vec![0, 0, 1, 0x28, 0x01]).unwrap();
        assert!(matches!(
            tagger.push(&bare),
            Err(MuxError::MissingHevcParameterSets {
                vps: 0,
                sps: 0,
                pps: 0
            })
        ));
        let bad_sps = Packet::new(Codec::Hevc, true, ms(0), ms(0), {
            let mut au = vec![0, 0, 1];
            au.extend_from_slice(&HEVC_VPS);
            au.extend_from_slice(&[0, 0, 1, 0x42, 0x01, 0x01]);
            au.extend_from_slice(&[0, 0, 1]);
            au.extend_from_slice(&HEVC_PPS);
            au
        })
        .unwrap();
        assert!(matches!(
            tagger.push(&bad_sps),
            Err(MuxError::InvalidHevcSps { .. })
        ));
        // A rejected packet changes nothing: the start tags still come with the first
        // packet that is accepted.
        let idr = Packet::new(Codec::Hevc, true, ms(0), ms(0), hevc_idr()).unwrap();
        assert_eq!(tagger.push(&idr).unwrap().len(), 4);
        // H.264's tagger refuses HEVC packets.
        let mut h264_tagger = FlvTagger::new(FlvConfig {
            video: Some(video_track(None)),
            ..FlvConfig::default()
        })
        .unwrap();
        assert!(matches!(
            h264_tagger.push(&idr),
            Err(MuxError::NoTrack { codec: Codec::Hevc })
        ));
    }

    #[test]
    fn av1_config_is_taken_from_the_first_keyframe() {
        let mut tagger = FlvTagger::with_enhanced_video(
            FlvConfig {
                video: Some(video_track(None)),
                ..FlvConfig::default()
            },
            ExVideoConfig::Av1(None),
        )
        .unwrap();
        let p = Packet::new(Codec::Av1, true, ms(0), ms(0), av1_keyframe()).unwrap();
        let tags = tagger.push(&p).unwrap();
        assert_eq!(tags.len(), 3, "metadata, sequence start, frame");
        let meta = Amf0Value::decode_all(&tags[0].body).unwrap();
        assert_eq!(
            meta[1].property("videocodecid"),
            Some(&Amf0Value::Number(1_635_135_537.0))
        );
        let mut seq = vec![0x90, b'a', b'v', b'0', b'1', 0x81, 0x20, 0x00, 0x00];
        seq.extend_from_slice(&AV1_SEQUENCE_HEADER);
        assert_eq!(tags[1].body, seq);
        // The temporal delimiter is dropped; the sequence header stays in the frame.
        let mut frame = vec![0x91, b'a', b'v', b'0', b'1'];
        frame.extend_from_slice(&av1_keyframe()[2..]);
        assert_eq!(tags[2].body, frame);

        let p = Packet::new(
            Codec::Av1,
            false,
            ms(33),
            ms(33),
            vec![0x12, 0x00, 0x32, 0x01, 0xEE],
        )
        .unwrap();
        assert_eq!(
            tagger.push(&p).unwrap()[0].body,
            [0xA1, b'a', b'v', b'0', b'1', 0x32, 0x01, 0xEE]
        );
        let reordered =
            Packet::new(Codec::Av1, false, ms(100), ms(66), vec![0x32, 0x01, 0xEE]).unwrap();
        assert!(matches!(
            tagger.push(&reordered),
            Err(MuxError::UnexpectedCompositionOffset {
                codec: Codec::Av1,
                ms: 34
            })
        ));
        let broken = Packet::new(Codec::Av1, false, ms(66), ms(66), vec![0x32, 0x09]).unwrap();
        assert!(matches!(
            tagger.push(&broken),
            Err(MuxError::InvalidObu { .. })
        ));
        assert_eq!(
            tagger.finish()[0].body,
            [0x92, b'a', b'v', b'0', b'1'],
            "end of sequence at the last accepted frame"
        );
        assert_eq!(tagger.finish()[0].timestamp, 33);

        let mut tagger = FlvTagger::with_enhanced_video(
            FlvConfig {
                video: Some(video_track(None)),
                ..FlvConfig::default()
            },
            ExVideoConfig::Av1(None),
        )
        .unwrap();
        let no_header = Packet::new(Codec::Av1, true, ms(0), ms(0), vec![0x32, 0x00]).unwrap();
        assert!(matches!(
            tagger.push(&no_header),
            Err(MuxError::MissingAv1SequenceHeader)
        ));
        let inter = Packet::new(Codec::Av1, false, ms(0), ms(0), av1_keyframe()).unwrap();
        assert!(matches!(
            tagger.push(&inter),
            Err(MuxError::MissingDecoderConfig { codec: Codec::Av1 })
        ));
        assert!(tagger.finish().is_empty(), "no video, no end of sequence");
    }

    /// Writes a tiny HEVC + AAC FLV and walks its tag structure, as
    /// [`writes_a_tiny_flv`] does for H.264. `ffprobe` reads such a file as an hevc
    /// stream (FFmpeg 6.1 and later read enhanced FLV); CI does not depend on it.
    #[test]
    fn writes_a_tiny_enhanced_flv() {
        let config = FlvConfig {
            video: Some(video_track(None)),
            audio: Some(audio_track()),
            ..FlvConfig::default()
        };
        let mut w =
            FlvWriter::with_enhanced_video(Vec::new(), config, ExVideoConfig::Hevc(None)).unwrap();
        w.write_packet(&Packet::new(Codec::Hevc, true, ms(0), ms(0), hevc_idr()).unwrap())
            .unwrap();
        w.write_packet(
            &Packet::new(Codec::Aac, true, ms(0), ms(0), vec![0x21, 0x10, 0x04]).unwrap(),
        )
        .unwrap();
        let bytes = w.finish().unwrap();

        assert_eq!(&bytes[..13], &file_header(true, true));
        let mut pos = 13;
        let mut kinds = Vec::new();
        while pos < bytes.len() {
            let size = usize::from(bytes[pos + 1]) << 16
                | usize::from(bytes[pos + 2]) << 8
                | usize::from(bytes[pos + 3]);
            let end = pos + 11 + size;
            let prev = u32::from_be_bytes(bytes[end..end + 4].try_into().unwrap());
            assert_eq!(prev as usize, 11 + size);
            kinds.push((bytes[pos], bytes[pos + 11]));
            pos = end + 4;
        }
        assert_eq!(pos, bytes.len());
        assert_eq!(
            kinds,
            [
                (18, 0x02), // onMetaData
                (8, 0xAF),  // AAC sequence header
                (9, 0x90),  // HEVC sequence start, taken from the keyframe
                (9, 0x93),  // IDR frame, CodedFramesX
                (8, 0xAF),  // AAC frame
                (9, 0x92),  // sequence end
            ]
        );
    }
}
