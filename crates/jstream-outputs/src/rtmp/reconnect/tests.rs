//! Reconnect tests against fake servers on loopback TCP and in-memory pipes, with a
//! clock the test advances by hand (docs/RULES.md#R-TEST-004).

use std::net::TcpListener;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use jstream_mux::amf0::Amf0Value;
use jstream_mux::flv::{ExVideoPacketType, VideoFourCc, ex_video_tag_body};

use super::*;
use crate::rtmp::publisher::tests::{KEY, Mode, accepting_script, fake_server_until, key, url};
use crate::rtmp::test_util::{Duplex, duplex};

/// An AVC sequence header (frame type 1, codec 7, `AVCPacketType` 0).
const AVC_HEADER: [u8; 9] = [0x17, 0, 0, 0, 0, 1, 0x64, 0, 0x1F];
/// An AVC end of sequence (`AVCPacketType` 2), as `jstream-mux` writes it.
const AVC_END: [u8; 5] = [0x17, 2, 0, 0, 0];
/// An AAC sequence header (`AACPacketType` 0) with a 2-byte `AudioSpecificConfig`.
const AAC_HEADER: [u8; 4] = [0xAF, 0, 0x12, 0x10];
/// A raw AAC frame.
const AAC_RAW: [u8; 3] = [0xAF, 1, 0x21];

/// A keyframe NALU tag body, 25 bytes.
fn keyframe() -> Vec<u8> {
    let mut body = vec![0x17, 1, 0, 0, 0];
    body.extend_from_slice(&[0xAA; 20]);
    body
}

/// An inter-frame NALU tag body, 15 bytes.
fn inter() -> Vec<u8> {
    let mut body = vec![0x27, 1, 0, 0, 0];
    body.extend_from_slice(&[0xBB; 10]);
    body
}

fn on_metadata() -> Vec<u8> {
    Amf0Value::encode_all(&[
        Amf0Value::String("onMetaData".into()),
        Amf0Value::EcmaArray(vec![("width".into(), Amf0Value::Number(1280.0))]),
    ])
    .unwrap()
}

/// A clock that moves only when the test says so.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<Instant>>);

impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn clock(&self) -> Clock {
        let now = Arc::clone(&self.0);
        Clock::from_fn(move || *now.lock().unwrap())
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

/// Every state the publisher has been in, without repeats.
struct States(Vec<LinkState>);

impl States {
    fn new() -> Self {
        Self(vec![LinkState::Active])
    }

    fn record<S: Read + Write + Send + 'static>(&mut self, p: &ReconnectingPublisher<S>) {
        if self.0.last() != Some(&p.state()) {
            self.0.push(p.state());
        }
    }
}

