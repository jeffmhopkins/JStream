//! Preview streams: the program mix as low-resolution video, sent to one session as
//! WebSocket binary frames (ADR-0045, ARCH › Control protocol).
//!
//! A client opens a stream with the `preview.subscribe` action, naming a `channel` of
//! its own choosing, and closes it with `preview.unsubscribe`. Both are JSON calls; the
//! frames themselves are [`BinaryFrame`]s, which only the session that subscribed
//! receives. Each frame starts with a fixed [`BINARY_HEADER_LEN`]-byte header (the
//! layout is on [`BinaryHeader`]) followed by one encoded picture: an H.264 access unit
//! in Annex B form, or a baseline JPEG.

use crate::error::ProtocolError;
use crate::id::Rev;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// How a preview stream's pictures are encoded.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PreviewCodec {
    /// H.264 access units in Annex B form (start codes, parameter sets in band), for
    /// WebCodecs' `VideoDecoder` with no `description`. A frame is a keyframe exactly
    /// when its header says so.
    H264,
    /// One baseline JPEG per frame, every one a keyframe. The fallback when the engine
    /// has no H.264 encoder or the client can't decode H.264.
    Jpeg,
}

impl PreviewCodec {
    /// The codec's byte in a [`BinaryHeader`].
    pub fn to_byte(self) -> u8 {
        match self {
            PreviewCodec::H264 => 1,
            PreviewCodec::Jpeg => 2,
        }
    }

    /// The codec a [`BinaryHeader`] byte names, if any.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(PreviewCodec::H264),
            2 => Some(PreviewCodec::Jpeg),
            _ => None,
        }
    }
}

/// The settings of one preview stream, from `preview.subscribe`'s fields with their
/// defaults filled in and checked by [`PreviewSettings::resolve`], which the engine
/// uses to accept or refuse a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewSettings {
    /// The longest the picture may be, in pixels.
    pub max_width: u32,
    /// The tallest the picture may be, in pixels.
    pub max_height: u32,
    /// Pictures per second, at most.
    pub fps: u32,
    /// The codecs the client accepts, most preferred first; never empty.
    pub codecs: Vec<PreviewCodec>,
}

impl PreviewSettings {
    /// `max_width` when the client gives none.
    pub const DEFAULT_MAX_WIDTH: u32 = 640;
    /// `max_height` when the client gives none.
    pub const DEFAULT_MAX_HEIGHT: u32 = 360;
    /// `fps` when the client gives none.
    pub const DEFAULT_FPS: u32 = 15;
    /// The widths a client may ask for.
    pub const WIDTH: std::ops::RangeInclusive<u32> = 16..=1920;
    /// The heights a client may ask for.
    pub const HEIGHT: std::ops::RangeInclusive<u32> = 16..=1080;
    /// The rates a client may ask for.
    pub const FPS: std::ops::RangeInclusive<u32> = 1..=30;
    /// The codecs when the client lists none: H.264, then JPEG.
    pub const DEFAULT_CODECS: [PreviewCodec; 2] = [PreviewCodec::H264, PreviewCodec::Jpeg];
    /// The most preview streams one session may have open at once.
    pub const MAX_PER_SESSION: usize = 16;

