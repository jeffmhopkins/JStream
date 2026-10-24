//! Encoded packets, as encoders produce them and muxers consume them.

use std::sync::Arc;

use crate::{MediaError, MediaTime};

/// Whether a stream carries video or audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Video.
    Video,
    /// Audio.
    Audio,
}

/// A compressed format. The baseline set follows docs/adr/0061-source-types-and-decoding-scope-v2.md:
/// H.264 video and AAC audio, from OS encoders. HEVC and AV1 are the video codecs enhanced
/// RTMP carries (docs/adr/0058-youtube-sign-in-with-a-jstream-client.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Codec {
    /// H.264 / AVC video. Payloads are Annex B (start codes), as OS encoders emit them.
    H264,
    /// AAC audio. Payloads are raw access units, without ADTS headers.
    Aac,
    /// HEVC / H.265 video. Payloads are Annex B (start codes), like H.264.
    Hevc,
    /// AV1 video. A payload is one temporal unit: OBUs in the low-overhead bitstream
    /// format, each with its size field (the last one may omit it), as encoders emit them.
    Av1,
}

impl Codec {
    /// Whether the codec is for video or audio.
    pub fn kind(self) -> MediaKind {
        match self {
            Self::H264 | Self::Hevc | Self::Av1 => MediaKind::Video,
            Self::Aac => MediaKind::Audio,
        }
    }
}

/// One encoded access unit (a video frame or a block of audio frames).
///
/// The payload is shared (`Arc<[u8]>`), so fanning one packet out to several outputs
/// clones a pointer, not the bytes. Invariants, checked by [`Packet::new`]: the payload
/// is non-empty and `dts <= pts`, compared as instants, so the two may use different time
/// bases.
///
/// ```
/// use jstream_media::{Codec, MediaTime, Packet, Timebase};
///
/// let pts = MediaTime::new(3003, Timebase::MPEG_90K);
/// let p = Packet::new(Codec::H264, true, pts, pts, vec![0, 0, 0, 1, 0x65])?;
/// assert!(p.is_keyframe());
/// assert!(Packet::new(Codec::H264, false, pts, MediaTime::new(3004, Timebase::MPEG_90K), vec![1]).is_err());
/// # Ok::<(), jstream_media::MediaError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    codec: Codec,
    keyframe: bool,
    pts: MediaTime,
    dts: MediaTime,
    data: Arc<[u8]>,
}

impl Packet {
    /// A packet of `data` for `codec`. `keyframe` marks a random-access point (an IDR
    /// frame for H.264; every AAC packet is one).
    ///
    /// # Errors
    /// [`MediaError::EmptyPacket`] when `data` is empty, and [`MediaError::DtsAfterPts`]
    /// when `dts` is later than `pts`.
    pub fn new(
        codec: Codec,
        keyframe: bool,
        pts: MediaTime,
        dts: MediaTime,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Self, MediaError> {
        let data = data.into();
        if data.is_empty() {
            return Err(MediaError::EmptyPacket);
        }
        if dts > pts {
            return Err(MediaError::DtsAfterPts { pts, dts });
        }
        Ok(Self {
            codec,
            keyframe,
            pts,
            dts,
            data,
        })
    }

    /// The codec.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether decoding can start at this packet.
    pub fn is_keyframe(&self) -> bool {
        self.keyframe
    }

    /// Presentation timestamp.
    pub fn pts(&self) -> MediaTime {
        self.pts
    }

    /// Decode timestamp; never later than [`Packet::pts`].
    pub fn dts(&self) -> MediaTime {
        self.dts
    }

    /// The encoded bytes; never empty.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// A shared handle to the encoded bytes.
    pub fn shared_data(&self) -> Arc<[u8]> {
        Arc::clone(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Codec, MediaKind, Packet};
    use crate::{MediaError, MediaTime, Timebase};

    fn ms(t: i64) -> MediaTime {
        MediaTime::new(t, Timebase::MILLIS)
    }

    #[test]
    fn codec_kinds() {
        assert_eq!(Codec::H264.kind(), MediaKind::Video);
        assert_eq!(Codec::Aac.kind(), MediaKind::Audio);
        assert_eq!(Codec::Hevc.kind(), MediaKind::Video);
        assert_eq!(Codec::Av1.kind(), MediaKind::Video);
    }

    #[test]
    fn empty_payload_is_rejected() {
        assert_eq!(
            Packet::new(Codec::Aac, true, ms(0), ms(0), Vec::new()),
            Err(MediaError::EmptyPacket)
        );
    }

    #[test]
    fn dts_after_pts_is_rejected() {
        assert_eq!(
            Packet::new(Codec::H264, false, ms(10), ms(11), vec![1]),
            Err(MediaError::DtsAfterPts {
                pts: ms(10),
                dts: ms(11)
            })
        );
    }

    #[test]
    fn b_frame_reordering_is_allowed() {
        // dts before pts, compared across time bases
        let pts = MediaTime::new(3003, Timebase::MPEG_90K);
        let p = Packet::new(Codec::H264, false, pts, ms(0), vec![1, 2]).unwrap();
        assert!(p.dts() < p.pts());
        assert!(!p.is_keyframe());
        assert_eq!(p.codec(), Codec::H264);
    }

    #[test]
    fn dts_equal_in_value_across_timebases_is_allowed() {
        let pts = MediaTime::new(90, Timebase::MPEG_90K);
        assert!(Packet::new(Codec::H264, true, pts, ms(1), vec![1]).is_ok());
    }

    #[test]
    fn clones_share_the_payload() {
        let p = Packet::new(Codec::Aac, true, ms(0), ms(0), vec![9, 8, 7]).unwrap();
        let q = p.clone();
        assert!(Arc::ptr_eq(&p.shared_data(), &q.shared_data()));
        assert_eq!(q.data(), &[9, 8, 7]);
        let from_arc: Arc<[u8]> = Arc::from(&[1u8][..]);
        let r = Packet::new(Codec::Aac, true, ms(0), ms(0), Arc::clone(&from_arc)).unwrap();
        assert!(Arc::ptr_eq(&r.shared_data(), &from_arc));
    }
}
