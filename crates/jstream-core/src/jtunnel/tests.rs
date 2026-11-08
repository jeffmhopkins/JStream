use super::*;
use jstream_jtunnel::wire::{AudioHeader, FrameOut};
use std::net::Ipv6Addr;
use std::sync::mpsc::{Receiver as Rx, Sender as Tx, channel};

/// The media clock: time since the test started.
struct Clock(Instant);

impl MediaClock for Clock {
    fn timebase(&self) -> Timebase {
        Timebase::NANOS
    }

    fn now(&self) -> MediaTime {
        MediaTime::new(
            i64::try_from(self.0.elapsed().as_nanos()).unwrap_or(i64::MAX),
            Timebase::NANOS,
        )
    }
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[test]
fn only_loopback_private_and_link_local_addresses_are_lan() {
    for ip in [
        v4(127, 0, 0, 1),
        v4(10, 1, 2, 3),
        v4(172, 16, 0, 1),
        v4(172, 31, 255, 255),
        v4(192, 168, 1, 20),
        v4(169, 254, 7, 7),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6(Ipv4Addr::new(192, 168, 0, 9).to_ipv6_mapped()),
    ] {
        assert!(is_lan(ip), "{ip}");
    }
    for ip in [
        v4(8, 8, 8, 8),
        v4(172, 32, 0, 1),
        v4(100, 64, 0, 1),
        v4(224, 0, 0, 251),
        v4(0, 0, 0, 0),
        "fe80::1".parse().expect("an address"),
        "2001:db8::1".parse().expect("an address"),
        IpAddr::V6(Ipv4Addr::new(8, 8, 4, 4).to_ipv6_mapped()),
    ] {
        assert!(!is_lan(ip), "{ip}");
    }
}

#[test]
fn an_accepted_sender_is_a_named_lan_address() {
    let sender: AcceptedSender = "Bitwig 2@192.168.1.5:9000".parse().expect("accepted");
    assert_eq!(sender.name(), "Bitwig 2");
    assert_eq!(
        sender.addr(),
        "192.168.1.5:9000".parse().expect("an address")
    );
    // The last `@` splits, so a name may hold one.
    let at: AcceptedSender = "a@b@127.0.0.1:1".parse().expect("accepted");
    assert_eq!(at.name(), "a@b");
    for bad in [
        "Bitwig@8.8.8.8:9000",
        "Bitwig@192.168.1.5:0",
        "@192.168.1.5:9000",
        "Bitwig 192.168.1.5:9000",
        "Bitwig@192.168.1.5",
        "a\nb@192.168.1.5:9000",
        // White space at either end: a source's `sender` is trimmed (review N6).
        " Bitwig@192.168.1.5:9000",
        "Bitwig @192.168.1.5:9000",
        // The socket is IPv4 (review N1).
        "Bitwig@[::1]:9000",
        "Bitwig@[fe80::1]:9000",
        &format!("{}@192.168.1.5:9000", "x".repeat(32)),
    ] {
        assert!(bad.parse::<AcceptedSender>().is_err(), "{bad:?}");
    }
    // An IPv4 address mapped into IPv6 is taken as itself.
    let mapped: AcceptedSender = "Bitwig@[::ffff:192.168.1.5]:9000"
        .parse()
        .expect("accepted");
    assert_eq!(
        mapped.addr(),
        "192.168.1.5:9000".parse().expect("an address")
    );
}

#[test]
fn a_setup_names_each_sender_once_and_at_most_the_limit() {
    let one = |name: &str, port: u16| {
        AcceptedSender::new(name, SocketAddr::new(v4(127, 0, 0, 1), port)).expect("a sender")
    };
    assert!(JtunnelSetup::new(vec![one("a", 1), one("b", 2)]).is_ok());
    assert!(JtunnelSetup::new(vec![one("a", 1), one("a", 2)]).is_err());
    // Two names at one address and port: datagrams couldn't tell them apart (review N2).
    assert!(JtunnelSetup::new(vec![one("a", 1), one("b", 1)]).is_err());
    let many = (0..=MAX_SENDERS)
        .map(|n| one(&format!("s{n}"), u16::try_from(n + 1).expect("a port")))
        .collect();
    assert!(JtunnelSetup::new(many).is_err());
}

/// What an input handler was given.
enum Got {
    Packet {
        samples: Vec<f32>,
        position: u64,
        time: Option<MediaTime>,
        silent: bool,
    },
    Error,
}

struct Collect(Tx<Got>);

impl AudioInputHandler for Collect {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        let _ = self.0.send(Got::Packet {
            samples: packet.samples.to_vec(),
            position: packet.device_position,
            time: packet.time,
            silent: packet.flags.silent,
        });
    }

