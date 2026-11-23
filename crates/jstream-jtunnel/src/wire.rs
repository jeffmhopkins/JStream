//! JTunnel's packets, version 2: small UDP datagrams, little-endian.
//!
//! ```text
//! header     magic "JTUN" (4), version (1), type (1)
//! audio      stream id (4), seq (4), sample rate (4), codec (1), channels (1),
//!            frame samples (2), frame count (1), then per frame (frame i is seq - i):
//!            event count (1), event count x [offset (2), event], audio bytes (2), audio
//! event      a MIDI message: its status byte and data bytes (1 to 3 bytes in all)
//!            a SysEx piece: tag 1, flags (1: first, 2: last), index (1), size (1), bytes
//!            a note expression: tag 2, id (1), channel (1), key (1), value (8, f64),
//!            with 255 for a channel or key of -1
//! subscribe  name length (1), name (no terminator)
//! bye        nothing
//! state      stream id (4), message id (4), piece (1), piece count (1), key length (1),
//!            key, data length (2), data
//! names      count (1), count x [slot (1), name length (1), name]
//! ```
//!
//! A state packet (type 4, JTunnel 0.13 and later) is one piece of a JSON message under a
//! key, the sender's side channel of what its DAW is doing (ADR-0171 decision 1);
//! [`StateAssembler`](crate::StateAssembler) puts the pieces back together.
//!
//! A names packet (type 5, JTunnel 0.15 and later) goes the other way, from a receiver to
//! a sender it subscribes to: what the receiver calls each of the sender's
//! [`MOD_SLOTS`] modulation params, which the sender streams as its `params` key, so the
//! DAW shows the names ([ADR-0181](../../../docs/adr/0181-jstream-names-a-jtunnel-senders-params.md)).
//! The list is whole: a slot it leaves out has no name. [`write_names`] writes one;
//! JStream never parses one, since it never takes names.
//!
//! Parsing checks everything the sender's own parser checks, so a packet JTunnel would
//! refuse is refused here too, and a parsed packet's events can be walked without
//! further errors.

use jstream_midi::{Channel, Message};
use std::fmt;

/// The packet magic.
pub const MAGIC: [u8; 4] = *b"JTUN";
/// The wire version this crate speaks. JTunnel 0.11 and later send it; an older or newer
/// version is refused as [`WireError::Version`].
pub const PROTOCOL_VERSION: u8 = 2;
/// Channels in every stream: stereo.
pub const CHANNELS: u8 = 2;
/// Opus frames: 5 ms at 48 kHz.
pub const OPUS_FRAME_SAMPLES: u16 = 240;
/// Opus streams' sample rate.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;
/// The most samples a PCM frame holds: 2 ms at 48 kHz.
pub const PCM_FRAME_SAMPLES: u16 = 96;
/// The most samples any frame holds.
pub const MAX_FRAME_SAMPLES: u16 = 240;
/// The most audio bytes a frame carries: a full PCM frame.
pub const MAX_FRAME_BYTES: usize = PCM_FRAME_SAMPLES as usize * CHANNELS as usize * 3;
/// The most events a frame carries.
pub const MAX_EVENTS_PER_FRAME: usize = 24;
/// The most frames a packet carries: the newest, and with redundancy the one before.
pub const MAX_FRAMES_PER_PACKET: usize = 2;
/// The largest packet: a 1500-byte MTU's UDP payload, so packets never fragment.
pub const MAX_PACKET_BYTES: usize = 1472;
/// The longest tunnel name, in bytes.
pub const MAX_TUNNEL_NAME: usize = 31;
/// The most bytes one event takes.
pub const MAX_EVENT_BYTES: usize = 64;
/// The most bytes of SysEx one piece carries.
pub const SYSEX_PIECE_BYTES: usize = MAX_EVENT_BYTES - 4;
/// The longest SysEx message carried (256 pieces). Longer ones are never sent.
pub const MAX_SYSEX_BYTES: usize = 256 * SYSEX_PIECE_BYTES;

/// The longest state key, in bytes.
pub const MAX_STATE_KEY: usize = 63;
/// The most bytes of a state message one piece carries.
pub const STATE_PIECE_BYTES: usize = 1200;
/// The most pieces a state message goes in.
pub const MAX_STATE_PIECES: u8 = 64;
/// The longest state message: every piece full.
pub const MAX_STATE_BYTES: usize = STATE_PIECE_BYTES * MAX_STATE_PIECES as usize;

const HEADER_BYTES: usize = 6;
const TAG_SYSEX: u8 = 1;
const TAG_EXPRESSION: u8 = 2;
const EXPRESSION_BYTES: usize = 12;
const SYSEX_FIRST: u8 = 1;
const SYSEX_LAST: u8 = 2;

/// A packet's type byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    /// Frames of audio and MIDI, from a sender.
    Audio = 1,
    /// A receiver asking for a tunnel, every half second as a keepalive.
    Subscribe = 2,
    /// A receiver leaving at once.
    Bye = 3,
    /// One piece of a state message, from a sender (ADR-0171 decision 1).
    State = 4,
    /// A receiver's names for its sender's modulation params (ADR-0181).
    Names = 5,
}

/// How a stream's audio is coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Opus at 48 kHz, 5 ms frames.
    Opus = 0,
    /// Uncompressed 24-bit PCM, interleaved, at the sender's rate, up to 2 ms frames.
    Pcm24 = 1,
    /// No audio: MIDI only, in 1 ms frames.
    None = 2,
}