/// Polls (with a real 1 ms pause, for the attempt thread) until `done`, recording
/// states. Returns the last poll result.
fn poll_until<S: Read + Write + Send + 'static>(
    p: &mut ReconnectingPublisher<S>,
    states: &mut States,
    done: impl Fn(&ReconnectingPublisher<S>) -> bool,
) -> Result<(), RtmpError> {
    for _ in 0..5000 {
        let result = p.poll();
        states.record(p);
        if done(p) {
            return result;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!(
        "condition not reached; state {:?}, {:?}",
        p.state(),
        p.stats()
    );
}

fn policy() -> ReconnectPolicy {
    ReconnectPolicy {
        max_outage: None,
        ..ReconnectPolicy::default()
    }
}

#[test]
fn backoff_doubles_to_the_cap_with_equal_jitter() {
    let policy = ReconnectPolicy::default();
    for seed in [1, 42, 0xDEAD_BEEF, u64::MAX] {
        let mut backoff = Backoff::new(seed);
        for n in 0..40 {
            let full = Duration::from_secs(1 << n.min(5)).min(Duration::from_secs(30));
            let delay = backoff.delay(&policy, n);
            assert!(
                delay >= full / 2 && delay <= full,
                "attempt {n}: {delay:?} outside [{:?}, {full:?}]",
                full / 2
            );
        }
    }
    // Not constant: the jitter spreads retries.
    let mut backoff = Backoff::new(7);
    let delays: Vec<_> = (0..8).map(|_| backoff.delay(&policy, 4)).collect();
    assert!(delays.windows(2).any(|w| w[0] != w[1]));
}

/// The server goes away mid-stream (peer close, seen by `poll`). The publisher drops
/// media while down, reconnects after the backoff, re-sends `onMetaData` and both
/// sequence headers, and resumes at the next keyframe.
#[test]
fn peer_close_reconnects_resends_headers_and_resumes_at_keyframe() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live2")).unwrap();
    let tc_url = url.tc_url().to_owned();
    let (log_tx, logs) = channel();
    let server = thread::spawn(move || {
        // The first session hangs up after three media messages; the second runs on.
        for hang_up in [Some(3), None] {
            let (tcp, _) = listener.accept().unwrap();
            log_tx
                .send(fake_server_until(tcp, Mode::Accept, &tc_url, hang_up))
                .unwrap();
        }
    });
    let timeouts = SessionTimeouts {
        connect: Duration::from_secs(5),
        write: Duration::from_secs(5),
        poll: Duration::from_millis(1),
    };
    let connector = rtmp_connector(url, key(), timeouts);
    let clock = FakeClock::new();
    let mut p =
        ReconnectingPublisher::new(connector().unwrap(), connector, policy(), clock.clock());
    let mut states = States::new();

    p.send_metadata(&on_metadata()).unwrap();
    p.send_video(0, &AVC_HEADER).unwrap();
    p.send_audio(0, &AAC_HEADER).unwrap();
    p.send_video(0, &keyframe()).unwrap();
    let first = logs.recv().unwrap(); // the server has hung up
    assert_eq!(
        first[first.len() - 4..],
        [
            "data @setDataFrame onMetaData",
            "video t=0 len=9 first=0x17",
            "audio t=0 len=4 first=0xaf",
            "video t=0 len=25 first=0x17",
        ]
    );

    poll_until(&mut p, &mut states, |p| {
        p.state() == LinkState::Reconnecting
    })
    .unwrap();
    // Down: media is dropped and counted, nothing is queued.
    p.send_video(33, &inter()).unwrap();
    p.send_audio(23, &AAC_RAW).unwrap();
    // The first attempt waits between 0.5 s and 1 s.
    clock.advance(Duration::from_millis(499));
    p.poll().unwrap();
    assert_eq!(p.stats().attempts, 0, "no attempt before the backoff");
    clock.advance(Duration::from_millis(501));
    poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();

    // Resuming: audio and inter frames wait for the keyframe.
    p.send_audio(40, &AAC_RAW).unwrap();
    p.send_video(66, &inter()).unwrap();
    p.send_video(100, &keyframe()).unwrap();
    p.send_audio(110, &AAC_RAW).unwrap();
    p.send_video(133, &inter()).unwrap();
    assert_eq!(
        p.stats(),
        ReconnectStats {
            reconnects: 1,
            attempts: 1,
            failed_attempts: 0,
            dropped_video: 2,
            dropped_audio: 2,
        }
    );
    drop(p.finish().unwrap().expect("connected"));

    let second = logs.recv().unwrap();
    server.join().unwrap();
    assert_eq!(
        second,
        [
            "set-chunk-size 4096",
            "connect txn=1 stream=0",
            "window-ack 2500000",
            "releaseStream txn=2 stream=0",
            "FCPublish txn=3 stream=0",
            "createStream txn=4 stream=0",
            "publish txn=5 stream=1",
            "pong 1234",
            "data @setDataFrame onMetaData",
            "video t=100 len=9 first=0x17",
            "video t=100 len=25 first=0x17",
            "audio t=110 len=4 first=0xaf",
            "audio t=110 len=3 first=0xaf",
            "video t=133 len=15 first=0x27",
            "FCUnpublish txn=6 stream=0",
            "deleteStream txn=7 stream=0",
        ]
    );
    assert_eq!(
        states.0,
        [
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Active
        ]
    );
}

/// A send that fails (the server is gone, so the write breaks) drops that packet and
/// starts reconnecting. An audio-only stream resumes at once, with its header first.
#[test]
fn failed_send_drops_the_packet_and_audio_only_resumes_at_once() {
    let (log_tx, logs) = channel();
    let connections = Arc::new(AtomicUsize::new(0));
    let connector: Connector<Duplex> = Arc::new(move || {
        let (client, server) = duplex();
        let hang_up = (connections.fetch_add(1, Ordering::SeqCst) == 0).then_some(2);
        let log_tx = log_tx.clone();
        thread::spawn(move || {
            let log = fake_server_until(server, Mode::Accept, url().tc_url(), hang_up);
            let _ = log_tx.send(log);
        });
        let mut p = RtmpPublisher::publish(client, &url(), &key())?;
        p.get_mut().nonblocking = true;
        Ok(p)
    });
    let clock = FakeClock::new();
    let mut p =
        ReconnectingPublisher::new(connector().unwrap(), connector, policy(), clock.clock());
    let mut states = States::new();

    p.send_audio(0, &AAC_HEADER).unwrap();
    p.send_audio(0, &AAC_RAW).unwrap();
    logs.recv().unwrap(); // the server has hung up
    p.send_audio(23, &AAC_RAW).unwrap();
    states.record(&p);
    assert_eq!(p.state(), LinkState::Reconnecting);
    assert_eq!(p.stats().dropped_audio, 1, "the packet whose send failed");

    clock.advance(Duration::from_secs(1));
    poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();
    p.send_audio(46, &AAC_RAW).unwrap();
    drop(p.finish().unwrap());

    let second = logs.recv().unwrap();
    let media: Vec<_> = second
        .iter()
        .filter(|l| l.starts_with("audio") || l.starts_with("video") || l.starts_with("data"))
        .collect();
    assert_eq!(
        media,
        ["audio t=46 len=4 first=0xaf", "audio t=46 len=3 first=0xaf"]
    );
    assert_eq!(
        states.0,
        [
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Active
        ]
    );
}

/// A link lost and restored after the AVC sequence header but before the first video
/// frame: the new session still gets `onMetaData` and the AVC header before the first keyframe
/// (#127 review B1).
#[test]
fn a_loss_before_the_first_frame_still_resends_the_avc_header() {
    let (log_tx, logs) = channel();
    let connections = Arc::new(AtomicUsize::new(0));
    let connector: Connector<Duplex> = Arc::new(move || {
        let (client, server) = duplex();
        let hang_up = (connections.fetch_add(1, Ordering::SeqCst) == 0).then_some(1);
        let log_tx = log_tx.clone();
        thread::spawn(move || {
            let log = fake_server_until(server, Mode::Accept, url().tc_url(), hang_up);
            let _ = log_tx.send(log);
        });
        let mut p = RtmpPublisher::publish(client, &url(), &key())?;
        p.get_mut().nonblocking = true;
        Ok(p)
    });
    let clock = FakeClock::new();
    let mut p =
        ReconnectingPublisher::new(connector().unwrap(), connector, policy(), clock.clock());
    let mut states = States::new();

    p.send_metadata(&on_metadata()).unwrap();
    p.send_video(0, &AVC_HEADER).unwrap();
    logs.recv().unwrap(); // the server has hung up before any frame
    // Audio finds the loss; the link is back before any video frame was sent.
    p.send_audio(23, &AAC_RAW).unwrap();
    states.record(&p);
    assert_eq!(p.state(), LinkState::Reconnecting);

    clock.advance(Duration::from_secs(1));
    poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();
    p.send_video(66, &keyframe()).unwrap();
    drop(p.finish().unwrap());

    let second = logs.recv().unwrap();
    let media: Vec<_> = second
        .iter()
        .filter(|l| l.starts_with("audio") || l.starts_with("video") || l.starts_with("data"))
        .map(|l| l.split(" len=").next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        media,
        ["data @setDataFrame onMetaData", "video t=66", "video t=66"],
        "{second:?}"
    );
    let video: Vec<_> = second.iter().filter(|l| l.starts_with("video")).collect();
    assert!(
        video[0].contains("len=9"),
        "the AVC header first: {video:?}"
    );
    assert!(video[1].contains("len=25"), "then the keyframe: {video:?}");
}

/// An enhanced RTMP video tag body (HEVC or AV1), built as `jstream-mux` builds it.
fn ex_video(keyframe: bool, packet_type: ExVideoPacketType, fourcc: VideoFourCc) -> Vec<u8> {
    let payload: &[u8] = match packet_type {
        ExVideoPacketType::SequenceEnd => &[],
        ExVideoPacketType::SequenceStart => &[0x01, 0x02, 0x03],
        _ if keyframe => &[0xAA; 20],
        _ => &[0xBB; 10],
    };
    ex_video_tag_body(keyframe, packet_type, fourcc, 0, payload).unwrap()
}

/// Every enhanced RTMP `VideoPacketType`, and the legacy AVC layout, is classified as
/// what the resync needs: only `SequenceStart` is kept as the header (never
/// `SequenceEnd`), and only a coded keyframe resumes video (#399).
#[test]
fn classifies_enhanced_and_legacy_video_tags() {
    use ExVideoPacketType::{CodedFrames, CodedFramesX, SequenceEnd, SequenceStart};
    for fourcc in [VideoFourCc::Hevc, VideoFourCc::Av1] {
        let class = |key, packet_type| classify_video(&ex_video(key, packet_type, fourcc));
        assert!(class(true, SequenceStart) == Packet::Header, "{fourcc:?}");
        assert!(class(true, CodedFrames) == Packet::Keyframe, "{fourcc:?}");
        assert!(class(true, CodedFramesX) == Packet::Keyframe, "{fourcc:?}");
        assert!(class(false, CodedFrames) == Packet::Other, "{fourcc:?}");
        assert!(class(false, CodedFramesX) == Packet::Other, "{fourcc:?}");
        assert!(class(true, SequenceEnd) == Packet::Other, "{fourcc:?}");
        assert!(class(false, SequenceEnd) == Packet::Other, "{fourcc:?}");
    }
    // Packet types `jstream-mux` doesn't write: Metadata (4), MPEG2TSSequenceStart (5),
    // Multitrack (6), ModEx (7), each on a keyframe; and a command frame (type 5).
    for first in [0x94, 0x95, 0x96, 0x97, 0xD1] {
        let body = [first, b'h', b'v', b'c', b'1', 0];
        assert!(classify_video(&body) == Packet::Other, "{first:#x}");
    }
    // The legacy AVC layout is unchanged.
    assert!(classify_video(&AVC_HEADER) == Packet::Header);
    assert!(classify_video(&keyframe()) == Packet::Keyframe);
    assert!(classify_video(&inter()) == Packet::Other);
    // An AVC end of sequence (`AVCPacketType` 2) is on a keyframe but is neither a
    // header nor a point to resume at (#411).
    assert!(classify_video(&AVC_END) == Packet::Other);
    assert!(classify_video(&[0x27, 2, 0, 0, 0]) == Packet::Other);
}

/// An enhanced RTMP (HEVC or AV1) or legacy AVC stream survives a link drop: the
/// sequence header is re-sent before the next coded keyframe and video resumes there;
/// inter frames and an end of sequence sent while resyncing are dropped, and the end of
/// sequence doesn't replace the cached header (#399, #411).
#[test]
fn video_resends_sequence_header_and_resumes_at_keyframe() {
    let mut cases = Vec::new();
    for (fourcc, coded) in [
        (VideoFourCc::Hevc, ExVideoPacketType::CodedFrames),
        (VideoFourCc::Av1, ExVideoPacketType::CodedFramesX),
    ] {
        cases.push((
            format!("{fourcc:?}"),
            ex_video(true, ExVideoPacketType::SequenceStart, fourcc),
            ex_video(true, coded, fourcc),
            ex_video(false, coded, fourcc),
            ex_video(true, ExVideoPacketType::SequenceEnd, fourcc),
        ));
    }
    cases.push((
        "Avc".to_owned(),
        AVC_HEADER.to_vec(),
        keyframe(),
        inter(),
        AVC_END.to_vec(),
    ));
    for (name, start, idr, inter, end) in cases {
        let (log_tx, logs) = channel();
        let connections = Arc::new(AtomicUsize::new(0));
        let connector: Connector<Duplex> = Arc::new(move || {
            let (client, server) = duplex();
            let hang_up = (connections.fetch_add(1, Ordering::SeqCst) == 0).then_some(2);
            let log_tx = log_tx.clone();
            thread::spawn(move || {
                let log = fake_server_until(server, Mode::Accept, url().tc_url(), hang_up);
                let _ = log_tx.send(log);
            });
            let mut p = RtmpPublisher::publish(client, &url(), &key())?;
            p.get_mut().nonblocking = true;
            Ok(p)
        });
        let clock = FakeClock::new();
        let mut p =
            ReconnectingPublisher::new(connector().unwrap(), connector, policy(), clock.clock());
        let mut states = States::new();

        p.send_video(0, &start).unwrap();
        p.send_video(0, &idr).unwrap();
        logs.recv().unwrap(); // the server has hung up
        p.send_video(33, &inter).unwrap();
        states.record(&p);
        assert_eq!(p.state(), LinkState::Reconnecting, "{name}");

        clock.advance(Duration::from_secs(1));
        poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();
        // Resyncing: these wait for the keyframe and are dropped.
        p.send_video(66, &inter).unwrap();
        p.send_video(80, &end).unwrap();
        p.send_video(100, &idr).unwrap();
        p.send_video(133, &inter).unwrap();
        assert_eq!(p.stats().dropped_video, 3, "{name}: {:?}", p.stats());
        drop(p.finish().unwrap());

        let second = logs.recv().unwrap();
        let media: Vec<_> = second
            .iter()
            .filter(|l| l.starts_with("video"))
            .cloned()
            .collect();
        let line =
            |t: u32, body: &[u8]| format!("video t={t} len={} first={:#04x}", body.len(), body[0]);
        assert_eq!(
            media,
            [line(100, &start), line(100, &idr), line(133, &inter)],
            "{name}"
        );
    }
}

/// Sessions over in-memory pipes whose server ends the test holds. The server has
/// already answered the publish; [`hang_up`](Self::hang_up) drops every held end, so
/// the client's next read sees the connection closed and its next write fails.
#[derive(Clone, Default)]
struct HeldServers(Arc<Mutex<Vec<Duplex>>>);

impl HeldServers {
    fn session(&self) -> Result<RtmpPublisher<Duplex>, RtmpError> {
        let (client, mut server) = duplex();
        server.write_all(&accepting_script(|_| {})).unwrap();
        self.0.lock().unwrap().push(server);
        let mut p = RtmpPublisher::publish(client, &url(), &key())?;
        p.get_mut().nonblocking = true;
        Ok(p)
    }

    fn connector(&self) -> Connector<Duplex> {
        let servers = self.clone();
        Arc::new(move || servers.session())
    }

    fn hang_up(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// A connector whose every attempt fails, and how many times it was called.
fn refusing_connector() -> (Connector<Duplex>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let connector: Connector<Duplex> = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Err(RtmpError::ConnectionClosed { during: "connect" })
    });
    (connector, calls)
}

#[test]
fn gives_up_after_max_attempts() {
    let servers = HeldServers::default();
    let (connector, calls) = refusing_connector();
    let clock = FakeClock::new();
    let policy = ReconnectPolicy {
        max_attempts: Some(3),
        ..policy()
    };
    let mut p =
        ReconnectingPublisher::new(servers.session().unwrap(), connector, policy, clock.clock());
    let mut states = States::new();

    servers.hang_up();
    p.poll().unwrap();
    states.record(&p);
    assert_eq!(p.state(), LinkState::Reconnecting);
    p.poll().unwrap();
    assert_eq!(p.stats().attempts, 0, "no attempt before the backoff");
    for n in 0..3u32 {
        clock.advance(Duration::from_secs(1 << n)); // the longest wait for attempt n
        let result = poll_until(&mut p, &mut states, |p| {
            p.stats().failed_attempts == u64::from(n) + 1
        });
        if n < 2 {
            result.unwrap();
            assert_eq!(p.state(), LinkState::Reconnecting);
        } else {
            assert!(
                matches!(result, Err(RtmpError::ReconnectGaveUp { attempts: 3, .. })),
                "{result:?}"
            );
        }
    }
    assert_eq!(p.state(), LinkState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // Once failed, every call says so, and nothing else is attempted.
    clock.advance(Duration::from_secs(60));
    assert!(matches!(
        p.send_video(0, &keyframe()),
        Err(RtmpError::ReconnectGaveUp { attempts: 3, .. })
    ));
    let err = p.poll().unwrap_err().to_string();
    assert!(!err.contains(KEY), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        states.0,
        [
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Failed
        ]
    );
    assert!(p.finish().unwrap().is_none());
}

#[test]
fn gives_up_when_the_next_attempt_would_pass_the_outage_limit() {
    let servers = HeldServers::default();
    let (connector, calls) = refusing_connector();
    let clock = FakeClock::new();
    let policy = ReconnectPolicy {
        max_outage: Some(Duration::from_secs(10)),
        ..ReconnectPolicy::default()
    };
    let mut p =
        ReconnectingPublisher::new(servers.session().unwrap(), connector, policy, clock.clock());
    let mut states = States::new();
    servers.hang_up();
    p.poll().unwrap();
    // Attempts at most 1, 3 and 7 s in. After the third fails, the fourth would start
    // 4 to 8 s later, past 10 s.
    let mut last = Ok(());
    for n in 0..3u32 {
        clock.advance(Duration::from_secs(1 << n));
        last = poll_until(&mut p, &mut states, |p| {
            p.stats().failed_attempts == u64::from(n) + 1
        });
    }
    match last {
        Err(RtmpError::ReconnectGaveUp { attempts, outage }) => {
            assert_eq!(attempts, 3);
            assert_eq!(outage, Duration::from_secs(7));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(p.state(), LinkState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// A server that accepts the publish and drops it at once must not keep the publisher
/// retrying forever: a session lost within `stable_after` continues the outage.
#[test]
fn a_session_that_drops_again_at_once_continues_the_outage() {
    let servers = HeldServers::default();
    let clock = FakeClock::new();
    let policy = ReconnectPolicy {
        max_attempts: Some(2),
        ..policy()
    };
    let mut p = ReconnectingPublisher::new(
        servers.session().unwrap(),
        servers.connector(),
        policy,
        clock.clock(),
    );
    let mut states = States::new();
    servers.hang_up();
    p.poll().unwrap();
    states.record(&p);

    clock.advance(Duration::from_secs(1));
    poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();
    servers.hang_up();
    p.poll().unwrap(); // lost again at once
    states.record(&p);
    assert_eq!(p.state(), LinkState::Reconnecting);

    clock.advance(Duration::from_secs(2)); // the second step of the same outage
    poll_until(&mut p, &mut states, |p| p.stats().reconnects == 2).unwrap();
    servers.hang_up();
    assert!(matches!(
        p.poll(),
        Err(RtmpError::ReconnectGaveUp { attempts: 2, .. })
    ));
    states.record(&p);
    assert_eq!(
        p.stats(),
        ReconnectStats {
            reconnects: 2,
            attempts: 2,
            failed_attempts: 2,
            dropped_video: 0,
            dropped_audio: 0,
        }
    );
    assert_eq!(
        states.0,
        [
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Failed
        ]
    );
}

/// A server that keeps accepting the publish and dropping it (#746: YouTube with a
/// stream it can't decode) must not read as live between drops: once a session has
/// dropped soon after reconnecting, the next one reports `Reconnecting` until it has
/// stayed up for `stable_after`.
#[test]
fn a_flapping_session_reports_reconnecting_until_it_is_stable() {
    let servers = HeldServers::default();
    let clock = FakeClock::new();
    let mut p = ReconnectingPublisher::new(
        servers.session().unwrap(),
        servers.connector(),
        policy(),
        clock.clock(),
    );
    let mut states = States::new();
    servers.hang_up();
    p.poll().unwrap();
    states.record(&p);

    clock.advance(Duration::from_secs(1));
    poll_until(&mut p, &mut states, |p| p.stats().reconnects == 1).unwrap();
    assert_eq!(p.state(), LinkState::Active, "one drop is not yet a flap");
    clock.advance(Duration::from_secs(10));
    servers.hang_up();
    p.poll().unwrap(); // lost again within `stable_after`
    states.record(&p);

    clock.advance(Duration::from_secs(2));
    poll_until(&mut p, &mut states, |p| p.stats().reconnects == 2).unwrap();
    assert_eq!(p.state(), LinkState::Reconnecting, "up, but not stable yet");
    clock.advance(Duration::from_secs(29));
    p.poll().unwrap();
    states.record(&p);
    assert_eq!(p.state(), LinkState::Reconnecting);
    clock.advance(Duration::from_secs(1)); // `stable_after` up
    p.poll().unwrap();
    states.record(&p);
    assert_eq!(p.state(), LinkState::Active);
    assert_eq!(
        states.0,
        [
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Active,
            LinkState::Reconnecting,
            LinkState::Active
        ]
    );
}

#[test]
fn a_session_that_stayed_up_starts_a_new_outage() {
    let servers = HeldServers::default();
    let clock = FakeClock::new();
    let policy = ReconnectPolicy {
        max_attempts: Some(1),
        ..policy()
    };
    let mut p = ReconnectingPublisher::new(
        servers.session().unwrap(),
        servers.connector(),
        policy,
        clock.clock(),
    );
    let mut states = States::new();
    servers.hang_up();
    p.poll().unwrap();
    clock.advance(Duration::from_secs(1));
    poll_until(&mut p, &mut states, |p| p.state() == LinkState::Active).unwrap();
    clock.advance(Duration::from_secs(31)); // longer than `stable_after`
    servers.hang_up();
    p.poll().unwrap();
    assert_eq!(
        p.state(),
        LinkState::Reconnecting,
        "a fresh outage, not a give-up"
    );
    assert_eq!(p.stats().failed_attempts, 0);
}

#[test]
fn caller_errors_leave_the_link_alone() {
    let servers = HeldServers::default();
    let mut p = ReconnectingPublisher::new(
        servers.session().unwrap(),
        servers.connector(),
        policy(),
        FakeClock::new().clock(),
    );
    assert!(matches!(
        p.send_video(0, &[]),
        Err(RtmpError::PayloadSize { len: 0 })
    ));
    assert!(matches!(
        p.send_metadata(&[]),
        Err(RtmpError::PayloadSize { len: 0 })
    ));
    p.poll().unwrap();
    assert_eq!(p.state(), LinkState::Active);
    assert_eq!(p.stats(), ReconnectStats::default());
}

#[test]
fn zero_attempts_fails_on_the_first_loss() {
    let servers = HeldServers::default();
    let policy = ReconnectPolicy {
        max_attempts: Some(0),
        ..policy()
    };
    let mut p = ReconnectingPublisher::new(
        servers.session().unwrap(),
        servers.connector(),
        policy,
        FakeClock::new().clock(),
    );
    servers.hang_up();
    assert!(matches!(
        p.poll(),
        Err(RtmpError::ReconnectGaveUp { attempts: 0, .. })
    ));
    assert_eq!(p.state(), LinkState::Failed);
    assert!(p.finish().unwrap().is_none());
}