    fn on_error(&mut self, _: &PlatformError) {
        let _ = self.0.send(Got::Error);
    }
}

/// 24-bit PCM for `frames` stereo frames, every sample at `value` (of 2^23 - 1).
fn pcm(frames: usize, value: i32) -> Vec<u8> {
    let bytes = value.to_le_bytes();
    (0..frames * 2)
        .flat_map(|_| [bytes[0], bytes[1], bytes[2]])
        .collect()
}

/// A fake JTunnel sender on loopback.
struct FakeSender {
    socket: UdpSocket,
}

impl FakeSender {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("a socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a timeout");
        Self { socket }
    }

    fn addr(&self) -> SocketAddr {
        self.socket.local_addr().expect("an address")
    }

    /// The next packet a receiver sends, and where from.
    fn next(&self) -> (Vec<u8>, SocketAddr) {
        let mut buf = [0; 128];
        let (n, from) = self.socket.recv_from(&mut buf).expect("a packet");
        (buf[..n].to_vec(), from)
    }

    /// Sends frame `seq` of a 48 kHz PCM stream, 96 samples at `value`, to `to`.
    fn send(&self, to: SocketAddr, rate: u32, seq: u32, value: i32) {
        let audio = pcm(96, value);
        let header = AudioHeader {
            stream_id: 7,
            seq,
            sample_rate: rate,
            codec: Codec::Pcm24,
            frame_samples: 96,
        };
        let frames = [FrameOut {
            events: &[],
            audio: &audio,
        }];
        let mut out = [0; wire::MAX_PACKET_BYTES];
        let n = wire::write_audio(&mut out, &header, &frames).expect("a packet");
        self.socket.send_to(&out[..n], to).expect("sent");
    }
}

fn setup(name: &str, sender: &FakeSender) -> JtunnelSetup {
    let mut setup = JtunnelSetup::new(vec![
        AcceptedSender::new(name, sender.addr()).expect("accepted"),
    ])
    .expect("a setup");
    setup.bind = "127.0.0.1:0".parse().expect("an address");
    setup
}

fn start(setup: &JtunnelSetup) -> Receiver {
    Receiver::start(setup, Arc::new(Clock(Instant::now())), None).expect("a receiver")
}

/// Waits up to 5 s for `check`.
fn eventually(mut check: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if check() {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    false
}

fn packets(rx: &Rx<Got>, want: usize) -> Vec<(Vec<f32>, u64, Option<MediaTime>, bool)> {
    let mut got = Vec::new();
    while got.len() < want {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Got::Packet {
                samples,
                position,
                time,
                silent,
            }) => got.push((samples, position, time, silent)),
            Ok(Got::Error) => panic!("the input failed"),
            Err(err) => panic!("only {} packets: {err}", got.len()),
        }
    }
    got
}

#[test]
fn a_sender_is_subscribed_to_by_name_and_its_frames_play_in_order() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    // The receiver subscribes by name, and keeps doing so.
    let (subscribe, to) = sender.next();
    assert_eq!(
        wire::parse(&subscribe),
        Ok(Packet::Subscribe { name: b"Studio" })
    );
    assert_eq!(to, receiver.local_addr());
    let (again, _) = sender.next();
    assert_eq!(again, subscribe, "a keepalive every half second");

    // Not heard from yet: the source is unavailable.
    assert!(receiver.input_format("Studio").is_err());
    assert!(receiver.input_format("Other").is_err(), "not accepted");
    sender.send(to, 48_000, 0, 1 << 22);
    assert!(eventually(|| receiver.input_format("Studio").is_ok()));
    let format = receiver.input_format("Studio").expect("a format");
    assert_eq!(format.sample_rate(), 48_000);
    assert_eq!(format.layout(), ChannelLayout::Stereo);

    let (tx, rx) = channel();
    let stream = receiver
        .open("Studio", 10, Box::new(Collect(tx)))
        .expect("an input");
    assert_eq!(stream.format(), format);
    for seq in 1..=20 {
        sender.send(to, 48_000, seq, 1 << 22);
        thread::sleep(Duration::from_millis(2));
    }
    let got = packets(&rx, 10);
    // Each frame's position follows the last, and its time never goes back.
    for pair in got.windows(2) {
        assert_eq!(pair[1].1, pair[0].1 + 96);
        assert!(pair[1].2 >= pair[0].2);
    }
    let heard = got.iter().find(|p| !p.3).expect("a frame that arrived");
    assert_eq!(heard.0.len(), 192);
    assert!(
        heard.0.iter().all(|&s| (s - 0.5).abs() < 1e-3),
        "half scale"
    );
    assert!(receiver.stats().played >= 10);

    // Dropping the stream stops the input; dropping the receiver says bye.
    drop(stream);
    drop(receiver);
    let mut bye = false;
    for _ in 0..8 {
        if wire::parse(&sender.next().0) == Ok(Packet::Bye) {
            bye = true;
            break;
        }
    }
    assert!(bye, "a bye on stopping");
}

