//! Where an output's FLV tags go. Tests record them in memory. A host that is
//! actually live uses [`connect_rtmp`], which reconnects when the connection drops.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jstream_mux::flv::{FlvTag, TagType};
pub use jstream_outputs::rtmp::LinkState;
use jstream_outputs::rtmp::{
    Clock, ReconnectPolicy, ReconnectingPublisher, RtmpError, RtmpUrl, SessionTimeouts, StreamKey,
    Transport, connect_publisher, rtmp_connector,
};
use jstream_protocol::OutputId;

use crate::lock;

/// A failure while opening or writing a live publish session.
#[derive(Debug)]
pub struct PublishError {
    reason: String,
}

impl PublishError {
    /// An error with `reason` as its text. A host's [`OpenPublish`] uses it to fail a
    /// start for its own reasons, such as an output with no stream key. `reason` must
    /// not contain a stream key (R-SEC-001).
    pub fn new(reason: impl Into<String>) -> Self {
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
    /// Sends one tag. While the link is [`Reconnecting`](LinkState::Reconnecting) a
    /// destination drops the tag and counts it in [`dropped`](Self::dropped) instead
    /// of waiting.
    ///
    /// # Errors
    /// When the destination refuses or drops the tag for good (for RTMP: it gave up
    /// reconnecting).
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError>;

    /// Reads anything the destination has sent since the last tag, and moves a
    /// reconnect along. Recording destinations do nothing.
    ///
    /// # Errors
    /// When the destination reports a failure.
    fn poll(&mut self) -> Result<(), PublishError>;

    /// Ends the session.
    ///
    /// # Errors
    /// When the destination does not close cleanly.
    fn finish(&mut self) -> Result<(), PublishError>;

    /// The output is stopping: every later call should return by `deadline`. A send
    /// that cannot finish by then fails instead, and `finish` past it closes without
    /// waiting on the peer. Called once, on the output thread, when the stop's drain
    /// starts. The default does nothing, for a destination that never blocks.
    fn stop_by(&mut self, deadline: Instant) {
        let _ = deadline;
    }

    /// Whether the link is up or being re-established, after the last call. The
    /// output reports `reconnecting` while it is not [`Active`](LinkState::Active).
    /// A destination that cannot lose its link keeps the default, `Active`.
    fn link_state(&self) -> LinkState {
        LinkState::Active
    }

    /// Media tags dropped because the link was down, as a running total. They count
    /// in `stats.tick`'s `output_packets`. Default 0.
    fn dropped(&self) -> u64 {
        0
    }
}

/// Opens the publish target of one `output.start`, for the output it names. The
/// engine calls it on the thread that runs the start, after `starting` and outside
/// the engine's locks, so it may block on the network (as [`connect_rtmp`] does). An
/// error fails the start; its text becomes the `action_failed` reason and the
/// `failed` log line, so it must not contain a stream key (R-SEC-001).
pub type OpenPublish =
    Arc<dyn Fn(&OutputId) -> Result<Box<dyn LivePublish>, PublishError> + Send + Sync>;

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

/// An RTMP or RTMPS publish session that reconnects with backoff when its
/// connection drops ([`ReconnectingPublisher`], with the default
/// [`ReconnectPolicy`]). `send_metadata` on the publisher adds `@setDataFrame`
/// itself, so script tags are the file-style `onMetaData` body.
///
/// Once the output is stopping ([`LivePublish::stop_by`]), each write's timeout is
/// cut to the time left before the deadline, a lost link is not reconnected, and past
/// the deadline sends fail and `finish` drops the connection without the
/// `FCUnpublish` … `deleteStream` goodbye, so a peer that has stopped reading cannot
/// hold the stop.
pub struct RtmpPublish {
    inner: Option<ReconnectingPublisher<Transport>>,
    /// Set by [`LivePublish::stop_by`].
    deadline: Option<Instant>,
}

impl RtmpPublish {
    fn publisher(&mut self) -> Result<&mut ReconnectingPublisher<Transport>, PublishError> {
        self.inner
            .as_mut()
            .ok_or_else(|| PublishError::new("the RTMP session has finished"))
    }