    /// Fills in the defaults of a `preview.subscribe` and checks its fields.
    ///
    /// # Errors
    /// `invalid_argument` naming the field (`max_width`, `max_height`, `fps`) when it is
    /// outside its range, or `codecs` when it names a codec twice.
    pub fn resolve(
        max_width: Option<u32>,
        max_height: Option<u32>,
        fps: Option<u32>,
        codecs: &[PreviewCodec],
    ) -> Result<Self, ProtocolError> {
        let ranged = |value: Option<u32>,
                      default: u32,
                      range: std::ops::RangeInclusive<u32>,
                      field: &str| {
            let value = value.unwrap_or(default);
            if range.contains(&value) {
                Ok(value)
            } else {
                Err(ProtocolError::invalid_argument(
                    field,
                    format!("must be from {} to {}", range.start(), range.end()),
                ))
            }
        };
        let max_width = ranged(max_width, Self::DEFAULT_MAX_WIDTH, Self::WIDTH, "max_width")?;
        let max_height = ranged(
            max_height,
            Self::DEFAULT_MAX_HEIGHT,
            Self::HEIGHT,
            "max_height",
        )?;
        let fps = ranged(fps, Self::DEFAULT_FPS, Self::FPS, "fps")?;
        let codecs = if codecs.is_empty() {
            Self::DEFAULT_CODECS.to_vec()
        } else {
            for (i, codec) in codecs.iter().enumerate() {
                if codecs[..i].contains(codec) {
                    return Err(ProtocolError::invalid_argument(
                        "codecs",
                        "names a codec twice",
                    ));
                }
            }
            codecs.to_vec()
        };
        Ok(Self {
            max_width,
            max_height,
            fps,
            codecs,
        })
    }

    /// The picture size for a canvas of `width` × `height`: the canvas scaled down
    /// (never up) to fit `max_width` × `max_height` with its aspect ratio, rounded down
    /// to even sides (H.264 4:2:0 needs them), and at least 2 × 2.
    pub fn picture_size(&self, width: u32, height: u32) -> (u32, u32) {
        let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let (max_w, max_h) = (u64::from(self.max_width), u64::from(self.max_height));
        // Scale by the tighter of the two bounds, and not at all if both fit.
        let (w, h) = if width <= max_w && height <= max_h {
            (width, height)
        } else if width * max_h >= height * max_w {
            (max_w, (height * max_w / width).max(1))
        } else {
            ((width * max_h / height).max(1), max_h)
        };
        let even = |side: u64| u32::try_from(side & !1).unwrap_or(u32::MAX - 1).max(2);
        (even(w), even(h))
    }
}

/// What a binary frame carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StreamKind {
    /// A preview stream's picture (`preview.subscribe`).
    Preview,
}

impl StreamKind {
    fn to_byte(self) -> u8 {
        match self {
            StreamKind::Preview => 1,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(StreamKind::Preview),
            _ => None,
        }
    }
}

/// The version byte every binary frame starts with. A client that reads another
/// version ignores the frame.
pub const BINARY_VERSION: u8 = 1;

/// The length of a [`BinaryHeader`] on the wire, in bytes.
pub const BINARY_HEADER_LEN: usize = 32;

/// `flags` bit: the picture is a keyframe (a decoder can start here).
const FLAG_KEYFRAME: u8 = 1;
/// `flags` bit: the stream has ended; the payload is the reason, as UTF-8.
const FLAG_END: u8 = 2;

/// The fixed header of a binary frame. On the wire it is [`BINARY_HEADER_LEN`] bytes,
/// little-endian:
///
/// | Offset | Size | Field |
/// |---|---|---|
/// | 0 | 1 | version, [`BINARY_VERSION`] |
/// | 1 | 1 | kind: 1 = preview |
/// | 2 | 1 | codec: 0 = none (an end frame), 1 = H.264, 2 = JPEG |
/// | 3 | 1 | flags: bit 0 keyframe, bit 1 end of stream; the rest are 0 |
/// | 4 | 4 | channel, as the client named it in `preview.subscribe` |
/// | 8 | 2 | picture width, in pixels |
/// | 10 | 2 | picture height, in pixels |
/// | 12 | 4 | reserved, 0 |
/// | 16 | 8 | presentation time, in microseconds since the stream opened (signed) |
/// | 24 | 8 | the collection `rev` the picture shows |
///
/// The payload follows: the encoded picture, or for an end frame the reason the
/// engine ended the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryHeader {
    /// What the frame carries.
    pub kind: StreamKind,
    /// The channel the client subscribed with.
    pub channel: u32,
    /// How the payload is encoded; `None` on an end frame.
    pub codec: Option<PreviewCodec>,
    /// Whether a decoder can start at this frame.
    pub keyframe: bool,
    /// Whether this is the stream's last frame: the engine ended it, and the payload
    /// says why. A client's own `preview.unsubscribe` sends none.
    pub end: bool,
    /// Picture width, in pixels (0 on an end frame).
    pub width: u16,
    /// Picture height, in pixels (0 on an end frame).
    pub height: u16,
    /// Presentation time in microseconds since the stream opened. It increases from
    /// frame to frame; WebCodecs takes it as the chunk's `timestamp`.
    pub pts_us: i64,
    /// The collection revision the picture shows: changes up to this `rev` are in it.
    pub rev: Rev,
}