/// Why a datagram isn't a JTunnel packet this crate accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Too short, or without the `JTUN` magic: not JTunnel at all.
    NotJTunnel,
    /// A JTunnel packet of another wire version.
    Version(u8),
    /// A type byte this version doesn't define.
    Type(u8),
    /// A JTunnel packet of this version that is malformed: the field named is out of
    /// range, or the packet ends early or runs on.
    Malformed(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJTunnel => f.write_str("not a JTunnel packet"),
            Self::Version(v) => write!(f, "JTunnel wire version {v}, not {PROTOCOL_VERSION}"),
            Self::Type(t) => write!(f, "unknown JTunnel packet type {t}"),
            Self::Malformed(what) => write!(f, "malformed JTunnel packet: {what}"),
        }
    }
}

impl std::error::Error for WireError {}

/// An audio packet's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    /// The sender's stream, new each time it starts, so a restart is seen.
    pub stream_id: u32,
    /// The newest frame's sequence number.
    pub seq: u32,
    /// The stream's sample rate, 8 kHz to 384 kHz.
    pub sample_rate: u32,
    /// How the audio is coded.
    pub codec: Codec,
    /// Samples per frame, per channel.
    pub frame_samples: u16,
}

/// One frame of an audio packet: a few milliseconds of audio and the events in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Its sequence number.
    pub seq: u32,
    event_count: u8,
    events: &'a [u8],
    /// Its coded audio: Opus bytes, interleaved 24-bit PCM, or nothing.
    pub audio: &'a [u8],
}

impl<'a> Frame<'a> {
    /// The frame's events in order, each with its offset in samples from the frame's
    /// start.
    #[must_use]
    pub fn events(&self) -> Events<'a> {
        Events {
            rest: self.events,
            left: self.event_count,
        }
    }

    /// The frame's events as their wire bytes, each with its offset: what
    /// [`FrameOut::events`] takes to write the frame again.
    #[must_use]
    pub fn raw_events(&self) -> RawEvents<'a> {
        RawEvents(self.events())
    }

    /// How many events the frame carries.
    #[must_use]
    pub fn event_count(&self) -> usize {
        usize::from(self.event_count)
    }
}

/// An event in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event<'a> {
    /// A MIDI message.
    Midi(Message),
    /// A status byte JTunnel carries but MIDI 1.0 leaves undefined (`F4`, `F5`, `F9`,
    /// `FD`), as its one byte.
    Undefined(u8),
    /// One piece of a SysEx message; [`SysexAssembler`](crate::SysexAssembler) joins
    /// them.
    SysexPiece(SysexPiece<'a>),
    /// A CLAP note expression (MPE-style per-note tuning, pressure and so on).
    Expression(Expression),
}

/// One piece of a SysEx message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SysexPiece<'a> {
    /// The message's first piece.
    pub first: bool,
    /// The message's last piece.
    pub last: bool,
    /// Its place in the message, from 0.
    pub index: u8,
    /// Its bytes: 1 to [`SYSEX_PIECE_BYTES`].
    pub data: &'a [u8],
}

/// A CLAP note expression.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expression {
    /// CLAP's expression id: 0 volume, 1 pan, 2 tuning, 3 vibrato, 4 expression,
    /// 5 brightness, 6 pressure.
    pub id: u8,
    /// The channel, or `None` for every channel.
    pub channel: Option<Channel>,
    /// The key, or `None` for every key.
    pub key: Option<u8>,
    /// The value, in CLAP's units for the id.
    pub value: f64,
}

/// The events of a [`Frame`], from [`Frame::events`].
#[derive(Clone, Debug)]
pub struct Events<'a> {
    rest: &'a [u8],
    left: u8,
}

impl<'a> Iterator for Events<'a> {
    type Item = (u16, Event<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let mut raw = RawEvents(self.clone());
        let (offset, event) = raw.next()?;
        *self = raw.0;
        Some((offset, decode_event(event)))
    }
}

/// The events of a [`Frame`] as wire bytes, from [`Frame::raw_events`].
#[derive(Clone, Debug)]
pub struct RawEvents<'a>(Events<'a>);

impl<'a> Iterator for RawEvents<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let events = &mut self.0;
        if events.left == 0 {
            return None;
        }
        events.left -= 1;
        let offset = u16::from_le_bytes([*events.rest.first()?, *events.rest.get(1)?]);
        let body = events.rest.get(2..)?;
        let (event, rest) = body.split_at(event_size(body)?);
        events.rest = rest;
        Some((offset, event))
    }
}

/// The length of the MIDI message a status byte starts as JTunnel counts it: 0 for a
/// data byte, `F0` and `F7`; 1 for the undefined system statuses.
fn wire_message_len(status: u8) -> usize {
    match status {
        0x00..=0x7F | 0xF0 | 0xF7 => 0,
        0x80..=0xEF if matches!(status & 0xF0, 0xC0 | 0xD0) => 2,
        0x80..=0xEF => 3,
        0xF2 => 3,
        0xF1 | 0xF3 => 2,
        _ => 1,
    }
}

