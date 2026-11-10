//! A VISCA controller emulator on loopback (ADR-0152): it sends what a joystick
//! keyboard or a PTZ control app sends, over each transport, and reads the replies a
//! camera would give, while the scene camera's live aim moves as the commands say.

use super::*;
use crate::Engine;
use crate::tests::{batch, block_on, connected, create, edits};
use jstream_protocol::{Batch, Canvas, Edit, SceneCamera};
use std::net::{Ipv4Addr, TcpStream};
use std::time::Instant;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// An engine with scene `a` (3D, on program), its keyframe scene `a2`, and scene `b`
/// (2D, on preview).
fn engine() -> Engine {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("a"),
                camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
            },
        ]),
    )
    .expect("scenes");
    batch(
        &control,
        Batch::single(Edit::SceneCreateVariant {
            scene_id: SceneId::new("a2"),
            variant_of: SceneId::new("a"),
            name: "A2".into(),
        }),
    )
    .expect("keyframe");
    block_on(control.call(Call::Action {
        action: Action::StudioSetPreview {
            scene_id: SceneId::new("b"),
        },
    }))
    .expect("preview");
    engine
}

fn endpoint(transport: PtzTransport, target: PtzControllerTarget) -> PtzController {
    PtzController::new((LOOPBACK, 0).into(), transport, 1, target).expect("endpoint")
}

fn start(engine: &Engine, controllers: Vec<PtzController>, allow: &[IpAddr]) -> PtzControllers {
    let setup = PtzControllerSetup::new(controllers, allow.to_vec()).expect("setup");
    engine.start_ptz_controllers(&setup).expect("started")
}

fn aim_of(engine: &Engine, scene: &str) -> CameraAimInfo {
    let control = connected(engine);
    match block_on(control.call(Call::Query {
        query: Query::SceneCameraAim {
            scene_id: SceneId::new(scene),
        },
    }))
    .expect("scene.camera_aim")
    .result
    {
        CallResult::Query {
            result: QueryResult::SceneCameraAim { aim },
        } => aim,
        other => panic!("{other:?}"),
    }
}

