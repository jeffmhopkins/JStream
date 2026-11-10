//! A VISCA head emulator on loopback, and the PTZ threads driven against it.

use super::*;
use jstream_visca::{ErrorKind, Request};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};

/// What the emulated head did and where it is.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeadState {
    /// Every command it took, in order.
    pub(crate) commands: Vec<Command>,
    pub(crate) pan: i32,
    pub(crate) tilt: i32,
    pub(crate) zoom: u16,
    pub(crate) focus: u16,
    presets: BTreeMap<u8, (i32, i32, u16)>,
    /// How many inquiries it answered.
    pub(crate) inquiries: u64,
}

impl HeadState {
    /// Takes `command` as a head does.
    fn take(&mut self, command: Command) {
        match command {
            Command::PanTiltAbsolute { pan, tilt, .. } => (self.pan, self.tilt) = (pan, tilt),
            Command::PanTiltRelative { pan, tilt, .. } => {
                self.pan += pan;
                self.tilt += tilt;
            }
            Command::Home | Command::PanTiltReset => (self.pan, self.tilt) = (0, 0),
            Command::ZoomDirect(z) => self.zoom = z,
            Command::FocusDirect(f) => self.focus = f,
            Command::Preset(PresetOp::Set, p) => {
                self.presets.insert(p, (self.pan, self.tilt, self.zoom));
            }
            Command::Preset(PresetOp::Recall, p) => {
                if let Some(&(pan, tilt, zoom)) = self.presets.get(&p) {
                    (self.pan, self.tilt, self.zoom) = (pan, tilt, zoom);
                }
            }
            _ => {}
        }
        self.commands.push(command);
    }

    /// The replies a head sends to `message`.
    fn answer(&mut self, message: &[u8]) -> Vec<Vec<u8>> {
        let cam = Address::new(1).unwrap_or(Address::BROADCAST);
        let encode = |reply: Reply<'_>| reply.encode(cam).map(|b| b.as_slice().to_vec()).ok();
        match Request::parse(message) {
            Ok((_, Request::Command(command))) => {
                self.take(command);
                [
                    encode(Reply::Ack { socket: 1 }),
                    encode(Reply::Completion { socket: 1 }),
                ]
                .into_iter()
                .flatten()
                .collect()
            }
            Ok((_, Request::Inquiry(inquiry))) => {
                self.inquiries += 1;
                let value = match inquiry {
                    Inquiry::PanTiltPosition => InquiryValue::PanTiltPosition {
                        pan: self.pan,
                        tilt: self.tilt,
                        width: PositionWidth::Pan5Tilt4,
                    },
                    Inquiry::ZoomPosition => InquiryValue::ZoomPosition(self.zoom),
                    Inquiry::FocusPosition => InquiryValue::FocusPosition(self.focus),
                    Inquiry::FocusMode => InquiryValue::FocusMode(FocusMode::Manual),
                    Inquiry::Power => InquiryValue::Power(true),
                };
                value
                    .encode(cam)
                    .map(|b| vec![b.as_slice().to_vec()])
                    .unwrap_or_default()
            }
            Err(jstream_visca::Error::Unsupported) => encode(Reply::Error {
                socket: 0,
                kind: ErrorKind::NotExecutable,
            })
            .into_iter()
            .collect(),
            Err(_) => encode(Reply::Error {
                socket: 0,
                kind: ErrorKind::Syntax,
            })
            .into_iter()
            .collect(),
        }
    }
}

/// An emulated VISCA head on loopback, until dropped.
pub(crate) struct FakeHead {
    addr: SocketAddr,
    transport: PtzTransport,
    state: Arc<Mutex<HeadState>>,
    stop: Arc<AtomicBool>,
}