/// The size of the event at the start of `data`, or `None` if it isn't a well-formed one.
fn event_size(data: &[u8]) -> Option<usize> {
    let tag = *data.first()?;
    let size = match tag {
        TAG_SYSEX => {
            let n = usize::from(*data.get(3)?);
            if !(1..=SYSEX_PIECE_BYTES).contains(&n) {
                return None;
            }
            4 + n
        }
        TAG_EXPRESSION => EXPRESSION_BYTES,
        _ => wire_message_len(tag),
    };
    if size == 0 || size > data.len() {
        return None;
    }
    // A MIDI message's data bytes are seven bits.
    if tag >= 0x80 && data[1..size].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    Some(size)
}

/// Decodes one event from its wire bytes (as [`Frame::raw_events`] gives them), or
/// `None` if the bytes aren't exactly one well-formed event.
#[must_use]
pub fn decode(event: &[u8]) -> Option<Event<'_>> {
    (event_size(event)? == event.len()).then(|| decode_event(event))
}

fn decode_event(event: &[u8]) -> Event<'_> {
    match event[0] {
        TAG_SYSEX => Event::SysexPiece(SysexPiece {
            first: event[1] & SYSEX_FIRST != 0,
            last: event[1] & SYSEX_LAST != 0,
            index: event[2],
            data: &event[4..],
        }),
        TAG_EXPRESSION => {
            let mut bits = [0; 8];
            bits.copy_from_slice(&event[4..12]);
            Event::Expression(Expression {
                id: event[1],
                channel: Channel::new(event[2]),
                key: (event[3] < 128).then_some(event[3]),
                value: f64::from_le_bytes(bits),
            })
        }
        status => match Message::parse(event) {
            Ok(m) => Event::Midi(m),
            Err(_) => Event::Undefined(status),
        },
    }
}

/// A parsed packet, borrowing the datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    /// Audio frames from a sender: the newest first, then (with redundancy) the one
    /// before it.
    Audio {
        /// The header.
        header: AudioHeader,
        /// The frames: one or two, newest first.
        frames: [Option<Frame<'a>>; MAX_FRAMES_PER_PACKET],
    },
    /// A receiver's subscription to the tunnel `name` (empty: whatever sends at this
    /// address).
    Subscribe {
        /// The tunnel's name, as bytes: JTunnel doesn't promise UTF-8.
        name: &'a [u8],
    },
    /// A receiver leaving.
    Bye,
    /// One piece of a sender's state message.
    State(StatePiece<'a>),
}

/// One piece of a state message (ADR-0171 decision 1): JSON under a key such as
/// `transport` or `track/3`, cut into up to [`MAX_STATE_PIECES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatePiece<'a> {
    /// The sender's state stream: a new one means it started again.
    pub stream_id: u32,
    /// The same for every piece of one message; a resend repeats it.
    pub message_id: u32,
    /// This piece's place, from 0, below `pieces`.
    pub piece: u8,
    /// How many pieces the message has: 1 to [`MAX_STATE_PIECES`].
    pub pieces: u8,
    /// The key: 1 to [`MAX_STATE_KEY`] of ASCII letters, digits, `/`, `_`, `.` and `-`
    /// ([`is_state_key`]), so it is always UTF-8.
    pub key: &'a str,
    /// This piece's bytes of the message: at most [`STATE_PIECE_BYTES`].
    pub data: &'a [u8],
}

/// Whether `key` is a state key JTunnel sends: 1 to [`MAX_STATE_KEY`] of ASCII letters,
/// digits, `/`, `_`, `.` and `-`.
#[must_use]
pub fn is_state_key(key: &[u8]) -> bool {
    (1..=MAX_STATE_KEY).contains(&key.len())
        && key
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'.' | b'-'))
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], WireError> {
        if n > self.data.len() {
            return Err(WireError::Malformed(what));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }
    fn u8(&mut self, what: &'static str) -> Result<u8, WireError> {
        Ok(self.take(1, what)?[0])
    }
    fn u16(&mut self, what: &'static str) -> Result<u16, WireError> {
        let b = self.take(2, what)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self, what: &'static str) -> Result<u32, WireError> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Parses one datagram.
///
/// # Errors
///
/// A [`WireError`]: not JTunnel, another version, an unknown type, or malformed.
pub fn parse(datagram: &[u8]) -> Result<Packet<'_>, WireError> {
    if datagram.len() < HEADER_BYTES || datagram[..4] != MAGIC {
        return Err(WireError::NotJTunnel);
    }
    if datagram[4] != PROTOCOL_VERSION {
        return Err(WireError::Version(datagram[4]));
    }
    let mut r = Reader {
        data: &datagram[HEADER_BYTES..],
    };
    let packet = match datagram[5] {
        1 => parse_audio(&mut r)?,
        2 => {
            let n = usize::from(r.u8("name length")?);
            if n > MAX_TUNNEL_NAME {
                return Err(WireError::Malformed("name length"));
            }
            Packet::Subscribe {
                name: r.take(n, "name")?,
            }
        }
        3 => Packet::Bye,
        4 => return parse_state(&mut r),
        t => return Err(WireError::Type(t)),
    };
    // Bytes after the packet's end are ignored, as the sender's own parser ignores them.
    Ok(packet)
}

/// A state packet ends with its data, as JTunnel's own parser insists: bytes after it
/// are refused, unlike the other types'.
fn parse_state<'a>(r: &mut Reader<'a>) -> Result<Packet<'a>, WireError> {
    let stream_id = r.u32("stream id")?;
    let message_id = r.u32("message id")?;
    let piece = r.u8("piece")?;
    let pieces = r.u8("piece count")?;
    let key_len = usize::from(r.u8("key length")?);
    if pieces == 0 || pieces > MAX_STATE_PIECES {
        return Err(WireError::Malformed("piece count"));
    }
    if piece >= pieces {
        return Err(WireError::Malformed("piece"));
    }
    let key = r.take(key_len, "key")?;
    if !is_state_key(key) {
        return Err(WireError::Malformed("key"));
    }
    let data_len = usize::from(r.u16("data length")?);
    if data_len > STATE_PIECE_BYTES {
        return Err(WireError::Malformed("data length"));
    }
    if r.data.len() != data_len {
        return Err(WireError::Malformed("state data"));
    }
    let data = r.take(data_len, "state data")?;
    Ok(Packet::State(StatePiece {
        stream_id,
        message_id,
        piece,
        pieces,
        // `is_state_key` admits ASCII only.
        key: std::str::from_utf8(key).map_err(|_| WireError::Malformed("key"))?,
        data,
    }))
}

