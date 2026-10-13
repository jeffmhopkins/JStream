//! The RTMP chunk stream: splitting messages into chunks with compressed headers
//! (types 0–3, extended timestamps), and reassembling the peer's chunks into messages.
//!
//! Both halves are pure byte transformations with no I/O, so they are tested directly
//! and the decoder never blocks or loses a partially received chunk.

use std::collections::HashMap;

use super::RtmpError;

/// Message type IDs used by the client.
pub(crate) mod msg_type {
    /// Set Chunk Size (protocol control).
    pub(crate) const SET_CHUNK_SIZE: u8 = 1;
    /// Abort Message (protocol control).
    pub(crate) const ABORT: u8 = 2;
    /// Acknowledgement (protocol control).
    pub(crate) const ACK: u8 = 3;
    /// User Control Message.
    pub(crate) const USER_CONTROL: u8 = 4;
    /// Window Acknowledgement Size (protocol control).
    pub(crate) const WINDOW_ACK_SIZE: u8 = 5;
    /// Set Peer Bandwidth (protocol control).
    pub(crate) const SET_PEER_BANDWIDTH: u8 = 6;
    /// Audio data (an FLV AUDIODATA tag body).
    pub(crate) const AUDIO: u8 = 8;
    /// Video data (an FLV VIDEODATA tag body).
    pub(crate) const VIDEO: u8 = 9;
    /// Command message, AMF3 encoded.
    pub(crate) const COMMAND_AMF3: u8 = 17;
    /// Data message, AMF0 encoded.
    pub(crate) const DATA_AMF0: u8 = 18;
    /// Command message, AMF0 encoded.
    pub(crate) const COMMAND_AMF0: u8 = 20;
}

/// The default chunk size both directions start with.
pub(crate) const DEFAULT_CHUNK_SIZE: u32 = 128;
/// The largest chunk size RTMP allows (31 bits).
pub(crate) const MAX_CHUNK_SIZE: u32 = 0x7FFF_FFFF;
/// The largest message length the 24-bit length field can carry.
pub(crate) const MAX_MESSAGE_LEN: usize = 0xFF_FFFF;
/// Timestamps at or above this value go in the 4-byte extended timestamp field.
const EXTENDED: u32 = 0xFF_FFFF;
/// The largest message accepted from the peer. A publish client only receives control
/// and command messages, which are small; the cap bounds memory a hostile server can use.
pub(crate) const MAX_INCOMING_MESSAGE: usize = 1 << 20;
/// The most bytes held across all partially received messages.
const MAX_INCOMING_PARTIAL: usize = 8 << 20;

/// A complete RTMP message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    /// The chunk stream it arrived on.
    pub(crate) csid: u32,
    /// Timestamp in milliseconds (wrapping at 2^32).
    pub(crate) timestamp: u32,
    /// Message type ID.
    pub(crate) type_id: u8,
    /// Message stream ID.
    pub(crate) stream_id: u32,
    /// The message body.
    pub(crate) payload: Vec<u8>,
}

/// Checks a chunk size against RTMP's 1..=2^31-1 range.
pub(crate) fn check_chunk_size(size: u32) -> Result<u32, RtmpError> {
    if (1..=MAX_CHUNK_SIZE).contains(&size) {
        Ok(size)
    } else {
        Err(RtmpError::InvalidChunkSize { size })
    }
}

#[derive(Clone, Copy)]
struct OutState {
    timestamp: u32,
    len: u32,
    type_id: u8,
    stream_id: u32,
    /// The delta the last header on this chunk stream carried (type 1 or 2), which a
    /// type-3 header for a new message repeats. `None` after a type-0 header, whose
    /// implied delta peers interpret differently, so it is never followed by a type 3.
    delta: Option<u32>,
}

/// Splits outgoing messages into chunks.
pub(crate) struct ChunkEncoder {
    chunk_size: u32,
    streams: HashMap<u32, OutState>,
}

