//! The publish session: connect, create a stream, publish, then send media.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jstream_media::{MediaTime, Rounding, Timebase};

use super::amf0::{self, Amf0Value};
use super::chunk::{ChunkDecoder, ChunkEncoder, Message, msg_type};
use super::{RtmpError, RtmpUrl, Scheme, StreamKey, handshake};

/// Chunk stream for protocol control messages (fixed by the RTMP specification).
const CSID_CONTROL: u32 = 2;
/// Chunk stream for NetConnection commands.
const CSID_COMMAND: u32 = 3;
/// Chunk stream for audio messages.
const CSID_AUDIO: u32 = 4;
/// Chunk stream for NetStream commands and data messages.
const CSID_STREAM: u32 = 5;
/// Chunk stream for video messages.
const CSID_VIDEO: u32 = 6;

/// User Control event types.
const UC_PING_REQUEST: u16 = 6;
const UC_PING_RESPONSE: u16 = 7;

/// Transaction IDs of the commands the client sends, in order.
const TXN_CONNECT: f64 = 1.0;
const TXN_RELEASE_STREAM: f64 = 2.0;
const TXN_FC_PUBLISH: f64 = 3.0;
const TXN_CREATE_STREAM: f64 = 4.0;
const TXN_PUBLISH: f64 = 5.0;
const TXN_FC_UNPUBLISH: f64 = 6.0;
const TXN_DELETE_STREAM: f64 = 7.0;

/// Settings for [`RtmpPublisher::publish_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOptions {
    /// The chunk size announced with Set Chunk Size before `connect` and used for every
    /// message after it. Larger chunks mean less header overhead for video.
    pub chunk_size: u32,
    /// The `flashVer` sent in `connect`. Some ingest servers expect an encoder-style
    /// value (`FMLE/3.0 (compatible; …)`).
    pub flash_ver: String,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4096,
            flash_ver: "FMLE/3.0 (compatible; JStream)".to_owned(),
        }
    }
}

/// Opens a TCP connection to `url`'s host and port, with Nagle's algorithm off.
///
/// `timeout` bounds each connection attempt (one per resolved address). Set read and
/// write timeouts on the returned stream before handing it to [`RtmpPublisher`] if the
/// session must not block indefinitely on a stalled server.
///
/// # Errors
/// [`RtmpError::TlsUnsupported`] for `rtmps://` URLs: TLS is not implemented yet (see
/// crates/jstream-outputs/README.md#purpose). [`RtmpError::Io`] when name resolution
/// or every connection attempt fails.
pub fn connect_tcp(url: &RtmpUrl, timeout: Option<Duration>) -> Result<TcpStream, RtmpError> {
    // TODO(TLS): rtmps:// needs a TLS stream here. Every Rust TLS option compiles C or
    // assembly, so the choice (rustls + ring, rustls + aws-lc-rs, or SChannel through the
    // `windows` crate) needs a native-code allowlist entry and the maintainer's decision.
    if url.scheme() == Scheme::Rtmps {
        return Err(RtmpError::TlsUnsupported);
    }
    let io = |e| RtmpError::io("connecting", e);
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host resolved to no address");
    for addr in (url.host(), url.port()).to_socket_addrs().map_err(io)? {
        let attempt = match timeout {
            Some(t) => TcpStream::connect_timeout(&addr, t),
            None => TcpStream::connect(addr),
        };
        match attempt {
            Ok(stream) => {
                stream.set_nodelay(true).map_err(io)?;
                return Ok(stream);
            }
            Err(e) => last = e,
        }
    }
    Err(io(last))
}

/// Converts a media timestamp to RTMP milliseconds (rounded to the nearest), wrapping at
/// 2^32 as RTMP timestamps do.
///
/// # Errors
/// [`RtmpError::NegativeTimestamp`] for times before zero, and [`RtmpError::Timestamp`]
/// when rescaling overflows.
pub fn timestamp_ms(t: MediaTime) -> Result<u32, RtmpError> {
    let ms = t.rescale(Timebase::MILLIS, Rounding::Nearest)?.ticks();
    let ms = u64::try_from(ms).map_err(|_| RtmpError::NegativeTimestamp { ms })?;
    Ok((ms & u64::from(u32::MAX)) as u32)
}

/// The transport plus both chunk-stream directions and acknowledgement bookkeeping.
struct Connection<S> {
    stream: S,
    encoder: ChunkEncoder,
    decoder: ChunkDecoder,
    out: Vec<u8>,
    read_buf: Vec<u8>,
    bytes_in: u64,
    bytes_out: u64,
    last_ack: u64,
    /// The peer's Window Acknowledgement Size: acknowledge after this many bytes.
    peer_window: Option<u32>,
    /// The window we last announced in answer to Set Peer Bandwidth.
    our_window: Option<u32>,
}