#[test]
fn datagrams_from_other_addresses_or_not_jtunnel_are_dropped_and_counted() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    let (_, to) = sender.next();
    // Another LAN address the receiver never subscribed to.
    let stranger = FakeSender::new();
    stranger.send(to, 48_000, 0, 1);
    // The accepted sender, but not a JTunnel packet, and a receiver's packet.
    sender.socket.send_to(b"hello", to).expect("sent");
    let mut out = [0; 64];
    let n = wire::write_subscribe(&mut out, b"x").expect("a packet");
    sender.socket.send_to(&out[..n], to).expect("sent");
    // Longer than any packet: counted, not parsed cut short (review N5).
    let long = vec![0_u8; wire::MAX_PACKET_BYTES + 1];
    sender.socket.send_to(&long, to).expect("sent");
    assert!(eventually(|| {
        let stats = receiver.stats();
        stats.unaccepted == 1 && stats.malformed == 3
    }));
    assert!(
        receiver.input_format("Studio").is_err(),
        "nothing accepted arrived"
    );
}

#[test]
fn a_rate_change_fails_the_input_so_it_reopens() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    let (_, to) = sender.next();
    sender.send(to, 48_000, 0, 0);
    assert!(eventually(|| receiver.input_format("Studio").is_ok()));
    let (tx, rx) = channel();
    let _stream = receiver
        .open("Studio", 10, Box::new(Collect(tx)))
        .expect("an input");
    sender.send(to, 44_100, 0, 0);
    let failed =
        (0..50).any(|_| matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(Got::Error)));
    assert!(failed, "the input reports the change");
    assert_eq!(
        receiver
            .input_format("Studio")
            .expect("a format")
            .sample_rate(),
        44_100
    );
}

/// Review N3: one source records a sender at a time, so a second is refused rather than
/// take it over (and be taken over back on its own reopen); and a source whose receiver
/// stops is told so, rather than keep recording silence.
#[test]
fn a_second_input_is_refused_and_a_stopped_one_told_why() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    let (_, to) = sender.next();
    sender.send(to, 48_000, 0, 0);
    assert!(eventually(|| receiver.input_format("Studio").is_ok()));
    let (first_tx, first) = channel();
    let opened = receiver
        .open("Studio", 10, Box::new(Collect(first_tx)))
        .expect("an input");
    let (second_tx, _second) = channel();
    let refused = receiver.open("Studio", 10, Box::new(Collect(second_tx)));
    assert!(
        refused.is_err_and(|err| err.contains("another source")),
        "a second source for the sender is unavailable"
    );
    // Once the first closes, the sender is free.
    drop(opened);
    let (third_tx, third) = channel();
    let _third = receiver
        .open("Studio", 10, Box::new(Collect(third_tx)))
        .expect("the sender is free again");
    let failed = |rx: &Rx<Got>| {
        (0..50).any(|_| matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(Got::Error)))
    };
    assert!(
        !matches!(first.try_recv(), Ok(Got::Error)),
        "a closed input isn't failed"
    );
    drop(receiver);
    assert!(failed(&third), "the stopped receiver's input reports it");
}

#[test]
fn opus_is_refused_and_counted() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    let (_, to) = sender.next();
    let header = AudioHeader {
        stream_id: 1,
        seq: 0,
        sample_rate: 48_000,
        codec: Codec::Opus,
        frame_samples: 240,
    };
    let frames = [FrameOut {
        events: &[],
        audio: &[0xFC, 0xFF, 0xFE],
    }];
    let mut out = [0; wire::MAX_PACKET_BYTES];
    let n = wire::write_audio(&mut out, &header, &frames).expect("a packet");
    sender.socket.send_to(&out[..n], to).expect("sent");
    assert!(eventually(|| receiver.stats().unsupported == 1));
    assert!(receiver.input_format("Studio").is_err());
}