/// One binary frame: a [`BinaryHeader`] and its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryFrame {
    /// The header.
    pub header: BinaryHeader,
    /// The encoded picture, or an end frame's reason.
    pub payload: Arc<[u8]>,
}

/// Why bytes are not a [`BinaryFrame`] this version can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BinaryError {
    /// Shorter than [`BINARY_HEADER_LEN`].
    #[error("a binary frame is at least {BINARY_HEADER_LEN} bytes, got {0}")]
    Short(usize),
    /// A version byte other than [`BINARY_VERSION`].
    #[error("binary frame version {0} is not {BINARY_VERSION}")]
    Version(u8),
    /// A kind byte this version doesn't know.
    #[error("unknown binary stream kind {0}")]
    Kind(u8),
    /// A codec byte this version doesn't know.
    #[error("unknown preview codec {0}")]
    Codec(u8),
}

impl BinaryFrame {
    /// An end frame for `channel`, carrying `reason`.
    pub fn end(kind: StreamKind, channel: u32, rev: Rev, reason: &str) -> Self {
        BinaryFrame {
            header: BinaryHeader {
                kind,
                channel,
                codec: None,
                keyframe: false,
                end: true,
                width: 0,
                height: 0,
                pts_us: 0,
                rev,
            },
            payload: Arc::from(reason.as_bytes()),
        }
    }

    /// The frame as it goes on the wire: the header, then the payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let h = &self.header;
        let mut out = Vec::with_capacity(BINARY_HEADER_LEN + self.payload.len());
        out.push(BINARY_VERSION);
        out.push(h.kind.to_byte());
        out.push(h.codec.map_or(0, PreviewCodec::to_byte));
        let mut flags = 0;
        if h.keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if h.end {
            flags |= FLAG_END;
        }
        out.push(flags);
        out.extend_from_slice(&h.channel.to_le_bytes());
        out.extend_from_slice(&h.width.to_le_bytes());
        out.extend_from_slice(&h.height.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&h.pts_us.to_le_bytes());
        out.extend_from_slice(&h.rev.0.to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Reads a frame. Unknown flag bits and the reserved bytes are ignored, so a later
    /// minor can use them.
    ///
    /// # Errors
    /// [`BinaryError`] when `bytes` are too short or name a version, kind or codec this
    /// version doesn't know.
    pub fn parse(bytes: &[u8]) -> Result<Self, BinaryError> {
        let Some((head, payload)) = bytes.split_first_chunk::<BINARY_HEADER_LEN>() else {
            return Err(BinaryError::Short(bytes.len()));
        };
        let header = BinaryHeader::parse(head)?;
        Ok(BinaryFrame {
            header,
            payload: Arc::from(payload),
        })
    }
}

impl BinaryHeader {
    /// Reads the header at the start of a frame.
    ///
    /// # Errors
    /// As for [`BinaryFrame::parse`].
    pub fn parse(head: &[u8; BINARY_HEADER_LEN]) -> Result<Self, BinaryError> {
        let [version, kind, codec, flags, rest @ ..] = *head;
        if version != BINARY_VERSION {
            return Err(BinaryError::Version(version));
        }
        let kind = StreamKind::from_byte(kind).ok_or(BinaryError::Kind(kind))?;
        let codec = match codec {
            0 => None,
            byte => Some(PreviewCodec::from_byte(byte).ok_or(BinaryError::Codec(byte))?),
        };
        let u16_at = |at: usize| u16::from_le_bytes([rest[at - 4], rest[at - 3]]);
        let u64_at = |at: usize| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&rest[at - 4..at + 4]);
            u64::from_le_bytes(bytes)
        };
        let channel = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
        Ok(BinaryHeader {
            kind,
            channel,
            codec,
            keyframe: flags & FLAG_KEYFRAME != 0,
            end: flags & FLAG_END != 0,
            width: u16_at(8),
            height: u16_at(10),
            pts_us: i64::from_le_bytes(u64_at(16).to_le_bytes()),
            rev: Rev(u64_at(24)),
        })
    }
}