fn parse_audio<'a>(r: &mut Reader<'a>) -> Result<Packet<'a>, WireError> {
    let stream_id = r.u32("stream id")?;
    let seq = r.u32("seq")?;
    let sample_rate = r.u32("sample rate")?;
    let codec = match r.u8("codec")? {
        0 => Codec::Opus,
        1 => Codec::Pcm24,
        2 => Codec::None,
        _ => return Err(WireError::Malformed("codec")),
    };
    if r.u8("channels")? != CHANNELS {
        return Err(WireError::Malformed("channels"));
    }
    let frame_samples = r.u16("frame samples")?;
    let count = usize::from(r.u8("frame count")?);
    if frame_samples == 0 || frame_samples > MAX_FRAME_SAMPLES {
        return Err(WireError::Malformed("frame samples"));
    }
    if !(8_000..=384_000).contains(&sample_rate) {
        return Err(WireError::Malformed("sample rate"));
    }
    if !(1..=MAX_FRAMES_PER_PACKET).contains(&count) {
        return Err(WireError::Malformed("frame count"));
    }
    match codec {
        Codec::Opus if sample_rate != OPUS_SAMPLE_RATE || frame_samples != OPUS_FRAME_SAMPLES => {
            return Err(WireError::Malformed("Opus frame"));
        }
        Codec::Pcm24 if frame_samples > PCM_FRAME_SAMPLES => {
            return Err(WireError::Malformed("PCM frame samples"));
        }
        _ => {}
    }
    let header = AudioHeader {
        stream_id,
        seq,
        sample_rate,
        codec,
        frame_samples,
    };
    let mut frames = [None; MAX_FRAMES_PER_PACKET];
    for (i, slot) in frames.iter_mut().enumerate().take(count) {
        let event_count = r.u8("event count")?;
        if usize::from(event_count) > MAX_EVENTS_PER_FRAME {
            return Err(WireError::Malformed("event count"));
        }
        let start = r.data;
        let mut used = 0;
        for _ in 0..event_count {
            let offset = r.u16("event offset")?;
            if offset >= frame_samples {
                return Err(WireError::Malformed("event offset"));
            }
            let size = event_size(&r.data[..r.data.len().min(MAX_EVENT_BYTES)])
                .ok_or(WireError::Malformed("event"))?;
            r.take(size, "event")?;
            used += 2 + size;
        }
        let audio_bytes = usize::from(r.u16("audio bytes")?);
        if audio_bytes > MAX_FRAME_BYTES {
            return Err(WireError::Malformed("audio bytes"));
        }
        match codec {
            Codec::Pcm24
                if audio_bytes != usize::from(frame_samples) * usize::from(CHANNELS) * 3 =>
            {
                return Err(WireError::Malformed("PCM audio bytes"));
            }
            Codec::None if audio_bytes != 0 => {
                return Err(WireError::Malformed("audio in a MIDI-only frame"));
            }
            _ => {}
        }
        let audio = r.take(audio_bytes, "audio")?;
        *slot = Some(Frame {
            seq: seq.wrapping_sub(i as u32),
            event_count,
            events: &start[..used],
            audio,
        });
    }
    Ok(Packet::Audio { header, frames })
}

fn write_header(out: &mut [u8], kind: PacketType) -> Option<usize> {
    let head = out.get_mut(..HEADER_BYTES)?;
    head[..4].copy_from_slice(&MAGIC);
    head[4] = PROTOCOL_VERSION;
    head[5] = kind as u8;
    Some(HEADER_BYTES)
}

/// Writes a subscribe packet for `name` (cut to [`MAX_TUNNEL_NAME`] bytes) into `out`,
/// and gives its length, or `None` if `out` is too small.
#[must_use]
pub fn write_subscribe(out: &mut [u8], name: &[u8]) -> Option<usize> {
    let name = &name[..name.len().min(MAX_TUNNEL_NAME)];
    let at = write_header(out, PacketType::Subscribe)?;
    let end = at + 1 + name.len();
    let body = out.get_mut(at..end)?;
    body[0] = name.len() as u8;
    body[1..].copy_from_slice(name);
    Some(end)
}

/// Writes a bye packet into `out`, and gives its length, or `None` if `out` is too small.
#[must_use]
pub fn write_bye(out: &mut [u8]) -> Option<usize> {
    write_header(out, PacketType::Bye)
}