impl ChunkEncoder {
    pub(crate) fn new() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            streams: HashMap::new(),
        }
    }

    /// Uses `size` for every chunk after this call. The caller sends Set Chunk Size first.
    pub(crate) fn set_chunk_size(&mut self, size: u32) -> Result<(), RtmpError> {
        self.chunk_size = check_chunk_size(size)?;
        Ok(())
    }

    /// Appends `payload` as one message on chunk stream `csid` (2..=65599) to `out`,
    /// choosing the smallest header the previous message on `csid` allows.
    ///
    /// # Errors
    /// [`RtmpError::PayloadSize`] when `payload` is longer than 16 MiB - 1.
    pub(crate) fn encode(
        &mut self,
        csid: u32,
        timestamp: u32,
        type_id: u8,
        stream_id: u32,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), RtmpError> {
        debug_assert!((2..=65_599).contains(&csid), "chunk stream ID {csid}");
        let len = u32::try_from(payload.len())
            .ok()
            .filter(|&l| l as usize <= MAX_MESSAGE_LEN)
            .ok_or(RtmpError::PayloadSize { len: payload.len() })?;

        let prev = self.streams.get(&csid).copied();
        let (fmt, field) = match prev {
            Some(p) if p.stream_id == stream_id && timestamp >= p.timestamp => {
                let delta = timestamp - p.timestamp;
                if p.len != len || p.type_id != type_id {
                    (1, delta)
                } else if p.delta == Some(delta) {
                    (3, delta)
                } else {
                    (2, delta)
                }
            }
            _ => (0, timestamp),
        };
        let ext = field >= EXTENDED;
        let header_field = field.min(EXTENDED);

        out.reserve(payload.len() + 18 + payload.len() / self.chunk_size as usize * 8);
        basic_header(fmt, csid, out);
        if fmt <= 2 {
            out.extend_from_slice(&header_field.to_be_bytes()[1..]);
        }
        if fmt <= 1 {
            out.extend_from_slice(&len.to_be_bytes()[1..]);
            out.push(type_id);
        }
        if fmt == 0 {
            out.extend_from_slice(&stream_id.to_le_bytes());
        }
        if ext {
            out.extend_from_slice(&field.to_be_bytes());
        }
        let mut chunks = payload.chunks(self.chunk_size as usize);
        if let Some(first) = chunks.next() {
            out.extend_from_slice(first);
        }
        for chunk in chunks {
            basic_header(3, csid, out);
            if ext {
                out.extend_from_slice(&field.to_be_bytes());
            }
            out.extend_from_slice(chunk);
        }

        self.streams.insert(
            csid,
            OutState {
                timestamp,
                len,
                type_id,
                stream_id,
                delta: (fmt != 0).then_some(field),
            },
        );
        Ok(())
    }
}

fn basic_header(fmt: u8, csid: u32, out: &mut Vec<u8>) {
    let fmt = fmt << 6;
    match csid {
        // The casts below take values the match arm has already bounded.
        2..=63 => out.push(fmt | csid as u8),
        64..=319 => out.extend_from_slice(&[fmt, (csid - 64) as u8]),
        _ => {
            let id = csid - 64;
            out.extend_from_slice(&[fmt | 1, (id & 0xFF) as u8, (id >> 8) as u8]);
        }
    }
}

#[derive(Default)]
struct InState {
    /// Whether a type-0 header has been seen, so later headers have a base.
    has_header: bool,
    timestamp: u32,
    len: u32,
    type_id: u8,
    stream_id: u32,
    delta: u32,
    /// Whether the last header used an extended timestamp, so type-3 chunks carry one.
    ext: bool,
    /// The message being reassembled; empty when none is in progress.
    partial: Vec<u8>,
    in_progress: bool,
}