/// Decides which frames a bounded queue of binary frames keeps, so that whatever a
/// reader gets still decodes. Each hop that queues frames (the engine, the server, the
/// WebSocket client) uses one.
///
/// A frame that finds the queue full is dropped, and so is every later frame on its
/// channel until a keyframe that finds room: an H.264 picture can't be decoded without
/// the ones before it back to a keyframe. End frames always pass, full or not, since
/// they are small and a reader must see them.
#[derive(Clone, Debug, Default)]
pub struct KeyframeGate {
    /// The channels waiting for a keyframe.
    waiting: Vec<u32>,
}

impl KeyframeGate {
    /// Whether to queue a frame with `header`, given whether the queue has `room` for
    /// it. `false` means drop it.
    pub fn admit(&mut self, header: &BinaryHeader, room: bool) -> bool {
        let channel = header.channel;
        let waiting = self.waiting.contains(&channel);
        if header.end {
            self.forget(channel);
            return true;
        }
        if !room {
            if !waiting {
                self.waiting.push(channel);
            }
            return false;
        }
        if waiting {
            if !header.keyframe {
                return false;
            }
            self.forget(channel);
        }
        true
    }

    /// Whether `channel` is dropping frames until its next keyframe.
    pub fn is_waiting(&self, channel: u32) -> bool {
        self.waiting.contains(&channel)
    }

    /// Forgets `channel`'s state: its stream ended or was closed.
    pub fn forget(&mut self, channel: u32) {
        self.waiting.retain(|&c| c != channel);
    }
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StreamKind::Preview => "preview",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> BinaryFrame {
        BinaryFrame {
            header: BinaryHeader {
                kind: StreamKind::Preview,
                channel: 0x0102_0304,
                codec: Some(PreviewCodec::H264),
                keyframe: true,
                end: false,
                width: 640,
                height: 360,
                pts_us: -5,
                rev: Rev(0x1122_3344_5566_7788),
            },
            payload: Arc::from(&[0, 0, 0, 1, 0x65][..]),
        }
    }

