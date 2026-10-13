//! FLV: the container RTMP carries. [`FlvTagger`] turns packets into tags with no I/O,
//! for the RTMP client to send as messages; [`FlvWriter`] adds the file header and tag
//! framing and writes to any [`std::io::Write`].
//!
//! Only legacy FLV codecs are written: H.264 (`CodecID` 7) and AAC (`SoundFormat` 10).
//! Enhanced RTMP (HEVC and AV1 through FourCC video tags) is not implemented yet.

use std::io::Write;

use jstream_media::{Codec, MediaError, MediaTime, Packet, Rounding, Timebase};

use crate::MuxError;
use crate::aac::AudioSpecificConfig;
use crate::amf0::Amf0Value;
use crate::h264::{AvcDecoderConfig, annexb_to_avcc};

/// FLV `CodecID` of AVC (H.264).
const VIDEO_CODEC_AVC: u8 = 7;
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

/// The H.264 track.
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
    /// The decoder configuration. When `None`, it is built from the SPS and PPS of the
    /// first video packet, which must then be a keyframe carrying them in-band.
    pub avc: Option<AvcDecoderConfig>,
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
    /// audio rate, size and channel flag. Optional values are written only when set.
    pub fn metadata(&self) -> Amf0Value {
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
            props.push(num("videocodecid", f64::from(VIDEO_CODEC_AVC)));
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
/// Video payloads are Annex B and are converted to AVCC; audio payloads are raw AAC.
#[derive(Debug)]
pub struct FlvTagger {
    config: FlvConfig,
    origin_ms: i64,
    started: bool,
    avc_sent: bool,
    last_video_ms: Option<i64>,
    last_audio_ms: Option<i64>,
}

impl FlvTagger {
    /// A tagger for `config`.
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
            origin_ms,
            started: false,
            avc_sent: false,
            last_video_ms: None,
            last_audio_ms: None,
        })
    }

    /// The configuration, including an AVC configuration taken from the stream.
    pub fn config(&self) -> &FlvConfig {
        &self.config
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
        values.push(self.config.metadata());
        let mut tags = vec![FlvTag {
            tag_type: TagType::Script,
            timestamp: 0,
            body: Amf0Value::encode_all(&values)?,
        }];
        self.started = true;
        if let Some(avc) = self.config.video.as_ref().and_then(|v| v.avc.as_ref()) {
            tags.push(avc_sequence_header(avc, 0));
            self.avc_sent = true;
        }
        if let Some(audio) = &self.config.audio {
            tags.push(FlvTag {
                tag_type: TagType::Audio,
                timestamp: 0,
                body: audio_tag_body(AacPacketType::SequenceHeader, audio.config.as_bytes()),
            });
        }
        Ok(tags)
    }

    /// The tags for one packet: the start tags first if not yet yielded, then the AVC
    /// sequence header if it comes from this packet, then the packet's own tag.
    ///
    /// # Errors
    /// [`MuxError::NoTrack`] for a codec with no configured track,
    /// [`MuxError::NegativeTimestamp`] for a dts before the origin,
    /// [`MuxError::NonMonotonicDts`] when a track's dts goes backwards,
    /// [`MuxError::CompositionOffsetOutOfRange`], [`MuxError::MissingAvcConfig`],
    /// [`MuxError::NotAnnexB`] and [`MuxError::Media`].
    pub fn push(&mut self, packet: &Packet) -> Result<Vec<FlvTag>, MuxError> {
        let codec = packet.codec();
        let has_track = match codec {
            Codec::H264 => self.config.video.is_some(),
            Codec::Aac => self.config.audio.is_some(),
            _ => false,
        };
        if !has_track {
            return Err(MuxError::NoTrack { codec });
        }
        let dts = to_ms(packet.dts())?.saturating_sub(self.origin_ms);
        let pts = to_ms(packet.pts())?.saturating_sub(self.origin_ms);
        if dts < 0 {
            return Err(MuxError::NegativeTimestamp { ms: dts });
        }
        let last = if codec == Codec::H264 {
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
        let (new_avc, tag) = if codec == Codec::H264 {
            let needs_avc =
                !self.avc_sent && self.config.video.as_ref().is_some_and(|v| v.avc.is_none());
            let new_avc = if needs_avc {
                Some(Self::avc_from_packet(packet)?)
            } else {
                None
            };
            let body = video_tag_body(
                packet.is_keyframe(),
                AvcPacketType::Nalu,
                cts,
                &annexb_to_avcc(packet.data())?,
            )?;
            let tag = FlvTag {
                tag_type: TagType::Video,
                timestamp,
                body,
            };
            (new_avc, tag)
        } else {
            let tag = FlvTag {
                tag_type: TagType::Audio,
                timestamp,
                body: audio_tag_body(AacPacketType::Raw, packet.data()),
            };
            (None, tag)
        };

        let mut tags = self.start()?;
        if let Some(avc) = new_avc {
            tags.push(avc_sequence_header(&avc, timestamp));
            if let Some(video) = &mut self.config.video {
                video.avc = Some(avc);
            }
            self.avc_sent = true;
        }
        if codec == Codec::H264 {
            self.last_video_ms = Some(dts);
        } else {
            self.last_audio_ms = Some(dts);
        }
        tags.push(tag);
        Ok(tags)
    }

    /// The AVC end-of-sequence tag, at the last video timestamp, if any video was
    /// written; otherwise nothing.
    pub fn finish(&mut self) -> Vec<FlvTag> {
        match self.last_video_ms {
            Some(ms) if self.avc_sent => vec![FlvTag {
                tag_type: TagType::Video,
                timestamp: wrap_timestamp(ms),
                body: vec![(1 << 4) | VIDEO_CODEC_AVC, 2, 0, 0, 0],
            }],
            _ => Vec::new(),
        }
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

fn avc_sequence_header(avc: &AvcDecoderConfig, timestamp: u32) -> FlvTag {
    let record = avc.to_bytes();
    let mut body = Vec::with_capacity(5 + record.len());
    body.extend_from_slice(&[(1 << 4) | VIDEO_CODEC_AVC, 0, 0, 0, 0]);
    body.extend_from_slice(&record);
    FlvTag {
        tag_type: TagType::Video,
        timestamp,
        body,
    }
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
    pub fn new(mut out: W, config: FlvConfig) -> Result<Self, MuxError> {
        let header = file_header(config.audio.is_some(), config.video.is_some());
        let mut tagger = FlvTagger::new(config)?;
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
    use crate::h264::tests::{PPS, SPS};

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
}
