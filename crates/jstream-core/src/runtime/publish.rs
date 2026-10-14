//! Where an output's FLV tags go. Tests record them in memory. A host that is
//! actually live uses [`connect_rtmp`].

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_mux::flv::{FlvTag, TagType};
use jstream_outputs::rtmp::{RtmpError, RtmpPublisher, RtmpUrl, StreamKey, Transport, connect};

use crate::lock;

/// A failure while opening or writing a live publish session.
#[derive(Debug)]
pub struct PublishError {
    reason: String,
}

impl PublishError {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The error text. Stream keys are not included by this type; RTMP rejections
    /// redact them before they become this string (R-SEC-001).
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for PublishError {}

fn rtmp_err(err: RtmpError) -> PublishError {
    PublishError::new(err.to_string())
}

/// Accepts the FLV tags of one live output. Called on the output thread only.
pub trait LivePublish: Send {
    /// Sends one tag.
    ///
    /// # Errors
    /// When the destination refuses or drops the tag.
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError>;

    /// Reads anything the destination has sent since the last tag. Recording
    /// destinations do nothing.
    ///
    /// # Errors
    /// When the destination reports a failure.
    fn poll(&mut self) -> Result<(), PublishError>;

    /// Ends the session.
    ///
    /// # Errors
    /// When the destination does not close cleanly.
    fn finish(&mut self) -> Result<(), PublishError>;
}

/// Keeps every tag in memory, in send order. Clones share the buffer, so a test
/// can read tags after the publish target has moved onto the output thread.
#[derive(Clone, Default)]
pub struct RecordingPublish {
    tags: Arc<Mutex<Vec<FlvTag>>>,
}

impl RecordingPublish {
    /// An empty recording.
    pub fn new() -> Self {
        Self::default()
    }

    /// The tags sent so far, cloned out of the buffer.
    pub fn tags(&self) -> Vec<FlvTag> {
        lock(&self.tags).clone()
    }
}

impl LivePublish for RecordingPublish {
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError> {
        lock(&self.tags).push(tag.clone());
        Ok(())
    }

    fn poll(&mut self) -> Result<(), PublishError> {
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PublishError> {
        Ok(())
    }
}

/// An RTMP or RTMPS publish session. `send_metadata` on the publisher adds
/// `@setDataFrame` itself, so script tags are the file-style `onMetaData` body.
pub struct RtmpPublish {
    inner: Option<RtmpPublisher<Transport>>,
}

impl RtmpPublish {
    fn publisher(&mut self) -> Result<&mut RtmpPublisher<Transport>, PublishError> {
        self.inner
            .as_mut()
            .ok_or_else(|| PublishError::new("the RTMP session has finished"))
    }
}

impl LivePublish for RtmpPublish {
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError> {
        let publisher = self.publisher()?;
        let result = match tag.tag_type {
            TagType::Script => publisher.send_metadata(&tag.body),
            TagType::Video => publisher.send_video(tag.timestamp, &tag.body),
            TagType::Audio => publisher.send_audio(tag.timestamp, &tag.body),
        };
        result.map_err(rtmp_err)
    }

    fn poll(&mut self) -> Result<(), PublishError> {
        self.publisher()?.poll_incoming().map_err(rtmp_err)
    }

    fn finish(&mut self) -> Result<(), PublishError> {
        match self.inner.take() {
            Some(publisher) => publisher.finish().map(|_| ()).map_err(rtmp_err),
            None => Ok(()),
        }
    }
}

/// How long the handshake and the publish sequence may wait on one read or write,
/// and the bound on one write once publishing (a stalled peer, see the README).
const RTMP_IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The read timeout once publishing: short, so the output thread's `poll` returns at
/// once when the server has sent nothing.
const RTMP_POLL_TIMEOUT: Duration = Duration::from_millis(1);

/// Connects to `url` (`rtmp://` or `rtmps://`) and publishes `key`.
///
/// The handshake and the `connect` … `publish` exchange read with a 10 s timeout:
/// the publisher retries only an interrupted read, so a shorter one would fail on
/// any server slower than that to answer. Once publishing has started, the read
/// timeout drops to 1 ms so the output thread can poll server messages without
/// blocking the pipeline. Tests reach it only through a fake server on loopback.
///
/// # Errors
/// When the URL or key is rejected, or the server does not accept the publish.
pub fn connect_rtmp(url: &str, key: &str) -> Result<RtmpPublish, PublishError> {
    let url = RtmpUrl::parse(url).map_err(rtmp_err)?;
    let key = StreamKey::new(key).map_err(rtmp_err)?;
    let transport = connect(&url, Some(RTMP_IO_TIMEOUT)).map_err(rtmp_err)?;
    transport
        .set_read_timeout(Some(RTMP_IO_TIMEOUT))
        .map_err(|err| PublishError::new(format!("setting the RTMP read timeout: {err}")))?;
    transport
        .set_write_timeout(Some(RTMP_IO_TIMEOUT))
        .map_err(|err| PublishError::new(format!("setting the RTMP write timeout: {err}")))?;
    let publisher = RtmpPublisher::publish(transport, &url, &key).map_err(rtmp_err)?;
    publisher
        .get_ref()
        .set_read_timeout(Some(RTMP_POLL_TIMEOUT))
        .map_err(|err| PublishError::new(format!("setting the RTMP poll timeout: {err}")))?;
    Ok(RtmpPublish {
        inner: Some(publisher),
    })
}

#[cfg(test)]
mod tests {
    use super::{LivePublish, connect_rtmp};
    use jstream_mux::amf0::Amf0Value;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    const HANDSHAKE: usize = 1536;
    /// Longer than the 1 ms poll timeout the publisher used to have during the handshake.
    const DELAY: Duration = Duration::from_millis(50);