    #[test]
    fn header_layout_is_as_documented() {
        let bytes = picture().to_bytes();
        assert_eq!(bytes.len(), BINARY_HEADER_LEN + 5);
        assert_eq!(&bytes[..4], &[BINARY_VERSION, 1, 1, 1]);
        assert_eq!(&bytes[4..8], &[4, 3, 2, 1]);
        assert_eq!(&bytes[8..12], &[0x80, 2, 0x68, 1]);
        assert_eq!(&bytes[12..16], &[0; 4]);
        assert_eq!(&bytes[16..24], &(-5i64).to_le_bytes());
        assert_eq!(&bytes[24..32], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&bytes[32..], &[0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn frames_round_trip() {
        let frame = picture();
        assert_eq!(BinaryFrame::parse(&frame.to_bytes()), Ok(frame));
        let end = BinaryFrame::end(StreamKind::Preview, 7, Rev(3), "no encoder");
        let back = BinaryFrame::parse(&end.to_bytes()).expect("parses");
        assert!(back.header.end && back.header.codec.is_none());
        assert_eq!(&*back.payload, b"no encoder");
    }

    #[test]
    fn unknown_bytes_are_refused_and_spare_bits_ignored() {
        let mut bytes = picture().to_bytes();
        assert_eq!(
            BinaryFrame::parse(&bytes[..31]),
            Err(BinaryError::Short(31))
        );
        bytes[3] |= 0x80;
        bytes[12] = 9;
        assert_eq!(BinaryFrame::parse(&bytes), Ok(picture()));
        bytes[2] = 9;
        assert_eq!(BinaryFrame::parse(&bytes), Err(BinaryError::Codec(9)));
        bytes[1] = 9;
        assert_eq!(BinaryFrame::parse(&bytes), Err(BinaryError::Kind(9)));
        bytes[0] = 2;
        assert_eq!(BinaryFrame::parse(&bytes), Err(BinaryError::Version(2)));
    }

    #[test]
    fn the_gate_drops_until_a_keyframe_finds_room() {
        let mut gate = KeyframeGate::default();
        let mut header = picture().header;
        let other = BinaryHeader {
            channel: 9,
            keyframe: false,
            ..header
        };
        header.keyframe = false;
        assert!(gate.admit(&header, true));
        assert!(!gate.admit(&header, false));
        assert!(gate.is_waiting(header.channel));
        // Other channels are not held back.
        assert!(gate.admit(&other, true));
        // With room again, inter frames still wait; a keyframe without room too.
        assert!(!gate.admit(&header, true));
        header.keyframe = true;
        assert!(!gate.admit(&header, false));
        assert!(gate.admit(&header, true));
        assert!(!gate.is_waiting(header.channel));
        // An end frame passes a full queue and clears the channel.
        header.keyframe = false;
        assert!(!gate.admit(&header, false));
        let end = BinaryFrame::end(StreamKind::Preview, header.channel, Rev(1), "gone");
        assert!(gate.admit(&end.header, false));
        assert!(!gate.is_waiting(header.channel));
    }

    #[test]
    fn settings_default_and_check_their_ranges() {
        let settings = PreviewSettings::resolve(None, None, None, &[]).expect("defaults");
        assert_eq!(
            (settings.max_width, settings.max_height, settings.fps),
            (640, 360, 15)
        );
        assert_eq!(settings.codecs, PreviewSettings::DEFAULT_CODECS);
        let field = |result: Result<PreviewSettings, ProtocolError>| match result {
            Err(ProtocolError::InvalidArgument { field, .. }) => field,
            other => panic!("expected invalid_argument, got {other:?}"),
        };
        assert_eq!(
            field(PreviewSettings::resolve(Some(8), None, None, &[])),
            "max_width"
        );
        assert_eq!(
            field(PreviewSettings::resolve(None, Some(4000), None, &[])),
            "max_height"
        );
        assert_eq!(
            field(PreviewSettings::resolve(None, None, Some(0), &[])),
            "fps"
        );
        let twice = [PreviewCodec::Jpeg, PreviewCodec::Jpeg];
        assert_eq!(
            field(PreviewSettings::resolve(None, None, None, &twice)),
            "codecs"
        );
    }

    #[test]
    fn picture_size_fits_the_box_with_even_sides() {
        let settings = PreviewSettings::resolve(None, None, None, &[]).expect("defaults");
        assert_eq!(settings.picture_size(1920, 1080), (640, 360));
        // Taller than 16:9: the height bounds it.
        assert_eq!(settings.picture_size(1080, 1080), (360, 360));
        // Wider: the width bounds it, the height rounds down to even.
        assert_eq!(settings.picture_size(2560, 1080), (640, 270));
        assert_eq!(settings.picture_size(1000, 333), (640, 212));
        // Never scaled up; odd sides round down.
        assert_eq!(settings.picture_size(321, 101), (320, 100));
        assert_eq!(settings.picture_size(1, 1), (2, 2));
    }
}
