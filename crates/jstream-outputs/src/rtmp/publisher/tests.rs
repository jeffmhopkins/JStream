//! Publish-session tests against in-memory fake servers (docs/RULES.md#R-TEST-004).

use std::thread;

use super::*;
use crate::rtmp::handshake::{HANDSHAKE_SIZE, RTMP_VERSION};
use crate::rtmp::test_util::{Duplex, Scripted, duplex};

const KEY: &str = "abcd-1234-efgh-5678";

fn s(v: &str) -> Amf0Value {
    Amf0Value::str(v)
}

fn n(v: f64) -> Amf0Value {
    Amf0Value::Number(v)
}

fn obj(props: &[(&str, Amf0Value)]) -> Amf0Value {
    Amf0Value::Object(
        props
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn status(level: &str, code: &str) -> Amf0Value {
    obj(&[
        ("level", s(level)),
        ("code", s(code)),
        ("description", s("test")),
    ])
}

fn url() -> RtmpUrl {
    RtmpUrl::parse("rtmp://ingest.example.invalid/live2").unwrap()
}

fn key() -> StreamKey {
    StreamKey::new(KEY).unwrap()
}

/// Builds server-to-client bytes.
struct Wire {
    enc: ChunkEncoder,
    out: Vec<u8>,
}

impl Wire {
    fn new() -> Self {
        Self {
            enc: ChunkEncoder::new(),
            out: Vec::new(),
        }
    }

    fn control(&mut self, type_id: u8, payload: &[u8]) {
        self.enc
            .encode(CSID_CONTROL, 0, type_id, 0, payload, &mut self.out)
            .unwrap();
    }

    fn command(&mut self, stream_id: u32, values: &[Amf0Value]) {
        let mut body = Vec::new();
        for v in values {
            amf0::encode(v, &mut body).unwrap();
        }
        self.enc
            .encode(
                CSID_COMMAND,
                0,
                msg_type::COMMAND_AMF0,
                stream_id,
                &body,
                &mut self.out,
            )
            .unwrap();
    }

    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

/// What the fake server does at each step.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Accept,
    RejectConnect,
    BadKey,
}

/// A fake ingest server on the far end of a [`Duplex`]. It answers the publish
/// sequence and returns a log of every message the client sent, in order.
fn fake_server(mut io: Duplex, mode: Mode) -> Vec<String> {
    // Handshake.
    let mut c0c1 = vec![0; 1 + HANDSHAKE_SIZE];
    io.read_exact(&mut c0c1).unwrap();
    assert_eq!(c0c1[0], RTMP_VERSION);
    let s1: Vec<u8> = (0..HANDSHAKE_SIZE).map(|i| (i * 7 % 256) as u8).collect();
    let mut reply = vec![RTMP_VERSION];
    reply.extend_from_slice(&s1);
    reply.extend_from_slice(&c0c1[1..]);
    io.write_all(&reply).unwrap();
    let mut c2 = vec![0; HANDSHAKE_SIZE];
    io.read_exact(&mut c2).unwrap();
    assert_eq!(c2, s1, "C2 echoes S1");

    let mut dec = ChunkDecoder::new();
    let mut wire = Wire::new();
    let mut log = Vec::new();
    let mut buf = [0u8; 1000]; // smaller than a video frame: exercises partial reads
    loop {
        let m = match dec.next_message().unwrap() {
            Some(m) => m,
            None => {
                let got = io.read(&mut buf).unwrap();
                if got == 0 {
                    return log;
                }
                dec.push(&buf[..got]);
                continue;
            }
        };
        let be32 = |p: &[u8]| u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
        match m.type_id {
            msg_type::SET_CHUNK_SIZE => {
                let size = be32(&m.payload);
                dec.set_chunk_size(size).unwrap();
                log.push(format!("set-chunk-size {size}"));
            }
            msg_type::WINDOW_ACK_SIZE => log.push(format!("window-ack {}", be32(&m.payload))),
            msg_type::ACK => log.push(format!("ack {}", be32(&m.payload))),
            msg_type::USER_CONTROL => {
                assert_eq!(m.payload[..2], UC_PING_RESPONSE.to_be_bytes());
                log.push(format!("pong {}", be32(&m.payload[2..])));
            }
            msg_type::VIDEO | msg_type::AUDIO => {
                assert_eq!(m.stream_id, 1);
                let kind = if m.type_id == msg_type::VIDEO {
                    "video"
                } else {
                    "audio"
                };
                log.push(format!(
                    "{kind} t={} len={} first={:#04x}",
                    m.timestamp,
                    m.payload.len(),
                    m.payload[0]
                ));
            }
            msg_type::DATA_AMF0 => {
                assert_eq!(m.stream_id, 1);
                let v = amf0::decode_all(&m.payload).unwrap();
                let names: Vec<_> = v.iter().filter_map(Amf0Value::as_str).collect();
                log.push(format!("data {}", names.join(" ")));
            }
            msg_type::COMMAND_AMF0 => {
                let v = amf0::decode_all(&m.payload).unwrap();
                let name = v[0].as_str().unwrap().to_owned();
                let txn = v[1].as_number().unwrap();
                log.push(format!("{name} txn={txn} stream={}", m.stream_id));
                match name.as_str() {
                    "connect" => {
                        assert_eq!(v[2].get("app"), Some(&s("live2")));
                        assert_eq!(
                            v[2].get("tcUrl"),
                            Some(&s("rtmp://ingest.example.invalid/live2"))
                        );
                        if mode == Mode::RejectConnect {
                            wire.command(
                                0,
                                &[
                                    s("_error"),
                                    n(txn),
                                    Amf0Value::Null,
                                    status("error", "NetConnection.Connect.Rejected"),
                                ],
                            );
                        } else {
                            wire.control(msg_type::WINDOW_ACK_SIZE, &2_500_000u32.to_be_bytes());
                            let mut bw = 2_500_000u32.to_be_bytes().to_vec();
                            bw.push(2);
                            wire.control(msg_type::SET_PEER_BANDWIDTH, &bw);
                            wire.control(msg_type::SET_CHUNK_SIZE, &4096u32.to_be_bytes());
                            wire.enc.set_chunk_size(4096).unwrap();
                            wire.command(0, &[s("onBWDone"), n(0.0), Amf0Value::Null]);
                            wire.command(
                                0,
                                &[
                                    s("_result"),
                                    n(txn),
                                    obj(&[("fmsVer", s("FMS/3,0,1,123"))]),
                                    status("status", "NetConnection.Connect.Success"),
                                ],
                            );
                        }
                    }
                    "releaseStream" => {
                        assert_eq!(v[3], s(KEY));
                        wire.command(
                            0,
                            &[s("_result"), n(txn), Amf0Value::Null, Amf0Value::Undefined],
                        );
                    }
                    "FCPublish" => {
                        assert_eq!(v[3], s(KEY));
                        wire.command(0, &[s("onFCPublish"), n(0.0), Amf0Value::Null]);
                    }
                    "createStream" => {
                        wire.command(0, &[s("_result"), n(txn), Amf0Value::Null, n(1.0)]);
                    }
                    "publish" => {
                        assert_eq!(m.stream_id, 1);
                        assert_eq!((&v[3], &v[4]), (&s(KEY), &s("live")));
                        if mode == Mode::BadKey {
                            wire.command(
                                1,
                                &[
                                    s("onStatus"),
                                    n(0.0),
                                    Amf0Value::Null,
                                    // A hostile server echoes the key in both fields.
                                    obj(&[
                                        ("level", s("error")),
                                        ("code", s(&format!("NetStream.Publish.BadName.{KEY}"))),
                                        ("description", s(&format!("bad stream name {KEY}"))),
                                    ]),
                                ],
                            );
                        } else {
                            wire.control(msg_type::USER_CONTROL, &[0, 0, 0, 0, 0, 1]);
                            wire.control(msg_type::USER_CONTROL, &[0, 6, 0, 0, 0x04, 0xD2]);
                            wire.command(
                                1,
                                &[
                                    s("onStatus"),
                                    n(0.0),
                                    Amf0Value::Null,
                                    status("status", "NetStream.Publish.Start"),
                                ],
                            );
                        }
                    }
                    _ => {}
                }
                let out = wire.take();
                if !out.is_empty() && io.write_all(&out).is_err() {
                    return log; // the client hung up after a rejection
                }
            }
            other => panic!("unexpected message type {other}"),
        }
    }
}

#[test]
fn full_publish_sequence_in_order() {
    let (client, server) = duplex();
    let server = thread::spawn(move || fake_server(server, Mode::Accept));

    let mut p =
        RtmpPublisher::publish_seeded(client, &url(), &key(), &PublishOptions::default(), 42)
            .unwrap();
    assert_eq!(p.stream_id(), 1);

    let mut on_metadata = Vec::new();
    amf0::encode(&s("onMetaData"), &mut on_metadata).unwrap();
    amf0::encode(
        &Amf0Value::EcmaArray(vec![("width".into(), n(1920.0))]),
        &mut on_metadata,
    )
    .unwrap();
    p.send_metadata(&on_metadata).unwrap();
    // A 10 000-byte keyframe spans three 4096-byte chunks.
    p.send_video(0, &[0x17; 10_000]).unwrap();
    p.send_audio(0, &[0xAF, 0x00, 0x12, 0x10]).unwrap();
    p.send_video(33, &[0x27; 500]).unwrap();
    p.send_audio(23, &[0xAF, 0x01, 0x21]).unwrap();
    p.send_video(66, &[0x27; 500]).unwrap();
    // Past 0xFFFFFF ms (about 4.6 hours): extended timestamps.
    p.send_video(0x0100_0000, &[0x27; 9000]).unwrap();
    assert!(p.bytes_sent() > 20_000);
    assert!(p.bytes_received() > 0);
    drop(p.finish().unwrap());

    let log = server.join().unwrap();
    assert_eq!(
        log,
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
            "video t=0 len=10000 first=0x17",
            "audio t=0 len=4 first=0xaf",
            "video t=33 len=500 first=0x27",
            "audio t=23 len=3 first=0xaf",
            "video t=66 len=500 first=0x27",
            "video t=16777216 len=9000 first=0x27",
            "FCUnpublish txn=6 stream=0",
            "deleteStream txn=7 stream=0",
        ]
    );
}

#[test]
fn connect_error_is_rejected() {
    let (client, server) = duplex();
    let server = thread::spawn(move || fake_server(server, Mode::RejectConnect));
    let err = RtmpPublisher::publish_seeded(client, &url(), &key(), &PublishOptions::default(), 1)
        .err()
        .unwrap();
    match err {
        RtmpError::Rejected { command, code, .. } => {
            assert_eq!(command, "connect");
            assert_eq!(code, "NetConnection.Connect.Rejected");
        }
        other => panic!("unexpected {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn bad_stream_key_is_rejected() {
    let (client, server) = duplex();
    let server = thread::spawn(move || fake_server(server, Mode::BadKey));
    let err = RtmpPublisher::publish_seeded(client, &url(), &key(), &PublishOptions::default(), 1)
        .err()
        .unwrap();
    assert!(
        matches!(&err, RtmpError::Rejected { command: "publish", code, description }
            if code == "NetStream.Publish.BadName.***" && description == "bad stream name ***"),
        "{err:?}"
    );
    // Neither the error text nor its debug form includes the key, even when the server
    // echoes it (R-SEC-001).
    assert!(!err.to_string().contains(KEY));
    assert!(!format!("{err:?}").contains(KEY));
    server.join().unwrap();
}

/// Server bytes for a handshake and a successful publish on stream 1.
fn accepting_script(after_start: impl FnOnce(&mut Wire)) -> Vec<u8> {
    let mut bytes = vec![RTMP_VERSION];
    bytes.extend_from_slice(&[0; 2 * HANDSHAKE_SIZE]);
    let mut wire = Wire::new();
    wire.command(0, &[s("_result"), n(1.0), Amf0Value::Null, Amf0Value::Null]);
    wire.command(0, &[s("_result"), n(4.0), Amf0Value::Null, n(1.0)]);
    let start = status("status", "NetStream.Publish.Start");
    wire.command(1, &[s("onStatus"), n(0.0), Amf0Value::Null, start]);
    after_start(&mut wire);
    bytes.extend_from_slice(&wire.take());
    bytes
}

/// The client's messages after the handshake.
fn client_messages(written: &[u8]) -> Vec<Message> {
    let mut dec = ChunkDecoder::new();
    dec.push(&written[1 + 2 * HANDSHAKE_SIZE..]);
    let mut msgs = Vec::new();
    while let Some(m) = dec.next_message().unwrap() {
        if m.type_id == msg_type::SET_CHUNK_SIZE {
            let p = &m.payload;
            dec.set_chunk_size(u32::from_be_bytes([p[0], p[1], p[2], p[3]]))
                .unwrap();
        }
        msgs.push(m);
    }
    msgs
}

#[test]
fn poll_answers_pings_then_reports_stream_errors() {
    let input = accepting_script(|w| {
        w.control(msg_type::USER_CONTROL, &[0, 6, 0, 0, 0, 99]);
        w.command(
            1,
            &[
                s("onStatus"),
                n(0.0),
                Amf0Value::Null,
                status("error", "NetStream.Publish.Idle"),
            ],
        );
    });
    let mut p = RtmpPublisher::publish_seeded(
        Scripted::new(input),
        &url(),
        &key(),
        &PublishOptions::default(),
        3,
    )
    .unwrap();
    let err = p.poll_incoming().unwrap_err();
    assert!(
        matches!(&err, RtmpError::Rejected { code, .. } if code == "NetStream.Publish.Idle"),
        "{err:?}"
    );
    let last = client_messages(p.get_ref().written()).pop().unwrap();
    assert_eq!(last.type_id, msg_type::USER_CONTROL);
    assert_eq!(last.payload, [0, 7, 0, 0, 0, 99]);
}

#[test]
fn poll_with_nothing_pending_returns() {
    let (client, mut server) = duplex();
    let script = accepting_script(|_| {});
    server.write_all(&script).unwrap();
    let mut p =
        RtmpPublisher::publish_seeded(client, &url(), &key(), &PublishOptions::default(), 5)
            .unwrap();
    p.get_mut().nonblocking = true;
    p.poll_incoming().unwrap();
    drop(server);
    assert!(matches!(
        p.poll_incoming(),
        Err(RtmpError::ConnectionClosed { .. })
    ));
}

#[test]
fn acknowledges_each_window() {
    let mut wire = Wire::new();
    wire.control(msg_type::WINDOW_ACK_SIZE, &100u32.to_be_bytes());
    for i in 0..10 {
        wire.command(0, &[s("onFiller"), n(f64::from(i)), s("0123456789abcdef")]);
    }
    let input = wire.take();
    let total = input.len() as u32;
    let mut conn = Connection::new(Scripted::new(input));
    for _ in 0..10 {
        conn.recv("test").unwrap();
    }
    let mut dec = ChunkDecoder::new();
    dec.push(conn.stream.written());
    let m = dec.next_message().unwrap().unwrap();
    assert_eq!(m.type_id, msg_type::ACK);
    assert_eq!(
        m.payload,
        total.to_be_bytes(),
        "sequence number is bytes received"
    );
    assert_eq!(dec.next_message().unwrap(), None, "one ack per window");
}

#[test]
fn peer_chunk_size_and_abort_are_applied() {
    let mut wire = Wire::new();
    wire.control(msg_type::SET_CHUNK_SIZE, &8u32.to_be_bytes());
    wire.enc.set_chunk_size(8).unwrap();
    wire.command(
        0,
        &[s("_result"), n(9.0), s("a string longer than eight bytes")],
    );
    let mut conn = Connection::new(Scripted::new(wire.take()));
    let m = conn.recv("test").unwrap();
    let values = command_values(&m).unwrap().unwrap();
    assert_eq!(values[2], s("a string longer than eight bytes"));

    // Abort drops a half-received message on chunk stream 3.
    let mut wire = Wire::new();
    let mut partial = Vec::new();
    wire.enc
        .encode(
            CSID_COMMAND,
            0,
            msg_type::COMMAND_AMF0,
            0,
            &[0x05; 300],
            &mut partial,
        )
        .unwrap();
    let mut input = partial[..12 + 128].to_vec();
    wire.control(msg_type::ABORT, &CSID_COMMAND.to_be_bytes());
    wire.command(0, &[s("after")]);
    input.extend_from_slice(&wire.take());
    let mut conn = Connection::new(Scripted::new(input));
    let m = conn.recv("test").unwrap();
    assert_eq!(command_values(&m).unwrap().unwrap(), [s("after")]);
}

#[test]
fn rejects_empty_media_and_bad_options() {
    let mut p = RtmpPublisher::publish_seeded(
        Scripted::new(accepting_script(|_| {})),
        &url(),
        &key(),
        &PublishOptions::default(),
        1,
    )
    .unwrap();
    assert!(matches!(
        p.send_video(0, &[]),
        Err(RtmpError::PayloadSize { len: 0 })
    ));
    assert!(matches!(
        p.send_audio(0, &[]),
        Err(RtmpError::PayloadSize { len: 0 })
    ));
    assert!(matches!(
        p.send_metadata(&[]),
        Err(RtmpError::PayloadSize { len: 0 })
    ));

    let options = PublishOptions {
        chunk_size: 0,
        ..PublishOptions::default()
    };
    assert!(matches!(
        RtmpPublisher::publish_with(Scripted::new(Vec::new()), &url(), &key(), &options),
        Err(RtmpError::InvalidChunkSize { size: 0 })
    ));
}

#[test]
fn create_stream_without_id_is_unexpected() {
    let mut bytes = vec![RTMP_VERSION];
    bytes.extend_from_slice(&[0; 2 * HANDSHAKE_SIZE]);
    let mut wire = Wire::new();
    wire.command(0, &[s("_result"), n(1.0), Amf0Value::Null, Amf0Value::Null]);
    wire.command(0, &[s("_result"), n(4.0), Amf0Value::Null, n(0.5)]);
    bytes.extend_from_slice(&wire.take());
    assert!(matches!(
        RtmpPublisher::publish_seeded(
            Scripted::new(bytes),
            &url(),
            &key(),
            &PublishOptions::default(),
            1
        ),
        Err(RtmpError::UnexpectedReply {
            command: "createStream",
            ..
        })
    ));
}

#[test]
fn server_hangup_is_connection_closed() {
    let mut bytes = vec![RTMP_VERSION];
    bytes.extend_from_slice(&[0; 2 * HANDSHAKE_SIZE]);
    assert!(matches!(
        RtmpPublisher::publish_seeded(
            Scripted::new(bytes),
            &url(),
            &key(),
            &PublishOptions::default(),
            1
        ),
        Err(RtmpError::ConnectionClosed { during: "connect" })
    ));
}

#[test]
fn rtmps_is_refused_until_tls_lands() {
    let url = RtmpUrl::parse("rtmps://a.rtmps.youtube.com/live2").unwrap();
    assert!(matches!(
        connect_tcp(&url, None),
        Err(RtmpError::TlsUnsupported)
    ));
}

#[test]
fn media_time_to_rtmp_milliseconds() {
    let t = |ticks, tb| timestamp_ms(MediaTime::new(ticks, tb));
    assert_eq!(t(3003, Timebase::MPEG_90K).unwrap(), 33); // 33.37 ms
    assert_eq!(t(1_500_000, Timebase::NANOS).unwrap(), 2); // 1.5 ms rounds up
    assert_eq!(t(1 << 32, Timebase::MILLIS).unwrap(), 0, "wraps at 2^32");
    assert!(matches!(
        t(-1, Timebase::MILLIS),
        Err(RtmpError::NegativeTimestamp { ms: -1 })
    ));
}