    /// While stopping: bounds the next write by the time left before the deadline.
    /// `Ok(false)` when the link is down, so the caller does not wait on a
    /// reconnect. Fails once the deadline has passed. Not stopping: `Ok(true)`.
    fn before_io(&mut self) -> Result<bool, PublishError> {
        let Some(deadline) = self.deadline else {
            return Ok(true);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(PublishError::new("the stop deadline passed"));
        }
        let Some(session) = self.publisher()?.publisher() else {
            return Ok(false);
        };
        // A zero timeout means "none" to the OS, so never go below 1 ms.
        session
            .get_ref()
            .set_write_timeout(Some(left.max(Duration::from_millis(1))))
            .map_err(|err| PublishError::new(format!("bounding the write timeout: {err}")))?;
        Ok(true)
    }
}

impl LivePublish for RtmpPublish {
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError> {
        if !self.before_io()? {
            return Err(PublishError::new("the RTMP link is down while stopping"));
        }
        let publisher = self.publisher()?;
        let result = match tag.tag_type {
            TagType::Script => publisher.send_metadata(&tag.body),
            TagType::Video => publisher.send_video(tag.timestamp, &tag.body),
            TagType::Audio => publisher.send_audio(tag.timestamp, &tag.body),
        };
        result.map_err(rtmp_err)
    }

    fn poll(&mut self) -> Result<(), PublishError> {
        if !self.before_io()? {
            // Stopping with the link down: no reconnect to move along.
            return Ok(());
        }
        self.publisher()?.poll().map_err(rtmp_err)
    }

    fn finish(&mut self) -> Result<(), PublishError> {
        let in_time = matches!(self.before_io(), Ok(true));
        match self.inner.take() {
            Some(publisher) if in_time => publisher.finish().map(|_| ()).map_err(rtmp_err),
            // Past the stop deadline, or the link is down: dropping the publisher
            // closes the connection without waiting on the peer.
            Some(_) | None => Ok(()),
        }
    }

    fn stop_by(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }

    fn link_state(&self) -> LinkState {
        self.inner
            .as_ref()
            .map_or(LinkState::Active, ReconnectingPublisher::state)
    }

    fn dropped(&self) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |publisher| publisher.stats().dropped())
    }
}

/// How long the handshake and the publish sequence may wait on one read or write,
/// and the bound on one write once publishing (a stalled peer, see the README).
const RTMP_IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The read timeout once publishing: short, so the output thread's `poll` returns at
/// once when the server has sent nothing.
const RTMP_POLL_TIMEOUT: Duration = Duration::from_millis(1);

/// The timeouts of the first session and of every reconnect attempt.
const RTMP_TIMEOUTS: SessionTimeouts = SessionTimeouts {
    connect: RTMP_IO_TIMEOUT,
    write: RTMP_IO_TIMEOUT,
    poll: RTMP_POLL_TIMEOUT,
};

/// Connects to `url` (`rtmp://` or `rtmps://`) and publishes `key`.
///
/// The handshake and the `connect` … `publish` exchange read with a 10 s timeout:
/// the publisher retries only an interrupted read, so a shorter one would fail on
/// any server slower than that to answer. Once publishing has started, the read
/// timeout drops to 1 ms so the output thread can poll server messages without
/// blocking the pipeline. The first session is opened here, on the caller's thread.
/// When it drops later, each reconnect attempt opens a new one the same way, with
/// the same timeouts, on a thread of its own. Tests reach it only through a fake
/// server on loopback.
///
/// # Errors
/// When the URL or key is rejected, or the server does not accept the publish.
pub fn connect_rtmp(url: &str, key: &str) -> Result<RtmpPublish, PublishError> {
    connect_rtmp_with(url, key, ReconnectPolicy::default())
}