#[test]
fn ipv4_addresses_match_mapped_or_not() {
    let plain: SocketAddr = "192.168.1.5:9000".parse().expect("an address");
    let mapped = SocketAddr::new(
        IpAddr::V6(Ipv4Addr::new(192, 168, 1, 5).to_ipv6_mapped()),
        9000,
    );
    assert!(same(plain, mapped));
    assert!(same(mapped, plain));
    assert!(!same(
        plain,
        "192.168.1.5:9001".parse().expect("an address")
    ));
}

/// ADR-0142 §2: a sender's MIDI reaches its own MIDI input and the every-input one, from a
/// MIDI-only stream that no source records, stamped on the media clock.
#[test]
fn a_senders_midi_feeds_its_midi_input_without_a_source() {
    let sender = FakeSender::new();
    let midi = Arc::new(MidiInputs::new());
    let started = Instant::now();
    let receiver = Receiver::start(
        &setup("Keys", &sender),
        Arc::new(Clock(started)),
        Some(Arc::clone(&midi)),
    )
    .expect("a receiver");
    let (_, to) = sender.next();
    let note_on: &[u8] = &[0x90, 60, 100];
    let cc: &[u8] = &[0xB3, 7, 99];
    let start: &[u8] = &[0xFA];
    let clock: &[u8] = &[0xF8];
    let events = [(0_u16, note_on), (10, cc), (20, start), (30, clock)];
    let first_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    for seq in 0..40 {
        let header = AudioHeader {
            stream_id: 3,
            seq,
            sample_rate: 48_000,
            codec: Codec::None,
            frame_samples: 48,
        };
        let frames = [FrameOut {
            events: if seq == 0 { &events } else { &[] },
            audio: &[],
        }];
        let mut out = [0; wire::MAX_PACKET_BYTES];
        let n = wire::write_audio(&mut out, &header, &frames).expect("a packet");
        sender.socket.send_to(&out[..n], to).expect("sent");
        thread::sleep(Duration::from_millis(1));
    }
    let keys = midi.drawn("Keys");
    assert!(eventually(
        || keys.held(0, 60) == 100 && keys.controller(3, 7) == 99
    ));
    assert_eq!(midi.any().held(0, 60), 100);
    assert!(eventually(|| receiver.stats().midi == 4));
    // Its clock runs the input's tempo (ADR-0142 §3).
    let tempo = keys.tempo().expect("a tempo from the sender's clock");
    assert!(tempo.playing);
    assert_eq!(midi.any().tempo(), Some(tempo));
    assert_eq!(
        receiver.stats().played,
        0,
        "a MIDI-only stream feeds no input"
    );
    // Stamped on the media clock: the sender's frames are due at the receiver's clock
    // time, so every stamp lies between the first send and the clock's now.
    let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let last = keys.last_ms().expect("a stamp");
    assert!(
        (first_ms..=now_ms).contains(&last),
        "stamped {last} ms, sent from {first_ms} ms, clock at {now_ms} ms"
    );
}

/// #1223: `profile.get`'s `network` reads each sender's playout and the drops.
#[test]
fn the_profile_reads_each_senders_playout() {
    let sender = FakeSender::new();
    let receiver = start(&setup("Studio", &sender));
    let before = receiver.profile();
    assert_eq!(before.inputs.len(), 1);
    assert_eq!(before.inputs[0].sender, "Studio");
    assert_eq!(before.inputs[0].sample_rate, 0, "nothing heard yet");
    assert!(!before.inputs[0].recording);
    let (_, to) = sender.next();
    sender.send(to, 48_000, 0, 0);
    assert!(eventually(|| receiver.input_format("Studio").is_ok()));
    let (tx, rx) = channel();
    let _stream = receiver
        .open("Studio", 20, Box::new(Collect(tx)))
        .expect("an input");
    for seq in 1..=4 {
        sender.send(to, 48_000, seq, 1000);
    }
    packets(&rx, 4);
    FakeSender::new().send(to, 48_000, 0, 1);
    assert!(eventually(|| {
        let profile = receiver.profile();
        let input = &profile.inputs[0];
        profile.unaccepted == 1 && input.recording && input.played >= 4
    }));
    let input = &receiver.profile().inputs[0];
    assert_eq!(input.sample_rate, 48_000);
    assert_eq!(input.lead_ms.get(), 20.0);
}