/// Waits up to two seconds for the aim of `scene` to settle where `want` says.
fn settled(engine: &Engine, scene: &str, want: impl Fn(&CameraAimInfo) -> bool) -> CameraAimInfo {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let aim = aim_of(engine, scene);
        if !aim.moving && want(&aim) {
            return aim;
        }
        assert!(Instant::now() < until, "never settled: {aim:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A controller on VISCA over IP: it numbers its messages and reads the replies to
/// each.
struct Controller {
    socket: UdpSocket,
    to: SocketAddr,
    seq: u32,
}

impl Controller {
    fn new(to: SocketAddr) -> Self {
        let socket = UdpSocket::bind((LOOPBACK, 0)).expect("bind");
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("timeout");
        Self { socket, to, seq: 0 }
    }

    /// Sends `payload` as `kind` and gives the payloads of the replies that come back
    /// with its sequence number.
    fn send(&mut self, kind: ip::PayloadType, payload: &[u8]) -> Vec<Vec<u8>> {
        self.seq += 1;
        let mut out = [0u8; 32];
        let len = ip::write(&mut out, kind, self.seq, payload).expect("datagram");
        self.socket.send_to(&out[..len], self.to).expect("send");
        let mut replies = Vec::new();
        let mut buf = [0u8; 64];
        while let Ok((n, _)) = self.socket.recv_from(&mut buf) {
            let d = ip::parse(&buf[..n]).expect("a VISCA-over-IP reply");
            assert_eq!(d.seq, self.seq, "a reply carries its request's number");
            replies.push(d.payload.to_vec());
            // A command's completion, an error or an answer ends the exchange.
            if !matches!(Reply::parse(d.payload), Ok((_, Reply::Ack { .. }))) {
                break;
            }
        }
        replies
    }

    fn command(&mut self, command: Command) -> Vec<Vec<u8>> {
        let bytes = command
            .encode(Address::new(1).expect("1"))
            .expect("encodes");
        self.send(ip::PayloadType::Command, bytes.as_slice())
    }

    fn inquire(&mut self, inquiry: Inquiry) -> InquiryValue {
        let bytes = inquiry.encode(Address::new(1).expect("1"));
        let replies = self.send(ip::PayloadType::Inquiry, bytes.as_slice());
        let [reply] = replies.as_slice() else {
            panic!("one answer: {replies:?}");
        };
        let Ok((_, Reply::Data(data))) = Reply::parse(reply) else {
            panic!("an answer: {reply:?}");
        };
        InquiryValue::decode(inquiry, data).expect("decodes")
    }
}

fn done() -> Vec<Vec<u8>> {
    vec![vec![0x90, 0x41, 0xFF], vec![0x90, 0x51, 0xFF]]
}

fn not_executable() -> Vec<Vec<u8>> {
    vec![vec![0x90, 0x41, 0xFF], vec![0x90, 0x61, 0x41, 0xFF]]
}

fn position(c: &mut Controller) -> (i32, i32) {
    match c.inquire(Inquiry::PanTiltPosition) {
        InquiryValue::PanTiltPosition { pan, tilt, .. } => (pan, tilt),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_controller_drives_aims_and_reads_a_scene_camera_over_visca_ip() {
    let engine = engine();
    let running = start(
        &engine,
        vec![endpoint(
            PtzTransport::ViscaIp,
            PtzControllerTarget::Scene(SceneId::new("a2")),
        )],
        &[LOOPBACK],
    );
    let mut c = Controller::new(running.local_addrs()[0]);

    // A controller resets the sequence first, then asks where the camera is: home.
    assert_eq!(
        c.send(ip::PayloadType::Control, &ip::CONTROL_RESET),
        vec![ip::CONTROL_RESET.to_vec()]
    );
    assert_eq!(c.inquire(Inquiry::Power), InquiryValue::Power(true));
    assert_eq!(position(&mut c), (0, 0));
    assert_eq!(
        c.inquire(Inquiry::ZoomPosition),
        InquiryValue::ZoomPosition(0)
    );

    // The joystick right and up drives the keyframe scene's family camera until the
    // stop, and the zoom rocker drives the zoom apart from it.
    let joystick = |pan, tilt| Command::PanTiltDrive {
        pan_speed: 0x18,
        tilt_speed: 0x10,
        pan,
        tilt,
    };
    assert_eq!(c.command(joystick(Pan::Right, Tilt::Up)), done());
    assert_eq!(c.command(Command::Zoom(Drive::Plus(Some(7)))), done());
    thread::sleep(Duration::from_millis(100));
    let moving = aim_of(&engine, "a");
    assert!(moving.moving && moving.pan_deg.get() > 0.0 && moving.tilt_deg.get() > 0.0);
    assert!(moving.zoom.get() > 1.0, "{moving:?}");
    assert_eq!(c.command(joystick(Pan::Stop, Tilt::Stop)), done());
    assert_eq!(c.command(Command::Zoom(Drive::Stop)), done());
    let held = settled(&engine, "a", |_| true);
    let (pan, tilt) = position(&mut c);
    assert_eq!(
        (pan, tilt),
        (
            (f64::from(held.pan_deg.get()) * 16.0).round() as i32,
            (f64::from(held.tilt_deg.get()) * 16.0).round() as i32
        ),
        "the position answers the aim, 16 counts a degree"
    );

    // An absolute move glides to 20° right, 10° down; the controller reads it back.
    assert_eq!(
        c.command(Command::PanTiltAbsolute {
            pan_speed: 0x18,
            tilt_speed: 0x17,
            pan: 320,
            tilt: -160,
            width: PositionWidth::Pan4Tilt4,
        }),
        done()
    );
    settled(&engine, "a", |a| {
        a.pan_deg.get() == 20.0 && a.tilt_deg.get() == -10.0
    });
    assert_eq!(position(&mut c), (320, -160));
    // A relative move adds to it.
    assert_eq!(
        c.command(Command::PanTiltRelative {
            pan_speed: 0x18,
            tilt_speed: 0x17,
            pan: -160,
            tilt: 160,
            width: PositionWidth::Pan5Tilt4,
        }),
        done()
    );
    settled(&engine, "a", |a| {
        a.pan_deg.get() == 10.0 && a.tilt_deg.get() == 0.0
    });
    assert_eq!(position(&mut c), (160, 0));

    // Zoom direct to the middle of its range is the square root of the most zoom.
    assert_eq!(c.command(Command::ZoomDirect(8192)), done());
    let zoomed = settled(&engine, "a", |a| a.zoom.get() > 2.8);
    assert!((zoomed.zoom.get() - 8f32.sqrt()).abs() < 1e-4, "{zoomed:?}");
    assert_eq!(
        c.inquire(Inquiry::ZoomPosition),
        InquiryValue::ZoomPosition(8192)
    );

    // A preset stores the aim in the endpoint; home sends pan and tilt back, keeping
    // the zoom, and recalling the preset returns.
    assert_eq!(c.command(Command::Preset(PresetOp::Set, 3)), done());
    assert_eq!(c.command(Command::Home), done());
    settled(&engine, "a", |a| {
        a.pan_deg.get() == 0.0 && a.zoom.get() > 2.8
    });
    assert_eq!(c.command(Command::Preset(PresetOp::Recall, 3)), done());
    settled(&engine, "a", |a| a.pan_deg.get() == 10.0);
    assert_eq!(
        c.command(Command::Preset(PresetOp::Recall, 4)),
        not_executable()
    );
    assert_eq!(c.command(Command::Preset(PresetOp::Reset, 3)), done());
    assert_eq!(
        c.command(Command::Preset(PresetOp::Recall, 3)),
        not_executable()
    );

    // What a scene camera can't do is taken and ignored; what isn't VISCA, or isn't
    // known, is answered as a camera would.
    assert_eq!(c.command(Command::FocusMode(FocusMode::Auto)), done());
    assert_eq!(c.command(Command::Power(true)), done());
    assert_eq!(
        c.send(
            ip::PayloadType::Command,
            &[0x81, 0x01, 0x04, 0x39, 0x00, 0xFF]
        ),
        vec![vec![0x90, 0x60, 0x41, 0xFF]],
        "exposure isn't a camera move: not executable"
    );
    assert_eq!(
        c.send(
            ip::PayloadType::Command,
            &[0x81, 0x01, 0x06, 0x01, 0x18, 0x17, 0x09, 0x03, 0xFF]
        ),
        vec![vec![0x90, 0x60, 0x02, 0xFF]],
        "a syntax error"
    );
    assert_eq!(
        c.send(ip::PayloadType::Command, &[0x82, 0x01, 0x06, 0x04, 0xFF]),
        Vec::<Vec<u8>>::new(),
        "a message to camera 2 isn't this one's"
    );
    assert_eq!(c.command(Command::IfClear), vec![vec![0x90, 0x50, 0xFF]]);
    assert!(running.answered() > 20);
    assert_eq!(running.dropped(), 0);
}

#[test]
fn only_allowed_controllers_are_answered() {
    let engine = engine();
    let running = start(
        &engine,
        vec![endpoint(
            PtzTransport::ViscaIp,
            PtzControllerTarget::Program,
        )],
        &["192.168.1.50".parse().expect("ip")],
    );
    let mut c = Controller::new(running.local_addrs()[0]);
    assert_eq!(
        c.command(Command::Home),
        Vec::<Vec<u8>>::new(),
        "loopback isn't on the allowlist"
    );
    // Not VISCA at all, from an allowed address, is dropped too.
    drop(running);
    let running = start(
        &engine,
        vec![endpoint(
            PtzTransport::ViscaIp,
            PtzControllerTarget::Program,
        )],
        &[LOOPBACK],
    );
    let socket = UdpSocket::bind((LOOPBACK, 0)).expect("bind");
    socket
        .send_to(b"GET / HTTP/1.1\r\n", running.local_addrs()[0])
        .expect("send");
    let until = Instant::now() + Duration::from_secs(2);
    while running.dropped() == 0 {
        assert!(Instant::now() < until, "never dropped");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(running.answered(), 0);
}

#[test]
fn program_and_preview_targets_follow_the_studio() {
    let engine = engine();
    let running = start(
        &engine,
        vec![
            endpoint(PtzTransport::ViscaIp, PtzControllerTarget::Program),
            endpoint(PtzTransport::ViscaIp, PtzControllerTarget::Preview),
        ],
        &[LOOPBACK],
    );
    let addrs = running.local_addrs();
    let mut program = Controller::new(addrs[0]);
    let mut preview = Controller::new(addrs[1]);
    let to = |pan| Command::PanTiltAbsolute {
        pan_speed: 0x18,
        tilt_speed: 0x17,
        pan,
        tilt: 0,
        width: PositionWidth::Pan5Tilt4,
    };
    // `a` is on program; `b`, on preview, draws through no camera.
    assert_eq!(program.command(to(160)), done());
    settled(&engine, "a", |a| a.pan_deg.get() == 10.0);
    assert_eq!(preview.command(to(160)), not_executable());

    // Switching moves the targets with the studio.
    let control = connected(&engine);
    for (action, _) in [
        (
            Action::SceneSwitch {
                scene_id: SceneId::new("b"),
                preset_id: None,
            },
            (),
        ),
        (
            Action::StudioSetPreview {
                scene_id: SceneId::new("a"),
            },
            (),
        ),
    ] {
        block_on(control.call(Call::Action { action })).expect("studio");
    }
    assert_eq!(program.command(to(-160)), not_executable());
    assert_eq!(preview.command(to(-160)), done());
    settled(&engine, "a", |a| a.pan_deg.get() == -10.0);
}

#[test]
fn raw_visca_over_udp_and_tcp_reach_the_same_camera() {
    let engine = engine();
    let running = start(
        &engine,
        vec![
            endpoint(
                PtzTransport::Udp,
                PtzControllerTarget::Scene(SceneId::new("a")),
            ),
            endpoint(
                PtzTransport::Tcp,
                PtzControllerTarget::Scene(SceneId::new("a")),
            ),
        ],
        &[LOOPBACK],
    );
    let addrs = running.local_addrs();
    let one = Address::new(1).expect("1");
    let absolute = Command::PanTiltAbsolute {
        pan_speed: 0x18,
        tilt_speed: 0x17,
        pan: 80,
        tilt: 48,
        width: PositionWidth::Pan5Tilt4,
    };

    let udp = UdpSocket::bind((LOOPBACK, 0)).expect("bind");
    udp.set_read_timeout(Some(Duration::from_millis(500)))
        .expect("timeout");
    udp.send_to(absolute.encode(one).expect("encodes").as_slice(), addrs[0])
        .expect("send");
    let mut buf = [0u8; 32];
    let (n, _) = udp.recv_from(&mut buf).expect("ack");
    assert_eq!(&buf[..n], &[0x90, 0x41, 0xFF]);
    let (n, _) = udp.recv_from(&mut buf).expect("completion");
    assert_eq!(&buf[..n], &[0x90, 0x51, 0xFF]);
    settled(&engine, "a", |a| {
        a.pan_deg.get() == 5.0 && a.tilt_deg.get() == 3.0
    });

    let mut tcp = TcpStream::connect(addrs[1]).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_millis(500)))
        .expect("timeout");
    // Two messages in one write, as a stream may carry them.
    let mut both = Inquiry::PanTiltPosition.encode(one).as_slice().to_vec();
    both.extend_from_slice(Inquiry::ZoomPosition.encode(one).as_slice());
    tcp.write_all(&both).expect("write");
    let mut got = Vec::new();
    let until = Instant::now() + Duration::from_secs(2);
    while got.iter().filter(|&&b| b == 0xFF).count() < 2 {
        assert!(Instant::now() < until, "answers: {got:?}");
        let mut buf = [0u8; 64];
        if let Ok(n) = tcp.read(&mut buf) {
            got.extend_from_slice(&buf[..n]);
        }
    }
    let first = ip::next_message(&got).expect("one").to_vec();
    let second = ip::next_message(&got[first.len()..]).expect("two").to_vec();
    let Ok((_, Reply::Data(data))) = Reply::parse(&first) else {
        panic!("{first:?}");
    };
    assert_eq!(
        InquiryValue::decode(Inquiry::PanTiltPosition, data),
        Ok(InquiryValue::PanTiltPosition {
            pan: 80,
            tilt: 48,
            width: PositionWidth::Pan5Tilt4
        })
    );
    let Ok((_, Reply::Data(data))) = Reply::parse(&second) else {
        panic!("{second:?}");
    };
    assert_eq!(
        InquiryValue::decode(Inquiry::ZoomPosition, data),
        Ok(InquiryValue::ZoomPosition(0))
    );
}

#[test]
fn an_endpoints_drives_stop_when_it_closes() {
    let engine = engine();
    let running = start(
        &engine,
        vec![endpoint(
            PtzTransport::ViscaIp,
            PtzControllerTarget::Program,
        )],
        &[LOOPBACK],
    );
    let mut c = Controller::new(running.local_addrs()[0]);
    assert_eq!(
        c.command(Command::PanTiltDrive {
            pan_speed: 1,
            tilt_speed: 1,
            pan: Pan::Left,
            tilt: Tilt::Stop,
        }),
        done()
    );
    assert!(aim_of(&engine, "a").moving);
    drop(running);
    let left = aim_of(&engine, "a");
    assert!(!left.moving, "{left:?}");
    // Its socket is closed: nothing answers.
    assert!(c.command(Command::Home).is_empty());
}

#[test]
fn endpoints_are_checked_as_the_host_lists_them() {
    let target = PtzControllerTarget::Program;
    for (text, problem) in [
        ("0.0.0.0:52381=program", "one interface"),
        ("[::]:52381=program", "one interface"),
        ("8.8.8.8:52381=program", "not a LAN address"),
        ("192.168.1.10:52381#8=program", "1 to 7"),
        ("192.168.1.10:52381/serial=program", "not a transport"),
        ("192.168.1.10:52381=air", "not a target"),
        ("192.168.1.10:52381=scene:", "not a target"),
        ("192.168.1.10:52381", "expected"),
    ] {
        let error = text.parse::<PtzController>().expect_err(text);
        assert!(error.contains(problem), "{text}: {error}");
    }
    let parsed: PtzController = "192.168.1.10:5678/tcp#2=scene:scn_1"
        .parse()
        .expect("parses");
    assert_eq!(parsed.to_string(), "192.168.1.10:5678/tcp#2=scene:scn_1");
    assert_eq!(
        "127.0.0.1:52381=preview"
            .parse::<PtzController>()
            .expect("parses")
            .to_string(),
        "127.0.0.1:52381/visca_ip#1=preview"
    );

    let one = endpoint(PtzTransport::ViscaIp, target.clone());
    let at = |port, transport| {
        PtzController::new((LOOPBACK, port).into(), transport, 1, target.clone()).expect("ok")
    };
    assert!(
        PtzControllerSetup::new(vec![one.clone()], Vec::new())
            .expect_err("no allowlist")
            .contains("allow at least one")
    );
    assert!(
        PtzControllerSetup::new(vec![one], vec!["8.8.8.8".parse().expect("ip")])
            .expect_err("a WAN controller")
            .contains("not a LAN address")
    );
    assert!(
        PtzControllerSetup::new(
            vec![at(5000, PtzTransport::ViscaIp), at(5000, PtzTransport::Udp)],
            vec![LOOPBACK]
        )
        .expect_err("one UDP port twice")
        .contains("two PTZ controller endpoints")
    );
    PtzControllerSetup::new(
        vec![at(5000, PtzTransport::Udp), at(5000, PtzTransport::Tcp)],
        vec![LOOPBACK],
    )
    .expect("a UDP and a TCP endpoint may share a port");
    assert!(
        PtzControllerSetup::new(Vec::new(), Vec::new()).is_ok(),
        "off"
    );
}

fn ptz_profile(engine: &Engine) -> Vec<PtzControllerProfile> {
    let control = connected(engine);
    match block_on(control.call(Call::Query {
        query: Query::ProfileGet {
            seconds: None,
            top: None,
        },
    }))
    .expect("profile.get")
    .result
    {
        CallResult::Query {
            result: QueryResult::ProfileGet { profile },
        } => profile.ptz_controllers,
        other => panic!("{other:?}"),
    }
}

/// ADR-0153 §1: a drive stops once its controller has been quiet for `IDLE_STOP`, and
/// only the axes its own drive left moving stop.
#[test]
fn a_quiet_controllers_drive_stops() {
    let engine = engine();
    let control = engine.connect();
    block_on(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "PTZ controller test".into(),
    }))
    .expect("hello");
    let c = endpoint(
        PtzTransport::ViscaIp,
        PtzControllerTarget::Scene(SceneId::new("a")),
    );
    let mut t = Translator::new(control, &c);
    let one = Address::new(1).expect("1");
    let send = |t: &mut Translator, command: Command| {
        t.handle(command.encode(one).expect("encodes").as_slice())
    };
    let now = Instant::now();
    t.heard(now);
    send(
        &mut t,
        Command::PanTiltDrive {
            pan_speed: 1,
            tilt_speed: 1,
            pan: Pan::Right,
            tilt: Tilt::Stop,
        },
    );
    send(&mut t, Command::Zoom(Drive::Plus(Some(0))));
    assert!(aim_of(&engine, "a").moving);
    assert!(!t.stop_if_idle(now + IDLE_STOP / 2), "not quiet yet");
    // An aim on pan and tilt takes those axes over; the zoom drive is still this
    // endpoint's to stop.
    send(&mut t, Command::Home);
    assert_eq!(
        t.driving,
        BTreeMap::from([(SceneId::new("a"), [false, false, true])])
    );
    assert!(t.stop_if_idle(now + IDLE_STOP), "quiet for IDLE_STOP");
    let aim = settled(&engine, "a", |_| true);
    assert!(aim.zoom.get() > 1.0, "{aim:?}");
    assert!(!t.stop_if_idle(now + IDLE_STOP * 2), "nothing left to stop");
    // A drive that is stopped by the controller itself leaves nothing to stop either.
    send(&mut t, Command::Zoom(Drive::Minus(None)));
    send(&mut t, Command::Zoom(Drive::Stop));
    assert!(t.driving.is_empty());
}

/// #1496 review N2: a program target that drives one scene, then another after a cut,
/// stops both when its controller goes quiet.
#[test]
fn a_drive_left_moving_before_a_cut_still_stops() {
    let engine = engine();
    let control = engine.connect();
    block_on(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "PTZ controller test".into(),
    }))
    .expect("hello");
    let mut t = Translator::new(
        control,
        &endpoint(PtzTransport::ViscaIp, PtzControllerTarget::Program),
    );
    let one = Address::new(1).expect("1");
    let pan_right = Command::PanTiltDrive {
        pan_speed: 1,
        tilt_speed: 1,
        pan: Pan::Right,
        tilt: Tilt::Stop,
    };
    let now = Instant::now();
    t.heard(now);
    assert_eq!(
        t.handle(pan_right.encode(one).expect("encodes").as_slice())
            .iter()
            .map(|b| b.as_slice().to_vec())
            .collect::<Vec<_>>(),
        done()
    );
    // Cut: scene `c` (3D, its own camera) goes on program; `a` keeps panning.
    let studio = connected(&engine);
    batch(
        &studio,
        edits(vec![
            create("c"),
            Edit::SceneSetCamera {
                scene_id: SceneId::new("c"),
                camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
            },
        ]),
    )
    .expect("scene c");
    block_on(studio.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("c"),
            preset_id: None,
        },
    }))
    .expect("cut");
    assert_eq!(
        t.handle(pan_right.encode(one).expect("encodes").as_slice())
            .iter()
            .map(|b| b.as_slice().to_vec())
            .collect::<Vec<_>>(),
        done()
    );
    assert!(aim_of(&engine, "a").moving && aim_of(&engine, "c").moving);
    assert_eq!(t.driving.len(), 2);
    assert!(t.stop_if_idle(now + IDLE_STOP));
    assert!(
        !aim_of(&engine, "a").moving,
        "the scene cut away from stops too"
    );
    assert!(!aim_of(&engine, "c").moving);
}