/// Reassembles the peer's chunks into messages.
///
/// Bytes are [`push`](Self::push)ed as they arrive; [`next_message`](Self::next_message)
/// consumes only complete chunks, so input may be split anywhere.
pub(crate) struct ChunkDecoder {
    chunk_size: u32,
    streams: HashMap<u32, InState>,
    buf: Vec<u8>,
    pos: usize,
    partial_bytes: usize,
}

impl ChunkDecoder {
    pub(crate) fn new() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            streams: HashMap::new(),
            buf: Vec::new(),
            pos: 0,
            partial_bytes: 0,
        }
    }

    /// Adds received bytes.
    pub(crate) fn push(&mut self, data: &[u8]) {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Applies the peer's Set Chunk Size.
    pub(crate) fn set_chunk_size(&mut self, size: u32) -> Result<(), RtmpError> {
        self.chunk_size = check_chunk_size(size)?;
        Ok(())
    }

    /// Applies the peer's Abort Message: drops the partial message on `csid`.
    pub(crate) fn abort(&mut self, csid: u32) {
        if let Some(s) = self.streams.get_mut(&csid) {
            self.partial_bytes -= s.partial.len();
            s.partial = Vec::new();
            s.in_progress = false;
        }
    }

    /// The next complete message, or `None` when more bytes are needed.
    ///
    /// # Errors
    /// [`RtmpError::Protocol`] when the peer breaks the chunk format: a compressed header
    /// with no earlier type-0 header, a new header in the middle of a message, or a
    /// message longer than the 1 MiB this client accepts.
    pub(crate) fn next_message(&mut self) -> Result<Option<Message>, RtmpError> {
        loop {
            match self.chunk()? {
                None => {
                    // Keep the unconsumed tail at the front so the buffer doesn't grow.
                    self.buf.drain(..self.pos);
                    self.pos = 0;
                    return Ok(None);
                }
                Some(Some(msg)) => return Ok(Some(msg)),
                Some(None) => {}
            }
        }
    }

    /// Consumes one chunk if it is complete: `Some(Some(msg))` when it finished a
    /// message, `Some(None)` when it didn't, and `None` when more bytes are needed.
    fn chunk(&mut self) -> Result<Option<Option<Message>>, RtmpError> {
        let mut c = Cursor {
            data: &self.buf[self.pos..],
            pos: 0,
        };
        let Some(b0) = c.u8() else { return Ok(None) };
        let fmt = b0 >> 6;
        let csid = match b0 & 0x3F {
            0 => match c.u8() {
                Some(b) => 64 + u32::from(b),
                None => return Ok(None),
            },
            1 => match (c.u8(), c.u8()) {
                (Some(lo), Some(hi)) => 64 + u32::from(lo) + 256 * u32::from(hi),
                _ => return Ok(None),
            },
            id => u32::from(id),
        };
        let state = self.streams.entry(csid).or_default();
        if fmt != 0 && !state.has_header {
            return Err(RtmpError::protocol(format!(
                "chunk type {fmt} on chunk stream {csid} before any type-0 header"
            )));
        }
        if fmt != 3 && state.in_progress {
            return Err(RtmpError::protocol(format!(
                "chunk type {fmt} on chunk stream {csid} in the middle of a message"
            )));
        }

        // Header fields: (timestamp or delta field, length and type, stream ID).
        let header_len = [11, 7, 3, 0][usize::from(fmt)];
        let Some(h) = c.take(header_len) else {
            return Ok(None);
        };
        let field = (fmt <= 2).then(|| u32::from_be_bytes([0, h[0], h[1], h[2]]));
        let ext = match field {
            Some(f) => f == EXTENDED,
            None => state.ext,
        };
        let ext_value = if ext {
            match c.take(4) {
                Some(b) => Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]])),
                None => return Ok(None),
            }
        } else {
            None
        };

        let mut next = (
            state.timestamp,
            state.delta,
            state.len,
            state.type_id,
            state.stream_id,
        );
        if !state.in_progress {
            let field = ext_value.or(field).unwrap_or(state.delta);
            match fmt {
                0 => {
                    next.0 = field;
                    next.1 = 0;
                }
                _ => {
                    next.0 = state.timestamp.wrapping_add(field);
                    next.1 = field;
                }
            }
            if fmt <= 1 {
                next.2 = u32::from_be_bytes([0, h[3], h[4], h[5]]);
                next.3 = h[6];
            }
            if fmt == 0 {
                next.4 = u32::from_le_bytes([h[7], h[8], h[9], h[10]]);
            }
            if next.2 as usize > MAX_INCOMING_MESSAGE {
                return Err(RtmpError::protocol(format!(
                    "incoming message of {} bytes exceeds the {MAX_INCOMING_MESSAGE}-byte limit",
                    next.2
                )));
            }
        }
        let remaining = next.2 as usize - state.partial.len();
        let n = remaining.min(self.chunk_size as usize);
        let Some(body) = c.take(n) else {
            return Ok(None);
        };
        if self.partial_bytes + n > MAX_INCOMING_PARTIAL {
            return Err(RtmpError::protocol(
                "too many bytes buffered in partial incoming messages",
            ));
        }

        // The chunk is complete: commit.
        if fmt <= 2 {
            state.ext = ext;
        }
        state.has_header = true;
        (
            state.timestamp,
            state.delta,
            state.len,
            state.type_id,
            state.stream_id,
        ) = next;
        state.partial.extend_from_slice(body);
        self.partial_bytes += n;
        self.pos += c.pos;
        if state.partial.len() < state.len as usize {
            state.in_progress = true;
            return Ok(Some(None));
        }
        state.in_progress = false;
        let payload = std::mem::take(&mut state.partial);
        self.partial_bytes -= payload.len();
        Ok(Some(Some(Message {
            csid,
            timestamp: state.timestamp,
            type_id: state.type_id,
            stream_id: state.stream_id,
            payload,
        })))
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(enc: &mut ChunkEncoder, csid: u32, ts: u32, ty: u8, sid: u32, p: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        enc.encode(csid, ts, ty, sid, p, &mut out).unwrap();
        out
    }

    fn decode_all(dec: &mut ChunkDecoder) -> Vec<Message> {
        let mut msgs = Vec::new();
        while let Some(m) = dec.next_message().unwrap() {
            msgs.push(m);
        }
        msgs
    }

    #[test]
    fn type0_header_layout() {
        let mut enc = ChunkEncoder::new();
        let out = encode(&mut enc, 3, 0x010203, 20, 0x0A0B0C0D, b"hi");
        assert_eq!(
            out,
            [
                0x03, // fmt 0, csid 3
                0x01, 0x02, 0x03, // timestamp
                0x00, 0x00, 0x02, // length
                20,   // type
                0x0D, 0x0C, 0x0B, 0x0A, // stream ID, little-endian
                b'h', b'i',
            ]
        );
    }

    #[test]
    fn headers_compress_through_types_1_2_3() {
        let mut enc = ChunkEncoder::new();
        encode(&mut enc, 6, 100, 9, 1, &[1, 2, 3]);
        // Same stream, new length: type 1 with a delta.
        let t1 = encode(&mut enc, 6, 140, 9, 1, &[1, 2, 3, 4]);
        assert_eq!(t1, [0x46, 0, 0, 40, 0, 0, 4, 9, 1, 2, 3, 4]);
        // Same length and type, new delta: type 2.
        let t2 = encode(&mut enc, 6, 173, 9, 1, &[5, 6, 7, 8]);
        assert_eq!(t2, [0x86, 0, 0, 33, 5, 6, 7, 8]);
        // Same delta again: type 3, header-less.
        let t3 = encode(&mut enc, 6, 206, 9, 1, &[9, 9, 9, 9]);
        assert_eq!(t3, [0xC6, 9, 9, 9, 9]);
        // Going back in time, or switching message stream, forces type 0.
        assert_eq!(encode(&mut enc, 6, 10, 9, 1, &[0; 4])[0], 0x06);
        assert_eq!(encode(&mut enc, 6, 50, 9, 2, &[0; 4])[0], 0x06);
    }

    #[test]
    fn type3_never_follows_type0() {
        let mut enc = ChunkEncoder::new();
        encode(&mut enc, 4, 0, 8, 1, &[1]);
        // Same length, type and a zero delta: type 2, not an ambiguous type 3.
        assert_eq!(encode(&mut enc, 4, 0, 8, 1, &[1]), [0x84, 0, 0, 0, 1]);
        assert_eq!(encode(&mut enc, 4, 0, 8, 1, &[1]), [0xC4, 1]);
    }

    #[test]
    fn basic_header_sizes() {
        let mut out = Vec::new();
        basic_header(0, 63, &mut out);
        basic_header(1, 64, &mut out);
        basic_header(2, 319, &mut out);
        basic_header(3, 320, &mut out);
        basic_header(0, 65_599, &mut out);
        assert_eq!(out, [63, 0x40, 0, 0x80, 255, 0xC1, 0, 1, 0x01, 0xFF, 0xFF]);
        for csid in [2, 63, 64, 300, 319, 320, 1000, 65_599] {
            let mut enc = ChunkEncoder::new();
            let mut dec = ChunkDecoder::new();
            dec.push(&encode(&mut enc, csid, 5, 20, 0, b"x"));
            let m = dec.next_message().unwrap().unwrap();
            assert_eq!((m.csid, m.payload.as_slice()), (csid, b"x".as_slice()));
        }
    }

    #[test]
    fn splits_at_chunk_size_boundaries() {
        let mut enc = ChunkEncoder::new();
        enc.set_chunk_size(4).unwrap();
        let out = encode(&mut enc, 5, 7, 9, 1, &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            out,
            [
                0x05, 0, 0, 7, 0, 0, 9, 9, 1, 0, 0, 0, // type 0 header
                1, 2, 3, 4, //
                0xC5, 5, 6, 7, 8, // type 3 continuation
                0xC5, 9,
            ]
        );
        // Exactly one chunk's worth needs no continuation.
        let exact = encode(&mut enc, 6, 0, 9, 1, &[1, 2, 3, 4]);
        assert_eq!(exact.len(), 12 + 4);
    }

    #[test]
    fn extended_timestamps_on_every_chunk() {
        let mut enc = ChunkEncoder::new();
        enc.set_chunk_size(2).unwrap();
        let ts = 0x0100_0000; // above the 24-bit field
        let out = encode(&mut enc, 4, ts, 8, 1, &[1, 2, 3]);
        assert_eq!(
            out,
            [
                0x04, 0xFF, 0xFF, 0xFF, 0, 0, 3, 8, 1, 0, 0, 0, // marker in the header
                0x01, 0x00, 0x00, 0x00, // extended timestamp
                1, 2, //
                0xC4, 0x01, 0x00, 0x00, 0x00, 3, // repeated on the type-3 chunk
            ]
        );
        let mut dec = ChunkDecoder::new();
        dec.set_chunk_size(2).unwrap();
        dec.push(&out);
        // A large delta is extended too.
        dec.push(&encode(&mut enc, 4, ts + 0x0100_0000, 8, 1, &[4, 5, 6]));
        let msgs = decode_all(&mut dec);
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            (msgs[0].timestamp, &msgs[0].payload[..]),
            (ts, &[1, 2, 3][..])
        );
        assert_eq!(msgs[1].timestamp, 2 * ts);
        // The exact boundary value 0xFFFFFF is itself extended.
        let edge = encode(&mut ChunkEncoder::new(), 4, 0xFF_FFFF, 8, 1, &[1]);
        assert_eq!(&edge[1..4], &[0xFF; 3]);
        assert_eq!(&edge[12..16], &[0, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn round_trip_interleaved_streams_fed_byte_by_byte() {
        let mut enc = ChunkEncoder::new();
        enc.set_chunk_size(5).unwrap();
        let big: Vec<u8> = (0..=255).collect();
        let sent = [
            (4, 0, 8, 1, vec![0xAF, 1]),
            (6, 0, 9, 1, big.clone()),
            (4, 23, 8, 1, vec![0xAF, 1]),
            (4, 46, 8, 1, vec![0xAF, 2]),
            (4, 69, 8, 1, vec![0xAF, 3]),
            (6, 33, 9, 1, big[..17].to_vec()),
            (3, 0, 20, 0, Vec::new()),
            (6, 66, 9, 1, big[..17].to_vec()),
        ];
        let mut wire = Vec::new();
        for (csid, ts, ty, sid, p) in &sent {
            enc.encode(*csid, *ts, *ty, *sid, p, &mut wire).unwrap();
        }
        let mut dec = ChunkDecoder::new();
        dec.set_chunk_size(5).unwrap();
        let mut got = Vec::new();
        for b in &wire {
            dec.push(std::slice::from_ref(b));
            got.extend(decode_all(&mut dec));
        }
        let got: Vec<_> = got
            .into_iter()
            .map(|m| (m.csid, m.timestamp, m.type_id, m.stream_id, m.payload))
            .collect();
        assert_eq!(got, sent);
    }

    #[test]
    fn rejects_protocol_violations() {
        let mut dec = ChunkDecoder::new();
        dec.push(&[0x43, 0, 0, 0, 0, 0, 1, 20, 0]); // type 1 with no base header
        assert!(matches!(
            dec.next_message(),
            Err(RtmpError::Protocol { .. })
        ));

        let mut dec = ChunkDecoder::new();
        let len = (MAX_INCOMING_MESSAGE + 1) as u32;
        let l = len.to_be_bytes();
        dec.push(&[0x03, 0, 0, 0, l[1], l[2], l[3], 20, 0, 0, 0, 0]);
        assert!(matches!(
            dec.next_message(),
            Err(RtmpError::Protocol { .. })
        ));

        let mut enc = ChunkEncoder::new();
        let mut dec = ChunkDecoder::new();
        let mut wire = encode(&mut enc, 3, 0, 20, 0, &[0; 200]);
        wire.truncate(12 + 128); // first chunk only
        wire.extend_from_slice(&[0x03, 0, 0, 0, 0, 0, 1, 20, 0, 0, 0, 0, 1]);
        dec.push(&wire);
        assert!(matches!(
            dec.next_message(),
            Err(RtmpError::Protocol { .. })
        ));
    }

    #[test]
    fn abort_discards_partial_message() {
        let mut enc = ChunkEncoder::new();
        let mut dec = ChunkDecoder::new();
        let wire = encode(&mut enc, 3, 0, 20, 0, &[7; 200]);
        dec.push(&wire[..12 + 128]);
        assert_eq!(dec.next_message().unwrap(), None);
        dec.abort(3);
        dec.push(&encode(&mut enc, 3, 0, 20, 0, &[8; 3]));
        let m = dec.next_message().unwrap().unwrap();
        assert_eq!(m.payload, [8; 3]);
    }

    #[test]
    fn chunk_size_bounds() {
        assert!(check_chunk_size(0).is_err());
        assert!(check_chunk_size(MAX_CHUNK_SIZE + 1).is_err());
        assert_eq!(check_chunk_size(4096).unwrap(), 4096);
        let mut out = Vec::new();
        assert!(matches!(
            ChunkEncoder::new().encode(3, 0, 9, 1, &vec![0; MAX_MESSAGE_LEN + 1], &mut out),
            Err(RtmpError::PayloadSize { .. })
        ));
    }
}