    fn s(v: &str) -> Amf0Value {
        Amf0Value::String(v.into())
    }

    fn status(code: &str) -> Amf0Value {
        Amf0Value::Object(vec![
            ("level".into(), s("status")),
            ("code".into(), s(code)),
            ("description".into(), s("test")),
        ])
    }

    /// One AMF0 command message on chunk stream 3, at the default 128-byte chunk size.
    fn command(stream_id: u32, values: &[Amf0Value], out: &mut Vec<u8>) {
        let body = Amf0Value::encode_all(values).expect("amf0");
        let len = u32::try_from(body.len()).expect("small");
        out.push(0x03); // fmt 0, chunk stream 3
        out.extend_from_slice(&[0, 0, 0]); // timestamp
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.push(0x14); // AMF0 command
        out.extend_from_slice(&stream_id.to_le_bytes());
        for (i, chunk) in body.chunks(128).enumerate() {
            if i > 0 {
                out.push(0xC3); // fmt 3 continuation on chunk stream 3
            }
            out.extend_from_slice(chunk);
        }
    }

    /// A server that answers the handshake and then `connect` only after [`DELAY`],
    /// then accepts the stream. It drains what the client sends until it hangs up.
    fn slow_server(mut io: TcpStream) {
        let mut c0c1 = vec![0; 1 + HANDSHAKE];
        io.read_exact(&mut c0c1).expect("C0C1");
        thread::sleep(DELAY);
        let mut reply = vec![3];
        reply.extend((0..HANDSHAKE).map(|i| (i % 251) as u8));
        reply.extend_from_slice(&c0c1[1..]);
        io.write_all(&reply).expect("S0S1S2");
        let mut c2 = vec![0; HANDSHAKE];
        io.read_exact(&mut c2).expect("C2");

        let drain = {
            let mut io = io.try_clone().expect("clone");
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while matches!(io.read(&mut buf), Ok(n) if n > 0) {}
            })
        };
        // The client has sent Set Chunk Size and `connect` by now; answer it late.
        thread::sleep(DELAY);
        let mut out = Vec::new();
        command(
            0,
            &[
                s("_result"),
                Amf0Value::Number(1.0),
                Amf0Value::Object(vec![("fmsVer".into(), s("FMS/3,0,1,123"))]),
                status("NetConnection.Connect.Success"),
            ],
            &mut out,
        );
        io.write_all(&out).expect("connect reply");
        thread::sleep(DELAY);
        out.clear();
        command(
            0,
            &[
                s("_result"),
                Amf0Value::Number(4.0),
                Amf0Value::Null,
                Amf0Value::Number(1.0),
            ],
            &mut out,
        );
        command(
            1,
            &[
                s("onStatus"),
                Amf0Value::Number(0.0),
                Amf0Value::Null,
                status("NetStream.Publish.Start"),
            ],
            &mut out,
        );
        io.write_all(&out).expect("publish replies");
        let _ = drain.join();
    }

    /// A server that takes 50 ms to answer the handshake and `connect` is still
    /// published to, and once publishing a poll returns at once.
    #[test]
    fn connect_rtmp_waits_for_a_slow_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let (io, _) = listener.accept().expect("accept");
            slow_server(io);
        });

        let mut publish = connect_rtmp(&format!("rtmp://127.0.0.1:{port}/live"), "test-key")
            .expect("publishing to a slow server");
        let started = Instant::now();
        publish.poll().expect("poll");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "poll waited {:?} for a quiet server",
            started.elapsed()
        );
        publish.finish().expect("finish");
        drop(publish);
        server.join().expect("server");
    }
}