/// ADR-0153 §1 and §2: a TCP controller's drive stops when its last connection closes,
/// and `profile.get` counts each endpoint while it runs.
#[test]
fn a_tcp_controllers_drive_stops_when_it_hangs_up_and_profile_get_counts_it() {
    let engine = engine();
    assert!(ptz_profile(&engine).is_empty(), "no endpoint yet");
    let running = start(
        &engine,
        vec![endpoint(
            PtzTransport::Tcp,
            PtzControllerTarget::Scene(SceneId::new("a")),
        )],
        &[LOOPBACK],
    );
    let addr = running.local_addrs()[0];
    let one = Address::new(1).expect("1");
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_millis(500)))
        .expect("timeout");
    let drive = Command::PanTiltDrive {
        pan_speed: 1,
        tilt_speed: 1,
        pan: Pan::Left,
        tilt: Tilt::Stop,
    };
    tcp.write_all(drive.encode(one).expect("encodes").as_slice())
        .expect("write");
    let mut got = Vec::new();
    let until = Instant::now() + Duration::from_secs(2);
    while got.iter().filter(|&&b| b == 0xFF).count() < 2 {
        assert!(Instant::now() < until, "replies: {got:?}");
        let mut buf = [0u8; 16];
        if let Ok(n) = tcp.read(&mut buf) {
            got.extend_from_slice(&buf[..n]);
        }
    }
    assert_eq!(got, [0x90, 0x41, 0xFF, 0x90, 0x51, 0xFF]);
    assert!(aim_of(&engine, "a").moving);
    drop(tcp);
    let left = settled(&engine, "a", |_| true);
    assert!(left.pan_deg.get() < 0.0, "{left:?}");
    let profile = ptz_profile(&engine);
    assert_eq!(
        profile,
        vec![PtzControllerProfile {
            address: addr.to_string(),
            transport: "tcp".into(),
            target: "scene:a".into(),
            answered: 1,
            unallowed: 0,
            malformed: 0,
            refused: 0,
            idle_stops: 1,
        }]
    );
    drop(running);
    assert!(ptz_profile(&engine).is_empty(), "a stopped endpoint leaves");
}