impl FakeHead {
    /// A head speaking `transport` on a free loopback port.
    pub(crate) fn start(transport: PtzTransport) -> Self {
        let state = Arc::new(Mutex::new(HeadState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (Arc::clone(&state), Arc::clone(&stop));
        let addr = match transport {
            PtzTransport::Tcp => {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
                let addr = listener.local_addr().expect("addr");
                listener.set_nonblocking(true).expect("nonblocking");
                thread::spawn(move || serve_tcp(&listener, &s, &st));
                addr
            }
            _ => {
                let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
                let addr = socket.local_addr().expect("addr");
                socket
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .expect("timeout");
                let ip = transport == PtzTransport::ViscaIp;
                thread::spawn(move || serve_udp(&socket, ip, &s, &st));
                addr
            }
        };
        Self {
            addr,
            transport,
            state,
            stop,
        }
    }

    /// A camera on this machine's list for it.
    pub(crate) fn camera(&self, name: &str) -> PtzCamera {
        PtzCamera::new(name, self.addr, self.transport, 1).expect("camera")
    }

    /// What it did so far.
    pub(crate) fn state(&self) -> HeadState {
        lock(&self.state).clone()
    }

    /// Waits up to ten seconds for `done` to hold: long, because it waits on the head and
    /// only a failure waits it out, and a loaded CI runner can be slow (#1494).
    pub(crate) fn wait(&self, done: impl Fn(&HeadState) -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if done(&lock(&self.state)) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

impl Drop for FakeHead {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve_udp(socket: &UdpSocket, ip: bool, state: &Mutex<HeadState>, stop: &AtomicBool) {
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let datagram = &buf[..n];
        if !ip {
            for reply in lock(state).answer(datagram) {
                let _ = socket.send_to(&reply, from);
            }
            continue;
        }
        let Ok(parsed) = ip::parse(datagram) else {
            continue;
        };
        let replies = match parsed.kind {
            ip::PayloadType::Control => {
                let mut out = [0u8; 16];
                if let Ok(n) =
                    ip::write(&mut out, ip::PayloadType::ControlReply, parsed.seq, &[0x01])
                {
                    let _ = socket.send_to(&out[..n], from);
                }
                continue;
            }
            _ => lock(state).answer(parsed.payload),
        };
        for reply in replies {
            let mut out = [0u8; 32];
            if let Ok(n) = ip::write(&mut out, ip::PayloadType::Reply, parsed.seq, &reply) {
                let _ = socket.send_to(&out[..n], from);
            }
        }
    }
}

fn serve_tcp(listener: &TcpListener, state: &Mutex<HeadState>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let Ok((mut stream, _)) = listener.accept() else {
            thread::sleep(Duration::from_millis(10));
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
        let mut buf = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            let mut chunk = [0u8; 64];
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
            while let Some(message) = ip::next_message(&buf).map(<[u8]>::to_vec) {
                buf.drain(..message.len());
                for reply in lock(state).answer(&message) {
                    let _ = stream.write_all(&reply);
                }
            }
        }
    }
}

fn setup(cameras: Vec<PtzCamera>) -> PtzSetup {
    PtzSetup::new(cameras).expect("setup")
}

fn client() -> ClientId {
    ClientId::new("client")
}

fn drive(camera: &str, pan: i8, tilt: i8, zoom: i8) -> Action {
    Action::PtzDrive {
        camera: camera.into(),
        pan,
        tilt,
        zoom,
    }
}

/// Every transport carries commands and inquiries: an aim lands on the head, and the
/// pose the head reports reaches `ptz.list`.
#[test]
fn each_transport_aims_the_head_and_reads_its_pose() {
    for transport in [PtzTransport::ViscaIp, PtzTransport::Udp, PtzTransport::Tcp] {
        let head = FakeHead::start(transport);
        let mut ptz = Ptz::start(&setup(vec![head.camera("cam")])).expect("start");
        let aim = Action::PtzAim {
            camera: "cam".into(),
            pose: PtzPose {
                pan: Some(-1200),
                tilt: Some(300),
                zoom: Some(0x2000),
                focus: Some(0x0400),
            },
            speed: Some(10),
        };
        assert!(matches!(ptz.act(&client(), &aim), Some(Ok(()))));
        assert!(
            head.wait(|h| (h.pan, h.tilt, h.zoom, h.focus) == (-1200, 300, 0x2000, 0x0400)),
            "{transport:?}: {:?}",
            head.state()
        );
        let commands = head.state().commands;
        assert!(
            commands.contains(&Command::PanTiltAbsolute {
                pan_speed: 10,
                tilt_speed: 10,
                pan: -1200,
                tilt: 300,
                width: PositionWidth::Pan5Tilt4,
            }),
            "{commands:?}"
        );
        assert!(commands.contains(&Command::FocusMode(FocusMode::Manual)));
        // The pose the head reports comes back through `ptz.list`, once a poll has read
        // all of it (a busy machine can make one exchange time out first).
        let want = PtzPose {
            pan: Some(-1200),
            tilt: Some(300),
            zoom: Some(0x2000),
            focus: Some(0x0400),
        };
        let until = Instant::now() + Duration::from_secs(5);
        let listed = loop {
            let listed = ptz.list();
            let info = &listed[0];
            if (info.pose == Some(want) && info.error.is_none() && info.reachable)
                || Instant::now() > until
            {
                break listed;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let info = &listed[0];
        assert!(info.reachable, "{transport:?}: {info:?}");
        assert_eq!(info.transport, transport);
        assert_eq!(
            info.pose,
            Some(PtzPose {
                pan: Some(-1200),
                tilt: Some(300),
                zoom: Some(0x2000),
                focus: Some(0x0400),
            }),
            "{transport:?}"
        );
        assert_eq!(info.error, None);
        assert!(info.sent > 0 && info.replies > 0);
    }
}

/// Drives, presets, home and focus become the VISCA commands ADR-0146 §3 names.
#[test]
fn actions_become_visca_commands() {
    let head = FakeHead::start(PtzTransport::ViscaIp);
    let mut ptz = Ptz::start(&setup(vec![head.camera("cam")])).expect("start");
    // Each waits for the one before, so a stop doesn't overtake what it follows.
    for (action, sent) in [
        (drive("cam", -24, 5, 8), 2),
        (
            Action::PtzStore {
                camera: "cam".into(),
                preset: 3,
            },
            3,
        ),
        (drive("cam", 0, 0, 0), 5),
        (
            Action::PtzHome {
                camera: "cam".into(),
            },
            6,
        ),
        (
            Action::PtzRecall {
                camera: "cam".into(),
                preset: 3,
            },
            7,
        ),
        (
            Action::PtzFocus {
                camera: "cam".into(),
                focus: PtzFocus::OnePush,
                speed: None,
            },
            8,
        ),
        (
            Action::PtzFocus {
                camera: "cam".into(),
                focus: PtzFocus::Near,
                speed: Some(2),
            },
            9,
        ),
    ] {
        assert!(
            matches!(ptz.act(&client(), &action), Some(Ok(()))),
            "{action:?}"
        );
        assert!(
            head.wait(|h| h.commands.len() >= sent),
            "{:?}",
            head.state()
        );
    }
    assert!(head.wait(|h| h.commands.len() >= 9), "{:?}", head.state());
    assert_eq!(
        head.state().commands,
        [
            Command::PanTiltDrive {
                pan_speed: 24,
                tilt_speed: 5,
                pan: Pan::Left,
                tilt: Tilt::Up,
            },
            Command::Zoom(Drive::Plus(Some(7))),
            Command::Preset(PresetOp::Set, 3),
            Command::PanTiltDrive {
                pan_speed: 1,
                tilt_speed: 1,
                pan: Pan::Stop,
                tilt: Tilt::Stop,
            },
            Command::Zoom(Drive::Stop),
            Command::Home,
            Command::Preset(PresetOp::Recall, 3),
            Command::FocusOnePush,
            Command::Focus(Drive::Minus(Some(1))),
        ]
    );
}

/// The queue never builds a backlog (ADR-0146 §3): a drive or an aim replaces an unsent
/// one in its place, a stop drops the unsent drives and goes first, and past
/// `MAX_QUEUE` the oldest unsent command goes.
#[test]
fn the_queue_merges_drives_and_aims_and_a_stop_goes_first() {
    let drive = |pan| Cmd::Drive {
        pan,
        tilt: 0,
        zoom: 0,
    };
    let aim = |pan| Cmd::Aim {
        pose: PtzPose {
            pan: Some(pan),
            tilt: Some(0),
            ..PtzPose::default()
        },
        speed: 24,
    };
    let mut queue = VecDeque::new();
    for cmd in [
        Cmd::Home,
        drive(1),
        aim(100),
        drive(5),
        Cmd::Recall(2),
        aim(200),
        drive(9),
    ] {
        enqueue(&mut queue, cmd);
    }
    assert_eq!(
        Vec::from(queue.clone()),
        [Cmd::Home, drive(9), aim(200), Cmd::Recall(2)]
    );
    enqueue(&mut queue, STOP);
    assert_eq!(
        Vec::from(queue.clone()),
        [STOP, Cmd::Home, aim(200), Cmd::Recall(2)]
    );
    for preset in 0..40 {
        enqueue(&mut queue, Cmd::Recall(preset));
    }
    assert_eq!(queue.len(), MAX_QUEUE);
    assert_eq!(queue.back(), Some(&Cmd::Recall(39)));
}

/// A head stops when its setup is dropped mid-drive, and when the session of the client
/// whose drive set it moving ends (ADR-0146 §3).
#[test]
fn a_drive_stops_when_its_client_leaves_or_the_head_is_dropped() {
    // A command sent again because its reply was late reaches the head twice in a row:
    // counted once.
    let stops = |h: &HeadState| {
        let mut sent = h.commands.clone();
        sent.dedup();
        sent.iter()
            .filter(|c| {
                matches!(
                    c,
                    Command::PanTiltDrive {
                        pan: Pan::Stop,
                        tilt: Tilt::Stop,
                        ..
                    }
                )
            })
            .count()
    };
    let head = FakeHead::start(PtzTransport::ViscaIp);
    let mut ptz = Ptz::start(&setup(vec![head.camera("cam")])).expect("start");
    let other = ClientId::new("other");
    assert!(matches!(
        ptz.act(&client(), &drive("cam", 10, 0, 0)),
        Some(Ok(()))
    ));
    assert!(head.wait(|h| h.commands.len() >= 2), "{:?}", head.state());
    // Another client's session ending leaves the drive alone.
    ptz.forget_client(&other);
    ptz.forget_client(&client());
    assert!(head.wait(|h| stops(h) == 1), "{:?}", head.state());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stops(&head.state()), 1);
    // A drive at 0 forgets its driver, so the session ending sends nothing more. The
    // zoom drive lands before the stop is sent: two drives queued together are merged
    // into the later one, and the head then sees one stop fewer (#1494).
    assert!(matches!(
        ptz.act(&client(), &drive("cam", 0, 0, 3)),
        Some(Ok(()))
    ));
    assert!(
        head.wait(|h| h.commands.contains(&Command::Zoom(Drive::Plus(Some(2))))),
        "{:?}",
        head.state()
    );
    assert!(matches!(
        ptz.act(&client(), &drive("cam", 0, 0, 0)),
        Some(Ok(()))
    ));
    assert!(head.wait(|h| stops(h) == 3), "{:?}", head.state());
    ptz.forget_client(&client());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stops(&head.state()), 3, "the session ending sent nothing");
    // Dropping the setup mid-drive stops the head, and its focus.
    assert!(matches!(
        ptz.act(&other, &drive("cam", -3, 2, 0)),
        Some(Ok(()))
    ));
    assert!(head.wait(|h| {
        h.commands
            .iter()
            .any(|c| matches!(c, Command::PanTiltDrive { pan: Pan::Left, .. }))
    }));
    drop(ptz);
    assert!(head.wait(|h| stops(h) == 4), "{:?}", head.state());
    assert!(
        head.wait(|h| h.commands.last() == Some(&Command::Focus(Drive::Stop))),
        "{:?}",
        head.state()
    );
}

/// An action is checked, then refused for a machine with no cameras or a camera it
/// doesn't list.
#[test]
fn actions_are_checked_and_refused_for_unknown_cameras() {
    let mut none = Ptz::default();
    assert!(
        none.act(
            &client(),
            &Action::SceneSwitch {
                scene_id: SceneId::new("a"),
                preset_id: None,
            }
        )
        .is_none()
    );
    let code = |r: Option<Result<(), ProtocolError>>| r.and_then(Result::err).map(|e| e.code());
    assert_eq!(
        code(none.act(&client(), &drive("cam", 1, 0, 0))),
        Some("action_failed")
    );
    assert_eq!(
        code(none.act(&client(), &drive("cam", 25, 0, 0))),
        Some("invalid_argument")
    );
    let head = FakeHead::start(PtzTransport::Udp);
    let mut ptz = Ptz::start(&setup(vec![head.camera("cam")])).expect("start");
    assert_eq!(
        code(ptz.act(&client(), &drive("other", 1, 0, 0))),
        Some("invalid_argument")
    );
    assert_eq!(
        code(ptz.act(
            &client(),
            &Action::PtzRecall {
                camera: "cam".into(),
                preset: 200,
            }
        )),
        Some("invalid_argument")
    );
    assert_eq!(
        code(ptz.act(
            &client(),
            &Action::PtzAim {
                camera: "cam".into(),
                pose: PtzPose::default(),
                speed: None,
            }
        )),
        Some("invalid_argument")
    );
}

/// A reply from any address but the camera's is dropped and counted (ADR-0123 §4), and
/// a head that never answers shows as unreachable with an error.
#[test]
fn foreign_replies_are_dropped_and_silence_is_reported() {
    let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let camera = PtzCamera::new(
        "mute",
        silent.local_addr().expect("addr"),
        PtzTransport::Udp,
        1,
    )
    .expect("camera");
    let mut ptz = Ptz::start(&setup(vec![camera])).expect("start");
    assert!(matches!(
        ptz.act(
            &client(),
            &Action::PtzHome {
                camera: "mute".into()
            }
        ),
        Some(Ok(()))
    ));
    // Find the engine's socket from what it sent, then answer from another one.
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut buf = [0u8; 64];
    let (_, engine) = silent.recv_from(&mut buf).expect("the home command");
    let spoof = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    spoof.send_to(&[0x90, 0x41, 0xFF], engine).expect("send");
    let until = Instant::now() + Duration::from_secs(3);
    let info = loop {
        let info = ptz.list().remove(0);
        if (info.dropped > 0 && info.error.is_some()) || Instant::now() > until {
            break info;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(info.dropped, 1, "{info:?}");
    assert_eq!(info.replies, 0);
    assert!(!info.reachable);
    assert_eq!(info.pose, None);
    assert_eq!(info.error.as_deref(), Some("no reply"));
}

/// The daemon's syntax, and the checks on a camera and a list.
#[test]
fn cameras_parse_and_are_checked() {
    let cam: PtzCamera = "wide@192.168.1.40:52381".parse().expect("parse");
    assert_eq!(cam.transport(), PtzTransport::ViscaIp);
    assert_eq!(cam.visca_address(), 1);
    let cam: PtzCamera = "bridge@10.0.0.9:5678/tcp#2".parse().expect("parse");
    assert_eq!(
        (cam.transport(), cam.visca_address()),
        (PtzTransport::Tcp, 2)
    );
    assert_eq!(cam.to_string(), "bridge@10.0.0.9:5678/tcp#2");
    let cam: PtzCamera = "v6@[::1]:1259/udp".parse().expect("parse");
    assert!(cam.addr().is_ipv6());
    for bad in [
        // ADR-0123 §2's ranges: IPv6 beyond loopback isn't on them.
        "v6@[fe80::1]:1259/udp",
        "v6@[fd00::1]:1259/udp",
        "wide@8.8.8.8:52381",
        "wide@192.168.1.40:0",
        "@192.168.1.40:52381",
        "wide@192.168.1.40:52381/serial",
        "wide@192.168.1.40:52381#8",
        "wide@192.168.1.40",
        "wide",
    ] {
        assert!(bad.parse::<PtzCamera>().is_err(), "{bad}");
    }
    let a: PtzCamera = "a@192.168.1.40:52381".parse().expect("parse");
    let b: PtzCamera = "b@192.168.1.40:52381".parse().expect("parse");
    let b2: PtzCamera = "b@192.168.1.40:52381#2".parse().expect("parse");
    assert!(PtzSetup::new(vec![a.clone(), a.clone()]).is_err());
    assert!(PtzSetup::new(vec![a.clone(), b]).is_err());
    assert!(PtzSetup::new(vec![a, b2]).is_ok());
}

/// The fastest step up to half a second, then slower, never below 1 (ADR-0146 §7).
#[test]
fn pose_shots_slow_down_with_the_transition() {
    assert_eq!(matched_speed(0), 24);
    assert_eq!(matched_speed(500), 24);
    assert_eq!(matched_speed(1000), 12);
    assert_eq!(matched_speed(3000), 4);
    assert_eq!(matched_speed(60_000), 1);
}