/// [`connect_rtmp`] with another reconnect policy, so tests need not wait out the
/// default backoff.
fn connect_rtmp_with(
    url: &str,
    key: &str,
    policy: ReconnectPolicy,
) -> Result<RtmpPublish, PublishError> {
    let url = RtmpUrl::parse(url).map_err(rtmp_err)?;
    let key = StreamKey::new(key).map_err(rtmp_err)?;
    let publisher = connect_publisher(&url, &key, &RTMP_TIMEOUTS).map_err(rtmp_err)?;
    let connector = rtmp_connector(url, key, RTMP_TIMEOUTS);
    Ok(RtmpPublish {
        inner: Some(ReconnectingPublisher::new(
            publisher,
            connector,
            policy,
            Clock::system(),
        )),
        deadline: None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{LinkState, LivePublish, RtmpPublish, connect_rtmp, connect_rtmp_with};
    use jstream_mux::amf0::Amf0Value;
    use jstream_mux::flv::{FlvTag, TagType};
    use jstream_outputs::rtmp::ReconnectPolicy;
    use std::io::{ErrorKind, Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
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

    /// A server that answers the handshake and then `connect` only after `delay`,
    /// then accepts the stream. A thread drains what the client sends until it hangs
    /// up, and returns how many bytes that was; it is returned, so the caller can wait
    /// for it or hang up first. The engine's tests publish to it too.
    pub(crate) fn serve(io: TcpStream, delay: Duration) -> thread::JoinHandle<u64> {
        let reader = answer(io.try_clone().expect("clone"), delay);
        thread::spawn(move || {
            let mut total = reader.join().unwrap_or(0);
            let mut io = io;
            io.set_read_timeout(None).expect("blocking reads");
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = io.read(&mut buf) {
                total += n as u64;
            }
            total
        })
    }

    /// Answers the handshake, `connect` and the publish exchange, each `delay` late.
    /// A thread reads what the client sends meanwhile, so the client never blocks on
    /// a write; it returns how many bytes it read once the publish replies are out,
    /// and is returned.
    fn answer(mut io: TcpStream, delay: Duration) -> thread::JoinHandle<u64> {
        let mut c0c1 = vec![0; 1 + HANDSHAKE];
        io.read_exact(&mut c0c1).expect("C0C1");
        thread::sleep(delay);
        let mut reply = vec![3];
        reply.extend((0..HANDSHAKE).map(|i| (i % 251) as u8));
        reply.extend_from_slice(&c0c1[1..]);
        io.write_all(&reply).expect("S0S1S2");
        let mut c2 = vec![0; HANDSHAKE];
        io.read_exact(&mut c2).expect("C2");

        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let mut io = io.try_clone().expect("clone");
            let done = Arc::clone(&done);
            // Reads with a short timeout so it can stop once the replies are out.
            io.set_read_timeout(Some(Duration::from_millis(5)))
                .expect("read timeout");
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut total = 0;
                while !done.load(Ordering::Acquire) {
                    match io.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => total += n as u64,
                        Err(err)
                            if matches!(
                                err.kind(),
                                ErrorKind::WouldBlock | ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
                total
            })
        };
        // The client has sent Set Chunk Size and `connect` by now; answer it late.
        thread::sleep(delay);
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
        thread::sleep(delay);
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
        done.store(true, Ordering::Release);
        reader
    }

    /// [`serve`] with [`DELAY`], until the client hangs up.
    fn slow_server(io: TcpStream) {
        let _ = serve(io, DELAY).join();
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

    /// A keyframe VIDEODATA body (AVC NALU), small enough for one chunk.
    fn keyframe(timestamp: u32) -> FlvTag {
        FlvTag {
            tag_type: TagType::Video,
            timestamp,
            body: vec![0x17, 1, 0, 0, 0, 0, 0, 0, 1, 0x65],
        }
    }

    /// Sends a keyframe and polls every 5 ms until `done`, which is always checked right
    /// after a send and a poll, so a reconnect that finished meanwhile is collected first.
    /// `limit` only stops a test that would otherwise hang: a stalled test process (#189)
    /// can use it all up while nothing runs, so a test must not rely on it for timing.
    fn send_until<P: LivePublish>(
        publish: &mut P,
        timestamp: &mut u32,
        limit: Duration,
        mut done: impl FnMut(&P) -> bool,
    ) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if done(publish) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            *timestamp += 33;
            publish.send_tag(&keyframe(*timestamp)).expect("send");
            publish.poll().expect("poll");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// A server that hangs up mid-stream is reconnected to with the same publish
    /// sequence: the link goes `Reconnecting`, tags sent meanwhile are dropped and
    /// counted instead of failing the send, and the first attempt brings it back
    /// `Active` on the second connection.
    ///
    /// The test waits for that attempt's outcome, not for a time budget (#189). The
    /// attempt runs on a thread of its own and is bounded by the 10 s I/O timeout, so
    /// it always ends; a budget shorter than that failed when the test process stalled
    /// for a few seconds on a loaded runner, though the attempt would have succeeded.
    #[test]
    fn connect_rtmp_reconnects_after_the_server_hangs_up() {
        /// Stops a hung test only: longer than one attempt's 10 s I/O timeout.
        const HANG: Duration = Duration::from_secs(30);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let (first, _) = listener.accept().expect("accept");
            let drain = serve(first.try_clone().expect("clone"), Duration::ZERO);
            // Let some media arrive, then hang up.
            thread::sleep(Duration::from_millis(100));
            first.shutdown(Shutdown::Both).expect("hang up");
            // Accept the reconnect without waiting for the first drain to end.
            let (second, _) = listener.accept().expect("accept the reconnect");
            let _ = drain.join();
            let _ = serve(second, Duration::ZERO).join();
        });

        let policy = ReconnectPolicy {
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(50),
            ..ReconnectPolicy::default()
        };
        let mut publish =
            connect_rtmp_with(&format!("rtmp://127.0.0.1:{port}/live"), "test-key", policy)
                .expect("publishing");
        let mut timestamp = 0;
        assert_eq!(publish.link_state(), LinkState::Active);
        assert!(
            send_until(&mut publish, &mut timestamp, HANG, |p| p.link_state()
                == LinkState::Reconnecting),
            "the hang-up was not noticed"
        );
        let stats = |p: &RtmpPublish| p.inner.as_ref().expect("publishing").stats();
        // Until the first attempt has either published or failed.
        let settled = send_until(&mut publish, &mut timestamp, HANG, |p| {
            p.link_state() == LinkState::Active || stats(p).failed_attempts > 0
        });
        let after = stats(&publish);
        assert!(settled, "the reconnect attempt never ended: {after:?}");
        assert_eq!(
            (
                publish.link_state(),
                after.reconnects,
                after.failed_attempts
            ),
            (LinkState::Active, 1, 0),
            "the first attempt did not reconnect: {after:?}"
        );
        assert!(publish.dropped() > 0, "nothing was counted while down");
        publish.finish().expect("finish");
        drop(publish);
        server.join().expect("server");
    }

    /// A server that accepts the publish and then stops reading. Once the stop has
    /// set a deadline, the first write that blocks gives up when it passes, later
    /// sends fail at once, and `finish` closes without the goodbye. Before, each
    /// blocked write waited out the 10 s write timeout.
    #[test]
    fn a_stopping_rtmp_publish_gives_up_at_its_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (io, _) = listener.accept().expect("accept");
            answer(io.try_clone().expect("clone"), Duration::ZERO)
                .join()
                .expect("reader");
            // Hold the connection open, reading nothing, until the test is done.
            let _ = release_rx.recv();
            drop(io);
        });

        let mut publish =
            connect_rtmp(&format!("rtmp://127.0.0.1:{port}/live"), "test-key").expect("publish");
        let started = Instant::now();
        let deadline = started + Duration::from_millis(300);
        publish.stop_by(deadline);
        let tag = FlvTag {
            tag_type: TagType::Video,
            timestamp: 0,
            body: [&[0x17u8, 1, 0, 0, 0][..], &vec![0u8; 64 * 1024]].concat(),
        };
        let mut failed = false;
        while started.elapsed() < Duration::from_secs(5) {
            if publish.send_tag(&tag).is_err() {
                failed = true;
                break;
            }
        }
        let sent_for = started.elapsed();
        publish.finish().expect("finish");
        let took = started.elapsed();
        let _ = release_tx.send(());
        server.join().expect("server");
        assert!(failed, "sends to a peer that reads nothing never failed");
        assert!(
            sent_for < Duration::from_secs(2),
            "the sends gave up {sent_for:?} after a 300 ms deadline"
        );
        assert!(
            took < Duration::from_secs(2),
            "finish returned {took:?} after a 300 ms deadline"
        );
    }
}