impl<S: Read + Write> Connection<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            encoder: ChunkEncoder::new(),
            decoder: ChunkDecoder::new(),
            out: Vec::new(),
            read_buf: vec![0; 16 * 1024],
            bytes_in: 0,
            bytes_out: 0,
            last_ack: 0,
            peer_window: None,
            our_window: None,
        }
    }

    /// Chunks and writes one message, then flushes.
    fn send(
        &mut self,
        csid: u32,
        timestamp: u32,
        type_id: u8,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<(), RtmpError> {
        self.out.clear();
        self.encoder
            .encode(csid, timestamp, type_id, stream_id, payload, &mut self.out)?;
        let io = |e| RtmpError::io("sending", e);
        self.stream.write_all(&self.out).map_err(io)?;
        self.stream.flush().map_err(io)?;
        self.bytes_out += self.out.len() as u64;
        Ok(())
    }

    fn send_control(&mut self, type_id: u8, payload: &[u8]) -> Result<(), RtmpError> {
        self.send(CSID_CONTROL, 0, type_id, 0, payload)
    }

    /// Sends an AMF0 command built from `values`.
    fn send_command(
        &mut self,
        csid: u32,
        stream_id: u32,
        values: &[Amf0Value],
    ) -> Result<(), RtmpError> {
        let mut body = Vec::new();
        for v in values {
            amf0::encode(v, &mut body)?;
        }
        self.send(csid, 0, msg_type::COMMAND_AMF0, stream_id, &body)
    }

    /// The next message that isn't protocol control, reading (and blocking) as needed.
    fn recv(&mut self, during: &'static str) -> Result<Message, RtmpError> {
        loop {
            if let Some(m) = self.next_buffered()? {
                return Ok(m);
            }
            match self.read_once() {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(RtmpError::io(during, e)),
            }
        }
    }

    /// The next already-received message that isn't protocol control, handling control
    /// messages on the way.
    fn next_buffered(&mut self) -> Result<Option<Message>, RtmpError> {
        self.maybe_ack()?;
        while let Some(m) = self.decoder.next_message()? {
            if let Some(m) = self.handle_control(m)? {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }

    /// One `read` from the transport into the decoder, acknowledging as the peer's
    /// window requires. End of stream is reported as `UnexpectedEof`.
    fn read_once(&mut self) -> io::Result<()> {
        let n = self.stream.read(&mut self.read_buf)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.decoder.push(&self.read_buf[..n]);
        self.bytes_in += n as u64;
        Ok(())
    }

    /// Sends an Acknowledgement once a full window has arrived since the last one.
    fn maybe_ack(&mut self) -> Result<(), RtmpError> {
        if let Some(window) = self.peer_window.filter(|&w| w > 0)
            && self.bytes_in - self.last_ack >= u64::from(window)
        {
            self.last_ack = self.bytes_in;
            // The sequence number is the byte count so far, wrapping at 2^32.
            let seq = (self.bytes_in & u64::from(u32::MAX)) as u32;
            self.send_control(msg_type::ACK, &seq.to_be_bytes())?;
        }
        Ok(())
    }

    /// Applies a protocol control or user control message, answering it if needed.
    /// Returns any other message unchanged.
    fn handle_control(&mut self, m: Message) -> Result<Option<Message>, RtmpError> {
        self.maybe_ack()?;
        let short = |what: &str| RtmpError::protocol(format!("{what} message too short"));
        let be32 = |p: &[u8]| {
            p.get(..4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        match m.type_id {
            msg_type::SET_CHUNK_SIZE => {
                let size = be32(&m.payload).ok_or_else(|| short("Set Chunk Size"))?;
                // The top bit is reserved and must be zero; ignore it if a peer sets it.
                self.decoder.set_chunk_size(size & 0x7FFF_FFFF)?;
            }
            msg_type::ABORT => {
                let csid = be32(&m.payload).ok_or_else(|| short("Abort"))?;
                self.decoder.abort(csid);
            }
            msg_type::ACK => {}
            msg_type::WINDOW_ACK_SIZE => {
                let size = be32(&m.payload).ok_or_else(|| short("Window Acknowledgement Size"))?;
                self.peer_window = Some(size);
            }
            msg_type::SET_PEER_BANDWIDTH => {
                let size = be32(&m.payload).ok_or_else(|| short("Set Peer Bandwidth"))?;
                if self.our_window != Some(size) {
                    self.our_window = Some(size);
                    self.send_control(msg_type::WINDOW_ACK_SIZE, &size.to_be_bytes())?;
                }
            }
            msg_type::USER_CONTROL => {
                let event = m
                    .payload
                    .get(..2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .ok_or_else(|| short("User Control"))?;
                if event == UC_PING_REQUEST {
                    let mut pong = UC_PING_RESPONSE.to_be_bytes().to_vec();
                    pong.extend_from_slice(m.payload.get(2..6).ok_or_else(|| short("Ping"))?);
                    self.send_control(msg_type::USER_CONTROL, &pong)?;
                }
                // Stream Begin/EOF/Dry and buffer events need no answer from a publisher.
            }
            _ => return Ok(Some(m)),
        }
        Ok(None)
    }
}

/// The AMF0 values of a command message, or `None` for any other message.
fn command_values(m: &Message) -> Result<Option<Vec<Amf0Value>>, RtmpError> {
    let body = match m.type_id {
        msg_type::COMMAND_AMF0 => &m.payload[..],
        // An AMF3 command whose first byte is 0 switches straight back to AMF0.
        msg_type::COMMAND_AMF3 => match m.payload.split_first() {
            Some((0, rest)) => rest,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    amf0::decode_all(body).map(Some)
}

/// The name and transaction ID of a decoded command.
fn name_and_txn(values: &[Amf0Value]) -> (Option<&str>, Option<f64>) {
    (
        values.first().and_then(Amf0Value::as_str),
        values.get(1).and_then(Amf0Value::as_number),
    )
}

/// `level`, `code` and `description` of a status info object (argument 3).
fn status_info(values: &[Amf0Value]) -> (&str, &str, &str) {
    let field = |k| {
        values
            .get(3)
            .and_then(|info| info.get(k))
            .and_then(Amf0Value::as_str)
            .unwrap_or("")
    };
    (field("level"), field("code"), field("description"))
}

fn rejected(command: &'static str, values: &[Amf0Value]) -> RtmpError {
    let (_, code, description) = status_info(values);
    RtmpError::Rejected {
        command,
        code: code.to_owned(),
        description: description.to_owned(),
    }
}

/// A live RTMP publish session over the byte stream `S`.
///
/// [`publish`](Self::publish) runs the handshake and the
/// `connect` → `releaseStream` → `FCPublish` → `createStream` → `publish` sequence and
/// returns once the server reports `NetStream.Publish.Start`. Then send
/// [`metadata`](Self::send_metadata) and [`video`](Self::send_video) /
/// [`audio`](Self::send_audio) messages, and end with [`finish`](Self::finish).
///
/// I/O is blocking. Use the publisher from one dedicated thread (see
/// crates/jstream-outputs/README.md#purpose), and set read and write timeouts on the
/// transport so a stalled server surfaces as an error.
///
/// The server sends small messages during the session (acknowledgements, pings, status
/// changes). Call [`poll_incoming`](Self::poll_incoming) regularly so they are read
/// and answered; it needs a transport whose reads time out or don't block.
pub struct RtmpPublisher<S> {
    conn: Connection<S>,
    stream_id: u32,
    key: StreamKey,
}

impl<S: Read + Write> RtmpPublisher<S> {
    /// Publishes `key` on `url`'s application over `stream` with default options.
    ///
    /// # Errors
    /// See [`publish_with`](Self::publish_with).
    pub fn publish(stream: S, url: &RtmpUrl, key: &StreamKey) -> Result<Self, RtmpError> {
        Self::publish_with(stream, url, key, &PublishOptions::default())
    }

    /// Publishes `key` on `url`'s application over `stream`, an already connected
    /// transport (TCP for `rtmp://`, TLS for `rtmps://`).
    ///
    /// # Errors
    /// [`RtmpError::InvalidChunkSize`] for a bad `options.chunk_size`;
    /// [`RtmpError::HandshakeVersion`] when the server is not RTMP 3;
    /// [`RtmpError::Rejected`] when the server answers `connect`, `createStream` or
    /// `publish` with an error (for example `NetStream.Publish.BadName` for a wrong
    /// key); [`RtmpError::UnexpectedReply`] for a malformed answer; and transport,
    /// chunk and AMF0 errors.
    pub fn publish_with(
        stream: S,
        url: &RtmpUrl,
        key: &StreamKey,
        options: &PublishOptions,
    ) -> Result<Self, RtmpError> {
        super::chunk::check_chunk_size(options.chunk_size)?;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self::publish_seeded(stream, url, key, options, seed)
    }

    fn publish_seeded(
        mut stream: S,
        url: &RtmpUrl,
        key: &StreamKey,
        options: &PublishOptions,
        seed: u64,
    ) -> Result<Self, RtmpError> {
        handshake::client(&mut stream, seed)?;
        let mut conn = Connection::new(stream);

        conn.send_control(msg_type::SET_CHUNK_SIZE, &options.chunk_size.to_be_bytes())?;
        conn.encoder.set_chunk_size(options.chunk_size)?;

        let s = Amf0Value::str;
        let connect_obj = Amf0Value::Object(vec![
            ("app".into(), s(url.app())),
            ("type".into(), s("nonprivate")),
            ("flashVer".into(), s(&options.flash_ver)),
            ("tcUrl".into(), s(url.tc_url())),
        ]);
        conn.send_command(
            CSID_COMMAND,
            0,
            &[s("connect"), Amf0Value::Number(TXN_CONNECT), connect_obj],
        )?;
        wait_result(&mut conn, TXN_CONNECT, "connect")?;

        let key_v = s(key.expose());
        for (name, txn) in [
            ("releaseStream", TXN_RELEASE_STREAM),
            ("FCPublish", TXN_FC_PUBLISH),
        ] {
            conn.send_command(
                CSID_COMMAND,
                0,
                &[
                    s(name),
                    Amf0Value::Number(txn),
                    Amf0Value::Null,
                    key_v.clone(),
                ],
            )?;
        }
        conn.send_command(
            CSID_COMMAND,
            0,
            &[
                s("createStream"),
                Amf0Value::Number(TXN_CREATE_STREAM),
                Amf0Value::Null,
            ],
        )?;
        let reply = wait_result(&mut conn, TXN_CREATE_STREAM, "createStream")?;
        let stream_id = reply
            .get(3)
            .and_then(Amf0Value::as_number)
            .filter(|n| n.fract() == 0.0 && *n >= 1.0 && *n <= f64::from(u32::MAX))
            .ok_or(RtmpError::UnexpectedReply {
                command: "createStream",
                reason: "no positive integer stream ID",
            })? as u32;

        conn.send_command(
            CSID_STREAM,
            stream_id,
            &[
                s("publish"),
                Amf0Value::Number(TXN_PUBLISH),
                Amf0Value::Null,
                key_v,
                s("live"),
            ],
        )?;
        loop {
            let m = conn.recv("waiting for publish to start")?;
            let Some(values) = command_values(&m)? else {
                continue;
            };
            match name_and_txn(&values) {
                (Some("onStatus"), _) => {
                    let (level, code, _) = status_info(&values);
                    if code == "NetStream.Publish.Start" {
                        break;
                    }
                    if level == "error" {
                        return Err(rejected("publish", &values));
                    }
                }
                (Some("_error"), Some(t)) if t == TXN_PUBLISH => {
                    return Err(rejected("publish", &values));
                }
                _ => {}
            }
        }
        Ok(Self {
            conn,
            stream_id,
            key: key.clone(),
        })
    }

    /// The message stream ID the server assigned in `createStream`.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Total bytes written to the transport after the handshake.
    pub fn bytes_sent(&self) -> u64 {
        self.conn.bytes_out
    }

    /// Total bytes read from the transport after the handshake.
    pub fn bytes_received(&self) -> u64 {
        self.conn.bytes_in
    }

    /// The transport, for example to change its timeouts.
    pub fn get_ref(&self) -> &S {
        &self.conn.stream
    }

    /// The transport, mutably. Reading or writing through it corrupts the session.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.conn.stream
    }

    /// Sends stream metadata as `@setDataFrame`.
    ///
    /// `on_metadata` is the body of an FLV `onMetaData` script tag: the AMF0 string
    /// `onMetaData` followed by an ECMA array of properties (width, height, framerate,
    /// codec IDs, …). This method prefixes the AMF0 string `@setDataFrame`, which tells
    /// the server to store the metadata and replay it to viewers.
    ///
    /// # Errors
    /// [`RtmpError::PayloadSize`] for an empty or oversized body; transport errors.
    pub fn send_metadata(&mut self, on_metadata: &[u8]) -> Result<(), RtmpError> {
        if on_metadata.is_empty() {
            return Err(RtmpError::PayloadSize { len: 0 });
        }
        let mut body = Vec::with_capacity(16 + on_metadata.len());
        amf0::encode(&Amf0Value::str("@setDataFrame"), &mut body)?;
        body.extend_from_slice(on_metadata);
        self.conn
            .send(CSID_STREAM, 0, msg_type::DATA_AMF0, self.stream_id, &body)
    }

    /// Sends a video message: `tag_body` is an FLV VIDEODATA tag body (for H.264, the
    /// frame-type/codec byte, AVC packet type, composition time and AVCC data) and
    /// `timestamp_ms` its decode timestamp in milliseconds.
    ///
    /// # Errors
    /// [`RtmpError::PayloadSize`] for an empty or oversized body; transport errors.
    pub fn send_video(&mut self, timestamp_ms: u32, tag_body: &[u8]) -> Result<(), RtmpError> {
        self.send_media(CSID_VIDEO, msg_type::VIDEO, timestamp_ms, tag_body)
    }

    /// Sends an audio message: `tag_body` is an FLV AUDIODATA tag body (for AAC, the
    /// format byte, AAC packet type and raw AAC data) and `timestamp_ms` its timestamp
    /// in milliseconds.
    ///
    /// # Errors
    /// [`RtmpError::PayloadSize`] for an empty or oversized body; transport errors.
    pub fn send_audio(&mut self, timestamp_ms: u32, tag_body: &[u8]) -> Result<(), RtmpError> {
        self.send_media(CSID_AUDIO, msg_type::AUDIO, timestamp_ms, tag_body)
    }

    fn send_media(
        &mut self,
        csid: u32,
        type_id: u8,
        timestamp_ms: u32,
        body: &[u8],
    ) -> Result<(), RtmpError> {
        if body.is_empty() {
            return Err(RtmpError::PayloadSize { len: 0 });
        }
        self.conn
            .send(csid, timestamp_ms, type_id, self.stream_id, body)
    }

    /// Reads whatever the server has sent, without waiting for more: answers pings,
    /// sends acknowledgements and applies control messages.
    ///
    /// It performs at most one `read`. A read that fails with `WouldBlock` or
    /// `TimedOut` means nothing is pending and is not an error, so the transport should
    /// be non-blocking or have a short read timeout; on a plain blocking transport this
    /// waits until the server sends something.
    ///
    /// # Errors
    /// [`RtmpError::Rejected`] when the server reports an error status for the stream;
    /// [`RtmpError::ConnectionClosed`] when it closed the connection; transport, chunk
    /// and AMF0 errors.
    pub fn poll_incoming(&mut self) -> Result<(), RtmpError> {
        self.drain_buffered()?;
        match self.conn.read_once() {
            Ok(()) => self.drain_buffered(),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(RtmpError::io("polling", e)),
        }
    }

    fn drain_buffered(&mut self) -> Result<(), RtmpError> {
        while let Some(m) = self.conn.next_buffered()? {
            if let Some(values) = command_values(&m)?
                && name_and_txn(&values).0 == Some("onStatus")
                && status_info(&values).0 == "error"
            {
                return Err(rejected("publish", &values));
            }
        }
        Ok(())
    }

    /// Ends the session: sends `FCUnpublish` and `deleteStream`, flushes, and returns
    /// the transport (which the caller then closes). It doesn't wait for replies.
    ///
    /// # Errors
    /// Transport errors while sending.
    pub fn finish(mut self) -> Result<S, RtmpError> {
        let s = Amf0Value::str;
        self.conn.send_command(
            CSID_COMMAND,
            0,
            &[
                s("FCUnpublish"),
                Amf0Value::Number(TXN_FC_UNPUBLISH),
                Amf0Value::Null,
                s(self.key.expose()),
            ],
        )?;
        self.conn.send_command(
            CSID_COMMAND,
            0,
            &[
                s("deleteStream"),
                Amf0Value::Number(TXN_DELETE_STREAM),
                Amf0Value::Null,
                Amf0Value::Number(f64::from(self.stream_id)),
            ],
        )?;
        Ok(self.conn.stream)
    }
}

/// Waits for `_result` to transaction `txn`, ignoring other commands (`onBWDone`,
/// replies to other transactions). `_error` to `txn` is a rejection.
fn wait_result<S: Read + Write>(
    conn: &mut Connection<S>,
    txn: f64,
    command: &'static str,
) -> Result<Vec<Amf0Value>, RtmpError> {
    loop {
        let m = conn.recv(command)?;
        let Some(values) = command_values(&m)? else {
            continue;
        };
        match name_and_txn(&values) {
            (Some("_result"), Some(t)) if t == txn => return Ok(values),
            (Some("_error"), Some(t)) if t == txn => return Err(rejected(command, &values)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