/// How many modulation params a JTunnel sender has, each of which a names packet may name.
pub const MOD_SLOTS: usize = 32;
/// The longest name a names packet carries, in bytes, so all [`MOD_SLOTS`] fit one packet.
pub const MAX_MOD_NAME: usize = 40;

/// Whether `name` may go in a names packet as JTunnel checks it: empty (no name), or at
/// most [`MAX_MOD_NAME`] bytes with no control character and no space at either end.
#[must_use]
pub fn is_mod_name(name: &str) -> bool {
    name.len() <= MAX_MOD_NAME
        && !name.chars().any(char::is_control)
        && !name.starts_with(' ')
        && !name.ends_with(' ')
}

/// Writes a names packet into `out`: `names[i]` names slot `i`, and an empty one leaves
/// it out. Gives its length, or `None` if `out` is too small, there are not exactly
/// [`MOD_SLOTS`] names, or one is not [`is_mod_name`].
#[must_use]
pub fn write_names(out: &mut [u8], names: &[String]) -> Option<usize> {
    if names.len() != MOD_SLOTS || !names.iter().all(|name| is_mod_name(name)) {
        return None;
    }
    let mut at = write_header(out, PacketType::Names)?;
    let count = names.iter().filter(|name| !name.is_empty()).count();
    *out.get_mut(at)? = u8::try_from(count).ok()?;
    at += 1;
    for (slot, name) in names.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let end = at + 2 + name.len();
        let body = out.get_mut(at..end)?;
        body[0] = u8::try_from(slot).ok()?;
        body[1] = u8::try_from(name.len()).ok()?;
        body[2..].copy_from_slice(name.as_bytes());
        at = end;
    }
    Some(at)
}

/// Writes one state piece into `out`, as a sender does, and gives its length, or `None`
/// if `out` is too small or the piece is one [`parse`] would refuse (a bad key, too much
/// data, a piece past the count). JStream doesn't send state; this is for tests and
/// fuzzing.
#[must_use]
pub fn write_state(out: &mut [u8], piece: &StatePiece<'_>) -> Option<usize> {
    if !is_state_key(piece.key.as_bytes())
        || piece.data.len() > STATE_PIECE_BYTES
        || piece.pieces == 0
        || piece.pieces > MAX_STATE_PIECES
        || piece.piece >= piece.pieces
    {
        return None;
    }
    let mut at = write_header(out, PacketType::State)?;
    let mut put = |bytes: &[u8]| -> Option<()> {
        out.get_mut(at..at + bytes.len())?.copy_from_slice(bytes);
        at += bytes.len();
        Some(())
    };
    put(&piece.stream_id.to_le_bytes())?;
    put(&piece.message_id.to_le_bytes())?;
    put(&[piece.piece, piece.pieces, piece.key.len() as u8])?;
    put(piece.key.as_bytes())?;
    put(&(piece.data.len() as u16).to_le_bytes())?;
    put(piece.data)?;
    Some(at)
}

/// One frame to write with [`write_audio`].
#[derive(Clone, Copy, Debug)]
pub struct FrameOut<'a> {
    /// Events, each an offset in samples and the event's wire bytes (a MIDI message,
    /// or a SysEx piece or note expression in its tagged form).
    pub events: &'a [(u16, &'a [u8])],
    /// The coded audio.
    pub audio: &'a [u8],
}

/// Writes an audio packet: `frames` newest first, the first with `header.seq`. Gives its
/// length, or `None` if `out` is too small, or there are no frames or too many, or a frame
/// has too many events or an event that isn't a whole event. It doesn't check what the bytes
/// mean (an event's offset against the frame's length, or the audio's length against the
/// codec), so a test can write packets [`parse`] refuses. JStream receives JTunnel and
/// doesn't send it; this is for tests and fuzzing.
#[must_use]
pub fn write_audio(out: &mut [u8], header: &AudioHeader, frames: &[FrameOut<'_>]) -> Option<usize> {
    if !(1..=MAX_FRAMES_PER_PACKET).contains(&frames.len()) {
        return None;
    }
    let mut at = write_header(out, PacketType::Audio)?;
    let mut put = |bytes: &[u8]| -> Option<()> {
        out.get_mut(at..at + bytes.len())?.copy_from_slice(bytes);
        at += bytes.len();
        Some(())
    };
    put(&header.stream_id.to_le_bytes())?;
    put(&header.seq.to_le_bytes())?;
    put(&header.sample_rate.to_le_bytes())?;
    put(&[header.codec as u8, CHANNELS])?;
    put(&header.frame_samples.to_le_bytes())?;
    put(&[frames.len() as u8])?;
    for f in frames {
        if f.events.len() > MAX_EVENTS_PER_FRAME {
            return None;
        }
        put(&[f.events.len() as u8])?;
        for (offset, event) in f.events {
            if event_size(event) != Some(event.len()) {
                return None;
            }
            put(&offset.to_le_bytes())?;
            put(event)?;
        }
        put(&u16::try_from(f.audio.len()).ok()?.to_le_bytes())?;
        put(f.audio)?;
    }
    Some(at)
}

/// Decodes a 24-bit PCM frame's interleaved samples into `out`, as JTunnel does (full
/// scale is 8388607), and gives how many samples it wrote: the smaller of the frame's
/// and `out`'s.
pub fn decode_pcm24(audio: &[u8], out: &mut [f32]) -> usize {
    let mut n = 0;
    for (dst, src) in out.iter_mut().zip(audio.chunks_exact(3)) {
        let v = i32::from(src[0]) | i32::from(src[1]) << 8 | i32::from(src[2] as i8) << 16;
        *dst = v as f32 / 8_388_607.0;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_names_packet_is_jtunnels_layout_and_refuses_what_jtunnel_would() {
        let mut names = vec![String::new(); MOD_SLOTS];
        names[4] = "Camera 1 Pan".into();
        names[31] = "Fog dénsité".into();
        let mut out = [0_u8; MAX_PACKET_BYTES];
        let len = write_names(&mut out, &names).expect("fits");
        let mut want = b"JTUN\x02\x05\x02\x04\x0cCamera 1 Pan\x1f".to_vec();
        want.push(u8::try_from("Fog dénsité".len()).expect("short"));
        want.extend_from_slice("Fog dénsité".as_bytes());
        assert_eq!(&out[..len], &want[..]);
        // No names: an empty list, which clears every slot.
        let none = vec![String::new(); MOD_SLOTS];
        assert_eq!(write_names(&mut out, &none), Some(7));
        assert_eq!(out[5..7], [5, 0]);
        // Every slot at the longest name fits one packet.
        let longest = vec!["x".repeat(MAX_MOD_NAME); MOD_SLOTS];
        assert!(write_names(&mut out, &longest).is_some_and(|len| len <= MAX_PACKET_BYTES));
        for bad in [
            "x".repeat(MAX_MOD_NAME + 1),
            "a\nb".into(),
            " Pan".into(),
            "Pan ".into(),
            "\u{85}".into(),
        ] {
            let mut names = none.clone();
            names[0] = bad;
            assert_eq!(write_names(&mut out, &names), None);
        }
        assert_eq!(write_names(&mut out, &none[1..]), None);
        assert_eq!(write_names(&mut out[..20], &longest), None);
    }

    fn header(codec: Codec, frame_samples: u16) -> AudioHeader {
        AudioHeader {
            stream_id: 7,
            seq: 1000,
            sample_rate: 48_000,
            codec,
            frame_samples,
        }
    }

    fn audio_frames<'a>(packet: &Packet<'a>) -> Vec<Frame<'a>> {
        match packet {
            Packet::Audio { frames, .. } => frames.iter().flatten().copied().collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_pcm_packet_with_midi_round_trips() {
        let pcm = [0_u8; MAX_FRAME_BYTES];
        let expression = {
            let mut e = [TAG_EXPRESSION, 2, 255, 60, 0, 0, 0, 0, 0, 0, 0, 0];
            e[4..].copy_from_slice(&0.5_f64.to_le_bytes());
            e
        };
        let events: [(u16, &[u8]); 4] = [
            (0, &[0x90, 60, 100]),
            (5, &[0xF8]),
            (
                6,
                &[TAG_SYSEX, SYSEX_FIRST | SYSEX_LAST, 0, 3, 0xF0, 0x7E, 0xF7],
            ),
            (95, &expression),
        ];
        let newest = FrameOut {
            events: &events,
            audio: &pcm,
        };
        let older = FrameOut {
            events: &[],
            audio: &pcm,
        };
        let mut buf = [0; MAX_PACKET_BYTES];
        let n = write_audio(&mut buf, &header(Codec::Pcm24, 96), &[newest, older]);
        let n = n.unwrap_or(0);
        assert!(n > 0 && n <= MAX_PACKET_BYTES);
        let packet = parse(&buf[..n]).unwrap();
        let frames = audio_frames(&packet);
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].seq, frames[1].seq), (1000, 999));
        let got: Vec<_> = frames[0].events().collect();
        assert_eq!(got.len(), 4);
        assert!(matches!(
            got[0],
            (
                0,
                Event::Midi(Message::NoteOn {
                    key: 60,
                    velocity: 100,
                    ..
                })
            )
        ));
        assert_eq!(got[1], (5, Event::Midi(Message::TimingClock)));
        assert!(matches!(
            got[2],
            (6, Event::SysexPiece(SysexPiece { first: true, last: true, index: 0, data })) if data == [0xF0, 0x7E, 0xF7]
        ));
        assert!(matches!(
            got[3],
            (95, Event::Expression(Expression { id: 2, channel: None, key: Some(60), value })) if value == 0.5
        ));
        assert_eq!(frames[1].event_count(), 0);
    }

    #[test]
    fn subscribe_and_bye_round_trip() {
        let mut buf = [0; 64];
        let n = write_subscribe(&mut buf, b"Studio Mac 1").unwrap_or(0);
        assert_eq!(
            parse(&buf[..n]),
            Ok(Packet::Subscribe {
                name: b"Studio Mac 1"
            })
        );
        let long = [b'x'; 40];
        let n = write_subscribe(&mut buf, &long).unwrap_or(0);
        assert_eq!(
            parse(&buf[..n]),
            Ok(Packet::Subscribe { name: &long[..31] })
        );
        let n = write_bye(&mut buf).unwrap_or(0);
        assert_eq!(parse(&buf[..n]), Ok(Packet::Bye));
        assert_eq!(write_subscribe(&mut [0; 4], b"a"), None);
    }

    #[test]
    fn the_header_is_checked_first() {
        assert_eq!(parse(b"JTUN"), Err(WireError::NotJTunnel));
        assert_eq!(parse(b"XTUN\x02\x01"), Err(WireError::NotJTunnel));
        assert_eq!(parse(b"JTUN\x01\x01"), Err(WireError::Version(1)));
        assert_eq!(parse(b"JTUN\x02\x09"), Err(WireError::Type(9)));
    }

    /// Bytes written by JTunnel 0.13's own `writeState` (Jclap's
    /// `plugins/jtunnel/src/protocol.cpp`, compiled and run once): this parser reads what
    /// JTunnel writes, and the writer writes the same bytes.
    #[test]
    fn reads_and_writes_the_state_pieces_jtunnel_writes() {
        let bytes =
            unhex("4a54554e0204d4c3b2a107000000010207747261636b2f330d007b226d757465223a747275657d");
        let piece = StatePiece {
            stream_id: 0xA1B2_C3D4,
            message_id: 7,
            piece: 1,
            pieces: 2,
            key: "track/3",
            data: br#"{"mute":true}"#,
        };
        assert_eq!(parse(&bytes), Ok(Packet::State(piece)));
        let mut buf = [0; MAX_PACKET_BYTES];
        let n = write_state(&mut buf, &piece).unwrap_or(0);
        assert_eq!(&buf[..n], &bytes[..]);
    }

    #[test]
    fn malformed_state_is_refused() {
        let mut buf = [0; MAX_PACKET_BYTES];
        let full = [b'x'; STATE_PIECE_BYTES];
        let piece = StatePiece {
            stream_id: 1,
            message_id: 2,
            piece: 0,
            pieces: 1,
            key: "transport",
            data: &full,
        };
        let n = write_state(&mut buf, &piece).unwrap_or(0);
        assert!(
            n <= MAX_PACKET_BYTES && n > 0,
            "a full piece fits one packet"
        );
        assert!(parse(&buf[..n]).is_ok());
        for cut in HEADER_BYTES..n {
            assert!(parse(&buf[..cut]).is_err(), "cut at {cut}");
        }
        let mut long = buf[..n].to_vec();
        long.push(0);
        assert_eq!(parse(&long), Err(WireError::Malformed("state data")));
        let with = |at: usize, value: u8| {
            let mut b = buf[..n].to_vec();
            b[at] = value;
            parse(&b).map(|_| ())
        };
        // The piece count, the piece, the key length and the key's characters.
        assert_eq!(with(15, 0), Err(WireError::Malformed("piece count")));
        assert_eq!(with(15, 65), Err(WireError::Malformed("piece count")));
        assert_eq!(with(14, 1), Err(WireError::Malformed("piece")));
        assert_eq!(with(16, 0), Err(WireError::Malformed("key")));
        assert_eq!(with(17, b' '), Err(WireError::Malformed("key")));
        assert_eq!(with(17, 0xC3), Err(WireError::Malformed("key")));
        // A data length over a piece's, even with that many bytes there.
        let mut over = buf[..n].to_vec();
        let at = 17 + "transport".len();
        over[at..at + 2].copy_from_slice(&1201_u16.to_le_bytes());
        over.push(b'x');
        assert_eq!(parse(&over), Err(WireError::Malformed("data length")));
        // The writer refuses what the parser would.
        let bad = |p: StatePiece<'_>| write_state(&mut [0; MAX_PACKET_BYTES], &p);
        assert_eq!(bad(StatePiece { key: "", ..piece }), None);
        assert_eq!(
            bad(StatePiece {
                key: "a b",
                ..piece
            }),
            None
        );
        assert_eq!(
            bad(StatePiece {
                key: &"k".repeat(64),
                ..piece
            }),
            None
        );
        assert_eq!(bad(StatePiece { piece: 1, ..piece }), None);
        assert_eq!(
            bad(StatePiece {
                pieces: 65,
                ..piece
            }),
            None
        );
        assert!(
            bad(StatePiece {
                key: &"k".repeat(63),
                pieces: 64,
                piece: 63,
                data: b"",
                ..piece
            })
            .is_some()
        );
        assert!(is_state_key(b"track/12") && is_state_key(b"a.b_c-d"));
        assert!(!is_state_key(b"track 1") && !is_state_key("é".as_bytes()));
    }

    #[test]
    fn malformed_audio_is_refused() {
        let mut buf = [0; MAX_PACKET_BYTES];
        let midi_only = FrameOut {
            events: &[(0, &[0xB0, 7, 100])],
            audio: &[],
        };
        let n = write_audio(&mut buf, &header(Codec::None, 48), &[midi_only]).unwrap_or(0);
        assert!(parse(&buf[..n]).is_ok());
        // Cut short anywhere, it's refused.
        for cut in HEADER_BYTES..n {
            assert!(parse(&buf[..cut]).is_err(), "cut at {cut}");
        }
        // A trailing byte is ignored, as JTunnel ignores it.
        assert!(parse(&buf[..=n]).is_ok());
        // An event past the frame's end.
        let mut bad = buf;
        bad[HEADER_BYTES + 17 + 1] = 48;
        assert_eq!(parse(&bad[..n]), Err(WireError::Malformed("event offset")));
        // A data byte with its top bit set.
        let mut bad = buf;
        bad[n - 3] = 0x87;
        assert_eq!(parse(&bad[..n]), Err(WireError::Malformed("event")));
        // Opus must be 48 kHz in 240-sample frames.
        let opus = FrameOut {
            events: &[],
            audio: &[1, 2, 3],
        };
        let mut h = header(Codec::Opus, 240);
        h.sample_rate = 44_100;
        let n = write_audio(&mut buf, &h, &[opus]).unwrap_or(0);
        assert_eq!(parse(&buf[..n]), Err(WireError::Malformed("Opus frame")));
        // PCM must carry exactly its frame.
        let short = FrameOut {
            events: &[],
            audio: &[0; 6],
        };
        let n = write_audio(&mut buf, &header(Codec::Pcm24, 96), &[short]).unwrap_or(0);
        assert_eq!(
            parse(&buf[..n]),
            Err(WireError::Malformed("PCM audio bytes"))
        );
        // The writer refuses what the parser would.
        let bad_event = FrameOut {
            events: &[(0, &[0x90, 60])],
            audio: &[],
        };
        assert_eq!(
            write_audio(&mut buf, &header(Codec::None, 48), &[bad_event]),
            None
        );
    }

    #[test]
    fn undefined_statuses_are_carried_not_refused() {
        let mut buf = [0; 64];
        let f = FrameOut {
            events: &[(1, &[0xF9])],
            audio: &[],
        };
        let n = write_audio(&mut buf, &header(Codec::None, 48), &[f]).unwrap_or(0);
        let packet = parse(&buf[..n]).unwrap();
        let frames = audio_frames(&packet);
        assert_eq!(
            frames[0].events().collect::<Vec<_>>(),
            [(1, Event::Undefined(0xF9))]
        );
    }

    #[test]
    fn pcm24_decodes_at_jtunnels_scale() {
        let bytes = [0xFF, 0xFF, 0x7F, 0x01, 0x00, 0x80, 0, 0, 0];
        let mut out = [9.0_f32; 4];
        assert_eq!(decode_pcm24(&bytes, &mut out), 3);
        assert_eq!(out[0], 1.0);
        assert!((out[1] - (-8_388_607.0 / 8_388_607.0)).abs() < 1e-6);
        assert_eq!(out[2], 0.0);
        assert_eq!(out[3], 9.0);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
            .collect()
    }

    /// Bytes written by JTunnel's own `writeAudio` and `writeSubscribe` (Jclap's
    /// `plugins/jtunnel/src/protocol.cpp`, compiled and run once): this parser reads what
    /// JTunnel writes, and the writer writes the same bytes.
    #[test]
    fn reads_and_writes_the_bytes_jtunnel_writes() {
        let audio = unhex(
            "4a54554e0201d4c3b2a14d00000080bb0000020230000203000091405a0a00f82f000206014000\
             0000000000d03f000001010001030003f043f70000",
        );
        let packet = parse(&audio).unwrap();
        let Packet::Audio { header, frames } = packet else {
            panic!("{packet:?}");
        };
        assert_eq!(
            header,
            AudioHeader {
                stream_id: 0xA1B2_C3D4,
                seq: 77,
                sample_rate: 48_000,
                codec: Codec::None,
                frame_samples: 48,
            }
        );
        let [Some(newest), Some(older)] = frames else {
            panic!("{frames:?}");
        };
        let events: Vec<_> = newest.events().collect();
        assert!(
            matches!(events[0], (0, Event::Midi(Message::NoteOn { key: 64, velocity: 90, channel })) if channel.number() == 2)
        );
        assert_eq!(events[1], (10, Event::Midi(Message::TimingClock)));
        assert!(matches!(
            events[2],
            (47, Event::Expression(Expression { id: 6, channel: Some(c), key: Some(64), value })) if c.wire() == 1 && value == 0.25
        ));
        assert_eq!(older.seq, 76);
        assert!(matches!(
            older.events().collect::<Vec<_>>()[..],
            [(
                1,
                Event::SysexPiece(SysexPiece {
                    first: true,
                    last: true,
                    index: 0,
                    data: [0xF0, 0x43, 0xF7]
                })
            )]
        ));
        // Written again from what was read, the bytes are JTunnel's.
        let mut ex = [2, 6, 1, 64, 0, 0, 0, 0, 0, 0, 0, 0];
        ex[4..].copy_from_slice(&0.25_f64.to_le_bytes());
        let a: [(u16, &[u8]); 3] = [(0, &[0x91, 64, 90]), (10, &[0xF8]), (47, &ex)];
        let b: [(u16, &[u8]); 1] = [(1, &[1, 3, 0, 3, 0xF0, 0x43, 0xF7])];
        let mut buf = [0; MAX_PACKET_BYTES];
        let n = write_audio(
            &mut buf,
            &header,
            &[
                FrameOut {
                    events: &a,
                    audio: &[],
                },
                FrameOut {
                    events: &b,
                    audio: &[],
                },
            ],
        )
        .unwrap_or(0);
        assert_eq!(&buf[..n], &audio[..]);
        let subscribe = unhex("4a54554e0202084269747769672032");
        assert_eq!(
            parse(&subscribe),
            Ok(Packet::Subscribe { name: b"Bitwig 2" })
        );
        let n = write_subscribe(&mut buf, b"Bitwig 2").unwrap_or(0);
        assert_eq!(&buf[..n], &subscribe[..]);
    }
}
